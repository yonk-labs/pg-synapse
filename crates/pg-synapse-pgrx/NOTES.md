# pg_synapse_pgrx implementation notes

## pgrx 0.18 API adaptations vs. the M7-phase-A brief

- **Schema name.** The brief specified `pg_synapse` for both the SQL schema and
  the `synapse_admin` / `synapse_user` roles. Postgres 11+ rejects schema and
  role names starting with the `pg_` prefix (reserved for system catalogs).
  Renamed to `synapse` / `synapse_admin` / `synapse_user`. The GUC namespace
  keeps the `pg_synapse.*` prefix because custom GUC prefixes are unrestricted.

- **Schema declaration.** pgrx 0.18's sql-entity-graph refuses to place a
  function in a schema that wasn't declared as a Rust module marked
  `#[pg_schema]`. Wrapped every `#[pg_extern]` inside a `pub(crate) mod
  synapse { ... }` annotated with `#[pg_schema]` rather than using the
  `schema = "..."` attribute.

- **Tokio runtime model.** The brief sketch used `new_multi_thread()`, but SPI
  calls (both from `SpiProfileSource` and `SpiSqlExecutor`) must run on the
  Postgres backend thread. A multi-thread runtime would hand polling to a
  worker thread and break SPI. Switched to `new_current_thread()`, so
  `block_on` polls inline on the backend thread and SPI works naturally.

- **Datum bindings.** pgrx 0.18 uses `DatumWithOid` (not `pg_sys::Datum`) for
  `Spi::run_with_args` and `client.select(..., args)`. There's a `From<T:
  IntoDatum>` impl, so we build `Vec<DatumWithOid>` via `DatumWithOid::from(v)`
  for scalars and `DatumWithOid::null::<T>()` for NULL. NUMERIC handling is
  deferred to phase B: `cost_cap_usd` and `cost_usd` round-trip as `f64`
  approximations for now.

- **GUCs.** `GucRegistry::define_*_guc` takes `&'static CStr` (not `&'static
  str`). Used C-string literals (`c"..."`, stable in edition 2021+).

- **`extension_sql_file!`.** The path is resolved relative to the crate root
  (where `Cargo.toml` lives), not relative to `lib.rs`. We use
  `"../sql/schema.sql"` because the macro expansion looks one level up from
  `crates/pg-synapse-pgrx/`. (The pgrx 0.18 macro actually resolves relative
  to `lib.rs` in our config; `../sql/schema.sql` happens to work because we
  generated the scaffold there. If you move files, adjust.)

- **Positional SQL params for the built-in tools.** Forwarding agent-supplied
  `params` through SPI requires translating arbitrary JSON values to typed
  Postgres datums. That's non-trivial and deferred. The current
  `SpiSqlExecutor` rejects non-empty `params`; the example agent prompt
  instructs the LLM to inline literals.

- **`execute()` error envelope.** The brief's pseudocode suggested raising on
  kernel error. Instead we return a `JsonB` envelope (`{"error": "...",
  "status": "errored"}`) so callers can branch in SQL without a savepoint.

- **`SECURITY DEFINER`.** The brief sketch put `security_definer` on
  `agent_create` etc.; pgrx 0.18's `#[pg_extern]` does not accept that key.
  Operators can `ALTER FUNCTION ... SECURITY DEFINER` post-install when they
  want it, or we'll wire it in phase B with a custom `extension_sql!` block.

## What was NOT built in phase A (deferred to phase B)

- Anthropic / llama-cpp provider plugins (M4 remainder).
- Embeddings plugin (M5).
- HTTP tools registration: the crate depends on `pg-synapse-tools-http` but
  doesn't register it in the kernel build yet. Add to `build_kernel_from_db`
  in `runtime_holder.rs` when HTTP tools are needed end-to-end.
- Positional bind parameters for `sql_query` / `sql_exec`.
- NUMERIC round-tripping for `cost_cap_usd` and `cost_usd`.
- `synapse.tools` registry: the table exists but no SQL function manages it.
- The docker-based integration harness (`examples/sql-agent-readwrite/run.sh
  --docker` is a stub).
- A `synapse.executions_logs` SECURITY DEFINER view filtered by `caller_role`.

## v0.1.1 N1 adaptations

These supersede the phase-A findings above where they conflict.

