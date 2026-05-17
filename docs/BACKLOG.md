# pg_synapse backlog: new plugins + loops + follow-ups

Durable backlog for work deliberately deferred. Not a TODO churn list;
each item has a why and a rough size. Ordered by leverage.

## New tool/provider plugins

| Plugin | Why | Size | Status |
|---|---|---|---|
| `pg-synapse-tools-delegate` (`call_agent`) | Unlocks multi-agent: OpenAI handoffs + ADK orchestrator parity. Highest leverage single primitive. `ToolCtx.delegation_depth` guard. | S | building (keystone wave) |
| `pg-synapse-tools-calc` (`calculator`) | LangGraph quickstart parity (add/multiply/divide). Trivial, deterministic. | XS | building (keystone wave) |
| `pg-synapse-tools-clock` (`get_current_time`) | ADK get-started parity. Trivial. | XS | building (keystone wave) |
| `pg-synapse-tools-email` (`send_email`) | Support autopilot; outbox-backed test sender + pluggable SMTP. A4. | S | queued (A4) |
| `pg-synapse-a2a` (client + server) | Google A2A parity; cross-service agent-to-agent. Maps onto gateway-level intercept/governance. Real protocol work. | L | v0.2 design direction |
| `pg-synapse-provider-candle` | In-process Rust inference, zero external server (attractive for the pgrx extension). Operator flagged candle has idiosyncrasies, so cautious bet. | L | v0.2, cautious |
| `pg-synapse-tools-http` hardening | Real HTTP tool exists; add auth/retry/timeout knobs for production agents. | S | v0.1.x |

## Loop / executor variants

| Item | Why | Size | Status |
|---|---|---|---|
| Resumable `CheckpointExecutor` + `synapse.checkpoints` table | LangGraph `thread_id` mid-run pause/resume parity. The downstream-product-relevant gap: persist loop state, resume on a new turn. Column sketch in `framework-parity/langgraph.md`. | M | v0.2, specified |
| `plan-and-solve` executor | Decompose-then-execute loop; better for the harder multi-step scenarios (a1_ingest, a3_triage) where react/conversation under-plan. | M | v0.2 |
| `reflection` executor tuning | Exists; tune the critique loop + add a max-revisions GUC. | S | v0.1.x |
| Per-agent loop budget GUCs | `max_iterations`/`cost_cap`/`timeout` are agent columns; add GUC defaults + a hard wall-clock kill. | S | v0.1.x |
| Delegation recursion + cycle detection | Once `call_agent` ships: detect A->B->A cycles, not just depth. | S | follows delegation |
| Reactive triggers (queue + inline modes) | Designed in `docs/reactive-triggers.md`. Dual-mode: queue (async, write commits, agent cannot rollback) vs inline (sync in-txn, agent reject -> RAISE -> triggering write rolls back). `synapse.agent_queue` + `enqueue`/`drain_queue`/`attach_agent_trigger`. | M | spec'd, queued (task T1, after keystone) |
| Self-draining queue worker | v0.2 upgrade of the above: Postgres bgworker + `LISTEN/NOTIFY` so the queue drains with no external scheduler (v0.1 drain is operator-driven via pg_cron). Depends on real background `execute_async` (D8). | M | v0.2 |

## Benchmark / scenario follow-ups

| Item | Why | Status |
|---|---|---|
| Diagnose a1_ingest / a3_triage 0-pass-on-strong-models | DIAGNOSED (B15): root cause was a hardcoded max_iterations=10 confound. Fixed (scenario MAX_ITER, default 25). a1_ingest now PASSes strong models. a3_triage still fails -> prompt tightening + query-alias gap = task B16. | done (a1); B16 (a3) |
| Build external parity scenarios `lg_calc`, `oai_triage`, `adk_root`, `adk_orchestrator` | "3 from external" test set. Needs delegation+calc+clock tools. | keystone wave |
| Full correctness + scale matrix | All built agents x all WORKS/PARTIAL serving models, SCALE=1 and a scaled run; skip the known-NO models. | after keystone + diagnosis |
| Scale dimension in RESULTS.md | Record SCALE per row; chart pass-rate + latency vs scale per model. | with the matrix |
| `MODEL-COMPATIBILITY.md` refresh | Fold agent-scenario results in once a1/a3 are trustworthy. | after diagnosis |

## Known NOs (skip in matrices, documented in MODEL-COMPATIBILITY.md)

smollm3-3b, qwen3.5-0.8b, qwen3.5-2b, llama-3.2-3b, granite-4.0-h-1b,
granite-4.0-tiny-preview, granite-4.1-3b: serve on CPU but do not emit
parseable tool calls at their scale/stack. Excluded from agent matrices;
re-test only if a serving-stack change (e.g. llama.cpp native tool
templates) lands.
