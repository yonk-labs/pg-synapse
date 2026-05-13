# Commercial + Non-Rust Agent Framework Landscape (2026-05)

Cross-reference for a Rust agent-runtime crate design. Public sources only. Clean-room — borrow patterns, not implementations.

---

## Summary table

| Framework | Lang | License / Pricing | Top abstraction | Best lesson |
|-----------|------|-------------------|-----------------|-------------|
| LangGraph | Python / JS | MIT (OSS) + LangSmith/LangGraph Platform paid | `StateGraph` + nodes + edges over a typed shared state | Stateful agent = checkpointed reducer over a typed state object; classic `AgentExecutor` was deprecated in favor of this |
| LlamaIndex Workflows 1.0 | Python / TS | MIT (OSS) + LlamaCloud | `@step`-decorated handlers + typed events + shared `Context` | Event-driven step graph inferred from type signatures — no explicit edges |
| Mastra | TypeScript | Elastic v2 / SSPL-like (OSS) + Mastra Cloud | `createAgent` / `createTool` / `createWorkflow` / `createMemory` | Composable primitives, Zod-typed tools, suspend/resume workflow as separate primitive from agent loop |
| CrewAI | Python | MIT (OSS) + CrewAI Enterprise | `Agent(role,goal,backstory)` + `Task` + `Crew(process=...)` | Role/task/crew metaphor maps to human teams; three explicit processes (sequential / hierarchical / consensual) |
| AutoGen → Microsoft Agent Framework | Python / .NET | MIT (OSS), in maintenance | Agents as conversational participants; MAF adds graph workflows | Agents-as-conversation is a viable orchestration primitive; MAF unifies it with SK's enterprise features |
| Semantic Kernel → Microsoft Agent Framework | C# / Python / Java | MIT (OSS) | `Agent` + `Tool` (formerly Kernel + Plugin) + middleware + telemetry | Plugin/MCP/OpenAPI as four tool sources behind one interface; middleware pipeline for cross-cutting concerns |
| Vercel AI SDK | TypeScript | Apache-2.0 (OSS) | `streamText({model, tools, ...})` + `Agent` + `ToolLoopAgent` (v6) | Inline tool definitions next to the call; streaming-first; same primitives in server and client |
| Spring AI | Java | Apache-2.0 (OSS) | `ChatClient` + `ChatMemory` + `VectorStore` + `Tools`/`MCP` + Agent Skills | Vendor-neutral abstractions per concern; A2A + MCP for interop; Agent Skills = discoverable folders of capability |
| DSPy | Python | MIT (OSS, Stanford NLP) | `Signature` + `Module` (e.g. `dspy.ReAct`) + `Optimizer` (e.g. MIPROv2) | Three-tier abstraction: declare I/O contract, compose modules, optimize prompts/weights from data + metric |
| OpenAI Assistants API (deprecated) | HTTP | Per-token + per-tool-call | `Assistant` + `Thread` + `Run` + `tool_outputs` | `requires_action` lifecycle, 10-minute run expiry — cautionary tale for opaque hosted runtimes |
| OpenAI Responses API + Agents SDK | HTTP + Python | Per-token + per-tool-call; SDK MIT | `Agent` + `tools` + `handoffs` + `guardrails` | Handoffs-as-tools, input/output/tool guardrails as first-class layers |
| Anthropic Claude Agent SDK + Managed Agents | Python / TS (SDK) + HTTP | Per-token + $0.08/session-hour for Managed Agents | `query()` / `ClaudeSDKClient` + hooks + custom-tool-as-in-process-MCP + memory tool | Hooks at deterministic loop points; tools as in-process MCP servers; memory as mounted directory |
| AWS Bedrock Agents / AgentCore | HTTP (AWS) | Token costs + infra (10-30% overhead) | `Agent` + `ActionGroup` (OpenAPI) + `KnowledgeBase` + managed orchestration | Action groups defined by OpenAPI schemas; "custom orchestration" Lambda escape hatch |
| Google Vertex AI Agent Builder (Gemini Enterprise) | Python (ADK) + HTTP | Per vCPU-hour + per GB-hour + token + per-query search | ADK (code-first) + Agent Studio (visual) + Agent Engine (managed runtime) | Code-and-low-code coexistence; pay separately for compute, memory events, retrieval, model |
| Langfuse | (observability) | Free 50K events/mo; Cloud $50/mo; Enterprise $2,499/mo+; self-host free | Trace + observation + score | Self-hostable trace store with strict unit-based pricing |
| LangSmith | (observability) | Free 5K traces/mo; $39/user/mo+ | Trace + run tree + evaluators | Run-tree shape; deep LangChain/LangGraph integration |
| Helicone | (observability) | Free 10K req/mo; $20/mo+ | Proxy in front of provider; per-request log | Proxy-style integration via base-URL swap — zero SDK change |

---

## Detailed analysis

### LangGraph

