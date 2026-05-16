# pg_synapse model benchmark

Purpose: measure how well different LLMs drive pg_synapse agents, with the
key metric being whether a model emits valid structured tool calls (the
thing small local models fail at) and whether the agent task actually
completes (deterministic DB assertion).

## Models under test

| key | kind | coordinates |
| --- | --- | --- |
| openai-gpt5-mini | remote_openai | api.openai.com, model `gpt-5-mini`, key from `.openai` |
| vllm-qwen3-coder | remote_openai | http://192.168.1.193:8000/v1 (baseline, proven) |
| gemma-4-E2B-it | local_gguf | unsloth/gemma-4-E2B-it-GGUF :: gemma-4-E2B-it-Q4_K_M.gguf |
| qwen3.5-2b | local_gguf | unsloth/Qwen3.5-2B-GGUF :: Qwen3.5-2B-Q4_K_M.gguf |
| qwen3.5-0.8b | local_gguf | unsloth/Qwen3.5-0.8B-GGUF :: Qwen3.5-0.8B-Q4_K_M.gguf |
| smollm3-3b | local_gguf | unsloth/SmolLM3-3B-GGUF :: SmolLM3-3B-Q4_K_M.gguf |

## Scenarios (scalable via SCALE env)

- S1 notes-crud (easy): add a note, then list. 1 to 2 tool calls.
- S2 support-triage (medium): classify + update N tickets. Scale = ticket count.
- S3 analytics-report (hard): answer a question needing a JOIN + GROUP BY,
  then write the finding back. Scale = data volume.

## Scoring per (model, scenario)

- tool_emitted: model produced a structured tool_call (0/1)
- task_passed: deterministic post-condition on the DB / answer (0/1)
- tokens_in, tokens_out, latency_ms, iterations, error (if any)

Output: bench/RESULTS.md leaderboard, regenerable, resumable.
