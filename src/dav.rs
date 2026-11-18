use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::{Client, connect::{HttpConnector, dns::GaiResolver}};
use libdav::{CalDavClient, caldav::{FindCalendarHomeSet, FindCalendars}, dav::{FindCurrentUserPrincipalError, RequestError, WebDavError}, names};
use tower_http::auth::AddAuthorization;
use thiserror::Error;
use hyper_util::client::legacy::Error as HttpError;

type AuthedHttp = AddAuthorization<Client<HttpsConnector<HttpConnector<GaiResolver>>, String>>;

pub type CalDav = CalDavClient<AuthedHttp>;

pub struct Calendar {
    pub href: String,
    pub name: Option<String>,
    pub color: Option<String>,
}

/**
 * ERRORS
 */
#[derive(Error, Debug)]
pub enum ListError {
    #[error("could not get principal URL")]
    Principal(#[from] FindCurrentUserPrincipalError<HttpError>),


    #[error("CalDAV request failed: {0}")]
    Dav(#[from] WebDavError<HttpError>)
}

pub async fn list_calendars(client: &CalDav) -> Result<Vec<Calendar>, ListError> {
    let home_sets = match client.webdav_client.find_current_user_principal().await? {
        Some(principal) => {
            let sets = client
                .webdav_client
                .request(FindCalendarHomeSet::new(principal.path()))
                .await?
                .home_sets;
            if sets.is_empty() { vec![client.webdav_client.base_url().clone()] } else { sets }
        },
        None => vec![client.webdav_client.base_url().clone()],
    };

    let mut out = Vec::new();
    for home in home_sets {
        for cal in client.webdav_client.request(FindCalendars::new(home.path())).await?.calendars {
            let prop = |name| {
                cal.properties
                    .iter()
                    .find(|(p, _)| **p == name)
                    .and_then(|(_, v)| v.clone())
            };
            let name = prop(names::DISPLAY_NAME);
            let color = prop(names::CALENDAR_COLOUR);

            out.push(Calendar { href: cal.href, name, color });
        }
    }

    Ok(out)
}
