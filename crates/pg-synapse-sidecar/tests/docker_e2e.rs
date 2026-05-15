//! Docker-based end-to-end test for the pg-synapse-sidecar.
//!
//! This test is `#[ignore]` by default: it requires Docker to be present and
//! accessible. Run it explicitly with:
//!
//!   cargo test -p pg-synapse-sidecar -- --ignored
//!
//! ## What it does
//!
//! 1. Finds two free TCP ports (Postgres + sidecar HTTP).
//! 2. Starts a `postgres:17` container on the Postgres port.
//! 3. Polls until Postgres is ready (max 30 s).
//! 4. Applies `sql/sidecar-install.sql` (with `{{SIDECAR_URL}}` substituted).
//! 5. Starts the `pg-synapse-sidecar` binary pointing at the container DB.
//! 6. Polls until `/v1/health` returns 200 (max 15 s).
//! 7. Asserts `/v1/version` returns a non-empty version string.
//! 8. Tears down the container and the sidecar process on drop.
//!
//! No LLM calls are made: CI lacks an LLM endpoint. The test validates the
//! install.sql schema and the sidecar HTTP surface without running agents.

#![allow(dead_code)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Find a free TCP port by binding to port 0 and reading the assigned port.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind port 0");
    listener.local_addr().unwrap().port()
}

/// Attempt to detect if Docker is available without running a container.
fn docker_available() -> bool {
    Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

struct DockerContainer {
    name: String,
}

impl Drop for DockerContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct SidecarProcess {
    child: Child,
}

impl Drop for SidecarProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Wait up to `timeout` for `predicate` to return true, sleeping 500 ms
/// between checks.
fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if predicate() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[tokio::test]
#[ignore = "requires docker"]
async fn sidecar_healthcheck_and_install_sql() {
    if !docker_available() {
        eprintln!("SKIP: docker not available");
        return;
    }

    let pg_port = free_port();
    let sidecar_port = free_port();
    let suffix: u32 = rand_u32();
    let container_name = format!("pg-synapse-sidecar-test-{suffix}");
    let sidecar_url = format!("http://127.0.0.1:{sidecar_port}");
    let db_url = format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres");

    // 1. Start Postgres container.
    let status = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-d",
            "--name",
            &container_name,
            "-e",
            "POSTGRES_PASSWORD=postgres",
            "-p",
            &format!("127.0.0.1:{pg_port}:5432"),
            "postgres:17",
        ])
        .status()
        .expect("docker run");
    assert!(status.success(), "docker run failed");

    let _container = DockerContainer {
        name: container_name.clone(),
    };

    // 2. Poll until Postgres is ready.
    let pg_ready = wait_until(Duration::from_secs(30), || {
        Command::new("docker")
            .args(["exec", &container_name, "pg_isready", "-U", "postgres"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    });
    assert!(pg_ready, "Postgres did not become ready within 30s");

    // 3. Apply sidecar-install.sql.
    let install_sql_path = workspace_root().join("sql/sidecar-install.sql");
    let raw_sql = std::fs::read_to_string(&install_sql_path).expect("read sql/sidecar-install.sql");
    let sql = raw_sql.replace("{{SIDECAR_URL}}", &sidecar_url);

    // Write substituted SQL to a temp file so psql can read it.
    let tmp_sql = std::env::temp_dir().join(format!("sidecar-install-{suffix}.sql"));
    std::fs::write(&tmp_sql, sql).expect("write tmp sql");

    let psql_status = Command::new("docker")
        .args([
            "exec",
            "-i",
            &container_name,
            "psql",
            "-U",
            "postgres",
            "-f",
            "/dev/stdin",
        ])
        .stdin(std::fs::File::open(&tmp_sql).unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .expect("psql");
    let _ = std::fs::remove_file(&tmp_sql);
    assert!(psql_status.success(), "sidecar-install.sql failed");

    // 4. Start the sidecar binary.
    let bin_path = workspace_root().join("target/debug/pg-synapse-sidecar");

    let child = Command::new(&bin_path)
        .args([
            "--port",
            &sidecar_port.to_string(),
            "--database-url",
            &db_url,
        ])
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn pg-synapse-sidecar binary (run `cargo build -p pg-synapse-sidecar` first)");
    let mut _sidecar = SidecarProcess { child };

    // 5. Poll /v1/health.
    let client = reqwest::Client::new();
    let health_url = format!("{sidecar_url}/v1/health");
    let healthy = wait_until(Duration::from_secs(15), || {
        tokio::runtime::Handle::current().block_on(async {
            client
                .get(&health_url)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        })
    });
    assert!(healthy, "sidecar /v1/health did not return 200 within 15s");

    // 6. Assert /v1/version.
    let version_resp = client
        .get(format!("{sidecar_url}/v1/version"))
        .send()
        .await
        .expect("version request");
    assert!(version_resp.status().is_success());
    let body: serde_json::Value = version_resp.json().await.unwrap();
    assert!(!body["version"].as_str().unwrap_or("").is_empty());
}

fn workspace_root() -> std::path::PathBuf {
    // Cargo sets CARGO_MANIFEST_DIR to the crate's manifest dir.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Go up two levels: crates/pg-synapse-sidecar -> crates -> workspace root
    std::path::Path::new(&manifest)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn rand_u32() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
}
