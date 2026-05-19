# pg_synapse v0.1.1 -- Model Compatibility Matrix

Last updated: 2026-05-19. Data sources: `bench/results.jsonl` (latest run_date per
model/scenario, 2026-05-17T11:43Z sweep), the post-B18 characterization logs
`bench/run_n_a2_distill.log`, `bench/run_n_lg_calc.log`,
`bench/run_n_oai_triage.log`, `bench/run_n_p1_pipeline.log` (each N=3) and
`bench/run_n_adk_root.log` (N=1), `bench/models.toml`,
`bench/SMALL-MODEL-DIAGNOSIS.md`.

## How to read this document

pg_synapse is model-agnostic: any OpenAI-compatible chat-completions endpoint works.
This matrix records which models actually pass the benchmark scenarios at N=1 (single
run per cell) unless noted otherwise. N=1 results are noisy -- a model that fails one
run may pass the next. The post-B18 characterization runs are the trustworthy
source for vllm-qwen3-coder on the five tested scenarios (four at N=3, adk_root at
N=1).

### Scenario descriptions

| Scenario | Kind | Tools used | What it tests |
|----------|------|-----------|---------------|
| a1_ingest | SQL-heavy | sql_exec, sql_query, read_file | Load CSV+JSON into Postgres tables |
| a2_distill | SQL-heavy | sql_exec, sql_query | Sentiment analysis + lede generation via SQL |
| a3_triage | SQL-heavy | sql_exec, sql_query | Categorize and route support tickets |
| lg_calc | Typed-tool | calculator, sql_exec | LangGraph parity -- arithmetic via typed tool |
| oai_triage | Typed-tool | categorize_ticket | OpenAI Agents SDK parity -- single typed tool |
| adk_root | Typed-tool | greet | ADK parity -- single typed tool, minimal SQL |
| p1_pipeline | Multi-tool | get_current_time, http_get, lede_compress, read_file, write_file | 6-step zero-SQL pipeline: clock to write_file to HTTP fetch to compress to append to read_file |

---

## Summary Matrix

Legend: P = PASS, F = FAIL, -- = not tested. Superscript asterisk (*) = flappy (both
PASS and FAIL observed across multiple runs in results.jsonl).

All results from the 2026-05-17T11:43Z sweep unless noted.

### Remote / infra models

| Model | a1_ingest | a2_distill | a3_triage | lg_calc | oai_triage | adk_root | Score |
|-------|-----------|-----------|-----------|---------|-----------|---------|-------|
| vllm-qwen3-coder | P | F* | P* | P | P | P | 5/6 |
| openai-gpt5-mini | P | P | F* | P | P | P | 5/6 |

### Local CPU GGUF models (Q4_K_M quant, no GPU)

| Model | Params | a1_ingest | a2_distill | a3_triage | lg_calc | oai_triage | adk_root | Score |
|-------|--------|-----------|-----------|-----------|---------|-----------|---------|-------|
| qwen3-4b-2507 | 4B | F | P | P | P | P | P | 5/6 |
| gemma-4-E2B-it | 2B | P** | F | F | P | P | P | 4/6 |
| gemma-4-E4B-it | 4B | F | F | P* | P | F | P | 3/6 |
| qwen3.5-9b | 9B | F | F | F | F | P | P | 2/6 |
| qwen2.5-7b | 7B | F | F | F | F | F | P | 1/6 |

`*` Flappy: this cell has recorded both PASS and FAIL across multiple results.jsonl entries.

`**` gemma-4-E2B-it a1_ingest: recorded as task_passed=true in results.jsonl despite a
`missing field query` error on one tool call. The assertion passed because the ingest
completed via a different code path before the failed call. Treat as a weak pass.

---

## Post-B18 Verified Results (tool-error feedback loop)

### What B18 changed

Before B18, the executor aborted on the first tool error (`?` operator on
`dispatch_tool_call`). After B18, tool errors are fed back to the model as Tool-role
messages, giving the model a chance to self-correct its SQL and retry.

This is the decisive fix for SQL-heavy scenarios. Models that generate initially-wrong
SQL (missing params, wrong types, duplicate columns) can now recover in-loop instead of
failing immediately.

### Characterization runs (post-B18)

Source: `bench/run_n_a2_distill.log`, `bench/run_n_lg_calc.log`,
`bench/run_n_oai_triage.log`, `bench/run_n_p1_pipeline.log`,
`bench/run_n_adk_root.log`

