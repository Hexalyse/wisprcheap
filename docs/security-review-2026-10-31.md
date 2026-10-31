# Security review — 31 October 2026

Follow-up: the [GTK4 migration](gtk4-migration.md) supersedes the GLib backport described below.
GTK3, Tao, the old layer-shell bindings and GLib 0.18 have been removed from Cargo.lock, and native
Linux validation now covers the replacement. This report records the original audit and initial fixes.

## Scope and result

Reviewed the desktop client, desktop companion IPC, shared sync/crypto implementation,
sync server/API/web UI, deployment examples, dependency lockfile and GitHub security reports.
Reviewed Android separately in `C:\dev\wisprcheap-android`; its findings are in that repository's
`docs/security-review-2026-10-31.md`.

Baseline: `Hexalyse/wisprcheap` commit `fb26a803e4e97d6825b709b5678819d7b125c088`.
The checkout matched `origin/master` at the start of review. Fixes are local, uncommitted changes;
no release, deployment, advisory dismissal or GitHub settings change was performed.
This was a source/dependency review and local regression test exercise, not a penetration test of a deployed instance.
Severity estimates for newly found code issues below are review judgments, not assigned CVSS scores.

## GitHub security reports

The authenticated GitHub API returned:

- One open Dependabot alert: [alert #1](https://github.com/Hexalyse/wisprcheap/security/dependabot/1),
  **medium**, `glib` 0.18.5, [GHSA-wrw7-89jp-8q8g](https://github.com/advisories/GHSA-wrw7-89jp-8q8g)
  / [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html).
- No repository security advisories returned.
- No open secret-scanning alerts returned.
- Code-scanning API returned `404: no analysis found`. This provides no SAST assurance.

Android returned no Dependabot alerts, no repository advisories and no open secret-scanning alerts;
its code-scanning API also returned `no analysis found`. Empty alert lists do not establish that software is secure.

## Confirmed findings and applied fixes

### 1. GLib memory unsafety — medium, GitHub advisory

`VariantStrIter::impl_get` passes an immutable pointer reference to a C output parameter, causing
undefined behavior and potentially crashes under optimization. The affected dependency is in the
Linux GTK3 desktop stack; it is not an Android or standalone sync-server dependency.

GLib >=0.20 fixes the issue, but GTK3/Tao/tray-icon/gtk-layer-shell require GLib 0.18. A standalone
version bump cannot satisfy their APIs. Vendored the original GLib 0.18.5 crate, retaining its license
and version, and applied the exact two-line [upstream fix](https://github.com/gtk-rs/gtk-rs-core/pull/1343).
The workspace patch covers transitive consumers; the Docker build copies the vendored source too.
See `vendor/glib/SECURITY-PATCH.md` for provenance, checksum and removal criteria.

Verified that `src/variant_iter.rs` is the only changed upstream file, that its diff is exactly the
upstream fix, and that Linux dependency resolution selects the local crate. Added forward/reverse/mixed
iterator regression coverage and a CI job running upstream iterator tests in release mode.

The optimized Linux runtime test could not run on this Windows host. CI must run it before release.
Version-only scanners may continue flagging 0.18.5. Cargo Audit does not assess local path packages;
the source comparison and regression tests are therefore essential evidence. The GitHub alert was not dismissed.

### 2. Redirect disclosure of provider credentials and private payloads — medium, conditional

Both clients followed HTTP redirects. Redirects can forward audio, transcript/LLM bodies and pairing
codes; custom `xi-api-key` headers are not protected by the standard Authorization-header stripping rule.
Exploitation requires a redirect from a configured endpoint, for example through an untrusted endpoint,
a compromised service/proxy, or a plaintext transport attacker.

Disabled redirects in the desktop provider and sync clients, removed the sync client's permissive
fallback HTTP client, and restricted caller-supplied OkHttp clients on Android. Clients now return a
redirect error and require the configured URL to be updated explicitly. Added network regressions
using a separate redirect destination and dummy credentials.

### 3. Untrusted sync-server resource exhaustion — medium

Both clients accepted server-provided Argon2 memory/iteration/lane parameters without an application
budget and read sync responses into memory without a size cap. A malicious sync server could exhaust
memory or monopolize CPU when a user unlocks/syncs.

Both implementations now reject unsupported KDF parameters before allocation: Argon2id only,
memory at most 64 MiB, at most 10 iterations and 8 lanes, `m >= 8*p`, and a 16-byte salt.
The server enforces compatible bounds, additionally retaining its minimum 8 MiB memory requirement.
Both sync HTTP clients cap response bodies at 2 MiB, including chunked bodies and error responses.
Regression tests cover extreme KDF values, invalid salts and oversized/chunked responses.
Default KDF settings and the shared interoperability vectors are unchanged. Existing custom keyrings
above these bounds require rewrapping with supported parameters using a trusted older client.

### 4. Unbounded login hashing and limiter state — medium

The web layer launched Argon2 work with unrestricted `spawn_blocking`; login backoff was updated after
verification, allowing concurrent attempts to enter expensive work before throttling. Failure-key
storage was unbounded, and login names/passwords could consume the full request-body allowance.

Added a separate two-job password-work budget that rejects overload with HTTP 429 and Retry-After.
Permits stay with blocking work after request cancellation. Dummy-hash initialization uses the same
budget. Added a per-IP admission rate, username validation, a 1,024-byte password limit and bounded
limiter maps. Active limits cannot be evicted by short-window keys. A saturation regression verifies
that readiness remains responsive. A distributed denial of service still requires edge-level protection.

### 5. Spoofable forwarded address bypasses throttling — medium, proxy deployment dependent

With `WCS_TRUST_PROXY=1`, the server selected the first X-Forwarded-For value. A proxy that appends the
real client address preserves an attacker-supplied prefix, allowing the attacker to choose limiter keys.

The server now parses the rightmost address as an IP and falls back to the socket peer on invalid input.
Updated tests and proxy documentation. This setting requires one trusted proxy which appends or replaces
the header, and a backend inaccessible directly to clients. Multi-proxy deployments intentionally use
the nearest upstream hop; explicit trusted-proxy-chain configuration is future work.

### 6. Local exposure of credentials through Unix file modes — high on a shared host

Desktop configuration contains the device token and data key, and can contain API keys. Configuration
and first-sync backup writes inherited the process umask, commonly creating mode-0644 files readable
by other local accounts. `.env` permissions were restricted only after the initial write, and existing
files could retain broad permissions. Companion saves preserved those broad modes.

Configuration, `.env` and sync backup writes now use atomic mode-0600 temporary files. Config editing
also tightens existing configuration permissions. Companion saves retain private Unix modes.
History/log appends create or tighten files to mode 0600 before writing; saved failed audio uses
private atomic writes too.
The server creates its database and backups with mode 0600, restricts existing WAL/SHM files, creates
new data directories privately, and the systemd example uses UMask 0077.
Added storage regressions, including Unix permission and symlink-replacement checks.

Previously created backup and failed-audio files are not migrated automatically. On existing shared Linux hosts,
restrict legacy config/`.env`/backup/audio files and rotate credentials/data keys if unauthorized reading
was possible. These changes do not introduce Windows DPAPI storage; Windows still relies on user ACLs.

## Additional dependency advisories patched

Cargo Audit 0.22.2 used RustSec database commit `2295466c4e9df26647fd67d8fa1c705ac2fc7f81`
(database update 30 October 2026). Its baseline found seven vulnerability entries in addition to
GLib/Tokio/lru memory-unsafety warnings. Updated compatible locked dependencies:

- `ctutils` 0.4.2 → 0.4.7:
  [RUSTSEC-2026-0385](https://rustsec.org/advisories/RUSTSEC-2026-0385.html), constant-time operations.
- `rustls` 0.23.45 → 0.23.46:
  [RUSTSEC-2026-0346](https://rustsec.org/advisories/RUSTSEC-2026-0346.html),
  [0347](https://rustsec.org/advisories/RUSTSEC-2026-0347.html),
  [0348](https://rustsec.org/advisories/RUSTSEC-2026-0348.html),
  [0349](https://rustsec.org/advisories/RUSTSEC-2026-0349.html).
  These address session-secret debugging output, ECH handling, certificate decompression limits and
  key-exchange downgrade behavior. Feature-specific reachability varies; no application-level exploit was established.
- `rustls-webpki` 0.103.15 → 0.103.16:
  [RUSTSEC-2026-0382](https://rustsec.org/advisories/RUSTSEC-2026-0382.html), distrusted intermediate certificates.
- `webpki-root-certs` 1.0.9 → 1.0.11:
  [RUSTSEC-2026-0383](https://rustsec.org/advisories/RUSTSEC-2026-0383.html), distrusted certificate roots.
- `tokio` 1.53.1 → 1.54.1:
  [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html), Unix AsyncFd safety.
- Replaced yanked `clipboard-win` 5.4.1 and `yoke-derive` 0.8.3 with compatible non-yanked releases.

After the updates, Cargo Audit reports **zero vulnerability-category entries**. This is not a clean
bill of health: local backports, the remaining unsoundness warning and unmaintained libraries need separate review.
Added weekly Cargo Audit/optimized GLib tests and Dependabot update configurations for both repositories.
No advisory is globally ignored.

## Remaining risks and recommended next work

1. **Migrate the GTK3 desktop stack.** Unmaintained warnings remain for gtk-layer-shell and its sys crate,
   basic-toml, proc-macro-error, ttf-parser, unsafe-libyaml, zbus_names, zvariant and zvariant_derive.
   Updating immediate versions cannot eliminate every transitive dependency. The local GLib backport
   is an interim maintenance responsibility, not a maintained GTK3 ecosystem.
2. **Track lru RUSTSEC-2026-0253.** `lru` 0.16.4 remains in Cargo.lock through optional
   `iced_wgpu → cryoglyph`. `cargo tree --locked --workspace --target all -e normal,build,dev`
   does not include those packages with current tiny-skia features, and the tested build does not
   compile them. Reassess and upgrade/backport before enabling the GPU renderer. The advisory is not ignored.
3. **Tighten plaintext HTTP policy.** Both clients intentionally support HTTP/LAN endpoints; Android's
   manifest allows cleartext globally. A network attacker can read tokens/API requests and modify
   sync traffic on HTTP. Use HTTPS for deployments and consider per-host explicit insecure opt-in.
4. **Strengthen sync integrity in a protocol revision.** AES-GCM binds encrypted content to user/kind/ID,
   but HLC ordering and deletion flags are not authenticated with the payload; tombstones have no
   encrypted payload. A malicious relay can replay old ciphertext under a newer HLC, suppress changes,
   or forge deletions. Encryption protects content confidentiality, not malicious-server freshness or
   deletion integrity. Authenticate record metadata/tombstones and add replay/reset protections in a
   coordinated desktop/Android/server protocol version. Server rejection of malformed data is insufficient.
5. **Expand automated security coverage.** Add supported CodeQL/SAST and ongoing resolved Android
   dependency scanning; the checked GitHub repositories had no code-scanning analysis. The Android
   OSV scan here is a point-in-time scan. Provider response bodies outside sync still lack a general
   application byte cap and merit the same hardening if untrusted custom endpoints are supported.
6. **Apply operational quotas.** Pairing/login/request limits and bounded password work do not impose
   account storage quotas or protect against all connection floods. Set reverse-proxy connection/body
   timeouts, storage monitoring and quotas for any multi-user/public deployment.

## Validation and limits

- `cargo test --locked --workspace`: **120 passed, one existing preview test ignored** on Windows.
- Focused network security tests rerun after changing the redirect fixture to a separate destination: passed.
- Existing sync end-to-end, user isolation, token revocation, disabled-account, CSRF, keyring, encrypted
  record binding and cross-language vector tests passed.
- `cargo audit --json`: zero vulnerability-category entries; residual warnings documented above.
- Vendored upstream file comparison and Linux Cargo dependency resolution: passed.
- Android `:core:test :app:assembleDebug :app:lintDebug`: **98 tests passed**, build successful,
  lint has zero errors and eight non-security warnings.
- Resolved Android release runtime dependencies queried against OSV: **110 coordinates, no known advisories returned**.
- Source patterns for common private-key/token formats found no matches; this was not a full history/entropy secret scan.
- `git diff --check` passed in both repositories.

Linux native execution, Unix permission regressions and the optimized GLib regression await Linux CI.
No production deployment, Android device instrumentation, Windows cross-user ACL attack, full historical
secret scan, fuzzing campaign or independent penetration test was performed.
