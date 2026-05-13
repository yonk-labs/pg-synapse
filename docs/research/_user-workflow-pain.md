# Current Rust Agent-Loop Workflow + Pain Points (2026-05)

Scope: research pass into how Rust developers build agent loops today, and what
hurts. Sources are public — GitHub repos, blog posts, dev.to articles,
documentation, HN threads. Where a claim lacks a primary source, it is marked
UNCLEAR rather than asserted.

## The trigger

A Rust developer reaches for an agent loop when they need an LLM to do more than
one round-trip. Common scenarios surfaced in the public material:

- **Coding agents / autonomous developer tools.** Pi-agent-rust, Hermes, Claude
  Code's Rust port, the AutoAgents demo apps, and various "build a coding agent
  in Rust" tutorials all fit here. Strong-typed tool dispatch + filesystem
  effects + multi-turn planning is the dominant use case.
- **Production agents handling real user data 24/7.** The Refresh Agent post
  ("a marketer built an AI agent with Axum, SQLite, and Rig") is the
  canonical "Rust pays for itself in operations" narrative — token-refresh
  logic, OAuth, long-running daemons.
- **RAG + retrieval pipelines** that grow into something more agentic — the
  origin of Swiftide, which started as a streaming indexing library and added
  agents in 0.16.
- **Sovereign/edge/WASM deployment** where Python's runtime weight is
  disqualifying. Quoted memory data: AutoAgents and Rig sit under 1.1 GB peak,
  vs >4.7 GB for Python frameworks measured (Zylos, dev.to benchmark).
- **Web3 / oracles / on-chain agents** — Anda, rs-graph-llm, the chains-aware
  forks of Rig.
- **CLI tools** wrapping an LLM (yoagent, sgr-agent, mini-agent, agent-io —
  small crates that exist because the "wrap a loop around async-openai"
  problem keeps getting solved from scratch).

So the trigger is real and recurring, but the *shape* of the trigger varies
enough that no one library has yet become "the obvious choice" the way
LangChain (briefly) was for Python.

## Today's workflow

### Path A: roll your own over async-openai (or reqwest directly)

**What it looks like.** Pull in `async-openai` (the de-facto unofficial Rust
OpenAI client) or `anthropic-sdk-rust`, write a struct that holds a `Vec<Message>`,
manually serialize tool definitions into the API's JSON schema shape, write a
match arm that reads `tool_calls` off the response and dispatches by string name
into a `HashMap<String, Box<dyn Fn(...)>>`-ish thing, append the tool result
back into the history, loop.

**Who picks this.** People who only need OpenAI (or only Anthropic), people
who don't want a framework, and — overwhelmingly — people building public
"build an agent in Rust" tutorials. Shuttle's "Agentic RAG with Rust" walkthrough,
the Coinmonks tutorial, "Building an AI Agent with Rig and Rust in 10 Minutes,"
and the mini-agent / yoagent / sgr-agent crates all encode this pattern.

**Pain points cited.**

- **Tool dispatch is string-keyed and runtime-unsafe.** No framework, no derive
  macro — you re-implement `name -> handler` dispatch by hand and convert
  `serde_json::Value` to your typed args inside each handler. The proliferation
  of micro-crates that exist *only* to wrap this pattern (mini-agent, yoagent,
  agent-io, sgr-agent — all <500 stars, all 2025-2026) is the signal: a lot of
  people keep solving the same problem from scratch.
  - Source: <https://github.com/RajMandaliya/mini-agent>, <https://lib.rs/crates/yoagent>,
    <https://docs.rs/sgr-agent>, <https://docs.rs/agent-io>.
- **Retries and rate limits mid-stream are genuinely hard.** Raj Mandaliya's
  "Building a Rust AI Agent Framework from Scratch — What I Learned" calls
  out: *"rate limits arriving halfway through streaming responses create
  partial state issues that took longer than any other single piece of the
  codebase to get right."*
  - Source: <https://dev.to/rajmandaliya/building-a-rust-ai-agent-framework-from-scratch-what-i-learned-3o23>.
