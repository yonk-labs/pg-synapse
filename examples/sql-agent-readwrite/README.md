# Demo: a SQL agent that reads and writes a Postgres table

## What this shows

An agent registered via `synapse.agent_create(...)` is asked, from SQL, to
manage a small `demo.notes` table. It uses the built-in `sql_query` and
`sql_exec` tools to read and write rows. The LLM (an OpenAI-compatible vLLM
endpoint by default) generates the tool calls; the pgrx-backed kernel
dispatches them through SPI; the rows show up in `demo.notes`.

## Prerequisites

- `cargo pgrx` 0.18+ installed (`cargo install cargo-pgrx --version 0.18.0 --locked`)
- A pgrx-managed Postgres 17 instance (`cargo pgrx init --pg17 download` if missing)
- An OpenAI-compatible LLM endpoint reachable from this machine.
  Default: `http://192.168.1.193:8000/v1` (vLLM). Edit `workflow.sql` to
  change the URL or model.

## Path A: cargo-pgrx (recommended for v0.1-alpha)

```bash
cd $(git rev-parse --show-toplevel)
cargo pgrx run pg17
```

That opens a psql session inside the pgrx-managed cluster. From there:

```sql
CREATE EXTENSION pg_synapse_pgrx;
\i examples/sql-agent-readwrite/seed.sql
\i examples/sql-agent-readwrite/workflow.sql
```

You should see:

1. Three `synapse.*` SELECTs that return `void` (agent + profile + execute).
2. A final `SELECT * FROM demo.notes` showing the agent's added row.

## Path B: docker (work in progress)

```bash
bash examples/sql-agent-readwrite/run.sh --docker
```

This currently spins up a Postgres 17 container on a free port but does NOT
install the compiled extension into it; building the .so against a container
runtime is deferred to a future iteration. The script prints a TODO when this
happens. For now, prefer Path A.

## v0.1-alpha caveats

- The built-in `sql_query` / `sql_exec` tools do not yet support positional
  bind parameters. The agent prompt above instructs the LLM to inline literal
  values into the SQL string. Full bind support arrives in M7-phase-B.
- `synapse.execute()` returns a JSONB envelope on every call. On agent error
  the envelope is `{"error": "...", "status": "errored"}`, not a Postgres
  ERROR, so you can `... ->> 'status'` and route accordingly.
- The `synapse` schema is used instead of `pg_synapse` because Postgres
  reserves the `pg_` prefix. Same reasoning for the `synapse_admin` and
  `synapse_user` role names. GUC names keep the `pg_synapse.*` prefix.
