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
