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
