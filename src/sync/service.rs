//! Background sync in the running app: a Tokio task that syncs at startup, after config changes and
//! new history entries (debounced), every 15 minutes and on "Sync now", with exponential backoff
//! when the server can't be reached. Sync never blocks dictation.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::Instant;
use wisprcheap_sync::protocol::MonthStats;

use crate::sync::engine::{SyncContext, SyncError, SyncOutcome, describe_counts, sync_once};
use crate::sync::state;

const PERIOD: Duration = Duration::from_secs(15 * 60);
const AFTER_START: Duration = Duration::from_secs(2);
const AFTER_CONFIG: Duration = Duration::from_secs(3);
const AFTER_HISTORY: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Off,
    Syncing,
    Ok,
    Offline,
    Disconnected,
    NeedsKey,
    Error,
}

#[derive(Debug, Clone, Default)]
pub struct SyncStatus {
    pub phase: Phase,
    pub last_sync: Option<DateTime<Local>>,
    pub message: String,
    pub device_id: Option<String>,
    /// This month, every device (from the server).
    pub month: Option<MonthStats>,
    pub devices: usize,
    pub retry_at: Option<DateTime<Local>>,
}

impl SyncStatus {
    /// Tray line; None when sync is off.
    pub fn tray_line(&self) -> Option<String> {
        let at = |t: &DateTime<Local>| t.format("%H:%M").to_string();
        Some(match self.phase {
            Phase::Off => return None,
            Phase::Syncing => "Sync: syncing...".into(),
            Phase::Ok => match &self.last_sync {
                Some(t) => format!("Synced at {}", at(t)),
                None => "Sync: on".into(),
            },
            Phase::Offline => match &self.retry_at {
                Some(t) => format!("Sync: offline (retry at {})", at(t)),
                None => "Sync: offline".into(),
            },
            Phase::Disconnected => "Sync: disconnected".into(),
            Phase::NeedsKey => "Sync: passphrase needed".into(),
            Phase::Error => "Sync: error (see the log)".into(),
        })
    }
}

enum Cmd {
    Now,
    ConfigChanged(bool),
    HistoryAppended,
}

/// Handle kept by the app. Cheap to clone.
#[derive(Clone)]
pub struct SyncHandle {
    tx: Option<UnboundedSender<Cmd>>,
    status: Arc<Mutex<SyncStatus>>,
}

impl SyncHandle {
    /// A handle that does nothing (tests, or no runtime).
    pub fn inert() -> Self {
        Self {
            tx: None,
            status: Arc::default(),
        }
    }

    /// Starts the sync task. `on_status` is called (from the task) whenever the status changes.
    pub fn spawn(
        ctx: SyncContext,
        enabled: bool,
        on_status: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let status = Arc::new(Mutex::new(SyncStatus {
            device_id: if enabled {
                state::device_id(&ctx.state_dir)
            } else {
                None
            },
            ..Default::default()
        }));
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = Self {
            tx: Some(tx),
            status: status.clone(),
        };
        tokio::spawn(task(ctx, enabled, rx, status, Arc::new(on_status)));
        handle
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }

    pub fn sync_now(&self) {
        self.send(Cmd::Now);
    }

    pub fn config_changed(&self, enabled: bool) {
        self.send(Cmd::ConfigChanged(enabled));
    }

    pub fn history_appended(&self) {
        self.send(Cmd::HistoryAppended);
    }

    pub fn status(&self) -> SyncStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn enabled(&self) -> bool {
        self.status().phase != Phase::Off
    }

    /// Device id for new history entries (None when sync is off).
    pub fn device_id(&self) -> Option<String> {
        let st = self.status.lock().unwrap();
        if st.phase == Phase::Off {
            None
        } else {
            st.device_id.clone()
        }
    }
}

fn earlier(current: Option<Instant>, candidate: Instant) -> Option<Instant> {
    Some(match current {
        Some(c) if c < candidate => c,
        _ => candidate,
    })
}