- **Schema generation for tool args is manual.** async-openai's recent README
  acknowledges users want "`parse::<T>()` with auto-generated JSON schema" but
  no single crate ships that + WebSockets + WASM together. Mentioned in
  "I ported the OpenAI Python SDK to Rust in 5 days" (dev.to/fortunto2).
- **`serde_json::Value` everywhere.** async-openai supports "bring your own
  type" specifically because users hit deserialization errors against
  OpenAI-compatible providers that drift from spec. The escape hatch is "use
  `serde_json::Value` instead of typed structs" — which moves the safety from
  compile-time to runtime and defeats half the reason to choose Rust.
  - Source: <https://github.com/64bit/async-openai> README, issue
    <https://github.com/64bit/async-openai/issues/436>.
- **You lose multi-provider support the moment you commit to async-openai.**
  Want to swap to Anthropic? Different SDK, different message shape, different
  tool-call envelope. Most "roll your own" code ends up either OpenAI-locked
  or with a hand-built abstraction layer.

### Path B: use Rig

**What it looks like.** Add `rig-core`, declare an `Agent` with a system prompt
and a list of tools (each implementing the `Tool` trait), call `prompt()` or
`chat()`. Rig manages the loop, the provider abstraction, and (as of late 2025)
optional persistent conversation memory via `rig-memory`.

**Who picks this.** This is the de-facto first choice in 2026 for Rust devs
who want a framework. Rig is the most-cited Rust LLM library on dev.to and the
flagship of the Zylos "Rust-Native AI Agent Frameworks 2026" report.

**Pain points cited.**

- **Function-calling, vision, and tool-use were still on the TODO list in
  some 2025 docs.** From BrightCoding's Rig overview: *"some cutting-edge LLM
  features like function-calling, vision, and tool-use are still todo."* By
  early 2026 most of these landed, but the API surface has been moving fast.
  - Source: <https://www.blog.brightcoding.dev/2025/09/28/building-modular-llm-powered-apps-with-rig-a-rust-framework-overview/>.
- **Provider abstraction leaks.** Rig wraps providers behind a unified
  interface, but provider-specific features (Anthropic's `cache_control`,
  Gemini's thinking tokens, OpenAI's strict structured outputs) tend to need
  escape hatches or to lag the upstream API. This is a structural complaint
  echoed against `genai` and most multi-provider Rust crates.
- **API churn.** Rig published a "stable API and substantial documentation in
  late 2025 / early 2026" (Zylos), which is another way of saying it wasn't
  stable before that. Anyone who built on early Rig has a migration story.
- **No widely-cited "Rig is broken" thread.** This is genuinely notable —
  search for "Rig rust frustrating" / "Rig hard to use" / "Rig too complex"
  returns the project's own promotional material and tutorials, not
  complaints. Either users are happy, or the user base is still small enough
  that complaints haven't aggregated. UNCLEAR which.

### Path C: use Swiftide

**What it looks like.** Build a streaming RAG pipeline first, then (as of
0.16, late 2025) add agents that can call tools and other agents. Heavier
on async-stream + tokio primitives than Rig; OpenTelemetry tracing baked in.

**Who picks this.** Teams who started in RAG and grew into agents, or who want
the streaming-pipeline model as the core abstraction. The maintainer (Timon
van Spronsen, bosun-ai) ships frequent releases.

**Pain points cited.**

- **Self-acknowledged early-stage.** The Swiftide README and docs page state
  plainly: *"Swiftide is under heavy development and can have breaking
  changes... documentation might fall short of all features and despite
  efforts be slightly outdated."*
  - Source: <https://github.com/bosun-ai/swiftide>.
- **Breaking changes between versions.** 0.9 → 0.16 → 0.26 (streaming agents)
  involved real refactors. Maintainer notes the AI space "is moving very
  fast" which justifies the churn but doesn't make it less painful to migrate.
  - Source: <https://dev.to/timonv/swiftide-016-brings-ai-agents-to-rust-3o17>,
    <https://dev.to/timonv/swiftide-026-streaming-agents-1j2h>.
- **Documentation gaps.** Self-acknowledged. No specific user complaint thread
  found — UNCLEAR whether this is biting users or whether maintainers are just
  being humble.
