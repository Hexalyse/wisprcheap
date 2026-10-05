# wisprcheap sync: wire specification (v1)

This is the normative description of what goes over the wire and how it's encrypted. Its readers are
the server (`server/`), the desktop client (`src/sync/`, which uses the Rust code of this crate
directly) and the Android client (a Kotlin port). Every implementation must pass
`testdata/vectors.json`. For the design and rationale, see `server/PLAN.md`.

## 1. Conventions
- JSON everywhere, UTF-8, camelCase field names.
- **base64url without padding** for all binary values ("b64" below).
- Timestamps: ISO 8601 UTC with milliseconds (`2026-09-29T12:34:56.789Z`).
- Ids:
  - users `usr_<22 b64 chars>`;
  - devices `dev_<22 b64 chars>`;
  - history entries: lowercase UUID strings.

## 2. Authentication
- Device requests: `Authorization: Bearer wcs_<43 b64 chars>`.
- `401 {"error":"unauthorized"}` means the token was revoked (or the user disabled). The client stops
  syncing and shows "disconnected".
- Pairing: `POST /v1/pair` with a one-time code shown in the web UI.
  - Codes are 8 characters from `ABCDEFGHJKMNPQRSTUVWXYZ23456789`.
  - Clients uppercase the code and remove spaces and `-` before sending.
  - The QR code / deep link is `wisprcheap://pair?server=<url-encoded public URL>&code=<code>`.

## 3. Encryption
| Item | Definition |
|---|---|
| Passphrase key PK | `Argon2id(v=0x13, password = UTF-8(NFC(passphrase)), salt, m, t, p, length 32)`; default `m = 65536` KiB, `t = 3`, `p = 1` |
| Data key DK | 32 random bytes, created once per account by the first device |
| Key id | `b64(HMAC-SHA256(key = DK, msg = "wisprcheap/keyid/v1")[0..12])` (16 chars) |
| Subkeys | `enc = HKDF-SHA256(ikm = DK, salt = none, info = "wisprcheap/enc/v1", L = 32)`, `ids = HKDF-SHA256(DK, info = "wisprcheap/ids/v1")` |
| Envelope | `"e1." + b64(nonce[12] ‖ AES-256-GCM ciphertext ‖ tag[16])`, random 12-byte nonce |
| Wrapped key | `envelope(key = PK, aad = "wisprcheap/keywrap/v1|" + userId, plaintext = DK)` |
| Record payload | `envelope(key = enc, aad = "wisprcheap/rec/v1|" + userId + "|" + kind + "|" + id, plaintext = P)` |
| P | UTF-8 JSON `{"v":1,"value":<value>}` followed by ASCII spaces up to the next multiple of 64 bytes (a JSON parser ignores them) |
| Blinded id | `b64(HMAC-SHA256(key = ids, msg = kind + ":" + key))`, first 22 characters |
| Stored data key | `"wck_" + b64(DK)` (desktop `sync.key`) |

- Keys for blinded ids:
  - `dict`: the term, trimmed and lowercased;
  - `pair`: `"<from or auto>><to>"` (e.g. `fr>en`, `auto>en`);
  - `price`: the model name.
- A wrong passphrase makes the wrapped key fail to decrypt.
- A different `keyId` in the keyring than the device's own DK means the encryption was reset. The
  device must forget DK, ask for the passphrase and re-upload its profile.

## 4. Hybrid logical clock
- Format `<ms, 13 decimal digits, zero-padded>-<counter, 4 lowercase hex digits>-<device id>`. String
  order equals time order. The device id is base64url, so it can contain `-` and `_`: split on the
  first two `-` only.
- `now(wall)`: if `wall > last.ms` → `(wall, 0)`; else `(last.ms, last.counter + 1)` (if the counter
  overflows: `(last.ms + 1, 0)`).
- `observe(h)`: if `h > (last.ms, last.counter)` → `last = (h.ms, h.counter)`.
- Every received HLC is observed, and the last issued HLC is persisted.
- The server rejects HLCs more than 10 minutes ahead of its clock (push status `rejected`,
  `error = "clock_skew"`).

## 5. Records
A record is `(kind, id)` with a value, an HLC and a `deleted` flag. For every kind except
`history`, **the greater HLC wins**. `history` records are immutable, apart from deletion.

