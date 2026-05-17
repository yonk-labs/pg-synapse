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
| `pg-synapse-tools-cli` (`run_cli`, GOVERNED) | Operator note 2026-05-17: let an agent call CLI tools, but ONLY commands approved in a DB table. Default-deny: a `synapse.cli_allowlist` table (binary/arg pattern, working-dir + role scope, enabled, max_runtime, approved_by/at); the tool refuses anything not matching an enabled row and audits every invocation (caller_role, argv, exit) to traces or a dedicated audit table. Approval is an admin-only SECURITY DEFINER fn (`synapse.cli_approve(...)`, same role/grant model as other admin fns); pairs with the inline-veto trigger so a gate agent can deny too. This is the constrained, governed form of the D14 "CodeAct excluded until a sandbox+rollback design exists" item: the DB allowlist + audit + admin approval IS that guardrail design. Highest-risk tool: default-deny, allowlist-only, fully audited, admin-approved. | M | v0.2, governance-gated |

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
| Auto-fix memory / correction hints | When the executor feeds a tool error back and the model self-corrects, capture the error-to-fix pattern (e.g., "column specified twice" -> fixed column list) in a per-agent correction memory (`synapse.correction_hints` or similar). Inject accumulated hints into the system prompt or as few-shot examples on subsequent runs so the model avoids repeating known mistakes. Think: agent-scoped "lessons learned" that compound over runs, not just within one run. | M | v0.2, depends on #3 trace/governance |

## Benchmark / scenario follow-ups

| Item | Why | Status |
|---|---|---|
| Diagnose a1_ingest / a3_triage 0-pass-on-strong-models | DIAGNOSED (B15): root cause was a hardcoded max_iterations=10 confound. Fixed (scenario MAX_ITER, default 25). a1_ingest now PASSes strong models. a3_triage still fails -> prompt tightening + query-alias gap = task B16. | done (a1); B16 (a3) |
| Build external parity scenarios `lg_calc`, `oai_triage`, `adk_root`, `adk_orchestrator` | "3 from external" test set. Needs delegation+calc+clock tools. | keystone wave |
| Full correctness + scale matrix | All built agents x all WORKS/PARTIAL serving models, SCALE=1 and a scaled run; skip the known-NO models. | after keystone + diagnosis |
| Scale dimension in RESULTS.md | Record SCALE per row; chart pass-rate + latency vs scale per model. | with the matrix |
| `MODEL-COMPATIBILITY.md` refresh | Fold agent-scenario results in once a1/a3 are trustworthy. | after diagnosis |

## Known flaky tests (fix opportunistically)

| Test | Symptom | Note |
|---|---|---|
| `pg-synapse-tools-delegate::tests::depth_decremented_on_sub_agent_failure` | ~1/5 fail, identical code between runs (`assert left==right` at lib.rs:340); passes 100% in isolation, flakes under concurrent full-suite runs | Concurrency/ordering issue in the test's delegation-depth accounting (shared atomic/global or async timing). Independent of Wave 2; surfaced 2026-05-17 when the B19 build repair (f5f812a) un-blocked the full `cargo test --workspace`. Stabilize the test (isolate the depth counter / make the assertion order-independent), do not paper over with retries. |

## Known NOs (skip in matrices, documented in MODEL-COMPATIBILITY.md)

smollm3-3b, qwen3.5-0.8b, qwen3.5-2b, llama-3.2-3b, granite-4.0-h-1b,
granite-4.0-tiny-preview, granite-4.1-3b: serve on CPU but do not emit
parseable tool calls at their scale/stack. Excluded from agent matrices;
re-test only if a serving-stack change (e.g. llama.cpp native tool
templates) lands.
