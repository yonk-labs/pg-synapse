//! GUC registration and agent fallback resolution for `pg_synapse`.
//!
//! GUCs ("Grand Unified Configuration") are Postgres' name for runtime
//! settings. Operators can `SET pg_synapse.disable_builtin_sql_tools = on;`
//! (per-session) or set defaults in `postgresql.conf` / `ALTER SYSTEM`.
//!
//! The design spec defines 10 v0.1 GUCs (see `docs/design.md`, "GUCs (10 in
//! v0.1)"). We register all 10 plus the two operational GUCs the pgrx host
//! shipped in v0.1.0 (`disable_builtin_sql_tools`, `default_timeout_ms`,
//! `default_max_iterations`); those are kept so removing them is not a
//! regression.
//!
//! [`apply_guc_fallbacks`] is the single seam where an [`AgentRow`] field
//! left NULL / zero by the operator is filled from the matching GUC before
//! the row reaches the kernel.

use std::ffi::CString;

use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};

use pg_synapse_core::types::AgentRow;

/// When true, the built-in `sql_query` / `sql_exec` tools refuse to run.
///
/// Decision D11: defaults to false; ops disable this when they want agents
/// in this database to be unable to SPI into tables from inside tool calls.
pub static DISABLE_BUILTIN_SQL_TOOLS: GucSetting<bool> = GucSetting::<bool>::new(false);

/// Default `llm_profile_main` name used when an agent row has none set.
pub static DEFAULT_LLM_PROFILE_MAIN: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(None);

/// Default `llm_profile_small` name used when an agent row has none set.
pub static DEFAULT_LLM_PROFILE_SMALL: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(None);

/// Default `llm_profile_judge` name used when an agent row has none set.
pub static DEFAULT_LLM_PROFILE_JUDGE: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(None);

/// Default embedding profile name used when an agent row has none set.
pub static DEFAULT_EMBEDDING_PROFILE: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(None);

/// Default per-execution timeout in milliseconds when the agent row has none.
///
/// This is the operational GUC the pgrx host shipped in v0.1.0. The design
/// spec also names `default_timeout_seconds`; both are registered. Fallback
/// resolution uses `default_timeout_ms` first (millisecond fidelity), then
/// `default_timeout_seconds * 1000`.
pub static DEFAULT_TIMEOUT_MS: GucSetting<i32> = GucSetting::<i32>::new(60_000);

/// Design-spec per-execution timeout fallback, in whole seconds.
pub static DEFAULT_TIMEOUT_SECONDS: GucSetting<i32> = GucSetting::<i32>::new(60);

/// Default per-execution iteration cap when the agent row has none.
pub static DEFAULT_MAX_ITERATIONS: GucSetting<i32> = GucSetting::<i32>::new(10);

/// Default per-execution USD cost cap when the agent row has none.
///
/// A string GUC (not float) because the design default is "(none)" and a
/// Postgres real GUC cannot express "unset". An empty string means no cap;
/// any other value is parsed to `f64`.
pub static DEFAULT_COST_CAP_USD: GucSetting<Option<CString>> =
    GucSetting::<Option<CString>>::new(None);

/// Whether to write trace rows. Default true.
pub static TRACE_ENABLED: GucSetting<bool> = GucSetting::<bool>::new(true);

/// If set, the extension forwards to a sidecar at this URL instead of running
/// the kernel in-process. (Plumbing for N5; registered here so operators can
/// set it and `all_gucs_registered` sees it.)
pub static SIDECAR_URL: GucSetting<Option<CString>> = GucSetting::<Option<CString>>::new(None);

/// pgcrypto master key for secret encryption. None means secrets are stored
/// in cleartext (the v0.1 default).
pub static MASTER_KEY: GucSetting<Option<CString>> = GucSetting::<Option<CString>>::new(None);

/// If a Compressor is registered, conversation history above this many tokens
/// triggers compression. 0 means "unset / no threshold".
pub static COMPRESSION_THRESHOLD_TOKENS: GucSetting<i32> = GucSetting::<i32>::new(0);

