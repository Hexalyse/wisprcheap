//! Command line: `wisprcheap [start|run|stop|devices|stats|shortcut|sounds|wayland]`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use crate::app::{self, RunOptions};
use crate::config::{ConfigArgs, load_config, resolve_base_dir};
use crate::history::{History, HistoryEntry, entry_month, read_history};
use crate::instance::send_command;
use crate::logging::{self, last_session_lines, log_file_path};

const USAGE: &str = "wisprcheap: push-to-talk dictation

Usage: wisprcheap [command] [options]

Commands:
  start       Start in the background, with the tray icon (default)
  run         Run in the foreground (logs in this terminal)
  stop        Quit the running instance
  devices     List audio input/output devices
  stats       Monthly cost summary from the history
  shortcut    Create a desktop shortcut (and a menu entry on Linux)
  sounds      Play every sound cue
  wayland     Linux: check the keyboard access needed on Wayland, and show the one-time setup
  help        Show this help

Options:
  -c, --config <file>   Config file (default: config.yaml in the app directory, or WISPRCHEAP_CONFIG)
  --no-tray             run: no tray icon (Ctrl+C to quit)
  --gui                 start: report through dialogs, show the log if already running";

struct Args {
    command: String,
    config: ConfigArgs,
    no_tray: bool,
    restarted: bool,
    gui: bool,
    /// Options to pass on to `run` (config file, --no-tray).
    passthrough: Vec<String>,
}

fn parse_args(raw: Vec<String>, gui_default: bool) -> Result<Args> {
    let mut args = Args {
        command: String::new(),
        config: ConfigArgs::default(),
        no_tray: false,
        restarted: false,
        gui: gui_default,
        passthrough: Vec::new(),
    };
    let mut it = raw.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-c" | "--config" => {
                let file = it
                    .next()
                    .ok_or_else(|| anyhow!("{arg} needs a file name"))?;
                args.passthrough.push("--config".into());
                args.passthrough.push(file.clone());
                args.config.config = Some(file);
            }
            "--no-tray" => {
                args.no_tray = true;
                args.passthrough.push(arg);
            }
            "--restarted" => args.restarted = true,
            "--gui" => args.gui = true,
            "--stop" => args.command = "stop".into(),
            "-h" | "--help" => args.command = "help".into(),
            "-V" | "--version" => args.command = "version".into(),
            s if s.starts_with('-') => return Err(anyhow!("unknown option {s}")),
            s if args.command.is_empty() => args.command = s.to_string(),
            s => return Err(anyhow!("unexpected argument {s}")),
        }
    }
    if args.command.is_empty() {
        args.command = "start".into();
    }
    Ok(args)
}

