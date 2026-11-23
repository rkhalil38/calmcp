use std::{env, process::{self, ExitCode}, sync::Arc};

use http::Uri;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use libdav::{CalDavClient, dav::WebDavClient};
use log::{error, info, warn};
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService, streamable_http_server::session::local::LocalSessionManager};
use tower_http::auth::AddAuthorization;

use crate::mcp::CalMcp;

mod dav;
mod free;
mod mcp;
mod ical;

#[tokio::main]
async fn main() -> ExitCode {
    env_logger::init();

    let tz_name: String = env::var("TZ").unwrap_or("Etc/UTC".to_string());
    let Ok(tz) = jiff::tz::TimeZone::get(&tz_name) else {
        error!("TZ is not a valid IANA timezone: {tz_name}");
        return ExitCode::FAILURE;
    };
    let port: u16 = env::var("PORT").unwrap_or("8000".to_string()).parse().unwrap();

    let Ok(caldav_url) = env::var("CALDAV_URL") else {
        error!("CALDAV_URL is not set");
        info!("exiting...");
        process::exit(1);
    };

    let Ok(uri) = caldav_url.parse::<Uri>() else {
        error!("CALDAV_URL is not a valid URI");
        return ExitCode::FAILURE;
    };

    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.host().is_none() {
        error!("CALDAV_URL must be an absolute http(s) URL"); return ExitCode::FAILURE;
    }

    if uri.scheme_str() == Some("http") {
        warn!("CALDAV_URL uses plain http; credentials will be sent unencrypted!");
    }

    let Ok(caldav_username) = env::var("CALDAV_USERNAME") else {
        error!("CALDAV_USERNAME is not set");
        info!("exiting...");
        process::exit(1);
    };

    let Ok(caldav_pw) = env::var("CALDAV_PASSWORD") else {
        error!("CALDAV_PASSWORD is not set");
        info!("exiting...");
        process::exit(1);
    };

    let https_connector = HttpsConnectorBuilder::new()
        .with_native_roots()
        .unwrap()
        .https_or_http()
        .enable_http1()
        .build();
    let https_client = Client::builder(TokioExecutor::new()).build(https_connector);
    let https_client = AddAuthorization::basic(https_client, &caldav_username, &caldav_pw);
    let webdav = WebDavClient::new(uri, https_client);
    let client = Arc::new(CalDavClient::new(webdav));

    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);

    let service = StreamableHttpService::new(
        move || Ok(CalMcp::new(Arc::clone(&client), tz.clone())),
        LocalSessionManager::default().into(),
        config
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = 
        tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await.unwrap();
    axum::serve(listener, router).await.unwrap();

    ExitCode::SUCCESS
}
