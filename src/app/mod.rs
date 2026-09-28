//! The running app: recording lifecycle, processing queue, config reload, tray and control commands.
//! Port of the TypeScript `main.ts`.

mod jobs;
mod shared;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::config::{ConfigArgs, LoadedConfig, ensure_config_file, load_config};
use crate::hotkey::{CancelReason, HotkeyEvent, Mode, PushToTalk, State};
use crate::instance::{AcquireError, Instance, Request, acquire_instance};
use crate::keyboard::{Hook, KeyEvent};
use crate::paths::paths;
use crate::recorder::Recorder;
use crate::sounds::{Cue, Sounds};
use crate::state::{load_state, save_state};
use crate::tray::{TrayAction, Ui};
use crate::{error, info, say, warn};

use jobs::{AddWordSource, Job};
use shared::{Pipeline, Shared, Status, js_number};

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub config_args: ConfigArgs,
    pub use_tray: bool,
    /// Spawned by "Restart": wait for the old instance to release the control socket.
    pub restarted: bool,
    /// Arguments given to `run`, reused when restarting.
    pub run_args: Vec<String>,
}

pub enum AppMsg {
    Key(KeyEvent),
    TapTimeout(u64),
    Tray(TrayAction),
    Pipe(Request),
    ScheduleReload,
    ReloadTimer(u64),
    MaxDuration(u64),
    StopNow {
        keep: bool,
        mode: Mode,
    },
    Stopped {
        pcm: Vec<i16>,
        keep: bool,
        mode: Mode,
    },
    Quit,
    Restart,
}

/// Instance acquired and config loaded: ready to run.
pub struct Prepared {
    loaded: LoadedConfig,
    instance: Instance,
    pipe_rx: UnboundedReceiver<Request>,
}

/// Become the single instance and load the config. Errors are user-facing messages.
pub async fn prepare(opts: &RunOptions) -> Result<Prepared, String> {
    let (pipe_tx, pipe_rx) = mpsc::unbounded_channel();
    let wait = if opts.restarted {
        Duration::from_secs(10)
    } else {
        Duration::ZERO
    };
    let instance = match acquire_instance(pipe_tx, wait).await {
        Ok(i) => i,
        Err(AcquireError::AlreadyRunning) => {
            return Err("wisprcheap is already running (see the tray icon). Quit it from the tray or with `wisprcheap stop` first.".into());
        }
        Err(e) => return Err(e.to_string()),
    };
    let loaded = load_config(&opts.config_args).map_err(|e| e.to_string())?;
    Ok(Prepared {
        loaded,
        instance,
        pipe_rx,
    })
}

fn spawn_forward<T: Send + 'static>(
    mut rx: UnboundedReceiver<T>,
    tx: UnboundedSender<AppMsg>,
    wrap: fn(T) -> AppMsg,
) {
    tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            if tx.send(wrap(v)).is_err() {
                break;
            }
        }
    });
}

fn send_later(tx: &UnboundedSender<AppMsg>, delay: Duration, msg: AppMsg) {
    let tx = tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = tx.send(msg);
    });
}

