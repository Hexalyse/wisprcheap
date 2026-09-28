# wisprcheap (Rust)

A minimal, pay-per-use take on Wispr Flow / Typeless for **Windows and Linux**:
hold **Ctrl + Win**, speak, release. The audio is transcribed (ElevenLabs Scribe v2 or OpenAI),
cleaned up by a cheap LLM ("polish" pass), copied to the clipboard and pasted into the focused text field.

This is a native Rust rewrite of the (now archived) [TypeScript/Node version](https://github.com/Hexalyse/wisprcheap-ts),
with the same features, config file, prompts, history format and tray menu, in a single executable.

- Push-to-talk, or **double-tap** the hotkey for hands-free mode (tap again to stop)
- **Command mode** (Ctrl + Win + Alt): select text and say "make this more formal", "translate to English"...,
  or say "write a short reply saying I'll be late" with nothing selected
- Dictionary of names and technical terms: sent to Scribe as `keyterms` (or to OpenAI as a `prompt`) and to the polish model.
  Add a word by selecting it and pressing **Ctrl + Win + Shift**
- Automatic language detection, and the polish pass never translates, unless you turn on **translation mode**
  (French → English, etc.: pairs configured in `config.yaml`, chosen from the tray)
- Short dictations can skip the polish step and paste ~2 s sooner (`polish.minWords`)
- Short sound cues for start, stop, hands-free, command, cancel and error, plus a desktop notification when something fails
- `history.jsonl` log with raw and polished text, timings and estimated cost, plus `wisprcheap stats`
- Runs in the background with a tray icon: status color, this month's estimated cost, log window, pause, retry a failed dictation...
- Follows the default microphone and speakers (plug in a headset, it's used from the next dictation)
- No settings UI: one YAML file, applied as soon as you save it

## Build

Requires Rust 1.85+ (edition 2024).

**Windows**: nothing else (MSVC toolchain).

**Linux** (X11 session, see [Linux notes](#linux-notes)): the GTK 3, AppIndicator, ALSA, X11/XTest and xdo development packages. On Debian/Ubuntu:

```sh
sudo apt install build-essential pkg-config libgtk-3-dev libayatana-appindicator3-dev \
  libasound2-dev libx11-dev libxtst-dev libxi-dev libxdo-dev libdbus-1-dev
```

Then:

```sh
cargo build --release
```

This produces two executables in `target/release/`:

- `wisprcheap`: the command line (and the app itself).
- `wisprcheapw`: the same program without a console window on Windows. It's what the background instance and the
  desktop shortcut use. It makes no difference on Linux.

## Setup

1. Put your API keys in a `.env` file (or in `config.yaml`, or in the environment):
   ```
   ELEVENLABS_API_KEY=...
   OPENAI_API_KEY=...
   ```
2. Optionally copy `config.example.yaml` to `config.yaml` and edit it. Every setting has a default, and
   **Open config.yaml** in the tray creates it from the example if it doesn't exist.
3. Run `wisprcheap shortcut` (optional: adds a desktop shortcut, and a menu entry on Linux), then `wisprcheap start`.

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
| `wisprcheap shortcut` | Create or update the desktop shortcut                                |
| `wisprcheap devices`  | List microphones (for `recording.device`) and speakers              |
| `wisprcheap stats`    | Words, audio minutes and estimated cost per month                    |
| `wisprcheap sounds`   | Play every sound cue                                                 |

Only one instance runs at a time. Starting it again from the shortcut shows the log window. Output is written to
`wisprcheap.log` (rotated at 1 MB).

## Tray icon

The icon color shows the state: **grey** ready, **red** recording, **amber** transcribing/polishing, **light grey with a slash** paused.

The top of the menu shows the status and this month's estimated cost and word count (from `history.jsonl`, updated after each dictation).

- **Left-click** (Windows) or **Show log** toggles the log window. Closing it, or pressing Esc, only hides it; the app keeps running.
- **Copy last dictation** puts the last polished text back on the clipboard.
- **Retry last failed** re-sends the last recording whose transcription failed (e.g. network or quota error).
  The result goes to the clipboard, since focus is on the tray at that moment.
- **Translate dictation** (only shown when `translation.pairs` is set): pick a pair, or Off. The choice is remembered.
- **Add clipboard to dictionary** adds the copied word or phrase to `config.yaml`.
- **Pause dictation** ignores the shortcuts until you resume.
- **Open config.yaml** opens it in your default editor. Saved changes apply immediately.
- **Restart** fully restarts the app (not needed for config changes).
- **Quit** waits for a dictation in progress to finish, then exits.

When something fails (transcription, command, translation, microphone, config reload...), a notification
says what happened; click it to open the log. Turn it off with `notifications.errors: false`.

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
`wisprcheap stats` shows your real numbers based on the history file, and the tray menu shows the current month's total.

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
- `cargo test` runs the unit tests (config, dictionary editing, prompts, pricing, audio, the hotkey state machine).
  `WISPRCHEAP_NO_INJECT=1` never sends Ctrl+C / Ctrl+V, and `WISPRCHEAP_INSTANCE=<name>` runs a separate instance,
  which is useful for manual end-to-end tests next to a real instance.

### Linux notes

- Global hotkeys and key injection use X11 (XRecord / XTest), like the original libuiohook. They work in X11 sessions,
  and on Wayland only while an XWayland window has focus. Native Wayland apps don't expose global keys.
- The tray uses AppIndicator / StatusNotifierItem. GNOME needs the "AppIndicator and KStatusNotifierItem Support" extension.
  AppIndicators don't report left clicks: use **Show log** in the menu. Notifications go through D-Bus
  (`org.freedesktop.Notifications`).
- Without a display, the app runs without the tray icon and logs why.
- The clipboard works on X11 and Wayland (wlr data-control).

## License

[MIT](LICENSE)