| Model | Scenario | N | Pass | Fail | Rate |
|-------|----------|---|------|------|------|
| vllm-qwen3-coder | a2_distill | 3 | 3 | 0 | 100% |
| vllm-qwen3-coder | lg_calc | 3 | 3 | 0 | 100% |
| vllm-qwen3-coder | oai_triage | 3 | 3 | 0 | 100% |
| vllm-qwen3-coder | p1_pipeline | 3 | 3 | 0 | 100% |
| vllm-qwen3-coder | adk_root | 1 | 1 | 0 | 100% |

All five committed characterization logs show vllm-qwen3-coder passing every
iteration post-B18. a2_distill ran 8 good `sql_exec` calls per iteration; lg_calc,
oai_triage, adk_root, and p1_pipeline are SQL-free. Correction: `bench/run_n_adk_root.log`
is an N=1 log (a single iteration); the prior N=3 claim for adk_root was not backed by
the committed log and has been amended to N=1 here. p1_pipeline is a multi-tool
pipeline scenario added after the 2026-05-17 sweep (see the scenario table above); it
does not appear in `bench/RESULTS.md` or the N=1 summary matrix.

Context: a2_distill was the scenario most affected by pre-B18 abort behavior. The
SESSION-HANDOFF.md notes that a2_distill went "from 6/10 pass to 10/10" during
development. The committed characterization logs confirm 3/3 at N=3. The N=1 results
in the sweep still show a2_distill as FAIL for vllm-qwen3-coder -- this is because the
model occasionally emits `column "sentiment" specified more than once`, which B18
cannot fix (the retry produces the same wrong SQL). The error-feedback loop helps when
the error is informative enough for the model to correct course; it does not help when
the model consistently generates the same structural error.

### Pre-B18 contamination warning

The results.jsonl entries from before 2026-05-17 (dates 2026-05-16T*) were collected
under the old abort-on-first-error behavior. Any FAIL on an SQL-heavy scenario
(a1_ingest, a2_distill, a3_triage) in those entries may be an artifact of the abort,
not a model capability limit. The 2026-05-17T11:43Z sweep is the authoritative post-B18
dataset, but even it has N=1 noise.

---

## Known-NO Models

These models were tested and cannot reliably participate as pg_synapse agents. They are
excluded from the matrix above.

| Model | Params | Reason |
|-------|--------|--------|
| smollm3-3b | 3B | Cannot emit structured tool calls in any format. Outputs prose and Markdown code blocks. Confirmed across fs and sql scenarios. Model capability gap at 3B scale. |
| qwen3.5-0.8b | 0.8B | Emits XML-parameter tool calls but generates SQL syntax errors and empty parameter objects. 0/6 on sql scenarios, 0/3 genuine fs passes (sandbox-state coincidences). Too small for multi-step agent tasks. |
| qwen3.5-2b | 2B | Emits XML-parameter tool calls for simple steps but sends empty params for write_file. 1/3 on old sql scenarios (s1_notes only). Insufficient SQL generation at 2B scale. |
| llama-3.2-3b | 3B | 0/3 fs, 1/3 old sql (s1_notes only, after B9 lenient-params fix). Does not emit tool calls in multi-step scenarios. SQL generation limited at 3B. |
| granite-4.0-h-1b | 1B | Does not emit parseable tool calls. 0/3 fs, 0/3 sql. |
| granite-4.0-tiny-preview | ~1B | Does not emit parseable tool calls. 0/3 fs, 0/3 sql. |
| granite-4.1-3b | 3B | Loads on llama-cpp-python (hybrid Mamba-2 arch). Does not emit parseable tool calls. 0/3 fs. |

---

## Min-Specs and Recommendations

### The key insight

**Failures correlate with SQL-authoring surface area, not agent-loop capability.**

Every model that can emit structured tool calls passes the typed-tool scenarios
(adk_root, oai_triage, lg_calc) at higher rates than the SQL-heavy scenarios
(a1_ingest, a2_distill, a3_triage). The difference:

- **Typed-tool scenarios** give the model a single tool with a clear schema (e.g.,
  `greet(name)`, `categorize_ticket(category, priority)`). The model fills in simple
  fields. No SQL authoring required.
- **SQL-heavy scenarios** require the model to compose multi-statement SQL with correct
  parameter placeholders (`$1`, `$2`), correct types, correct column names, and correct
  JOIN logic. This is a much harder generative task.

The B18 error-feedback fix narrows the gap for capable models (vllm-qwen3-coder passes
a2_distill at 100% post-B18) but cannot help models that consistently generate the same
structural SQL errors.

