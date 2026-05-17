# LangGraph Quickstart: Mapping to pg_synapse

**Source:** https://langchain-ai.github.io/langgraph/tutorials/introduction/
(WebFetch was blocked during this run. Content is drawn from training-data knowledge of the public LangGraph quickstart, which has been stable and widely referenced. Treat any specific import paths as "verify against current docs" before citing them externally.)

**pg_synapse version:** v0.1 design spec (2026-05-13)

---

## 1. The LangGraph Quickstart: What It Actually Shows

The LangGraph quickstart builds a tool-calling calculator agent two ways: the explicit Graph API and the Functional API. The same underlying behavior in both cases: an LLM receives a user message, decides whether to call a tool, gets the tool result back, and continues until it produces a final text answer.

### 1.1 Tool Definition

```python
from langchain_core.tools import tool

@tool
def calculator(expression: str) -> str:
    """Evaluate a basic arithmetic expression."""
    return str(eval(expression))

tools = [calculator]
```

The `@tool` decorator wraps a plain Python function into a LangChain tool object with auto-generated JSON schema from the type annotations and docstring. The LLM receives that schema in its tool-use context.

### 1.2 Graph API (StateGraph / Explicit Nodes + Edges)

```python
from langgraph.graph import StateGraph, START, END
from langgraph.prebuilt import ToolNode
from langchain_core.messages import BaseMessage
from typing import Annotated
from typing_extensions import TypedDict
import operator

class State(TypedDict):
    messages: Annotated[list[BaseMessage], operator.add]

# Alternatively, the quickstart often uses the built-in shorthand:
# from langgraph.graph import MessagesState

llm_with_tools = llm.bind_tools(tools)

def call_model(state: State):
    response = llm_with_tools.invoke(state["messages"])
    return {"messages": [response]}

def should_continue(state: State):
    last = state["messages"][-1]
    if last.tool_calls:
        return "tools"
    return END

tool_node = ToolNode(tools)

graph = StateGraph(State)
graph.add_node("agent", call_model)
graph.add_node("tools", tool_node)
graph.add_edge(START, "agent")
graph.add_conditional_edges("agent", should_continue, {"tools": "tools", END: END})
graph.add_edge("tools", "agent")

app = graph.compile()
```

Key primitives: `StateGraph`, `MessagesState` (or a TypedDict with `Annotated[list, operator.add]`), `add_node`, `add_edge`, `add_conditional_edges`, `ToolNode`, `START`, `END`, `compile()`. The graph is a directed graph; `add_conditional_edges` is where the routing function lives. The compiled graph is the invokable object.

Invocation:

```python
result = app.invoke({"messages": [("user", "what is 3 * 7 + 2?")]})
# or stream:
for chunk in app.stream({"messages": [("user", "what is 3 * 7 + 2?")]}):
    print(chunk)
```

### 1.3 Functional API (@entrypoint / @task)

The Functional API was introduced as an alternative that feels more like writing ordinary async code:

```python
from langgraph.func import entrypoint, task

@task
def call_model(messages):
    return llm_with_tools.invoke(messages)

@task
def call_tools(tool_calls):
    return tool_node.invoke({"messages": tool_calls})

@entrypoint()
def agent(messages):
    response = call_model(messages).result()
    while response.tool_calls:
        tool_results = call_tools(response).result()
        messages = messages + [response] + tool_results
        response = call_model(messages).result()
    return response
```

`@entrypoint` marks the entry function; `@task` marks units that can be checkpointed and retried. The Functional API lowers boilerplate for simple loops at the cost of losing the visual graph DSL.

### 1.4 Checkpointing and Persistence

Both APIs accept a `checkpointer` at compile time:

```python
from langgraph.checkpoint.memory import MemorySaver

memory = MemorySaver()
app = graph.compile(checkpointer=memory)
```

Every invocation is keyed by a `thread_id` in the config dict:

```python
config = {"configurable": {"thread_id": "session-abc"}}
app.invoke({"messages": [("user", "...")]}, config=config)
```

The checkpointer snapshots the full graph state after every node. Threads are resumable: if the graph is interrupted (via a `human-in-the-loop` interrupt node or an error), calling `invoke` again with the same `thread_id` resumes from the last checkpoint. Persistent checkpointers (Postgres-backed: `langgraph-checkpoint-postgres`) replace `MemorySaver` for production, writing to a `checkpoints` table.

