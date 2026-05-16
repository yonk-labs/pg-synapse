# pg_synapse model benchmark leaderboard

Run date: 2026-05-16T19:05Z  Scale: 1

Cells: PASS/FAIL/TOOL? + tokens_in/tokens_out. TOOL? = model emitted tool calls but task assertion failed.

| model | s1_notes | s2_triage | s3_report | f1_find | f2_edit | f3_collate | passed |
|-------|----------|-----------|-----------|---------|---------|------------|--------|
| openai-gpt5-mini | PASS 943in/266out | FAIL(err) 0in/0out | PASS 1605in/287out | - | - | - | 2/6 |
| vllm-qwen3-coder | PASS 1554in/89out | PASS 5647in/647out | PASS 2545in/160out | PASS 3900in/204out | PASS 2263in/133out | PASS 4454in/273out | 6/6 |
| gemma-4-E2B-it | PASS 866in/69out | FAIL(err) 0in/0out | PASS 1487in/119out | - | - | - | 2/6 |
| qwen3.5-2b | PASS 1465in/61out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | - | - | - | 1/6 |
| qwen3.5-0.8b | FAIL(err) 0in/0out | FAIL(err) 0in/0out | TOOL? 2348in/172out | - | - | - | 0/6 |
| smollm3-3b | FAIL 209in/542out | FAIL 353in/649out | FAIL 235in/308out | - | - | - | 0/6 |
| gemma-4-E4B-it | PASS 866in/74out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | - | - | - | 1/6 |
| llama-3.2-3b | PASS 1371in/63out | FAIL 795in/168out | FAIL 677in/106out | - | - | - | 1/6 |
| qwen3-4b-2507 | PASS 1215in/63out | PASS 2714in/316out | FAIL(err) 0in/0out | - | - | - | 2/6 |
| qwen2.5-7b | PASS 1226in/64out | TOOL? 1607in/341out | TOOL? 1284in/103out | - | - | - | 1/6 |

## Summary ranking

| rank | model | scenarios passed | total tokens |
|------|-------|-----------------|--------------|
| 1 | vllm-qwen3-coder | 6/6 | 21869 |
| 2 | gemma-4-E2B-it | 2/6 | 2541 |
| 3 | openai-gpt5-mini | 2/6 | 3101 |
| 4 | qwen3-4b-2507 | 2/6 | 4308 |
| 5 | gemma-4-E4B-it | 1/6 | 940 |
| 6 | qwen3.5-2b | 1/6 | 1526 |
| 7 | llama-3.2-3b | 1/6 | 3180 |
| 8 | qwen2.5-7b | 1/6 | 4625 |
| 9 | smollm3-3b | 0/6 | 2296 |
| 10 | qwen3.5-0.8b | 0/6 | 2520 |

## Notes

- smollm3-3b: never emitted tool calls across all tested scenarios.

