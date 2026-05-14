# Demo: a SQL agent that reads and writes a Postgres table

## What this shows

An agent registered via `synapse.agent_create(...)` is asked, from SQL, to
manage a small `demo.notes` table. It uses the built-in `sql_query` and
`sql_exec` tools to read and write rows. The LLM (an OpenAI-compatible vLLM
endpoint by default) generates the tool calls; the pgrx-backed kernel
dispatches them through SPI; the rows show up in `demo.notes`.

See `EXAMPLE_OUTPUT.md` for a captured run including timings, token counts,
the agent's tool calls, and the message log.

## Prerequisites

- `cargo pgrx` 0.18+ installed (`cargo install cargo-pgrx --version 0.18.0 --locked`)
- A pgrx-managed Postgres 17 instance (`cargo pgrx init --pg17 download` if missing)
- The extension installed into that pgrx pg17 tree. The simplest way to do this
  is to run `cargo pgrx run pg17` once from the repo root and then `\q` out of
  the psql prompt; this places `pg_synapse_pgrx--0.1.0.sql`,
  `pg_synapse_pgrx.control`, and the `.so` under
  `~/.pgrx/17.<x>/pgrx-install/`.
- An OpenAI-compatible LLM endpoint that supports tool calls. Default:
  `http://192.168.1.193:8000/v1` (vLLM serving
  `Intel/Qwen3-Coder-Next-int4-AutoRound`).

## Path A: automated (recommended for v0.1-alpha)

```bash
cd $(git rev-parse --show-toplevel)
bash examples/sql-agent-readwrite/run.sh
```

`run.sh` does everything end-to-end:

1. Starts the pgrx-managed Postgres if it is not running.
2. Drops and recreates `pg_synapse_demo` so the run is idempotent.
3. `CREATE EXTENSION pg_synapse_pgrx;`
4. Applies `seed.sql` (creates `demo.notes` with 2 seed rows).
5. Applies `workflow.sql` (registers the LLM profile + the `notes_agent`,
   calls `synapse.execute('notes_agent', '...')` twice).
6. Prints the final `demo.notes`, `synapse.executions`, and `synapse.messages`
   contents.
7. Exits 0 if the agent added at least one row, exits 1 otherwise.

### Configuration env vars

| Var | Default | Purpose |
| --- | --- | --- |
| `PG_SYNAPSE_LLM_BASE_URL` | `http://192.168.1.193:8000/v1` | OpenAI-compatible endpoint |
| `PG_SYNAPSE_LLM_MODEL` | `Intel/Qwen3-Coder-Next-int4-AutoRound` | Model id |
| `PGRX_PG_VERSION` | `17` | Which `cargo pgrx` managed pg to use |
| `PGRX_PORT` | `28817` | TCP port that pgrx pg17 listens on |
| `PGRX_HOST` | `${HOME}/.pgrx` | Unix-socket directory pgrx uses |
| `DEMO_DB` | `pg_synapse_demo` | Database name to drop + recreate |

### Manual variant (for poking around)

```bash
cargo pgrx run pg17
```

Then, inside the psql prompt that opens:

```sql
CREATE EXTENSION pg_synapse_pgrx;
\i examples/sql-agent-readwrite/seed.sql
\i examples/sql-agent-readwrite/workflow.sql
SELECT * FROM demo.notes ORDER BY id;
SELECT * FROM synapse.executions ORDER BY started_at;
SELECT * FROM synapse.messages ORDER BY execution_id, seq;
```

## Path B: docker (NOT YET IMPLEMENTED)

`run.sh --docker` exits non-zero with an explanatory message. Building the
`.so` against a container's pg17 build environment is deferred to M7-phase-B
(the v0.1 build only targets the host pgrx install).

## v0.1-alpha caveats

- The built-in `sql_query` / `sql_exec` tools do not yet support positional
  bind parameters. The agent prompt in `workflow.sql` instructs the LLM to
  inline literal values into the SQL string. Full bind support arrives in
  M7-phase-B.
- `synapse.execute()` returns a JSONB envelope on every call. On agent error
  the envelope is `{"error": "...", "status": "errored"}`, not a Postgres
  ERROR, so you can `... ->> 'status'` and route accordingly.
- The `synapse` schema is used instead of `pg_synapse` because Postgres
  reserves the `pg_` prefix. Same reasoning for the `synapse_admin` and
  `synapse_user` role names. GUC names keep the `pg_synapse.*` prefix.
- The default model (`Intel/Qwen3-Coder-Next-int4-AutoRound`) supports
  OpenAI-style tool calls natively on vLLM at the time of capture. If you
  swap in a model without tool-call support, the agent will keep talking
  instead of calling tools, the assertion at the end of `run.sh` will fail,
  and the failure will be visible in `synapse.messages`. Pick a model that
  supports tools, or run vLLM with `--tool-call-parser` set appropriately
  for the model you chose.

## Common failure modes

| Symptom | Likely cause |
| --- | --- |
| `psql: could not connect to server` | `cargo pgrx start pg17` hasn't been run, or `PGRX_HOST` does not match the pgrx data dir |
| `extension "pg_synapse_pgrx" is not available` | Extension was never installed into the pgrx pg17 tree; run `cargo pgrx run pg17` once |
| `status: errored` in the `execute()` envelope | LLM endpoint unreachable, or model refused to tool-call; inspect `synapse.messages` |
| Agent talks but does not insert | Model lacks tool-call support; see the v0.1 caveat above |
| `positional params not yet supported` in a tool message | Prompt drift; reinforce the v0.1 "inline literals" rule in the system prompt |
