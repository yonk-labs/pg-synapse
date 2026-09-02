//! `pg-synapse`: a command line client that talks to Postgres, not to a server.
//!
//! Deliberately not an HTTP client. Everything this needs is a SQL function
//! call, so speaking Postgres directly means the CLI works against a database
//! with the extension installed and nothing else running: no harness, no
//! deployment, no port to open. `psql` is the existence proof that this is the
//! natural shape for a tool like this.
//!
//! It also keeps the product honest. If the CLI needed a web tier to function,
//! that would be a sign logic had leaked out of the database, which is exactly
//! what this architecture exists to avoid.

use std::process::ExitCode;

use tokio_postgres::NoTls;

const USAGE: &str = "\
pg-synapse: run agents and saved questions against your own Postgres.

USAGE:
    pg-synapse <COMMAND> [ARGS]

COMMANDS:
    apps                          list apps
    agents                        list agents
    run <agent> <input>           run an agent and print its output
    ask <app> <question>          run a saved question and print its rows
    questions <app>               list an app's saved questions
    schedules                     list every schedule
    tick                          fire any schedules that are due
    runs [n]                      recent runs, newest first (default 10)

CONNECTION:
    Uses DATABASE_URL, or PG* environment variables, or libpq defaults.
    Example: DATABASE_URL=\"host=/var/run/postgresql dbname=mydb\" pg-synapse apps
";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // stderr, and a non-zero exit: this is a tool people will put in a
            // shell pipeline, and a failure that exits 0 is a failure that gets
            // ignored by whatever runs it next.
            eprintln!("pg-synapse: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: &[String]) -> Result<(), String> {
    let url = std::env::var("DATABASE_URL").unwrap_or_default();
    let (client, conn) = tokio_postgres::connect(&url, NoTls)
        .await
        .map_err(|e| format!("could not connect: {e}. Set DATABASE_URL or the PG* variables"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let arg = |i: usize, what: &str| -> Result<&str, String> {
        args.get(i)
            .map(String::as_str)
            .ok_or_else(|| format!("missing <{what}>. Run `pg-synapse help`"))
    };

    match args[0].as_str() {
        "apps" => {
            let rows = client
                .query(
                    "SELECT name, coalesce(schema_name,'') FROM synapse.apps ORDER BY name",
                    &[],
                )
                .await
                .map_err(sql_err)?;
            for r in rows {
                println!("{:<28} {}", r.get::<_, String>(0), r.get::<_, String>(1));
            }
        }
        "agents" => {
            let rows = client
                .query(
                    "SELECT name, executor_name FROM synapse.agents ORDER BY name",
                    &[],
                )
                .await
                .map_err(sql_err)?;
            for r in rows {
                println!("{:<28} {}", r.get::<_, String>(0), r.get::<_, String>(1));
            }
        }
        "run" => {
            let agent = arg(1, "agent")?;
            let input = args
                .get(2..)
                .map(|rest| rest.join(" "))
                .filter(|s| !s.is_empty())
                .ok_or("missing <input>. Run `pg-synapse help`")?;
            let row = client
                .query_one("SELECT synapse.execute($1, $2)::text", &[&agent, &input])
                .await
                .map_err(sql_err)?;
            let env: serde_json::Value =
                serde_json::from_str(&row.get::<_, String>(0)).unwrap_or_default();
            // The output is what a person or a pipeline wants; the status goes
            // to stderr so `pg-synapse run ... | thing` stays clean.
            if let Some(err) = env.get("error").and_then(|e| e.as_str()) {
                return Err(err.to_owned());
            }
            eprintln!(
                "status: {} ({} in / {} out)",
                env.get("status").and_then(|s| s.as_str()).unwrap_or("?"),
                env.get("tokens_in").and_then(|t| t.as_i64()).unwrap_or(0),
                env.get("tokens_out").and_then(|t| t.as_i64()).unwrap_or(0),
            );
            println!(
                "{}",
                env.get("output").and_then(|o| o.as_str()).unwrap_or("")
            );
        }
        "ask" => {
            let app = arg(1, "app")?;
            let question = arg(2, "question")?;
            let rows = client
                .query(
                    "SELECT sql_text, (confirmed_at IS NOT NULL) FROM synapse.questions \
                     WHERE app = $1 AND name = $2 AND kind = 'sql'",
                    &[&app, &question],
                )
                .await
                .map_err(sql_err)?;
            let row = rows
                .first()
                .ok_or_else(|| format!("no saved question \"{question}\" for app \"{app}\""))?;
            let confirmed: bool = row.get(1);
            if !confirmed {
                return Err(format!(
                    "question \"{question}\" has not been reviewed; its SQL must be approved before it runs"
                ));
            }
            let sql: String = row.get::<_, Option<String>>(0).unwrap_or_default();
            let out = client
                .query(&format!("SELECT to_jsonb(r)::text FROM ({sql}) r"), &[])
                .await
                .map_err(sql_err)?;
            for r in out {
                println!("{}", r.get::<_, String>(0));
            }
        }
        "questions" => {
            let app = arg(1, "app")?;
            let rows = client
                .query(
                    "SELECT name, nl_text FROM synapse.questions WHERE app = $1 ORDER BY name",
                    &[&app],
                )
                .await
                .map_err(sql_err)?;
            for r in rows {
                println!("{:<22} {}", r.get::<_, String>(0), r.get::<_, String>(1));
            }
        }
        "schedules" => {
            let rows = client
                .query(
                    "SELECT app, agent, every_interval::text, next_run_at::text, enabled \
                     FROM synapse.schedules ORDER BY next_run_at",
                    &[],
                )
                .await
                .map_err(sql_err)?;
            for r in rows {
                println!(
                    "{:<24} {:<24} every {:<12} next {} {}",
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, String>(2),
                    r.get::<_, String>(3),
                    if r.get::<_, bool>(4) {
                        ""
                    } else {
                        "(disabled)"
                    }
                );
            }
        }
        "tick" => {
            let row = client
                .query_one("SELECT synapse.tick()", &[])
                .await
                .map_err(sql_err)?;
            println!("fired {}", row.get::<_, i32>(0));
        }
        "runs" => {
            let n: i64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
            let rows = client
                .query(
                    "SELECT agent_name, status, coalesce(model,''), duration_ms, \
                            started_at::text \
                     FROM synapse.executions ORDER BY started_at DESC LIMIT $1",
                    &[&n],
                )
                .await
                .map_err(sql_err)?;
            for r in rows {
                println!(
                    "{:<24} {:<16} {:<16} {:>7}ms  {}",
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, String>(2),
                    r.get::<_, Option<i64>>(3).unwrap_or(0),
                    r.get::<_, String>(4),
                );
            }
        }
        other => {
            return Err(format!(
                "unknown command \"{other}\". Run `pg-synapse help`"
            ));
        }
    }
    Ok(())
}

/// Postgres errors arrive with a lot of context a CLI user does not want. Keep
/// the message, drop the rest.
fn sql_err(e: tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => db.message().to_owned(),
        None => e.to_string(),
    }
}