- **`SECURITY DEFINER` IS a valid `#[pg_extern]` key in pgrx 0.18.** The
  phase-A note ("pgrx 0.18's `#[pg_extern]` does not accept that key") was
  wrong. `pgrx-sql-entity-graph` 0.18 parses `security_definer` and
  `security_invoker` (see `extern_args.rs` / `pg_extern/attribute.rs`). Every
  `synapse.*` function is now `#[pg_extern(security_definer, ...)]`. No
  post-install `ALTER FUNCTION` is required.

- **Grant ordering via `extension_sql_file!(..., finalize)`.** GRANT/REVOKE
  on `synapse.*` functions must run after pgrx emits the `CREATE FUNCTION`
  statements. `schema.sql` stays `bootstrap` (first); the new `grants.sql`
  uses `finalize`, which pgrx emits last in the install script. This gives
  the correct ordering without a `requires = [...]` list or a deferred
  `synapse._apply_grants()` function.

- **SAVEPOINT-per-tool-call uses a Postgres *internal subtransaction*, not
  the SQL `SAVEPOINT` statement.** The kernel invokes the SQL tools from
  inside `synapse.execute(...)`, which is a SECURITY DEFINER function.
  Postgres rejects `SAVEPOINT` / `ROLLBACK TO SAVEPOINT` / `RELEASE
  SAVEPOINT` SQL when issued from within a function (valid only at the top
  transaction level or inside a PROCEDURE). The supported primitive in this
  position is an internal subtransaction, the same one PL/pgSQL's
  `BEGIN ... EXCEPTION` block uses. `spi_executor::with_savepoint` drives it
  via the documented C API (`BeginInternalSubTransaction`,
  `ReleaseCurrentSubTransaction`, `RollbackAndReleaseCurrentSubTransaction`)
  wrapped in pgrx's `PgTryBuilder` so a hard Postgres `ERROR` longjmp is
  converted into a catchable path and the subtransaction is rolled back
  instead of unwinding past it. `CurrentMemoryContext` /
  `CurrentResourceOwner` are saved before and restored after, because an
  aborted subxact leaves them dangling.

- **`#![forbid(unsafe_code)]` relaxed to `#![deny(unsafe_code)]`.** The
  internal-subtransaction calls and the two global reads/writes they need
  are FFI and therefore `unsafe`. They are confined to the single helper
  `spi_executor::with_savepoint`, each `unsafe` block carrying a `SAFETY:`
  note. Nothing else in the crate uses `unsafe`.

- **Positional bind params are implemented.** `SpiSqlExecutor::query` /
  `::execute` no longer reject a non-empty `params` array. `json_to_datum`
  maps each `serde_json::Value` to a typed `DatumWithOid` (string -> TEXT,
  integer -> INT8, float -> FLOAT8, bool -> BOOL, null -> typed TEXT NULL,
  object/array -> JSONB) and the slice is passed to `client.select` /
  `client.update`, exactly as `SpiProfileSource::secrets` already binds its
  array argument. The `sql_query` to_jsonb wrapper is a superquery, so the
  user's `$1..$n` placeholders resolve through it unchanged. The example
  agent prompts were switched from "inline literals" to "use `$1, $2`
  placeholders with a params array".

## v0.1.1 N2 adaptations

### N2.1 NUMERIC cost round-trip

- **`NUMERIC` maps to `pgrx::AnyNumeric`.** `cost_cap_usd` and `cost_usd`
  are `NUMERIC(12,6)` columns. pgrx 0.18 exposes `AnyNumeric` (both
  `FromDatum` and `IntoDatum`). Read path: `row.get::<AnyNumeric>(n)` then
  `f64::try_from(AnyNumeric)` (Postgres `numeric_float8`). Write path:
  `AnyNumeric::try_from(f64)` (Postgres `float8_numeric`), bound as a
  `DatumWithOid`. **Precision tradeoff:** an `f64` carries ~15-17
  significant decimal digits; the column holds at most 12 digits / 6
  decimals, so the round trip is lossless for any storable value. Both
  conversions are fallible; on failure (only a non-finite f64) we fall
  back to NULL rather than poisoning the audit row.

