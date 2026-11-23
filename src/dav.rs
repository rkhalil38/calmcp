use http::{Method, Request, StatusCode, Uri};
use jiff::tz::TimeZone;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Error as HttpError;
use hyper_util::client::legacy::{
    Client,
    connect::{HttpConnector, dns::GaiResolver},
};
use libdav::{
    CalDavClient, Depth, PropertyName,
    caldav::{
        FindCalendarHomeSet, FindCalendars, GetCalendarResources, GetCalendarResourcesResponse,
    },
    dav::{
        Delete, FindCollections, FindCurrentUserPrincipalError, Propfind, PutResource, WebDavError,
        make_relative_url,
    },
    names,
    requests::{DavRequest, ParseResponseError, xml_content_type_header},
    xmlutils::XmlNode,
};
use log::warn;
use thiserror::Error;
use tower_http::auth::AddAuthorization;

use crate::feed::{self, FeedError};
use crate::ical::{self, Calendar, Event, EventFields, IcalError, parse_events, parse_feed};

type AuthedHttp = AddAuthorization<Client<HttpsConnector<HttpConnector<GaiResolver>>, String>>;

pub type CalDav = CalDavClient<AuthedHttp>;

#[derive(Error, Debug)]
pub enum DavError {
    #[error("could not get principal URL")]
    Principal(#[from] FindCurrentUserPrincipalError<HttpError>),

    #[error("CalDAV request failed: {0}")]
    Dav(#[from] WebDavError<HttpError>),

    #[error("event not found")]
    NotFound,

    #[error("the event changed on the server since it was read; list events again for the current etag")]
    Conflict,

    #[error(transparent)]
    Ical(#[from] IcalError),

    #[error("invalid date/time: {0}")]
    Time(#[from] jiff::Error),

    #[error("could not fetch the subscribed calendar: {0}")]
    Feed(#[from] FeedError),

    #[error("this calendar is a read-only subscription")]
    ReadOnly,
}

/// The result of writing an event.
#[derive(serde::Serialize)]
pub struct Saved {
    pub id: String,
    /// The new etag, if the server returned one.
    pub etag: Option<String>,
}

const ICS_CONTENT_TYPE: &str = "text/calendar; charset=utf-8";

const EXPAND: PropertyName = PropertyName::new(names::CALDAV, "expand");

/// Resource type of a subscription to an external .ics feed (webcal), as
/// created by Radicale's web UI or Apple Calendar.
const SUBSCRIBED: PropertyName = PropertyName::new(names::CALENDAR_SERVER, "subscribed");
/// The URL of a subscription's feed, in a `<D:href>`.
const SOURCE: PropertyName = PropertyName::new(names::CALENDAR_SERVER, "source");

fn to_caldav(ts: jiff::Timestamp) -> String {
    ts.strftime("%Y%m%dT%H%M%SZ").to_string()
}

/// A `calendar-query` REPORT for the VEVENTs overlapping a time range, with
/// recurrences expanded by the server (RFC 4791 9.6.5).
///
/// We rely on the server for recurrence expansion instead of expanding RRULEs
/// ourselves. Verified against Radicale 3.5.4 (DST changes, EXDATE, and
/// overridden occurrences all handled); other servers are untested.
struct ExpandedEvents<'a> {
    collection_href: &'a str,
    start: String,
    end: String,
}

impl DavRequest for ExpandedEvents<'_> {
    type Response = GetCalendarResourcesResponse;
    type ParseError = ParseResponseError;
    type Error<E> = WebDavError<E>;

    fn prepare_request(&self, base_url: Uri) -> Result<Request<String>, http::Error> {
        let range = vec![("start", self.start.as_str()), ("end", self.end.as_str())];

        let expand = XmlNode::new(&EXPAND).with_attributes(range.clone());
        let prop = XmlNode::new(&names::PROP).with_children(vec![
            XmlNode::new(&names::GETETAG),
            XmlNode::new(&names::CALENDAR_DATA).with_children(vec![expand]),
        ]);

        let time_range = XmlNode::new(&names::TIME_RANGE).with_attributes(range);
        let vevent = XmlNode::new(&names::COMP_FILTER)
            .with_attributes(vec![("name", "VEVENT")])
            .with_children(vec![time_range]);
        let vcalendar = XmlNode::new(&names::COMP_FILTER)
            .with_attributes(vec![("name", "VCALENDAR")])
            .with_children(vec![vevent]);
        let filter = XmlNode::new(&names::FILTER).with_children(vec![vcalendar]);

        let body = XmlNode::new(&names::CALENDAR_QUERY)
            .with_children(vec![prop, filter])
            .render_node();

        let (ct_name, ct_value) = xml_content_type_header();
        Request::builder()
            .method(Method::from_bytes(b"REPORT")?)
            .uri(make_relative_url(base_url, self.collection_href)?)
            .header("Depth", "1")
            .header(ct_name, ct_value)
            .body(body)
    }

    fn parse_response(
        &self,
        parts: &http::response::Parts,
        body: &[u8],
    ) -> Result<Self::Response, ParseResponseError> {
        // Same multistatus shape as a calendar-multiget, so reuse libdav's parser.
        GetCalendarResources::new(self.collection_href).parse_response(parts, body)
    }
}

pub async fn list_calendars(client: &CalDav) -> Result<Vec<Calendar>, DavError> {
    let home_sets = match client.webdav_client.find_current_user_principal().await? {
        Some(principal) => {
            let sets = client
                .webdav_client
                .request(FindCalendarHomeSet::new(principal.path()))
                .await?
                .home_sets;
            if sets.is_empty() {
                vec![client.webdav_client.base_url().clone()]
            } else {
                sets
            }
        }
        None => vec![client.webdav_client.base_url().clone()],
    };

    let prop = |props: &[(&PropertyName, Option<String>)], name| {
        props.iter().find(|(p, _)| **p == name).and_then(|(_, v)| v.clone())
    };

    let mut out = Vec::new();
    for home in home_sets {
        for cal in client
            .webdav_client
            .request(FindCalendars::new(home.path()))
            .await?
            .calendars
        {
            out.push(Calendar {
                name: prop(&cal.properties, names::DISPLAY_NAME),
                color: prop(&cal.properties, names::CALENDAR_COLOUR),
                href: cal.href,
                subscribed: false,
            });
        }

        let subscriptions = client
            .webdav_client
            .request(
                FindCollections::new(home.path())
                    .with_collection_type(&SUBSCRIBED)
                    .with_additional_properties(&[&names::DISPLAY_NAME, &names::CALENDAR_COLOUR]),
            )
            .await?
            .collections;
        for sub in subscriptions {
            out.push(Calendar {
                name: prop(&sub.properties, names::DISPLAY_NAME),
                color: prop(&sub.properties, names::CALENDAR_COLOUR),
                href: sub.href,
                subscribed: true,
            });
        }
    }

    Ok(out)
}

pub async fn list_events(
    client: &CalDav,
    calendar_id: &str,
    tz: &TimeZone,
    start: jiff::Timestamp,
    end: jiff::Timestamp,
) -> Result<Vec<Event>, DavError> {
    // The server compares all-day (floating) dates as if they were UTC, which
    // shifts them by our UTC offset. Query a day wider on each side, then
    // filter against the real range using dates local to `tz`.
    let events = match subscription_source(client, calendar_id).await? {
        Some(url) => parse_feed(calendar_id, &feed::fetch(&url).await?, tz, start, end)?,
        None => fetch_events(client, calendar_id, tz, start, end).await?,
    };

    let mut kept = Vec::with_capacity(events.len());
    for ev in events {
        if ev.overlaps(start, end, tz)? {
            let (ev_start, _) = ev.interval(tz)?;
            kept.push((ev_start, ev));
        }
    }
    kept.sort_by_key(|(ev_start, _)| *ev_start);
    Ok(kept.into_iter().map(|(_, ev)| ev).collect())
}

/// The events of a CalDAV collection, roughly in `[start, end)`.
async fn fetch_events(
    client: &CalDav,
    calendar_id: &str,
    tz: &TimeZone,
    start: jiff::Timestamp,
    end: jiff::Timestamp,
) -> Result<Vec<Event>, DavError> {
    let margin = jiff::SignedDuration::from_hours(24);
    let request = ExpandedEvents {
        collection_href: calendar_id,
        start: to_caldav(start.saturating_sub(margin)?),
        end: to_caldav(end.saturating_add(margin)?),
    };
    let fetched = client.webdav_client.request(request).await?.resources;

    let mut events = Vec::new();
    for item in fetched {
        match item.content {
            Ok(content) => match parse_events(&item.href, &content.etag, &content.data, tz) {
                Ok(mut evs) => events.append(&mut evs),
                Err(e) => warn!("skipping {}: {e}", item.href),
            },
            Err(status) => {
                warn!("skipping {}: server returned {status}", item.href);
            }
        }
    }
    Ok(events)
}

/// If the collection at `href` is a subscription, the URL of its feed.
async fn subscription_source(client: &CalDav, href: &str) -> Result<Option<String>, DavError> {
    let response = client
        .webdav_client
        .request(
            Propfind::new(href)
                .with_properties(&[&names::RESOURCETYPE, &SOURCE])
                .with_depth(Depth::Zero),
        )
        .await?;
    let doc = response
        .xml_tree()
        .map_err(WebDavError::from)?;

    let subscribed = doc
        .descendants()
        .filter(|n| n.tag_name() == names::RESOURCETYPE)
        .any(|rt| rt.children().any(|n| n.tag_name() == SUBSCRIBED));
    if !subscribed {
        return Ok(None);
    }

    // Normally `<CS:source><D:href>url</D:href></CS:source>`; accept bare text too.
    let source = doc
        .descendants()
        .find(|n| n.tag_name() == SOURCE)
        .and_then(|s| {
            s.descendants()
                .find(|n| n.tag_name() == names::HREF)
                .and_then(|h| h.text())
                .or_else(|| s.text())
        })
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(FeedError::InvalidUrl)?;
    Ok(Some(source.to_string()))
}

/// Fail with `ReadOnly` if `calendar_id` is a subscription.
async fn ensure_writable(client: &CalDav, calendar_id: &str) -> Result<(), DavError> {
    match subscription_source(client, calendar_id).await? {
        Some(_) => Err(DavError::ReadOnly),
        None => Ok(()),
    }
}

pub async fn create_event(
    client: &CalDav,
    calendar_id: &str,
    fields: &EventFields,
    tz: &TimeZone,
) -> Result<Saved, DavError> {
    ensure_writable(client, calendar_id).await?;
    let uid = uuid::Uuid::new_v4().to_string();
    let ics = ical::new_event(&uid, fields, tz)?;
    let href = format!("{}/{uid}.ics", calendar_id.trim_end_matches('/'));

    let response = client
        .webdav_client
        .request(PutResource::new(&href).create(ics, ICS_CONTENT_TYPE))
        .await?;
    Ok(Saved { id: href, etag: response.etag })
}

/// Edit the event at `id`. Fails with `Conflict` if it no longer matches `etag`.
pub async fn update_event(
    client: &CalDav,
    id: &str,
    etag: &str,
    recurrence_id: Option<&str>,
    fields: &EventFields,
    tz: &TimeZone,
) -> Result<Saved, DavError> {
    ensure_writable(client, collection_of(id)?).await?;
    if let Some(rid) = recurrence_id {
        check_occurrence(client, id, rid, tz).await?;
    }
    let current = fetch_current(client, id, etag).await?;
    let ics = ical::update_event(&current, recurrence_id, fields, tz)?;
    save_current(client, id, etag, ics).await
}

/// Delete the event at `id`, or with `recurrence_id`, only that occurrence.
/// Returns the remaining event when an occurrence was cancelled, or `None`
/// when the whole resource was deleted.
pub async fn delete_event(
    client: &CalDav,
    id: &str,
    etag: &str,
    recurrence_id: Option<&str>,
    tz: &TimeZone,
) -> Result<Option<Saved>, DavError> {
    ensure_writable(client, collection_of(id)?).await?;
    if let Some(rid) = recurrence_id {
        check_occurrence(client, id, rid, tz).await?;
        let current = fetch_current(client, id, etag).await?;
        if let Some(ics) = ical::cancel_occurrence(&current, rid, tz)? {
            return Ok(Some(save_current(client, id, etag, ics).await?));
        }
    }

    client
        .webdav_client
        .request(Delete::new(id).with_etag(etag))
        .await
        .map_err(write_error)?;
    Ok(None)
}

/// The calendar an event id belongs to. Ids of events from subscriptions are
/// `<calendar>#<uid>`.
fn collection_of(id: &str) -> Result<&str, DavError> {
    let id = id.split_once('#').map_or(id, |(calendar, _)| calendar);
    id.rfind('/').map(|i| &id[..=i]).ok_or(DavError::NotFound)
}

/// The current .ics of `id`, as long as it still matches `etag`.
async fn fetch_current(client: &CalDav, id: &str, etag: &str) -> Result<String, DavError> {
    let current = client
        .webdav_client
        .request(GetCalendarResources::new(collection_of(id)?).with_hrefs([id]))
        .await?
        .resources
        .into_iter()
        .next()
        .ok_or(DavError::NotFound)?;
    let content = current.content.map_err(|status| match status {
        StatusCode::NOT_FOUND => DavError::NotFound,
        status => WebDavError::BadStatusCode(status).into(),
    })?;
    if content.etag != etag {
        return Err(DavError::Conflict);
    }
    Ok(content.data)
}

/// Overwrite `id` with `ics`, as long as it still matches `etag`.
async fn save_current(client: &CalDav, id: &str, etag: &str, ics: String) -> Result<Saved, DavError> {
    let response = client
        .webdav_client
        .request(PutResource::new(id).update(ics, ICS_CONTENT_TYPE, etag))
        .await
        .map_err(write_error)?;
    Ok(Saved { id: id.to_string(), etag: response.etag })
}

/// Map the statuses of a conditional write to their meaning.
fn write_error(e: WebDavError<HttpError>) -> DavError {
    match e {
        WebDavError::BadStatusCode(StatusCode::PRECONDITION_FAILED) => DavError::Conflict,
        WebDavError::BadStatusCode(StatusCode::NOT_FOUND) => DavError::NotFound,
        e => e.into(),
    }
}

/// Ask the server whether the event at `id` has an occurrence `recurrence_id`,
/// since we leave recurrence expansion to the server.
async fn check_occurrence(
    client: &CalDav,
    id: &str,
    recurrence_id: &str,
    tz: &TimeZone,
) -> Result<(), DavError> {
    // A few days around the occurrence, so all-day dates match whatever
    // timezone the server reads them in.
    let at = ical::recurrence_instant(recurrence_id)?;
    let day = jiff::SignedDuration::from_hours(24);
    let request = ExpandedEvents {
        collection_href: collection_of(id)?,
        start: to_caldav(at.checked_sub(day).map_err(IcalError::from)?),
        end: to_caldav(at.checked_add(day * 2).map_err(IcalError::from)?),
    };

    let resources = client.webdav_client.request(request).await?.resources;
    let found = resources
        .into_iter()
        .filter(|r| r.href == id)
        .filter_map(|r| r.content.ok())
        .filter_map(|c| parse_events(id, &c.etag, &c.data, tz).ok())
        .flatten()
        .any(|e| e.recurrence_id.as_deref() == Some(recurrence_id));
    if found {
        Ok(())
    } else {
        Err(IcalError::OccurrenceNotFound(recurrence_id.to_string()).into())
    }
}