/// Entry point of both executables. `gui_binary`: the no-console Windows build (`wisprcheapw`).
pub fn main(gui_binary: bool) {
    let args = match parse_args(std::env::args().skip(1).collect(), gui_binary) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let code = match args.command.as_str() {
        "run" => run(args),
        "start" => start(args),
        "stop" => stop(),
        "devices" => devices(),
        "stats" => stats(&args),
        "shortcut" => shortcut(&args),
        "sounds" => sounds(),
        "wayland" => {
            let (report, ok) = crate::keyboard::setup_report();
            println!("{report}");
            if ok { 0 } else { 1 }
        }
        "help" => {
            println!("{USAGE}");
            0
        }
        "version" => {
            println!("wisprcheap {}", env!("CARGO_PKG_VERSION"));
            0
        }
        other => {
            eprintln!("Unknown command \"{other}\".\n\n{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

// ---------------------------------------------------------------------------
// run (the app itself)
// ---------------------------------------------------------------------------

fn run(args: Args) -> i32 {
    logging::setup(&log_file_path(&resolve_base_dir(&args.config)));
    std::panic::set_hook(Box::new(|info| {
        crate::error!("Fatal error: {info}");
    }));

    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut opts = RunOptions {
        config_args: args.config.clone(),
        use_tray: !args.no_tray,
        restarted: args.restarted,
        run_args: args.passthrough.clone(),
    };
    // Without a display (no X11/Wayland session), keep running without the tray icon.
    #[cfg(target_os = "linux")]
    if opts.use_tray && gtk::init().is_err() {
        crate::warn!(
            "[tray] Can't open the display (GTK failed to initialize): running without the tray icon."
        );
        opts.use_tray = false;
    }
    let rt = runtime();
    let prepared = match rt.block_on(app::prepare(&opts)) {
        Ok(p) => p,
        Err(message) => {
            crate::warn!("{message}");
            return 1;
        }
    };

    if !opts.use_tray {
        rt.block_on(app::run(prepared, opts, None, None));
        return 0;
    }

    let (event_loop, ui) = crate::tray::create_event_loop();
    {
        let ui = ui.clone();
        logging::add_sink(move |line| ui.log(line));
    }
    let (tray_tx, tray_rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = rt.spawn(app::run(prepared, opts, Some(ui.clone()), Some(tray_rx)));
    rt.spawn(async move {
        if let Err(e) = handle.await {
            crate::error!("Fatal error: {e}");
            std::process::exit(1);
        }
    });
    crate::tray::run(event_loop, ui, tray_tx, rt)
}

// ---------------------------------------------------------------------------
// start / stop (launcher)
// ---------------------------------------------------------------------------

/// The executable to run in the background: `wisprcheapw.exe` (no console) when it's next to us on Windows.
fn background_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    #[cfg(windows)]
    {
        let gui = exe.with_file_name("wisprcheapw.exe");
        if gui.is_file() {
            return Ok(gui);
        }
    }
    Ok(exe)
}

/// Start a detached copy of this program (no console window, no stdio).
pub fn spawn_background(args: &[String]) -> Result<std::process::Child> {
    let mut cmd = Command::new(background_exe()?);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    Ok(cmd.spawn()?)
}

/// Open a file with its default application.
pub fn open_path(path: &Path) -> Result<()> {
    #[cfg(windows)]
    let mut cmd = Command::new("explorer.exe");
    #[cfg(not(windows))]
    let mut cmd = Command::new("xdg-open");
    cmd.arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

fn message_box(message: &str, is_error: bool) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW,
        };
        let text: Vec<u16> = message.encode_utf16().chain([0]).collect();
        let title: Vec<u16> = "wisprcheap".encode_utf16().chain([0]).collect();
        let icon = if is_error {
            MB_ICONERROR
        } else {
            MB_ICONINFORMATION
        };
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | icon,
            );
        }
    }
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        if gtk::init().is_ok() {
            let kind = if is_error {
                gtk::MessageType::Error
            } else {
                gtk::MessageType::Info
            };
            let dialog = gtk::MessageDialog::new(
                None::<&gtk::Window>,
                gtk::DialogFlags::MODAL,
                kind,
                gtk::ButtonsType::Ok,
                message,
            );
            dialog.set_title("wisprcheap");
            dialog.run();
            unsafe {
                dialog.destroy();
            }
        } else {
            eprintln!("{message}");
        }
    }
}

