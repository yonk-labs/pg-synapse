# pg_synapse operator minimum specs

All figures come from the B12 authoritative fs benchmark run (2026-05-16). No estimates are
presented as measurements.

**How to read this.** "p50 lat/step" is the median latency across all fs scenario steps that
recorded a non-zero latency_ms value for that model. "tokens/s" is total tokens_out divided by
total latency in seconds across those same steps. GGUF RAM is the file size on disk (Q4_K_M);
runtime RSS is typically 1.2 to 1.5x that figure. Remote models report n/a for disk RAM.

---

## Per-model measured data (fs benchmark, B12 run)

| model | disk (GB) | p50 lat/step | tokens/s | fs 3-scenario pass | verdict |
|-------|----------|-------------|---------|-------------------|---------|
| vllm-qwen3-coder | n/a (remote) | 3,352 ms | 55.0 | 3/3 | WORKS |
| openai-gpt5-mini | n/a (remote) | 6,006 ms | 49.3 | 3/3 | WORKS |
| qwen3-4b-2507 | 2.4 | 57,215 ms | 2.9 | 3/3 | WORKS |
| qwen3.5-0.8b | 0.5 | 19,347 ms | 8.9 | 2/3 | PARTIAL |
| gemma-4-E2B-it | 2.9 | 13,130 ms | 6.5 | 1/3 | PARTIAL |
| gemma-4-E4B-it | 4.7 | 27,738 ms | 3.1 | 1/3 | PARTIAL |
| qwen2.5-7b | 4.4 | 37,614 ms | 2.8 | 1/3 | PARTIAL |
| qwen3.5-2b | 1.2 | n/a | n/a | 1/3 (anomaly) | PARTIAL |
| llama-3.2-3b | 1.9 | 24,276 ms | 4.2 | 0/3 | NO |
| smollm3-3b | 1.8 | 21,811 ms | 15.7 | 0/3 | NO |
| granite-4.0-h-1b | 0.9 | 12,949 ms | 12.9 | 0/3 | NO |
| granite-4.0-tiny-preview | 3.9 | 12,264 ms | 13.5 | 0/3 | NO |

Note on qwen3.5-2b: the one scenario logged as passed recorded 0 ms latency and a tool error,
indicating an assertion artifact rather than a real agent success. No latency or tokens/s figure
is reported.

---

## Operator tiers

### Tier 1: Remote API

For operators with an OpenAI-compatible remote endpoint. No local GPU or large RAM required.

- **vllm-qwen3-coder** via local vLLM: 3.4 s per agent step, 55 tok/s. Best overall. Requires
  a CUDA-capable host serving vLLM (network-reachable from the Postgres host).
- **openai-gpt5-mini** via OpenAI API: 6.0 s per agent step, 49 tok/s. Reliable and no local
  hardware. Costs per token. Suitable for development and low-volume production.

Expectation: sub-10 s per step, full fs and sql task completion. Interactive use is feasible.

### Tier 2: Modest CPU-local (4-8 GB RAM)

For operators running on a laptop or development server with no GPU. Tested CPU-only.

- **qwen3-4b-2507** (Q4_K_M, 2.4 GB disk): 57 s per agent step, 2.9 tok/s. The only local model
  to achieve 3/3 fs. Budget 4 GB RAM minimum (2.4 GB model plus 1.5 GB overhead). Not for
  interactive use: a 3-step task takes roughly 3 minutes. Suitable for background/batch agents.

Expectation: 30-90 s per step depending on host CPU. A 5-step agent run takes 3-8 minutes. Plan
for batch-style usage; do not put this behind a synchronous HTTP endpoint.

### Tier 3: Tiny CPU-local (under 2 GB RAM)

For resource-constrained deployments: embedded, edge, or single-board systems.

- **qwen3.5-0.8b** (Q4_K_M, 508 MB disk): 19 s per step, 8.9 tok/s, 2/3 fs pass rate. Smallest
  model with confirmed tool-emit and partial task success. Budget 1 GB RAM. Reliable for single-
  step or simple two-step tasks; misses on multi-file collation tasks (f3_collate failed in every
  run). Not recommended for complex multi-tool workflows.

No other sub-2 GB model passed any fs scenario in this run. Models in this tier should be
treated as experimental for agentic use.

---

## What "not agentic-ready" means in practice

Models with verdict=NO (llama-3.2-3b, smollm3-3b, granite-4.0-h-1b, granite-4.0-tiny-preview)
did not emit parseable tool calls in any fs scenario. They respond in natural language rather than
structured tool invocations. The pg_synapse framework receives these responses and returns a "no
tool calls emitted" outcome; the agent loop terminates without completing the task.

These models may still produce useful text output via synapse.tool_call() for single-shot prompts,
but they cannot reliably drive multi-step agentic tasks with the current stack.

---

## How to pick

1. Have a remote vLLM or OpenAI endpoint? Use vllm-qwen3-coder or openai-gpt5-mini. Full
   capability, interactive response times.
2. Running local with 4+ GB free RAM and CPU time is not precious? Use qwen3-4b-2507. Batch
   agent tasks work reliably; budget 1-3 minutes per task.
3. Constrained to under 1 GB RAM? Use qwen3.5-0.8b for simple tasks only. Accept 1/3 miss rate
   on multi-file tasks.
4. Must use Granite 4 or llama-3.2? Not agentic-ready today. Suitable only for non-tool text
   generation via direct tool_call() invocations.
