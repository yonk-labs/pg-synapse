# Examples

For a complete catalog of all six benchmark agents (with schemas, sample code,
and expected outcomes), see `docs/AGENTS-GUIDE.md`. For the reactive trigger
demos (queue mode and inline mode), see `examples/reactive-triggers/`.

Three repeatable, end-to-end workflows for pg_synapse v0.1. Each directory has
its own `README.md` (full prerequisites and caveats) and an
`EXAMPLE_OUTPUT.md` (a captured run with timings, tokens, and the message
log). Every example is idempotent: it drops and recreates its demo database.

## Shared prerequisites

- A reachable OpenAI-compatible LLM endpoint that supports tool calls.
  Default: `http://192.168.1.193:8000/v1` (vLLM serving
  `Intel/Qwen3-Coder-Next-int4-AutoRound`). Override with
  `PG_SYNAPSE_LLM_BASE_URL` / `PG_SYNAPSE_LLM_MODEL`.
- `cargo-pgrx` 0.18 and a pgrx-managed Postgres 17
  (`cargo pgrx init --pg17 download`), with `pg_synapse_pgrx` installed into
  that tree.
- For `with-local-embeddings` only: an ONNX Runtime 1.24.x shared library on
  the library path (set `ORT_DYLIB_PATH`, or `pip install onnxruntime`), and
  the BGE-small model cached at
  `~/.cache/pg-synapse/models/BAAI/bge-small-en-v1.5/`.

## The examples

| Example | What it shows | Run |
| --- | --- | --- |
| [`sql-agent-readwrite/`](./sql-agent-readwrite/) | The canonical demo: an agent reads and writes a `demo.notes` table via `sql_query` / `sql_exec` tool calls | `bash examples/sql-agent-readwrite/run.sh` |
| [`customer-support-triage/`](./customer-support-triage/) | A multi-step agent that classifies and routes support tickets, joins customer tier, and updates rows | `bash examples/customer-support-triage/run.sh` |
| [`with-local-embeddings/`](./with-local-embeddings/) | Local BGE embeddings via `synapse.embed()` plus cosine similarity search in pure SQL | `bash examples/with-local-embeddings/run.sh` |

Run each from the repository root. `run.sh` starts the pgrx Postgres if
needed, recreates the demo database, `CREATE EXTENSION pg_synapse_pgrx`,
applies `seed.sql` then `workflow.sql`, prints the result tables, and exits
non-zero if the expected outcome did not occur.

The Docker-based harness (`run.sh --docker`) is deferred to v0.1.x and
currently exits with an explanatory message.
