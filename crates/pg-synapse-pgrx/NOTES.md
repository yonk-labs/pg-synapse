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
