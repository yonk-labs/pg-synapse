# Framework Parity: OpenAI Agents SDK (Python) vs pg_synapse

**Source:** https://openai.github.io/openai-agents-python/quickstart/
**Note:** WebFetch was not available during authoring. This document draws on training-data knowledge of the OpenAI Agents SDK Python quickstart (the SDK was released March 2025, well within model knowledge). Where a detail is uncertain, it is flagged.

---

## 1. The Canonical Example

The quickstart introduces two primitives: `Agent` and `Runner`.

### Agent construction

```python
from agents import Agent, Runner

agent = Agent(
    name="Assistant",
    instructions="You are a helpful assistant.",
)
```

`Agent` accepts:

- `name` (str): display name used in traces and handoff routing.
- `instructions` (str): the system prompt.
- `tools` (list): a list of callable tool objects, usually decorated with `@function_tool`.
- `handoffs` (list): a list of other `Agent` instances this agent may transfer control to.
- `handoff_description` (str): a short description of what this agent does, used by a routing agent to decide whether to hand off to it. Only meaningful when the agent appears in another agent's `handoffs` list.
- `model` (str, optional): model name or a LiteLLM-style string (e.g. `"anthropic/claude-3-5-sonnet"`).

### Running an agent

```python
result = await Runner.run(agent, "Write a haiku about recursion.")
print(result.final_output)
```

`Runner.run` is async, accepts an agent and an input string, runs the full tool-call loop, and returns a `RunResult` whose `.final_output` is the last text message from the model.

### Tools via decorator

```python
from agents import function_tool

@function_tool
def get_weather(city: str) -> str:
    """Return the current weather for city."""
    return f"The weather in {city} is sunny."

agent = Agent(
    name="Weather Bot",
    instructions="Answer weather questions.",
    tools=[get_weather],
)
```

`@function_tool` (also `@function_tool` as a class import) inspects the function signature and docstring, auto-generates a JSON Schema for the LLM, and wraps the function as a tool object. No manual schema writing.

### Multi-agent triage (the canonical routing example)

```python
from agents import Agent, Runner

history_tutor = Agent(
    name="History Tutor",
    handoff_description="Specialist in historical questions.",
    instructions="You answer questions about history clearly and accurately.",
)

math_tutor = Agent(
    name="Math Tutor",
    handoff_description="Specialist in math questions.",
    instructions="You answer math questions with step-by-step reasoning.",
)

triage_agent = Agent(
    name="Triage Agent",
    instructions=(
        "Route the user's question to the appropriate specialist. "
        "Hand off to History Tutor for history questions, "
        "Math Tutor for math questions."
    ),
    handoffs=[history_tutor, math_tutor],
)

result = await Runner.run(triage_agent, "What caused the French Revolution?")
print(result.final_output)
```

The `Runner` sees the triage agent's `handoffs` list, exposes each sub-agent as a special handoff tool to the LLM (the SDK calls these `transfer_to_<agent_name>` tools automatically), and when the model calls one, the runner re-enters the loop with the target agent's instructions. The end user sees only the final output.

### `agent.as_tool()` variant

The SDK also supports composing agents without native handoffs:

```python
tool_agent = sub_agent.as_tool(
    tool_name="consult_expert",
    tool_description="Ask the expert for help.",
)
parent_agent = Agent(
    name="Parent",
    instructions="...",
    tools=[tool_agent],
)
```

`as_tool()` wraps an agent as an ordinary function tool: when the parent calls it, the SDK runs the sub-agent to completion and returns its output as the tool result. This is explicitly supported by the SDK as a first-class alternative to `handoffs=`.

### LiteLLM provider-agnosticism

The SDK accepts a `model` string in LiteLLM format:

```python
Agent(name="...", instructions="...", model="anthropic/claude-3-5-sonnet-20241022")
Agent(name="...", instructions="...", model="ollama/llama3")
```

No other code changes are needed to switch providers. The runner handles routing.

---

## 2. pg_synapse Mapping Table