### 5.1 Kinds
| Kind | Id | Value |
|---|---|---|
| `setting` | canonical path (5.2), in clear | depends on the setting |
| `secret` | `elevenlabs`, `openai`, `polish`, `command`, `translation` | string; `""` = fallback (5.3) |
| `dict` | blinded (`dict`, lowercase term) | `{"term": "Kubernetes", "soundsLike": ["cube"]}` |
| `pair` | blinded (`pair`, `fr>en`) | `{"from": "fr" \| null, "to": "en"}` |
| `price` | blinded (`price`, model) | `{"model": "…", "perMinute"?: n, "inputPerM"?: n, "outputPerM"?: n}` |
| `history` | entry UUID, in clear | the full history entry (desktop `history.jsonl` schema + `id`, `device`); plus clear `stats` (6) |

Deleted records have `"deleted": true` and `"payload": null`.

### 5.2 Settings
Value types:
- `str`, `int`, `bool`;
- `str?` / `num?`: `null` means "omitted from requests";
- `inherit`: `{"inherit": true}` means "same as the cleanup (polish) setting".

| Id | Type |
|---|---|
| `transcription.provider` | `str` (`elevenlabs` \| `openai`) |
| `transcription.language` | `str` (`auto` or a code) |
| `transcription.timeoutMs` | `int` |
| `transcription.elevenlabs.model` / `.baseUrl` | `str` |
| `transcription.elevenlabs.keyterms` / `.noVerbatim` | `bool` |
| `transcription.openai.model` / `.baseUrl` / `.prompt` | `str` |
| `polish.enabled` | `bool` |
| `polish.baseUrl` / `.model` / `.instructions` | `str` |
| `polish.reasoningEffort` | `str?` |
| `polish.temperature` | `num?` |
| `polish.timeoutMs` / `.minWords` | `int` |
| `command.baseUrl` / `.model` | `str` \| inherit |
| `command.reasoningEffort` | `str?` \| inherit |
| `command.temperature` | `num?` \| inherit |
| `command.timeoutMs` | `int` |
| `translation.*` | same as `command.*` |

Anything else (hotkeys, bubble, recording, output, history options, active translation pair, command
mode on/off) is per device and never synced.

### 5.3 Secrets fallback
- `secret:polish = ""` → use the OpenAI key:
  - desktop: the same reference as `transcription.openai.apiKey`;
  - Android: an empty cleanup key, which falls back to the OpenAI key on the OpenAI host.
- `secret:command = ""` / `secret:translation = ""` → use the cleanup key.

### 5.4 Merging on the first sync of a device
1. Pull everything.
2. For records present on both sides, the server value wins (applied locally). Two exceptions keep
   local data: a server tombstone for an item the device has, and an empty server API key where the
   device has one. Those local values are pushed instead.