/// Default executor name when an agent row leaves `executor_name` empty.
/// Not in the design's 10-GUC table but required by the N2.3 fallback list.
pub static DEFAULT_EXECUTOR: GucSetting<Option<CString>> = GucSetting::<Option<CString>>::new(None);

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
        c"Falls back when synapse.agents.llm_profile_main is NULL.",
        &DEFAULT_LLM_PROFILE_MAIN,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_synapse.default_llm_profile_small",
        c"Default llm_profile_small name when an agent row has none set.",
        c"Falls back when synapse.agents.llm_profile_small is NULL.",
        &DEFAULT_LLM_PROFILE_SMALL,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_synapse.default_llm_profile_judge",
        c"Default llm_profile_judge name when an agent row has none set.",
        c"Falls back when synapse.agents.llm_profile_judge is NULL.",
        &DEFAULT_LLM_PROFILE_JUDGE,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_synapse.default_embedding_profile",
        c"Default embedding profile name when an agent row has none set.",
        c"Falls back when synapse.agents.embedding_profile is NULL.",
        &DEFAULT_EMBEDDING_PROFILE,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_synapse.default_timeout_ms",
        c"Default agent execution timeout (milliseconds).",
        c"Default: 60000. Used before default_timeout_seconds when both are set.",
        &DEFAULT_TIMEOUT_MS,
        100,
        i32::MAX,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_synapse.default_timeout_seconds",
        c"Default agent execution timeout (whole seconds).",
        c"Default: 60. Design-spec name; default_timeout_ms takes precedence.",
        &DEFAULT_TIMEOUT_SECONDS,
        1,
        i32::MAX / 1000,
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

    GucRegistry::define_string_guc(
        c"pg_synapse.default_cost_cap_usd",
        c"Default per-execution USD cost cap when the agent row has none.",
        c"Empty string means no cap. Any other value is parsed as a number.",
        &DEFAULT_COST_CAP_USD,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_synapse.trace_enabled",
        c"Whether to write trace rows.",
        c"Default: true.",
        &TRACE_ENABLED,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_synapse.sidecar_url",
        c"If set, the extension forwards to a sidecar at this URL.",
        c"Empty means run the kernel in-process (the v0.1 default).",
        &SIDECAR_URL,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_synapse.master_key",
        c"Pgcrypto master key for secret encryption.",
        c"Empty means secrets are stored in cleartext (the v0.1 default).",
        &MASTER_KEY,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_synapse.compression_threshold_tokens",
        c"If a Compressor is registered, compress history above this many tokens.",
        c"Default: 0 (no threshold).",
        &COMPRESSION_THRESHOLD_TOKENS,
        0,
        i32::MAX,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_synapse.default_executor",
        c"Default executor name when an agent row leaves executor_name empty.",
        c"Empty means keep the agent row's value (defaults to 'conversation').",
        &DEFAULT_EXECUTOR,
        GucContext::Userset,
        GucFlags::default(),
    );
}

/// Read an `Option<CString>` GUC into an `Option<String>`, treating an empty
/// string as "unset" (`None`).
fn guc_string(setting: &GucSetting<Option<CString>>) -> Option<String> {
    setting
        .get()
        .and_then(|c| c.to_str().ok().map(str::to_owned))
        .filter(|s| !s.is_empty())
}

/// Fill any agent field the operator left NULL / zero / empty from the
/// matching GUC default, in one place, before the row reaches the kernel.
///
/// Resolution rules (per the v0.1.1 N2.3 spec):
///
/// * `llm_profile_main` / `_small` / `_judge` NULL -> matching GUC
/// * `embedding_profile` NULL -> `default_embedding_profile`
/// * `timeout_ms` 0 -> `default_timeout_ms`, else `default_timeout_seconds * 1000`
/// * `max_iterations` 0 -> `default_max_iterations`
/// * `cost_cap_usd` NULL -> parsed `default_cost_cap_usd` (empty = no cap)
/// * `executor_name` empty -> `default_executor`
pub fn apply_guc_fallbacks(agent: &mut AgentRow) {
    if agent.llm_profile_main.is_none() {
        agent.llm_profile_main = guc_string(&DEFAULT_LLM_PROFILE_MAIN);
    }
    if agent.llm_profile_small.is_none() {
        agent.llm_profile_small = guc_string(&DEFAULT_LLM_PROFILE_SMALL);
    }
    if agent.llm_profile_judge.is_none() {
        agent.llm_profile_judge = guc_string(&DEFAULT_LLM_PROFILE_JUDGE);
    }
    if agent.embedding_profile.is_none() {
        agent.embedding_profile = guc_string(&DEFAULT_EMBEDDING_PROFILE);
    }
    if agent.timeout_ms == 0 {
        let ms = DEFAULT_TIMEOUT_MS.get();
        if ms > 0 {
            agent.timeout_ms = ms as u64;
        } else {
            let secs = DEFAULT_TIMEOUT_SECONDS.get();
            if secs > 0 {
                agent.timeout_ms = (secs as u64) * 1000;
            }
        }
    }
    if agent.max_iterations == 0 {
        let it = DEFAULT_MAX_ITERATIONS.get();
        if it > 0 {
            agent.max_iterations = it as u32;
        }
    }
    if agent.cost_cap_usd.is_none() {
        agent.cost_cap_usd = guc_string(&DEFAULT_COST_CAP_USD).and_then(|s| s.parse::<f64>().ok());
    }
    if agent.executor_name.is_empty() {
        if let Some(exec) = guc_string(&DEFAULT_EXECUTOR) {
            agent.executor_name = exec;
        }
    }
}
