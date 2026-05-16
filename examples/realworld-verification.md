# Real-world verification (v0.1.1)

Overall verdict: **PASS with one honest model-capability limitation.**

Real systems only: live vLLM at `http://192.168.1.193:8000/v1`
(`Intel/Qwen3-Coder-Next-int4-AutoRound`), a real dockerized Postgres 17 on
a non-default port, a real `pg-synapse-sidecar` binary, and locally
downloaded Granite GGUF models served via `llama-cpp-python`. No mocks.

Two real bugs were found by this testing and fixed (see below). That is the
point of real-world testing: the mock suites were green and missed both.

---

## 1. Provider wiring into the pgrx host (defect fixed) - PASS

Before: only the OpenAI factory was registered in `runtime_holder.rs`, so
the llama-cpp and Anthropic plugins existed but were unreachable from SQL.

Fix: `runtime_holder.rs` now registers `LlamaCppProviderFactory`,
`LlamaCppEmbeddingFactory`, and `AnthropicProviderFactory` behind the
`provider-llama-cpp` / `provider-anthropic` features (default on). The
extension builds and installs with all of them:

```
cargo pgrx install --features pg17,embed-ort,provider-llama-cpp,provider-anthropic --no-default-features
  Discovered 21 SQL entities: 1 schemas, 18 functions ...
  Finished installing pg_synapse_pgrx
```

## 2. N2 SQL functions, live via vLLM through pgrx - PASS (bug found + fixed)

Bug found: `tool_call('sql_exec', ...)` failed with
`missing field 'statement'`. The `sql_exec` tool still expected a
`statement` field while `sql_query` used `query` and the N1.3-updated
prompts told the model to use `query` for both. The earlier agent demos
only worked because the LLM reads the tool JSON schema and self-corrected;
the direct `tool_call` path has no LLM to paper over it.

Fix: `SqlExecArgs.query` with `#[serde(alias = "statement")]` for
backward compatibility. Real output after the fix:

```
tool_call sql_query  -> [{"n": 2}]
tool_call sql_exec   -> {"rows_affected": 1}        (row landed: id 3 "via_toolcall")
agent_list           -> [{"name":"n2_agent","tools":["sql_query","sql_exec"],...}]
execute_async        -> b7bfd95c-c589-4c0f-a67c-66bf095741fd
execution_status     -> {"output":"There are 3 rows in the demo.notes table.",
                          "status":"completed","tokens_in":1245,"tokens_out":50}
```

The async agent actually ran the LLM, called `sql_query`, and answered
correctly. All five N2 surfaces verified live.

## 3. Sidecar end-to-end: docker Postgres + sidecar binary + live vLLM - PASS (bug found + fixed)

Bug found: the sidecar built its kernel Runtime once at startup. Admin
endpoints wrote rows to Postgres but never refreshed the in-memory Runtime,
so a freshly registered agent returned `agent 'sc_agent' not found` until a
process restart. The pgrx host avoids this by calling `rebuild_kernel()`
after admin writes; the sidecar had no equivalent. The mock oneshot tests
never exercised register-then-execute against a live Runtime, so they
missed it.

Fix: `AppState.runtime` is now `RwLock<Arc<Runtime>>`; admin handlers call
`rebuild_runtime()` after a successful write (mirrors pgrx
`rebuild_kernel`). Read paths take a cheap `Arc` snapshot.

Real run after the fix (no restart between register and execute):

```
health:  {"status":"ok"}
version: {"version":"0.1.1"}
admin without token -> HTTP 422 (rejected)
register llm profile -> {"name":"vllm","ok":true}
register agent       -> {"name":"sc_agent","ok":true}
POST /v1/execute "Add a note that says: sidecar e2e works"
  out: Note "sidecar e2e works" has been successfully added.
rows in docker Postgres:
   1 | seed a            | seed
   2 | seed b            | seed
   3 | sidecar e2e works | sidecar      <- inserted by the agent via the sidecar
POST /v1/execute "What notes are in the table?"
  out: lists all three rows correctly
```

The agent ran through the sidecar HTTP API, against live vLLM, and wrote a
real row into a real dockerized Postgres. Note: admin rejection returns 422
(missing-header extractor rejection) rather than 401; the security property
(no token, no admin access) holds, the status code is a cosmetic follow-up.
The `/v1/execute` response omits a `status` field that the pgrx envelope
includes; output and DB effect are correct. Both noted as v0.2 polish.

## 4. Local Granite, downloaded and served - PARTIAL (infra PASS, model limitation)

The explicit ask: download and try local Granite under 2B.

Tested two models, both downloaded from Hugging Face and served via
`llama-cpp-python`'s OpenAI-compatible server:

- `granite-3.1-1b-a400m-instruct` Q4_K_M (784 MB)
- `granite-3.1-2b-instruct` Q4_K_M (1.5 GB)

Infrastructure: PASS. Download, the llama-cpp-python server, the
`llama-cpp` provider, the pgrx host, and the kernel agent loop all worked
end-to-end. Plain completion works (sanity prompts returned correct text);
the agent loop ran to `status: completed` with token accounting.

Limitation (honest): neither small Granite model emitted a structured
OpenAI tool call through llama-cpp-python's default chat handler. Both
described the intended `sql_exec` call as prose / a markdown code block
instead of returning `tool_calls`, so no row was inserted (`tool_calls: []`,
table unchanged). This is a model plus serving-stack capability limit, not a
pg_synapse defect. The exact same pg_synapse code path tool-calls perfectly
when the LLM is vLLM serving a tool-tuned model with a tool parser.

Takeaway for operators: pg_synapse drives whatever the endpoint returns.
For agentic tool use with a local model, use a tool-call-tuned model and a
server configured with a matching tool-call parser (vLLM with
`--tool-call-parser`, or llama.cpp with the model's tool template). Small
Granite via the default llama-cpp-python handler is fine for plain
generation but not for structured tool calls.

---

## Honest scorecard

| Area | Verdict | Real evidence |
| --- | --- | --- |
| OpenAI provider vs live vLLM | PASS | prior runs + this one |
| pgrx SQL agent read/write (hardened host) | PASS | row inserted + read back, live |
| N2 SQL functions (tool_call, async, status, list) | PASS | live, after sql_exec fix |
| Provider wiring (llama-cpp, anthropic in pgrx) | PASS | builds + installs |
| Sidecar e2e (docker PG + vLLM) | PASS | row inserted via HTTP, after rebuild fix |
| Local Granite infrastructure | PASS | downloaded, served, agent loop ran |
| Local Granite agentic tool-calling | LIMITED | small model does not emit tool calls |
| Anthropic real API | NOT TESTED | no Anthropic endpoint/key available; mock-only, stated plainly |

Two real defects (sql_exec field mismatch, sidecar stale-Runtime) were
caught only by this real-world testing and are fixed and committed.