/// Run until quit (never returns normally: quitting exits the process).
pub async fn run(
    prepared: Prepared,
    opts: RunOptions,
    ui: Option<Ui>,
    tray_rx: Option<UnboundedReceiver<TrayAction>>,
) {
    let Prepared {
        loaded,
        instance,
        pipe_rx,
    } = prepared;
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppMsg>();
    let (job_tx, job_rx) = mpsc::unbounded_channel::<Job>();

    let hotkey = match PushToTalk::new(&loaded.config.hotkey) {
        Ok(h) => Arc::new(Mutex::new(h)),
        Err(e) => {
            error!("{e}");
            std::process::exit(1);
        }
    };
    let status = Status {
        paused: false,
        pending: 0,
        recording: false,
        recording_mode: Mode::Dictation,
        busy_label: "Transcribing...".into(),
        state: load_state(),
        last_text: None,
        last_failed: None,
    };
    let shared = Arc::new(Shared {
        pipeline: RwLock::new(Arc::new(Pipeline::build(&loaded))),
        status: Mutex::new(status),
        hotkey,
        sounds: Mutex::new(Sounds::new(&loaded.config.sounds)),
        ui: ui.clone(),
        base_dir: loaded.base_dir.clone(),
        app_tx: app_tx.clone(),
        job_tx,
    });
    // Applies the translation check and fills the tray.
    shared.pipeline_changed();

    spawn_forward(pipe_rx, app_tx.clone(), AppMsg::Pipe);
    if let Some(rx) = tray_rx {
        spawn_forward(rx, app_tx.clone(), AppMsg::Tray);
    }
    tokio::spawn(jobs::worker(shared.clone(), job_rx));
    {
        let tx = app_tx.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = tx.send(AppMsg::Quit);
            }
        });
    }
    #[cfg(unix)]
    {
        let tx = app_tx.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            if let Ok(mut term) = signal(SignalKind::terminate()) {
                term.recv().await;
                let _ = tx.send(AppMsg::Quit);
            }
        });
    }

    let (key_tx, key_rx) = mpsc::unbounded_channel::<KeyEvent>();
    spawn_forward(key_rx, app_tx.clone(), AppMsg::Key);
    let hook = match Hook::start(key_tx) {
        Ok(h) => Some(h),
        Err(e) => {
            error!("Could not listen to the keyboard: {e}");
            None
        }
    };
    if let Some(warning) = hook.as_ref().and_then(|h| h.warning()) {
        error!("[hotkey] {warning}");
        shared.notify_error("Keyboard setup needed", warning);
    }

    let mut actor = Actor {
        recorder: Recorder::new(loaded.config.recording.device.clone()),
        shared,
        opts,
        app_tx,
        stopping: false,
        start_after_stop: None,
        reload_pending: false,
        reload_gen: 0,
        max_gen: 0,
        ready: false,
        hook,
        watcher: None,
        instance: Some(instance),
    };
    actor.watch_config_files();
    actor.print_banner(&loaded);
    actor.ready = true;
    actor.shared.update_status();

    while let Some(msg) = app_rx.recv().await {
        actor.handle(msg).await;
    }
}

struct Actor {
    shared: Arc<Shared>,
    opts: RunOptions,
    app_tx: UnboundedSender<AppMsg>,
    recorder: Recorder,
    stopping: bool,
    start_after_stop: Option<Mode>,
    reload_pending: bool,
    reload_gen: u64,
    max_gen: u64,
    ready: bool,
    hook: Option<Hook>,
    watcher: Option<notify::RecommendedWatcher>,
    instance: Option<Instance>,
}

