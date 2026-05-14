#!/usr/bin/env bash
# Repeatable harness for the sql-agent-readwrite demo.
#
# v0.1-alpha note: the recommended path is `cargo pgrx run pg17` followed by
# `\i seed.sql` and `\i workflow.sql`. The docker-based harness below spins up
# a Postgres 17 container but does NOT yet install the compiled extension into
# it; bind-mounting the built .so requires the .so to match the container's PG
# build environment. A future iteration will either build the .so against the
# container or ship a prebuilt image.

set -euo pipefail

if [[ "${1:-}" == "--docker" ]]; then
  PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
  NAME="pg-synapse-demo-$$"

  cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
  trap cleanup EXIT

  echo "Starting Postgres 17 on 127.0.0.1:$PORT ..."
  docker run --rm -d --name "$NAME" \
    -e POSTGRES_PASSWORD=postgres \
    -p "127.0.0.1:$PORT:5432" \
    postgres:17 >/dev/null

  for _ in {1..30}; do
    if docker exec "$NAME" pg_isready -U postgres >/dev/null 2>&1; then break; fi
    sleep 1
  done

  CONN="postgres://postgres:postgres@127.0.0.1:$PORT/postgres"
  echo "Postgres is ready at $CONN"
  echo "TODO: install the pg_synapse_pgrx extension into the container."
  echo "      For now, use the cargo-pgrx path:"
  echo "        cargo pgrx run pg17"
  echo "        \\i $(dirname "$0")/seed.sql"
  echo "        \\i $(dirname "$0")/workflow.sql"
  exit 0
fi

cat <<EOF
Recommended path (cargo-pgrx managed Postgres):

  cd $(git rev-parse --show-toplevel 2>/dev/null || echo .)
  cargo pgrx run pg17

Then, in the psql prompt that opens:

  CREATE EXTENSION pg_synapse_pgrx;
  \\i examples/sql-agent-readwrite/seed.sql
  \\i examples/sql-agent-readwrite/workflow.sql

To explore the docker-based path (work in progress), pass --docker.
EOF
