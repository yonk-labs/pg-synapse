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

### Wave 0 - finish what is in flight (cheap, data already exists)

1. **M-CLOSE-1: Refresh MODEL-COMPATIBILITY.md + finish B8 min-specs**
   from the just-completed combined matrix (6 agents x 7 WORKS-tier
   models) and the measured latency/RAM in results.jsonl. No new code;
   pure write-up. Closes the open B8 task and the original backlog
   "MODEL-COMPATIBILITY refresh" + "scale dimension" items.

### Wave 1 - foundational, unblocked by PS-8 (parallel-safe)

2. **PS-1: Provider capability model + pre-flight verification.** No
   `ProviderCapabilities` today. Highest unblock; PS-5 depends on it.
3. **PS-2a: Reliability - jittered retry + Retry-After headers.**
   Opt-in/feature-gated wrapper, never a silent core default (G4).
   `retry_after_ms` is currently always None; fix the plumbing.

### Wave 2 - build on Wave 1

4. **PS-3: Canonical `ExecutionEvent` + wire the dead `synapse.traces`
   table.** Fills the already-decided D6 writer (do not redesign the
   schema). Persist + pollable only; no push (D8). PS-4 depends on it.
5. **PS-2b: Compaction plugin (`plugins/pg-synapse-compaction`).** The
   `DefaultCompressor` as a plugin crate, not core (G8).
6. **PS-2c: Recovery wrapper (overflow -> compact -> retry).** Opt-in
   composable wrapper (G4). Uses PS-2b.

### Wave 3 - build on Wave 2

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
catalog / workflow engine / CodeAct (downstream-product product modules per D14),
more provider plugins (not value until PS-1/PS-5 make it safe).

## Recommended next action

Wave 0 item 1 (close MODEL-COMPATIBILITY + B8 from the matrix data),
then Wave 1: PS-1 and PS-2a in parallel branches.
