# Rust OSS Agent-Runtime Landscape (2026-05)

> Intermediate research doc for `/research-and-design`. Pulled from public READMEs,
> crates.io listings, release notes, blog posts, and Hacker News / DEV.to discussions.
> Where a number is approximate or could not be verified directly (because WebFetch
> was disabled in this session), it is marked `~` or "approx, as of last search".
> Every claim has a corresponding URL in the **Data sources** section at the bottom.

---

## Summary table

| Project | Stars (approx) | Latest release | License | Relevance | Key strength | Key gap |
|---|---|---|---|---|---|---|
| **0xPlaygrounds/rig** (`rig-core`) | ~6.4k | v0.36 (May 2026, ~772k all-time downloads) | MIT | **Direct competitor** | Mature trait-based provider abstraction, broad ecosystem (20+ providers, 10+ vector stores), Tokio-native | API still pre-1.0; minor releases break; opinionated builder ergonomics not loved by everyone |
| **bosun-ai/swiftide** (`swiftide-agents`) | ~628 | swiftide-agents v0.32.1, swiftide-core v0.13.2 (2026) | MIT | **Direct competitor** (agent + RAG) | Pluggable `ToolExecutor` (local / Docker / remote), human-in-the-loop, streaming agents, Langfuse observability | Heavy workspace surface area; agent crate younger than core indexing crates; pre-1.0 breaking changes |
| **liquidos-ai/AutoAgents** | not surfaced in search snippets (newer, late-2025/early-2026 launch) | autoagents v0.2+ (Mar 2026) | Dual (Apache-2.0 / MIT — typical Rust) | **Direct competitor** | Ractor actor-model concurrency, supervision trees, WASM tool sandbox, derive-macro tools, ReAct + Basic executors | Newer project, less battle-tested; actor model adds conceptual overhead vs. plain async |
| **EricLBuehler/mistral.rs** | (large, inference engine) | mistralrs crate active 2026 | MIT | **Adjacent** (inference + server-side agent loop) | Built-in server-side agent loop with MCP client, web search, HTTP tool dispatch; runs models locally | Tied to mistral.rs's own runtime; not a general agent library you'd embed standalone in arbitrary apps |
| **RightNow-AI/openfang** | new (March 2026 launch) | v0.5.10 (2026) | MIT | **Adjacent** ("Agent OS" — larger scope than a crate) | 14-crate workspace, scheduler, RBAC, budgets, 53 tools, MCP/A2A, WASM sandbox, OpenAI-compatible API | ~138 kLoC monolith — likely overkill if you just want an agent loop primitive |
| **Abraxas-365/langchain-rust** | ~1.2k | v4.6.0 (Oct 2025) | MIT | **Partial** (chains, agents, tools) | Familiar LangChain mental model, many integrations, active dep updates | Known issue: "async operations are not really async" (#142) — CPU-bound calls block Tokio; thin async story |
| **sobelio/llm-chain** | ~1.4k | v0.10.x (~2024 era; some 2026 activity) | MIT | **Partial / fading** | Early Rust LLM workflow library, prompt templates, simple tools (bash/python/web) | Largely overtaken by Rig/Swiftide; sparse agent loop; release cadence slowed |
| **a-agmon/rs-graph-llm** | small/medium | active 2025–2026 | MIT | **Adjacent** (graph workflow, builds on Rig) | LangGraph-style stateful task graph with HITL, pluggable DB session storage | Sits on top of Rig — not a standalone runtime; positioned as workflow not raw agent loop |
| **The-Swarm-Corporation/swarms-rs** | small (early Rust port of Python "swarms") | v0.1.5 (crates.io) | MIT / Apache-2.0 (mismatched between repo & crates.io) | **Partial** | Production-orchestration framing, concurrent multi-agent | Very early, low version, license discrepancy, single-author-led brand |
| **fcn06/swarm** | small | active 2026 | (permissive, not fully confirmed) | **Adjacent** | MCP + A2A first-class, JSON config-driven workflows, Planner/Specialist pattern | Single maintainer; depends on `a2a-rs` (small ecosystem); planner-centric design may not fit all loops |
| **yarenty/kowalski** | small | v0.5.0 line (2026) | (permissive, not fully confirmed) | **Partial / personal scope** | Local-first (Ollama + OpenAI-compatible), pgvector + Apache AGE memory, MCP, Vue operator UI | Single maintainer; in "active refactor" phase; agent crates collapsed into one — moving target |
| **emersonmde/anchor-chain** | ~4 (tiny) | last commit ~Jun 2024 | (MIT/Apache; lib.rs) | **Abandoned-leaning** | Statically typed, compile-time-checked chains; nice type-level idea | Single dev, basically dormant since mid-2024 |
| **AdamStrojek/rust-agentai** (`agentai`) | ~32 | crates.io `agentai` active | MIT | **Adjacent / hobby** | Thin wrapper over `genai` crate, simple ToolBox API | Tiny project, "interface may change at any time", limited scope |
| **graniet/llm** | medium | active 2026 | (permissive) | **Adjacent** | Unifies many backends (OpenAI/Claude/Gemini/Ollama/ElevenLabs incl. TTS/STT), backoff, evaluation, REST server, reactive agents | Crate name reused — prior crate archived; broad scope dilutes the "agent runtime" focus |
| **axiom-rust/axiom** | small/new | recent 2025–2026 | MIT | **Partial** | Streaming-first, separate `axiom-agents`, WASM sandbox, monitoring/tracing baked in | New entrant; ecosystem reach unproven; overlaps heavily with Rig + AutoAgents |
| **floneum/kalosm** | medium | Kalosm 0.4 (2025) | MIT/Apache-2.0 | **Adjacent** (local inference + structured generation) | `#[derive(Parse, Schema)]` for structured outputs, candle-based local models, parser-engine sampler | Not primarily an "agent loop" library; tool/loop story thinner than Rig/Swiftide |
| **rustformers/llm** | (large historical) | archived Jun 24 2024 | MIT/Apache-2.0 | **Abandoned** | Pioneer of Rust LLM inference | Archived by owner; superseded by candle and mistral.rs |

> 17 projects surveyed. The user asked for 5–10; the count is high to give a complete
> map. The **directly relevant agent-runtime contenders** are Rig, Swiftide-Agents,
> AutoAgents, and (as a higher-level reference) OpenFang.

---

## Detailed analysis

### Rig (`0xPlaygrounds/rig`, crate `rig-core`)

- **Repo:** https://github.com/0xPlaygrounds/rig
- **Crates.io:** https://crates.io/crates/rig-core — v0.36.0 (latest snapshot), 54 versions published, ~772,549 all-time downloads (approx, as of last search).
- **Stars / Forks:** ~6.4k stars (approx); v0.31 announcement in Feb 2026 explicitly targeted the 6k milestone.
- **Last release:** Active; v0.31 announced Feb 2026; v0.36 reflected on crates.io.
- **License:** MIT.
- **Maintainer profile:** **Org-backed.** 0xPlaygrounds (the "Playgrounds" group, also publishes `rig-onchain-kit` for Solana/EVM). Multiple contributors; visible adoption (ilert, Neon's app.build V2 Rust reboot, Cairnify, St Jude chatbot, Coral Protocol, VT Code).
- **What it does:** A composable, trait-driven LLM application framework. Provides a unified interface across 20+ providers (OpenAI, Anthropic, Cohere, Gemini, Groq, Ollama, Bedrock, EternalAI, etc.), 10+ vector stores (Mongo, Postgres, Qdrant, Neo4j, Milvus, HelixDB, LanceDB, FastEmbed), agentic workflows, multi-turn streaming, prompting, and RAG.
- **Architecture:**
  - Core traits: `CompletionModel`, `EmbeddingModel`, `VectorStore`, `Tool`. All async. Each provider has a `Client` struct that constructs typed completion/embedding models.
  - Tool dispatch: from v0.22 onward, "tool servers" — Tokio-spawned tasks with message passing — solve the async tool storage problem.
  - Async runtime: Tokio. Streaming is first-class (parallel embedding jobs, concurrent vector upserts).
  - Companion crates per integration (`rig-postgres`, `rig-bedrock`, `rig-fastembed`, `rig-milvus`, `rig-helixdb`, `rig-eternalai`, `rig-extra`...) keep the root crate light.
- **Strengths:**
  - Clean trait surface that swaps providers/stores via single-line changes.
  - Production adoption (ilert, Neon, Coral, VT Code) — strongest of the Rust pack.
  - "rig facade" pattern: use `rig` for feature-gated everything, or `rig-core` alone for minimal footprint.
  - Built explicitly for Tokio with stable async-fn-in-traits idioms.
- **Gaps / limitations:**
  - Pre-1.0: every minor release can break — explicitly stated in the README.
  - Older docs flagged function-calling / vision / tool-use as TODO; those have since shipped, but the speed of churn is real.
  - Opinionated builder ergonomics — some users find them heavy compared to plain async fns.
  - Crypto/Solana adjacency (rig-onchain-kit, Playgrounds branding) makes some non-crypto teams wary; the core is neutral.
- **Relevance:** **Direct competitor.** This is the project a new Rust agent-runtime crate has to beat or differentiate against.

---

### Swiftide / swiftide-agents (`bosun-ai/swiftide`)

- **Repo:** https://github.com/bosun-ai/swiftide
- **Crates.io:** `swiftide` (umbrella), `swiftide-core` v0.13.2, `swiftide-agents` v0.32.1, plus `swiftide-indexing`, `swiftide-query`, `swiftide-integrations`, `swiftide-docker-executor`.
- **Stars / Forks:** ~628 (approx).
- **Last release:** Active. Recent blog posts: Swiftide 0.16 introduced agents, 0.26 added streaming agents, 0.27 added human-in-the-loop, 0.31 added Tasks / Langfuse / multi-modal.
- **License:** MIT.
- **Maintainer profile:** **Org-backed.** bosun.ai (company; "Automated Software Maintenance"). Swiftide powers Bosun's own codebase intelligence product. Lead maintainer Timon V. (visible across blog + DEV posts).
- **What it does:** Started as a streaming async RAG indexing/query library; swiftide-agents added in 0.16. Now spans: simple prompt completion → streaming indexing pipelines → query pipelines → agents that call tools and other agents → HITL flows → Langfuse observability.
- **Architecture:**
  - Workspace with explicit separation: `swiftide-core` (primitives), `swiftide-indexing`, `swiftide-query`, `swiftide-agents`, `swiftide-integrations`.
  - Agent has a context, the context has a `ToolExecutor` — local by default, but `swiftide-docker-executor` ships a Docker variant; remote executors are pluggable.
  - "ToolBox" pattern — any toolbox is an addable bundle; MCP services and lists-of-tools are both ToolBoxes.
  - Trait-heavy: loaders, transformers, storages all extend simple traits.
  - Async: Tokio-based, streams everywhere.
- **Strengths:**
  - Strongest "tool execution sandbox" story of the pack (Docker executor out-of-the-box).
  - Human-in-the-loop is a designed primitive, not bolted on.
  - Indexing + agents in one library means agents have first-class RAG inputs.
  - Langfuse integration for observability.
- **Gaps / limitations:**
  - Self-described: "under heavy development and can have breaking changes while working towards 1.0."
  - Workspace surface area is large — onboarding cost higher than Rig.
  - Agent crate is younger than core indexing crates; some API churn there specifically.
- **Relevance:** **Direct competitor**, especially if the new crate also wants pluggable tool execution / sandboxing.

---

### AutoAgents (`liquidos-ai/AutoAgents`, crate `autoagents` / `autoagents-core`)

- **Repo:** https://github.com/liquidos-ai/AutoAgents
- **Crates.io:** `autoagents`, `autoagents-core`, `autoagents-llm`, `autoagents-llamacpp` (March 2026 era publications).
- **Stars / Forks:** not surfaced in WebSearch snippets; benchmarking writeups from Q1 2026 treat it as one of the three leading Rust frameworks alongside Rig and OpenFang — suggests a meaningful audience, though stars are likely well below Rig.
- **Last release:** Active. HN/Dev.to coverage in late 2025 / early 2026.
- **License:** Search snippets say "You may choose either license" — typical Rust dual MIT/Apache-2.0. Verify in repo.
- **Maintainer profile:** **Org-backed.** LiquidOS (commercial sponsor; "Open-Source Agent SDK in Rust"). Also publishes `Odyssey` (packaging/securing/operating portable agents).
- **What it does:** Type-safe multi-agent framework with structured tool calling, configurable memory, pluggable LLM backends, ReAct + Basic executors out of the box, and a WASM tool sandbox.
- **Architecture:**
  - Uses **Ractor** (Erlang/OTP-style actor model in Rust). Each agent is an actor with its own mailbox, state, and message handlers. **No shared mutable state** between agents.
  - **Supervision trees:** parent supervises children; on panic, can restart / escalate / terminate sub-tree.
  - **Executor trait:** ships `BasicExecutor` (single turn) and `ReActExecutor` (iterative reasoning + tool calls). New executors implement a single trait.
  - **Derive macros:** `#[derive(Tool)]` on functions and `#[derive(AgentOutput)]` on structs — type-safe tools without manual schema wiring.
  - **Memory:** sliding window default, extensible backends.
  - **Typed pub/sub:** structured message passing between agents with compile-time type safety; publish/subscribe topics for decoupling.
  - **WASM sandbox** for executing untrusted tools.
  - WASM compilation of the runtime itself — agents in the browser.
- **Strengths:**
  - Best-in-class supervision / fault-tolerance story among Rust agent libs.
  - Derive-macro ergonomics are the cleanest of the pack for tool definition.
  - Concrete published benchmarks vs LangChain/LangGraph/CrewAI/LlamaIndex (claimed 4.97 rps vs ~2.7–3.7 for Python frameworks; ~24% CPU; <1.1 GB peak memory).
  - WASM sandboxing for untrusted tools is a differentiator.
- **Gaps / limitations:**
  - Younger than Rig — adoption mostly via blog posts and benchmarks, not yet broad production references.
  - Actor model adds a learning curve for teams comfortable with plain async/await.
  - Single org sponsor — bus factor risk if LiquidOS shifts focus.
- **Relevance:** **Direct competitor.** The reference design to study for a clean-room rebuild's *concurrency / supervision* layer.

---

### mistral.rs (`EricLBuehler/mistral.rs`, crate `mistralrs`)

- **Repo:** https://github.com/EricLBuehler/mistral.rs
- **Crates.io:** `mistralrs`.
- **License:** MIT.
- **Maintainer profile:** Eric Buehler (lead) + contributors; well-known in the Rust LLM inference space.
- **What it does:** A fast, flexible LLM inference engine. **It also ships an agentic layer**: server-side tool-call loop with built-in web search (`search_the_web`, `website_content_extractor`), MCP client, callback-based tool dispatch (Python + Rust SDK), and HTTP tool dispatch (server POSTs to a configured endpoint).
- **Architecture:**
  - Inference engine first; agent loop is a server-side loop that runs tools and feeds results back to the model until it produces a final answer.
  - Tool dispatch order: built-in search tools → registered callbacks → tool-dispatch URL.
  - Rust SDK: `builder.with_tool_callback(name, callback)`.
- **Strengths:**
  - **The agent loop lives next to the model** — zero client round-trips, very low latency for tool-heavy workloads.
  - OpenAI-compatible tool calling across HTTP / Python / Rust SDKs.
- **Gaps / limitations:**
  - You bring mistral.rs's inference runtime — coupling that an embedded agent library would normally avoid.
  - Not framed as a general "build any agent" library — it's an inference server with agent capabilities.
- **Relevance:** **Adjacent.** Important reference for the "inference engine ships the agent loop" pattern (also seen with vLLM / Ollama on the Python side).

---

### OpenFang (`RightNow-AI/openfang`)

- **Repo:** https://github.com/RightNow-AI/openfang
- **Site:** https://www.openfang.sh/
- **License:** MIT.
- **Maintainer profile:** RightNow-AI (org-backed). Open-sourced March 1, 2026.
- **What it does:** **An "Agent Operating System,"** not just a crate. 14 Rust crates, ~137,728 lines of code, ~32MB single binary with bundled "Hands" (autonomous capability packages), 40 channel adapters, 38–53 built-in tools, 16 security systems, WASM sandbox, MCP, A2A, OpenAI-compatible API.
- **Architecture (named crates):**
  - `openfang-kernel` — orchestration, workflows, metering, RBAC, scheduler, budget tracking.
  - `openfang-runtime` — agent loop, 3 LLM drivers, 53 tools, WASM sandbox, MCP, A2A.
  - `openfang-api` — 140+ REST/WS/SSE endpoints, OpenAI-compatible API, dashboard.
  - `openfang-channels` — 40 messaging adapters with rate limiting.
- **Strengths:**
  - Most ambitious Rust agent system shipped to date.
  - Production primitives most others lack: scheduler, budget tracking, RBAC, metering, dashboard.
- **Gaps / limitations:**
  - **Massive footprint** — ~138 kLoC is overkill if all you need is an embeddable agent loop crate.
  - Pre-1.0 (v0.5.10) — likely still stabilising.
  - "Operating system" framing implies opinionated lock-in.
- **Relevance:** **Adjacent.** A reference architecture for scheduler / budget / RBAC primitives, but too monolithic to compete with a "single crate" agent runtime directly.

---

### langchain-rust (`Abraxas-365/langchain-rust`)

- **Repo:** https://github.com/Abraxas-365/langchain-rust
- **Crates.io:** `langchain-rust` v4.6.0 (Oct 2025), ~137k downloads, 68 versions.
- **Stars:** ~1.2k.
- **License:** MIT.
- **Maintainer profile:** Single-author led (Abraxas-365) with active community contributions; renovate bot running for deps.
- **What it does:** A Rust port of LangChain — chains, agents, tools, document loaders, vector store integrations. Familiar LangChain mental model.
- **Architecture:** Chains-and-runnables surface; agent abstractions with tool integration (web search, command execution); integrates Qdrant, Postgres, SurrealDB; supports OpenAI, Azure, Anthropic, Ollama.
- **Strengths:**
  - Familiarity for teams migrating from Python LangChain.
  - Many built-in integrations.
- **Gaps / limitations:**
  - **Issue #142: "async operations are not really async"** — CPU-bound work blocks Tokio because the library wasn't designed async-first. The reporter notes a "non-trivial refactor" is needed. This is a *structural* limitation, not a missing feature.
  - Inherits LangChain's reputation issues (abstraction sprawl, ambiguous APIs).
- **Relevance:** **Partial.** Useful for migration ergonomics, not for being the cleanest Rust-native design.

---

### llm-chain (`sobelio/llm-chain`)

- **Repo:** https://github.com/sobelio/llm-chain
- **Stars:** ~1.4k.
- **License:** MIT (typical; not confirmed from snippet).
- **Maintainer profile:** sobelio (company-affiliated origin); 7 open PRs, 222 closed.
- **What it does:** Early-Rust LLM chain library — prompt templates, multi-step chains, tools (bash, python, web).
- **Architecture:** Chain-first, tool execution attached; pre-dates the trait-driven Rig/Swiftide patterns.
- **Strengths:** Early to market; reasonable simple-chain UX.
- **Gaps / limitations:** Largely overtaken by Rig and Swiftide for serious agent work; release cadence has slowed; agent loop is rudimentary.
- **Relevance:** **Partial / fading.** Useful historical reference for "what an early Rust LangChain port looked like."

---

### rs-graph-llm (`a-agmon/rs-graph-llm`)

- **Repo:** https://github.com/a-agmon/rs-graph-llm
- **License:** MIT.
- **What it does:** LangGraph-style stateful task graph for multi-agent workflows in Rust. Pluggable storage backends, session management, step-by-step / batch / mixed execution, human-in-the-loop.
- **Architecture:** Built **on top of Rig** for the LLM/agent layer. The crate provides `graph-flow` (the graph engine) and example services.
- **Strengths:** Brings LangGraph's workflow design to Rust; production-oriented (DB schema, sessions, HITL).
- **Gaps / limitations:** Not a standalone runtime — depends on Rig; smaller community.
- **Relevance:** **Adjacent.** Important if the new crate needs a graph orchestration layer; otherwise an example of how to stack on top of an agent primitive.

---

### swarms-rs (`The-Swarm-Corporation/swarms-rs`)

- **Repo:** https://github.com/The-Swarm-Corporation/swarms-rs
- **Crates.io:** `swarms-rs` v0.1.5.
- **License:** MIT in repo, Apache-2.0 listed on crates.io — **mismatch flagged.** Verify before depending.
- **Maintainer profile:** Author Kye Gomez (also runs the Python `swarms` brand). Single-person-led brand presence.
- **What it does:** Multi-agent orchestration framework; near-zero latency claims, modular agents.
- **Architecture:** Early — v0.1.x. Concurrent multi-agent emphasis.
- **Strengths:** Production framing; rides the broader swarms.ai brand.
- **Gaps / limitations:** Very early version; license inconsistency; small Rust ecosystem footprint vs. the Python sibling.
- **Relevance:** **Partial.**

---

### fcn06/swarm

- **Repo:** https://github.com/fcn06/swarm
- **What it does:** Agent SDK with first-class **MCP** and **A2A** support; static-workflow (JSON-defined) and self-correcting agent-team modes; Planner/Specialist conductor pattern.
- **Architecture:** Built on the MCP Rust SDK and `a2a-rs`. Static workflow JSON is the default; Agent Factory pattern for spinning up agents.
- **Strengths:** Standards-first (MCP + A2A) — useful reference if the new crate wants to plug into those ecosystems.
- **Gaps / limitations:** Solo maintainer; small dependency ecosystem; planner-centric design is opinionated.
- **Relevance:** **Adjacent.**

---

### kowalski (`yarenty/kowalski`)

- **Repo:** https://github.com/yarenty/kowalski
- **Crates.io:** `kowalski`, `kowalski-core`, `kowalski-cli`, `kowalski-code-agent`, etc. v0.5.0 line.
- **Maintainer profile:** Single maintainer (Jaroslaw Nowosad / yarenty).
- **What it does:** Local-first multi-agent framework — Ollama + OpenAI-compatible APIs, MCP tools, optional Postgres memory (pgvector + Apache AGE graph), federation hooks, Vue operator UI.
- **Architecture:** Modular crates: `kowalski-core` (now the unified `TemplateAgent`), `kowalski-cli`, `kowalski-mcp-datafusion`. Recently collapsed specialized agent crates (`kowalski-web-agent`, `kowalski-code-agent`) into one template.
- **Strengths:** Local-first, zero Python deps, MCP-native, Vue operator UI is unusual for Rust crates.
- **Gaps / limitations:** Single maintainer; explicitly in "active refactor / hardening" — moving target; project breadth (agent core + UI + datafusion + pgvector + graph queries) wide for one person.
- **Relevance:** **Partial.**

---

### anchor-chain (`emersonmde/anchor-chain`)

- **Repo:** https://github.com/emersonmde/anchor-chain
- **Stars:** ~4 (tiny).
- **Last commit:** ~June 23, 2024.
- **License:** MIT/Apache (typical).
- **What it did:** Statically typed framework for LLM workflows — compile-time-checked chains, async parallel processing, OpenSearch RAG.
- **Strengths (design):** Type-level chain composition catches mismatches at compile time — a Rust-idiomatic differentiator others haven't replicated.
- **Gaps / limitations:** Solo dev; dormant since mid-2024; never reached critical mass.
- **Relevance:** **Abandoned-leaning** but valuable as a design reference for **statically typed chain composition** if the new crate wants that property.

---

### rust-agentai / agentai (`AdamStrojek/rust-agentai`)

- **Repo:** https://github.com/AdamStrojek/rust-agentai
- **Crates.io:** `agentai`.
- **Stars:** ~32.
- **License:** MIT.
- **What it does:** Thin wrapper over the `genai` crate (multi-LLM client) with a simple `ToolBox` API and a `#[tool]`-style derive surface.
- **Strengths:** Minimal surface area; easy to read.
- **Gaps / limitations:** Self-flagged "under heavy development, interface may change at any time"; small scope; hobby-tier.
- **Relevance:** **Adjacent / hobby.**

---

### graniet/llm (`graniet/llm`)

- **Repo:** https://github.com/graniet/llm
- **What it does:** Unifies many LLM + voice backends (OpenAI / Anthropic / Ollama / Gemini / DeepSeek / xAI / Phind / Groq / OpenRouter / Cohere / ElevenLabs / Google) under one API. Multi-step chains, templates, evaluation/scoring, parallel multi-provider racing, function calling, REST server in OpenAI-compatible format, reactive agents with shared memory.
- **Strengths:** Voice + LLM unified is unusual; built-in evaluation / multi-LLM race.
- **Gaps / limitations:** **Crate name was reused** — the prior `llm` crate is archived and unrelated; potential confusion. Scope is broad ("everything backend") which dilutes the agent runtime focus.
- **Relevance:** **Adjacent.**

---

### axiom-rust/axiom

- **Repo:** https://github.com/axiom-rust/axiom
- **Crates:** `axiom-ai-core`, `axiom-ai-llm`, `axiom-ai-agents`.
- **License:** MIT.
- **What it does:** Streaming-first LangChain alternative. Workspace split: `axiom-core` (traits), `axiom-llm` (provider gateway), `axiom-agents` (agent framework), `axiom-rag`, `axiom-wasm` (WASM sandbox). Built-in monitoring (metrics / tracing / health / alerting).
- **Strengths:** Streaming + WASM sandbox + observability bundled.
- **Gaps / limitations:** New entrant; overlaps heavily with Rig and AutoAgents; unproven adoption.
- **Relevance:** **Partial.**

---

### kalosm / floneum (`floneum/floneum`)

- **Repo:** https://github.com/floneum/floneum
- **Crates.io:** `kalosm`, `kalosm-language-model`, `kalosm-llama`, `kalosm-ocr`, etc.
- **License:** MIT/Apache-2.0.
- **What it does:** Local-first model interface (candle-based) with **structured generation** — `#[derive(Parse, Schema)]` on any Rust type, custom parser-engine sampler, structure-aware acceleration. Floneum is the visual workflow editor on top.
- **Strengths:** Best-in-Rust structured-output ergonomics; local inference covered.
- **Gaps / limitations:** Not primarily an agent loop; tool/loop machinery is thinner than Rig/Swiftide; visual editor (Floneum) is a different audience.
- **Relevance:** **Adjacent.** Strong reference for structured output / typed parsing.

---

### rustformers/llm — **archived**

- **Repo:** https://github.com/rustformers/llm
- **Archived:** June 24, 2024.
- **Why:** Maintainers cited lack of time and resources. Plan had been to switch backend to candle, but candle itself absorbed GGUF/GGML model support and quantization fast enough to obsolete `llm`. Mistral.rs and Ratchet were named as continuations.
- **Lesson:** When a fast-moving lower layer (candle) absorbs your differentiator (model loading + quantization), your wrapper layer's value collapses. **An agent-runtime crate should not bet its differentiation on a layer below it that's also moving fast.**
- **Relevance:** **Abandoned** — historical reference.

---

## Abandoned attempts (lessons)

### rustformers/llm
Already covered above. Lesson: don't build the differentiator at a layer where a faster-moving crate (here, candle) will overtake you.

### anchor-chain
Single maintainer, dormant since mid-2024, only ~4 stars. Lesson: **statically typed compile-time chain composition is a real differentiator but requires sustained authorship.** If the new crate wants this property, the type-level machinery has to be built in *and maintained* — solo projects can't carry it.

### llm-chain (fading rather than dead)
Last meaningful releases were 2024; activity slowed as Rig and Swiftide gained traction. Lesson: **a "LangChain port" framing didn't survive the move to trait-first, async-native Rust idioms.** The new crate should not lean on "we are Rust LangChain"; that framing is an anchor pulling down, not lifting up.

---

## Patterns observed across the landscape

**Async runtime.** Universal Tokio. None of the serious contenders use async-std (deprecated) or sync. New crate should be Tokio-native.

**Trait vs builder.** Trait-first wins. Rig, Swiftide, AutoAgents all expose **provider/tool/store as traits**, with builders as ergonomic wrappers. Builder-only designs (early llm-chain) felt clunky and got overtaken.

**LLM provider abstraction.** Two patterns:
1. **Single trait, many impls** (Rig: `CompletionModel`). Each provider crate implements the trait. **Wins on swap-ability.**
2. **Many small per-provider crates with their own types**, federated by a meta-crate (`graniet/llm`, `axiom-llm`). **Wins on per-provider feature coverage** but loses on swap-ability.

The new crate should choose deliberately. Rig's single-trait approach is the modal pick.

**Tool dispatch.**
- **Typed via derive macros** (AutoAgents `#[derive(Tool)]`, Kalosm `#[derive(Parse, Schema)]`) — best ergonomics.
- **String-keyed via registries** (langchain-rust style) — flexible but loses compile-time safety.
- **Async tool storage problem:** Rig solved it with "tool servers" (Tokio-spawned actor-style tasks with message passing). Worth studying — async tools are surprisingly hard in Rust.
- **Sandboxing:** AutoAgents and Axiom ship WASM tool sandboxes; Swiftide ships a Docker `ToolExecutor`; OpenFang has WASM + 16 security systems. **Sandboxing is becoming table stakes for a serious agent runtime in 2026.**

**State management.** Two camps:
- **Message-log as state** (Kalosm doc + Rig style): everything is messages; state is reconstructible from history; easy replay/persistence/compression.
- **Actor mailboxes** (AutoAgents via Ractor): each agent owns its state; supervision tree manages lifecycle.

These are not mutually exclusive — the new crate could log messages *and* run agents as actors.

**Observability.** Langfuse integration is the de-facto standard (Swiftide ships it). OpenTelemetry / tracing is universal. Axiom bakes in metrics + alerting. The new crate must at minimum emit `tracing` spans for: model calls, tool calls, agent steps, errors. Langfuse export is a "yes by 1.0" item.

**Concurrency / supervision.** AutoAgents (Ractor) is the only mainstream Rust agent crate with real supervision trees. The others rely on plain Tokio + caller-supplied retries. **Supervision is an open differentiation surface** for a new crate.

**Multi-agent / protocols.** MCP support is required (Swiftide, AutoAgents, fcn06/swarm, OpenFang, kowalski all ship it). A2A is emerging (fcn06/swarm, OpenFang). **Skipping MCP in 2026 would be a positioning miss.**

**Streaming.** Swiftide 0.26 ("Streaming agents") and Rig (parallel/streaming first-class) treat streaming as a primary concern, not a feature. A new crate needs streaming on day one — bolted-on streaming is painful (langchain-rust illustrates this).

**License.** MIT or MIT/Apache-2.0 dual. **No GPL/AGPL/copyleft found in any major contender.** New crate should match.

**Maintainer profile.** Three viable profiles in this market:
1. **Org-backed open source** (Rig/Playgrounds, Swiftide/bosun.ai, AutoAgents/LiquidOS, OpenFang/RightNow-AI). Sustained, multi-contributor.
2. **Solo / hobby** (anchor-chain, agentai, kowalski). Higher bus-factor risk; users hesitate to adopt.
3. **Crypto-adjacent** (Rig's onchain kit, swarms-rs brand) — adoption ceiling for non-crypto teams.

A clean-room rebuild should aim for profile 1 even if it starts as 2.

---

## Data sources

- https://github.com/0xPlaygrounds/rig
- https://github.com/0xPlaygrounds/rig/discussions/1406 (Rig v0.31 release)
- https://github.com/0xPlaygrounds/rig/releases
- https://github.com/0xPlaygrounds/rig/blob/main/README.md
- https://github.com/0xPlaygrounds/rig-onchain-kit
- https://docs.rig.rs/
- https://docs.rig.rs/docs/concepts/tools
- https://crates.io/crates/rig-core
- https://docs.rs/rig-core/latest/rig/index.html
- https://lib.rs/crates/rig-core
- https://rig.rs/
- https://www.blog.brightcoding.dev/2025/09/28/building-modular-llm-powered-apps-with-rig-a-rust-framework-overview/
- https://github.com/bosun-ai/swiftide
- https://github.com/bosun-ai/swiftide/blob/master/AGENTS.md
- https://github.com/bosun-ai/swiftide/graphs/contributors
- https://github.com/bosun-ai/swiftide/issues
- https://github.com/bosun-ai/swiftide/stargazers
- https://bosun-ai.github.io/swiftide/
- https://swiftide.rs/
- https://swiftide.rs/core-concepts/indexing-pipeline/
- https://blog.bosun.ai/swiftide-0-16/ (agents introduced)
- https://dev.to/timonv/swiftide-026-streaming-agents-1j2h
- https://bosun.ai/posts/swiftide-0-27/ (HITL)
- https://blog.bosun.ai/swiftide-0-31/ (Tasks, Langfuse, multi-modal)
- https://crates.io/crates/swiftide
- https://crates.io/crates/swiftide-core
- https://crates.io/crates/swiftide-agents
- https://crates.io/crates/swiftide-docker-executor
- https://docs.rs/swiftide-agents/latest/swiftide_agents/
- https://github.com/liquidos-ai/AutoAgents
- https://liquidos-ai.github.io/AutoAgents/
- https://liquidos.ai/blog/autoagents-release/
- https://crates.io/crates/autoagents
- https://crates.io/crates/autoagents-core
- https://crates.io/crates/autoagents-llm
- https://lib.rs/crates/autoagents-core
- https://deepwiki.com/liquidos-ai/AutoAgents
- https://dev.to/saivishwak/autoagents-a-rust-based-multi-agent-framework-for-llm-powered-intelligence-27h2
- https://dev.to/saivishwak/benchmarking-ai-agent-frameworks-in-2026-autoagents-rust-vs-langchain-langgraph-llamaindex-338f
- https://dev.to/harshal_rembhotkar/case-study-liquidoss-autoagents-building-smarter-ai-agents-in-rust-20nl
- https://news.ycombinator.com/item?id=45582935 (AutoAgents HN)
- https://github.com/EricLBuehler/mistral.rs
- https://github.com/EricLBuehler/mistral.rs/blob/master/docs/TOOL_CALLING.md
- https://ericlbuehler.github.io/mistral.rs/TOOL_CALLING.html
- https://crates.io/crates/mistralrs
- https://docs.rs/mistralrs/latest/mistralrs/
- https://github.com/RightNow-AI/openfang
- https://www.openfang.sh/
- https://www.sitepoint.com/openfang-rust-agent-os-performance-benchmarks/
- https://github.com/Abraxas-365/langchain-rust
- https://github.com/Abraxas-365/langchain-rust/releases
- https://github.com/Abraxas-365/langchain-rust/issues/142 (async-not-really-async)
- https://crates.io/crates/langchain-rust
- https://docs.rs/crate/langchain-rust/latest
- https://github.com/sobelio/llm-chain
- https://github.com/sobelio/llm-chain/releases
- https://docs.rs/llm-chain
- https://www.shuttle.dev/blog/2024/06/06/llm-chain-langchain-rust
- https://github.com/a-agmon/rs-graph-llm
- https://github.com/The-Swarm-Corporation/swarms-rs
- https://crates.io/crates/swarms-rs/0.1.5
- https://lib.rs/crates/swarms-rs
- https://github.com/fcn06/swarm
- https://github.com/yarenty/kowalski
- https://dev.to/yarenty/kowalski-the-rust-native-agentic-ai-framework-53k4
- https://crates.io/crates/kowalski
- https://crates.io/crates/kowalski-core
- https://github.com/emersonmde/anchor-chain
- https://docs.rs/anchor-chain
- https://crates.io/crates/anchor-chain
- https://github.com/AdamStrojek/rust-agentai
- https://crates.io/crates/agentai
- https://github.com/graniet/llm
- https://github.com/graniet/rllm
- https://docs.rs/llm/latest/llm/
- https://lib.rs/crates/llm
- https://github.com/axiom-rust/axiom
- https://lib.rs/crates/axiom-ai-llm
- https://lib.rs/crates/axiom-ai-agents
- https://lib.rs/crates/axiom-ai-core
- https://github.com/floneum/floneum
- https://floneum.com/blog/kalosm_0_4/
- https://docs.rs/kalosm
- https://crates.io/crates/kalosm
- https://crates.io/crates/kalosm-language-model
- https://github.com/rustformers/llm
- https://github.com/rustformers/llm/discussions/435 (project continuation discussion)
- https://github.com/jondot/awesome-rust-llm (curated list)
- https://hackmd.io/@Hamze/Hy5LiRV1gg (Rust LLM ecosystem overview)
- https://zylos.ai/research/2026-04-01-rust-native-ai-agent-frameworks-ecosystem-2026
- https://aarambhdevhub.medium.com/why-im-exploring-agentic-ai-in-rust-and-you-should-too-916f2ac6c413
- https://users.rust-lang.org/t/rust-for-ai-agents/136946
- https://www.morphllm.com/ai-agent-framework