3. Local items missing on the server are pushed (dictionary terms, pairs and prices by blinded id;
   settings and secrets the server doesn't have yet, e.g. the whole profile on the first device).

## 6. History statistics (clear, `stats` of `history` records)
```json
{"ts":"2026-09-29T12:34:56.789Z","mode":"dictation","durationSec":3.2,"sttProvider":"elevenlabs",
 "sttModel":"scribe_v2","sttMs":812,"keyterms":2,"llmModel":"gpt-6-luna","llmMs":640,"inputTokens":350,
 "outputTokens":20,"words":2,"status":"ok","retry":false,"costStt":0.0002,"costLlm":0.00004,"costTotal":0.00024}
```
- `mode`: `dictation` \| `command`.
- `status`: `failed` (the entry has an `error`), `empty` (0 words), otherwise `ok`.
- `llm*` are `null` when there was no LLM step.
- The server recomputes the costs with its price table and keeps the client's values too.
- Resolve STT and LLM costs independently: server price, else client estimate. Sum resolved
  components when complete; a client total may supply an estimate when a component is unavailable.
  Otherwise report a partial total and increment `unknownPrice` when an applicable component is
  missing (excluding failed entries).
- Entries written before sync have no `id`: use `UUIDv5(namespace 6ba7b811-9dad-11d1-80b4-00c04fd430c8 (URL), "wisprcheap:" + deviceId + ":" + ts)`.

## 7. API
All bodies are JSON. Errors look like `{"error": "<code>", "message": "<text>"}`.

| Request | Response |
|---|---|
| `POST /v1/pair {code, name, platform, appVersion}` (no auth) | `200 {token, device: {id, name, platform}, user: {id, username}}`; `404 invalid_code`; `429 rate_limited` |
| `GET /v1/me` | `{user, device, serverTime, serverVersion, keyring: {keyVersion, keyId, salt, kdf: {alg, m, t, p}, wrappedKey} \| null, capabilities: {syncReport: true}}`; missing capabilities means false |
| `PUT /v1/keyring {keyId, salt, kdf, wrappedKey}` | Without `If-Match`: create, `409 keyring_exists` if one exists. With `If-Match: <keyVersion>`: replace, `412 version_mismatch` if stale. `200 {keyVersion}` |
| `GET /v1/changes?since=<seq>&limit=<≤1000, default 500>&exclude=history` | `{changes: [{seq, kind, id, hlc, deleted, payload, device, stats?}], nextSince, hasMore}`, oldest first |
| `POST /v1/changes {changes: [{kind, id, hlc, deleted, payload, stats?}]}` (≤ 500) | `{results: [{kind, id, status, seq?, current?, error?}]}` in the same order |
| `GET /v1/stats?from=YYYY-MM&to=YYYY-MM&offset=<minutes east of UTC>` | `{months: [{month, entries, commands, failed, words, audioMinutes, sttUsd, llmUsd, totalUsd, unknownPrice, byDevice: [{device, entries, words, totalUsd}]}], devices: [{id, name, platform}]}`, newest month first |
| `PATCH /v1/device {name}` | `204` |
| `DELETE /v1/device` | `204` (the token is revoked) |
| `POST /v1/sync-complete {uploaded, downloaded, pending, appVersion}` | `204`; only send when `capabilities.syncReport` is true. Counts describe the completed cycle; `pending: 0` updates the device's last successful sync |

Push statuses:
- `applied` → stored as `seq`;
- `stale` → the server has a newer HLC, and `current` is its record (apply it);
- `exists` → history entry already stored;
- `rejected` → invalid, with `error`: `invalid_kind`, `invalid_id`, `invalid_hlc`, `clock_skew`,
  `payload_too_large`, `invalid_stats`, `missing_payload`.

Limits: request bodies and pull responses ≤ 1 MiB, payloads ≤ 64 KiB, HLC strings ≤ 128 bytes, 120 requests per minute per token
(`429 rate_limited`).
Clients split push batches by serialized UTF-8 size (target 768 KiB), including JSON escaping and
envelope overhead. Pulls can return fewer records than `limit` because of the byte limit; always
follow `hasMore`. Feed sequence numbers remain monotonic across encryption resets and retention.

## 8. Client algorithm (summary)
1. Compare the local profile with the last synced snapshot (except on the first sync). Every
   difference becomes a change with a fresh HLC in the outbox. Doing this before any network call
   gives offline edits an HLC close to when they were made.
2. `GET /v1/me`.
   - No keyring: the first device creates DK and pushes the keyring when pairing.
   - Keyring with an unknown `keyId`: ask for the passphrase, unwrap DK, store it.
3. Pull pages from the saved cursor, decrypt, observe the HLCs. A pulled record older than (or equal
   to) the snapshot is skipped; if the outbox has a newer change of the same record, the outbox wins,
   otherwise the pulled record replaces it. Apply the values locally in `seq` order (without echoing
   them back). Persist each page's encrypted profile/history inbox and cursor atomically before
   applying it. Remove inbox entries only after local writes succeed; failed entries are retried.
4. Compare the local profile with the snapshot again (first sync: what the server didn't have).
5. Push the outbox:
   - `applied` / `exists` → remove from the outbox;
   - `stale` → apply `current`;
   - `rejected` → drop, and log it.
6. Upload new history entries (`history` records with `stats`).
7. Persist the completed local state, then optionally report cycle counts with `/v1/sync-complete`.
   Validate push acknowledgement count/order/record IDs before removing outbox entries. Persist
   stale `current` records before another network request, so interrupted pushes are recoverable.