fn start(args: Args) -> i32 {
    let gui = args.gui;
    let log_file = log_file_path(&resolve_base_dir(&args.config));
    let fail = |message: String| -> i32 {
        if gui {
            message_box(&message, true);
        } else {
            eprintln!("{message}");
        }
        1
    };
    let rt = runtime();
    rt.block_on(async {
        let existing = send_command(if gui { "show-log" } else { "ping" }, Duration::from_secs(2)).await;
        if existing.is_some() {
            if !gui {
                println!("wisprcheap is already running (see the tray icon). Use `wisprcheap stop` to quit it.");
            }
            return 0;
        }

        let mut run_args = vec!["run".to_string()];
        run_args.extend(args.passthrough.iter().cloned());
        let mut child = match spawn_background(&run_args) {
            Ok(c) => c,
            Err(e) => return fail(format!("Could not start wisprcheap: {e}")),
        };

        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if let Ok(Some(status)) = child.try_wait() {
                let details = last_session_lines(&log_file, 25).join("\n");
                let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                let details = if details.is_empty() {
                    format!("See {}", log_file.display())
                } else {
                    details
                };
                return fail(format!("wisprcheap failed to start (exit code {code}).\n\n{details}"));
            }
            if send_command("ping", Duration::from_secs(1)).await.as_deref() == Some("pong") {
                if !gui {
                    println!(
                        "wisprcheap is running in the background: use the tray icon, or `wisprcheap stop` to quit.\nLog: {}",
                        log_file.display()
                    );
                }
                return 0;
            }
        }
        fail(format!("wisprcheap did not become ready within 30 s. See {}", log_file.display()))
    })
}

fn stop() -> i32 {
    let reply = runtime().block_on(send_command("quit", Duration::from_secs(2)));
    println!(
        "{}",
        if reply.is_none() {
            "wisprcheap is not running."
        } else {
            "wisprcheap is quitting."
        }
    );
    0
}

// ---------------------------------------------------------------------------
// devices / stats / sounds
// ---------------------------------------------------------------------------

fn devices() -> i32 {
    println!("Input devices (use the index or part of the name as recording.device):");
    for (i, name) in crate::recorder::list_input_devices().iter().enumerate() {
        println!("  {i}: {name}");
    }
    println!("\nOutput devices (sound cues use the system default):");
    for (i, name) in crate::recorder::list_output_devices().iter().enumerate() {
        println!("  {i}: {name}");
    }
    0
}

fn usd(n: f64) -> String {
    if n < 1.0 {
        format!("${n:.4}")
    } else {
        format!("${n:.2}")
    }
}

fn stats(args: &Args) -> i32 {
    let loaded = match load_config(&args.config) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let file = History::new(&loaded.config.history, &loaded.base_dir)
        .file()
        .to_path_buf();
    let entries: Vec<HistoryEntry> = read_history(&file)
        .into_iter()
        .filter(|e| e.error.is_none() && e.words > 0)
        .collect();
    if entries.is_empty() {
        println!("No successful dictations in {} yet.", file.display());
        return 0;
    }
    let mut by_month: BTreeMap<String, Vec<&HistoryEntry>> = BTreeMap::new();
    for e in &entries {
        if let Some(month) = entry_month(e) {
            by_month.entry(month).or_default().push(e);
        }
    }
    println!("History: {}\n", file.display());
    println!(
        "Month     Dictations    Words   Audio min   Transcribe      Polish       Total   Per 10k words"
    );
    for (month, list) in &by_month {
        let words: usize = list.iter().map(|e| e.words).sum();
        let minutes: f64 = list.iter().map(|e| e.duration_sec).sum::<f64>() / 60.0;
        let stt: f64 = list
            .iter()
            .map(|e| e.cost_usd.transcription.unwrap_or(0.0))
            .sum();
        let polish: f64 = list.iter().map(|e| e.cost_usd.polish.unwrap_or(0.0)).sum();
        let total = stt + polish;
        println!(
            "{:<9} {:>10} {:>8} {:>11} {:>12} {:>11} {:>11} {:>15}",
            month,
            list.len(),
            words,
            format!("{minutes:.1}"),
            usd(stt),
            usd(polish),
            usd(total),
            usd(total / words.max(1) as f64 * 10_000.0),
        );
    }
    let unknown = entries
        .iter()
        .filter(|e| e.cost_usd.total.is_none())
        .count();
    if unknown > 0 {
        println!(
            "\n{unknown} dictation(s) used a model without a known price and are counted as $0."
        );
    }
    0
}

