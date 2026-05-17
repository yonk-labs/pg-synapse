# pg_synapse model benchmark leaderboard

Run date: 2026-05-17T11:43Z  Scale: 1

Cells: PASS/FAIL/TOOL? + tokens_in/tokens_out. TOOL? = model emitted tool calls but task assertion failed.

| model | s1_notes | s2_triage | s3_report | f1_find | f2_edit | f3_collate | a1_ingest | a2_distill | a3_triage | lg_calc | oai_triage | adk_root | passed |
|-------|----------|-----------|-----------|---------|---------|------------|-----------|------------|-----------|---------|------------|----------|--------|
| openai-gpt5-mini | PASS 943in/266out | FAIL(err) 0in/0out | PASS 1605in/287out | PASS 2933in/147out | PASS 1692in/259out | PASS 5299in/627out | PASS 7604in/945out | PASS 15479in/1556out | FAIL(err) 0in/0out | PASS 2675in/516out | PASS 1669in/181out | PASS 1748in/709out | 10/12 |
| vllm-qwen3-coder | PASS 1554in/89out | PASS 5647in/647out | PASS 2545in/160out | PASS 4372in/181out | PASS 2557in/129out | PASS 4886in/297out | PASS 11139in/585out | FAIL(err) 0in/0out | PASS 24256in/1093out | PASS 1994in/176out | PASS 2552in/133out | PASS 2598in/228out | 11/12 |
| gemma-4-E2B-it | PASS 866in/69out | FAIL(err) 0in/0out | PASS 1487in/119out | PASS 2526in/118out | PASS 1386in/86out | PASS 4838in/231out | PASS(err) 0in/0out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | PASS 2259in/87out | PASS 1590in/87out | PASS 1613in/42out | 9/12 |
| qwen3.5-2b | PASS 1465in/61out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | PASS(err) 0in/0out | FAIL(err) 0in/0out | - | - | - | - | - | - | 2/12 |
| qwen3.5-0.8b | FAIL(err) 0in/0out | FAIL(err) 0in/0out | TOOL? 2348in/172out | FAIL(err) 0in/0out | PASS(err) 0in/0out | FAIL(err) 0in/0out | - | - | - | - | - | - | 1/12 |
| smollm3-3b | FAIL 209in/542out | FAIL 353in/649out | FAIL 235in/308out | FAIL 284in/437out | FAIL 236in/329out | FAIL 351in/330out | - | - | - | - | - | - | 0/12 |
| gemma-4-E4B-it | PASS 866in/74out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | PASS 2529in/119out | PASS 1386in/86out | PASS 4845in/261out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | PASS 20487in/556out | PASS 2255in/112out | FAIL 459in/29out | PASS 1613in/42out | 7/12 |
| llama-3.2-3b | PASS 1371in/63out | FAIL 795in/168out | FAIL 677in/106out | FAIL 943in/109out | FAIL 716in/77out | FAIL 957in/130out | - | - | - | - | - | - | 1/12 |
| qwen3-4b-2507 | PASS 1215in/63out | PASS 2714in/316out | FAIL(err) 0in/0out | PASS 3814in/157out | PASS 2140in/129out | PASS 6650in/266out | TOOL? 9919in/501out | PASS 4383in/579out | PASS 5353in/548out | PASS 1599in/136out | PASS 2171in/189out | PASS 2196in/98out | 10/12 |
| qwen2.5-7b | PASS 1226in/64out | TOOL? 1607in/341out | TOOL? 1284in/103out | FAIL(err) 0in/0out | PASS 1419in/128out | TOOL? 1842in/74out | FAIL(err) 0in/0out | TOOL? 2437in/692out | TOOL? 3095in/462out | FAIL(err) 0in/0out | TOOL? 1322in/87out | PASS 2219in/73out | 3/12 |
| granite-4.0-h-1b | - | - | - | FAIL 963in/53out | FAIL 759in/41out | FAIL 1003in/243out | - | - | - | - | - | - | 0/12 |
| granite-4.0-tiny-preview | - | - | - | FAIL 940in/94out | FAIL 702in/94out | FAIL 970in/368out | - | - | - | - | - | - | 0/12 |
| qwen3.5-4b | - | - | - | PASS 4184in/227out | PASS 2454in/150out | FAIL(err) 0in/0out | - | - | - | - | - | - | 2/12 |
| qwen3.5-9b | - | - | - | PASS(err) 0in/0out | PASS 2454in/141out | PASS 4668in/477out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | FAIL(err) 0in/0out | PASS 2486in/158out | PASS 2579in/143out | 5/12 |
| granite-4.1-3b | - | - | - | FAIL 959in/265out | FAIL 757in/172out | FAIL 999in/71out | - | - | - | - | - | - | 0/12 |

## Summary ranking

| rank | model | scenarios passed | total tokens |
|------|-------|-----------------|--------------|
| 1 | vllm-qwen3-coder | 11/12 | 67818 |
| 2 | qwen3-4b-2507 | 10/12 | 45136 |
| 3 | openai-gpt5-mini | 10/12 | 47140 |
| 4 | gemma-4-E2B-it | 9/12 | 17404 |
| 5 | gemma-4-E4B-it | 7/12 | 35719 |
| 6 | qwen3.5-9b | 5/12 | 13106 |
| 7 | qwen2.5-7b | 3/12 | 18475 |
| 8 | qwen3.5-2b | 2/12 | 1526 |
| 9 | qwen3.5-4b | 2/12 | 7015 |
| 10 | qwen3.5-0.8b | 1/12 | 2520 |
| 11 | llama-3.2-3b | 1/12 | 6112 |
| 12 | granite-4.0-h-1b | 0/12 | 3062 |
| 13 | granite-4.0-tiny-preview | 0/12 | 3168 |
| 14 | granite-4.1-3b | 0/12 | 3223 |
| 15 | smollm3-3b | 0/12 | 4263 |

## Notes

- smollm3-3b: never emitted tool calls across all tested scenarios.
- granite-4.0-h-1b: never emitted tool calls across all tested scenarios.
- granite-4.0-tiny-preview: never emitted tool calls across all tested scenarios.
- granite-4.1-3b: never emitted tool calls across all tested scenarios.