async fn task(
    ctx: SyncContext,
    mut enabled: bool,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    status: Arc<Mutex<SyncStatus>>,
    on_status: Arc<dyn Fn() + Send + Sync>,
) {
    let set = |f: &dyn Fn(&mut SyncStatus)| {
        f(&mut status.lock().unwrap());
        on_status();
    };
    let mut next: Option<Instant> = enabled.then(|| Instant::now() + AFTER_START);
    let mut failures: u32 = 0;
    if enabled {
        set(&|s| s.phase = Phase::Syncing);
    }
    loop {
        let sleep = async {
            match next {
                Some(t) => tokio::time::sleep_until(t).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return };
                match cmd {
                    Cmd::Now => {
                        if enabled {
                            next = Some(Instant::now());
                        } else {
                            crate::info!("[sync] Sync is not set up (see `wisprcheap sync pair`).");
                        }
                    }
                    Cmd::ConfigChanged(on) => {
                        if on != enabled {
                            enabled = on;
                            failures = 0;
                            if on {
                                crate::info!("[sync] Sync enabled.");
                                let device = state::device_id(&ctx.state_dir);
                                set(&|s| {
                                    s.device_id = device.clone();
                                    s.phase = Phase::Syncing;
                                });
                            } else {
                                crate::info!("[sync] Sync disabled.");
                                set(&|s| *s = SyncStatus::default());
                                next = None;
                                continue;
                            }
                        }
                        if enabled {
                            next = earlier(next, Instant::now() + AFTER_CONFIG);
                        }
                    }
                    Cmd::HistoryAppended => {
                        if enabled {
                            next = earlier(next, Instant::now() + AFTER_HISTORY);
                        }
                    }
                }
            }
            _ = sleep => {
                next = None;
                if !enabled {
                    continue;
                }
                set(&|s| s.phase = Phase::Syncing);
                let started = std::time::Instant::now();
                let result = sync_once(&ctx).await;
                let secs = started.elapsed().as_secs_f64();
                match result {
                    Ok(outcome) => {
                        failures = 0;
                        log_outcome(&outcome, secs);
                        let device = Some(outcome.device_id.clone());
                        set(&|s| {
                            s.phase = Phase::Ok;
                            s.last_sync = Some(Local::now());
                            s.message.clear();
                            s.device_id = device.clone();
                            s.month = outcome.month.clone();
                            s.devices = outcome.devices;
                            s.retry_at = None;
                        });
                        next = Some(Instant::now() + PERIOD);
                    }
                    Err(e) => {
                        let (phase, retry) = match &e {
                            SyncError::Network(_) | SyncError::Server(_) => {
                                failures += 1;
                                let secs = 30u64.saturating_mul(1 << (failures - 1).min(10));
                                (
                                    if matches!(e, SyncError::Network(_)) { Phase::Offline } else { Phase::Error },
                                    Some(Duration::from_secs(secs).min(MAX_BACKOFF)),
                                )
                            }
                            SyncError::Local(_) => (Phase::Error, Some(PERIOD)),
                            SyncError::Unauthorized => (Phase::Disconnected, None),
                            SyncError::NeedsKey(_) => (Phase::NeedsKey, None),
                            SyncError::Disabled => (Phase::Off, None),
                        };
                        let retry_at = retry.map(|d| Local::now() + chrono::Duration::from_std(d).unwrap_or_default());
                        match (&e, retry) {
                            (SyncError::Disabled, _) => {}
                            (_, Some(d)) => crate::warn!(
                                "[sync] {e}; retrying in {}",
                                human(d)
                            ),
                            (_, None) => crate::error!("[sync] {e}"),
                        }
                        let message = e.to_string();
                        set(&|s| {
                            s.phase = phase;
                            s.message = message.clone();
                            s.retry_at = retry_at;
                        });
                        next = retry.map(|d| Instant::now() + d);
                    }
                }
            }
        }
    }
}

fn human(d: Duration) -> String {
    let s = d.as_secs();
    if s < 90 {
        format!("{s} s")
    } else {
        format!("{} min", s.div_ceil(60))
    }
}

fn log_outcome(o: &SyncOutcome, secs: f64) {
    if o.first {
        let applied = describe_counts(&o.applied);
        let pushed = describe_counts(&o.pushed);
        let mut parts = Vec::new();
        if !applied.is_empty() {
            parts.push(format!("{applied} updated from the server"));
        }
        if !pushed.is_empty() {
            parts.push(format!("{pushed} uploaded"));
        }
        let what = if parts.is_empty() {
            "nothing to merge".to_string()
        } else {
            parts.join(", ")
        };
        crate::info!("[sync] First sync done: {what} ({secs:.1} s).");
    } else {
        let summary = o.summary();
        if summary != "up to date" {
            crate::info!("[sync] {summary} ({secs:.1} s).");
        }
    }
}
