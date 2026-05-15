# Demo: a SQL agent powered by a local llama.cpp server

## What this shows

An agent registered via `synapse.agent_create(...)` is asked, from SQL, to
manage a small `demo.tasks` table. It uses the built-in `sql_query` and
`sql_exec` tools to read and write rows. The LLM backend is a local
`llama-server` process (llama.cpp) running the
`granite-3.0-2b-instruct-Q4_K_M` GGUF. The llama.cpp server exposes an
OpenAI-compatible API, so the `llama-cpp` provider speaks to it without any
translation layer beyond the provider adapter.

See `EXAMPLE_OUTPUT.md` for the expected output structure (template only until
you run on a host with llama-server installed).

## Prerequisites

- `cargo-pgrx` 0.18+ (`cargo install cargo-pgrx --version =0.18.0 --locked`)
- `cargo pgrx init --pg17 download` run at least once
- The extension installed into the pgrx pg17 tree: run
  `cargo pgrx run pg17` from the repo root once, then `\q`
- `curl` (for GGUF download)
- `python3` (for free-port detection)
- `llama-server` on PATH (script SKIPs gracefully if absent)

### Installing llama-server

| Platform | Command |
| -------- | ------- |
| macOS (Homebrew) | `brew install llama.cpp` |
| Linux (pre-built) | download from https://github.com/ggml-org/llama.cpp/releases |
| From source | `git clone https://github.com/ggml-org/llama.cpp && cd llama.cpp && cmake -B build && cmake --build build -j $(nproc)` |

After building from source the binary is at `build/bin/llama-server`. Add it
to your `PATH` or symlink it.

## Running

```bash
cd $(git rev-parse --show-toplevel)
bash examples/local-llama-agent/run.sh
```

`run.sh` is fully repeatable:

1. If `llama-server` is not on PATH, it prints install hints and exits 0 (SKIP).
2. Downloads the Granite GGUF into `~/.cache/pg-synapse/models/` if not already present.
3. Starts `llama-server` on a free port.
4. Starts and connects to the pgrx-managed Postgres 17.
5. Drops and recreates `pg_synapse_demo` (idempotent).
6. `CREATE EXTENSION pg_synapse_pgrx;`
7. Applies `seed.sql` (creates `demo.tasks` with 2 seed rows).
8. Applies `workflow.sql` (registers the `llama-cpp` profile + `task_agent`,
   calls `synapse.execute` twice).
9. Prints final `demo.tasks`, `synapse.executions`, `synapse.messages`.
10. Asserts `demo.tasks` grew by at least one row.
11. Kills `llama-server` (trap on exit).

## Configuration env vars

| Var | Default | Purpose |
| --- | ------- | ------- |
| `LLAMA_HF_REPO` | `lmstudio-community/granite-3.0-2b-instruct-GGUF` | HuggingFace repo slug |
| `LLAMA_HF_FILE` | `granite-3.0-2b-instruct-Q4_K_M.gguf` | GGUF filename within the repo |
| `LLAMA_HF_REV` | `main` | Git revision / branch on HuggingFace |
| `LLAMA_MODEL_CACHE` | `~/.cache/pg-synapse/models` | Local model cache root |
| `PGRX_PG_VERSION` | `17` | pgrx-managed Postgres version |
| `PGRX_PORT` | `28817` | TCP port of the pgrx-managed Postgres |
| `PGRX_HOST` | `${HOME}/.pgrx` | Unix-socket directory pgrx uses |
| `DEMO_DB` | `pg_synapse_demo` | Database to drop and recreate |

## Manual variant

```bash
cargo pgrx run pg17
```

Inside the psql prompt:

```sql
CREATE EXTENSION pg_synapse_pgrx;
\i examples/local-llama-agent/seed.sql
\i examples/local-llama-agent/workflow.sql
SELECT * FROM demo.tasks ORDER BY id;
SELECT * FROM synapse.executions ORDER BY started_at;
SELECT * FROM synapse.messages ORDER BY execution_id, seq;
```

Note: when running manually you must start `llama-server` yourself and
configure the `base_url` in `workflow.sql` to match.

## GGUF coordinates

The primary model download target is:

- Repo: `lmstudio-community/granite-3.0-2b-instruct-GGUF`
- File: `granite-3.0-2b-instruct-Q4_K_M.gguf`
- URL: `https://huggingface.co/lmstudio-community/granite-3.0-2b-instruct-GGUF/resolve/main/granite-3.0-2b-instruct-Q4_K_M.gguf`

If that URL becomes unavailable, override with:

```bash
LLAMA_HF_REPO=bartowski/granite-3.1-2b-instruct-GGUF \
LLAMA_HF_FILE=granite-3.1-2b-instruct-Q4_K_M.gguf \
bash examples/local-llama-agent/run.sh
```

## Common failure modes

| Symptom | Likely cause |
| ------- | ------------ |
| `SKIP: llama-server not found on PATH` | Install llama.cpp (see above) |
| `llama-server did not become ready in 30 s` | Model too large for available RAM; try a smaller quant |
| `psql: could not connect to server` | pgrx Postgres not started; run `cargo pgrx start pg17` |
| `extension "pg_synapse_pgrx" is not available` | Run `cargo pgrx run pg17` once to install |
| `status: errored` in the execute envelope | llama-server lost or model did not tool-call; check `/tmp/llama-server-demo.log` |
| `FAILURE: demo.tasks did not grow` | Model lacks tool-call support at this quant; try a larger quant or a different model |
