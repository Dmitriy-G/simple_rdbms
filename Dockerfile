# Builder: compiles the headless server binary in release mode.
FROM rust:1-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p server

# Runtime: a minimal image with only the binary and what it needs to run,
# never the toolchain that built it.
FROM debian:stable-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

# Fixed uid/gid so the volume's ownership is stable across image rebuilds.
RUN groupadd --gid 1001 simple_rdbms \
    && useradd --uid 1001 --gid simple_rdbms --create-home --shell /usr/sbin/nologin simple_rdbms

COPY --from=builder /app/target/release/simple_rdbms_server /usr/local/bin/simple_rdbms_server

WORKDIR /app

# The database file's directory. A volume, not the container's writable
# layer - see docs/ROADMAP.md's M13 entry: a database that keeps its data
# in the writable layer loses everything on `docker rm`.
RUN mkdir -p /data && chown simple_rdbms:simple_rdbms /data
VOLUME ["/data"]

USER simple_rdbms

EXPOSE 9090

# Long start period: ARIES recovery on a large log can take minutes, and
# readiness must stay false (503) for the whole window rather than the
# container being killed for taking a while to recover - see
# crates/server/src/health.MD.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5m --retries=3 \
    CMD curl --fail --silent --show-error http://localhost:9090/health/ready || exit 1

ENTRYPOINT ["/usr/local/bin/simple_rdbms_server"]
CMD ["/data/simple_rdbms.db"]