- **Cost-cap surfacing decision (no `RAISE`).** The kernel returns
  `OutcomeStatus::CostCapExceeded`; `status_label` already maps it to
  `"cost_cap_exceeded"`. The plan floated `RAISE EXCEPTION` on cap breach.
  We deliberately do **not** raise: `synapse.execute` is SECURITY DEFINER
  and a raised error would roll back the whole function, **including the
  `executions` audit row** that records the partial cost and the
  `cost_cap_exceeded` status. The better contract is: return the JSONB
  envelope with `"status":"cost_cap_exceeded"` AND log the `executions`
  row with `status='cost_cap_exceeded'` and the partial NUMERIC cost. The
  caller can branch in SQL on the envelope status; the audit trail
  survives. (`log_execution` already records `o.status` verbatim, so this
  needed no new code beyond the NUMERIC cost fix.)

### N2.2 Remaining SQL functions

- **`TABLE` returns vs JSONB.** `agent_list` / `tool_list` return a single
  `JsonB` array (objects) rather than `TABLE(...)` / `SETOF`. Consistent
  with `execution_status` and trivial to consume from SQL
  (`jsonb_array_elements`). Avoids the pgrx 0.18 `TableIterator` lifetime
  ceremony for a read that is not hot.

- **`execute_async` is synchronous under the hood (v0.1.1).** SPI is only
  legal on the backend thread that owns the transaction. The shared tokio
  runtime is `current_thread` precisely so `block_on` polls inline on that
  thread; a `tokio::spawn`ed task would poll on a different logical context
  with no backend transaction and **cannot SPI**. True background
  execution needs a Postgres bgworker (its own backend + transaction),
  which is out of scope for v0.1.1. So `execute_async`: (1) inserts a
  `status='queued'` placeholder row keyed by a fresh uuid, (2) runs the
  agent inline via the same kernel path as `execute`, (3) on success
  deletes the placeholder and logs the real outcome through
  `log_execution` (identical audit rows to the sync path), returning the
  kernel's execution_id; on failure marks the placeholder
  `status='errored'` and returns its id. The async **contract** (returns a
  uuid, pollable via `execution_status`) holds; only the scheduling is
  synchronous. True background execution is deferred to v0.2.

- **`tool_call` via new additive kernel method.** Added
  `Runtime::call_tool(name, input, caller_role) -> Result<Value,
  RuntimeError>` to `pg-synapse-core` (additive, no existing signature
  changed). It resolves the tool in the shared registry, runs it with a
  fresh `ToolCtx`, and flattens `ToolOutput` (`Text` -> JSON string,
  `Json` -> value, `Empty` -> null). The pgrx `tool_call` wraps it.

- **`tool_register` uses `default!`.** pgrx 0.18 `#[pg_extern]` supports
  `default!(&str, "'manual'")` / `default!(JsonB, "'{}'")` for SQL-level
  DEFAULTs; the SQL signature is `tool_register(text, text, jsonb, text,
  jsonb)` for the GRANT.

- **Grants extended.** `grants.sql` now also grants the new admin
  functions (`tool_register`, `llm_profile_drop`,
  `embedding_profile_drop`, `secret_drop`) to `synapse_admin` only and the
  new run/read functions (`agent_list`, `tool_list`, `tool_call`,
  `execute_async`, `execution_status`) to both roles.

### N2.3 Full GUC set + fallback resolution

- **All 10 design-spec GUCs registered** (`docs/design.md`, "GUCs (10 in
  v0.1)"): `default_llm_profile_main/_small/_judge`,
  `default_embedding_profile`, `default_timeout_seconds`,
  `default_cost_cap_usd`, `trace_enabled`, `sidecar_url`, `master_key`,
  `compression_threshold_tokens`. The v0.1.0 operational GUCs
  (`disable_builtin_sql_tools`, `default_timeout_ms`,
  `default_max_iterations`) are kept (removing them would regress) plus
  `default_executor` (named in the N2.3 fallback list but not the design
  table).

- **`default_cost_cap_usd` is a string GUC, not float.** The design
  default is "(none)" and a Postgres `real` GUC cannot represent "unset"
  (it always has a value). A string GUC distinguishes "" (no cap) from a
  parseable number. `master_key` is `Suset` context (superuser-set);
  everything else is `Userset`.

