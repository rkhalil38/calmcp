//! Fetching subscribed (webcal) calendar feeds.
//!
//! Subscriptions are plain .ics files on another server. The CalDAV server
//! only stores their URL, so we download them ourselves, without the CalDAV
//! credentials.

use std::{sync::LazyLock, time::Duration};

use http::{Request, StatusCode, Uri, header};
use http_body_util::{BodyExt, Limited};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use thiserror::Error;

const MAX_REDIRECTS: usize = 5;
const MAX_SIZE: usize = 20 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

// Errors never include the URL: feed URLs often embed a secret token.
#[derive(Error, Debug)]
pub enum FeedError {
    #[error("unsupported subscription URL")]
    InvalidUrl,
    #[error("request failed: {0}")]
    Request(String),
    #[error("server returned {0}")]
    Status(StatusCode),
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("timed out")]
    Timeout,
    #[error("feed is larger than {} MB or could not be read", MAX_SIZE / 1024 / 1024)]
    Body,
    #[error("feed is not valid UTF-8")]
    NotUtf8,
}

static CLIENT: LazyLock<Client<HttpsConnector<HttpConnector>, String>> = LazyLock::new(|| {
    let connector = HttpsConnectorBuilder::new()
        .with_native_roots()
        .expect("no native root certificates")
        .https_or_http()
        .enable_http1()
        .build();
    Client::builder(TokioExecutor::new()).build(connector)
});

/// Download the .ics feed at `url` (`webcal://`, `webcals://`, `http://` or `https://`).
pub async fn fetch(url: &str) -> Result<String, FeedError> {
    tokio::time::timeout(TIMEOUT, fetch_inner(url))
        .await
        .map_err(|_| FeedError::Timeout)?
}

async fn fetch_inner(url: &str) -> Result<String, FeedError> {
    let mut uri = to_http(url)?;
    for _ in 0..=MAX_REDIRECTS {
        let request = Request::get(uri.clone())
            .header(header::USER_AGENT, concat!("calmcp/", env!("CARGO_PKG_VERSION")))
            .header(header::ACCEPT, "text/calendar, */*;q=0.5")
            .body(String::new())
            .map_err(|_| FeedError::InvalidUrl)?;
        let response = CLIENT
            .request(request)
            .await
            .map_err(|e| FeedError::Request(error_chain(&e)))?;

        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .ok_or(FeedError::Status(status))?;
            uri = resolve(&uri, location)?;
            continue;
        }
        if !status.is_success() {
            return Err(FeedError::Status(status));
        }

        let body = Limited::new(response.into_body(), MAX_SIZE)
            .collect()
            .await
            .map_err(|_| FeedError::Body)?
            .to_bytes();
        return String::from_utf8(body.to_vec()).map_err(|_| FeedError::NotUtf8);
    }
    Err(FeedError::TooManyRedirects)
}

/// Map a webcal URL to the http(s) URL it stands for.
fn to_http(url: &str) -> Result<Uri, FeedError> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://").ok_or(FeedError::InvalidUrl)?;
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "webcal" | "webcals" | "https" => "https",
        "http" => "http",
        _ => return Err(FeedError::InvalidUrl),
    };
    let uri: Uri = format!("{scheme}://{rest}").parse().map_err(|_| FeedError::InvalidUrl)?;
    if uri.host().is_none() {
        return Err(FeedError::InvalidUrl);
    }
    Ok(uri)
}

/// Resolve a redirect's Location header against the URL that returned it.
fn resolve(base: &Uri, location: &str) -> Result<Uri, FeedError> {
    if location.contains("://") {
        return to_http(location);
    }
    let authority = base.authority().ok_or(FeedError::InvalidUrl)?;
    let scheme = base.scheme_str().unwrap_or("https");
    let path = if location.starts_with('/') {
        location.to_string()
    } else {
        let dir = base.path().rsplit_once('/').map_or("", |(dir, _)| dir);
        format!("{dir}/{location}")
    };
    format!("{scheme}://{authority}{path}")
        .parse()
        .map_err(|_| FeedError::InvalidUrl)
}

/// An error and its sources, e.g. "client error (Connect): dns error: ...".
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webcal_maps_to_https() {
        let uri = to_http("webcal://canvas.example.edu/feeds/calendars/user_abc.ics").unwrap();
        assert_eq!(uri.to_string(), "https://canvas.example.edu/feeds/calendars/user_abc.ics");
        assert!(to_http("ftp://example.com/a.ics").is_err());
    }

    #[test]
    fn resolves_relative_redirects() {
        let base: Uri = "https://a.example/x/y.ics?t=1".parse().unwrap();
        assert_eq!(resolve(&base, "/z.ics").unwrap().to_string(), "https://a.example/z.ics");
        assert_eq!(resolve(&base, "z.ics").unwrap().to_string(), "https://a.example/x/z.ics");
        assert_eq!(resolve(&base, "webcal://b.example/c.ics").unwrap().to_string(), "https://b.example/c.ics");
    }
}
