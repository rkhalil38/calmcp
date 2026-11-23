use std::{env, process::{self, ExitCode}, sync::Arc};

use axum::{extract::{Request, State}, middleware::{self, Next}, response::{IntoResponse, Response}};
use http::{StatusCode, Uri, header::{AUTHORIZATION, WWW_AUTHENTICATE}};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use libdav::{CalDavClient, dav::WebDavClient};
use log::{error, info, warn};
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService, streamable_http_server::session::local::LocalSessionManager};
use subtle::ConstantTimeEq;
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

    let Ok(api_key) = env::var("MCP_API_KEY") else {
        error!("MCP_API_KEY is not set");
        return ExitCode::FAILURE;
    };
    if api_key.is_empty() {
        error!("MCP_API_KEY must not be empty");
        return ExitCode::FAILURE;
    }
    let api_key: Arc<str> = api_key.into();

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

    let mut config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);

    // Host header allowlist (DNS rebinding protection). Defaults to localhost only;
    // set MCP_ALLOWED_HOSTS to the hostname(s) clients use, or "*" to allow any.
    if let Ok(hosts) = env::var("MCP_ALLOWED_HOSTS") {
        let hosts: Vec<String> = hosts
            .split(',')
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .collect();
        if hosts.iter().any(|h| h == "*") {
            warn!("MCP_ALLOWED_HOSTS=*: Host header validation is disabled");
            config = config.disable_allowed_hosts();
        } else if !hosts.is_empty() {
            config = config.with_allowed_hosts(hosts);
        }
    }

    let service = StreamableHttpService::new(
        move || Ok(CalMcp::new(Arc::clone(&client), tz.clone())),
        LocalSessionManager::default().into(),
        config
    );

    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(api_key, require_api_key));
    let listener = 
        tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await.unwrap();
    axum::serve(listener, router).await.unwrap();

    ExitCode::SUCCESS
}

/// Rejects requests without an `Authorization: Bearer <MCP_API_KEY>` header.
/// The key is compared in constant time to avoid leaking it through response timing.
async fn require_api_key(State(key): State<Arc<str>>, req: Request, next: Next) -> Response {
    let authorized = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.as_bytes().strip_prefix(b"Bearer "))
        .is_some_and(|token| token.ct_eq(key.as_bytes()).into());

    if authorized {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, "Bearer")]).into_response()
    }
}
