# WisprCheap sync server 0.2.2

- Handle both `SIGINT` and Unix `SIGTERM` for graceful server shutdown.
- Distinguish malformed sync payloads (`invalid_payload`) from oversized ones (`payload_too_large`).
- Use constant-time comparison for web admin CSRF tokens and `SETUP_KEY` verification.

The sync protocol and database formats remain compatible.

Docker image: `ghcr.io/hexalyse/wisprcheap-server:0.2.2` (amd64 and arm64).
