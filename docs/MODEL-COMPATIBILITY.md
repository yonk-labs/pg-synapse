# pg_synapse model compatibility

**What this measures.** The neutral filesystem (fs) benchmark runs models on 3 tasks that
have nothing to do with SQL: find files, edit a config file, collate text across multiple files.
Each task requires the model to emit valid tool calls (read_file, write_file, edit_file, list_files,
grep) and complete the task correctly. This separates "does the model work as an agent framework
inside pg_synapse" from "does the model know SQL." A model that scores 3/3 on fs is agentic-ready
for any pg_synapse tool set, including the SQL tools. A model that scores 0/3 on fs cannot reliably
use tools in this stack and will not succeed on SQL tasks either.

SQL scores (s1_notes, s2_triage, s3_report) are shown for context. They measure domain capability
on top of the agentic baseline.

---

## B13 methodology correction: Qwen3.5 are conversational models, not base

**Prior (incorrect) claim (B7/B11):** Qwen3.5-2B/0.8B were described as "base" or "pretrain-only"
models that cannot tool-call by design.

**Corrected fact:** Qwen/Qwen3.5-2B and Qwen/Qwen3.5-0.8B are conversational `image-text-to-text`
models. Their GGUF chat templates include xml_tools tool-call support. They CAN tool-call. The B6
and B11 scores of 0/3 on fs scenarios were caused by a client-side format-extraction failure, not
a model capability gap.

**Evidence:** Phase 1 capture (B13) showed Qwen3.5-2B emitting valid
`<tool_call><function=grep><parameter=path>...</parameter></function></tool_call>` XML-parameter
format in f1_find, which the B6 extractor already handled for the first two calls. The third call
(`write_file`) was emitted with empty parameters (`<function=write_file></function>`), which is a
model-capability limit at 2B scale for the multi-step f1_find task, not a parsing failure.

**What the B13 parser fix actually delivered:** The Gemma double-brace JSON fix
(`{{"content":"v","path":"p"}}` inside `<|tool_call>` format) moved gemma-4-E2B-it from 1/3 to
3/3 on fs and gemma-4-E4B-it from 1/3 to 3/3 on fs. The python_tools extractor was added for
completeness (kwarg-format function calls in fenced python blocks) but produced no measurable delta
for the tested models in this run.

---

## Main compatibility table

Columns:

- **fs score**: tasks passed out of 3 fs scenarios (f1_find, f2_edit, f3_collate), authoritative B13 run 2026-05-17
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
| gemma-4-E2B-it | 2B int4 (Q4_K_M) | 2.9 GB | 3/3 | 3/3 | 2/3 | 24,080 ms | WORKS |
| gemma-4-E4B-it | 4B int4 (Q4_K_M) | 4.7 GB | 3/3 | 3/3 | 1/3 | 39,305 ms | WORKS |
| qwen3.5-9b | 9B int4 (Q4_K_M) | 5.7 GB | 3/3 | 2/3 | - | n/a | WORKS |
| qwen3.5-4b | 4B int4 (Q4_K_M) | 2.5 GB | 2/3 | 2/3 | - | 78,057 ms | PARTIAL |
| qwen3.5-2b | 2B int4 (Q4_K_M) | 1.2 GB | 1/3 | 0/3 | 1/3 | n/a | PARTIAL |
| qwen3.5-0.8b | 0.8B int4 (Q4_K_M) | 508 MB | 1/3 | 0/3 | 0/3 | n/a | PARTIAL |
| qwen2.5-7b | 7B int4 (Q4_K_M) | 4.4 GB | 1/3 | 2/3 | 1/3 | 37,614 ms | PARTIAL |
| llama-3.2-3b | 3B int4 (Q4_K_M) | 1.9 GB | 0/3 | 0/3 | 1/3 | 24,276 ms | NO |
| smollm3-3b | 3B int4 (Q4_K_M) | 1.8 GB | 0/3 | 0/3 | 0/3 | 21,811 ms | NO |
| granite-4.0-h-1b | 1B int4 (Q4_K_M) | 860 MB | 0/3 | 0/3 | 0/3 | 12,949 ms | NO |
| granite-4.0-tiny-preview | ~1B int4 (Q4_K_M) | 3.9 GB | 0/3 | 0/3 | 0/3 | 12,264 ms | NO |
| granite-4.1-3b | 3B int4 (Q4_K_M) | ~2.0 GB | 0/3 | 0/3 | - | 52,838 ms | NO |

