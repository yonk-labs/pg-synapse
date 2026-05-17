# pg_synapse model benchmark leaderboard

Run date: 2026-05-17T02:21Z  Scale: 1

Cells: PASS/FAIL/TOOL? + tokens_in/tokens_out. TOOL? = model emitted tool calls but task assertion failed.

| model | s1_notes | s2_triage | s3_report | f1_find | f2_edit | f3_collate | passed |
|-------|----------|-----------|-----------|---------|---------|------------|--------|
| openai-gpt5-mini | PASS 943in/266out | FAIL(err) 0in/0out | PASS 1605in/287out | PASS 2933in/147out | PASS 1692in/259out | PASS 5299in/627out | 5/6 |
| vllm-qwen3-coder | PASS 1554in/89out | PASS 5647in/647out | PASS 2545in/160out | PASS 4372in/181out | PASS 2557in/129out | PASS 4886in/297out | 6/6 |
| gemma-4-E2B-it | PASS 866in/69out | FAIL(err) 0in/0out | PASS 1487in/119out | PASS 2526in/118out | PASS 1386in/86out | PASS 4838in/231out | 5/6 |
| qwen3.5-2b | PASS 1465in/61out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | PASS(err) 0in/0out | FAIL(err) 0in/0out | 2/6 |
| qwen3.5-0.8b | FAIL(err) 0in/0out | FAIL(err) 0in/0out | TOOL? 2348in/172out | FAIL(err) 0in/0out | PASS(err) 0in/0out | FAIL(err) 0in/0out | 1/6 |
| smollm3-3b | FAIL 209in/542out | FAIL 353in/649out | FAIL 235in/308out | FAIL 284in/437out | FAIL 236in/329out | FAIL 351in/330out | 0/6 |
| gemma-4-E4B-it | PASS 866in/74out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | PASS 2529in/119out | PASS 1386in/86out | PASS 4845in/261out | 4/6 |
| llama-3.2-3b | PASS 1371in/63out | FAIL 795in/168out | FAIL 677in/106out | FAIL 943in/109out | FAIL 716in/77out | FAIL 957in/130out | 1/6 |
| qwen3-4b-2507 | PASS 1215in/63out | PASS 2714in/316out | FAIL(err) 0in/0out | PASS 3814in/157out | PASS 2140in/129out | PASS 6650in/266out | 5/6 |
| qwen2.5-7b | PASS 1226in/64out | TOOL? 1607in/341out | TOOL? 1284in/103out | FAIL(err) 0in/0out | PASS 1419in/128out | TOOL? 1842in/74out | 2/6 |
| granite-4.0-h-1b | - | - | - | FAIL 963in/53out | FAIL 759in/41out | FAIL 1003in/243out | 0/6 |
| granite-4.0-tiny-preview | - | - | - | FAIL 940in/94out | FAIL 702in/94out | FAIL 970in/368out | 0/6 |
| qwen3.5-4b | - | - | - | PASS 4184in/227out | PASS 2454in/150out | FAIL(err) 0in/0out | 2/6 |
| qwen3.5-9b | - | - | - | PASS(err) 0in/0out | PASS 2454in/141out | PASS 4668in/477out | 3/6 |
| granite-4.1-3b | - | - | - | FAIL 959in/265out | FAIL 757in/172out | FAIL 999in/71out | 0/6 |

## Summary ranking

| rank | model | scenarios passed | total tokens |
|------|-------|-----------------|--------------|
| 1 | vllm-qwen3-coder | 6/6 | 23064 |
| 2 | gemma-4-E2B-it | 5/6 | 11726 |
| 3 | openai-gpt5-mini | 5/6 | 14058 |
| 4 | qwen3-4b-2507 | 5/6 | 17464 |
| 5 | gemma-4-E4B-it | 4/6 | 10166 |
| 6 | qwen3.5-9b | 3/6 | 7740 |
| 7 | qwen3.5-2b | 2/6 | 1526 |
| 8 | qwen3.5-4b | 2/6 | 7015 |
| 9 | qwen2.5-7b | 2/6 | 8088 |
| 10 | qwen3.5-0.8b | 1/6 | 2520 |
| 11 | llama-3.2-3b | 1/6 | 6112 |
| 12 | granite-4.0-h-1b | 0/6 | 3062 |
| 13 | granite-4.0-tiny-preview | 0/6 | 3168 |
| 14 | granite-4.1-3b | 0/6 | 3223 |
| 15 | smollm3-3b | 0/6 | 4263 |

## Notes

- smollm3-3b: never emitted tool calls across all tested scenarios.
- granite-4.0-h-1b: never emitted tool calls across all tested scenarios.
- granite-4.0-tiny-preview: never emitted tool calls across all tested scenarios.
- granite-4.1-3b: never emitted tool calls across all tested scenarios.

