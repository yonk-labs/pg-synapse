# Work prompt — pg-synapse next backlog

Paste this to an agent working **inside the `pg-synapse` repo**. Ship `docs/next-backlog.md` (the companion file) alongside it.

---

You are working in the `pg-synapse` repo (workspace v0.1.1). Your backlog is `docs/next-backlog.md` — items PS-1 through PS-8, already verified against the live source with file:line evidence. Read it fully before touching code.

## Non-negotiable constraints (these override the backlog if they ever conflict)

1. **G4 — zero pre-built tower layers in the kernel.** Reliability (retry, recovery) ships as *tested, supported, opt-in* composable types behind a feature flag or as an explicit wrapper. Never a silent default in `pg-synapse-core`'s default path. If a task seems to require baking a layer into core, stop and flag it.
2. **G8 — compression/memory are plugin-only in v0.1.** `DefaultCompressor` is a new `plugins/pg-synapse-compaction` crate, not core.
3. **D6 — trace schema is decided.** PS-3 implements the *already-specified* `tracing`→`synapse.traces` adapter; don't redesign the schema, fill the writer.
4. **D8 — LISTEN/NOTIFY + live push are v0.2.** PS-3 persists events and makes them pollable; do not build SSE/WebSocket push.
5. **N2.2 — the `synapse.*` SQL surface is frozen for v0.1.1.** Do **not** add `synapse.provider_verify`, `synapse.plugin_*`, or `synapse.execution_export`. Build the kernel capability and open a tracked v0.2 issue for the SQL surface.
6. Respect locked decisions D1–D13 and the N1–N6 v0.1.1 plan. If your change touches anything they cover, cite the decision and confirm before proceeding.

## How to work

- **One item per branch/PR.** No mixing. Surgical changes only — every changed line traces to the item. If you spot an unrelated problem, log it under "NOTICED BUT NOT TOUCHING", don't fix it.
- **Rule of 100.** Split anything over ~100 LOC. PS-2 is explicitly three slices (2a retry+headers, 2b compaction plugin, 2c recovery wrapper) — ship them separately; the tree must compile and pass tests between slices.
- **Prove it works — TDD.** Each item's "Success criteria" are testable. Write the failing test first, then make it pass. "Runs" is not "verified"; paste the test output. Existing wiremock/live tests must stay green (PS-6 explicitly: OpenAI client behavior unchanged).
- **Verify framework APIs against docs** before writing provider/HTTP/pgrx code (pgrx, tower, axum, schemars versions are pinned in `Cargo.toml`/`workspace.package`). Cite non-obvious API choices inline.
- **Per-item summary** on completion: CHANGES MADE / THINGS I DIDN'T TOUCH (intentionally) / POTENTIAL CONCERNS, plus the success-criteria evidence (test names + output).

## Order

1. **PS-8 first** — the ADR + compatibility matrix. ~1 day, no code, prevents scope drift for everything after. Get it reviewed before Wave 1.
2. **PS-1** and **PS-2a** in parallel — no dependencies, highest unblock.
3. **PS-3** and **PS-2b/2c** next.
4. **PS-4** (needs PS-3), **PS-5** (needs PS-1).
5. **PS-6**, **PS-7** — independent, slot in anywhere.

Do not start an item whose "Depends on" is unmet. Do not pull anything from the "Not Yet" list — if you think a parked item is now necessary, raise it, don't silently do it.

## Definition of done (per item)

- All "Success criteria" bullets have a passing test or a produced artifact, with evidence pasted.
- No regression in existing tests (`cargo test --workspace`, plus `live-tests` where the item touches a provider).
- `cargo fmt` + `cargo clippy --workspace` clean.
- Constraint check stated explicitly: which of G4/G8/D6/D8/N2.2 the item touched and how it stayed compliant.
- Summary block delivered.

Start with PS-8. Confirm the ADR wording with the maintainer before writing Wave 1 code.
