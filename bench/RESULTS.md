# pg_synapse model benchmark leaderboard

Run date: 2026-05-16T16:00Z  Scale: 1

Cells: PASS/FAIL/TOOL? + tokens_in/tokens_out. TOOL? = model emitted tool calls but task assertion failed.

| model | s1_notes | s2_triage | s3_report | passed |
|-------|----------|-----------|-----------|--------|
| openai-gpt5-mini | PASS 943in/266out | FAIL(err) 0in/0out | PASS 1605in/287out | 2/3 |
| vllm-qwen3-coder | PASS 1554in/89out | PASS 5647in/647out | PASS 2545in/160out | 3/3 |
| gemma-4-E2B-it | PASS 866in/69out | FAIL(err) 0in/0out | PASS 1487in/119out | 2/3 |
| qwen3.5-2b | PASS 1465in/61out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | 1/3 |
| qwen3.5-0.8b | FAIL(err) 0in/0out | FAIL(err) 0in/0out | TOOL? 2348in/172out | 0/3 |
| smollm3-3b | FAIL 209in/542out | FAIL 353in/649out | FAIL 235in/308out | 0/3 |

## Summary ranking

| rank | model | scenarios passed | total tokens |
|------|-------|-----------------|--------------|
| 1 | vllm-qwen3-coder | 3/3 | 10642 |
| 2 | gemma-4-E2B-it | 2/3 | 2541 |
| 3 | openai-gpt5-mini | 2/3 | 3101 |
| 4 | qwen3.5-2b | 1/3 | 1526 |
| 5 | smollm3-3b | 0/3 | 2296 |
| 6 | qwen3.5-0.8b | 0/3 | 2520 |

## Notes

**B6 diagnosis (2026-05-16):** All 4 small models previously scored 0/3 because the llama-cpp-python server (default config) does not post-process model output into the `tool_calls` JSON array. Instead, models emit raw tool call syntax inside `content`. This is a serving-stack gap, not model incapability for the models that understand tool use.

Fix applied: client-side extraction fallback in `pg-synapse-provider-llama-cpp` and `pg-synapse-provider-openai` (`extract_tool_calls_from_content`). Three formats are now parsed from `content`: (1) `<tool_call>{"name":...,"arguments":{...}}</tool_call>` (Qwen3.5 native), (2) `<tool_call><function=NAME><parameter=KEY>value</parameter></function></tool_call>` (Qwen3 XML params), (3) Gemma-4 special-token format `<|tool_call>call:NAME{...}<tool_call|>`.

After fix, scores:
- gemma-4-E2B-it: 0/3 to 2/3 (s1_notes PASS, s3_report PASS; s2_triage fails on tool input validation due to the model hallucinating a params format).
- qwen3.5-2b: 0/3 to 1/3 (s1_notes PASS; s2 and s3 fail on SQL correctness or params format).
- qwen3.5-0.8b: 0/3 to 0/3, but now emits parseable tool calls (s3_report TOOL?); fails on SQL correctness.
- smollm3-3b: 0/3 to 0/3. This model does not emit tool calls in any parseable format at 3B scale. It produces reasoning text and Markdown code blocks with Python-style function call syntax rather than JSON. Raw evidence: see bench/SMALL-MODEL-DIAGNOSIS.md.

Remaining failures in qwen3.5-2b and qwen3.5-0.8b are SQL correctness issues (wrong query syntax) from small-model capability limits, not serving-stack configuration.

- smollm3-3b: never emitted tool calls in any parseable format across all tested scenarios.