- **URL.** https://github.com/langchain-ai/langgraph , https://docs.langchain.com/oss/python/langgraph/overview
- **Language / runtime.** Python (primary), JavaScript/TypeScript.
- **License.** MIT (OSS). Commercial layer = **LangGraph Platform** + **LangSmith**.
- **Maintainer.** LangChain (vendor-backed).
- **What it does.** Low-level orchestration framework and runtime for long-running, stateful, multi-actor agent graphs. As of 2026 the LangChain team has effectively absorbed `langchain.agents` into LangGraph: `AgentExecutor` is deprecated (EOL Dec 2026), legacy chains moved to `langchain-classic`, and the new agent factory is `langchain.agents.create_agent` built on the LangGraph runtime ([LangChain v1 migration guide](https://docs.langchain.com/oss/python/migrate/langchain-v1), [LangChain & LangGraph v1.0](https://blog.langchain.com/langchain-langgraph-1dot0/)).
- **Public abstractions.**
  - `StateGraph` over a typed state object (Pydantic v3 recommended).
  - `add_node(name, fn)` — each node reads/writes the shared state.
  - `add_edge` and `add_conditional_edges(router_fn)` for routing.
  - `MemorySaver` / `AsyncSqliteSaver` / `PostgresSaver` checkpointers.
  - `interrupt` for human-in-the-loop gates.
  - Subgraphs for modular multi-agent composition.
  - Middleware system attached to `create_agent` for HITL approval, conversation compression, PII scrubbing, retries, output validation ([LangChain v1.0 blog](https://blog.langchain.com/langchain-langgraph-1dot0/)).
- **Tool calling.** Standard LangChain `BaseTool` / `@tool` decorator → JSON schema, dispatched inside the agent node.
- **State / memory.** Typed shared state is *the* primary memory. Checkpointers persist per-thread state. Long-term memory via separate stores (vector DB, kv store).
- **Observability.** LangSmith — trace tree, token cost, latency, evals. LangGraph emits OpenTelemetry-compatible spans.
- **Notable lessons.**
  - **Borrow:** The state-machine + reducer-over-typed-state pattern. A Rust runtime can model an agent step as `fn(State, Event) -> (State, Vec<Event>)` with checkpointing at the trait boundary.
  - **Borrow:** Middleware as the cross-cutting-concerns layer (HITL, redaction, retry). A Rust `Layer`/`Service` (tower-style) maps naturally.
  - **Cautionary:** LangChain's first agent abstraction (`AgentExecutor`) lasted ~2 years before being deprecated. Keep public surface small and re-derive from a small kernel.

---

### LlamaIndex Workflows 1.0

- **URL.** https://www.llamaindex.ai/blog/announcing-workflows-1-0-a-lightweight-framework-for-agentic-systems , https://developers.llamaindex.ai/python/llamaagents/workflows/
- **Language / runtime.** Python + TypeScript.
- **License.** MIT (OSS). Commercial layer = LlamaCloud / LlamaParse.
- **Maintainer.** LlamaIndex (vendor-backed).
- **What it does.** Event-driven, async-first, step-based control flow for agentic apps. Each "step" is a `@step`-decorated method; steps consume one event type and emit another. The graph is *inferred* from type signatures rather than declared.
- **Public abstractions.**
  - `Workflow` (base class).
  - `@step` decorator on coroutine methods.
  - `Event` subclasses (typed) — produced and consumed by steps.
  - Shared `Context` for cross-step state and serialization.
  - `StartEvent` / `StopEvent` markers.
  - Validity check performed before run by walking inferred input/output types.
- **Tool calling.** Function-calling via the underlying LLM integrations (OpenAI / Anthropic). Typed via Pydantic. The Workflow itself is event-driven, not tool-driven; tools live inside steps.
- **State / memory.** `Context` holds shared state; workflows can be serialized mid-run and resumed.
- **Observability.** Built-in event stream + integration with Arize, Langfuse, OpenTelemetry.
- **Notable lessons.**
  - **Borrow:** Typed events as the wire format between steps. In Rust this maps to an enum or trait-object channel.
  - **Borrow:** Validity check from type signatures — Rust gets this *for free* from the type system if events are enums.
  - **Reject:** Implicit graph from types is magical; explicit graph (LangGraph) is easier to debug. Rust idioms favor explicitness anyway.

---

### Mastra

- **URL.** https://mastra.ai/ , https://github.com/mastra-ai/mastra
- **Language / runtime.** TypeScript / Node.
- **License.** OSS (Elastic v2 family). Commercial layer = **Mastra Cloud**.
- **Maintainer.** Mastra (the team behind Gatsby).
- **What it does.** "Batteries-included" TypeScript framework with one consistent set of primitives for agents, tools, workflows, memory, and RAG. Aimed at full-stack TS shops shipping agent products.
- **Public abstractions.**
  - `createAgent({ model, instructions, tools, memory })` — agent definition.
  - `createTool({ id, description, inputSchema, execute })` — Zod-typed tool.
  - `createWorkflow({ steps, triggers })` — graph-based, durable state machine with suspend/resume and live "watch" streams.
  - `createMemory({ store, vector })` — built-in conversation history + semantic retrieval.
  - RAG ingestion utilities (PDF/Markdown chunkers, embedders).
- **Tool calling.** Zod schemas, automatically converted to JSON schema for the LLM. Parameter validation built in.
- **State / memory.** Persistent memory abstraction with both KV (conversation) and vector (semantic) backends.
- **Observability.** Built-in tracing, OpenTelemetry export, Mastra Cloud dashboards.
- **Notable lessons.**
  - **Borrow:** Workflow as a primitive *separate from* the agent loop. Agent = "loop until done"; Workflow = "explicit graph with suspend/resume." Keeping them distinct avoids the LangChain-style "chain vs agent" identity crisis.
  - **Borrow:** Schema-validated tools at the type-system level (Zod in TS ≈ `schemars`/`serde` in Rust).
  - **Borrow:** A "watch" stream as a first-class API surface, not just observability — clients can subscribe to in-flight workflow state.

---

### CrewAI

- **URL.** https://crewai.com/ , https://github.com/crewaiinc/crewai
- **Language / runtime.** Python.
- **License.** MIT (OSS). Commercial = **CrewAI Enterprise**.
- **Maintainer.** crewAIInc (vendor-backed).
- **What it does.** Role-based multi-agent orchestration. You define agents with personalities ("role/goal/backstory"), attach tasks, group them into a crew, and run.
- **Public abstractions.**
  - `Agent(role, goal, backstory, tools, llm)`.
  - `Task(description, expected_output, agent)`.
  - `Crew(agents, tasks, process=...)`.
  - Three processes: **sequential** (run tasks in order), **hierarchical** (a manager agent delegates), **consensual** (agents vote).
- **Tool calling.** LangChain-compatible tool interface; JSON schema under the hood.
- **State / memory.** Per-agent and per-crew memory; short-term + long-term + entity memory tiers.
- **Observability.** Built-in event log; integrations with LangSmith, AgentOps, Langfuse.
- **Notable lessons.**
  - **Borrow:** The three-process taxonomy (sequential / hierarchical / consensual) is a useful catalog of *coordination patterns*. A Rust crate doesn't need to ship roleplay metaphors, but the patterns themselves are real.
  - **Reject:** The "backstory" prompt-engineering DSL conflates *configuration* with *prompt template*. A cleaner separation: prompt is a typed input to the agent, not part of its identity.

---

### AutoGen → Microsoft Agent Framework (MAF)

- **URL.** https://github.com/microsoft/autogen , https://learn.microsoft.com/en-us/agent-framework/overview/
- **Language / runtime.** Python + .NET (MAF). AutoGen 0.x = Python.
- **License.** MIT (OSS).
- **Maintainer.** Microsoft. AutoGen is now in *maintenance mode*; new users are directed to MAF 1.0 ([AutoGen → MAF migration guide](https://learn.microsoft.com/en-us/agent-framework/migration-guide/from-autogen/)).
- **What it does.** AutoGen models agent systems as **conversations between agents**. MAF 1.0 combines AutoGen's conversational primitives with Semantic Kernel's enterprise features (session state, type safety, middleware, telemetry) and adds **graph-based workflows** for explicit multi-agent orchestration.
- **Public abstractions.**
  - `ConversableAgent` / `AssistantAgent` / `UserProxyAgent` (AutoGen).
  - `Agent` + `Tool` + middleware pipeline (MAF).
  - Workflow graph for explicit orchestration (MAF).
  - A2A protocol + MCP for interop.
- **Tool calling.** Function calling via the model provider; in MAF, tools can be native code, prompt templates, OpenAPI specs, or MCP servers.
- **State / memory.** Session-based state management in MAF; conversation history in AutoGen.
- **Observability.** OpenTelemetry, Azure Monitor integration in MAF.
- **Notable lessons.**
  - **Cautionary:** AutoGen → MAF is the *second* Microsoft attempt (Semantic Kernel was the first). Three years of churn — agents-as-conversation was elegant but underspecified for production. **A Rust crate should not lean on pure "agents talking to agents"** without an explicit orchestration layer.
  - **Borrow:** Four tool sources behind one interface (native / prompt / OpenAPI / MCP) is a useful abstraction.

---

### Semantic Kernel → Microsoft Agent Framework

- **URL.** https://github.com/microsoft/semantic-kernel , https://learn.microsoft.com/en-us/agent-framework/migration-guide/from-semantic-kernel/
- **Language / runtime.** C# (primary), Python, Java.
- **License.** MIT (OSS).
- **Maintainer.** Microsoft. SK is being replaced by MAF; SK's `Kernel + Plugin` collapse into MAF's `Agent + Tool` ([SK → MAF migration guide](https://learn.microsoft.com/en-us/agent-framework/migration-guide/from-semantic-kernel/)).
- **What it does.** Multi-language SDK that integrates LLMs into application code through a "Kernel" container plus a plugin ecosystem (native code, prompt templates, OpenAPI, MCP).
- **Public abstractions.** `Kernel`, `IChatClient`, `KernelPlugin`, `KernelFunction` — and in MAF, `Agent`, `Tool`, middleware.
- **Notable lessons.**
  - **Cautionary:** "Kernel" as an explicit container ended up *too implicit* for what plugins are. MAF renamed Plugin → Tool. Names matter — pick "Tool" not "Plugin," "Skill," or "Function." (Anthropic, OpenAI, Vercel all converged on **Tool**.)
  - **Borrow:** Dependency-injection-friendly design. In Rust, this is generics + traits or `Box<dyn Tool>`.

---

### Vercel AI SDK

- **URL.** https://ai-sdk.dev/ , https://github.com/vercel/ai
- **Language / runtime.** TypeScript / Node / browser.
- **License.** Apache-2.0 (OSS). Commercial = **AI Gateway** (per-token markup).
- **Maintainer.** Vercel (vendor-backed).
- **What it does.** Provider-agnostic TS SDK for streaming LLM responses, tool calling, and (as of v6) reusable Agents.
- **Public abstractions.**
  - `streamText({ model, prompt, tools, toolChoice, maxToolRoundtrips })` — inline tool definitions co-located with the call.
  - `generateObject({ model, schema })` — structured output.
  - v6: `Agent({ model, instructions, tools })` — reusable agent definition; `ToolLoopAgent` is a production-ready loop implementation ([AI SDK 6 launch blog](https://vercel.com/blog/ai-sdk-6)).
  - MCP client built-in.
- **Tool calling.** Zod schemas inline at the call site (`tools: { getWeather: tool({ description, parameters: z.object({...}), execute }) }`).
- **State / memory.** Caller-managed; `useChat` hook handles message history client-side. No built-in long-term memory.
- **Observability.** OpenTelemetry traces, AI Gateway dashboards.
- **Notable lessons.**
  - **Borrow:** Streaming-first API. In Rust, model the agent step as a `Stream<Item = AgentEvent>` rather than a `Result<Response>`.
  - **Borrow:** Inline tool definitions adjacent to the call site for the "one-shot tool use" case. Pair this with a separate `Agent` type for the "reusable, multi-call" case — Vercel ships *both*.
  - **Reject:** Building an entire SDK around `useChat`-style React hooks couples runtime to a frontend framework. Keep the Rust runtime UI-agnostic.

---

### Spring AI

- **URL.** https://spring.io/ai/ , https://spring.io/blog/2026/01/13/spring-ai-generic-agent-skills/
- **Language / runtime.** Java / Kotlin (Spring ecosystem).
- **License.** Apache-2.0 (OSS).
- **Maintainer.** VMware / Broadcom (Spring team).
- **What it does.** Vendor-neutral Java abstractions for chat, memory, vector stores, tools, MCP, and (in 2026) agentic patterns including Agent Skills, Subagent Orchestration, and A2A.
- **Public abstractions.**
  - `ChatClient` — model access.
  - `ChatMemory` — stateful conversations.
  - `VectorStore` — RAG.
  - `Tool` + `@McpTool` annotations.
  - **Agent Skills**: discoverable folders of instructions/scripts/resources loaded on demand ([Spring AI Agent Skills](https://spring.io/blog/2026/01/13/spring-ai-generic-agent-skills/)).
  - **Subagent Orchestration**: `Task` tool spawns specialized subagents in isolated context windows, returning only essentials ([Spring AI Subagents](https://spring.io/blog/2026/01/27/spring-ai-agentic-patterns-4-task-subagents/)).
  - **A2A protocol** support for cross-platform agent communication.
- **Tool calling.** JSON schema via Java reflection on annotated methods; MCP for external tools.
- **State / memory.** `ChatMemory` interface with in-memory, JDBC, and vector-backed implementations.
- **Observability.** Micrometer integration → any APM (Datadog, Dynatrace, Prometheus).
- **Notable lessons.**
  - **Borrow:** **Agent Skills as discoverable folders**. This is essentially Anthropic's Skills concept lifted into Java. A Rust runtime could ship a "skill manifest" loader (`skill.toml`) without inventing a new format.
  - **Borrow:** **Subagent isolation** — spawn a subagent with its own context window, only return the distilled result. This bounds context bloat in long-running agents.
  - **Borrow:** **A2A + MCP** as orthogonal interop protocols (agent ↔ agent vs. agent ↔ tool).

---

### DSPy

- **URL.** https://dspy.ai/ , https://github.com/stanfordnlp/dspy
- **Language / runtime.** Python.
- **License.** MIT (OSS).
- **Maintainer.** Stanford NLP (academic-led, community).
- **What it does.** "Programming, not prompting" — declare program structure in code, then a compiler optimizes prompts (and optionally LM weights) from data and a metric. Three-tier abstraction: **Signature** → **Module** → **Optimizer**.
- **Public abstractions.**
  - **Signature**: I/O contract, either string form (`"question -> answer"`) or class form (`class QA(dspy.Signature): question: str = dspy.InputField(); answer: str = dspy.OutputField()`).
  - **Module**: encapsulates a prompting technique. Examples: `dspy.Predict`, `dspy.ChainOfThought`, `dspy.ReAct(signature, tools=[...], max_iters=10)`.
  - **Optimizer**: e.g. `MIPROv2` — Bayesian optimization over instructions + few-shot examples, given a metric and a few hundred examples ([DSPy Optimizers](https://dspy.ai/learn/optimization/optimizers/)).
- **Tool calling.** Tools are Python functions with **docstring + type hints**; DSPy synthesizes the JSON schema. `dspy.ReAct(signature, tools=[...])` is the agent module.
- **State / memory.** Stateless by default; community examples wire memory via Mem0 or via custom modules.
- **Observability.** Trace history per module; integrations with MLflow, LangFuse, Phoenix.
- **Notable lessons.**
  - **Borrow:** **The Signature/Module/Optimizer three-tier split** is the cleanest separation of concerns in the space. Rust analog: a `Signature` trait (input/output associated types), a `Module` trait (one call method), an `Optimizer` that drives modules against a metric. This decouples *what* an agent computes from *how* the prompt is shaped.
  - **Borrow:** Tools defined by docstring + type hints. In Rust, derive the schema from struct types via `schemars` and pull the description from doc comments via a proc-macro.
  - **Cautionary:** The optimizer feedback loop requires *labeled data + a metric* — most production teams don't have that. Ship the runtime without forcing this commitment.

---

### OpenAI Assistants API (deprecated — sunsetting Aug 26, 2026)

- **URL.** https://platform.openai.com/docs/api-reference/runs , https://platform.openai.com/docs/assistants/tools/function-calling
- **Language / runtime.** HTTP API; client SDKs in many languages.
- **License / pricing.** Per-token at standard model rates, plus tool-specific surcharges. Not separately priced beyond that.
- **Maintainer.** OpenAI. Successor is the **Responses API**.
- **What it does.** Hosted agent lifecycle. You create an `Assistant`, attach `Tools`, create a `Thread` (persistent conversation), then `Run` the Thread.
- **Public abstractions.**
  - `Assistant` (model + tools + instructions, persistent).
  - `Thread` (conversation, persistent).
  - `Run` (one execution of the Assistant against the Thread).
  - **`Run.status` lifecycle:** `queued → in_progress → requires_action → in_progress → completed | failed | expired`.
  - When `requires_action`, the caller reads `required_action.submit_tool_outputs.tool_calls`, executes them client-side, and POSTs results back via `submit_tool_outputs`.
  - Runs expire **10 minutes** after creation.
- **Notable lessons.**
  - **Cautionary:** The 10-minute run expiry forced everyone to manage their own polling and re-submission. Don't bake hard timeouts into a Rust runtime's public API; expose them as policy.
  - **Cautionary:** `Assistants` is being **deprecated after ~2 years** (Aug 26, 2026 sunset; [migration guide](https://ragwalla.com/docs/guides/openai-assistants-api-deprecation-2026-migration-guide-wire-compatible-alternatives)). Hosted lifecycles change. A self-hostable runtime crate insulates users from this churn.
  - **Borrow:** The explicit `requires_action` state. A Rust agent step can model "needs tool result" as an enum variant the caller must satisfy.

---

### OpenAI Responses API + Agents SDK

- **URL.** https://openai.github.io/openai-agents-python/ , https://developers.openai.com/api/docs/guides/agents , https://openai.com/index/new-tools-for-building-agents/
- **Language / runtime.** HTTP (Responses API) + Python SDK (Agents SDK) + JS port.
- **License / pricing.** SDK: MIT (OSS). API: per-token at standard model rates plus tool surcharges:
  - **Web Search**: $10 / 1K calls + 8K tokens per search.
  - **File Search**: $2.50 / 1K queries + $0.10 / GB / day storage.
  - **Code Interpreter**: $0.03 / session.
  - **Computer Use** (preview, tier 3-5): $3 / $12 per 1M input/output tokens.
- **Maintainer.** OpenAI.
- **What it does.** Responses API is OpenAI's primary agent primitive going forward — combining Chat Completions simplicity with built-in tools. The Agents SDK is a thin orchestration layer on top.
- **Public abstractions (Agents SDK).**
  - `Agent(name, instructions, model, tools, handoffs, guardrails)`.
  - **Handoffs**: agent-to-agent delegation modeled *as tools* — e.g. a handoff to "Refund Agent" appears as `transfer_to_refund_agent` in the tool list.
  - **Guardrails** in three places:
    - **Input guardrails** — validate user input (applied to the first agent in a chain).
    - **Output guardrails** — validate final response (applied to the agent producing final output).
    - **Tool guardrails** — wrap each function tool call.
  - `Runner.run(agent, input)` drives the loop.
- **Tool calling.** `@function_tool` decorator → Pydantic-derived JSON schema.
- **State / memory.** Caller-managed via `Runner` and message history; persistent threads not part of the SDK (caller plugs in their own store).
- **Observability.** SDK ships built-in tracing; integrates with the OpenAI dashboard and external sinks.
- **Notable lessons.**
  - **Borrow:** **Handoffs as tools.** The LLM already knows how to call tools; reusing that channel for agent-to-agent delegation is elegant. A Rust runtime can model `Handoff` as a special `Tool` variant.
  - **Borrow:** **Three guardrail tiers** (input / tool / output) — explicit places to inject policy. Compose well with a middleware pipeline.
  - **Borrow:** Decoupling "API" (Responses) from "orchestration" (Agents SDK) — keeps each layer simple.

---

### Anthropic Claude Agent SDK + Managed Agents

- **URL.** https://github.com/anthropics/claude-agent-sdk-python , https://code.claude.com/docs/en/agent-sdk/overview , https://platform.claude.com/docs/en/managed-agents/overview
- **Language / runtime.** Python and TypeScript SDKs; Managed Agents = hosted runtime.
- **License / pricing.** SDK: MIT (OSS). Tokens at Claude rates (Haiku 4.5 $1/$5, Sonnet 4.6 $3/$15, Opus 4.7 $5/$25 per M input/output). **Managed Agents adds $0.08 per session-hour** of *running* time (idle/paused not billed). Web search at $10 / 1K. Starting Jun 15 2026, subscription users get a separate **Agent SDK credit pool** ([Claude Managed Agents pricing](https://wavespeed.ai/blog/posts/claude-managed-agents-pricing-2026/)).
- **Maintainer.** Anthropic.
- **What it does.** Programmable runtime for Claude Code-style agents — same kernel that powers Claude Code, exposed as an SDK. Managed Agents is the hosted version with persistent memory and runtime.
- **Public abstractions.**
  - `query(prompt, options)` — one-shot.
  - `ClaudeSDKClient(...)` — stateful, supports custom tools and hooks.
  - **Custom tools = in-process MCP servers** — `@tool`-decorated Python functions run inside the host process, no separate subprocess.
  - **Hooks** — Python functions invoked at deterministic loop points (e.g. `PreToolUse`, `PostToolUse`, `Stop`). Used for validation, automated feedback, deterministic gates.
  - **Memory tool** — agents can persist text documents to a workspace-scoped store mounted at `/mnt/memory/` inside the agent container (Managed Agents only).
  - **Subagents** — can have their own prompts/tools/disallowed-tools/models/permission-modes/MCP-servers/hooks/memory-scopes/effort-settings/background-behavior/isolation-rules/skills.
- **Tool calling.** MCP-native — both external MCP servers (subprocess) and in-process MCP (custom tools in the host language). JSON schema.
- **State / memory.** Three tiers: in-flight context, memory tool (per-session structured docs), Managed Agents persistent memory (workspace-scoped, across sessions).
- **Observability.** Built-in trace events surfaced through hooks; OTel export.
- **Notable lessons.**
  - **Borrow:** **Hooks at deterministic loop points.** This is the cleanest "extension" model in the space — a few named lifecycle points (`PreToolUse`, `PostToolUse`, `Stop`, etc.), each with a typed payload, each able to mutate, block, or annotate. A Rust runtime can ship this as a trait with default-impl methods.
  - **Borrow:** **Tools as in-process MCP.** Same schema as external MCP, but called directly. One protocol, two transports. Rust analog: `Tool` trait with both local impls and an MCP-bridge impl.
  - **Borrow:** Per-subagent **isolation knobs** (own tools, own permissions, own model, own memory scope). These are the actual production-grade controls.
  - **Reject:** A *mount-a-directory* memory model is OS-specific. In Rust, prefer a typed `MemoryStore` trait with filesystem as one impl.

---

### AWS Bedrock Agents / AgentCore

- **URL.** https://aws.amazon.com/bedrock/agents/ , https://aws.amazon.com/bedrock/agentcore/pricing/
- **Language / runtime.** HTTP (Bedrock API); SDKs across languages.
- **License / pricing.** No separate per-agent price; you pay for the underlying model inference, knowledge-base retrieval, and guardrails. **AgentCore infra runs ~10-30% of total agent cost at scale**.
- **Maintainer.** Amazon.
- **What it does.** Managed agent platform on AWS. The Apr 2026 "managed harness" runs the full loop (reason → choose tool → execute → stream).
- **Public abstractions.**
  - `Agent` (foundation model + instructions).
  - **Action Group**: a set of API operations defined by an **OpenAPI schema**, optionally backed by a Lambda for execution.
  - **Knowledge Base**: RAG over vector store.
  - **Guardrail** (content filters / PII / topic blocking).
  - **Custom orchestration** Lambda — escape hatch when the default loop isn't enough.
  - **Multi-agent collaboration** — supervisor + sub-agents.
- **Tool calling.** OpenAPI schema (not just JSON schema for a function — a whole API surface). Each operation is a "tool."
- **State / memory.** Session state managed by AgentCore; persistent memory + traces.
- **Observability.** CloudWatch + the AgentCore console.
- **Notable lessons.**
  - **Borrow:** **OpenAPI as a tool source.** Instead of N hand-written tool defs, point at one OpenAPI doc and get N tools for free. Rust analog: a `from_openapi(path) -> Vec<Tool>` adapter.
  - **Borrow:** "Custom orchestration Lambda" escape hatch. The default loop covers 80%; allow users to plug in their own.

---

### Google Vertex AI Agent Builder (rebranded "Gemini Enterprise Agent Platform" at Cloud Next 2026)

- **URL.** https://cloud.google.com/products/agent-builder , https://docs.cloud.google.com/agent-builder/agent-development-kit/overview
- **Language / runtime.** Python (ADK), HTTP (Agent Engine).
- **License / pricing.** Per-resource:
  - **Agent Engine runtime**: $0.0864 per vCPU-hour + $0.0090 per GB-hour.
  - **Session / memory storage**: $0.25 per 1K events.
  - **Vertex AI Search**: $1.50–$6.00 per 1K queries.
  - **Foundation model tokens** priced separately.
  - Free tier: 50 vCPU-hours + 100 GB-hours per month; $300 new-customer credits.
- **Maintainer.** Google Cloud.
- **What it does.** Unified platform with ADK (code-first), Agent Studio (visual), Agent Engine (runtime), persistent memory, 200+ model catalog including Gemini and Claude.
- **Public abstractions.** ADK: `Agent`, `Tool`, `Workflow`. Model-agnostic. Plus Agent Studio's visual builder backed by the same primitives.
- **Notable lessons.**
  - **Borrow:** Code-first SDK + visual builder both backed by the **same** primitives. A Rust crate's primitives should be cleanly serializable so an external UI could one day target them.
  - **Borrow:** **Per-resource pricing dimensions** reveal what's actually expensive: compute time, memory events, retrieval queries, tokens. Mirror these in observability (cost per dimension).

---

### Observability platforms

#### Langfuse — https://langfuse.com/

- OSS (self-host free) + Cloud ($50/mo, Enterprise from $2,499/mo). Free tier 50K events/mo.
- Unit-based pricing (traces + observations + scores). **Trade-off:** agent traces with many small spans hit thresholds faster.
- Model: trace → observation tree → scores → datasets/evals.

#### LangSmith — https://www.langchain.com/langsmith/

- Hosted only. Free 5K traces/mo; $39/user/mo paid; seat-based.
- Deep LangChain/LangGraph integration — auto-wired.
- Model: run tree, evaluators, datasets, prompt versioning.

#### Helicone — https://helicone.ai/

- Free 10K req/mo; from $20/mo.
- **Proxy-style** integration — change the OpenAI/Anthropic base URL, get logs. Zero SDK change.
- Model: request log + per-user/per-prompt analytics.

**Lessons for a Rust runtime.** The market converged on **OpenTelemetry-compatible span trees**. Every platform consumes OTel. A Rust runtime should emit OTel spans natively and let the user pick the backend. Don't ship a proprietary trace format.

---

## Common abstractions across frameworks

What every serious framework now ships:

- **Tool calling.** A `Tool` (or `Function` / `Plugin`) primitive with a schema (JSON Schema, typically derived from native types — Pydantic / Zod / structs / reflection / docstrings + type hints). Two transports: in-process functions and remote MCP servers. MCP has become *the* interop standard in 2026.
- **State.** A typed, serializable shared state object passed between steps. Checkpointing to KV/Postgres/SQLite. Conversation history as one slice of state, not the whole thing.
- **Streaming.** First-class. Either Server-Sent Events from the agent loop or async iterators over event types (token / tool_call / tool_result / step_complete / stop).
- **Observability.** Span-tree traces (OpenTelemetry-compatible), per-step token/cost accounting, evals attached to traces.
- **Multi-agent.** Either **conversation** (AutoGen), **handoffs** (OpenAI), **graphs** (LangGraph, MAF, Mastra), **roles** (CrewAI), or **subagents with isolation** (Anthropic, Spring AI). The "subagent with own context window" pattern has emerged as the workhorse.
- **Memory.** Three tiers everywhere: in-flight context, per-session structured memory, cross-session persistent memory.
- **Guardrails / middleware.** A pipeline of pre/post-tool, pre/post-LLM, pre/post-handoff checks. Names differ (Anthropic = hooks; OpenAI = guardrails; LangChain = middleware; Spring = filters), but the shape is identical: typed function called at a named lifecycle point.

---

## Patterns worth borrowing for Rust

1. **Three-tier abstraction (DSPy).** Separate the *I/O contract* (Signature) from the *strategy* (Module) from the *optimization* (Optimizer). In Rust: a `Signature` trait with associated `Input`/`Output` types; a `Module` trait that calls one; an `Optimizer` that drives modules against a metric. Even if you never ship an optimizer, the separation alone is worth it.

2. **Hooks at named lifecycle points (Anthropic).** Pick 5–8 named points (`pre_tool_use`, `post_tool_use`, `pre_llm_call`, `post_llm_call`, `on_step_complete`, `on_stop`). Each is a typed payload. Compose hooks as a `Vec<Box<dyn Hook>>`. Decisions allowed: continue / mutate / abort / annotate. **This is the cleanest extensibility model in the entire space.**

3. **Tool-and-handoff symmetry (OpenAI Agents SDK).** Model agent-to-agent delegation as a special tool variant. The LLM already knows how to call tools — don't invent a second channel.

4. **Workflow as a primitive separate from agent (Mastra, LangGraph).** Agent loop = "loop until done." Workflow = "explicit graph with suspend/resume." Different surfaces, same kernel. **Don't conflate them** — that's the LangChain-classic mistake.

5. **Typed events between steps (LlamaIndex, LangGraph).** Steps consume one event variant and emit another. In Rust this is `enum AgentEvent { ... }` and a `Channel<AgentEvent>`. The compiler enforces step compatibility.

6. **Tools from multiple sources behind one trait (Bedrock, Spring AI).** `from_function`, `from_mcp_server`, `from_openapi`, `from_skill_folder` — all return a `Box<dyn Tool>`. Users plug in whichever they have.

7. **Subagent isolation (Anthropic, Spring AI).** A subagent can specify its own model, own tools, own permissions, own memory scope. Parent gets a distilled result, not the subagent's whole context. Bounds context bloat in long-running workflows.

8. **Per-resource cost observability (Vertex).** Emit metrics partitioned by *what costs money*: tokens (in/out by model), tool invocations, vector queries, runtime seconds. Don't bury cost in trace blobs.

9. **OpenTelemetry-native traces.** Don't invent a proprietary format. The whole observability market consumes OTel.

10. **MCP as first-class.** Both as a *server* (expose your tools to other clients) and a *client* (consume tools from MCP servers). Two transports for one schema.

---

## Patterns to avoid

1. **Conflating "agent" with "chain" with "workflow"** (LangChain pre-v1.0). Each is a distinct concept; collapsing them into one class is what caused the AgentExecutor deprecation. Keep them separate from day one.

2. **Roleplay-as-configuration** (CrewAI's `role/goal/backstory`). Mixing prompt-template content into the agent's *identity field* couples persona to runtime. Keep prompt content as data the agent consumes, not part of its struct definition.

3. **Hard timeouts in the public API** (OpenAI Assistants' 10-minute run cap). Push timeout policy to the caller; ship it as configurable, not baked.

4. **Inferring control flow from type signatures alone** (LlamaIndex Workflows). Elegant in Python, *redundant* in Rust where the type system does this anyway, and harder to debug than an explicit graph.

5. **Hosted-only / opaque runtimes** (OpenAI Assistants, before deprecation). Two-year half-life. A self-hostable Rust runtime is a *feature*, not a limitation.

6. **Per-token markup observability tiers** (LangSmith $39/user/mo, Langfuse $2,499/mo enterprise). If you ship observability, make the local path free and friction-free; charge for hosted convenience, not for the ability to see your own traces.

7. **Renaming everything every two years** (Microsoft: Semantic Kernel → AutoGen → MAF; LangChain: Chains → AgentExecutor → LangGraph → create_agent). The damage compounds. Commit to terminology — **"Tool" not "Plugin"/"Skill"/"Function"; "Agent" not "Crew"/"Assistant"/"Worker"; "Hook" or "Middleware" not both**.

8. **`useChat`-coupled APIs** (Vercel AI SDK). Tying the runtime to a frontend framework limits reach. A Rust runtime should be UI-agnostic.

9. **Proprietary trace formats.** OTel won. Don't fight it.

10. **"Backstory" / persona prompt-engineering DSLs.** Free-form prompt fields disguised as structured config. Either it's structured (typed) or it's a system prompt string — not a halfway thing.

---

## Market signals

### Pricing tiers

- **Hosted runtimes are converging on per-session-time billing.** Anthropic charges **$0.08 per session-hour** of running time (idle not billed) on top of tokens. Vertex charges **per vCPU-hour + per GB-hour**. AWS doesn't separately charge, but AgentCore infra runs **10-30% of total agent cost** at scale. Translation: **the market values the runtime, not just the model**.
- **Tool calls are priced separately.** Web search $10/1K (OpenAI and Anthropic both); File search $2.50/1K + storage; Code Interpreter $0.03/session. **Tools are a profit center.**
- **Memory is priced per-event.** Vertex: $0.25 per 1K memory events. Memory is a metered resource.
- **Observability is per-event or per-seat.** Langfuse: per-trace + per-observation + per-score. LangSmith: $39/user/mo seat. Helicone: per-request.

### Hosted vs. self-host split

- **Self-hostable OSS frameworks** (LangGraph, LlamaIndex, Mastra, CrewAI, Spring AI, MAF, DSPy, Vercel AI SDK) own the developer mindshare. Every one has a hosted commercial tier that adds: persistence, deploy/scale, observability, evals, RBAC.
- **Hosted-only APIs** (OpenAI Assistants — deprecating, Bedrock Agents, Vertex Agent Builder) are losing ground to self-host + provider-agnostic SDKs. Anthropic's Agent SDK is **OSS + optional Managed Agents** — a hybrid model the market seems to reward.
- **The proxy/observability layer** (Helicone, Langfuse Cloud, LangSmith) is sticky because it's a 1-line integration and gives immediate ROI. A Rust runtime can win adoption by *emitting OTel that these platforms consume* — no integration work for the user.

### What every paid tier charges for

In rough order of frequency:
1. **Hosted observability + trace storage** (every observability vendor)
2. **Managed runtime / session-hour compute** (Anthropic, Vertex, AgentCore, Mastra Cloud, LangGraph Platform)
3. **Built-in tools** (web search, file search, code interp) — OpenAI, Anthropic, Bedrock
4. **Memory storage / events** — Vertex, Anthropic Managed Agents
5. **Evaluation / dataset management** — LangSmith, Langfuse, Braintrust
6. **Enterprise SSO / RBAC / audit** — every cloud tier
7. **Per-user seats** — LangSmith, CrewAI Enterprise

What the market *doesn't* successfully charge for: the framework code itself. **The runtime is open; the operations are paid.**

---

## Data sources

- LangChain / LangGraph v1.0 announcement — https://blog.langchain.com/langchain-langgraph-1dot0/
- LangChain v1 migration guide — https://docs.langchain.com/oss/python/migrate/langchain-v1
- LangGraph overview — https://docs.langchain.com/oss/python/langgraph/overview
- LangGraph repo — https://github.com/langchain-ai/langgraph
- LangGraph 2026 patterns — https://callsphere.ai/blog/langgraph-agent-patterns-2026-stateful-multi-step-ai-workflows
- LlamaIndex Workflows 1.0 — https://www.llamaindex.ai/blog/announcing-workflows-1-0-a-lightweight-framework-for-agentic-systems
- LlamaIndex Workflows docs — https://developers.llamaindex.ai/python/llamaagents/workflows/
- LlamaIndex Workflows event-driven architecture — https://www.dataleadsfuture.com/deep-diving-into-llamaindex-workflow-event-driven-llm-architecture/
- Mastra framework — https://mastra.ai/framework
- Mastra repo — https://github.com/mastra-ai/mastra
- Mastra 2026 guide — https://www.generative.inc/mastra-ai-the-complete-guide-to-the-typescript-agent-framework-2026
- CrewAI docs (Agents) — https://docs.crewai.com/en/concepts/agents
- CrewAI repo — https://github.com/crewaiinc/crewai
- AutoGen repo — https://github.com/microsoft/autogen
- Microsoft Agent Framework overview — https://learn.microsoft.com/en-us/agent-framework/overview/
- AutoGen → MAF migration — https://learn.microsoft.com/en-us/agent-framework/migration-guide/from-autogen/
- Semantic Kernel → MAF migration — https://learn.microsoft.com/en-us/agent-framework/migration-guide/from-semantic-kernel/
- Vercel AI SDK 6 — https://vercel.com/blog/ai-sdk-6
- AI SDK streamText reference — https://ai-sdk.dev/docs/reference/ai-sdk-core/stream-text
- AI SDK repo — https://github.com/vercel/ai
- Spring AI Agent Skills — https://spring.io/blog/2026/01/13/spring-ai-generic-agent-skills/
- Spring AI Subagent Orchestration — https://spring.io/blog/2026/01/27/spring-ai-agentic-patterns-4-task-subagents/
- Spring AI A2A — https://spring.io/blog/2026/01/29/spring-ai-agentic-patterns-a2a-integration/
- DSPy site — https://dspy.ai/
- DSPy Signatures — https://dspy.ai/learn/programming/signatures/
- DSPy Modules — https://dspy.ai/learn/programming/modules/
- DSPy Optimizers — https://dspy.ai/learn/optimization/optimizers/
- DSPy ReAct API — https://dspy.ai/api/modules/ReAct/
- DSPy paper — https://arxiv.org/pdf/2310.03714
- OpenAI Assistants Runs reference — https://platform.openai.com/docs/api-reference/runs
- OpenAI Assistants function calling — https://platform.openai.com/docs/assistants/tools/function-calling
- OpenAI Assistants deprecation — https://ragwalla.com/docs/guides/openai-assistants-api-deprecation-2026-migration-guide-wire-compatible-alternatives
- OpenAI Assistants vs Responses — https://ragwalla.com/blog/openai-assistants-api-vs-openai-responses-api-complete-comparison-guide
- OpenAI Agents SDK docs — https://openai.github.io/openai-agents-python/
- OpenAI Agents SDK handoffs — https://openai.github.io/openai-agents-python/handoffs/
- OpenAI Agents SDK guardrails — https://openai.github.io/openai-agents-python/guardrails/
- OpenAI API pricing — https://openai.com/api/pricing/
- OpenAI new tools for agents — https://openai.com/index/new-tools-for-building-agents/
- Anthropic Agent SDK overview — https://code.claude.com/docs/en/agent-sdk/overview
- Anthropic Agent SDK repo — https://github.com/anthropics/claude-agent-sdk-python
- Claude Managed Agents overview — https://platform.claude.com/docs/en/managed-agents/overview
- Claude Managed Agents pricing — https://wavespeed.ai/blog/posts/claude-managed-agents-pricing-2026/
- Claude API pricing — https://platform.claude.com/docs/en/about-claude/pricing
- Claude Code architecture deep-dive — https://www.penligent.ai/hackinglabs/inside-claude-code-the-architecture-behind-tools-memory-hooks-and-mcp/
- Anthropic Managed Agents memory — https://opentools.ai/news/anthropic-managed-agents-add-memory-persistent-state-for-ai-that-actually-ships
- AWS Bedrock Agents — https://aws.amazon.com/bedrock/agents/
- AWS Bedrock AgentCore pricing — https://aws.amazon.com/bedrock/agentcore/pricing/
- AWS Bedrock Agents docs — https://docs.aws.amazon.com/bedrock/latest/userguide/agents-how.html
- AWS Bedrock custom orchestration — https://docs.aws.amazon.com/bedrock/latest/userguide/agents-custom-orchestration.html
- Vertex AI Agent Builder — https://cloud.google.com/products/agent-builder
- Gemini Enterprise Agent Platform — https://cloud.google.com/products/gemini-enterprise-agent-platform
- Vertex ADK overview — https://docs.cloud.google.com/agent-builder/agent-development-kit/overview
- Vertex pricing breakdown — https://www.cloudzero.com/blog/google-vertex-ai-pricing/
- Langfuse — https://langfuse.com/
- LangSmith — https://www.langchain.com/langsmith/observability
- Helicone / observability comparison — https://getathenic.com/blog/langsmith-vs-helicone-vs-langfuse-comparison
- LLM observability tools 2026 — https://www.firecrawl.dev/blog/best-llm-observability-tools
