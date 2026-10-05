//! wisprcheap sync server: device sync API (`/v1`) and web UI. See `PLAN.md` and `../sync/SPEC.md`.

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod maintenance;
pub mod reports;
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
    blocking_slots: Arc<tokio::sync::Semaphore>,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(config: Config, db: Db) -> SharedState {
        Arc::new(Self {
            config,
            db,
            limiter: auth::RateLimiter::default(),
            blocking_slots: Arc::new(tokio::sync::Semaphore::new(8)),
        })
    }

    /// SQLite and filesystem work must not block Tokio's network workers.
    pub async fn blocking<R: Send + 'static>(
        self: &SharedState,
        job: impl FnOnce(&AppState) -> R + Send + 'static,
    ) -> anyhow::Result<R> {
        let state = self.clone();
        let permit = self.blocking_slots.clone().acquire_owned().await?;
        Ok(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            job(&state)
        })
        .await?)
    }
}

/// The whole HTTP application: sync API, web UI, health check and security headers.
pub fn app(state: SharedState) -> Router {
    Router::new()
        .merge(api::router())
        .merge(web::router())
        .route("/healthz", axum::routing::get(readiness))
        .route("/readyz", axum::routing::get(readiness))
        .layer(axum::middleware::from_fn_with_state(state.clone(), security_headers))
        .layer(DefaultBodyLimit::max(wisprcheap_sync::protocol::MAX_REQUEST_BYTES))
        .with_state(state)
}

async fn readiness(State(state): State<SharedState>) -> (axum::http::StatusCode, &'static str) {
    let check = state.blocking(|s| s.db.lock().query_row("SELECT 1", [], |r| r.get::<_, i64>(0)));
    match tokio::time::timeout(std::time::Duration::from_secs(2), check).await {
        Ok(Ok(Ok(1))) => (axum::http::StatusCode::OK, "ok"),
        _ => (axum::http::StatusCode::SERVICE_UNAVAILABLE, "database unavailable"),
    }
}

async fn security_headers(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let is_static = req.uri().path().starts_with("/static/");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    if !is_static {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
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