The quickstart also demonstrates `get_state(config)` and `update_state(config, ...)` for inspecting and patching mid-run state.

---

## 2. pg_synapse Mapping Table

| LangGraph Primitive | pg_synapse Equivalent | Verdict |
|---|---|---|
| `@tool` / `@tool` decorator | `#[derive(Tool)]` proc macro or manual `Tool` trait impl; registered in `ToolRegistry`; schema auto-derived | REPLICABLE |
| Tool-calling agent loop | `ReActExecutor`: issues one LLM turn, dispatches all `tool_calls` via `ToolRegistry`, loops until a terminal text response | REPLICABLE |
| `ToolNode` (batch tool dispatch) | `LoopHarness::dispatch_tool_call` called per tool call in the `ReActExecutor` loop | REPLICABLE |
| `call_model` node | `LoopHarness::one_llm_turn` (inside `ReActExecutor::execute`) | REPLICABLE |
| Routing function / `should_continue` | Implicit in `ReActExecutor`: `TurnResult::ToolCalls` continues, `TurnResult::AssistantText` terminates | REPLICABLE |
| `StateGraph` / `add_node` / `add_edge` / `add_conditional_edges` / `compile()` | No equivalent DSL. The `Executor` trait is the alternative abstraction: implement a struct, write the loop, register it. A custom `GraphExecutor` plugin is the forward path. | NOT REPLICABLE by design |
| `MessagesState` (append-only message list) | `ExecutionContext.messages` (internal to `LoopHarness`); not a user-visible typed state struct | PARTIAL (same semantics, not user-addressable) |
| `@entrypoint` / `@task` Functional API | No equivalent. Same mitigation: implement `Executor` directly. | NOT REPLICABLE by design |
| `checkpointer=MemorySaver()` at compile time | No equivalent. Execution state is ephemeral within a single `Executor::execute` call. | PARTIAL (see below) |
| `thread_id` resumable sessions | Not supported in v0.1. Each `synapse.execute()` call is a fresh run. | GAP |
| `MemorySaver` / Postgres checkpointer | `synapse.executions` + `synapse.messages` + `synapse.traces` persist complete run history, but these are write-once audit logs, not mid-loop checkpoints. | PARTIAL |
| `get_state` / `update_state` | No equivalent. Run state is not externally readable mid-loop. | GAP |
| `app.stream(...)` | Not in v0.1; noted as future via LISTEN/NOTIFY partials (design.md non-goals). | GAP |
| Cost cap / iteration cap | `ExecutionContext.cost_cap_usd` + `max_iterations`; checked each loop turn. LangGraph has no direct equivalent. | pg_synapse ONLY |
| SQL invocation surface | `SELECT synapse.execute('agent', 'input')` from any Postgres client. LangGraph has no equivalent. | pg_synapse ONLY |
| `tower::Layer` middleware | Composable cross-cutting concerns (cost, retry, rate-limit, tracing). LangGraph uses callback handlers instead. | pg_synapse ONLY |

---

## 3. The Calculator Agent in pg_synapse: Runnable Specification

This specifies the `bench/scenarios/lg_calc/` scenario precisely enough for a builder to implement it. Do not build it from this doc; this is the spec.

### 3.1 Tool: `calculator`

```sql
-- Register the tool (sidecar / pgrx SQL surface)
SELECT synapse.tool_register(
  'calculator',
  'Evaluate a basic arithmetic expression. Returns the numeric result as text.',
  '{
    "type": "object",
    "properties": {
      "expression": {
        "type": "string",
        "description": "The arithmetic expression to evaluate, e.g. \"3 * 7 + 2\""
      }
    },
    "required": ["expression"]
  }'::jsonb,
  'builtin'  -- implementation lives in Rust, registered at startup
);
```

The Rust implementation:

```rust
// plugins/pg-synapse-tools-calc/src/lib.rs  (new plugin crate)
#[derive(Tool)]
#[tool(
    name = "calculator",
    description = "Evaluate a basic arithmetic expression. Returns the numeric result as text."
)]
pub struct CalculatorTool;

#[async_trait]
impl CalculatorToolImpl for CalculatorTool {
    async fn run(&self, input: CalcInput, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        // four ops: +, -, *, /  (no eval(); parse explicitly)
        let result = evaluate_four_ops(&input.expression)?;
        Ok(ToolOutput::text(result.to_string()))
    }
}

#[derive(serde::Deserialize)]
struct CalcInput {
    expression: String,
}
```

