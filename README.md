# wisprcheap (Rust)

A minimal, pay-per-use take on Wispr Flow / Typeless for **Windows and Linux**:
hold **Ctrl + Win**, speak, release. The audio is transcribed (ElevenLabs Scribe v2 or OpenAI),
cleaned up by a cheap LLM ("polish" pass), copied to the clipboard and pasted into the focused text field.

This is a native Rust rewrite of the (now archived) [TypeScript/Node version](https://github.com/Hexalyse/wisprcheap-ts),
with the same config file, prompts, history format and tray menu, plus an optional desktop companion.

- Push-to-talk, or **double-tap** the hotkey for hands-free mode (tap again to stop)
- **Command mode** (Ctrl + Win + Alt): select text and say "make this more formal", "translate to English"...,
  or say "write a short reply saying I'll be late" with nothing selected
- Dictionary of names and technical terms: sent to Scribe as `keyterms` (or to OpenAI as a `prompt`) and to the polish model.
  Add a word by selecting it and pressing **Ctrl + Win + Shift**
- Automatic language detection, and the polish pass never translates, unless you turn on **translation mode**
  (French → English, etc.: pairs configured in `config.yaml`, chosen from the tray)
- Short dictations can skip the polish step and paste ~2 s sooner (`polish.minWords`)
- Short sound cues for start, stop, hands-free, command, cancel and error, plus a desktop notification when something fails
- A small [overlay](#recording-overlay) at the bottom of the screen while recording (with a live waveform) and transcribing
- `history.jsonl` log with raw and polished text, timings and estimated cost, plus `wisprcheap stats`
- Runs in the background with a tray icon: status color, log window, pause, retry a failed dictation...
- Follows the default microphone and speakers (plug in a headset, it's used from the next dictation)
- On-demand Iced desktop UI: visual settings, dictionary, translation pairs, searchable history and activity stats

## Download

Prebuilt binaries for Windows and Linux (x86_64) are on the [releases page](https://github.com/Hexalyse/wisprcheap/releases).
Extract the archive anywhere, then follow [Setup](#setup). On Linux, install the runtime libraries first
(Debian/Ubuntu: `sudo apt install libgtk-3-0 libayatana-appindicator3-1 libasound2 libxdo3 libxkbcommon0 libxkbcommon-x11-0 libgtk-layer-shell0`).

Windows SmartScreen may warn about an unrecognized app the first time, because the executables aren't code-signed
(**More info** > **Run anyway**).

## Build

Requires Rust 1.88+ for the desktop companion (edition 2024).

**Windows**: nothing else (MSVC toolchain).

**Linux** (X11 or Wayland session, see [Linux notes](#linux-notes)): the GTK 3, AppIndicator, ALSA, X11/XTest and xdo development packages. On Debian/Ubuntu:

```sh
sudo apt install build-essential pkg-config libgtk-3-dev libayatana-appindicator3-dev \
  libasound2-dev libx11-dev libxtst-dev libxi-dev libxdo-dev libdbus-1-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libgtk-layer-shell-dev
```

Then:

```sh
cargo build --release
```

This produces three executables in `target/release/`:

- `wisprcheap`: the command line (and the app itself).
- `wisprcheapw`: the same program without a console window on Windows. It's what the background instance and the
  desktop shortcut use. It makes no difference on Linux.
- `wisprcheap-ui`: a separate Iced companion, created only when opened and exited when closed.
  Keep it beside the dictation executables. `cargo build --release -p wisprcheap` builds only the background app.

## Setup

1. Put your API keys in a `.env` file (or in `config.yaml`, or in the environment):
   ```
   ELEVENLABS_API_KEY=...
   OPENAI_API_KEY=...
   ```
2. Optionally copy `config.example.yaml` to `config.yaml` and edit it. Every setting has a default, and
   **Open config.yaml** in the tray creates it from the example if it doesn't exist.
3. Run `wisprcheap shortcut` (optional: adds a desktop shortcut, and a menu entry on Linux), then `wisprcheap start`.
4. Linux on Wayland: run `wisprcheap wayland` and follow the [one-time setup](#wayland) it prints.

Alternatively, run `wisprcheap ui`, enter provider keys in **Settings**, save, then choose **Start dictation**.
Keys entered in the UI are saved in your local YAML file. Existing `.env` keys and `${VAR}` references are preserved.
See [desktop companion details](docs/desktop-ui.md) for architecture, current scope and validation notes.

**Where files live.** If a `config.yaml` sits next to the executable, that directory is used (portable mode:
config, `.env`, history, log, and a `.cache/` folder). Otherwise:

| | Config, `.env`, history, log | Cache (icons, remembered translation choice) |
| --- | --- | --- |
| Windows | `%APPDATA%\wisprcheap` | `%LOCALAPPDATA%\wisprcheap` |
| Linux | `~/.config/wisprcheap` | `~/.cache/wisprcheap` |

`--config <file>` (or `WISPRCHEAP_CONFIG`) picks another config file; `.env`, `history.jsonl`, `recordings/` and
`wisprcheap.log` then live next to it.

| Command               | What it does                                                         |
| --------------------- | -------------------------------------------------------------------- |
| `wisprcheap start`    | Start in the background and return (the default command; `--config <file>` to pick a config) |
| `wisprcheap stop`     | Quit the running instance                                            |
| `wisprcheap run`      | Run in the current terminal instead (Ctrl+C quits), still with the tray icon (`--no-tray` without) |
| `wisprcheap ui`       | Open or focus the desktop companion; works while dictation is stopped |
| `wisprcheap shortcut` | Create or update the desktop shortcut                                |
| `wisprcheap wayland`  | Linux: check the keyboard access needed on Wayland and print the [setup](#wayland) |
| `wisprcheap devices`  | List microphones (for `recording.device`) and speakers              |
| `wisprcheap stats`    | Words, audio minutes and estimated cost per month                    |
| `wisprcheap sounds`   | Play every sound cue                                                 |

Only one dictation instance and one companion run at a time. The desktop shortcut opens or focuses the companion.
Re-run `wisprcheap shortcut` to update an older shortcut. Output is written to
`wisprcheap.log` (rotated at 1 MB).

## Tray icon

The icon color shows the state: **grey** ready, **red** recording, **amber** transcribing/polishing, **light grey with a slash** paused.

The top of the menu shows dictation status. Activity, costs and sync controls are in the companion UI.

- **Left-click** (Windows) or **Open WisprCheap** opens the companion. Closing it exits the companion process; dictation keeps running.
- **Copy last dictation** puts the last polished text back on the clipboard.
- **Retry last failed** re-sends the last recording whose transcription failed (e.g. network or quota error).
  The result goes to the clipboard, since focus is on the tray at that moment.
- **Translate dictation** (only shown when `translation.pairs` is set): pick a pair, or Off. The choice is remembered.
- **Add clipboard to dictionary** adds the copied word or phrase to `config.yaml`.
- **Pause dictation** ignores the shortcuts until you resume.
- **Open config.yaml** opens it in your default editor. Saved changes apply immediately.
- **Restart** fully restarts the app (not needed for config changes).
- **Quit** waits for a dictation in progress to finish, then exits and closes the companion. Unsaved settings retain their close confirmation.

When something fails (transcription, command, translation, microphone, config reload...), a notification
says what happened; click it to open the log. Turn it off with `notifications.errors: false`.

## Recording overlay

The recording overlay uses native Win32/GTK windows, with Layer Shell on supported Wayland compositors.
Iced runs only in the companion; the background executable does not depend on Iced, Winit or a webview.

While you speak, a small pill at the bottom center of the screen (above the taskbar or panel)
shows a live waveform: red with a microphone for a dictation, indigo with sparkles for a command, plus a
padlock in hands-free mode. It then shrinks to a spinner while the audio is transcribed and polished, and briefly shows
how it went: a check (pasted), a clipboard (only copied), a crossed-out microphone (nothing heard) or a shaking "!" (error).

It never takes the focus and clicks go through it. Turn it off with `overlay.enabled: false`.

## Translation mode

List the pairs you want in `config.yaml`; they appear under **Translate dictation** in the tray:

```yaml
translation:
  pairs:
    - { from: fr, to: en }   # speak French, paste English
    - { from: en, to: fr }
    - { to: de }             # any language -> German
```

While a pair is selected, every dictation (even a short one) is cleaned up and translated in one LLM call, and `from`
is sent to the transcriber as the spoken language. Command mode is not affected. Translation uses the polish model
unless `translation.model` is set (e.g. `gpt-6-sol` with `reasoningEffort: low` for more natural translations, but slower).

## How it behaves

| You do                                   | It does                                                        |
| ---------------------------------------- | -------------------------------------------------------------- |
| Hold Ctrl+Win, talk, release             | Rising beep, records, falling beep, pastes the result about 1-2 s later |
| Tap Ctrl+Win twice quickly               | Triple beep: hands-free recording. Press Ctrl+Win again to finish |
| Select text, hold Ctrl+Win+Alt, say an instruction, release | The selection is replaced by the rewritten text |
| Same with nothing selected               | The requested text is written at the cursor                    |
| Press Alt while dictating with Ctrl+Win  | Switches that recording to command mode (quick rising arpeggio) |
| Select a word, press Ctrl+Win+Shift      | Adds it to the dictionary (two high beeps; a low beep if it was already there) |
| Ctrl+Win + another key (Left, D...)      | The recording is cancelled and the system shortcut works as usual |
| A single short tap                       | Low beep, nothing is sent                                      |
| Transcription fails                      | Error buzz. The audio is kept in `recordings/` and can be retried from the tray |
| Polish fails or times out                | The raw transcript is pasted instead                           |

Pasting waits until you've released the hotkey, so Win+V (clipboard history) is never triggered by accident.

Command mode and the add-word shortcut read the selection with a simulated **Ctrl+C** after you release the keys.
Two consequences: in a terminal with nothing selected, Ctrl+C interrupts the running program; and some editors
(VS Code...) copy the whole current line when nothing is selected, which command mode then treats as the selection.

### Command mode model

By default, command mode uses the polish model (gpt-6-luna). Rewrites, tone changes and translations benefit from
a stronger model, and since commands are occasional, it stays cheap:

```yaml
command:
  model: gpt-6-sol
  reasoningEffort: low
```

Dictation keeps using the fast polish model, so its latency doesn't change. Any OpenAI-compatible provider works
here too (`command.baseUrl` / `command.apiKey`).

## Costs

List prices, September 2026. At about 140 words per minute, **10,000 words is about 70 minutes of audio**.

| Step                          | Model                          | Per 10,000 words |
| ----------------------------- | ------------------------------ | ---------------- |
| Transcription (default)       | Scribe v2 ($0.22/h) + keyterms ($0.05/h) | ~$0.32 |
| Transcription (alternatives)  | gpt-4o-transcribe / gpt-transcribe / gpt-4o-mini-transcribe | ~$0.43 / ~$0.32 / ~$0.21 |
| Polish (default)              | gpt-6-luna ($0.10 / $0.50 per 1M tokens) | ~$0.02 |
| Polish (alternatives)         | gpt-4.1-mini / gpt-5.4-mini    | ~$0.08 / ~$0.17  |
| **Total (default)**           |                                | **~$0.34**       |

Command mode is billed per command: about $0.0002 with gpt-6-luna, $0.003-0.005 with gpt-6-sol (low).
`wisprcheap stats` shows your real numbers based on the history file. The companion's Overview and History pages let you switch between **Current device** and **All devices** for the selected period. All-device history is loaded from sync on demand, including when `sync.history` is `upload`, and merged with local recordings without double-counting.

Models missing from the built-in price table (or with other prices) can be listed in `config.yaml`:

```yaml
pricing:
  overrides:
    - { model: llama-3.3-70b-versatile, inputPerM: 0.59, outputPerM: 0.79 }
    - { model: my-whisper, perMinute: 0.004 }
```

## Sync (optional)

Keep several computers and the [Android app](https://github.com/Hexalyse/wisprcheap-android) in sync through
your own [wisprcheap sync server](server/README.md): settings, API keys, dictionary, translation pairs, price
overrides, and the history (with totals for all your devices). Everything is **end-to-end encrypted** with a sync
passphrase the server never sees; only the history statistics (dates, durations, models, word counts, costs) are
readable by the server, so it can show them on its web page.

1. On the server's web page, click **Connect a device**: it shows a QR code and an 8-character code.
2. On this computer:

   ```
   wisprcheap sync pair https://sync.example.com ABCD-2345
   ```

   The first device chooses the sync passphrase; the next ones ask for it. The first sync merges this computer's
   settings with the server's: the server's values win, local dictionary terms, pairs and prices are added, and a
   copy of the previous file is kept (`config.yaml.bak-<date>`).

Then the app syncs by itself: at startup, a few seconds after you save `config.yaml` or `.env`, after dictations,
every 15 minutes, and from **Sync now** in the companion's Overview (which also shows the sync status). Changes from other devices
are written into `config.yaml` (comments and layout are kept) and API keys into `.env` next to it.

What's synced: transcription, cleanup (polish), command and translation settings, the five API keys, the
dictionary, translation pairs and `pricing.overrides`. Hotkeys, microphone, sounds, overlay, output, history options and the
`sync` section itself stay per device. `sync.history` chooses what happens to the history: `upload` (default),
`download` (also add the other devices' entries to `history.jsonl`) or `off`.

| Command                          | What it does                                                  |
| -------------------------------- | ------------------------------------------------------------- |
| `wisprcheap sync status`         | Server, device, last sync, connection and key check           |
| `wisprcheap sync now`            | Sync right away and print what changed                        |
| `wisprcheap sync passphrase`     | Change the sync passphrase (other devices keep working)       |
| `wisprcheap sync unlock`         | Enter the passphrase again after it was reset on another device |
| `wisprcheap sync rename <name>`  | Rename this device on the server                              |
| `wisprcheap sync unpair`         | Disconnect this computer (local files stay as they are)       |

`wisprcheap sync pair` writes the device token and this device's copy of the encryption key into the `sync`
section of `config.yaml`: keep that file private, like `.env`.

## Notes

- Keyterms must be under 50 characters, at most 5 words, and contain none of `< > { } [ ] \`. Invalid entries are only sent to the polish model.
  With more than 100 keyterms, ElevenLabs bills every request at least 20 seconds.
- The keyboard hook observes keys but never blocks them, so the hotkey also reaches the focused app.
  The app never injects keys while the hotkey is held, because Ctrl+Win+<key> combinations can trigger system shortcuts
  (for example, Ctrl+Win+F24 toggles the touchpad on Windows).
- Windows won't let a normal process send keystrokes into elevated (admin) windows, so pasting into those only copies to the clipboard.
- Audio is captured in the device's native format and resampled to 16 kHz mono (windowed-sinc) before being sent.
- The control channel (single instance, `start`/`stop`) is a per-user named pipe on Windows (`\\.\pipe\wisprcheap-<user>`,
  the same one the TypeScript version uses, so only one of the two runs at a time) and a Unix socket in
  `$XDG_RUNTIME_DIR` on Linux.
- `cargo test` runs the unit tests (config, dictionary editing, prompts, pricing, audio, the hotkey state machine,
  the overlay's states). `cargo test overlay_previews -- --ignored` renders every look of the overlay to
  `target/overlay-preview/`.
  On Linux, `cargo test -- --ignored virtual_keyboards` also tests reading and typing through virtual keyboards
  (needs access to `/dev/uinput`, e.g. as root).
  `WISPRCHEAP_NO_INJECT=1` never sends Ctrl+C / Ctrl+V, and `WISPRCHEAP_INSTANCE=<name>` runs a separate instance,
  which is useful for manual end-to-end tests next to a real instance.
- On Linux, `python3 tests/wayland_overlay.py` checks the actual native Wayland overlay in an isolated Sway
  session (install `sway grim xvfb xauth` first). It verifies click-through, focus, hide/show and scaling,
  and writes screenshots and protocol logs to `target/wayland-overlay/`. The Linux release workflow runs it too.

### Linux notes

- On X11, global hotkeys and key injection use XRecord / XTest, like the original libuiohook. No setup needed.
- On Wayland, see [below](#wayland).
- The tray uses AppIndicator / StatusNotifierItem. GNOME needs the "AppIndicator and KStatusNotifierItem Support" extension.
  AppIndicators don't report left clicks: use **Open WisprCheap** in the menu, then **Session log** for logs. Notifications go through D-Bus
  (`org.freedesktop.Notifications`).
- Without a display, the app runs without the tray icon and logs why.
- The [recording overlay](#recording-overlay) works on X11 with compositing and on Wayland compositors that
  support Layer Shell (KDE Plasma, Sway, Hyprland and others). On Wayland, the compositor chooses the monitor;
  the pill is anchored at the bottom center, respects space reserved by panels, and doesn't take focus or intercept clicks.
  Compositors without Layer Shell, including GNOME, omit the overlay and explain why in the log; dictation still works.
  Linux requires GTK Layer Shell 0.6 or newer (`libgtk-layer-shell0` on Debian/Ubuntu).
- The clipboard works on X11 and Wayland (data-control protocol, with XWayland's clipboard as the fallback).

### Wayland

Wayland doesn't let apps see global keys or type into other windows. So on Wayland, wisprcheap reads the keyboards
directly from `/dev/input` (without blocking them: keys still reach the focused app), and types Ctrl+V / Ctrl+C through
a virtual keyboard (`/dev/uinput`), which the compositor treats like a real one. This works in every app, on every
compositor (GNOME, KDE, Sway, Hyprland...). The same approach is used by `ydotool`, `keyd` and `espanso`.

It needs a one-time setup to give your user access to those devices. `wisprcheap wayland` checks the access and prints
these commands. Pick one option:

**Option 1 (recommended): a udev rule.** Only the user sitting at the computer (the active local session) gets access,
and there's nothing to log out of:

```sh
sudo tee /etc/udev/rules.d/70-wisprcheap.rules <<'EOF'
SUBSYSTEM=="input", ENV{ID_INPUT_KEYBOARD}=="1", TAG+="uaccess"
KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"
EOF
echo uinput | sudo tee /etc/modules-load.d/wisprcheap.conf
sudo modprobe uinput
sudo udevadm control --reload-rules && sudo udevadm trigger
```

**Option 2: the `input` group.** Simpler, but the access applies to all your sessions (SSH too), and it only takes
effect after logging out and back in:

```sh
sudo usermod -aG input "$USER"
sudo tee /etc/udev/rules.d/70-wisprcheap.rules <<'EOF'
KERNEL=="uinput", SUBSYSTEM=="misc", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"
EOF
echo uinput | sudo tee /etc/modules-load.d/wisprcheap.conf
sudo modprobe uinput
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Then restart wisprcheap (tray > **Restart**). The log says `Reading N keyboard(s) from /dev/input, typing through a
virtual keyboard`. To undo it, delete `/etc/udev/rules.d/70-wisprcheap.rules` and `/etc/modules-load.d/wisprcheap.conf`
(and `sudo gpasswd -d "$USER" input` for option 2).

Good to know:

- **Security**: this lets any program running as your user read keystrokes (including passwords) and type keys.
  X11 always allowed that; Wayland is designed to prevent it.
- Without the setup, wisprcheap falls back to X11 through XWayland: the hotkeys and pasting then only work while an
  X11 app has focus. It says so in the log and in a notification.
- Ctrl+V / Ctrl+C are sent as the keys at the V and C positions of a QWERTY keyboard, which the compositor translates
  with your layout. That's the right letter on QWERTY, AZERTY, QWERTZ and Colemak, but not on Dvorak.
- Keyboards plugged in while wisprcheap runs are picked up automatically.
- `WISPRCHEAP_KEYBOARD=x11` forces the X11 method, and `WISPRCHEAP_KEYBOARD=evdev` forces the input devices
  (on X11 too).

## License

[MIT](LICENSE)