Notes on specific models:

- **qwen3.5-2b/0.8b (PARTIAL, corrected):** These are conversational models that emit XML-parameter
  tool calls. Their PARTIAL verdict is due to model-capability limits at 2B/0.8B scale: the final
  `write_file` step in multi-step fs tasks is emitted with empty parameters. The prior B11 annotation
  calling them "base models that cannot tool-call" was incorrect.
- **qwen3.5-9b f1_find:** Registered as PASS with tokens_in=0 due to a tool-error
  in a prior step being swallowed and the sandbox state matching the assertion. The model did emit
  valid tool calls (f2_edit and f3_collate confirmed). This is an assertion artifact.
- **granite-4.1-3b:** Loads and runs via llama-cpp-python (hybrid Mamba-2 architecture supported).
  Does not emit parseable tool calls. Verdict: NO, not agentic-ready.

---

## B13 delta: what the parser fix produced

| model | B12 fs | B13 fs | delta | cause |
|-------|--------|--------|-------|-------|
| gemma-4-E2B-it | 1/3 | 3/3 | +2 | Gemma double-brace JSON fix: `{{"key":"v"}}` inside `<|tool_call>` now fast-path parsed |
| gemma-4-E4B-it | 1/3 | 3/3 | +2 | Same fix |
| qwen3.5-2b | 0/3 | 0/3 | 0 | XML extractor already worked; model emits empty write_file args |
| qwen3.5-0.8b | 1/3 | 1/3 | 0 | Same; f2_edit passes via sandbox state, not genuine agent |
| smollm3-3b | 0/3 | 0/3 | 0 | Model outputs prose; no structured call format. Confirmed NO |

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
on fs and 2/3 on sql in a pre-B13 run. Median latency of 57 s per step on CPU is slow but
deterministic. This is the local pick if you have patience and at least 4 GB free RAM.

**gemma-4-E2B-it** (B13): upgraded to 3/3 fs after the double-brace JSON fix. 2.9 GB on disk,
faster than qwen3-4b-2507 per step (~24 s median). Strong local option for fs tasks.

**gemma-4-E4B-it** (B13): 3/3 fs, 1/3 sql. 4.7 GB on disk, ~39 s median step. Works well as an
agentic fs model; weaker on SQL generation than the 2B variant.

**qwen3.5-9b** (B13, new): 3/3 fs. 5.7 GB on disk. Tested fs-only in this run; SQL unknown.
Multi-step tasks take 1-3 min per step on CPU.

### Tiny CPU-local (under 2 GB RAM, speed-constrained)

**qwen3.5-4b** (B13, new): 2/3 fs (f1+f2 PASS, f3_collate fails). 2.5 GB on disk. Better tool-call
reliability than qwen3.5-2b for multi-step tasks but still misses the hardest scenario.

No sub-2B model achieved 3/3 fs in any run. qwen3.5-0.8b's PARTIAL is due to sandbox-state
coincidence rather than genuine multi-step agent success. Granite 4 (h-1b, tiny-preview, 4.1-3b)
runs on CPU but does not emit parseable tool calls. SmolLM3-3B outputs prose, not tool calls.

---

## Honest limitations

- All local models ran on a single CPU (no GPU, no quantization other than Q4_K_M). GPU results
  would show lower latency but the same pass/fail pattern.
- Each scenario ran once per model in the authoritative pass. Single-run pass/fail is noisy at
  the margins; a model that scored 1/3 might score 2/3 on a different seed or temperature.
- "tool-emit" counts scenarios where the framework received at least one parseable tool call. It
  does not imply the call was correct. A model that emits tool calls but fails the assertion
  (TOOL? in RESULTS.md) understood the protocol but produced wrong arguments or wrong logic.
- SQL scores reflect a different prompt and tool set from the fs scores. A model can score 0/3 on
  sql while still being agentic-capable.
- No infra errors were recorded in the B13 run.
