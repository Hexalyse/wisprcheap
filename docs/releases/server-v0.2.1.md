# WisprCheap sync server 0.2.1

- Bound concurrent password hashing/verification, cap password input, and reject overload with HTTP 429.
- Limit admission to login attempts and bound in-memory limiter state.
- Use the rightmost validated forwarded address behind a trusted proxy, preventing spoofed prefixes
  from selecting rate-limit keys. Block direct backend access when enabling WCS_TRUST_PROXY.
- Validate keyring KDF budgets consistently with updated desktop/Android clients.
- Create or tighten SQLite databases, sidecars and backups to private Unix permissions; add UMask 0077
  to the systemd example.
- Update shared cryptographic/runtime dependencies affected by security advisories.

The sync protocol and database formats remain compatible. Existing custom keyrings above the new
KDF limits need rewrapping using a trusted older client. Review legacy backup permissions separately.

Docker image: `ghcr.io/hexalyse/wisprcheap-server:0.2.1` (amd64 and arm64).
