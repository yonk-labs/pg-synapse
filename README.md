# pg_synapse

pg_synapse is a Postgres-native agent-loop runtime in Rust. It runs agent execution loops, tool calling, and embedding workflows close to your data — either as a Postgres extension (via pgrx) or as a sidecar HTTP service that connects back to Postgres. The design contract lives in `docs/design.md`.

Status: pre-alpha, in active development.
