# Sidecar demo: build log, fixes, and open items

Running log for the "sidecar accesses a remote Postgres" demo work
(branch `demo-harness`). Newest at the bottom of each section.

## What this demo proves

`pg-synapse-sidecar` (the out-of-process kernel host) runs beside a managed
Postgres it cannot extend, connects over the wire (sqlx/libpq), and drives the
same agent loop / SQL tools as the in-process extension. Mimicked locally with
two containers: `rds` (stock `postgres:17`, the RDS stand-in) + `sidecar`.

## Verified working

- [x] `docker-compose.sidecar.yml` `rds` service seeds the `synapse` schema on a
      STOCK postgres:17 via plain `schema.sql` init script. 9 tables created,
      zero errors. Confirms the schema is plain DDL (no extension needed).
- [x] `cargo build --release -p pg-synapse-sidecar` succeeds (exit 0).
- [x] Sidecar boots against the remote container: `GET /v1/health` -> `{"status":"ok"}`,
      `GET /v1/version` -> `{"version":"0.1.1"}`.
- [x] READ remote PG via sidecar: `POST /v1/tool_call {tool:sql_query}` listed all
      9 remote `synapse.*` tables.
- [x] WRITE remote PG via sidecar: `sql_exec` create + insert, then read back the
      row. Independently confirmed with `docker exec ... psql` directly in the
      remote container. The write really landed in the remote DB.

## Bugs / gotchas found

