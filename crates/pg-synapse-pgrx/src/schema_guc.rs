//! GUC registration for `pg_synapse`.
//!
//! GUCs ("Grand Unified Configuration") are Postgres' name for runtime
//! settings. Operators can `SET pg_synapse.disable_builtin_sql_tools = on;`
//! (per-session) or set defaults in `postgresql.conf` / `ALTER SYSTEM`.

use std::ffi::CString;

use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};

/// When true, the built-in `sql_query` / `sql_exec` tools refuse to run.
///
/// Decision D11: defaults to false; ops disable this when they want agents
/// in this database to be unable to SPI into tables from inside tool calls.
pub static DISABLE_BUILTIN_SQL_TOOLS: GucSetting<bool> = GucSetting::<bool>::new(false);

/// Default `llm_profile_main` name used when an agent row has none set.
pub static DEFAULT_LLM_PROFILE_MAIN: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(None);

/// Default per-execution timeout in milliseconds when the agent row has none.
pub static DEFAULT_TIMEOUT_MS: GucSetting<i32> = GucSetting::<i32>::new(60_000);

/// Default per-execution iteration cap when the agent row has none.
pub static DEFAULT_MAX_ITERATIONS: GucSetting<i32> = GucSetting::<i32>::new(10);

/// Register every GUC. Called once from `_PG_init`.
pub fn register_gucs() {
    GucRegistry::define_bool_guc(
        c"pg_synapse.disable_builtin_sql_tools",
        c"Disable the built-in sql_query / sql_exec tools.",
        c"When true, agents in this database cannot read or write tables via the built-in SQL tools.",
        &DISABLE_BUILTIN_SQL_TOOLS,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_synapse.default_llm_profile_main",
        c"Default llm_profile_main name when an agent row has none set.",
        c"Falls back when pg_synapse.agents.llm_profile_main is NULL.",
        &DEFAULT_LLM_PROFILE_MAIN,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_synapse.default_timeout_ms",
        c"Default agent execution timeout (milliseconds).",
        c"Default: 60000.",
        &DEFAULT_TIMEOUT_MS,
        100,
        i32::MAX,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_synapse.default_max_iterations",
        c"Default agent loop iteration cap.",
        c"Default: 10.",
        &DEFAULT_MAX_ITERATIONS,
        1,
        1_000,
        GucContext::Userset,
        GucFlags::default(),
    );
}