- **Smaller community than Rig.** Fewer tutorials, fewer "I built X with
  Swiftide" posts. Discord-driven rather than issue-tracker-driven feedback.

### Path D: call Python from Rust (PyO3 or sidecar)

**What it looks like.** Two flavors:

1. **PyO3 in-process.** Embed CPython, call LangChain / LlamaIndex / the
   official OpenAI Python SDK from Rust. The llm-rs-python project is a
   reference for the inverse direction (Python calling Rust), but the same
   bindings work both ways. Wim Henderickx's "Combining Rust and Python for
   Extensible Automation Systems" documents the pattern.
2. **Python sidecar service.** Stand up a FastAPI/gRPC service in Python that
   owns the LLM/agent layer; the Rust binary calls it over HTTP or a Unix
   socket. Pi-agent-rust uses a SQLite sidecar for session state, which is
   a milder version of the same idea.

**Who picks this.** Shops with substantial existing Python ML infrastructure
who don't want to port. Also: people who tried Path A or B, hit a wall on a
provider-specific feature (PydanticAI structured outputs, LangGraph's state
machine), and bailed out rather than rewrite.

**Pain points cited.**

- **GIL kills concurrency.** Red Hat's "Why some agentic AI developers are
  moving code from Python to Rust" makes this the headline argument — in a
  multi-agent Python system, only one agent can "think" at a time because of
  the GIL. PyO3 inherits this when calling into CPython.
  - Source: <https://developers.redhat.com/articles/2025/09/15/why-some-agentic-ai-developers-are-moving-code-python-rust>.
- **Deployment complexity.** You now ship two runtimes, two dependency trees,
  two CVE surfaces. The "Rust as glue / Python as core" pattern moves the
  problem rather than solving it.
- **Latency tax for sidecar.** Every tool call crosses a process boundary.
  Acceptable for many uses, fatal for low-latency interactive agents.

The fact that this path is *common enough to write articles about* — but no
one celebrates it — is itself a pain signal.

### Path E: skip Rust entirely

**What it looks like.** Build the agent in TypeScript (Vercel AI SDK,
Mastra, LangChain.js) or Python (LangGraph, PydanticAI, the OpenAI Agents
SDK, CrewAI), even when the rest of the system is Rust. Pi-agent-rust is
a Rust agent specifically because Claude Code is in TypeScript and the
author wanted the "Rust port" experience — but Claude Code's existence in
TS at all is the signal.

**Who picks this and why.**

- **Ecosystem maturity.** Quoted from the dev.to "Python vs Go vs Rust"
  2026 piece: *"LangGraph's state machine approach, CrewAI's role-based
  agents, and AutoGen's multi-agent conversations are proven patterns
  with thousands of production deployments. The Go/Rust equivalents are
  1-2 years behind in maturity."*
  - Source: <https://dev.to/thedailyagent/python-vs-go-vs-rust-for-ai-agents-in-2026-a-pragmatic-field-guide-5fda>.
- **Provider SDKs.** OpenAI's Python and TypeScript SDKs are first-party;
  Rust SDKs (async-openai, anthropic-sdk-rust, anthropic-rs) are all
  unofficial. New API features land in the official SDKs first, in Rust
  weeks or months later (or never, for niche features).
- **Tutorials, blogs, Stack Overflow.** Search "how to fine-tune an LLM"
  → Python notebooks. Search "how to implement a ReAct loop" → Python
  + LangGraph examples first, Rust deep in the results.
- **Armin Ronacher's "A Language For Agents" (Feb 2026)** explicitly argues
  the agent ecosystem still defaults to dynamic, fuzzy-data-tolerant
  languages — Python absorbs LLM output unpredictability that Rust's type
  system rejects at compile time.
  - Source: <https://lucumr.pocoo.org/2026/2/9/a-language-for-agents/>.

This is the largest cohort and the most painful for any new Rust crate to
displace, because the choice often happens before Rust is even evaluated.

## Pain inventory (ranked by severity + frequency)