- **Fallback resolution lives in one function.**
  `schema_guc::apply_guc_fallbacks(&mut AgentRow)` fills NULL / zero /
  empty agent fields from the matching GUC. It is called once, from
  `SpiProfileSource::agents`, after each row is read and before it reaches
  the kernel. Timeout precedence: `default_timeout_ms` first
  (millisecond fidelity), then `default_timeout_seconds * 1000`.

## v0.1.1 B5: stringified-param type coercion

Benchmark run (gpt-5-mini s2_triage) revealed that the model emits integer
ids as JSON strings (e.g. `params: ["api", "3"]`). The prior TEXT binding
raised "operator does not exist: bigint = text".

**Preferred approach attempted: UNKNOWNOID binding.**
`DatumWithOid::new(CString, pg_sys::UNKNOWNOID)` was tried first. The
Postgres `unknown` pseudo-type (OID 705) is what the parser assigns to
untyped string literals, and its wire format is a null-terminated C string
(typlen = -2). Tagging a CString datum with `UNKNOWNOID` and passing it to
`SPI_execute_with_args` did resolve `bigint = $1` correctly, but raised
"failed to find conversion function from unknown to text" for `text = $1`
predicates. The `unknown -> text` implicit cast is wired in the parser's
type-resolution path but is not available through the `SPI_execute_with_args`
typed-parameter path. UNKNOWNOID is therefore unsafe as a general solution.

**Chosen approach: numeric-coercion fallback.**
`json_to_datum` for `Value::String` now tries `parse::<i64>()` (bind INT8),
then `parse::<f64>()` with finite-only guard (bind FLOAT8), and falls back to
TEXT. This covers the dominant LLM pattern (stringified integer id) without
breaking text columns. The fix is in `spi_executor::json_to_datum`; no unsafe
code is required. Four regression tests were added under the B5 heading in
`lib.rs`: `bind_stringified_int_id_coerces`, `bind_numeric_id_still_works`,
`bind_update_with_stringified_id`, `bind_text_predicate_still_works`.

## v0.1.1 B10: pg-synapse-tools-fs wiring

`tools-fs` is now in the `default` feature list. At kernel build time
(`build_kernel_from_db`) the fs plugin is registered under
`#[cfg(feature = "tools-fs")]`. The sandbox root is hardcoded to
`/tmp/pg_synapse_fs` (created with `create_dir_all` if absent).

**TODO (GUC):** Wire a `pg_synapse.fs_tools_root` string GUC so operators can
configure the sandbox root without recompiling. The pgrx GUC registration API
(via `pgrx::GucSetting`) would live in a new `gucs.rs` module. Deferred
because the GUC fetch path requires a `_PG_init` hook that is not yet present;
the hardcoded default is safe for current development use.

**Install command used for B10:**
```
ORT_DYLIB_PATH=... cargo pgrx install --pg-config .../pg_config \
  --features pg17,embed-ort,provider-llama-cpp,provider-anthropic,tools-fs \
  --no-default-features
```

## v0.1.1 B12: fs arg-alias leniency + harness robustness

**fs tool arg aliases.** The neutral fs benchmark revealed that models (gemma-4-E2B/E4B,
qwen3.5-2b/0.8b) emit tool calls with aliased argument names rather than the canonical
field names. For example, `{"file":"x.txt"}` instead of `{"path":"x.txt"}`.
`#[serde(alias = "...")]` was added to every fs tool input struct:

- `path` on read_file/write_file/edit_file/grep: aliases `file`, `filename`, `filepath`,
  `file_path`, `filePath`
- `content` on write_file: aliases `text`, `data`, `body`, `file_content`, `contents`
- `dir` on list_files: aliases `directory`, `folder`, `dir_path`, `dirpath`, `path`
- `old` on edit_file: aliases `old_string`, `old_str`, `search`, `find`, `from`
- `new` on edit_file: aliases `new_string`, `new_str`, `replace`, `replacement`, `to`
- `pattern` on grep: aliases `query`, `search`, `regex`, `q`, `text`

The `#[derive(JsonSchema)]` is still used (no schema conflict with aliases); schemars derives
the schema from the canonical field names, so the advertised schema remains clean. Seven alias
unit tests were added to `plugins/pg-synapse-tools-fs/tests/fs.rs`.

**Harness robustness.** `bench/run_bench.sh` now:

