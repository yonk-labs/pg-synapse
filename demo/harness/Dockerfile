# The axum demo harness + static web UI.
#
# Build context is demo/harness (the crate builds standalone; it has no
# path dependencies on the workspace):
#   docker build -t pg-synapse-demo-harness demo/harness

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/pg-synapse-demo-harness /usr/local/bin/
EXPOSE 8080
CMD ["pg-synapse-demo-harness"]