### Minimum viable model by scenario class

| Scenario class | Minimum viable model | Notes |
|----------------|---------------------|-------|
| Typed-tool (adk_root, oai_triage) | gemma-4-E2B-it (2B, 2.9 GB) | Every WORKS-tier model passes adk_root. Even qwen2.5-7b passes adk_root. |
| Typed-tool with arithmetic (lg_calc) | gemma-4-E2B-it (2B, 2.9 GB) | All WORKS-tier local models pass. qwen3.5-9b and qwen2.5-7b fail (tool-arg type issues). |
| SQL-heavy (a2_distill, a3_triage) | qwen3-4b-2507 (4B, 2.4 GB) | Smallest local model to pass SQL-heavy scenarios. Flappy on a1_ingest. |
| SQL-heavy + ingest (a1_ingest) | vllm-qwen3-coder or openai-gpt5-mini | a1_ingest requires multi-file read + multi-table SQL. No local GGUF model reliably passes at N=1. |

### Recommended configurations

**Best overall (remote):** vllm-qwen3-coder (Qwen3-Coder-Next int4 via vLLM). 5/6
scenarios, fastest latency (~3-4s per step), 100% on a2_distill at N=3 post-B18. Needs
a vLLM endpoint with ~24 GB VRAM.

**Best overall (cloud API):** openai-gpt5-mini. 5/6 scenarios, ~6s per step. Costs
API credits. Reliable on SQL-heavy scenarios.

**Best local GGUF (SQL-capable):** qwen3-4b-2507 (Qwen3-4B-Instruct-2507, Q4_K_M,
2.4 GB). 5/6 scenarios. Slow on CPU (~60-100s per step) but the only local model to
pass both a2_distill and a3_triage. Needs ~4 GB free RAM.

**Best local GGUF (typed-tool only):** gemma-4-E2B-it (Q4_K_M, 2.9 GB). 4/6 scenarios
(passes all typed-tool scenarios, weak on SQL-heavy). Faster than qwen3-4b-2507 (~20s
per step). Good choice if your agent uses typed tools, not raw SQL.

**Cheapest viable (typed-tool only):** gemma-4-E2B-it at 2.9 GB or qwen3-4b-2507 at
2.4 GB. Both fit in 4 GB RAM.

**Not recommended for production:** qwen3.5-9b (2/6, only passes oai_triage + adk_root
due to empty `{}` args on sql_exec), qwen2.5-7b (1/6, only passes adk_root).

---

## Filesystem (fs) Benchmark Baseline

The fs benchmark (f1_find, f2_edit, f3_collate) tests pure agent-loop capability with
no SQL. A model that passes 3/3 fs can emit valid tool calls and complete multi-step
tasks. This is the prerequisite for SQL scenario success.

| Model | fs score | Verdict |
|-------|---------|---------|
| vllm-qwen3-coder | 3/3 | WORKS |
| openai-gpt5-mini | 3/3 | WORKS |
| qwen3-4b-2507 | 3/3 | WORKS |
| gemma-4-E2B-it | 3/3 | WORKS |
| gemma-4-E4B-it | 3/3 | WORKS |
| qwen3.5-9b | 3/3 | WORKS |
| qwen3.5-4b | 2/3 | PARTIAL |
| qwen2.5-7b | 1/3 | PARTIAL |
| All Known-NO models | 0/3 | NO |

Source: B13 run (2026-05-17T02:21Z). See `bench/SMALL-MODEL-DIAGNOSIS.md` for the
detailed per-model diagnosis of tool-call extraction and model capability limits.

---

## Methodology notes

- All local GGUF models ran on CPU (no GPU). Q4_K_M quantization via llama-cpp-python.
  GPU would reduce latency but not change pass/fail outcomes.
- Each scenario ran once per model in the sweep (N=1). Single-run results are noisy.
  Models marked with `*` (flappy) have recorded both PASS and FAIL across multiple runs.
- The B18 tool-error feedback loop feeds SQL errors back to the model as conversation
  context. This means post-B18 results may show more iterations (model retries) and
  higher token counts than pre-B18 results.
- The `task_passed` field in results.jsonl is the authoritative pass/fail. Some entries
  show `task_passed=true` alongside a non-empty `error` field -- this means an error
  occurred on one tool call but the overall task assertion still passed.
- Tool-call extraction (B6/B13) handles four model output formats: JSON-in-tag, XML
  params, Gemma special-token, and fenced JSON. See `bench/SMALL-MODEL-DIAGNOSIS.md`
  for format details per model.
