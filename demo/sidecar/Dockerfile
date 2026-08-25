# The pg-synapse-sidecar binary host (out-of-process kernel).
#
# Unlike demo/harness, the sidecar crate has workspace path deps
# (pg-synapse-core, the tool plugins), so the build context is the REPO ROOT:
#   docker build -t pg-synapse-sidecar -f demo/sidecar/Dockerfile .
#
# Config is read from the environment (clap fallbacks):
#   DATABASE_URL, PG_SYNAPSE_PORT (default 8088), PG_SYNAPSE_ADMIN_TOKEN.

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release -p pg-synapse-sidecar

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/pg-synapse-sidecar /usr/local/bin/
EXPOSE 8088
ENTRYPOINT ["pg-synapse-sidecar"]