impl Actor {
    async fn handle(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Key(ev) => {
                let out = {
                    let mut h = self.shared.hotkey.lock().unwrap();
                    if ev.down {
                        h.key_down(ev.code)
                    } else {
                        h.key_up(ev.code)
                    }
                };
                if let Some((generation, delay)) = out.arm_tap_timer {
                    send_later(&self.app_tx, delay, AppMsg::TapTimeout(generation));
                }
                for event in out.events {
                    self.on_hotkey(event);
                }
            }
            AppMsg::TapTimeout(generation) => {
                let events = self.shared.hotkey.lock().unwrap().tap_timeout(generation);
                for event in events {
                    self.on_hotkey(event);
                }
            }
            AppMsg::Tray(action) => self.on_tray(action).await,
            AppMsg::Pipe((command, reply)) => {
                let answer = self.handle_command(&command);
                let _ = reply.send(answer);
            }
            AppMsg::ScheduleReload => {
                self.reload_gen += 1;
                // editors often write a file in several steps
                send_later(
                    &self.app_tx,
                    Duration::from_millis(400),
                    AppMsg::ReloadTimer(self.reload_gen),
                );
            }
            AppMsg::ReloadTimer(generation) => {
                if generation == self.reload_gen {
                    self.reload_config();
                }
            }
            AppMsg::MaxDuration(generation) => {
                if generation == self.max_gen && self.recorder.is_recording() {
                    let max = self.shared.pipeline().config.recording.max_duration_sec;
                    info!("Max duration ({}s) reached, stopping.", js_number(max));
                    self.shared.hotkey.lock().unwrap().reset();
                    let mode = self.shared.status.lock().unwrap().recording_mode;
                    self.stop_recording(true, mode);
                }
            }
            AppMsg::StopNow { keep, mode } => self.stop_now(keep, mode),
            AppMsg::Stopped { pcm, keep, mode } => {
                self.stopping = false;
                if keep {
                    let pcm = Arc::new(pcm);
                    self.shared.enqueue(match mode {
                        Mode::Command => Job::Command { pcm },
                        Mode::Dictation => Job::Dictation { pcm, retry: false },
                    });
                }
                self.shared.update_status();
                self.apply_pending_reload();
                if let Some(mode) = self.start_after_stop.take() {
                    self.start_recording(mode);
                }
            }
            AppMsg::Quit => self.quit().await,
            AppMsg::Restart => self.restart().await,
        }
    }

    fn on_hotkey(&mut self, event: HotkeyEvent) {
        match event {
            HotkeyEvent::Start(mode) => self.start_recording(mode),
            HotkeyEvent::Mode(mode) => {
                self.shared.status.lock().unwrap().recording_mode = mode;
                self.shared.play(Cue::Command);
                self.shared.update_status();
            }
            HotkeyEvent::Stop(mode) => self.stop_recording(true, mode),
            HotkeyEvent::Lock => {
                self.shared.play(Cue::Lock);
                info!("Hands-free mode: press the hotkey again to stop.");
            }
            HotkeyEvent::Cancel(reason) => {
                let mode = self.shared.status.lock().unwrap().recording_mode;
                self.stop_recording(false, mode);
                if reason == CancelReason::Tap {
                    self.shared.play(Cue::Cancel);
                }
                self.apply_pending_reload();
            }
            HotkeyEvent::AddWord => self.shared.enqueue(Job::AddWord(AddWordSource::Selection)),
        }
    }

    // -----------------------------------------------------------------------
    // Recording lifecycle
    // -----------------------------------------------------------------------

    fn start_recording(&mut self, mode: Mode) {
        if self.stopping {
            // still finishing the tail of the previous recording
            self.start_after_stop = Some(mode);
            return;
        }
        if self.recorder.is_recording() {
            return;
        }
        if let Err(e) = tokio::task::block_in_place(|| self.recorder.start()) {
            self.shared.hotkey.lock().unwrap().reset();
            self.shared.play(Cue::Error);
            error!("Could not start the microphone: {e}");
            self.shared
                .notify_error("Microphone unavailable", &e.to_string());
            self.shared.update_status();
            return;
        }
        {
            let mut st = self.shared.status.lock().unwrap();
            st.recording = true;
            st.recording_mode = mode;
        }
        self.shared.play(if mode == Mode::Command {
            Cue::Command
        } else {
            Cue::Start
        });
        self.shared.update_status();
        self.max_gen += 1;
        let max = self.shared.pipeline().config.recording.max_duration_sec;
        send_later(
            &self.app_tx,
            Duration::from_secs_f64(max),
            AppMsg::MaxDuration(self.max_gen),
        );
    }

    fn stop_recording(&mut self, keep: bool, mode: Mode) {
        self.max_gen += 1; // clears the max-duration timer
        if !self.recorder.is_recording() || self.stopping {
            return;
        }
        self.stopping = true;
        let tail = self.shared.pipeline().config.recording.tail_ms;
        if keep && tail > 0 {
            // Keep recording a bit after release so the last word isn't clipped.
            send_later(
                &self.app_tx,
                Duration::from_millis(tail),
                AppMsg::StopNow { keep, mode },
            );
        } else {
            self.stop_now(keep, mode);
        }
    }

    fn stop_now(&mut self, keep: bool, mode: Mode) {
        let Some(handle) = self.recorder.stop() else {
            self.stopping = false;
            return;
        };
        self.shared.status.lock().unwrap().recording = false;
        if keep {
            self.shared.play(Cue::Stop);
        }
        let tx = self.app_tx.clone();
        tokio::task::spawn_blocking(move || {
            let pcm = handle.finish();
            let _ = tx.send(AppMsg::Stopped { pcm, keep, mode });
        });
    }

    fn set_paused(&mut self, value: bool) {
        self.shared.status.lock().unwrap().paused = value;
        self.shared.hotkey.lock().unwrap().set_enabled(!value);
        if value {
            self.stop_recording(false, Mode::Dictation);
        }
        if let Some(ui) = &self.shared.ui {
            ui.set_paused(value);
        }
        info!(
            "{}",
            if value {
                "Dictation paused."
            } else {
                "Dictation resumed."
            }
        );
        self.shared.update_status();
    }

    // -----------------------------------------------------------------------
    // Config reload
    // -----------------------------------------------------------------------

    fn reload_config(&mut self) {
        // Don't swap the hotkey or pipeline in the middle of a hold; apply once it's over.
        let hotkey_busy = self.shared.hotkey.lock().unwrap().state() != State::Idle;
        if self.recorder.is_recording() || self.stopping || hotkey_busy {
            self.reload_pending = true;
            return;
        }
        self.reload_pending = false;
        let next = match load_config(&self.opts.config_args) {
            Ok(n) => n,
            Err(e) => {
                error!("Config not reloaded, keeping the previous settings:\n{e}");
                self.shared.play(Cue::Error);
                self.shared.notify_error(
                    "Config not reloaded",
                    &format!("{e}\nThe previous settings are still used."),
                );
                return;
            }
        };
        self.shared.install_pipeline(Pipeline::build(&next));
        if let Err(e) = self
            .shared
            .hotkey
            .lock()
            .unwrap()
            .configure(&next.config.hotkey)
        {
            error!("{e}");
        }
        self.recorder
            .set_device(next.config.recording.device.clone());
        {
            let mut sounds = self.shared.sounds.lock().unwrap();
            sounds.release();
            *sounds = Sounds::new(&next.config.sounds);
        }
        info!(
            "Config reloaded ({} dictionary term(s)).",
            next.dictionary.len()
        );
        self.shared.update_status();
    }

    fn apply_pending_reload(&mut self) {
        if self.reload_pending {
            self.reload_config();
        }
    }

    fn watch_config_files(&mut self) {
        use notify::{EventKind, RecursiveMode, Watcher, event::AccessKind, event::AccessMode};

        let p = self.shared.pipeline();
        let app_dir = paths().app_dir.clone();
        let config_file = p
            .config_path
            .clone()
            .unwrap_or_else(|| app_dir.join("config.yaml"));
        let key = |dir: &Path| dir.to_string_lossy().to_lowercase();
        let mut targets: HashMap<String, (PathBuf, HashSet<String>)> = HashMap::new();
        for file in [
            config_file,
            self.shared.base_dir.join(".env"),
            app_dir.join(".env"),
        ] {
            let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
                continue;
            };
            targets
                .entry(key(dir))
                .or_insert_with(|| (dir.to_path_buf(), HashSet::new()))
                .1
                .insert(name.to_string_lossy().to_lowercase());
        }
        let names: HashMap<String, HashSet<String>> = targets
            .iter()
            .map(|(k, (_, n))| (k.clone(), n.clone()))
            .collect();
        let tx = self.app_tx.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else {
                return;
            };
            // Reading the file must not trigger a reload; finishing a write should.
            if let EventKind::Access(kind) = event.kind
                && kind != AccessKind::Close(AccessMode::Write)
            {
                return;
            }
            let relevant = event
                .paths
                .iter()
                .any(|path| match (path.parent(), path.file_name()) {
                    (Some(dir), Some(name)) => names
                        .get(&key(dir))
                        .is_some_and(|n| n.contains(&name.to_string_lossy().to_lowercase())),
                    _ => false,
                });
            if relevant {
                let _ = tx.send(AppMsg::ScheduleReload);
            }
        });
        let mut watcher = match watcher {
            Ok(w) => w,
            Err(e) => {
                warn!("[config] Can't watch the config for changes: {e}. Use Restart instead.");
                return;
            }
        };
        for (dir, _) in targets.values() {
            let _ = std::fs::create_dir_all(dir);
            if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
                warn!(
                    "[config] Can't watch {} for changes: {e}. Use Restart instead.",
                    dir.display()
                );
            }
        }
        self.watcher = Some(watcher);
    }

    // -----------------------------------------------------------------------
    // Tray actions and control commands
    // -----------------------------------------------------------------------

    async fn on_tray(&mut self, action: TrayAction) {
        match action {
            TrayAction::CopyLast => {
                let sh = self.shared.clone();
                tokio::spawn(async move { jobs::copy_last(&sh).await });
            }
            TrayAction::RetryFailed => self.retry_failed(),
            TrayAction::AddClipboard => self.shared.enqueue(Job::AddWord(AddWordSource::Clipboard)),
            TrayAction::TogglePause => {
                let paused = self.shared.status.lock().unwrap().paused;
                self.set_paused(!paused);
            }
            TrayAction::Translate(index) => self.select_translation(index),
            TrayAction::OpenConfig => self.open_config(),
            TrayAction::Restart => self.restart().await,
            TrayAction::Quit => self.quit().await,
        }
    }

    fn handle_command(&mut self, command: &str) -> String {
        let has_ui = self.shared.ui.is_some();
        let reply = match command {
            "ping" => {
                if self.ready {
                    "pong"
                } else {
                    "starting"
                }
            }
            "quit" => {
                let _ = self.app_tx.send(AppMsg::Quit);
                "ok"
            }
            "restart" => {
                let _ = self.app_tx.send(AppMsg::Restart);
                "ok"
            }
            "show-log" => {
                if let Some(ui) = &self.shared.ui {
                    ui.show_log();
                }
                if has_ui { "ok" } else { "no-tray" }
            }
            // Same as the tray menu items (scriptable).
            "retry-failed" => {
                let has_failed = self.shared.status.lock().unwrap().last_failed.is_some();
                let _ = self.app_tx.send(AppMsg::Tray(TrayAction::RetryFailed));
                if has_failed { "ok" } else { "nothing-to-retry" }
            }
            "add-clipboard" => {
                let _ = self.app_tx.send(AppMsg::Tray(TrayAction::AddClipboard));
                "ok"
            }
            other => match other
                .strip_prefix("translate ")
                .and_then(|n| n.trim().parse::<i32>().ok())
            {
                Some(index) => {
                    let _ = self.app_tx.send(AppMsg::Tray(TrayAction::Translate(index)));
                    "ok"
                }
                None => "unknown-command",
            },
        };
        reply.to_string()
    }

    fn retry_failed(&mut self) {
        let Some(failed) = self.shared.status.lock().unwrap().last_failed.clone() else {
            return;
        };
        info!(
            "Retrying the recording from {}...",
            failed.ts.format("%-I:%M:%S %p")
        );
        self.shared.enqueue(Job::Dictation {
            pcm: failed.pcm,
            retry: true,
        });
    }

    fn select_translation(&mut self, index: i32) {
        let p = self.shared.pipeline();
        let pair = usize::try_from(index)
            .ok()
            .and_then(|i| p.pairs.get(i))
            .cloned();
        let state = {
            let mut st = self.shared.status.lock().unwrap();
            st.state.translation = pair.as_ref().map(|p| p.id.clone());
            st.state.clone()
        };
        save_state(&state);
        match &pair {
            Some(pair) => info!("Translation on: {}.", pair.label),
            None => info!("Translation off."),
        }
        self.shared.refresh_tray();
        self.shared.update_status();
    }

    fn open_config(&mut self) {
        let p = self.shared.pipeline();
        let file = match ensure_config_file(p.config_path.as_deref()) {
            Ok(f) => f,
            Err(e) => {
                error!("Could not create the config file: {e}");
                return;
            }
        };
        if p.config_path.is_none() {
            let _ = self.app_tx.send(AppMsg::ScheduleReload);
        }
        if let Err(e) = crate::cli::open_path(&file) {
            error!("Could not open {}: {e}", file.display());
        }
    }

    // -----------------------------------------------------------------------
    // Startup banner, restart, quit
    // -----------------------------------------------------------------------

    fn print_banner(&self, loaded: &LoadedConfig) {
        let p = self.shared.pipeline();
        let config = &p.config;
        let (label, command_label, add_word_label) = {
            let h = self.shared.hotkey.lock().unwrap();
            (h.label(), h.command_label(), h.add_word_label())
        };
        let polish_info = if p.polisher.is_some() {
            let host = reqwest::Url::parse(&config.polish.base_url)
                .ok()
                .and_then(|u| {
                    u.host_str().map(|h| match u.port() {
                        Some(port) => format!("{h}:{port}"),
                        None => h.to_string(),
                    })
                })
                .unwrap_or_else(|| config.polish.base_url.clone());
            let skipped = if config.polish.min_words > 0 {
                format!(" (skipped under {} words)", config.polish.min_words)
            } else {
                String::new()
            };
            format!("{} @ {host}{skipped}", config.polish.model)
        } else {
            "disabled".to_string()
        };
        let pair_info = if p.pairs.is_empty() {
            "none configured".to_string()
        } else {
            let current = self
                .shared
                .current_pair(&p)
                .map(|p| p.label)
                .unwrap_or_else(|| "off".into());
            let labels: Vec<&str> = p.pairs.iter().map(|x| x.label.as_str()).collect();
            format!("{} (currently: {current})", labels.join(", "))
        };
        let mut shortcuts = vec![format!(
            "hold {label} to dictate{}",
            if config.hotkey.hands_free_double_tap {
                " (double-tap: hands-free)"
            } else {
                ""
            }
        )];
        if let Some(c) = command_label {
            shortcuts.push(format!(
                "hold {c} for a command ({})",
                loaded.command_llm.model
            ));
        }
        if let Some(a) = add_word_label {
            shortcuts.push(format!("press {a} to add the selection to the dictionary"));
        }
        let config_line = match &p.config_path {
            Some(path) => path.display().to_string(),
            None => "(none, using defaults)".to_string(),
        };
        let follows = if config.recording.device.is_default() {
            " (follows the system default)"
        } else {
            ""
        };
        let history = if config.history.enabled {
            p.history.file().display().to_string()
        } else {
            "disabled".to_string()
        };
        let footer = if self.shared.ui.is_some() {
            "Use the tray icon to show this log, pause, restart or quit."
        } else {
            "Press Ctrl+C to quit."
        };
        say!(
            "wisprcheap ready\n  config:     {config_line} (reloaded automatically when saved)\n  hotkeys:    {}\n  mic:        {}{follows}\n  transcribe: {} / {} (language: {})\n  polish:     {polish_info}\n  translate:  {pair_info}\n  dictionary: {} term(s)\n  history:    {history}\n{footer}",
            shortcuts.join("\n              "),
            self.recorder.current_device_name(),
            p.transcriber.provider.as_str(),
            p.transcriber.model,
            config.transcription.language,
            p.dictionary.len(),
        );
    }

    async fn teardown(&mut self) {
        if let Some(hook) = self.hook.take() {
            hook.stop();
        }
        self.max_gen += 1;
        self.reload_gen += 1;
        self.watcher = None;
        // Let a dictation that's being transcribed finish and paste.
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.shared.status.lock().unwrap().pending > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.recorder.release();
        self.shared.sounds.lock().unwrap().release();
        if let Some(instance) = self.instance.take() {
            instance.close();
        }
    }

    fn exit(&self) -> ! {
        if let Some(ui) = &self.shared.ui {
            // The UI thread removes the tray icon, then ends the process.
            ui.exit();
            std::thread::sleep(Duration::from_secs(3));
        }
        std::process::exit(0);
    }

    async fn quit(&mut self) {
        info!("Quitting...");
        self.teardown().await;
        info!("Bye.");
        self.exit();
    }

    async fn restart(&mut self) {
        info!("Restarting (the new instance runs in the background)...");
        self.teardown().await;
        let mut args: Vec<String> = vec!["run".into()];
        args.extend(
            self.opts
                .run_args
                .iter()
                .filter(|a| *a != "--restarted")
                .cloned(),
        );
        args.push("--restarted".into());
        if let Err(e) = crate::cli::spawn_background(&args) {
            error!("Could not restart: {e}");
        }
        self.exit();
    }
}