1. Calls `ensure_pg_ready()` before the model loop: retries `psql -c 'SELECT 1'` up to 20s,
   verifies `pg_synapse_pgrx` is in `pg_available_extensions`, hard-fails with a clear message
   if not. Prevents every model cell from failing with a CREATE EXTENSION error when the issue
   is a stopped Postgres server.
2. `install_extension_with_retry()`: wraps CREATE EXTENSION in 3 attempts with 2s backoff;
   records `infra_error=true` in the JSONL row on persistent failure.
3. `warmup_llama_server()`: fires one tiny chat completion after the server passes
   `wait_for_llama_server`, ensuring the model is fully loaded before timed scenarios.
4. Network-error retry: detects "network error" or "error sending request" in the execute
   output and retries the agent execute once.
5. `infra_error` field added to every JSONL row. Consumers (MODEL-COMPATIBILITY.md,
   min-specs.md) exclude infra-error rows from model verdicts.

## v0.1.1 A2: pg-synapse-tools-lede + a2_distill scenario

### A2.1 Plugin (pg-synapse-tools-lede)

`lede_compress` is a documented shim tool. Two code paths:

1. **lede CLI on PATH:** spawns `lede --max-tokens <n>`, feeds `text` on stdin,
   captures stdout. Wrapped in `ToolError::Execution` on non-zero exit. This is
   the only subprocess in the crate; `#![forbid(unsafe_code)]` holds.

2. **Extractive shim (no lede CLI):** splits text on `. ! ?` boundaries,
   scores each sentence by length-normalized keyword salience (words >4 chars
   with frequency >1 in the full text), selects greedily in score order until
   the approximate token budget (words * 1.3) is reached, returns sentences in
   original order.

Returns `ToolOutput::Json({ "brief", "source": "lede-cli"|"extractive-shim",
"input_chars", "brief_chars" })`.

Arg aliases: `content`/`input` -> `text`; `budget`/`limit`/`max` -> `max_tokens`.

### A2.2 Host wiring

- Feature flag: `tools-lede = ["dep:pg-synapse-tools-lede"]` added to pgrx features.
- `tools-lede` added to pgrx `default` feature list.
- `runtime_holder.rs`: `#[cfg(feature = "tools-lede")]` block registers
  `LedeToolsPlugin::new()` after the `tools-fs` block.
- Sidecar `main.rs`: `LedeToolsPlugin::new()` registered unconditionally next
  to `FsToolsPlugin` in `build_runtime`.
- `pg-synapse-sidecar/Cargo.toml`: `pg-synapse-tools-lede` added as a direct dep.

**Install command used for A2:**
```
ORT_DYLIB_PATH=... cargo pgrx install --pg-config .../pg_config \
  --features pg17,embed-ort,provider-llama-cpp,provider-anthropic,tools-fs,tools-lede \
  --no-default-features
```

### A2.3 Scenario (bench/scenarios/a2_distill)

8 deterministic mixed-sentiment feedback items in `feedback.items`. Agent must:
classify sentiment, write gist, insert 8 rows into `feedback.digest`, then call
`lede_compress` on concatenated gists.

Assertion grades loosely: all 8 rows present, all sentiments in
(positive/negative/neutral), and at least 6/8 match the known-good map (items
1,2,5,6 -> positive; 3,4,7 -> negative; 8 -> neutral). The >=6/8 threshold
measures agent loop correctness without requiring sentiment perfection.

**Authoritative fs leaderboard (B12, 2026-05-16):**

| model | fs pass | fs tool-emit | verdict |
|-------|---------|-------------|---------|
| vllm-qwen3-coder | 3/3 | 3/3 | WORKS |
| openai-gpt5-mini | 3/3 | 3/3 | WORKS |
| qwen3-4b-2507 | 3/3 | 3/3 | WORKS |
| qwen3.5-0.8b | 2/3 | 1/3 | PARTIAL |
| gemma-4-E2B-it | 1/3 | 1/3 | PARTIAL |
| gemma-4-E4B-it | 1/3 | 1/3 | PARTIAL |
| qwen2.5-7b | 1/3 | 2/3 | PARTIAL |
| qwen3.5-2b | 1/3* | 0/3 | PARTIAL |
| llama-3.2-3b | 0/3 | 0/3 | NO |
| smollm3-3b | 0/3 | 0/3 | NO |
| granite-4.0-h-1b | 0/3 | 0/3 | NO |
| granite-4.0-tiny-preview | 0/3 | 0/3 | NO |

