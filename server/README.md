# wisprcheap sync server

Optional, self-hosted server that syncs your configuration between the
[wisprcheap desktop app](../README.md) and the [Android app](https://github.com/Hexalyse/wisprcheap-android):
dictionary, prompts, models, translation pairs and **API keys**. It also gathers your cost and usage
statistics from all devices on one dashboard.

- **End-to-end encrypted.** API keys, settings, dictionary terms and dictated text are encrypted on
  your devices with a *sync passphrase* before they're sent. The server (and its admin) only sees
  statistics: costs, durations, models, token and word counts, timestamps, device names.
- **Several users.**
  - An admin account invites the others.
  - Each device gets its own revocable token by pairing: a QR code or a one-time code. Devices never
    know the account password.
- **Small.** One binary (or Docker image) with an SQLite database, no other service.

The design and the wire protocol are in [PLAN.md](PLAN.md) and [../sync/SPEC.md](../sync/SPEC.md).

## Deploy with Docker

The image (`ghcr.io/hexalyse/wisprcheap-server`) runs on `linux/amd64` and `linux/arm64` (Raspberry Pi 3
and newer with a 64-bit OS).

```sh
mkdir -p wisprcheap/data && cd wisprcheap
sudo chown 65532:65532 data          # the image runs as a non-root user
# (without sudo: docker run --rm -v "$PWD/data:/data" busybox chown 65532:65532 /data)
curl -O https://raw.githubusercontent.com/Hexalyse/wisprcheap/master/server/docker-compose.example.yml
mv docker-compose.example.yml docker-compose.yml   # edit WCS_PUBLIC_URL
docker compose up -d
docker compose logs wisprcheap       # shows the one-time admin setup link
```

**HTTPS is your part.** Run the container behind the reverse proxy you already use (nginx-proxy +
acme-companion, Caddy, Traefik, nginx…), which terminates TLS and forwards to port 8080.
- `WCS_PUBLIC_URL` must be the public `https://` address. It's used in the QR codes and for the
  cookies.
- The server refuses to start with a plain `http://` public URL unless it's localhost, or you set
  `WCS_ALLOW_HTTP=1` for a trusted LAN.

## Without Docker

Download the static Linux binary from the `server-v*` releases (or `cargo build --release -p
wisprcheap-server`), then use [wisprcheap-server.service](wisprcheap-server.service) as a systemd
unit.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `WCS_PUBLIC_URL` | `http://localhost:8080` | Public base URL (https:// in production) |
| `WCS_BIND` | `0.0.0.0:8080` | Listen address |
| `WCS_DATA_DIR` | `data` (`/data` in Docker) | Database and backups |
| `WCS_TRUST_PROXY` | off | Use `X-Forwarded-For` for rate limits and logs (only behind a proxy) |
| `WCS_ALLOW_HTTP` | off | Allow a plain-HTTP public URL (LAN without TLS) |
| `WCS_BACKUP_DAILY` | off | Daily consistent backup into `<data dir>/backups` (keeps 7) |
| `WCS_HISTORY_RETENTION_DAYS` | `0` | Remove server history older than this many days; `0` keeps it indefinitely |
| `WCS_AUDIT_RETENTION_DAYS` | `0` | Remove audit events older than this many days; `0` keeps them indefinitely |
| `WCS_LOG` | `info` | Log level (`debug`, `warn`…) |

## First steps

1. **Admin account.** Open the setup link printed in the log at the first start (it works once), or
   run `wisprcheap-server admin create <username>`. In Docker, that's
   `docker compose exec wisprcheap wisprcheap-server admin create <name>`. To rename an account later
   (devices and data are kept): `wisprcheap-server admin rename <username> <new-username>`.
2. **Your devices.** Devices → **Add a device** shows a QR code and a one-time code, valid 10 minutes.
   - **Android:** tap **Open WisprCheap** on the same phone, scan the QR code with the camera, or WisprCheap → Settings → Sync.
   - **Desktop:** `wisprcheap sync pair https://sync.example.com CODE`.
   - The first device asks you to create the **sync passphrase**, and the other devices ask for it.
     If you lose it and no device has it anymore, Account → Reset encryption starts over (the
     statistics stay).
3. **Other people.** Admin → **Create an invitation link**, and send it to them.
   - "Password reset link" works the same way for a forgotten password.
   - The admin never sees or sets passwords.

## Dashboard and history

- Filter by date, device, provider, STT/LLM model or status, then export the matching statistics as
  CSV. Exports allow 10,000 entries; larger exports ask you to narrow the range. Encrypted text and
  keys are never exported. Model/provider filters match exact names.
- Account → **Timezone** controls dates, month boundaries and device times, including daylight saving.
  The device statistics API still accepts a fixed UTC offset for older clients.
- The dashboard reports failure rate and average/p95 STT and LLM latency. Failed and empty attempts
  participate in the failure-rate denominator; missing latency measurements are excluded.
- Known prices are recomputed on the server. Custom-model client estimates are included; missing
  STT or LLM prices are marked as incomplete. Totals include available component costs and any
  larger client total estimate; the actual cost may be higher when a component is unpriced.
- Devices distinguishes **Last contact** from **Last successful sync**, with upload/download counts
  and pending changes from the last reported cycle. Successful-sync reporting needs updated clients;
  older clients remain compatible and show contact only.
- Pairing pages offer copy buttons, expiry feedback, and confirmation when the one-time code is
  redeemed. This confirms pairing; passphrase setup continues in the app.

## Transfers and recovery

Uploads are split by the actual UTF-8 JSON size (target 768 KiB, hard request limit 1 MiB), with at
most 500 records and 64 KiB per encrypted payload. Pull pages stop at 1 MiB even when fewer records
fit than the requested count; follow `hasMore` and `nextSince` until complete.

Clients persist an encrypted inbox atomically with each pull cursor before applying changes. A
later network failure or local write failure can be retried without losing fetched data. Android
sync writes wait for settings, keys and history to be persisted before acknowledging them.

SQLite work runs on a bounded blocking pool; reporting uses indexed numeric fields rather than
decoding the entire history. Existing statistics are indexed once during the first upgraded start.

## Backups and upgrades

- `wisprcheap-server backup` (or `WCS_BACKUP_DAILY=1`) writes a consistent copy of the database while
  the server runs.
- To restore: stop the server, replace `data/wisprcheap.db`, start it again.
- A backup contains no secrets in clear: passwords, tokens and sessions are hashed, and API keys and
  texts are encrypted with keys the server never has.
- Upgrading: pull the new image and restart. Database migrations run at startup.
- Admin shows backup attempt/success/failure, database size including WAL/SHM, readiness and the last
  cleanup. `/healthz` and `/readyz` both check a live database connection and return 503 if unavailable.
- Expired sessions, invites and pairing codes are cleaned at startup and hourly. Retention is opt-in:
  server history is tombstoned in batches of 1,000 per cleanup to prevent stale clients resurrecting
  expired entries; local device history is retained. Deleted-history markers remain on the server.

## Security notes

- Web logins are throttled per IP and per username (5 failures, then an increasing wait).
- Every form checks a CSRF token and the Origin.
- The pages send a strict Content-Security-Policy. Small same-origin JavaScript helpers handle copy
  buttons and pairing confirmation; private pages and API responses use `Cache-Control: no-store`.
- Device tokens (`wcs_…`) can only sync their own user's data; revoke a lost device from the Devices
  page.
- The audit log (Account, and Admin for all users) records logins, pairings, revocations, password
  and passphrase changes, and encryption resets.
