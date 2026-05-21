# Next Steps

A current snapshot of `pg_synapse`: where the repo is, what just shipped,
and what is queued. Updated 2026-05-21.

Authoritative `MASTER-PLAN.md` defines the waves and priority order; this
file complements it with concrete state and a punch list a new contributor
or a follow-up session can pick up from.

## Current state

- Workspace builds clean: `cargo fmt`, `cargo clippy --workspace
  --all-targets -- -D warnings`, and `cargo test --workspace --no-fail-fast
  --exclude pg_synapse_pgrx` all green.
- Test count: 395 unit + integration tests, 13 doctests, 10 `#[ignore]`-d
  fixture regenerators across the workspace.
- CI: the `test` job in `.github/workflows/ci.yml` excludes
  `pg_synapse_pgrx` (its tests use `#[pg_test]` and require
  `cargo pgrx test`); `pgrx-tests.yml` runs the pgrx test harness against
  pg15/16/17 on relevant changes.
- Repo visibility: PRIVATE pending the security close-out items below.

## What just shipped (PS-5: provider conformance suite)

Ten slices landed on `auto/whats-next-2026-05-18` between 2026-05-18 and
2026-05-21:

| Commit | Slice |
| --- | --- |
| `26ac4c2` | 1 cassette replay + conformance harness |
| `bd62da0` | 2a `RecordingProvider` + cassette file IO |
| `d8d33e7` | 2b OpenAi wired |
| `a954d00` | 2c LlamaCpp wired |
| `b016270` | 2d Anthropic wired |
| `86fe905` | 3 golden cassettes committed per provider |
| `ac6ca7a` | 4 canonical fixture generator + drift check + regenerator |
| `e43881b` | 5 three-entry canonical cassette (text + tool_call + auth error) |
| `e8bfd0e` | 6 kernel unit tests + module doc refresh |
| `e84006c` | 7 `docs/provider-conformance.md` authoring guide |
| `53e6a79` | 8 non-None Option shapes in canonical cassette |
| `fe38af8` | 9 doctest the canonical helper |
| `15b0ddc` | 10 CI fix (exclude pgrx from `cargo test --workspace`) |

Net: every `LlmProvider` plugin in the workspace ships a hermetic
conformance test against a deterministic golden cassette plus a feature-
gated live record/replay. The CI test job is green for the first time
since the workflow was scaffolded on 2026-05-13.

## MUST items (user decisions, not engineering)

These were flagged in the original `auto/whats-next-2026-05-18` brief as
out of scope for engineering and were not touched this session:

1. **Bundle deletion**: delete the pre-rewrite repo bundle from disk.
2. **GH residual cleanup**: confirm GitHub no longer surfaces the
   pre-rewrite history (the rewrite was the leak fix; verify nothing
   stale lingers on the remote).
3. **Repo re-publication**: flip the repo from PRIVATE to PUBLIC once the
   above two are done. Until then no `git push` should reach a public
   remote; the project's own CI is fine.

## Queued engineering work (by leverage)

### Wave 4 (independent, slot in anywhere)

- **PS-6**: structured-output contract + generalized schema sanitizer.
  Generalize the OpenAI-only inline sanitizer; OpenAI client behavior
  must stay unchanged (existing wiremock/live tests green).
- **PS-7**: plugin manifest + capability/version metadata. The
  tool/plugin introspection formalization. Trait metadata is substrate;
  the `synapse.plugin_*` introspection SQL is v0.2 (N2.2).

### v0.1.x small wins

- Per-agent loop-budget GUCs (default `max_iterations` / `cost_cap` /
  `timeout` + hard wall-clock kill). S.
- Reflection-executor tuning (critique loop + max-revisions GUC). S.
- `pg-synapse-tools-http` hardening (auth / retry / timeout). S.

### Conformance suite follow-ups (low priority)

PS-5 is complete as a contract harness; the items below are
optional polish, not gaps in the contract:

- **Streaming conformance**. Blocked on the D8 decision to defer
  streaming in v0.1. When `LlmProvider::stream` graduates to
  "supported," the harness can grow a streaming-conformance layer
  mirroring the existing `complete` one.
- **`EmbeddingProvider` conformance**. PS-5 covers `LlmProvider` only.
  A parallel harness for the embedding trait would mirror the existing
  one across `pg-synapse-embeddings-ort` and the
  `LlamaCppEmbeddingProvider`.
- **A `regen-all` convenience.** Each provider has its own
  `#[ignore]`-d regenerator test. Today, refreshing all fixtures means
  three commands or one `cargo test --workspace -- --ignored`. A
  workspace alias or a tiny `xtask` would centralize that.

### Known flakies

- `pg-synapse-tools-delegate::depth_decremented_on_sub_agent_failure`
  (per `BACKLOG.md`): nondeterministic, ~1 in 5 failure rate. Independent
  of any active wave. Smallest possible fix is to convert any
  wall-clock or scheduling dependency to a synchronous deterministic
  shape.

## v0.2 deliberately deferred (rough value order)

See `MASTER-PLAN.md` items 14-20 for the full list. Highlights:
self-draining queue worker (bgworker + LISTEN/NOTIFY), resumable
`CheckpointExecutor`, `pg-synapse-a2a` client + server, governed
`pg-synapse-tools-cli`, `pg-synapse-provider-candle`, plan-and-solve
executor.

## Parked (`Not Yet` per the next-backlog process)

Do not start; raise a backlog item if pressure builds. Tracked items:
`synapse.plugin_*` / `provider_verify` / `execution_export` SQL (N2.2
until v0.2; tracked inside PS-1/4/7), real-time push (D8), OpenAPI gen
+ CI freshness, plugin-owned migrations, memory plugin, NL2SQL,
semantic catalog, workflow engine, CodeAct (downstream product modules
per D14), more provider plugins until PS-1 + PS-5 + PS-7 make adding
them safe (PS-5 now done; PS-7 is the remaining gate).

## How to pick the next thing

The shortest path:

1. Read `MASTER-PLAN.md` for current wave + priority order.
2. Read the latest commit log: `git log --oneline -20`.
3. Pick the highest-leverage Wave 4 item, or one of the v0.1.x small
   wins if Wave 4 needs more design.
4. Open a `mission-brief` for anything non-trivial (4+ steps), per the
   project conventions.