| Rank | Pain | Severity | Frequency | Source URLs |
|------|------|----------|-----------|-------------|
| 1 | Tool dispatch is string-keyed; typed args require boilerplate or hand-rolled macros | High | Widespread (multiple micro-crates exist just to wrap this) | <https://github.com/RajMandaliya/mini-agent>, <https://dev.to/rajmandaliya/building-a-rust-ai-agent-framework-from-scratch-what-i-learned-3o23>, <https://docs.rs/sgr-agent>, <https://lib.rs/crates/yoagent> |
| 2 | Multi-provider abstraction leaks (provider-specific features lag) | High | Widespread across rig/genai/swiftide | <https://docs.rs/genai>, <https://crates.io/crates/genai>, <https://github.com/0xPlaygrounds/rig> |
| 3 | Schema generation for structured outputs / tool args is manual | High | Widespread | <https://github.com/64bit/async-openai/issues/436>, OpenAI community thread on strict json_schema |
| 4 | Retries / rate-limit handling mid-stream is hard to get right | High | Cited by multiple builders | <https://dev.to/rajmandaliya/building-a-rust-ai-agent-framework-from-scratch-what-i-learned-3o23>, refreshagent.com post |
| 5 | API churn / breaking changes in Rig and Swiftide | Medium | Real for early adopters | swiftide README, Zylos 2026 report |
| 6 | Context window / memory management left to the user (except Rig's late-2025 `rig-memory` and Pi's compaction algorithm) | Medium | Widespread | <https://github.com/Dicklesworthstone/pi_agent_rust>, hermes-agent-windows-rust |
| 7 | Observability requires hand-rolled OpenTelemetry spans (no auto-instrumentation like Python has) | Medium | Widespread | base14 Scout Rust LLM observability guide, Laminar Claude-agent-sdk proxy blog |
| 8 | Async runtime lock-in (tokio assumed everywhere; sync embedding awkward) | Medium | Implicit, widely felt | tokio.rs bridging-with-sync docs cited often |
| 9 | Documentation gaps in Swiftide and rapid Rig API churn | Medium | Acknowledged by maintainers | swiftide README, Rig CONTRIBUTING |
| 10 | Official LLM-vendor SDKs lag (Rust SDKs are all unofficial) | Medium | Structural | <https://github.com/64bit/async-openai>, <https://github.com/AbdelStark/anthropic-rs>, <https://crates.io/crates/anthropic-sdk-rust> |
| 11 | Soft-cap budget / max-turn enforcement is approximate, not exact | Low | Specific to Claude-Agent-SDK-Rust | <https://docs.rs/claude-agent-sdk>, <https://laminar.sh/blog/2025-12-03-claude-agent-sdk-instrumentation> |
| 12 | Black-box subprocess observability when wrapping vendor CLIs (Claude Code) | Medium | Specific to "wrap the CLI" path | Laminar blog above |

## Workarounds people invented

- **Hand-rolled `Tool` traits with a `serde_json::Value` arg adapter.** Every
  micro-framework (mini-agent, yoagent, sgr-agent, agent-io, Synaptic's
  `#[tool]` macro) is a workaround for the missing "typed tool dispatch" in
  async-openai. Synaptic's `#[tool]` proc-macro is the cleanest version of
  the workaround I found.
- **`bring your own type` escape hatch.** async-openai added this specifically
  because users were hitting deserialization errors against OpenAI-compatible
  providers (Groq, OpenRouter, Together, etc.) whose response shapes drift.
  Documented in the README as a feature, but it's really a hack admitting the
  typed model can't cover the real provider zoo.
- **Internal forks of Rig.** UNCLEAR with public evidence, but Zylos's 2026
  ecosystem report describes Rig's API as "moving fast through late 2025,"
  which strongly implies early adopters were pinning specific git revs and
  carrying patches. Mentioned but not proven.
- **PyO3 bridges to LangChain.** llm-rs-python's `langchain` extra is a
  literal package that exists to let Python LangChain see Rust LLM code.
  The inverse direction — Rust agent calling Python LangChain — is built
  by hand and reported in the Wim Henderickx "Combining Rust and Python"
  Medium piece.
- **Python sidecar over gRPC/HTTP.** No single canonical reference, but the
  pattern shows up in Red Hat's piece and in Pi-agent-rust (SQLite sidecar
  for session state — a milder version).