| OpenAI Agents SDK concept | pg_synapse equivalent | Notes |
|---|---|---|
| `Agent(name=, instructions=, tools=[...])` | Row in `pg_synapse.agents` with `name`, `system_prompt`, and `tools` array | `instructions` maps directly to `system_prompt`; `tools` is an allowlist of registered tool names |
| `Runner.run(agent, input)` | `SELECT pg_synapse.execute('agent_name', 'input')` | Both run the full tool-call loop and return the final output; pg_synapse returns a JSONB envelope |
| `@function_tool` decorator | `#[derive(Tool)]` proc macro on a Rust struct | Both auto-derive the JSON Schema from the type definition; both register into a shared registry |
| `Agent.handoffs=[sub_agent_a, sub_agent_b]` | No native equivalent; use a `call_agent` tool that re-enters `Runtime::execute` | See delegation primitive spec below |
| `agent.as_tool(tool_name=, tool_description=)` | The `call_agent` tool is the direct equivalent: a tool that runs a named agent and returns its output | The SDK explicitly supports this pattern; pg_synapse's delegation-as-tool is not a workaround, it is parity with `as_tool()` |
| `handoff_description` | Agent row's `description` column (not yet in v0.1 schema) or encoded in `system_prompt` | No first-class routing field in v0.1; the triage agent's instructions carry the routing logic |
| `model="anthropic/..."` | `pg_synapse.llm_profiles` row with `provider = 'anthropic'`, `model = 'claude-3-5-sonnet'` | Switching providers means pointing the agent at a different named profile, not changing code |
| LiteLLM provider swap | `pg-synapse-provider-openai`, `pg-synapse-provider-anthropic`, `pg-synapse-provider-llama-cpp` + `base_url` field on profile | Three provider plugins ship in v0.1; same agent row, different profile name |
| `RunResult.final_output` | `ExecutorOutcome.output` field in the JSONB envelope | pg_synapse wraps in `{"output": "...", "tool_calls": [...], "cost_usd": ...}` |
| `Runner` sessions / conversation history | No native session state in v0.1; every `execute` is stateless | Durable memory via `stele-pg-synapse` plugin implements `MemoryProvider`; without it, history is not carried across executions |
| Streaming (`Runner.run_streamed`) | Not available in v0.1; planned as LISTEN/NOTIFY partials in v0.2 | `LlmProvider::stream` exists at the trait level but the SQL surface does not expose it yet |

---

## 3. The Delegation Primitive Spec (`call_agent` tool)

pg_synapse has no native handoff mechanism. The faithful equivalent is a `call_agent` tool that re-enters `Runtime::execute` on a named sub-agent and returns its output as a tool result. This matches exactly what the SDK calls `agent.as_tool()`.

**Tool name:** `call_agent`

**Input schema (JSON):**
```json
{
  "type": "object",
  "properties": {
    "agent":  { "type": "string", "description": "Name of the sub-agent to invoke." },
    "input":  { "type": "string", "description": "The input to pass to the sub-agent." }
  },
  "required": ["agent", "input"]
}
```

**Behavior:** resolve `agent` in `Runtime::agents`, call `Runtime::execute(agent, input)`, serialize `ExecutorOutcome.output` as a JSON string, return `ToolOutput::Text(result)` to the parent executor. Errors from the sub-agent surface as `ToolError::Execution`.

**Depth-guard concern:** without a guard, a triage agent that hands off to another agent that also has `call_agent` in its tool list can recurse arbitrarily. The kernel must enforce a cap.

**Enforcement mechanism:** `ToolCtx` already carries `execution_id` and `agent_name`. Add a `delegation_depth: u8` field (defaults to 0). `call_agent` reads `ctx.delegation_depth`, rejects the call with `ToolError::DepthExceeded` if `>= MAX_DELEGATION_DEPTH` (suggested default: 4), and passes `delegation_depth + 1` in the new `ToolCtx` it constructs for the sub-call. The cap is configurable per-agent via an `agent_row.max_delegation_depth` column (nullable; falls back to the global constant).

**Crate:** ships as `pg-synapse-tools-delegate` plugin, registering `call_agent` into the `ToolRegistry` via the standard `Plugin::register` path. The plugin takes a `Arc<Runtime>` at construction time; `call_agent.run()` borrows it to call `Runtime::execute`.

