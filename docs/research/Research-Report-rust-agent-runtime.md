# Research Report — Rust Agent-Runtime Crate

**Date:** 2026-05-13
**Problem Statement:** Design a new public-domain Rust crate that provides the kernel of an LLM agent runtime — executor strategies, tool dispatch, LLM provider abstraction, lifecycle hooks, services — clean-room, citing only public sources.
**Target User:** Rust developers building agent-driven systems who currently either (a) wrap `async-openai` by hand, (b) maintain their own kernel-shaped micro-crate, or (c) skip Rust entirely for Python/TypeScript agent frameworks.
**Implied Scope:** Standalone Rust library crate. Permissive license (MIT or Apache-2.0). MCP-first for tool interop. Async by default (tokio). No vendor lock-in on LLM provider.

## TL;DR

The Rust agent space already has a credible framework leader (**Rig**, ~6.4k stars, MIT, production users) and a credible RAG-native option (**Swiftide**). What it does **not** have is a minimal unopinionated kernel that the long tail of community micro-crates (`mini-agent`, `yoagent`, `sgr-agent`, `agent-io`, `Synaptic`) would adopt instead of rolling their own. The opportunity is **the layer below Rig**, not a replacement for it. Specific shape: tower-middleware-aligned, MCP-first, typed-tool-args via derive macros, ~5 lifecycle hooks (not Swiftide's 10), and a deliberate stop at "runtime kernel" with no opinions about RAG, vector stores, or multi-agent crews. The novelty verdict is **Better Implementation in a narrower scope** — the angle is "the boring kernel" with `tower::Service` + `Layer` middleware as the distinctive design choice.

---

## Current User Workflow

### Trigger

A Rust developer needs an agent loop when:
- They want to run autonomous LLM-driven behavior with tool calls inside an existing Rust service (a server, daemon, embedded binary, or Postgres extension).
- They have Rust constraints they cannot trade away: zero-runtime-dep deployment, no-Python policy, embedded targets, type safety on tool arguments, deterministic memory profile.
- They are extracting an agent loop out of a larger product (the case that motivated this research).

### Today's workflow

**Path A — roll your own over `async-openai`** is the most common path observed in the micro-crate population (`mini-agent`, `yoagent`, `sgr-agent`, `agent-io`, `Synaptic`). Each ~500-2,000 LOC, each implementing the same loop: prompt → tool call → tool dispatch → loop. They duplicate ~80% of their structure.

**Path B — use Rig** is the default for "I want to ship an agent in Rust today." Rig has positive sentiment in community discussions and no surfaced "Rig sucks" thread of any size. The user base is real.

**Path C — use Swiftide / swiftide-agents** is the default when the use case is RAG-heavy or HITL (human-in-the-loop) workflows. Swiftide ships Docker-sandboxed `ToolExecutor`, Langfuse observability, and explicit RAG primitives.

**Path D — Python sidecar via PyO3 or REST** is real but disliked. Cited as "the language barrier kills the type story."

**Path E — skip Rust entirely.** Armin Ronacher's "A Language For Agents" (Feb 2026) argues agent code wants dynamic typing. Official OpenAI and Anthropic Rust SDKs do not exist; every Rust SDK is unofficial.

### Pain inventory (synthesized from `_user-workflow-pain.md`)

| Rank | Pain | Severity | Frequency |
|------|------|----------|-----------|
| 1 | Tool dispatch is string-keyed; typed args require boilerplate or hand-rolled macros | High | Widespread |
| 2 | Multi-provider abstraction leaks (OpenAI `tool_choice`, Anthropic `cache_control`, Gemini thinking config) | High | Widespread |
| 3 | Schema generation for structured outputs is manual; `serde_json::Value` is the escape hatch that defeats half the reason to choose Rust | High | Widespread |
| 4 | Async runtime mismatch (tokio vs async-std vs sync) when embedding into a non-tokio host | Medium | Niche but blocking |
| 5 | Observability / cost / tracing requires hand-rolled middleware | Medium | Widespread |
| 6 | State management is left to the user (conversation history, scratchpad, memory window) | Medium | Widespread |
| 7 | No Tower / Service-Layer middleware integration | Low | Niche but architecturally telling |

### Anti-finding

Rig is mature and well-liked. A pitch of "another Rig" has no pain to mine. The micro-crate proliferation is the real signal: people are rolling their own kernel underneath Rig's framework opinions, which suggests Rig is "too much" for a class of users who want **just the loop**.

---

## OSS Landscape (Rust)

### Summary table

| Project | Stars (approx) | Last activity | License | Relevance | Key strength | Key gap |
|---------|----------------|---------------|---------|-----------|--------------|---------|
| **Rig** (`0xPlaygrounds/rig`) | ~6,400 | active | MIT | Direct competitor | Trait-first, Tokio-native, broad provider/vector-store coverage, production users | Framework opinions; not a kernel |
| **Swiftide / swiftide-agents** (`bosun-ai/swiftide`) | ~628 | active | MIT | Direct competitor | Pluggable `ToolExecutor` (Docker), HITL primitives, Langfuse, RAG-native | RAG-coupled; not a clean kernel |
| **AutoAgents** (`liquidos-ai/autoagents`) | not surfaced | active | dual MIT/Apache-2.0 | Direct competitor | Ractor actor-model supervision trees, derive-macro tools, WASM sandbox, published benchmarks | Heavy abstraction; opinionated |
| **mistral.rs** | ~5,000+ | active | MIT | Adjacent | Agent loop bundled with inference engine | Server-side, model-coupled |
| **OpenFang** | "138 kLoC" | active | (varies) | Adjacent | Full "Agent OS" with scheduler/RBAC/budget | Too monolithic to be a crate |
| **Kalosm** | ~1,800 | active | MIT/Apache-2.0 | Adjacent | `#[derive(Parse, Schema)]` is gold-standard structured-output | Embedded-model-focused |
| **anchor-chain** | ~4 | abandoned (Jun 2024) | MIT | Abandoned | Statically-typed compile-time chains (sound idea) | Single-maintainer attrition |
| **llm-chain** | "fading" | low activity | MIT | Abandoned-ish | "LangChain port" framing | Framing didn't survive Rust idioms |
| **langchain-rust** | "alive" | active | MIT | Direct (struggling) | Surface compatible with LangChain Python | Issue #142: "async operations are not really async" — structural defect |
| **rustformers/llm** | "archived" | archived Jun 2024 | MIT | Abandoned | Model loading + quantization | Candle absorbed the differentiator |
| **swarms-rs** | not surfaced | active | MIT/Apache (mismatch in repo vs crates.io) | Adjacent | Multi-agent swarm orchestration | License mismatch worth flagging |
| **Axiom** | not surfaced | active | not surfaced | Adjacent | Sandboxed tool execution | UNCLEAR |
| **Synaptic / agent-io / sgr-agent / yoagent / mini-agent** | each <100 | varies | varies | The "micro-crate population" | Each solves the kernel for one user | Each duplicates ~80% of structure |

Full per-project detail in `_oss-landscape-rust.md`.

### Patterns observed across the landscape

- **Universal**: Tokio runtime, trait-first design, MIT or Apache-2.0 license, MCP support (becoming so in 2026).
- **Differentiation surfaces still open**:
  - **Supervision / fault-tolerance**: only AutoAgents (via Ractor) does it
  - **Statically-typed compile-time chains**: anchor-chain showed it's possible but died; opportunity remains
  - **Structured-output ergonomics**: Kalosm's `#[derive(Parse, Schema)]` is the gold standard; few others match it
  - **Tower middleware**: nobody has built a Service+Layer story for agent loops
- **Sandboxed tool execution is becoming table stakes**. Swiftide (Docker), AutoAgents/Axiom/OpenFang (WASM). A new crate without sandboxing will look behind in 2026.

### Abandoned attempts — lessons

- **rustformers/llm** (archived June 2024) — failed when candle absorbed its differentiator. Lesson: **don't bet on inference engine territory**; that race is run. Stay above the model.
- **anchor-chain** (dormant since June 2024, ~4 stars) — sound idea (statically typed chains) crushed by single-maintainer attrition. Lesson: **community signal matters**; ship something the micro-crate authors will adopt, or solo-maintain forever.
- **llm-chain** — "LangChain port" framing didn't survive Rust's move to trait-first async-native idioms. Lesson: **don't port Python idioms**; Rust idioms win in Rust.
- **langchain-rust** — open issue #142: "async operations are not really async." Lesson: **async-correctness is a contract**; faking it is structurally worse than admitting sync.

---

## Commercial / Non-Rust Landscape

Source: `_commercial-landscape.md` (~5,700 words). Highlights:

### Summary

| Framework | Lang | License/Pricing | Top abstraction | Best lesson for Rust |
|-----------|------|-----------------|-----------------|----------------------|
| **LangGraph** | Python | MIT | State graph (nodes + edges + state object) | Graphs for workflows, not chains |
| **LlamaIndex Workflows** | Python | MIT | Event-driven step decorators | Event-emit/handle decouples step orchestration |
| **Mastra** | TypeScript | MIT/Elastic | Agent + Workflow + RAG primitives | Layer separation: agent ≠ workflow |
| **CrewAI** | Python | MIT | Role + Task + Crew | Multi-agent is opinion-heavy; don't bake it into the kernel |
| **AutoGen / MAF** | Python/.NET (MSFT) | MIT | ConversableAgent | Microsoft has consolidated to MAF; AutoGen v0.4+ deprecates older shapes |
| **Semantic Kernel** | C#/Python/Java (MSFT) | MIT | Plugin + Planner | "Plugin" lost; "Tool" won |
| **Vercel AI SDK 6** | TypeScript | Apache-2.0 | `streamText` + inline tools | Streaming-first is the default expectation in 2026 |
| **Spring AI Agent Skills** | Java | Apache-2.0 | Skill + Prompt + ChatClient | Spring's verbosity is a cautionary tale |
| **DSPy** | Python | MIT/Apache-2.0 | Signature + Module + Optimizer | Three-tier separation is gold; even without shipping optimizer, the separation is valuable |
| **OpenAI Agents SDK** | Python | MIT | Agent + Tools + Guardrails + Sessions | The successor to deprecated Assistants — small, focused |
| **OpenAI Assistants** | API | per-token + storage | Thread + Run + Tool outputs | **Deprecated August 26, 2026** |
| **Anthropic Agent SDK** | TS/Python | MIT | Agent + MCP tools + Hooks + Memory | Hooks-at-named-lifecycle-points is the cleanest extensibility pattern in the space |
| **Anthropic Managed Agents** | API | per-session + storage | Hosted Agent | Hosted control plane, MCP-first |
| **AWS Bedrock AgentCore** | API | per-action + Lambda | Agent + Action Group + Knowledge Base | Action-centric pricing reveals market value |
| **Vertex AI Agent Builder** | API | per-query | Reasoning Engine + Tools | Less differentiated; Google in catch-up mode |
| **Langfuse / LangSmith / Helicone** | hosted | usage-based | Observability layer | Observability is a sellable layer above OSS agents |

### Patterns worth borrowing for Rust

1. **MCP-first tool interop** — Anthropic, OpenAI, Microsoft, Spring AI, Vercel all integrate MCP in 2026. New runtime should be MCP client + server first-class.
2. **Anthropic's lifecycle hooks** — named extension points (before_completion, after_tool, etc.) are the cleanest extensibility pattern. Swiftide ships 10 of them. Aim for ~5-7 in the kernel.
3. **DSPy's Signature/Module/Optimizer separation** — even without shipping an optimizer, separating "what the LLM call promises" from "how to execute it" from "how to tune it" is portable.
4. **Mastra's layer separation** — agent ≠ workflow ≠ RAG. Bake the boundary in.
5. **Vercel AI SDK's streaming-first default** — `stream_text` is the default; non-streaming is the opt-out. New runtime should match.
6. **Tool as the universal term** — Plugin/Skill/Function all lost. Call them Tools.
7. **CrewAI's anti-pattern** — multi-agent is opinion-heavy; don't bake it into the kernel. Make it a separate crate users opt into.

### Patterns to avoid

1. **Don't conflate agent / chain / workflow** — single most-repeated mistake (LangChain, AutoGen, Semantic Kernel all hit it)
2. **Don't ship an inference layer** — candle won that race
3. **Don't port LangChain's class hierarchy** — Rust idioms win in Rust
4. **Don't fake async** — langchain-rust's issue #142 is the cautionary tale
5. **Don't lock to one LLM provider** — every leaky abstraction in the multi-provider crates is a lesson

### Market signals

- **Pricing pattern**: market charges for *operations* (session-hours, traces, memory events, tool calls), not framework code. "Runtime open, operations paid."
- **Hosted vs self-host split**: hosted is winning revenue (Bedrock, Anthropic Managed Agents, OpenAI Agents), but self-hostable runtimes are how teams escape vendor lock-in. A self-hostable Rust runtime fits the escape-hatch demand.
- **Half-life is short**: OpenAI Assistants deprecated 24 months after launch; LangChain AgentExecutor EOL ~24 months. Keep public surface small.

---

## Public Pattern Literature

Source: `_pattern-literature.md` (~3,900 words). Highlights:

### Table-stakes patterns

A credible agent runtime in 2026 must support:

| Pattern | Citation | Why it's table-stakes |
|---------|----------|------------------------|
| **Conversation loop with tool calls** | OpenAI / Anthropic function calling specs | Universal |
| **ReAct (Reason+Act)** | Yao et al, arXiv:2210.03629 | The canonical iterative agent pattern |
| **Chain-of-Thought** | Wei et al, arXiv:2201.11903 | Foundational reasoning primitive (with caveats — Turpin and Lanham critique faithfulness) |
| **Tool result feedback into next turn** | All tool-using frameworks | Universal |
| **Streaming responses** | Every modern API | Vercel SDK shows the expectation |
| **MCP tool interop** | modelcontextprotocol.io/specification/2025-11-25 | Won the protocol race in 2026 |

### Differentiator opportunities

| Pattern | Citation | Opportunity |
|---------|----------|-------------|
| **Reflexion (generate→critique→revise)** | Shinn et al, arXiv:2303.11366 | Few Rust crates implement it as a first-class executor |
| **ReWOO (Plan-then-Execute, no observation)** | Xu et al, arXiv:2305.18323 | Reduces token spend by ~50%; underutilized in OSS implementations |
| **Plan-and-Solve** | Wang et al, arXiv:2305.04091 | Cleaner separation than ReAct for known-task workflows |
| **Tree of Thoughts** | Yao et al, arXiv:2305.10601 | Branching reasoning; not seen in any major Rust crate |
| **Constitutional / RLAIF guardrails** | Bai et al, arXiv:2212.08073 | Hookable safety layer |
| **Tower `Service` + `Layer` middleware** | tower-rs documentation | **Distinctive Rust angle**: no agent runtime executes on this cleanly |

### Critique literature worth heeding

- **CoT faithfulness**: Turpin et al (NeurIPS 2023, arXiv:2305.04388) and Lanham et al (arXiv:2307.13702) show CoT tokens are not always faithful reasoning. **Lesson**: treat Thought tokens as suggestive, not authoritative — don't predicate hard control flow on them.
- **Multi-agent skepticism**: Cognition AI's *"Don't Build Multi-Agents"* counter-positions against CrewAI/AutoGen. **Lesson**: ship single-agent first; multi-agent is a separate crate.

---

## Industry Barriers & Minimum Viable Features

### Industry barriers (price of admission, not features)

| Barrier | Why it's non-negotiable |
|---------|--------------------------|
| **Async-correct** | The langchain-rust counter-example shows fake-async is structural debt; pick sync OR async and honor the contract |
| **No data loss on tool failure** | Tools throw; the loop must not lose conversation history |
| **No silent cost overruns** | Cost cap with hard stop is expected by any production caller |
| **MCP client interoperability** | Tools-as-MCP is the 2026 interop standard |
| **Provider-agnostic core** | Locking to one LLM provider is a non-starter |
| **MIT or Apache-2.0 license** | Anything more restrictive blocks adoption |
| **Runs on stable Rust** | No nightly-only features in the core crate |

### Minimum viable features

**Must have (no adoption without these):**

- [ ] `Executor` trait with one method
- [ ] Conversation-loop executor (the canonical loop)
- [ ] ReAct executor
- [ ] `Tool` trait + `ToolRegistry` (typed args via derive macro)
- [ ] `LlmProvider` trait with at least OpenAI + Anthropic providers (mock provider for tests)
- [ ] Streaming responses (default)
- [ ] MCP client (consume MCP tool servers)
- [ ] Cost tracking + hard-stop cap
- [ ] Tracing via `tracing` crate
- [ ] 5-7 lifecycle hooks (before_completion, after_completion, before_tool, after_tool, on_error, on_complete)
- [ ] Mock backends for unit tests

**Should have (users will switch back without):**

- [ ] Reflection executor (generate→critique→revise)
- [ ] Plan-and-Solve executor
- [ ] MCP server (expose own tools as MCP)
- [ ] Sandboxed tool execution (WASM and/or process boundary)
- [ ] Conversation history persistence interface (`HistoryStore` trait)
- [ ] Scratchpad / shared-state interface (`Scratchpad` trait)
- [ ] Per-tool retry with backoff
- [ ] Token budget / context-window management

**Differentiators (worth switching for):**

- [ ] **Tower `Service` + `Layer` middleware integration** — no existing Rust agent crate ships this; every cross-cutting concern (cost, tracing, retry, rate-limit, deduplication, caching) becomes a `Layer` instead of a hook
- [ ] **Compile-time-typed tool args via derive macros** — `#[derive(Tool)]` + `Schema` so `serde_json::Value` is never the user's escape hatch
- [ ] **Multi-provider abstraction that doesn't leak** — provider-specific knobs surface via typed extension structs, not opaque maps
- [ ] **Embedded-friendly** — feature flags to compile without tokio (sync executor for pgrx-style hosts), without reqwest (custom transport), without serde-derive (lean)
- [ ] **MCP-first by default** — tools authored as MCP are zero-cost to consume; non-MCP tools are second-class wrappers

---

## Novelty & Differentiation Assessment

### Novelty level

**Better Implementation in a narrower scope.**

The space has a credible framework leader (Rig). A new entry in the framework category has no real angle. But the layer **below** Rig — the boring kernel of "executor + tool + provider + hooks" — has no clean entry. The micro-crate proliferation (`mini-agent`, `yoagent`, `sgr-agent`, `agent-io`, `Synaptic`) is the demand signal.

### The gap

There is no public Rust crate that:
1. Ships a minimal, unopinionated **kernel** (no RAG, no vector store, no multi-agent, no workflow engine)
2. Integrates with **Tower `Service` + `Layer`** middleware so cross-cutting concerns are not hand-rolled hooks
3. Treats **MCP** as first-class for both consuming and exposing tools
4. Gets **structured tool args** right via derive macros (matching Kalosm's bar for output, but for tool input)
5. Is embeddable in **non-tokio hosts** via feature flags
6. Has a public surface small enough to **fit on one screen**

### The angle (unfair advantage)

**Tower middleware as the cross-cutting concern story.** Every existing agent runtime ships ad-hoc hooks (Swiftide has 10, Anthropic has ~7). Tower's `Service` and `Layer` traits are the canonical Rust pattern for composable middleware. No agent crate has applied them cleanly. The result is an idiom Rust developers already know: cost tracking is a `Layer`, retry is a `Layer`, tracing is a `Layer`, MCP client is a `Layer`, dedup is a `Layer`.

This is the differentiator no Python framework can replicate (no equivalent in the Python ecosystem) and that addresses a real Rust developer pain (rolling your own middleware around `async-openai`).

### Risks (honest)

| Risk | Severity | Mitigation |
|------|----------|------------|
| Rig adds tower middleware before the new crate ships | High | Ship fast; tower compatibility is not their roadmap priority |
| Tower-as-middleware turns out to be the wrong shape (Service trait too generic for agent loops) | Medium | Prototype the middleware story first; spike before committing |
| The micro-crate authors don't migrate (each thinks their version is fine) | High | Ship migration shims; document the "from `mini-agent`" path |
| Market half-life is short — by the time the crate is mature, the standard moves | Medium | Keep surface small; let community own everything above the kernel |
| Solo-maintainer attrition (the anchor-chain failure mode) | High | Open-source from day one; recruit co-maintainers from the micro-crate community |
| Async-runtime religion war (sync hosts can't use tokio core) | Medium | Feature-flag the async dependency; provide sync executor variant |

### Verdict

**Build it.** The space has a leader and a long tail, and the gap between them is real and shaped like a kernel. The angle (tower middleware) is distinctive, Rust-idiomatic, and not copy-pasta-able from any other ecosystem. The risk profile is moderate (not "novel R&D"; "better implementation").

The path is **not** to compete with Rig at the framework level. The path is to ship a kernel that Rig itself could rebase on (and that the micro-crate community will adopt because the kernel is what they were rolling by hand).

---

## Code Deep-Dive

### Swiftide / swiftide-agents

Source: `https://raw.githubusercontent.com/bosun-ai/swiftide/master/swiftide-agents/src/agent.rs` (fetched 2026-05-13).

- **`Agent` is a concrete struct**, not a trait. Abstractions live behind the fields:
  - `context: Arc<dyn AgentContext>` — the runtime environment
  - `tools: HashSet<Box<dyn Tool>>` + `toolboxes: Vec<Box<dyn ToolBox>>`
  - `llm: Box<dyn ChatCompletion>` — the provider abstraction
- **Loop entry**: `pub async fn query(&mut self, query: impl Into<Prompt>) -> Result<(), AgentError>`, with variants `query_once`, `run`, `run_once`. Internally: `run_agent(maybe_query, just_once)`.
- **Tool dispatch**: `async fn invoke_tools(&mut self, tool_calls: &[ToolCall]) -> Result<(), AgentError>` — tools spawned as concurrent `tokio::spawn` tasks.
- **LLM abstraction**: `ChatCompletion` trait with `complete(req)` and `complete_stream(req)`.
- **Fully async** (tokio). State machine: running / stopped / pending. `pending_user_messages: VecDeque<String>` for in-flight queuing.
- **10 lifecycle hooks**: `before_all`, `on_start`, `on_stream`, `before_completion`, `after_completion`, `before_tool`, `after_tool`, `after_each`, `on_new_message`, `on_stop`.

**Observations:**
- 10 hooks is on the heavy side; the proposed kernel can ship with ~5 and let users add more via tower `Layer` rather than baked-in slots.
- Hooks-as-`Vec<Hook>` is invoked via `invoke_hooks!` macro — readable enough but tower middleware would be more composable.
- `HashSet<Box<dyn Tool>>` for tool storage implies `Hash + Eq` on `dyn Tool` — likely keyed on tool name. String-keyed dispatch is convergent across the ecosystem; typed dispatch via derive macros is the open angle.
- The struct has direct fields for `streaming: bool`, `limit: Option<usize>`, `tool_retry_limit: usize` (default 3) — these are agent-level config, not service-level. A tower middleware story would make these `Layer`s.

### Rig

WebFetch returned 404 on the documentation and source paths attempted (`docs.rs/rig-core/.../struct.Agent.html` and `raw.githubusercontent.com/0xPlaygrounds/rig/main/rig-core/src/agent.rs`). The OSS landscape doc summarizes the architecture from search snippets: trait-first, builder-heavy, Tokio-native, with provider crates (`rig-openai`, `rig-anthropic`, etc.) and a vector-store integration layer. Confirming the exact `Agent` shape from primary source is **deferred to a follow-up code-deep-dive pass** — flagged so the implementation phase does not assume any specific Rig API shape.

### AutoAgents

Not fetched. Per OSS landscape doc: Ractor actor-model supervision trees, derive-macro tools, WASM tool sandbox. The supervision-tree angle is interesting but heavier than the kernel scope.

---

## Data Sources

### OSS Rust
- See `_oss-landscape-rust.md` for the full bulleted source list (~30 URLs covering GitHub repos, crates.io pages, and third-party references for the 17 crates surveyed)

### Commercial / non-Rust
- See `_commercial-landscape.md` for the full source list (~30 URLs covering official docs, announcements, and pricing pages)

### Pattern literature
- See `_pattern-literature.md` for arXiv DOIs and canonical URLs for the 13 named patterns

### User workflow + pain
- See `_user-workflow-pain.md`. Note: GitHub issue trackers and Reddit site-restricted search were not accessible to the research agent; direct user-complaint quotes from r/rust and from rig/swiftide issue trackers are missing and would benefit from a follow-up evidence pass

### Code deep-dive
- `https://raw.githubusercontent.com/bosun-ai/swiftide/master/swiftide-agents/src/agent.rs` — Swiftide agent.rs (fetched, summarized above)
- Rig source — **deferred**, 404 on attempted paths; needs `gh repo view` or a different URL