- **Pi's compaction algorithm** (preserves user-turn boundaries, summarizes
  older messages when token estimate exceeds `context_window - reserve`) is
  itself a workaround for "no framework gives me context-window management."
  - Source: <https://github.com/Dicklesworthstone/pi_agent_rust>.
- **Wrapping the vendor CLI as a subprocess.** Claude-Agent-SDK-Rust wraps
  the Claude Code CLI. Laminar instrumented it with a "tiny Rust proxy"
  *outside* the SDK because the subprocess is opaque. This is a workaround on
  top of a workaround.
  - Source: <https://laminar.sh/blog/2025-12-03-claude-agent-sdk-instrumentation>.

## Failure modes of current options

- **Async runtime mismatch.** Tokio is assumed everywhere. Calling an async
  LLM client from a sync embedding (a Tauri command handler, a CLI tool that
  doesn't want `#[tokio::main]`, a WASM target without tokio) requires
  `block_on` gymnastics or a dedicated runtime, with the classic "cannot
  start a runtime from within a runtime" footgun. (tokio.rs bridging-with-sync
  docs are linked from every Rust-LLM Stack Overflow question that touches
  this.)
- **String-keyed tool dispatch is the dominant pattern.** The OpenAI tool-call
  envelope is *fundamentally* `{ "name": "...", "arguments": "<json>" }` —
  Rust has to bridge that to typed handlers somehow, and every framework
  picks a slightly different bridge. No standard.
- **Provider abstraction is leaky by design.** OpenAI's `tool_choice`,
  Anthropic's `cache_control`, Gemini's `thinkingConfig`, xAI's `live_search`
  — all of these are provider-only knobs. Multi-provider crates (rig, genai,
  llm-connector, litellm-rs) face a forced choice: expose every provider's
  knobs (and abandon the unified abstraction) or hide them (and force users
  to drop into raw HTTP). All current crates compromise; none win.
- **State externalization is opt-in at best.** Rig has `ConversationMemory`
  with an `InMemoryConversationMemory` default and a `rig-memory` companion
  crate — *available* but not the default surface. Swiftide's agents carry
  context but persistence is up to the user. Pi-agent-rust ships a SQLite
  sidecar because nothing else does. Anyone building a multi-process or
  resumable agent is writing the persistence layer themselves.
- **Observability is hand-rolled.** base14's Scout guide is explicit: *"Unlike
  Python or Node.js, Rust has no auto-instrumentation libraries for LLM SDKs,
  requiring manual spans following OpenTelemetry GenAI semantic conventions
  and custom metrics for token and cost tracking."* Rig and Swiftide both
  emit tracing spans; the burden of mapping them to GenAI conventions and
  shipping to Langfuse/Phoenix/Arize is the user's.
- **Memory / context window left to the user.** Pi-agent-rust's
  compaction algorithm exists *because* nothing else handles it. Rig added
  sliding-window and token-budget shapers in `rig-memory` (late 2025) which
  is progress, but not parity with how LangGraph / OpenAI Agents SDK / Claude
  Agent SDK handle it.

## The anti-finding

A real version exists and must be flagged:

