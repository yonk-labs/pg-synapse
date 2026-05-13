# pg_synapse v0.1 — Locked Decisions

Resolved from spec Section 19 during M0. Each decision is binding for v0.1.

## D1 — Crate names

Verified via `cargo search` on 2026-05-13:

```
$ cargo search pg-synapse --limit 5
(no results)

$ cargo search pg-synapse-core --limit 5
(no results)

$ cargo search pg-synapse-pgrx --limit 5
(no results)

$ cargo search pg-synapse-sidecar --limit 5
(no results)
```

All `pg-synapse-*` names are unclaimed on crates.io as of M0.

Decision:
- `pg-synapse-core`, `pg-synapse-pgrx`, `pg-synapse-sidecar`
- `pg-synapse-provider-openai`, `pg-synapse-provider-anthropic`, `pg-synapse-provider-llama-cpp`
- `pg-synapse-embeddings-ort`
- `pg-synapse-tools-http`
- `pg-synapse-macros` (proc-macro crate for `#[derive(Tool)]`, planned for M6)

Fallback if collision found: replace `-core` with `-kernel`, `-pgrx` with `-extension`.

## D2 — GitHub namespace

- `yonk-labs/pg-synapse` (verified accessible).
- HTTP status check on https://github.com/yonk-labs returned: `200`.

```
$ curl -s -o /dev/null -w "%{http_code}\n" https://github.com/yonk-labs
200
```

## D3 — pgrx version pin

- `pgrx = "=0.18.x"` (exact at M0; bumps allowed in v0.1.x stream).
- Supported Postgres: 15, 16, 17.
- Latest pgrx from cargo search on 2026-05-13: **0.18.0**.

```
$ cargo search pgrx --limit 1
pgrx = "0.18.0"    # pgrx:  A Rust framework for creating Postgres extensions
... and 40 crates more (use --limit N to see more)
note: to learn more about a package, run `cargo info <name>`
```

**Deviation from spec template:** The M0 task template suggested `pgrx = "=0.13.x"`. The actual
latest pgrx on crates.io at M0 is 0.18.0. We pin to the 0.18.x line so we get the current,
maintained release stream rather than a pre-PG17 series. Re-verify PG15/16/17 compatibility at M7.

## D4 — Embeddings plugin scope

- `pg-synapse-embeddings-ort` SHIPS in v0.1 (M5).
- Feature flags: `gpu`, `download`.
- Re-evaluate at M5 entry: if ORT integration exceeds 4 days of effort, defer to v0.2 and document the deferral.

## D5 — JSON Schema dialect for `tools.schema_json`

- Draft 2020-12 (matches MCP spec; future-aligned).
- Validate via `jsonschema` crate (or `boon` if more actively maintained at M2 entry).

## D6 — `tracing` crate integration

- Kernel uses `tracing` macros throughout.
- A `TracingSubscriber` adapter in `pg-synapse-pgrx` and `pg-synapse-sidecar` captures structured events and writes them into `pg_synapse.traces.payload` JSONB.
- `traces.payload` schema: `{"event": "<event_type>", "fields": {<flattened span fields>}, "level": "info|warn|error"}`.

## D7 — Sidecar HTTP API endpoints (v1)

- `POST /v1/execute` — sync agent execution
- `POST /v1/execute_async` — async (returns execution_id)
- `GET /v1/status/{execution_id}` — poll async
- `POST /v1/embed` — embedding
- `POST /v1/tool_call` — direct tool invocation (for testing)
- `GET /v1/health` — liveness
- `GET /v1/version` — server version + commit hash
- `POST /v1/admin/agent` — admin: create/update agent
- `POST /v1/admin/profile/llm` — admin: create/update LLM profile
- `POST /v1/admin/profile/embedding` — admin: create/update embedding profile
- `POST /v1/admin/secret` — admin: write secret
- `POST /v1/admin/tool` — admin: register tool
- All `/v1/admin/*` require a shared-secret header (`X-PG-Synapse-Admin-Token`) the operator configures.

## D8 — LISTEN/NOTIFY on sidecar startup failures

- DEFERRED to v0.2.
- v0.1: sidecar logs failures to stderr; operator monitors via systemd/journald or container logs.
