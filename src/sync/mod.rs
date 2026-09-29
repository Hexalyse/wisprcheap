//! Optional sync with a self-hosted wisprcheap server: settings, API keys, dictionary, translation
//! pairs, price overrides and history, end-to-end encrypted. See `server/PLAN.md` and
//! `sync/SPEC.md`.

pub mod cli;
pub mod client;
pub mod engine;
pub mod local;
pub mod service;
pub mod state;

pub use engine::{SyncContext, SyncError, SyncOutcome, sync_once};
pub use service::SyncHandle;
