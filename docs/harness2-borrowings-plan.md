# Design: harness2 borrowings (branch `harness2-borrowings`)

Date: 2026-10-19. Companion to `docs/harness2-comparison.md`, which argues
the case. This doc is the build plan: what changes, in what order, and how
each piece is verified. Items are marked as they land.

## Goal

Make a pg_synapse agent's harness a structured, cost-tiered, evolvable
artifact instead of one always-on string, and give the bench a
Propose-Probe-Compose tuner that improves it from the agent's own
execution history. Everything stays inside the existing three tiers (SQL
surface, host, kernel) and respects the CLAUDE.md non-goals: durable agent
memory and lessons are data the host loads, not kernel logic.

## Constraints

- Kernel stays small. One new value type (`Harness`) on `ExecutionContext`
  and one new built-in tool (`load_skill`). No new traits.
- `synapse.*` SQL additions are v0.2 shaped (N2.2 froze v0.1.1). New
  functions are additive; existing signatures do not change.
- Typed errors only, no `unsafe`, no em-dashes, surgical diffs.
- Quality gates: `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace --exclude
  pg_synapse_pgrx`. The pgrx host is verified by building the Plexus image
  and running SQL against it (no local managed pgrx Postgres on this box).

## Todo

### Bench honesty (prerequisites for tuning)

- [x] B1 `iterations` is 0 in all 289 result rows. The `execute()` envelope
      never carried it. Add `iterations` (count of assistant turns) to the
      envelope; the bench already reads that key.
- [x] B2 Fingerprint results by harness. Add `harness_sha` (sha1 over
      `system_prompt.txt`, `task.txt`, `meta.env`) to each row. Resume
      skips only when the sha matches. RESULTS.md regeneration uses the
      newest row per (model, scenario, scale).
- [x] B3 `bench/lint_scenarios.py`: flag system prompts that restate their
      own `assert.sql` conditions or contain grader words. Report only.

### Kernel and host

- [x] B4 `synapse.trajectory(execution_id) RETURNS TEXT`: renders
      `synapse.messages` plus `synapse.traces` in the
      `[ACTION] / [OBSERVATION] / [ERROR]` grammar. Pure SQL function in
      `schema.sql`, so it works on the sidecar path too.
- [x] B5 Tool-error hints. `synapse.tool_hints(agent, tool_name,
      error_pattern, hint, enabled)`; `Harness.tool_hints`; matched in
      `LoopHarness::push_tool_error`, appended to the tool message. Seeded
      with the four signatures the bench already shows.
- [x] B6 Cost-tiered components. `synapse.agents` gains `guardrails` and
      `memory` (always-on). `synapse.skills(agent, name, description,
      body)` is on-demand: one trigger line per skill is injected always-on,
      the body loads through a kernel-provided `load_skill` tool.
      `seed_messages` composes prefix + system_prompt + soul + guardrails +
      memory + skill triggers. Always-on total capped at 24,000 chars,
      enforced at write time by `synapse.agent_set_component` and
      `synapse.skill_set`.
- [x] B7 Reflection critique rubric: replace the one-line critique with the
      ordered checks from harness2's selector (delivery, conformance,
      fidelity to source, internal consistency, operational fitness,
      finish). Constant in `reflection.rs`, overridable per agent via the
      `critique` component.
- [x] B8 Lessons. `synapse.lessons(agent, cue, lesson, evidence,
      confidence, source_execution, status)` plus `synapse.lesson_add`.
      The pgrx host folds active lessons into the agent's memory component
      at load; the kernel never knows they exist.

### Tuner

- [x] B9 `bench/tune.py`: Propose-Probe-Compose over one scenario.
      k base rollouts, INV and DOM proposals from an improver (Claude CLI
      by default, any OpenAI-compatible endpoint otherwise), one probe
      each, one composed final, selection by `assert.sql` with token count
      as tie-break, then a distilled lesson written through
      `synapse.lesson_add`. Evidence is served as files; grader text is
      stripped from what the improver sees; harness2's evidence-boundary
      and KEEP_BASE rules are carried over verbatim.

### Docs and defaults

- [x] B10 Evidence-boundary guardrail text as a shared default in
      `docs/AGENTS-GUIDE.md` and in the Plexus seeds.
- [x] B11 CHANGELOG, `docs/reference.md` (new tables, functions), update
      `docs/harness2-comparison.md` status column.
- [x] B12 Verification: fmt, clippy, tests green; Plexus image builds;
      live SQL smoke against the image (`trajectory`, `load_skill`, hint
      injection).

## Implementation notes

(Filled in as items land.)

### B1 iterations

`ExecutorOutcome` has no iteration counter. The envelope now derives it
as the number of assistant-role messages, which equals LLM turns for all
three executors. Added at `sql_functions.rs::execute` only; the kernel is
untouched.

### B2 fingerprint

`already_done` compares `(model, scenario, scale, harness_sha)`. Rows
written before this change have no sha and never match, so the first run
after this lands re-executes everything once. That is intended: those
rows were produced under unknown prompts.

