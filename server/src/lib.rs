//! wisprcheap sync server: device sync API (`/v1`) and web UI. See `PLAN.md` and `../sync/SPEC.md`.

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod stats;
pub mod util;
pub mod web;

use std::sync::Arc;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

pub use config::Config;
pub use db::Db;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct AppState {
    pub config: Config,
    pub db: Db,
    pub limiter: auth::RateLimiter,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(config: Config, db: Db) -> SharedState {
        Arc::new(Self { config, db, limiter: auth::RateLimiter::default() })
    }
}

/// The whole HTTP application: sync API, web UI, health check and security headers.
pub fn app(state: SharedState) -> Router {
    Router::new()
        .merge(api::router())
        .merge(web::router())
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(state.clone(), security_headers))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state)
}

async fn security_headers(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'",
        ),
    );
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // `same-origin`: our own form POSTs carry their Origin (checked against CSRF); other sites get
    // no referrer. (`no-referrer` makes browsers send `Origin: null` even on same-origin POSTs.)
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    if state.config.is_https() {
        h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    }
    res
}
