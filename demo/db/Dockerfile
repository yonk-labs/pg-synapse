# Postgres 17 with the pg_synapse extension baked in.
#
# Stage 1 builds the pgrx extension against the PGDG postgresql-server-dev-17
# headers (the same packaging the official postgres:17 image uses, so
# pkglibdir / sharedir paths line up). Stage 2 copies the packaged artifact
# tree (the .so, the .control, and the generated SQL) into postgres:17.
#
# The demo build drops the default `embed-ort` feature so the image does not
# need ONNX Runtime (the demo never calls synapse.embed). It also drops the
# fs / lede / llama-cpp / anthropic plugins the demo does not use. What stays:
#   pg17            required Postgres version feature
#   tools-http      http_get / http_post / http_head
#   tools-calc      calculator (used by the guardrails scenario)
#   tools-clock     get_current_time
#   tools-delegate  call_agent
# provider-openai and tools-sql are non-optional dependencies of the host
# crate and are always compiled in.
#
# Build from the REPO ROOT:
#   docker build -f demo/db/Dockerfile -t pg-synapse-demo-db .

FROM rust:1-bookworm AS builder

# PGDG repo for postgresql-server-dev-17; clang/libclang for pgrx bindgen.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      postgresql-common ca-certificates gnupg clang libclang-dev pkg-config \
 && YES=1 /usr/share/postgresql-common/pgdg/apt.postgresql.org.sh \
 && apt-get update \
 && apt-get install -y --no-install-recommends postgresql-server-dev-17 \
 && rm -rf /var/lib/apt/lists/*

# cargo-pgrx must match the pinned pgrx crate version (=0.18.0).
RUN cargo install --locked cargo-pgrx --version 0.18.0

RUN cargo pgrx init --pg17 /usr/lib/postgresql/17/bin/pg_config

WORKDIR /src
COPY . .

# `cargo pgrx package` builds --release and lays the artifacts out under
# target/release/pg_synapse_pgrx-pg17/ mirroring the absolute install paths
# reported by pg_config (usr/lib/postgresql/17/lib, usr/share/postgresql/17).
RUN cd crates/pg-synapse-pgrx \
 && cargo pgrx package --no-default-features \
      --features "pg17 tools-http tools-calc tools-clock tools-delegate"

FROM postgres:17

COPY --from=builder /src/target/release/pg_synapse_pgrx-pg17/ /
COPY demo/db/initdb/ /docker-entrypoint-initdb.d/
