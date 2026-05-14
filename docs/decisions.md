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

---

## Operator-set constraints (D9-D13, added 2026-05-13)

The operator specified additional constraints after M0 was scaffolded. These are now part of the v0.1 contract.

## D9 — Test LLM endpoint

- Operator-provided test LLM is reachable at `http://192.168.1.193:8000` (assumed OpenAI-compatible API per standard vLLM / llama-cpp-server / LM Studio conventions on port 8000).
- The `pg-synapse-provider-openai` plugin MUST expose a configurable `base_url` field in `LlmProfileRow.params` so it can target any OpenAI-compatible endpoint (real OpenAI, vLLM, llama-cpp-server, LM Studio, Ollama with `/v1` shim, etc.).
- Live integration tests for the operator scenario use this endpoint when `PG_SYNAPSE_TEST_LLM_BASE_URL` env var is set.
- Default test profile: `provider="openai"`, `model="<discovered-at-runtime>"`, `base_url="http://192.168.1.193:8000/v1"`, no API key required (assumed unauthenticated on internal LAN).

## D10 — Local model selection for offline testing

- LLM: Granite 4 family, models under 2B parameters (e.g., `ibm-granite/granite-3.1-1b-a400m-instruct`, `ibm-granite/granite-3.1-2b-instruct`, future granite-4 variants in the same size class once released).
- Embeddings: BGE family from BAAI (`BAAI/bge-small-en-v1.5` for 384-dim, `BAAI/bge-base-en-v1.5` for 768-dim).
- These are downloaded on demand by the `download` feature of the relevant plugin (`pg-synapse-embeddings-ort`, or a llama-cpp-server invocation).
- Local cache location: `~/.cache/pg-synapse/models/` (XDG cache home if available).

### D10 addendum — vLLM endpoint reality check (live test 2026-05-13)

- The operator's vLLM endpoint at `http://192.168.1.193:8000/v1` was probed via `GET /v1/models`.
- It is currently serving model id `Intel/Qwen3-Coder-Next-int4-AutoRound` (Intel's 4-bit AutoRound quantization of Qwen3-Coder-Next).
- For now, all sample agent profiles target this model id by default. Operators wanting Granite 4 can change the vLLM launch args; the plugin doesn't care about the underlying model.
- The `PG_SYNAPSE_TEST_LLM_MODEL` env var (default in tests/live.rs) controls which model id the test suite uses.
- Live test passed: `complete()` returned "pong" when asked for it, `list_models()` returned the qwen-coder id.

## D11 — SQL read/write tool capability

- The operator's goal: agents can read and write Postgres tables via tool calls inside `pg_synapse.execute(...)`.
- v0.1 ships a built-in `sql_query` and `sql_exec` tool registered automatically by the pgrx host (and a forwarded variant in the sidecar host).
- Security: these tools execute via SPI using the CURRENT_USER role (not the SECURITY DEFINER role of the wrapping function), so existing Postgres role-based grants apply unchanged. The agent cannot escape the caller's privileges.
- The pgrx host registers them via a built-in `BuiltinTool` plugin loaded at `_PG_init`. Operators can disable via the GUC `pg_synapse.disable_builtin_sql_tools = true`.
- Schema:
  - `sql_query(query: text, params: json[]) -> json[]` — read; binds positional params; returns rows as JSON array.
  - `sql_exec(statement: text, params: json[]) -> json` — write; returns `{"rows_affected": N}`. Wrapped in the same SAVEPOINT as other tool calls.

## D12 — Docker Postgres for tests

- Operator constraint: any Postgres needed for tests runs in a Docker container on a NON-USED port (no default 5432).
- M7 / M8 test harnesses spin up `postgres:17-bookworm` (and pg15 / pg16 variants) on dynamically allocated ports.
- The port is chosen by binding to `127.0.0.1:0` to discover a free port, then docker `-p <port>:5432`.
- Container name pattern: `pg-synapse-test-pg{15,16,17}-<random-suffix>`; auto-removed via `--rm`.
- Test fixtures use a connection string like `postgres://postgres:postgres@127.0.0.1:<port>/postgres`.

## D13 — Repeatability requirement

- Operator constraint: every workflow demonstrated must be repeatable end-to-end.
- All sample agentic workflows under `examples/` are paired with a shell script `examples/<name>/run.sh` that:
  1. Starts a fresh dockerized Postgres,
  2. Installs the extension (M7) or runs `sidecar-install.sql` against the sidecar (M8),
  3. Inserts the seed data,
  4. Runs the agent flow,
  5. Asserts expected outcome,
  6. Tears down.
- Same script forms the basis of `make demo` and CI integration jobs.

## Local repo status

- The local git repository is ready (4 commits on `main`) with no remote configured.
- The operator instructed that no external origin push happens at M0. The operator will create the GitHub remote when ready.