### B4 trajectory

Pure SQL, no plpgsql, so the sidecar's seeded schema gets it for free.
Observations longer than `trunc` (default 10,000 chars, the floor
harness2 found safe) are cut with an explicit marker so an improver does
not read the cut as solver behaviour.

### B5 hints

Hints are compiled with the `regex` crate at context build time; an
invalid pattern is a `RuntimeError::Config` naming the row, never a
silent skip. Matching is first-hit in `(agent-specific first, then
global)` order. The hint text is appended after the `ERROR:` line of the
tool message so the model sees both.

### B6 components

`Harness` lives in `types/context.rs` and derives `Default`, so the 16
existing `ExecutionContext` literals gain one line each. `AgentRow`
gains `components: Harness` the same way. `load_skill` is a kernel tool
(`tool/builtin.rs`) constructed per execution with the agent's skill
bodies captured; it is registered into the per-run `ToolRegistry` only
when the agent has skills, so agents without skills see no new tool.

### B8 lessons

Host-side only. `SpiProfileSource::agents` runs one extra query per
agent for `status = 'active'` lessons and appends them to
`components.memory` under a fixed header. The sidecar loads the new
columns but not lessons yet (noted as a gap).

### B9 tuner

Lives in `bench/tune.py`, no new Python dependencies. It talks to
Postgres through `psql` so it works against the Plexus container, the
pgrx managed instance, or anything else with the extension. The improver
is invoked as `claude -p` with a workspace directory; the prompts are
adapted from harness2's `propose`, `aggregate`, and `experience`
functions with benchmark-specific text replaced by a `WORK` paragraph
built from the scenario's `task.txt`. Selection uses the deterministic
assertion directly, which harness2 deliberately refuses to do; see the
comparison doc for why that is the right trade here.

### Live check of B5 and B6 (2026-10-19, Plexus container, qwen3 4B via Ollama)

Agent `t1` with one skill (`counting`) and the global guardrail about
placeholders. First run: the agent called `load_skill` on its own, then hit
`syntax error at or near ";"` from `sql_query` (the SQL tools reject a
trailing semicolon), retried the identical statement three times and gave
up: 5 iterations, 3123 tokens in, wrong answer. One `tool_hint_add` row
keyed on that error substring, same prompt: the hint appeared under the
`ERROR:` line, the agent resent the statement without the semicolon on the
next turn and answered correctly: 4 iterations, 2401 tokens in. This is
the Harness2 claim ("a hook costs nothing until the error happens and
cannot be skipped") reproduced on a 4B model with no prompt change.

### B9 tuner, first live run

`bench/tune.py` was first run against a3_triage at k=1 with Ollama's
default 4k context: the 4B model's context overflowed around iteration 20,
the run hit the 240 s wall clock, and pg_synapse returned a `timed_out`
outcome with no messages, so the improver saw an empty trajectory and
correctly answered KEEP_BASE. Two fixes followed: Ollama now serves with
`OLLAMA_CONTEXT_LENGTH=32768`, and the tuner accepts a scenario path plus
`--from-agent` so it can start from a live agent's composed harness (the
Plexus scenario has no `system_prompt.txt`; its harness lives in the
database). The Plexus k=2 run is recorded in `../plexus/RUN.md`.

## Gaps and next steps

- **A timed-out run loses its trajectory.** `Runtime::execute_inner` wraps
  the executor in `tokio::time::timeout`, which drops the future and its
  message log; the envelope has no `execution_id` and nothing lands in
  `synapse.messages`. For tuning this is the worst case: the runs that most
  need diagnosing are the ones with no evidence. Fix: have the executor
  write messages into a shared buffer the runtime can read after the
  timeout, or persist incrementally at `trace_level = full`.
- **Sidecar parity.** The sidecar loads `guardrails`, `memory` and
  `critique` but not skills, hints or lessons. Same follow-up as
  `trace_level`.
- **Schema upgrade.** `schema.sql` is the bootstrap file; the three new
  columns use `ADD COLUMN IF NOT EXISTS`, but the new tables and functions
  only arrive with `CREATE EXTENSION`. A proper `pg_synapse_pgrx--0.1.1--0.2.0.sql`
  upgrade script is needed before release.
- **Tool hints are substrings, not regexes.** Deliberate (no new kernel
  dependency), but a `kind` column would let a regex variant land later
  without a migration.
- **`bench/tune.py` supports `KIND=sql` scenarios only.** The fs scenarios
  need a sandbox directory per rollout.
- **`lesson_add` keeps the source execution as text when the rollout DB
  was dropped.** The tuner's rollouts live in throwaway databases, so the
  FK cannot point at them; the evidence string carries the id instead.
- **Selection is deterministic by design.** The tuner picks by the
  assertion. That is the right call for a product with a deterministic
  judge, but it means a harness can overfit to one scenario; a held-out
  sibling scenario (different `SCALE`, or the a3_triage data under the
  Plexus prompt) is the check to add.
