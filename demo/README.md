# pg_synapse containerized demo

A self-contained playground for showing pg_synapse to a room: Postgres 17
with the extension baked in, a thin axum harness, and a single-page web UI
that drives the whole `synapse.*` SQL surface. Built for a 30-minute live
talk; also useful for kicking the tires locally.

## What you need

- Docker with Compose.
- An OpenAI-compatible LLM endpoint reachable FROM YOUR NETWORK that supports
  tool calls (vLLM, llama.cpp server, LM Studio, or real OpenAI). The LLM is
  deliberately not part of the containers; you point the UI at it at runtime.

## Run it

From the repo root:

```bash
docker compose up --build
```

The first build is slow (it compiles the Rust extension inside the db image,
expect 10 to 25 minutes cold); later builds hit the Docker cache. Then:

1. Open <http://localhost:8080>.
2. Panel 1 (LLM endpoint): the form is prefilled with the repo's test
   endpoint; change it to your server, hit **Test** (probes `GET /models`
   from the harness), then **Save**. This writes `synapse.llm_profiles` via
   `synapse.llm_profile_set()`, and the API key (if any) via
   `synapse.secret_set()`.
3. Panel 2 (Load a demo): pick a scenario, click **Load**, then click one of
   the suggested-run buttons.

Postgres is also exposed on `localhost:55432` (user `postgres`, password
`postgres`, database `synapse_demo`) if you want a psql side channel during
the talk:

```bash
psql "host=localhost port=55432 user=postgres password=postgres dbname=synapse_demo"
```

## The four things the UI shows

1. **Configure the LLM endpoint.** base_url + model + optional API key, with
   a connectivity test. Everything persists in Postgres tables, not in the
   harness.
2. **Natural-language instructions.** The agent editor edits the system
   prompt, tool allow-list, executor, and the safety caps (max_iterations,
   timeout_ms, cost_cap_usd). Saved with `synapse.agent_create()`; the cost
   cap is set on the `synapse.agents` row (the create function does not take
   it as a parameter).
3. **Run an agent loop.** `SELECT synapse.execute(agent, input)` on a
   dedicated connection. The UI polls the run, shows elapsed time and the
   backend PID while the loop runs, then renders the outcome envelope
   (status, tokens, cost, duration, tool calls) and the full message trace
   from `synapse.messages` / `synapse.traces`. Note: the runtime persists
   the trace when the run finishes, not mid-run, so the trace lands at
   completion and the UI animates it in.
4. **Event triggers.** Attach `synapse.attach_agent_trigger()` to the demo
   tables, INSERT rows from the UI, and watch:
   - queue mode: the INSERT commits instantly, a job appears in
     `synapse.agent_queue`, and **Drain queue now** runs the agent
     (`synapse.drain_queue`) to enrich the row;
   - inline mode: the agent gates the INSERT inside the transaction; a
     rejected row ROLLS BACK and the UI surfaces the agent's reason from the
     Postgres error.

## Baked scenarios

The first three are the priority scenarios for a DBA audience: the agent
takes action ON the database itself. Each is assert-able; the Scenario watch
panel has a "Check expected end state" probe that prints PASS/FAIL lines.

| Menu item | Source | What it shows | Expected end state |
| --- | --- | --- | --- |
| Autonomous index tuner | new | 100k-row `perf.orders`, no index on `customer_id`. Agent runs EXPLAIN (via a seeded `perf.explain_query()` helper), creates the missing index with plain `CREATE INDEX` (transaction-safe), re-EXPLAINs. Use the EXPLAIN probe before and after: Seq Scan flips to a Bitmap/Index Scan. | Index exists; planner uses it. |
| The DBA that opens tickets | new | Four health signals. One is transaction-safe to auto-fix (missing index on `dba.audit_log(actor_id)`); three are not (`work_mem`, REINDEX CONCURRENTLY, `shared_buffers`) and become `dba.recommendations` tickets with rationale. The agent knows the difference. | All signals resolved; 3+ tickets; index created. |
| LLM-powered ETL | new | Messy free-text notes in `etl.raw_contacts` (Deutschland / SPAIN / U.K. / "the states"). Agent extracts name/company/email, normalizes country to ISO codes, classifies intent, inserts into `etl.contacts`. | Every raw row has a clean row. |
| Ticket triage (warm-up) | `examples/customer-support-triage` | Agent reads tickets with `sql_query`, writes category/priority/escalated back with `sql_exec`. | Every ticket categorized. |
| Transaction bouncer | `examples/reactive-triggers` | Both trigger modes; the inline `policy_agent` rejects a negative-amount order and the transaction rolls back with its reason. | No non-positive order committed. |
| Guardrails | new | `cost_capped_agent` trips `cost_cap_usd` (synthetic pricing on a derived profile), `time_capped_agent` trips `timeout_ms`, and `marathon_agent` runs long enough to kill with the **Cancel** button (`pg_cancel_backend`). | Run panel shows `cost cap exceeded`, `timed_out`, `cancelled`. |

