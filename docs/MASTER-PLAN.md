# pg_synapse master plan (consolidated)

One ordered list merging `BACKLOG.md` (original) and `next-backlog.md`
(PS-1..PS-8), de-duped. Plan only. Generated 2026-05-17.

## Carried constraints (from next-backlog-prompt, binding)

- One item per branch/PR. Rule of 100 (split >100 LOC; PS-2 ships as
  2a/2b/2c). TDD: failing test first, evidence pasted.
- G4 (no prebuilt tower layers in core), G8 (compaction/memory are
  plugin crates), D6 (fill the traces writer, do not redesign), D8
  (persist/poll only, no live push), N2.2 (synapse.* SQL frozen v0.1.1
  EXCEPT the operator-approved reactive-trigger surface, 2026-05-17).
- Respect D1-D14, N1-N6. Never pull from next-backlog "Not Yet"; raise
  instead.

## Merges / de-dupes (these were the same item in both lists)

- next-backlog PS-2b compaction plugin == original "DefaultCompressor /
  pg-synapse-compaction (G8)". Single item: PS-2b.
- PS-3 (ExecutionEvent + wire synapse.traces) == original "tracing ->
  synapse.traces writer (D6)". Single item: PS-3.
- PS-7 (plugin manifest + capability metadata) == original "tool/plugin
  introspection" and the tool-extensibility formalization. Single
  item: PS-7.
- Already DONE this cycle, struck from the plan: PS-8 (ADR D14 +
  matrix), reactive triggers (T1), the 3 external parity agents +
  keystone tools (calc/clock/call_agent), email tool (A4),
  a1/a3 diagnosis (B15/B16/B17), toolchain isolation (scripts/pgrx).

## The ordered master list

### Wave 0 - DONE

M-CLOSE-1 (MODEL-COMPATIBILITY.md refresh + B8 min-specs), B18
(tool-error feedback loop, 40%->0%), B19 (trace/governance kernel +
SQL surface), P1/P2 (harness integrity), parity fidelity (zero-SQL
lg_calc/oai_triage/adk_root), p1_pipeline (6-hop multi-tool
zero-SQL scenario), http_get plugin registration. All committed.

### Wave 1 - DONE

PS-1 (ProviderCapabilities + pre-flight check, 7d19a49), PS-2a
(RetryProvider jittered backoff, opt-in per G4, 81e363e). 156 core
tests green. Minor follow-up: parse Retry-After header in OpenAI
provider (retry layer works, provider sends None currently).

### Wave 2 - DONE

4. **PS-3** (ExecutionEvent recorded in LoopHarness + carried via
   ExecutorOutcome.events + persisted to synapse.traces gated by
   trace_level>=debug, 40f01c9). D6/D8 honored; PS-4 unblocked.
5. **PS-2b** (`plugins/pg-synapse-compaction` DefaultCompressor,
   deterministic LLM-free, G8/D14, 2b7213c).
6. **PS-2c** (`RecoveryProvider` overflow->compact->retry, opt-in G4,
   depends only on the Compressor trait, d50d05e).

Minor follow-up CLOSED: OpenAI Retry-After parsing was already shipped
in PS-1 (7d19a49); added regression coverage (60a1bdc). Workspace
build repaired: a pre-existing B19 trace_level drift left sidecar +
doctests + several test bins non-compiling on main (f5f812a).

### Wave 3 - build on Wave 2 (NEXT)

7. **PS-4: Redacted diagnostics export bundle.** Needs PS-3. No
   `synapse.execution_export` SQL (N2.2): build the kernel capability,
   file the v0.2 SQL issue.
8. **PS-5: Cassette / record-replay provider conformance suite.** Needs
   PS-1. Dev/test tooling, not runtime core.

### Wave 4 - independent, slot in anywhere

9. **PS-6: Structured-output contract + generalized schema sanitizer.**
   Generalize the OpenAI-only inline sanitizer; OpenAI client behavior
   must stay unchanged (existing wiremock/live tests green).
10. **PS-7: Plugin manifest + capability/version metadata.** The
    tool/plugin introspection formalization. Trait metadata is
    substrate; the `synapse.plugin_*` introspection SQL is v0.2 (N2.2).

### Small wins - v0.1.x, fit between waves opportunistically

11. **Per-agent loop-budget GUCs** (default max_iterations/cost_cap/
    timeout + hard wall-clock kill). S.
12. **Reflection-executor tuning** (critique loop + max-revisions GUC). S.
13. **`pg-synapse-tools-http` hardening** (auth/retry/timeout). S.

### v0.2 - deliberately deferred (rough value order)

14. **Self-draining queue worker** (Postgres bgworker + LISTEN/NOTIFY):
    completes reactive triggers so the queue drains with no external
    scheduler. Depends on real background execute_async (D8).
15. **Resumable `CheckpointExecutor` + `synapse.checkpoints`**:
    LangGraph thread_id mid-run pause/resume parity.
16. **`pg-synapse-a2a`** (client + server): cross-service agent-to-agent;
    maps onto the gateway-intercept/governance angle. Large.
17. **`pg-synapse-tools-cli`** (`run_cli`, GOVERNED): DB-allowlist +
    admin approval + audit; the constrained CodeAct guardrail per D14.
18. **Delegation cycle detection** (A->B->A, beyond depth). Follows
    call_agent (shipped).
19. **`plan-and-solve` executor**: decompose-then-execute loop for the
    harder multi-step scenarios where conversation/react under-plan.
20. **`pg-synapse-provider-candle`**: in-process Rust inference (no
    external model server). Cautious - operator flagged candle
    idiosyncrasies.

### Parked (next-backlog "Not Yet" - do not start, raise instead)

`synapse.plugin_*`/`provider_verify`/`execution_export` SQL (N2.2 until
v0.2; tracked inside PS-1/4/7), real-time push (D8), OpenAPI gen + CI
freshness, plugin-owned migrations, memory plugin / NL2SQL / semantic
catalog / workflow engine / CodeAct (downstream product modules per D14),
more provider plugins (not value until PS-1/PS-5 make it safe).

## Recommended next action

Wave 3: PS-4 (redacted diagnostics export bundle) now that PS-3 lands
the trace writer + ExecutorOutcome.events; then PS-5 (cassette /
record-replay conformance, needs PS-1). Opportunistic: fix the flaky
`pg-synapse-tools-delegate::depth_decremented_on_sub_agent_failure`
(see BACKLOG) -- nondeterministic, ~1/5 fail, independent of Wave 2.