This spec is implementable-grade. Do not build it as part of this doc.

---

## 4. Runnable Plan: Triage Tutor Scenario

This specifies a future benchmark scenario at `bench/scenarios/oai_triage/` that replicates the OpenAI Agents SDK triage tutor example using pg_synapse primitives. Do not build it as part of this doc.

**Required artifacts:**

1. **Three agent rows** in `seed.sql`:
   - `math_tutor`: `executor=react`, `system_prompt="You answer math questions with step-by-step reasoning."`, `tools=[call_agent]` absent (leaf agent), `llm_profile_main='default'`.
   - `history_tutor`: same shape, `system_prompt="You answer history questions clearly and accurately."`.
   - `triage`: `executor=react`, `system_prompt="Route the user's question. For math questions call agent 'math_tutor'; for history questions call agent 'history_tutor'. Pass the full user input unchanged."`, `tools=['call_agent']`, `llm_profile_main='default'`.

2. **One plugin:** `pg-synapse-tools-delegate` registered at runtime, providing the `call_agent` tool.

3. **Entry point:** `SELECT pg_synapse.execute('triage', 'What is the derivative of x^2?');`

**Expected tool-call trace for a math question:**
```
triage: [call_agent(agent="math_tutor", input="What is the derivative of x^2?")]
  -> math_tutor: (no tool calls, direct answer)
     -> "The derivative of x^2 is 2x."
triage: final_output = "The derivative of x^2 is 2x."
```

**Scenario files:** `meta.env` (`KIND=sql`, `TOOLS=call_agent,sql_query`), `seed.sql`, `task.txt`, `system_prompt.txt` (empty, overridden by agent row), `assertion.sql` (checks that the output text contains "2x" and that exactly one `call_agent` tool call appears in the execution trace). Scales via `SCALE=1`.

---

## 5. Honest Why-Not

**What pg_synapse covers well:**

- Single-agent tool-calling loops: exact parity with `Runner.run` via `synapse.execute`.
- Tool authoring: `#[derive(Tool)]` is the Rust equivalent of `@function_tool`, including schema derivation.
- Provider switching: LLM profiles plus three provider plugins cover the LiteLLM use case.
- Delegation-as-tool: `call_agent` is a clean implementation of the SDK's own `agent.as_tool()` pattern. The routing behavior of `handoffs=` is reproduced without native handoff semantics.

**What pg_synapse does not have:**

- **Native handoff primitives:** The SDK's `handoffs=` list compiles sub-agents into a special class of tool with automatic `transfer_to_*` naming and runner-level state transfer. pg_synapse has no kernel concept of handoffs; routing is expressed purely through tool calls. This is an intentional kernel constraint (multi-agent crews are explicitly a non-goal in v0.1, deferred to `pg-synapse-crew` in v0.2+).
- **Streaming responses:** `Runner.run_streamed` delivers tokens as they arrive. pg_synapse has no streaming SQL surface in v0.1. The `LlmProvider::stream` trait method exists but is not wired to any SQL function. Planned via LISTEN/NOTIFY partials in v0.2.
- **Session continuity:** The SDK's `Runner` can thread conversation history across turns within a session. pg_synapse executions are stateless by default; continuity requires the `stele-pg-synapse` memory plugin, which is a separate integration.
- **Built-in tracing / guardrails:** The SDK ships a trace-viewer UI and input/output guardrails. pg_synapse exposes raw `executions.*` columns and defers observability to tower layers (user-authored, per the design spec). No built-in guardrail concept.
- **Handoff context preservation:** When the SDK transfers control between agents, the full conversation history is passed to the receiving agent. With delegation-as-tool, only the explicit `input` string is passed; the receiving agent starts fresh. Callers that need context continuity must serialize it into the `input` argument.

The kernel's minimal stance is deliberate: the six-trait surface + tower middleware is what makes pg_synapse outlast the 24-month half-life of agent framework APIs (noted in design.md, Section 4). The gaps above are not oversights; they are the cost of keeping the kernel on one screen.
