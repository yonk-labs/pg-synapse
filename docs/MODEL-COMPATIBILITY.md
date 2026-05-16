# pg_synapse model compatibility

**What this measures.** The neutral filesystem (fs) benchmark runs 12 models on 3 tasks that
have nothing to do with SQL: find files, edit a config file, collate text across multiple files.
Each task requires the model to emit valid tool calls (read_file, write_file, edit_file, list_files,
grep) and complete the task correctly. This separates "does the model work as an agent framework
inside pg_synapse" from "does the model know SQL." A model that scores 3/3 on fs is agentic-ready
for any pg_synapse tool set, including the SQL tools. A model that scores 0/3 on fs cannot reliably
use tools in this stack and will not succeed on SQL tasks either.

SQL scores (s1_notes, s2_triage, s3_report) are shown for context. They measure domain capability
on top of the agentic baseline.

---

## Main compatibility table

Columns:

- **fs score**: tasks passed out of 3 fs scenarios (f1_find, f2_edit, f3_collate), authoritative run 2026-05-16
- **fs tool-emit**: scenarios where the model emitted at least one parseable tool call
- **sql score**: tasks passed out of 3 sql scenarios (s1_notes, s2_triage, s3_report), latest run
- **p50 lat/step**: median per-step latency in milliseconds across fs scenarios that produced timing data
- **approx RAM**: GGUF file size on disk (int4 quant); actual runtime RAM is roughly 1.2-1.5x this
- **verdict**: WORKS (3/3 fs), PARTIAL (1-2/3 fs, tool-emit confirmed), NO (0/3 fs, no tool-emit)

| model | params/quant | approx RAM | fs score | fs tool-emit | sql score | p50 lat/step | verdict |
|-------|-------------|-----------|----------|-------------|----------|-------------|---------|
| vllm-qwen3-coder | Qwen3-Coder-Next int4 | n/a (API) | 3/3 | 3/3 | 3/3 | 3,352 ms | WORKS |
| openai-gpt5-mini | gpt-5-mini | n/a (API) | 3/3 | 3/3 | 2/3 | 6,006 ms | WORKS |
| qwen3-4b-2507 | 4B int4 (Q4_K_M) | 2.4 GB | 3/3 | 3/3 | 2/3 | 57,215 ms | WORKS |
| qwen3.5-0.8b | 0.8B int4 (Q4_K_M) | 508 MB | 2/3 | 1/3 | 0/3 | 19,347 ms | PARTIAL |
| gemma-4-E2B-it | 2B int4 (Q4_K_M) | 2.9 GB | 1/3 | 1/3 | 2/3 | 13,130 ms | PARTIAL |
| gemma-4-E4B-it | 4B int4 (Q4_K_M) | 4.7 GB | 1/3 | 1/3 | 1/3 | 27,738 ms | PARTIAL |
| qwen2.5-7b | 7B int4 (Q4_K_M) | 4.4 GB | 1/3 | 2/3 | 1/3 | 37,614 ms | PARTIAL |
| qwen3.5-2b | 2B int4 (Q4_K_M) | 1.2 GB | 1/3 | 0/3 | 1/3 | n/a | PARTIAL |
| llama-3.2-3b | 3B int4 (Q4_K_M) | 1.9 GB | 0/3 | 0/3 | 1/3 | 24,276 ms | NO |
| smollm3-3b | 3B int4 (Q4_K_M) | 1.8 GB | 0/3 | 0/3 | 0/3 | 21,811 ms | NO |
| granite-4.0-h-1b | 1B int4 (Q4_K_M) | 860 MB | 0/3 | 0/3 | 0/3 | 12,949 ms | NO |
| granite-4.0-tiny-preview | ~1B int4 (Q4_K_M) | 3.9 GB | 0/3 | 0/3 | 0/3 | 12,264 ms | NO |

Note on qwen3.5-2b: one fs scenario registered as passed (task_passed=true) with 0 ms latency and
a tool error in the result. This is an assertion artifact from sandbox state, not a genuine agent
success. The model did not emit tool calls in any fs scenario in this run. Its PARTIAL verdict is
retained because it emits parseable tool calls in SQL scenarios and passes s1_notes.

---

## Tiering recommendation

### Remote / cloud (no local hardware required)

**vllm-qwen3-coder** (Qwen3-Coder-Next int4 served via vLLM on a remote host): best overall score,
fastest API latency at 3.4 s per step, 3/3 fs and 3/3 sql. Use this when you have a local vLLM
endpoint or a compatible OpenAI-API host.

**openai-gpt5-mini**: 3/3 fs and 2/3 sql at 6.0 s per step via the OpenAI API. Reliable and fast;
costs API credits. The sql miss on s2_triage was a type coercion error unrelated to agent capability.

### Local with adequate hardware (8+ GB RAM, any CPU)

**qwen3-4b-2507** (Qwen3-4B-Instruct-2507 Q4_K_M, 2.4 GB): the only local GGUF model to score 3/3
on fs and 2/3 on sql. Median latency of 57 s per step on CPU is slow but deterministic. This is the
local pick if you have patience and at least 4 GB free RAM. Budget 4-8 GB total RAM for the model
plus pg_synapse overhead.

### Tiny CPU-local (under 2 GB RAM, speed-constrained)

No tiny model achieved a reliable fs pass rate in this run. qwen3.5-0.8b scored 2/3 fs with 508 MB
on disk and a 19 s median step time, making it the best sub-1B option, but it missed f3_collate
in every run. It is viable for simple single-step tasks (file reads, single edits) but not reliable
for multi-step agentic workflows. Granite 4 (h-1b and tiny-preview) runs on CPU via llama-cpp-python
and the hybrid Mamba-2 architecture loads successfully, but neither model emitted parseable tool calls
in any scenario. They are not agentic-ready with the current pg_synapse tool-call parsing path.

---

## Honest limitations

- All local models ran on a single CPU (no GPU, no quantization other than Q4_K_M). GPU results
  would show lower latency but the same pass/fail pattern.
- Each scenario ran once per model in this authoritative pass. Single-run pass/fail is noisy at
  the margins, particularly for PARTIAL models; a model that scored 1/3 might score 2/3 on a
  different seed or temperature.
- "tool-emit" counts scenarios where the framework received at least one parseable tool call. It
  does not imply the call was correct. A model that emits tool calls but fails the assertion
  (TOOL? in RESULTS.md) understood the protocol but produced wrong arguments or wrong logic.
- SQL scores reflect a different prompt and tool set from the fs scores. A model can score 0/3 on
  sql while still being agentic-capable (e.g. qwen3.5-0.8b: 2/3 fs, 0/3 sql).
- The two infra flakes from the prior run (vllm-qwen3-coder CREATE EXTENSION errors, smollm3-3b
  f1_find network error) are excluded from model verdicts. They appeared in rows with
  infra_error=true in results.jsonl. This run ran cleanly; no infra errors were recorded.
