//! Baked demo scenarios for the Load-demo menu. Each scenario is a SQL file
//! (adapted from the repo's examples/ where one exists) applied with
//! `batch_execute`, plus metadata the UI uses to prefill the run panel,
//! watch tables, and probe buttons (including an assert-able expected end
//! state per scenario).

use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct SuggestedRun {
    pub agent: &'static str,
    pub input: &'static str,
    pub label: &'static str,
}

/// A whitelisted read-only query the UI can fire (see api::PROBE_QUERIES),
/// rendered as plain text lines. Used for EXPLAIN output and end-state
/// assertions.
#[derive(Clone, Serialize)]
pub struct Probe {
    pub key: &'static str,
    pub label: &'static str,
}

#[derive(Clone, Serialize)]
pub struct Scenario {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// Table keys (see api::TABLE_QUERIES) worth watching for this scenario.
    pub watch_tables: &'static [&'static str],
    pub suggested_runs: &'static [SuggestedRun],
    pub probes: &'static [Probe],
    #[serde(skip)]
    pub sql: &'static str,
}

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        id: "index-tuner",
        title: "Autonomous index tuner",
        description: "perf.orders has 100k rows and no index on customer_id, so the \
                      canonical query seq-scans. The agent runs EXPLAIN, creates the \
                      missing index (plain CREATE INDEX is transaction-safe), and \
                      verifies the plan flipped to an index scan. Use the EXPLAIN \
                      probe before and after the run.",
        watch_tables: &[],
        suggested_runs: &[SuggestedRun {
            agent: "index_tuner",
            input: "This query is slow: SELECT count(*), sum(amount) FROM perf.orders \
                    WHERE customer_id = 4242. Diagnose it and fix it if you can.",
            label: "Diagnose and fix the slow query",
        }],
        probes: &[
            Probe {
                key: "explain_orders",
                label: "EXPLAIN the slow query",
            },
            Probe {
                key: "perf_indexes",
                label: "List indexes on perf.orders",
            },
            Probe {
                key: "assert_index_tuner",
                label: "Check expected end state",
            },
        ],
        sql: include_str!("../scenarios/index_tuner.sql"),
    },
    Scenario {
        id: "dba-tickets",
        title: "The DBA that opens tickets",
        description: "Four health signals: one is safe to auto-fix inside a \
                      transaction (a missing index), three are not (work_mem, \
                      REINDEX CONCURRENTLY, shared_buffers). The agent applies the \
                      safe fix and files dba.recommendations tickets for the rest, \
                      with rationale.",
        watch_tables: &["signals", "recommendations"],
        suggested_runs: &[SuggestedRun {
            agent: "dba_advisor",
            input: "Review the pending health signals and act on each one.",
            label: "Review health signals",
        }],
        probes: &[Probe {
            key: "assert_dba",
            label: "Check expected end state",
        }],
        sql: include_str!("../scenarios/dba_tickets.sql"),
    },
    Scenario {
        id: "etl",
        title: "LLM-powered ETL",
        description: "etl.raw_contacts holds messy free-text notes (inconsistent \
                      country names, buried emails, mixed tone). The agent extracts \
                      name / company / email, normalizes countries to ISO codes, \
                      classifies intent, and inserts clean rows into etl.contacts. \
                      Unstructured to structured, entirely in the database.",
        watch_tables: &["raw_contacts", "contacts"],
        suggested_runs: &[SuggestedRun {
            agent: "etl_agent",
            input: "Process all raw contacts into the clean contacts table.",
            label: "Run the ETL pass",
        }],
        probes: &[Probe {
            key: "assert_etl",
            label: "Check expected end state",
        }],
        sql: include_str!("../scenarios/etl.sql"),
    },
    Scenario {
        id: "triage",
        title: "Ticket triage (warm-up)",
        description: "An agent reads support tickets with SQL, decides category and \
                      priority, and writes the classification back to the row. \
                      Reused from examples/customer-support-triage.",
        watch_tables: &["support_tickets"],
        suggested_runs: &[
            SuggestedRun {
                agent: "triage_agent",
                input: "Triage ticket 1.",
                label: "Triage ticket 1",
            },
            SuggestedRun {
                agent: "triage_agent",
                input: "Triage ticket 2.",
                label: "Triage ticket 2",
            },
            SuggestedRun {
                agent: "triage_agent",
                input: "Triage ticket 3.",
                label: "Triage ticket 3",
            },
        ],
        probes: &[Probe {
            key: "assert_triage",
            label: "Check expected end state",
        }],
        sql: include_str!("../scenarios/triage.sql"),
    },
    Scenario {
        id: "bouncer",
        title: "Transaction bouncer",
        description: "Reactive triggers in both modes: INSERTs into demo.tickets \
                      enqueue an enrichment agent (queue mode), and INSERTs into \
                      demo.orders are gated by a policy agent that can ROLL BACK \
                      the transaction with a reason (inline mode). Reused from \
                      examples/reactive-triggers. Drive it from the Event triggers \
                      panel below the run panel.",
        watch_tables: &["tickets", "orders", "queue"],
        suggested_runs: &[],
        probes: &[Probe {
            key: "assert_bouncer",
            label: "Check expected end state",
        }],
        sql: include_str!("../scenarios/bouncer.sql"),
    },
    Scenario {
        id: "guardrails",
        title: "Guardrails",
        description: "Runaway agents stopped by the runtime: cost_capped_agent trips \
                      its cost_cap_usd (synthetic pricing on a derived profile), \
                      time_capped_agent trips its timeout_ms budget, and \
                      marathon_agent runs long enough to stop with the Cancel \
                      button (pg_cancel_backend). Watch the run panel status chip; \
                      capped and cancelled runs surface there, not in the audit \
                      tables.",
        watch_tables: &[],
        suggested_runs: &[
            SuggestedRun {
                agent: "cost_capped_agent",
                input: "Sum the numbers 1 through 200, one addition at a time.",
                label: "Trip the cost cap",
            },
            SuggestedRun {
                agent: "time_capped_agent",
                input: "Sum the numbers 1 through 200, one addition at a time.",
                label: "Trip the timeout",
            },
            SuggestedRun {
                agent: "marathon_agent",
                input: "Sum the numbers 1 through 500, one addition at a time.",
                label: "Start a marathon (then hit Cancel)",
            },
        ],
        probes: &[],
        sql: include_str!("../scenarios/guardrails.sql"),
    },
];

pub fn find(id: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|s| s.id == id)
}