*qwen3.5-2b f2_edit pass=true with lat=0/tool_error is an assertion artifact, not a real pass.

The gemma/qwen3.5 models still trigger `missing field path` on f1 and f3 even after the alias
fix. Investigation showed those models used a field name not in our alias set (the error
`missing field path` means the payload had none of: path/file/filename/filepath/file_path/filePath).
Likely the model sent a bare string or an unrecognized field. Further alias extension may help
but was not pursued in B12.

## T1: reactive triggers (ADR D14 / operator approval 2026-05-17)

T1 adds `synapse.agent_queue` and four new SQL functions. The `synapse.*`
surface additions are explicitly approved by ADR D14 and the operator decision
recorded 2026-05-17, which override the N2.2 next-backlog deferral.

### Schema

`synapse.agent_queue` added to `schema.sql` (8 original tables + 1 = 9 total).
Columns: `job_id uuid pk`, `agent text`, `input text`, `status text check (...)`,
`result jsonb`, `error text`, `source text`, `enqueued_at`, `started_at`,
`finished_at`. Grants: `synapse_user` gets SELECT; `synapse_admin` gets full DML
via the existing `GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES` clause.

### Functions

**`synapse.enqueue(agent, input, source) -> uuid`**: Plain INSERT into
`synapse.agent_queue`. No LLM call. Returns the new `job_id`. Granted to both
`synapse_user` and `synapse_admin` because writers (including trigger functions
running in the writer's session) need enqueue access.

**`synapse.drain_queue(max_jobs int) -> int`**: Claims up to `max_jobs` queued
rows with `FOR UPDATE SKIP LOCKED` (concurrency-safe), marks each `running`,
calls the existing `execute()` function in-process (no SPI round-trip for the
kernel call), writes `done`/`error` + result/finished_at. Returns count
processed. Admin-only (runs agent execution). v0.1 drain model: operator-driven
via pg_cron or sidecar poller. A true bgworker drain is v0.2 (design spec D8).

**`synapse.attach_agent_trigger(target_table, agent, mode, events, when_sql, input_expr)`**:
Generates a per-table PL/pgSQL trigger function + AFTER row-level trigger via SPI.
Identifier safety: the table name used in the trigger body is a PL/pgSQL string
literal (SQL-quoted via `replace('\'', "''")`) not an identifier. The trigger name
and function name are built from a safe underscored form of the table name. The
DROP and CREATE TRIGGER statements use `format(%I, %s::regclass)` via Postgres
`format()` to handle identifier quoting safely.

Queue-mode body: `PERFORM synapse.enqueue(agent_lit, (input_expr)::text, 'trigger:table')`.

Inline-mode body: calls `synapse.execute`, checks `status != 'completed'` or
`output->>'decision' = 'reject'`, and `RAISE EXCEPTION` with the reason. This
rolls back the triggering statement (and everything in its transaction) because
RAISE in a trigger function aborts the statement.

Recursion guard: `IF pg_trigger_depth() > 1 THEN RETURN NEW; END IF;` in both
modes. An agent's `sql_exec` writing back to the same table increments trigger
depth to 2, so the guard skips the enqueue/execute path. This prevents infinite
recursion without requiring per-trigger state.

**`synapse.detach_agent_trigger(target_table) -> void`**: Drops the trigger and
trigger function by the same derived names. Uses `format(%I, %s::regclass)` for
the DROP TRIGGER statement. Idempotent (IF EXISTS).

### Grant model

`enqueue`: both roles (trigger functions run in the session of the DML caller,
who may hold only `synapse_user`).
`drain_queue`, `attach_agent_trigger`, `detach_agent_trigger`: `synapse_admin`
only (DDL creation + LLM execution).

### pg_test suite (5 new tests)

All 5 tests run without a live LLM:
1. `enqueue_inserts_queued_row`: plain INSERT assertion.
2. `drain_queue_on_empty_returns_zero`: drain on empty queue = 0.
3. `attach_and_detach_agent_trigger_round_trip`: pg_trigger + pg_proc introspection.
4. `queue_mode_trigger_enqueues_on_insert`: INSERT fires trigger, queue row appears.
5. `inline_mode_raise_rolls_back_insert`: simulated raise via stub trigger function.
6. `trigger_depth_guard_prevents_double_enqueue`: one INSERT = one queue row.

All 36 pg_tests pass (31 existing + 5 new).

### Feature install command (unchanged from B-keystone)

```
pg17,embed-ort,provider-llama-cpp,provider-anthropic,tools-fs,tools-lede,tools-calc,tools-clock,tools-delegate
```

## B-keystone: external-framework parity tools + scenarios

### Three new tool plugins

**`pg-synapse-tools-calc`** (calculator tool, add/sub/mul/div)
Registered as feature `tools-calc`. Pure Rust, no unsafe. Serde aliases: `operation` for `op`, `x` for `a`, `y` for `b`. Division by zero returns `ToolError::Execution`.

**`pg-synapse-tools-clock`** (get_current_time tool)
Registered as feature `tools-clock`. Returns RFC 3339 timestamps. Supports UTC and fixed-offset (+HH:MM) timezones; IANA zone names fall back to UTC with a tracing warning. Uses `chrono` (workspace dep).

**`pg-synapse-tools-delegate`** (call_agent tool)
Registered as feature `tools-delegate`. Enables multi-agent delegation (OpenAI SDK handoff parity, ADK orchestrator parity). Depth guard via process-wide `AtomicU32`; max depth = 4. Returns sub-agent output as `ToolOutput::Text`.

### Depth guard design choice

The guard uses a process-wide `AtomicU32` (incremented on entry, decremented on exit even on error) rather than threading a `delegation_depth: u8` field through `ToolCtx`. Rationale: `Runtime::execute` starts a fresh `ExecutionContext` and does not forward any caller `ToolCtx`, so depth threading would require changes to `Runtime::execute`, `execute_inner`, `ExecutionContext`, and `Executor::execute`. The atomic is correct for pgrx (current-thread tokio, one request per backend at a time) and for the sidecar (nested awaits within one request are sequential). A concurrent-top-level-requests edge case exists in theory but not in practice for these hosts.

### Arc<Runtime> circular-dependency resolution (two-phase wiring)

`RuntimeBuilder` moves the `Registry` into `Arc` at `.build()` time; post-build insertion is not possible without interior mutability. The `DelegateToolsPlugin` pattern:

1. Create `Arc<CallAgentTool::empty()>` (holds `OnceLock<Weak<Runtime>>`).
2. Register it via `DelegateToolsPlugin::with_tool(tool.clone())` BEFORE `.build()`.
3. After `.build()`, wrap the `Runtime` in `Arc`, then call `tool.inject(Arc::downgrade(&runtime_arc))`.

pgrx host: the shell is stored in `DELEGATE_TOOL_PENDING` (a `OnceCell<Mutex<Option<Arc<CallAgentTool>>>>`) between `build_kernel_from_db` (phase 1) and `kernel_handle` (phase 2, where `Arc::new(built)` is created and the Weak is injected).

sidecar: `build_runtime` is `async` and returns `Arc<Runtime>` directly, so phases 1 and 2 happen sequentially in the same function. The return type changed from `anyhow::Result<Runtime>` to `anyhow::Result<Arc<Runtime>>`.

### Feature set for extension reinstall

```
pg17,embed-ort,provider-llama-cpp,provider-anthropic,tools-fs,tools-lede,tools-calc,tools-clock,tools-delegate
```

### Three external-framework benchmark scenarios

`lg_calc`: LangGraph calculator parity. Two chained calculator calls, result stored in `lg.result`. Assert: `value = 294` (12+30)*7.

`oai_triage`: OpenAI Agents SDK handoff/triage parity. Entry triage agent uses `call_agent` to delegate to `math_specialist` sub-agent (seeded in seed.sql.tmpl with `bench_profile`, which exists by execute time). Assert: `answer LIKE '%72%'` (18 * 4 = 72).

`adk_root`: Google ADK root_agent + tool parity. Agent calls `get_current_time`, checks the iso8601 field, inserts true/false into `adk.probe`. Assert: `has_time = true`.

### oai_triage sub-agent seeding note

Sub-agent rows (`math_specialist`, `history_specialist`) are inserted in `seed.sql.tmpl` referencing `llm_profile_main = 'bench_profile'`. The seed runs before the harness calls `llm_profile_set`, but the kernel is built lazily on first `synapse.execute()` (after the profile exists). This is safe.
