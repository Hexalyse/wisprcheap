# WisprCheap sync server: plan

Status: **plan only, nothing implemented yet** (written 2026-09-29).

The goal is an optional, self-hostable server that syncs the configuration (dictionary, prompts,
models, translation pairs, **API keys**) and the history/cost statistics between a user's devices:
the desktop app (this repo, Rust) and the Android app
([wisprcheap-android](https://github.com/Hexalyse/wisprcheap-android), Kotlin).

Everything except the statistics is **end-to-end encrypted** with a sync passphrase, so the server
(and its admin) never sees API keys, dictionary terms, prompts or dictated text.

---

## 0. Decisions already taken

| Topic | Decision |
|---|---|
| Hosting | Self-hosted first: one server, **several user accounts** (a personal server simply has one account) |
| Web authentication | Username + password (Argon2id), for the **web UI only**; an admin account manages users (invitations, no open sign-up) |
| Device authentication | **One token per device**, obtained by pairing (QR code / one-time code); devices never know the password; tokens are revocable per device |
| Desktop token storage | In `config.yaml` (`sync.token`), like the API keys today. No credential store |
| Optional | Sync stays **optional** in both apps: without a `sync` configuration they behave exactly as today |
| Encryption | Client-side, with a **sync passphrase** distinct from the password. Only statistics stay readable by the server (costs, durations, models, token and word counts, timestamps, device) |
| API keys | **Synced**, encrypted like the rest |
| Code location | This repository: a Cargo workspace with a shared crate (`sync/`) and the server (`server/`); the desktop app stays at the root |

## 1. Goals and non-goals

**Goals**
- Edit the dictionary, prompts, models or an API key on one device → the other devices get it within
  minutes (or immediately with "Sync now").
- One place to see the costs and usage of all devices (web dashboard, and "all devices" totals in the apps).
- Safe to expose on the internet: strong authentication, per-device revocation, and a stolen database
  leaks no secrets and no dictated text.
- Offline-first: dictation never waits for the server; a server outage only delays the sync.
- Small and easy to run: one binary or Docker image, SQLite, no external services.

**Non-goals (v1)**
- Relaying the transcription/LLM calls through the server ("gateway mode"): possible later.
- Syncing device-specific settings (hotkeys, bubble, microphone, insertion method…).
- Syncing audio (failed recordings stay on their device).
- Showing decrypted text in the web dashboard (it only has the statistics; see 15).
- A hosted multi-tenant service, email, OAuth/SSO.

## 2. What syncs

### 2.1 Shared profile (encrypted)
Canonical names follow the desktop `config.yaml` (camelCase). One **record per setting**, so edits of
different settings on different devices never conflict.

| Record | Value | Desktop (`config.yaml`) | Android (`Settings` / `ApiKeys`) |
|---|---|---|---|
| `setting:transcription.provider` | `"elevenlabs"` \| `"openai"` | same path | `transcription.provider` |
| `setting:transcription.language` | string | same | same |
| `setting:transcription.timeoutMs` | int | same | same |
| `setting:transcription.elevenlabs.{model,baseUrl,keyterms,noVerbatim}` | string / bool | same | same |
| `setting:transcription.openai.{model,baseUrl,prompt}` | string | same | same |
| `setting:polish.{enabled,baseUrl,model,timeoutMs,minWords,instructions}` | bool / string / int | same | same |
| `setting:polish.reasoningEffort` | string \| `null` (omit) | same (`null` = omit) | same |
| `setting:polish.temperature` | number \| `null` | same | same |
| `setting:command.{baseUrl,model,timeoutMs}` | string \| `null` (= same as cleanup) | key absent = inherit | `command.llm.*` (null = inherit) |
| `setting:command.{reasoningEffort,temperature}` | tri-state (2.3) | key absent / `null` / value | `inheritX` flag + value |
| `setting:translation.{baseUrl,model,timeoutMs,reasoningEffort,temperature}` | as command | same | `translation.llm.*` |
| `pair:<blinded id>` | `{from: code \| null, to: code}` | `translation.pairs[]` | `translation.pairs[]` |
| `dict:<blinded id>` | `{term, soundsLike: []}` | `dictionary[]` | `dictionary[]` |
| `price:<blinded id>` | `{model, perMinute?, inputPerM?, outputPerM?}` | new `pricing.overrides` | `pricing.overrides` |
| `secret:elevenlabs` | string | `transcription.elevenlabs.apiKey` | `ApiKeys.elevenlabs` |
| `secret:openai` | string | `transcription.openai.apiKey` | `ApiKeys.openai` |
| `secret:polish` / `secret:command` / `secret:translation` | string, `""` = fallback (2.3) | `polish.apiKey`, … | `ApiKeys.polish`, … |

### 2.2 Per device (never synced)
Desktop: `hotkey.*`, `recording.*`, `sounds.*`, `output.*`, `notifications.*`, `history.*`, the `sync`
section itself, the active translation pair (`state.json`), and command mode on/off (`hotkey.commandKeys`).
Android: `bubble.*`, `recording.*`, `output.*`, `history.*`, `notifications.*`, `command.enabled`,
`translation.active`, the learned per-app insertion methods.

### 2.3 Semantics that differ between the apps
- **Tri-state LLM options** (command/translation `reasoningEffort`, `temperature`): JSON
  `{"inherit": true}` = same as cleanup, `null` = omitted from the request, otherwise the value.
  Desktop: key absent / `null` / value. Android: `inheritX = true` / `inheritX = false, value = null` / value.
- **Secondary API keys** (`secret:polish|command|translation`): `""` means "fallback":
  - polish → the OpenAI key (desktop default `${OPENAI_API_KEY}`; Android: when on the OpenAI host);
  - command / translation → the cleanup key.
  Desktop pushes `""` when the YAML value is the same text as the one it falls back to (e.g. both
  `${OPENAI_API_KEY}`). When it applies `""`, it writes that reference back.
- **Translation pairs / dictionary**: sets, merged item by item (union with deletions), never replaced
  as a whole.

### 2.4 History (statistics in clear, content encrypted)
Each history entry gets two new fields in both apps: `id` (random UUID) and `device` (the device id).
Old entries without an id get a deterministic one, UUIDv5(namespace, `device` + `ts`), so re-uploading
them never duplicates anything. Entries are immutable. Deleting one sends a tombstone.

| Readable by the server (statistics) | Encrypted |
|---|---|
| `id`, `device`, `ts`, `mode` (dictation/command), `durationSec`, transcription `provider`/`model`/`ms`/`keyterms`, LLM `model`/`ms`/`inputTokens`/`outputTokens`, `words`, `costUsd`, `status` (`ok` / `failed` / `empty` / `skipped`), `retry` | the **full entry** (including `raw`, `text`, `selection`, `error`, `app`, `translation`, `insertMethod`) |

Uploading history is on by default when sync is enabled. **Downloading** the other devices' entries
(to see all of them in the app's history list) is an option. The "all devices" totals come from the
server's statistics endpoint and don't need it.

---

## 3. Architecture

```
 Desktop app (Rust)            Android app (Kotlin)               Browser
 ┌─────────────────┐           ┌─────────────────┐               ┌──────────┐
 │ config.yaml/.env│           │ Settings/ApiKeys│               │ web UI   │
 │ history.jsonl   │           │ history.jsonl   │               │ (session)│
 │ sync engine ────┼──HTTPS──┐ │ sync engine ────┼──HTTPS──┐     └────┬─────┘
 │ (wisprcheap-sync│  device │ │ (:core, Kotlin  │  device │          │ HTTPS
 │  crate)         │  token  │ │  port + vectors)│  token  │          │
 └─────────────────┘         ▼ └─────────────────┘         ▼          ▼
                     ┌────────────────────────────────────────────────────┐
                     │ wisprcheap-server (axum, one binary)               │
                     │  /v1/* sync API (Bearer device token)              │
                     │  web UI: login, devices, pairing, admin, dashboard │
                     │  SQLite: users, sessions, devices, records, stats  │
                     └────────────────────────────────────────────────────┘
                       behind a reverse proxy run by the deployer, for TLS
```

### 3.1 Repository layout (Cargo workspace)
```
wisprcheap/
├─ Cargo.toml            [package] wisprcheap (desktop, unchanged) + [workspace] members = ["sync", "server"]
├─ src/                  desktop app; gains src/sync/ (engine, YAML/.env write-back, CLI commands)
├─ sync/                 crate wisprcheap-sync (library, no I/O)
│  ├─ src/crypto.rs      Argon2id, HKDF, AES-256-GCM envelope, key wrapping, blinded ids
│  ├─ src/hlc.rs         hybrid logical clock
│  ├─ src/protocol.rs    API request/response types (serde)
│  ├─ src/profile.rs     canonical record ids and value types (section 2)
│  ├─ src/history.rs     HistoryEntry (moved from src/history.rs) + the stats/encrypted split
│  ├─ src/pricing.rs     price table (moved from src/pricing.rs), used by the server to recompute costs
│  └─ testdata/vectors.json   crypto/HLC/id test vectors, also copied into the Android repo
└─ server/               crate wisprcheap-server (binary) + PLAN.md (this file), Dockerfile, templates/
```
- The desktop release workflow keeps building only the desktop binaries (`cargo build -p wisprcheap`).
- The server gets its own workflow and tags (`server-v*`).

---

## 4. Security model

### 4.1 Accounts and web sessions
- **Passwords**: Argon2id (m = 19 MiB, t = 2, p = 1, the OWASP minimum, adjustable). Minimum 12 characters.
  Hashes are stored in PHC string format, so the parameters can change later.
- **Sessions**:
  - a random 256-bit id in the `wcs_session` cookie (`HttpOnly`, `Secure`, `SameSite=Lax`, path `/`);
  - the server stores only its SHA-256;
  - expiry: 30 days absolute, 7 days idle.
  - Logout deletes the session. "Sign out everywhere" deletes all of them.
- **CSRF**: a per-session token in every form, plus an `Origin` / `Referer` check on every POST.
- **Login throttling**: per IP and per username, 5 failures then an exponential delay (up to 15 min).
  A failed login takes the same time whether or not the user exists.
- **Admin bootstrap** (never "first visitor becomes admin"):
  - `wisprcheap-server admin create <username>` asks for the password on the terminal (or reads it
    from stdin);
  - alternatively, when the database has no user at startup, the server logs a one-time URL
    `https://…/setup?token=…`. It works once, until an admin exists.
- **Users by invitation**:
  - the admin creates an invite: a single-use link valid 7 days (`/invite/<code>`) where the new user
    chooses a username and password;
  - "reset password" issues the same kind of link, so the admin never sees or sets a password;
  - the admin can disable a user (sessions and device tokens are refused immediately) or delete it
    (all of its data is deleted).
  - Open sign-up doesn't exist in v1.
- **Two-factor login**: not in v1. Sessions are designed so TOTP or passkeys can be added later without
  touching the devices, which never log in with the password.

### 4.2 Devices and tokens
- **Pairing**:
  1. The user clicks "Add device" in the web UI, optionally giving it a name.
  2. The page shows:
     - an 8-character code (base32 without ambiguous characters, `ABCD-2345`), valid 10 minutes and
       usable once;
     - a QR code containing `wisprcheap://pair?server=<public URL>&code=<code>`.
  3. The device calls `POST /v1/pair {code, name, platform, appVersion}` and receives its token:
     - Android: the phone's camera app opens the deep link, or the code is typed;
     - desktop: `wisprcheap sync pair <server> <code>`.
  - Pairing attempts are rate-limited (10 per minute per IP). 40 bits of code with 10 minutes of
    validity make guessing hopeless.
- **Token**: `wcs_` + 43 characters of base64url (256 random bits). The server stores its SHA-256 and
  compares in constant time. Tokens don't expire by themselves; optionally, a token unused for 180
  days expires.
- **Scope**: a device token only gives access to the `/v1/*` sync API, for its own user. It can't
  manage the account, the devices or other users.
- **Revocation**:
  - from the web UI (Devices → Revoke), or by the device itself ("Disconnect", `DELETE /v1/device`);
  - the next request gets `401`, and the app shows "Disconnected from the sync server" while keeping
    its local data.
- **Storage on devices**:
  - desktop: `sync.token` in `config.yaml` (like the API keys; `${VAR}` works);
  - Android: in the encrypted `SecretStore`, with the API keys.
- **Device list**: name, platform, app version, paired at, last seen (time and IP), with a "revoke"
  button.

### 4.3 End-to-end encryption
All primitives come from the shared crate (Rust), and the Android port must reproduce them exactly.
The test vectors in `sync/testdata/vectors.json` check both implementations.

**Keys**
- **Data key (DK)**: 32 random bytes, created by the **first device** that enables sync for an account.
  It never changes when the passphrase changes.
- **Passphrase key (PK)** = Argon2id(passphrase NFC-normalised, salt, m = 64 MiB, t = 3, p = 1,
  32 bytes).
  - The salt is 16 random bytes per account.
  - The salt and parameters are stored in clear on the server (the "keyring"), so all devices derive
    the same PK.
- **Wrapped DK** = AES-256-GCM(PK, random 12-byte nonce, AAD = `wisprcheap/keywrap/v1|<user id>`, DK).
  The server stores it with a `keyVersion` number.
- Subkeys come from DK with HKDF-SHA256: `enc = HKDF(DK, info "wisprcheap/enc/v1")` and
  `ids = HKDF(DK, info "wisprcheap/ids/v1")`.

**Records**
- Payload = AES-256-GCM(enc, random 12-byte nonce, AAD = `wisprcheap/rec/v1|<user id>|<kind>|<record id>`,
  JSON `{"v":1,"value":…}` padded to a multiple of 64 bytes).
  - Binding the user id and record id in the AAD means a server can't move a payload to another
    record or user without it failing to decrypt.
  - Random 96-bit nonces are safe here: a personal account stores far fewer than 2³² payloads.
- Envelope (string): `e1.` + base64url(nonce ‖ ciphertext ‖ tag).
- **Blinded ids**: record ids that would reveal content (dictionary terms, pair ids, price models) use
  `base64url(HMAC-SHA256(ids, "dict:" + lowercase term))[:22]`.
  - The same term therefore gets the same id on every device, which is what lets the dictionary merge
    term by term.
  - Setting and secret ids (`setting:polish.model`, `secret:openai`) are part of the public schema and
    stay in clear.

**On the devices**
- **Setting up the first device**: after pairing, if the account has no keyring, the app asks for a
  new passphrase (twice, with a warning that it can't be recovered), creates DK, uploads the keyring,
  then uploads its local profile.
- **Other devices**: they fetch the keyring and ask for the passphrase. Wrong passphrase = the wrapped
  key fails to decrypt ("wrong passphrase"). After that they keep DK locally:
  - desktop: `sync.key` in `config.yaml`, written by the pairing command;
  - Android: `SecretStore`.
  - The passphrase itself is never stored.
- **Changing the passphrase** (from any device that has DK): new salt + PK, re-wrap DK,
  `PUT /v1/keyring` with `If-Match: <keyVersion>`. The data is not re-encrypted and the other devices
  keep working.
- **Forgotten passphrase**, if no device has DK anymore: "Reset encryption" in the web UI.
  - It deletes the keyring and all encrypted records (the statistics stay).
  - The devices then see a new `keyVersion`, ask for a new passphrase and re-upload their profile.
- **Not in v1**: rotating DK itself, e.g. after a device with DK was stolen. The mitigation is to
  revoke the device, reset the encryption and change the API keys at the providers.

### 4.4 Threat model

| Threat | Mitigation |
|---|---|
| Internet attacker on an exposed server | TLS (reverse proxy), login throttling, no default credentials, invitation-only accounts, high-entropy tokens and pairing codes, security headers |
| Stolen / lost device | Revoke its token from the web UI; its local secrets are already encrypted (Android) or in the user's own config (desktop) |
| Database leak / server compromise | Password hashes (Argon2id), token and session hashes only, secrets and content encrypted with a key the server never has; only the statistics are readable |
| Malicious or curious server admin | Same as above: they see statistics, device names and timestamps, not keys or text (documented clearly) |
| Another user of the same server | Every query filtered by `user_id`; integration tests try every endpoint with another user's token and ids |
| Server swapping or replaying payloads | AAD binds user, kind and record id; LWW with HLC timestamps (a replayed older record never wins) |
| CSRF / XSS on the web UI | CSRF tokens + Origin check; server-rendered HTML with auto-escaping, strict CSP (`default-src 'self'`), no inline script |

### 4.5 Server hardening
- Security headers: `Content-Security-Policy`, `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`, and HSTS when the public URL is https.
- Plain HTTP is refused, unless the public URL is `http://localhost` or `WCS_ALLOW_HTTP=1` for a LAN.
- **Audit log**, visible to the user (own events) and to the admin (all):
  - logins (successful and failed);
  - pairing and revocation;
  - password and passphrase changes;
  - users created, disabled or deleted;
  - encryption reset.
- Request size limits: 1 MiB per sync batch, 64 KiB per record payload.
- Per-token API rate limit: 120 requests per minute.

---

## 5. Data model (SQLite)

```sql
users        (id TEXT PK, username TEXT UNIQUE, password_hash TEXT, is_admin INT, disabled INT, created_at)
sessions     (id_hash BLOB PK, user_id FK, csrf TEXT, created_at, last_seen_at, ip, user_agent)
invites      (code_hash BLOB PK, created_by FK, purpose TEXT /* signup|reset */, user_id FK NULL, expires_at, used_at)
devices      (id TEXT PK /* dev_… */, user_id FK, name, platform, app_version, token_hash BLOB UNIQUE,
              created_at, last_seen_at, last_ip, revoked_at)
pairing_codes(code_hash BLOB PK, user_id FK, device_name, expires_at, used_at)
keyrings     (user_id PK FK, key_version INT, salt BLOB, kdf_params TEXT, wrapped_key TEXT, updated_at)
records      (user_id FK, kind TEXT, id TEXT, hlc TEXT, deleted INT, payload TEXT NULL,
              device_id FK, seq INTEGER UNIQUE, updated_at, PRIMARY KEY (user_id, kind, id))
history_stats(user_id FK, entry_id TEXT, device_id FK, ts TEXT, mode, duration_sec REAL,
              stt_provider, stt_model, stt_ms, keyterms, llm_model, llm_ms, input_tokens, output_tokens,
              words, status, retry, client_cost_stt REAL, client_cost_llm REAL, client_cost_total REAL,
              deleted INT, PRIMARY KEY (user_id, entry_id))
audit_log    (id INTEGER PK, user_id FK NULL, device_id NULL, event TEXT, detail TEXT, ip, at)
meta         (key TEXT PK, value TEXT)   -- schema version, setup token hash
```
- `seq` is a global counter, increasing on every insert or update. A user's change feed is
  `WHERE user_id = ? AND seq > ?`.
- History entries are both a `records` row (`kind = 'history'`, encrypted payload, in the change feed)
  and a `history_stats` row (clear statistics for the dashboard).
- Migrations are embedded in the binary and applied at startup.

---

## 6. Sync protocol

### 6.1 Hybrid logical clock (HLC)
- Every change gets an HLC timestamp `(ms, counter, device id)`, serialised so that string order =
  time order: `<ms, 13 digits>-<counter, 4 hex>-<device id>`, e.g. `1759154400123-0000-dev_k3j9`.
- A device sets `ms = max(wall clock, last HLC seen)`. It bumps `counter` when `ms` doesn't move, and
  merges every HLC it receives. So a device with a slow clock can still overwrite older changes.
- The server refuses HLCs more than 10 minutes in its own future (`400 clock_skew`), so a badly set
  clock can't "win forever".

### 6.2 Conflict resolution
- **Settings, secrets, pairs, dictionary terms, prices**: last writer wins, per record, by HLC.
  - The server applies a pushed record only if its HLC is greater than the stored one. Otherwise it
    answers `stale` with the current record, which the device then applies.
  - Deletions are records with `deleted = true` (tombstones), kept forever (they are tiny).
- **Dictionary and translation pairs** are sets of records: a term added on one device and another
  added elsewhere both survive, and a deletion wins over older edits of the same term.
- **History**: an entry is inserted if it doesn't exist and is never modified. A tombstone deletes it.
- **First sync of a device that already has a local configuration** (pairing an existing install):
  - pull first;
  - server values win for records that exist on both sides; local-only items (dictionary terms,
    pairs, prices) are uploaded;
  - if the server has no profile yet, the local profile becomes it (first device).
  - The app shows a summary ("12 settings updated from the server, 3 dictionary terms uploaded").
  - The desktop keeps a copy of the previous file (`config.yaml.bak-<date>`).

### 6.3 Exchanges
1. **Pull**: `GET /v1/changes?since=<seq>&limit=500[&exclude=history]` → records newer than the
   device's cursor, oldest first, plus `nextSince` and `hasMore`. Pull pages until done. Decrypt,
   apply, save the cursor.
2. **Push**: `POST /v1/changes` with up to 500 local changes (outbox) → per record, `applied` (with
   its new `seq`) or `stale` (with the winning record). Remove the applied ones from the outbox.
3. **Cursor and outbox persistence**:
   - desktop: `<cache dir>/sync/state.json` (cursor, snapshot, outbox and not-yet-applied changes),
     with a lock file so the app and the CLI never sync at the same time;
   - Android: files in the app's private storage.
4. **When a device syncs**:
   - at startup;
   - a few seconds after a local change (debounced);
   - after each new history entry (batched);
   - every 15 minutes;
   - on "Sync now".
   - Errors are retried with exponential backoff (max 30 min) and never block dictation.
   - `401` → "disconnected" state. A keyring version mismatch → ask for the passphrase again.

### 6.4 Wire format of a record
```json
{ "kind": "setting", "id": "polish.model", "hlc": "1759154400123-0000-dev_k3j9", "deleted": false,
  "payload": "e1.AbC…", "stats": null }
```
- For `kind = "history"`, `id` is the entry UUID and `stats` holds the clear fields of section 2.4.
- The server validates `stats` (types, ranges) and recomputes the costs with its own price table
  (shared `pricing.rs`). It keeps both values: the client's estimate and its own.

---

## 7. API

All `/v1` endpoints take and return JSON and require `Authorization: Bearer wcs_…`, except
`POST /v1/pair`. Errors look like `{"error": "code", "message": "…"}`.

| Method & path | Purpose |
|---|---|
| `POST /v1/pair` | `{code, name, platform, appVersion}` → `{token, device, user}` (no auth; single-use code) |
| `GET /v1/me` | `{user: {id, username}, device, serverTime, keyring: {keyVersion, salt, kdf, wrappedKey} \| null}` |
| `PUT /v1/keyring` | Create the keyring (only if none), or replace it with `If-Match: <keyVersion>` (passphrase change) |
| `GET /v1/changes` | Change feed (6.3), `since`, `limit`, `exclude` |
| `POST /v1/changes` | Push a batch (6.3) |
| `GET /v1/stats` | `?from=YYYY-MM&to=YYYY-MM&tz=Europe/Paris&group=month,device,provider,model` → totals (entries, words, audio minutes, STT / LLM / total cost, per 10k words) |
| `PATCH /v1/device` | Rename this device |
| `DELETE /v1/device` | Unpair this device (revokes its own token) |
| `GET /healthz` | Liveness, no auth |

Web routes (session cookie):
- `/login`, `/logout`, `/setup`, `/invite/<code>`
- `/` (dashboard), `/history`, `/devices` (+ `/devices/new` with the QR code, revoke)
- `/account` (password, sign out everywhere, passphrase reset, delete account, own audit log)
- `/admin/users` (invite, reset link, disable, delete, audit log)

---

## 8. Web UI

- Server-rendered HTML: Askama templates, one small CSS file, no JavaScript except an optional
  "copy" button. Light and dark themes follow the system.
- **Dashboard**:
  - this month and the last 12 months: cost, words, dictations;
  - breakdowns per device, provider and model;
  - bar charts as server-generated inline SVG;
  - cost per 10k words;
  - entries whose model has no known price.
- **History**: the entries' statistics only (time, device, mode, duration, words, models, cost,
  status). The text is encrypted and not shown (see 15).
- **Devices**: list, add (QR + code, with a countdown), rename, revoke.
- **Account**: change password, sign out everywhere, reset encryption (with a strong warning), delete
  the account and all its data, own audit log.
- **Admin**: users (invite link, reset link, disable, delete), global audit log, server info
  (version, database size, last backup).

---

## 9. Desktop client (this repo, `src/sync/`)

**Configuration** (new section, never synced; sync is off when `server` is empty):
```yaml
sync:
  server: https://sync.example.com
  token: ${WISPRCHEAP_SYNC_TOKEN}   # or the literal wcs_… token; written by `wisprcheap sync pair`
  key: ${WISPRCHEAP_SYNC_KEY}       # this device's copy of the data key (never sent); written by pairing
  deviceName: Work laptop
  history: upload                   # upload | download (upload + get the other devices' entries) | off
```
- **CLI**:
  - `wisprcheap sync pair <server> <code>`: asks for the passphrase on the terminal (or reads it with
    `--passphrase-stdin`), pairs, sets up or unlocks the encryption, then writes `sync.*` into
    `config.yaml` (in place, keeping comments);
  - `wisprcheap sync status`, `wisprcheap sync now` (runs in the terminal, then pokes the running
    instance through its control socket), `wisprcheap sync unpair`, `wisprcheap sync passphrase`
    (change it), `wisprcheap sync unlock` (enter it again after a reset elsewhere),
    `wisprcheap sync rename <name>`.
- **Engine**:
  - a Tokio task in the app actor;
  - triggers from 6.3 (startup, config reload, history append, 15-minute timer, `sync-now` control
    command);
  - logs sync lines (`[sync] pushed 3, pulled 12 (0.4 s)`) and errors (`[sync] offline, retrying in 2 min`).
- **Local change detection**:
  - after each successful config load, compute the canonical profile (section 2) from the effective
    config;
  - compare it with the last synced snapshot (hashes in `state.json`);
  - differences are local edits, which get an HLC and go to the outbox.
- **Applying remote changes (write-back)**. The file stays the single source of truth, so the user
  always sees what's in effect:
  - **settings**: a generic comment-preserving YAML editor (`src/yaml_edit.rs`), generalised from the
    dictionary code. It can set a scalar at a path, remove a key, insert a key in the right section,
    replace a sequence, and add, update or remove a dictionary item. Every edit is re-parsed and must
    produce exactly the expected document; otherwise the file isn't written, the change stays
    pending, and an error is logged and notified;
  - **secrets**: if the YAML value is a `${VAR}` reference, the key is written to `<config dir>/.env`
    (line updated or appended, other lines untouched). If it's a literal, the YAML scalar is edited.
    If the key is absent, it goes to `.env` under the default name (`ELEVENLABS_API_KEY`,
    `OPENAI_API_KEY`), or into a new YAML key for polish/command/translation;
  - the write triggers the usual hot reload. The reloaded profile then equals the snapshot, so nothing
    is pushed back (echo suppression).
- **Tray**: a status line under the month line (`Synced 2 min ago` / `Sync: offline` /
  `Sync: disconnected`) and a "Sync now" item.
- **History**:
  - new entries get `id` and `device`;
  - with `history: download`, other devices' entries are appended to `history.jsonl` (with their
    `device`), so `wisprcheap stats` and the tray month line include them;
  - with `upload`, the tray month line uses the server's all-device totals when online.
- **Shared crate**: `HistoryEntry` and the pricing code move from `src/history.rs` and
  `src/pricing.rs` to `wisprcheap-sync`, re-exported so the rest of the desktop code is unchanged.

## 10. Android client ([wisprcheap-android](https://github.com/Hexalyse/wisprcheap-android))

- **`:core`** (pure Kotlin, JVM-tested):
  - `sync/` with the crypto port (AES-GCM and HMAC from the JCA, HKDF hand-written, Argon2id from
    Bouncy Castle's pure-Java `Argon2BytesGenerator`), the HLC, the protocol types, the profile
    mapping and the engine;
  - tests with the shared vectors, and MockWebServer tests of the engine.
- **Settings → Sync** (new page):
  - off / connected / error, last sync, "Sync now";
  - "Connect to a server": opened by the QR code's deep link (`wisprcheap://pair?server=…&code=…`,
    handled by `MainActivity`), or server URL + code typed by hand;
  - then the passphrase (created twice on the first device, entered once on the others);
  - options: upload history (default on), download other devices' history (default off);
  - device name; "Change passphrase"; "Disconnect this device" (revokes the token, keeps the local data).
- **Storage**: token and data key in `SecretStore` (Keystore-encrypted); cursor, snapshot hashes and
  outbox in private files.
- **Scheduling**:
  - WorkManager periodic work (15 min, network required);
  - an immediate one-off sync a few seconds after a settings change or a new history entry;
  - a sync at app start.
- **Mapping**: `Settings` / `ApiKeys` ↔ records (section 2), with `LlmOverride` ↔ tri-state, and
  `ApiKeys.polish` `""` ↔ fallback.
- **History**: `id` and `device` added to `HistoryEntry` (+ JSONL). Home's "This month" gets an
  "All devices" toggle using `/v1/stats` (cached).

---

## 11. Deployment

- **Image**: `ghcr.io/hexalyse/wisprcheap-server:<version>`. It's a multi-stage build producing a
  static musl binary in a distroless image (about 15 MB), running as a non-root user.
- **Data**: volume `/data` (`wisprcheap.db` + `backups/`).
- **Configuration** (environment variables):
  - `WCS_PUBLIC_URL` (required, used in QR codes and cookies);
  - `WCS_BIND` (default `0.0.0.0:8080`);
  - `WCS_DATA_DIR` (default `/data`);
  - `WCS_TRUST_PROXY` (use `X-Forwarded-For` for rate limits and logs; only behind a proxy);
  - `WCS_ALLOW_HTTP` (LAN without TLS);
  - `WCS_LOG` (level).
- **HTTPS is up to the deployer**: nginx-proxy + acme-companion, a Caddy or Traefik container, or an
  existing reverse proxy. The server listens on plain HTTP inside the container and trusts
  `X-Forwarded-*` headers only when `WCS_TRUST_PROXY=1`.
- **docker-compose example** (the proxy is left out, as it depends on the deployment):
  ```yaml
  services:
    wisprcheap:
      image: ghcr.io/hexalyse/wisprcheap-server:0.1.0
      environment:
        WCS_PUBLIC_URL: "https://sync.example.com"
        WCS_TRUST_PROXY: "1"
      volumes: ["./data:/data"]
      expose: ["8080"]
  ```
- **Bare metal**: the same binary (Linux x86_64 / arm64 release assets). `wisprcheap-server serve`
  runs it; a sample systemd unit is in `server/`.
- **Backups**: `wisprcheap-server backup` (SQLite online backup to `/data/backups/`, keeps the last
  7), plus an optional daily automatic backup. Restoring = stop, replace the file, start. A backup
  contains no secrets in clear (4.3).
- **Upgrades**: migrations run at startup, and the `/v1` API stays backward compatible within a major
  version. Clients send their version, so the server can warn about outdated apps.

## 12. Testing

- **Shared vectors** (`sync/testdata/vectors.json`):
  - Argon2id (passphrase, salt, parameters → PK);
  - key wrap and unwrap;
  - record encryption with fixed nonces;
  - blinded ids;
  - HLC ordering and merge.
  - Run by the Rust tests and copied into the Android `:core` tests, so interoperability is checked on
    both sides.
- **Server integration tests** (axum test client on a temporary database):
  - admin bootstrap, invites, login and throttling, sessions and CSRF;
  - pairing (expired or reused codes), token revocation, disabled users;
  - LWW (stale answers, clock skew), change feed paging, history idempotency, statistics;
  - account deletion.
  - An **isolation suite** calls every endpoint with user B's token on user A's ids and expects
    `404` / `403`.
- **Desktop**:
  - YAML write-back tests on many config variants (comments, flow and block lists, CRLF, missing
    sections, `${VAR}` values);
  - `.env` writer tests; snapshot diff and echo suppression;
  - a full round trip against a test server.
- **Android**: `:core` engine tests with MockWebServer, the mapping tests, and a manual test with a
  real server.
- **End to end** (manual, before each release): desktop + phone on the same account. Add a term on one
  and change a key on the other, check both converge; revoke a device; change the passphrase.

## 13. Milestones (≈ 3 weeks)

| # | Milestone | Content | Done when |
|---|---|---|---|
| S0 | Workspace + shared crate | Workspace, `wisprcheap-sync` (crypto, HLC, protocol, profile, history, pricing), vectors; desktop builds unchanged | `cargo test --workspace` green; desktop release workflow unchanged |
| S1 | Server core | DB and migrations, admin CLI and setup token, users, sessions, invites, pairing, tokens, `/v1` API, statistics, audit log | Integration and isolation suites green |
| S2 | Web UI | Login, setup, devices (QR), account, admin, dashboard, history | A new user can be invited, log in and pair a device from the browser |
| S3 | Desktop client | Config section, CLI, engine, YAML/.env write-back, history ids, tray status | Two desktop instances on one account converge |
| S4 | Android client | `:core` sync + crypto port, Sync page, deep-link pairing, WorkManager, history ids, all-device totals | Phone + desktop converge (dictionary, prompts, keys, stats) |
| S5 | Packaging | Dockerfile, compose example, systemd unit, server CI (tests, image to GHCR and binaries on `server-v*` tags), docs | `docker compose up` behind a reverse proxy works from the README |
| S6 | Hardening | Security checklist review, backup/restore drill, 50k history entries performance check | Checklist signed off; v0.1.0 of the server released |

## 14. Risks

| Risk | Mitigation |
|---|---|
| YAML write-back damages a hand-edited config | Re-parse and verify every edit, refuse otherwise; `.bak` copy at first sync; many fixture tests |
| Crypto differences between Rust and Kotlin | Shared test vectors run on both sides; versioned envelope (`e1.`) and KDF parameters |
| Lost passphrase | Clear warnings at setup, change it from any device that has the key, reset flow that keeps the statistics |
| Clock skew between devices | HLC plus server-side skew limit |
| Secrets in plain text on desktop (`config.yaml`/`.env`) | Unchanged from today and documented; `${VAR}` supported to keep them in `.env` |
| Settings meaning slightly different on each platform | Canonical schema (section 2) with explicit mapping tests on both sides |
| Exposed server attacked | Section 4; the README shows the reverse-proxy setup, and the server refuses plain HTTP by default |

## 15. Later
- **Web dashboard with decrypted text**: the passphrase is typed in the browser and decrypted
  client-side (Argon2 in WebAssembly + WebCrypto), which needs JavaScript and a careful CSP.
- **Gateway mode**: the server holds the provider keys and relays the transcription/LLM calls, giving
  exact billing, spending caps and family accounts.
- **Syncing platform-specific settings** between devices of the same kind (two desktops, two phones).
- **Profiles** (work / personal), TOTP or passkeys for the web UI, data-key rotation, server-sent
  events for instant sync.