- **Rig is genuinely well-regarded.** Public reviews are positive
  ("5/5 from 676 reviews" per aiagentsdirectory.com, multiple "I built X
  with Rig in 10 minutes" tutorials). Search for "Rig rust frustrating"
  returns no public complaint thread of any size.
- **2026 is the year the Rust ecosystem "crossed from experimental to
  production-viable"** (Zylos). Rig, AutoAgents, OpenFANG all shipping
  stable APIs. The pain that existed in 2024 is reduced, not eliminated.
- **The most-cited frustrations cluster on `async-openai` + roll-your-own
  loops, not on the frameworks.** Devs who pick Rig or Swiftide are
  largely satisfied; devs who try to skip the framework keep reinventing
  the same wheel.

So the honest read is: the *framework* layer (Rig especially) is healthy.
The *primitive* layer (async-openai + ergonomic typed tool dispatch) is not,
and that's why micro-crates keep appearing.

What's missing in the space:

- A small, ergonomic primitive that gives typed tool dispatch + retries +
  context management *without* the framework opinion. Something between
  "raw async-openai" and "full Rig agent."
- Better story for sync / non-tokio embedding.
- An official-feeling provider abstraction that doesn't leak. Probably
  impossible — but every existing attempt compromises here.

If your new crate's pitch is "another Rig," there is no pain to mine. If
it's "the boring kernel of an agent loop with no opinions about RAG or
multi-agent" then the micro-crate proliferation is your TAM signal.

## Data sources

- <https://github.com/0xPlaygrounds/rig>
- <https://github.com/bosun-ai/swiftide>
- <https://github.com/64bit/async-openai>
- <https://github.com/64bit/async-openai/issues/436>
- <https://github.com/Abraxas-365/langchain-rust>
- <https://github.com/RajMandaliya/mini-agent>
- <https://github.com/liquidos-ai/AutoAgents>
- <https://github.com/Dicklesworthstone/pi_agent_rust>
- <https://github.com/jeremychone/rust-genai>
- <https://github.com/EricLBuehler/mistral.rs>
- <https://github.com/Wally869/claude_agent_sdk_rust>
- <https://github.com/AbdelStark/anthropic-rs>
- <https://crates.io/crates/anthropic-sdk-rust>
- <https://crates.io/crates/genai>
- <https://crates.io/crates/mini-agent>
- <https://lib.rs/crates/yoagent>
- <https://docs.rs/sgr-agent>
- <https://docs.rs/agent-io>
- <https://rig.rs/>
- <https://swiftide.rs/>
- <https://zylos.ai/research/2026-04-01-rust-native-ai-agent-frameworks-ecosystem-2026>
- <https://dev.to/saivishwak/benchmarking-ai-agent-frameworks-in-2026-autoagents-rust-vs-langchain-langgraph-llamaindex-338f>
- <https://dev.to/rajmandaliya/building-a-rust-ai-agent-framework-from-scratch-what-i-learned-3o23>
- <https://dev.to/timonv/swiftide-016-brings-ai-agents-to-rust-3o17>
- <https://dev.to/timonv/swiftide-026-streaming-agents-1j2h>
- <https://dev.to/thedailyagent/python-vs-go-vs-rust-for-ai-agents-in-2026-a-pragmatic-field-guide-5fda>
- <https://dev.to/fortunto2/squeezing-every-millisecond-from-the-openai-api-in-rust-4b11>
- <https://dev.to/joshmo_dev/implementing-design-patterns-for-agentic-ai-with-rig-rust-1o71>
- <https://dev.to/0thtachi/rig-a-rust-library-for-building-llm-powered-applications-3g75>
- <https://dev.to/brooks_wilson_36fbefbbae4/claude-code-architecture-explained-agent-loop-tool-system-and-permission-model-rust-rewrite-41b2>
- <https://developers.redhat.com/articles/2025/09/15/why-some-agentic-ai-developers-are-moving-code-python-rust>
- <https://lucumr.pocoo.org/2026/2/9/a-language-for-agents/>
- <https://refreshagent.com/engineering/building-ai-agents-in-rust>
- <https://www.blog.brightcoding.dev/2025/09/28/building-modular-llm-powered-apps-with-rig-a-rust-framework-overview/>
- <https://laminar.sh/blog/2025-12-03-claude-agent-sdk-instrumentation>
- <https://docs.base14.io/guides/ai-observability/rust-llm-observability/>
- <https://hackmd.io/@Hamze/Hy5LiRV1gg>
- <https://news.ycombinator.com/item?id=41709436> (Comparing Rust-based RAG to LangChain)
- <https://users.rust-lang.org/t/rust-for-ai-agents/136946>
- <https://mdwla.substack.com/p/revisiting-rust-in-2026>

UNCLEAR / not surfaced with primary sources (would need direct GitHub-issue
crawl, which was blocked in this pass):

- Specific feature-request volume on rig and swiftide issue trackers.
- r/rust threads about LLM agent work (Reddit's site-restricted search
  returned nothing usable in this pass).
- Stack Overflow questions tagged `rust` + `openai` (volume + topic).
- Internal-fork evidence for Rig (suspected but not proven).
