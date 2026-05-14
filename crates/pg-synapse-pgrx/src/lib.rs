//! `pg_synapse_pgrx` is the Postgres extension host for `pg_synapse`.
//!
//! It wires the kernel ([`pg_synapse_core::Runtime`]) into Postgres via pgrx,
//! exposing a small SQL surface under the `synapse` schema:
//!
//! * `synapse.execute(agent_name text, input text) -> jsonb`
//! * `synapse.agent_create(...)`, `agent_drop(name)`
//! * `synapse.llm_profile_set(...)`
//! * `synapse.secret_set(name, value)`
//! * `synapse.version() -> text`
//!
//! GUCs live under the `pg_synapse.*` namespace (custom GUC prefixes are
//! allowed even when the matching schema name would be reserved).
//!
//! A single `current_thread` tokio runtime is built at backend startup
//! (`_PG_init`) and reused for every call. The kernel is built lazily on first
//! `execute()` (reading agent / profile / secret rows from `synapse.*` via
//! SPI) and rebuilt whenever an admin function mutates configuration.

#![forbid(unsafe_code)]
#![allow(non_snake_case)]
#![warn(missing_docs)]

use pgrx::prelude::*;

pgrx::pg_module_magic!(name, version);

mod runtime_holder;
mod schema_guc;
mod spi_executor;
mod sql_functions;

pub use runtime_holder::{kernel_handle, rebuild_kernel};

/// pgrx-invoked at extension load. Registers GUCs and builds the shared tokio
/// runtime. The kernel itself is built lazily on first `execute()`.
#[pg_guard]
extern "C-unwind" fn _PG_init() {
    schema_guc::register_gucs();
    runtime_holder::initialize_tokio_runtime();
}

// Schema bootstrap: creates the synapse schema, tables, and roles when the
// extension is installed (`CREATE EXTENSION pg_synapse_pgrx`).
extension_sql_file!("../sql/schema.sql", name = "synapse_schema", bootstrap);

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn version_returns_a_string() {
        let v: Option<String> = Spi::get_one("SELECT synapse.version()").unwrap();
        let v = v.expect("version() must return a value");
        assert!(!v.is_empty(), "version string must be non-empty");
    }

    #[pg_test]
    fn schema_tables_exist() {
        let count: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM pg_tables WHERE schemaname = 'synapse'")
                .unwrap();
        let count = count.unwrap_or(0);
        assert!(
            count >= 8,
            "expected at least 8 tables in synapse schema, saw {count}",
        );
    }

    #[pg_test]
    fn roles_exist() {
        let admin: Option<bool> =
            Spi::get_one("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='synapse_admin')")
                .unwrap();
        let user_: Option<bool> =
            Spi::get_one("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='synapse_user')")
                .unwrap();
        assert_eq!(admin, Some(true));
        assert_eq!(user_, Some(true));
    }

    #[pg_test]
    fn agent_and_profile_writes_persist() {
        Spi::run(
            "SELECT synapse.llm_profile_set('test-llm', 'openai', 'gpt-test', NULL, NULL, '{}'::jsonb)",
        )
        .unwrap();
        Spi::run(
            "SELECT synapse.agent_create('echo_agent', 'You are helpful.', 'conversation', 'test-llm', ARRAY[]::text[], 5, 30000)",
        )
        .unwrap();

        let agent_count: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM synapse.agents WHERE name='echo_agent'")
                .unwrap();
        assert_eq!(agent_count, Some(1));

        let profile_count: Option<i64> =
            Spi::get_one("SELECT count(*)::bigint FROM synapse.llm_profiles WHERE name='test-llm'")
                .unwrap();
        assert_eq!(profile_count, Some(1));
    }
}

/// pgrx test framework hook.
#[cfg(test)]
pub mod pg_test {
    /// Per-test setup (none required for this extension).
    pub fn setup(_options: Vec<&str>) {}

    /// No postgresql.conf options required.
    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
