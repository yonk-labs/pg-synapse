# Demo: a customer support triage agent

## What this shows

A multi-step agent registered via `synapse.agent_create(...)` reads each
ticket in `support.tickets`, joins it against `support.customers` to pull
the customer's tier, classifies the ticket by category and priority, and
writes the result back via `sql_exec`. Enterprise-tier urgent issues
get an `escalated=true` flag.

The agent uses two built-in tools: `sql_query` (SELECT) and `sql_exec`
(INSERT / UPDATE / DELETE). The LLM (an OpenAI-compatible vLLM endpoint
by default) generates the tool calls.

See `EXAMPLE_OUTPUT.md` for a captured run.

## Prerequisites

Same as the `sql-agent-readwrite` demo:

- `cargo pgrx` 0.18+
- A pgrx-managed Postgres 17 instance (`cargo pgrx init --pg17 download`)
- The extension installed into the pgrx pg17 tree (run `cargo pgrx run pg17`
  once and exit, or `cargo pgrx install --pg-config $(...)`)
- An OpenAI-compatible LLM that supports tool calls. Default:
  `http://192.168.1.193:8000/v1` (vLLM serving
  `Intel/Qwen3-Coder-Next-int4-AutoRound`).

## Run it

```bash
cd $(git rev-parse --show-toplevel)
bash examples/customer-support-triage/run.sh
```

`run.sh`:

1. Starts the pgrx-managed Postgres if not already running.
2. Drops + recreates `pg_synapse_triage` for an idempotent run.
3. `CREATE EXTENSION pg_synapse_pgrx;`
4. Applies `seed.sql` (3 customers across `free` / `pro` / `enterprise`,
   3 tickets, none yet classified).
5. Applies `workflow.sql` (registers the LLM profile + the `triage_agent`,
   then calls `synapse.execute('triage_agent', 'Triage ticket N.')`
   three times).
6. Prints the final state of `support.tickets` and the execution log.
7. Exits 0 if every ticket has a non-null `category` and `priority`.

### Configuration env vars

| Var | Default | Purpose |
| --- | --- | --- |
| `PG_SYNAPSE_LLM_BASE_URL` | `http://192.168.1.193:8000/v1` | OpenAI-compatible endpoint |
| `PG_SYNAPSE_LLM_MODEL` | `Intel/Qwen3-Coder-Next-int4-AutoRound` | Model id |
| `PGRX_PG_VERSION` | `17` | pgrx-managed pg version |
| `PGRX_PORT` | `28817` | TCP port |
| `PGRX_HOST` | `${HOME}/.pgrx` | Unix-socket directory |
| `DEMO_DB` | `pg_synapse_triage` | Database name to drop + recreate |

## What the agent does step-by-step

For each ticket the operator names:

1. Calls `sql_query`:
   `SELECT t.id, t.subject, t.body, c.email, c.tier FROM support.tickets t JOIN support.customers c ON c.id=t.customer_id WHERE t.id = <ID>`
2. Picks a category in `{api, billing, account, howto, other}` and a
   priority in `{low, normal, high, urgent}`.
3. Decides whether to escalate (only enterprise tier + urgent priority).
4. Calls `sql_exec`:
   `UPDATE support.tickets SET category='...', priority='...', escalated=... WHERE id=<ID>`
5. Replies with a one-line summary.

## v0.1-alpha caveats

Same as the sibling example: positional params are not yet supported in
the pgrx host, so the prompt instructs the agent to inline literal values
into the SQL string. If the model lacks tool-call support, the workflow
will fail the final assertion and the diagnostic lives in
`synapse.messages`.
