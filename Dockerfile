# Builder: compiles the headless server binary in release mode, with the
# dependency build cached in its own layer so an ordinary source edit
# doesn't force every one of this workspace's eleven crates' dependencies
# to recompile. Uses cargo-chef rather than the manual
# stub-crate-per-workspace-member approach (write a stub `src/lib.rs` or
# `main.rs` for each crate, build, delete the stubs, copy real sources,
# rebuild) - cargo-chef computes its dependency-only build plan from
# `Cargo.lock`/every `Cargo.toml` alone, so it doesn't need a stub kept in
# sync by hand for each of the eleven members, which is exactly the
# "fiddly... sharp edges" the manual approach has at this crate count.
ARG RUST_VERSION=1.85.0

FROM rust:${RUST_VERSION}-slim-bookworm AS chef
WORKDIR /app
# The slim variant ships only rustc/cargo, not a linker - cargo install
# needs one to link cargo-chef's own binary, so pull in build-essential
# first or the install fails with a bare "exit code: 101" and no
# compiler diagnostics.
RUN apt-get update && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*
# Pinned rather than latest: cargo-chef's own dependencies have crept
# past what rustc 1.85.0 (this stage's pinned toolchain, matched to the
# workspace's rust-version - see crates/server/README.md) can compile.
# 0.1.71 predates Rust 1.85's release, so its published Cargo.lock still
# resolves to dependency versions built for that era.
RUN cargo install cargo-chef --locked --version 0.1.71

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Builds only the dependency graph described by recipe.json - this layer
# is cache-hit as long as no Cargo.toml/Cargo.lock changes, regardless of
# how often application source changes.
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
# Docker COPY doesn't always advance mtimes past what cargo chef cook's
# dummy sources were built with, which can make cargo think the real
# sources copied just now are already-built stale output and skip them;
# touching every source file forces cargo to see them as newer than that
# cached dependency build.
RUN find crates -name '*.rs' -exec touch {} + && cargo build --release -p server

# Runtime: a minimal image with only the binary and what it needs to run,
# never the toolchain that built it. Pinned to the same Debian release
# (bookworm) the builder's rust image is based on, so the runtime's glibc
# is guaranteed at least as new as what the binary was linked against -
# see crates/server/README.md for the pinned versions and why they must
# be bumped together, deliberately, rather than left to float.
FROM debian:bookworm-slim AS runtime

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
# crates/server/src/health.MD. Exec form (no shell) calling the binary's
# own --health-check flag instead of curl - see crates/server/src/main.MD -
# so the runtime image doesn't need curl installed at all.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5m --retries=3 \
    CMD ["/usr/local/bin/simple_rdbms_server", "--health-check"]

ENTRYPOINT ["/usr/local/bin/simple_rdbms_server"]
CMD ["/data/simple_rdbms.db"]
