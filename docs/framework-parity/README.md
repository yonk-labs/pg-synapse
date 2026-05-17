# Framework parity: LangGraph, OpenAI Agents SDK, Google ADK vs pg_synapse

Honest assessment of replicating the three official agent-framework
quickstarts in pg_synapse. Per-framework detail: `langgraph.md`,
`openai-agents-sdk.md`, `google-adk.md`.

Provenance: the controller fetched the live docs
(docs.langchain.com/oss/python/langgraph/quickstart,
openai.github.io/openai-agents-python/quickstart, adk.dev/a2a/intro) and
verified the per-framework docs against them. The LangGraph and OpenAI
docs were drafted by subagents from training data (their WebFetch was
sandbox-blocked) then cross-checked against the live fetch and found
accurate; the ADK doc was authored by the controller with the A2A
section grounded in the live fetch.

## What replicates today

| Quickstart capability | pg_synapse | How |
|---|---|---|
| Tool-calling agent loop (LangGraph calculator, OpenAI single agent, ADK root_agent) | YES | ReAct/conversation executor + a Tool plugin + an agent row; `synapse.execute(agent, input)` |
| Typed function tools (`@tool`, `@function_tool`, plain Python fn) | YES | `#[derive(Tool)]` macro (schema from the struct) |
| Provider swap (LiteLLM model strings) | YES | provider plugins (openai/anthropic/llama-cpp) + `base_url` |
| Multi-agent routing (OpenAI triage handoffs, ADK orchestrator+children) | YES, via ONE new primitive | an agent-as-tool `call_agent` delegation tool that re-enters `Runtime::execute` on a named sub-agent |
| Execution-state persistence | PARTIAL | `synapse.executions`/`messages`/`traces` already log run state to Postgres; not mid-loop resumable |

## What does not replicate, honestly

| Capability | Why not |
|---|---|
| LangGraph explicit Graph API (`StateGraph`, nodes, conditional edges, Functional `@entrypoint`/`@task`) | Deliberate design difference, not a gap. pg_synapse's `Executor` trait is the alternative to a user-built node graph; the loop lives inside the executor. A custom `GraphExecutor` plugin is the forward path if graph semantics are wanted. |
| LangGraph checkpointing / `thread_id` mid-run resume | Genuine future feature. The quickstart page itself does not cover it; LangGraph's separate `langgraph-checkpoint-postgres` does. pg_synapse logs run state but cannot pause/resume mid-loop. Needs a new resumable Executor + a `synapse.checkpoints` table (column sketch in `langgraph.md`). This is the product-layer-relevant gap. |
| `adk create` CLI scaffold | Different surface, same outcome. pg_synapse scaffolds via SQL, not a project CLI. |
| Google A2A protocol (cross-service agent-to-agent) | Genuine unbuilt capability. pg_synapse ships an MCP client (agent-to-tool) but no A2A (agent-to-agent wire protocol). A future `pg-synapse-a2a` plugin (client + server, the way the MCP client is a plugin) maps onto a gateway-level intercept/governance model. v0.2+ design direction, not a quickstart-sized example. |

## The one high-leverage primitive

A single new tool unlocks the multi-agent patterns from BOTH the OpenAI
SDK (handoffs/triage) and Google ADK (orchestrator + children):

**`call_agent`** (ships as `pg-synapse-tools-delegate`, holds an
`Arc<Runtime>`): args `{agent, input}`; resolves the named agent, calls
`Runtime::execute(agent, input)`, returns its output as the tool result.
Recursion guard: a `delegation_depth` field on `ToolCtx` (default 0,
suggested max 4, optionally per-agent overridable), incremented per
nested call, rejected past the cap. This keeps multi-agent as
composition (a tool) rather than a kernel feature, preserving the
minimal six-trait design.

This is legitimate parity, not a workaround: the OpenAI SDK itself
exposes `agent.as_tool()` as a first-class equivalent to `handoffs=`,
and ADK sub-agent delegation is the same shape.

## Build plan (sequenced)

1. `pg-synapse-tools-delegate` (the `call_agent` tool) + `ToolCtx`
   depth-guard. Host-wired (pgrx + sidecar). Must follow the A4 email
   tool: both edit the same host-wiring files (root `Cargo.toml`, pgrx
   `runtime_holder.rs`, sidecar `main.rs`), so they cannot be built in
   parallel.
2. Replicable example scenarios driven by the existing benchmark
   harness: `lg_calc` (LangGraph calculator via ReAct + a calculator
   tool), `oai_triage` (OpenAI triage via `call_agent` across 3 agent
   rows), `adk_root` + `adk_orchestrator` (ADK root tool + delegation).
3. Recorded-not-built: the resumable-Executor + `synapse.checkpoints`
   design (LangGraph parity), and the `pg-synapse-a2a` plugin (ADK A2A
   parity). Both are real v0.2+ features, specified here so they are
   not mistaken for shipped behavior.
