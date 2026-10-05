# Build from the repository root: docker build -f server/Dockerfile -t wisprcheap-server .
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
# The workspace root is the desktop app: its sources are needed for Cargo to load the workspace,
# but only the server is built.
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY desktop-ui desktop-ui
COPY sync sync
COPY server server
RUN cargo build --release --locked -p wisprcheap-server \
    && cp target/release/wisprcheap-server /wisprcheap-server \
    && mkdir /data

# Static musl binary (SQLite is bundled) on a minimal image, running as a non-root user (uid 65532).
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /wisprcheap-server /usr/local/bin/wisprcheap-server
# Owned by the non-root user, so a named volume on /data is writable (a bind mount needs a chown).
COPY --from=build --chown=65532:65532 /data /data
ENV WCS_DATA_DIR=/data \
    WCS_BIND=0.0.0.0:8080
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/wisprcheap-server"]
CMD ["serve"]
