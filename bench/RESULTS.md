# pg_synapse model benchmark leaderboard

Run date: 2026-05-16T12:41Z  Scale: 1

Cells: PASS/FAIL/TOOL? + tokens_in/tokens_out. TOOL? = model emitted tool calls but task assertion failed.

| model | s1_notes | s2_triage | s3_report | passed |
|-------|----------|-----------|-----------|--------|
| openai-gpt5-mini | PASS 943in/266out | FAIL(err) 0in/0out | PASS 1605in/287out | 2/3 |
| vllm-qwen3-coder | PASS 1554in/89out | PASS 5647in/647out | PASS 2545in/160out | 3/3 |
| gemma-4-E2B-it | FAIL 402in/47out | FAIL 545in/55out | FAIL 426in/39out | 0/3 |
| qwen3.5-2b | FAIL 713in/60out | FAIL 855in/70out | FAIL 737in/45out | 0/3 |
| qwen3.5-0.8b | FAIL 713in/57out | FAIL 855in/59out | FAIL 737in/56out | 0/3 |
| smollm3-3b | FAIL 209in/542out | FAIL 353in/649out | FAIL 235in/308out | 0/3 |

## Summary ranking

| rank | model | scenarios passed | total tokens |
|------|-------|-----------------|--------------|
| 1 | vllm-qwen3-coder | 3/3 | 10642 |
| 2 | openai-gpt5-mini | 2/3 | 3101 |
| 3 | gemma-4-E2B-it | 0/3 | 1514 |
| 4 | smollm3-3b | 0/3 | 2296 |
| 5 | qwen3.5-0.8b | 0/3 | 2477 |
| 6 | qwen3.5-2b | 0/3 | 2480 |

## Notes

- gemma-4-E2B-it: never emitted tool calls across all tested scenarios.
- qwen3.5-2b: never emitted tool calls across all tested scenarios.
- qwen3.5-0.8b: never emitted tool calls across all tested scenarios.
- smollm3-3b: never emitted tool calls across all tested scenarios.