The transaction constraint shapes the split deliberately: the agent runs
inside a transaction via SPI, so plain `CREATE INDEX`, DML, and `ANALYZE` are
auto-appliable, while `CREATE INDEX CONCURRENTLY`, `VACUUM`, `ALTER SYSTEM`,
and memory changes are not and must become human tickets.

Scenario SQL lives in `demo/harness/scenarios/*.sql` and is applied verbatim
with one click (reload-safe; the DBA scenarios drop and reseed their tables
so a demo can be reset live). Scenarios assume the `vllm-default` profile
exists (step 2 above), so configure the endpoint first.

## Architecture

```
browser (static single page, vanilla JS)
   |  JSON over HTTP, short-polling
axum harness (demo/harness, workspace member; no dependency on the kernel)
   |  tokio-postgres, one connection per request,
   |  one dedicated connection per agent run (cancellable by backend PID)
Postgres 17 + pg_synapse extension (demo/db/Dockerfile)
   |  synapse.execute / agent_create / llm_profile_set / secret_set /
   |  attach_agent_trigger / drain_queue / executions / messages / traces
external OpenAI-compatible LLM endpoint (yours)
```

The db image is a two-stage build: a `rust:1-bookworm` stage installs
`cargo-pgrx 0.18.0` (matching the pinned `pgrx = "=0.18.0"`), builds against
PGDG `postgresql-server-dev-17`, and runs `cargo pgrx package` with
`--no-default-features` plus only the demo features, which drops `embed-ort`
so no ONNX Runtime is needed in the image. The packaged tree (`.so`,
`.control`, generated SQL) is copied into `postgres:17`, and an initdb script
runs `CREATE EXTENSION pg_synapse_pgrx` on first boot.

## Suggested talk flow

1. `docker compose up` before the talk; open the UI.
2. Test + save the LLM endpoint (30 seconds, proves it is live).
3. Warm up with **Ticket triage**: run "Triage ticket 1", walk the trace,
   show the updated row.
4. **Autonomous index tuner** (the money shot): hit the EXPLAIN probe (Seq
   Scan over 100k rows), run the agent, hit EXPLAIN again (Index Scan), then
   "Check expected end state" prints PASS PASS.
5. **The DBA that opens tickets**: the agent auto-creates the safe index and
   files tickets for the fixes that need a human.
6. **LLM-powered ETL**: messy notes become clean ISO-coded rows.
7. **Transaction bouncer**: INSERT a good order (commits), then the bad
   order button: the transaction rolls back and the agent's reason is on
   screen. This is the showstopper.
8. **Guardrails**: trip the cost cap, trip the timeout, then start the
   marathon agent and hit Cancel.
9. Close on the psql side channel: everything the UI did is plain SQL and
   plain tables (`synapse.executions`, `synapse.messages`).

## Notes and limits

- Single-user local tool: no auth, no TLS, no persistence beyond the
  Postgres volume. Do not expose it to a network you do not trust.
- The tool list in the agent editor is the set compiled into the demo image
  (`sql_query`, `sql_exec`, `http_get`, `http_post`, `http_head`,
  `calculator`, `get_current_time`, `call_agent`).
- `synapse.embed` is present but non-functional in this image by design
  (the `embed-ort` feature is dropped to keep ONNX Runtime out).