The implementation should parse the expression for `+`, `-`, `*`, `/` only (no `eval()`; safe explicit parser). Division by zero returns a `ToolError::Execution` with a clear message.

### 3.2 Agent Row

```sql
SELECT synapse.agent_create(
  'lg_calc',                          -- name
  'You are a calculator assistant. Use the calculator tool to evaluate arithmetic expressions. Always show your reasoning before calling the tool.',
  'react',                            -- executor_name: ReActExecutor
  'openai-gpt4o-mini',                -- llm_profile_main
  ARRAY['calculator']                 -- tools
);
```

### 3.3 Invocation and Expected Tool Calls

```sql
SELECT synapse.execute('lg_calc', 'What is 3 * 7 + 2?');
```

Expected trace in `synapse.messages` (by `role` and key fields):

| seq | role | content / tool fields |
|---|---|---|
| 0 | system | "You are a calculator assistant..." + ReAct addendum |
| 1 | user | "What is 3 * 7 + 2?" |
| 2 | assistant | tool_call: `calculator({"expression": "3 * 7 + 2"})` |
| 3 | tool | tool_output: `{"result": "23"}` |
| 4 | assistant | "3 * 7 + 2 = 23." (final text) |

The `synapse.executions` row should show `status = 'completed'`, `tokens_in` and `tokens_out` populated, and `finished_at` set.

### 3.4 Scenario Fixture Files (bench/scenarios/lg_calc/)

```
meta.env:     KIND=calc; TOOLS=calculator; EXECUTOR=react
task.txt:     "What is 3 * 7 + 2?"
assertion.sql: SELECT (output LIKE '%23%') FROM synapse.executions
               WHERE agent_name = 'lg_calc' ORDER BY started_at DESC LIMIT 1;
```

---

## 4. Honest Why-Not: Graph DSL and Checkpointing

**Graph DSL.** pg_synapse's kernel is deliberately a six-trait minimal surface; it has no `StateGraph`, node registry, or conditional edge router. This is a design choice, not an oversight: every framework that has shipped a graph DSL (LangChain `AgentExecutor`, LangGraph, CrewAI flow graphs) has deprecated or significantly revised it within 24 months as usage patterns clarified. pg_synapse bets that the `Executor` trait (implement a struct, write a loop, register it) outlasts any specific DSL. A builder who wants an explicit graph abstraction can implement `GraphExecutor: Executor` as a plugin crate that owns its own node/edge/router types. That keeps the kernel stable while allowing the DSL to evolve externally.

**Checkpointing.** pg_synapse does persist run state: `synapse.executions`, `synapse.messages`, and `synapse.traces` are written at execution completion and are queryable from SQL. What is missing is mid-loop snapshotting and resume. To replicate LangGraph's `thread_id` resume contract, a future `CheckpointedExecutor` would need to: (1) write a checkpoint row after each tool dispatch, (2) accept a `resume_from` parameter that rehydrates the message list from a prior checkpoint, and (3) keep the loop iteration counter consistent. The table sketch below captures the minimal schema. This is a genuine gap in v0.1, explicitly named in the design spec's non-goals as a v0.2+ concern.

### Checkpoint Table Sketch (v0.2+ proposal)

```sql
CREATE TABLE synapse.checkpoints (
  checkpoint_id  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  thread_id      TEXT NOT NULL,            -- user-supplied session key
  execution_id   UUID REFERENCES synapse.executions(execution_id),
  seq            INT  NOT NULL,            -- loop iteration index
  messages_snap  JSONB NOT NULL,           -- full message list at this point
  tool_calls     JSONB,                    -- tool calls issued this turn
  status         TEXT NOT NULL,            -- 'active' | 'interrupted' | 'completed'
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (thread_id, seq)
);
```

Resume contract: `synapse.execute_resume(thread_id TEXT, new_input TEXT)` looks up the latest `active` checkpoint for `thread_id`, reconstructs `ExecutionContext.messages` from `messages_snap`, and calls `Executor::execute` with the hydrated context. The executor sees a warm message list and continues from where it left off.
