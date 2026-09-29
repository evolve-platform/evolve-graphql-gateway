# syntax=docker/dockerfile:1
#
# Reusable evolve-gateway image: a custom Hive Router binary with the
# federated_token plugin compiled in, plus the standard router config.
#
#   docker build -t evolve-gateway .
#
# Both git dependencies (graphql-hive/router and the federated-token plugin) are
# public, so no credentials are needed at build time.

ARG RUST_VERSION=1.98

##
# chef: shared base with cargo-chef for dependency-layer caching (hive-router is
# a large git dependency; caching its compile is the whole point of the split).
##
FROM rust:${RUST_VERSION}-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

##
# planner: distill the dependency graph into recipe.json (changes only when
# Cargo.toml / Cargo.lock change, keeping the cook layer cached).
##
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

##
# builder: cook dependencies (cached), then build the binary.
##
FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
RUN cargo build --release --bin evolve-gateway

##
# runner: minimal glibc runtime (binary is dynamically linked against bookworm).
##
FROM debian:bookworm-slim AS runner

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl dumb-init \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 1001 --user-group --create-home app

# Hive Router's own env overrides; both are overridable at runtime.
ENV HOST=0.0.0.0 \
    PORT=4000

USER app
WORKDIR /app

COPY --from=builder /app/target/release/evolve-gateway /usr/local/bin/evolve-gateway

# The standard config. It is a template — the binary renders its ${VAR}
# placeholders from the environment at startup and writes the result next to
# this file, so /app must stay writable by the app user.
#
# To use a project-specific config instead, mount it and set
# ROUTER_CONFIG_FILE_PATH; the same templating applies to it.
COPY --chown=app:app router.config.yaml ./router.config.yaml

# NOTE: the supergraph is NOT baked in. Set HIVE_CDN_ENDPOINT + HIVE_CDN_KEY to
# poll it from the Hive CDN, or mount a file and point SUPERGRAPH_FILE_PATH at it.

EXPOSE 4000

HEALTHCHECK --interval=30s --timeout=3s --start-period=20s \
    CMD curl --fail http://127.0.0.1:${PORT}/health || exit 1

ENTRYPOINT ["/usr/bin/dumb-init", "--", "/usr/local/bin/evolve-gateway"]