- **sql_exec is one statement per call.** sqlx uses prepared statements, so a
  multi-statement body ("create ...; insert ...") fails with
  `cannot insert multiple commands into a prepared statement`. Split into one
  tool_call per statement. (Product behavior, not a demo bug. Worth surfacing
  in the sidecar tab / docs so users don't trip on it.)
- **tool_call requires `Content-Type: application/json`** (axum Json extractor).
  Obvious, but curl without the header returns a 4xx.
- **`/v1/admin/agent` silently drops unknown fields.** The struct fields are
  `executor_name` and `llm_profile_main`; sending `executor` / `llm_profile`
  (my first draft of the Install-tab curl) returns `{"ok":true}` but the agent
  lands with `llm_profile_main = NULL`, so `execute` later fails with no profile
  bound. Fixed the Install tab + compose comment to the real field names.
  (Product note: `AdminAgentRequest` has no `deny_unknown_fields`, so typos in
  the admin API fail open. Worth a serde `deny_unknown_fields` in the sidecar.)

## Docs drift noticed (NOT fixing inline; flagging)

- `docs/sidecar-quickstart.md` still says the sidecar is "not yet shipped."
  README + CHANGELOG + the shipped `pg-synapse-sidecar` crate contradict this;
  the binary is real and verified live here. The quickstart doc needs an update.

## Done this session

- [x] New dedicated page `/sidecar`: topology diagram + LIVE probe of the remote
      PG through the sidecar. Renders clean (headless screenshot verified).
- [x] Harness proxy route `/api/sidecar/probe` (server-side -> sidecar, no
      browser CORS). Verified: returns remote server identity
      (`<container-ip>:5432`, PostgreSQL 17.10) + live SQL rows.
- [x] Fable-authored revised diagram (#viz2) + hero flip on the main page
      (Extension <-> Sidecar / RDS). Fixed a CSS specificity bug where
      `#p-arch svg { display: block }` beat `#viz2 { display: none }` so both
      diagrams showed at once; scoped the flip rules under `#p-arch`.
- [x] Install tab (Extension | Sidecar) with the real install steps; the seg
      toggle also flips the hero diagram.
- [x] harness: `cargo build` + `cargo fmt --check` + `cargo clippy -D warnings`
      all clean.

## LLM endpoints (as of this session)

- `llm-host-a:8000` (the demo default vLLM): **DOWN** (host pings, but :8000
  refuses; no listener).
- `llm-host-a:11434`: Ollama, **UP**, OpenAI-compatible at /v1, ~20 models.
  The 30B (`qwen3-coder-30b-opt`) was too slow to finish a run promptly.
- `llm-host-b:8000`: vLLM, **UP + fast** (0.1s), `cyankiwi/gemma-4-26B-A4B-it-AWQ-4bit`,
  supports tool calls. This is the one that works.

## Environment issue: RESOLVED (Docker subnet collision)

- Symptom: containers reached the internet but NOT the LAN LLM
  (`llm-host-b/.193:8000`), so the in-container extension could not run
  agents. Host processes (harness, sidecar) reached the LLM fine.
- **Root cause (not a code bug): Docker subnet sprawl.** 25 Docker networks
  exhausted the `172.16.0.0/12` pool, so Docker's second default pool
  `<lan-range>/16` got carved into /20s that OVERLAPPED the real LAN
  (`<lan-range>/24`). An on-link bridge route to an overlapping subnet wins over
  the default route, so LAN-bound traffic (`192.168.1.x`) blackholed into a
  bridge. Internet still worked, which masked it.
- **Fix:** `docker network prune -f` (freed the 192.168 allocations) + set
  `default-address-pools` to `172.16.0.0/12 size 24` in the (rootless) docker
  daemon config, then restart docker. New nets now allocate from 172.x; a fresh
  container reaches `llm-host-b:8000` directly. Verified.
- Temporary workaround used before the root cause was found: a host-side TCP
  forwarder (`0.0.0.0:8500 -> llm-host-b:8000`) reachable from the container
  at `10.0.2.2:8500`. Removed after the real fix.

## Both paths verified working (post-fix)

- LOCAL (extension, `:8091` Talk to DB): `db_architect` created `public.customers`
  for real; `executions=1`, `messages=5` (full reasoning trace), table exists.
- SIDECAR (`:8091/sidecar`, via `/api/sidecar/execute` proxy): created `widgets`
  in rds. Both use `llm-host-b:8000` directly (no forwarder).

## Runs proven through the sidecar

- Registered profile `vllm -> llm-host-b:8000/v1` + agent `asst` via the admin
  API; seeded `public.orders` in rds; `POST /v1/execute` ran a real tool-calling
  loop (LLM on .133 + `sql_query` on remote rds) and returned the correct answer
  ("count is 2 and total is 420.5") in ~2.5s. tokens ~700/60.

## The "proxy" clarified + the run-via-sidecar path

- The proxy we first built (`/api/sidecar/probe`) is browser -> harness -> sidecar
  for the sidecar page's remote-PG READ probe. It does NOT route agent runs.
- The extension's `pg_synapse.sidecar_url` "forward execute() to a sidecar" is
  **defined but NOT implemented** (only a registered GUC; `execute()` never reads
  it). So there is no existing proxy that carries the extension's LLM traffic.
  (Fixed the Install-tab claim that said forwarding works.)
- `--network host` on the ext-db does NOT help here: the container still gets
  "No route to host" to the LAN. The docker network namespace in this env cannot
  reach the LAN in any mode; only host processes can.
- **Fix built:** added `POST /api/sidecar/execute` to the harness (host process)
  that forwards a run to the sidecar's `/v1/execute`, plus a "Run an agent" panel
  on `/sidecar`. Proven end to end: browser -> harness(:8091) -> sidecar(:8088)
  -> LLM(llm-host-b) + remote rds -> "2 rows, total 420.5" in ~1.7s.

## Bug: sidecar sql_query returns NULL for `numeric` columns

- `select amount from public.orders` via the sidecar returns `null` for a
  `numeric` column whose real value is `120.50`. `amount::text` -> "120.50" and
  `amount::float8` -> 120.5 serialize fine. So it's a numeric/decimal gap in the
  sidecar's sqlx -> JSON mapping (likely missing the sqlx `bigdecimal`/`rust_decimal`
  feature). Workaround in prompts/queries: cast numerics to text or float8.
  (Product bug in `crates/pg-synapse-sidecar/src/db.rs`, worth a real fix.)

## Open / TODO

- [ ] Build + run the sidecar as a Docker *image* via the compose file. Only the
      binary has been run against the `rds` container so far; the sidecar image
      build (`demo/sidecar/Dockerfile`, context = repo root) is unverified.
- [ ] Update stale `docs/sidecar-quickstart.md` ("not yet shipped").
- [ ] Consider `#[serde(deny_unknown_fields)]` on the sidecar admin request
      structs so typo'd fields fail loudly instead of silently dropping.