fn sounds() -> i32 {
    use crate::sounds::{Cue, Sounds};
    let sounds = Sounds::new(&crate::config::SoundsConfig {
        enabled: true,
        volume: 0.25,
    });
    for cue in [
        Cue::Start,
        Cue::Stop,
        Cue::Lock,
        Cue::Command,
        Cue::Added,
        Cue::Cancel,
        Cue::Error,
    ] {
        println!("{cue:?}");
        sounds.play_blocking(cue);
        std::thread::sleep(Duration::from_millis(250));
    }
    0
}

// ---------------------------------------------------------------------------
// shortcut
// ---------------------------------------------------------------------------

fn shortcut(args: &Args) -> i32 {
    let icon_dir = crate::icons::write_icons(&crate::paths::icon_dir());
    let exe = match background_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Could not find the executable: {e}");
            return 1;
        }
    };
    let mut launch_args = vec!["start".to_string(), "--gui".to_string()];
    launch_args.extend(
        args.passthrough
            .iter()
            .filter(|a| *a != "--no-tray")
            .cloned(),
    );
    match create_shortcut(&exe, &launch_args, &icon_dir) {
        Ok(created) => {
            for path in created {
                println!("Created {}", path.display());
            }
            println!(
                "It starts {}; run `wisprcheap shortcut` again if you move it.",
                exe.display()
            );
            0
        }
        Err(e) => {
            eprintln!("Could not create the shortcut:\n{e}");
            1
        }
    }
}

#[cfg(windows)]
fn create_shortcut(exe: &Path, args: &[String], icon_dir: &Path) -> Result<Vec<PathBuf>> {
    let quoted: Vec<String> = args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect();
    let script = r#"
$desktop = [Environment]::GetFolderPath('Desktop')
$link = Join-Path $desktop 'wisprcheap.lnk'
$shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($link)
$shortcut.TargetPath = $env:WC_TARGET
$shortcut.Arguments = $env:WC_ARGS
$shortcut.WorkingDirectory = $env:WC_DIR
$shortcut.IconLocation = $env:WC_ICON + ',0'
$shortcut.Description = 'Push-to-talk dictation (runs in the tray)'
$shortcut.Save()
$link
"#;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("WC_TARGET", exe)
        .env("WC_ARGS", quoted.join(" "))
        .env("WC_DIR", exe.parent().unwrap_or(Path::new(".")))
        .env("WC_ICON", icon_dir.join("idle.ico"))
        .output()?;
    if !output.status.success() {
        return Err(anyhow!("{}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(vec![PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    )])
}

#[cfg(not(windows))]
fn create_shortcut(exe: &Path, args: &[String], icon_dir: &Path) -> Result<Vec<PathBuf>> {
    use std::os::unix::fs::PermissionsExt;
    let quote = |s: &str| {
        if s.chars()
            .any(|c| c.is_whitespace() || "\"'\\$`".contains(c))
        {
            format!(
                "\"{}\"",
                s.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('$', "\\$")
                    .replace('`', "\\`")
            )
        } else {
            s.to_string()
        }
    };
    let exec: Vec<String> = std::iter::once(exe.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|a| quote(&a))
        .collect();
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=wisprcheap\nComment=Push-to-talk dictation (runs in the tray)\nExec={}\nIcon={}\nTerminal=false\nCategories=Utility;\n",
        exec.join(" "),
        icon_dir.join("idle.png").display()
    );
    let mut created = Vec::new();
    if let Some(data) = dirs::data_dir() {
        let apps = data.join("applications");
        std::fs::create_dir_all(&apps)?;
        let file = apps.join("wisprcheap.desktop");
        std::fs::write(&file, &entry)?;
        created.push(file);
    }
    if let Some(desktop) = dirs::desktop_dir().filter(|d| d.is_dir()) {
        let file = desktop.join("wisprcheap.desktop");
        std::fs::write(&file, &entry)?;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))?;
        created.push(file);
    }
    if created.is_empty() {
        return Err(anyhow!("no applications or desktop directory found"));
    }
    Ok(created)
}
