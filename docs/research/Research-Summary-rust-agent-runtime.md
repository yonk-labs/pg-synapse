# Research Summary — Rust Agent-Runtime Crate

**Date:** 2026-05-13
**Audience:** the operator deciding whether and how to build a new Rust agent-runtime crate, clean-room of any private codebase.

## One-Liner

Ship a minimal, unopinionated agent-loop kernel for Rust — the layer below Rig — with `tower::Service` + `Layer` middleware as the distinctive design choice, MCP-first tool interop, and typed tool args via derive macros, so the long tail of community micro-crates has something to adopt instead of rolling their own.

## The Problem Today (2-3 sentences)

Rust developers building agent loops in 2026 face a forced choice between adopting Rig's framework opinions (the dominant crate, ~6.4k stars, but framework-shaped) or hand-rolling a 500-2,000 LOC kernel over `async-openai` (the path observed across `mini-agent`, `yoagent`, `sgr-agent`, `agent-io`, `Synaptic`, and others — each implementing ~80% of the same loop). The cross-cutting concerns (cost tracking, tracing, retry, MCP integration, provider-specific knobs) get hand-rolled into hooks or ad-hoc wrappers in every crate, and the typed-tool-args story is broken everywhere — `serde_json::Value` is the escape hatch that defeats half the reason to use Rust. There is no public crate that targets the kernel layer with tower-shaped middleware and typed dispatch.

## What Already Exists

**OSS Rust** (17 crates surveyed; full list in Research-Report):
- **Rig** — framework leader. MIT, ~6.4k stars, production users. Trait-first, Tokio-native, broad provider/vector-store coverage. Not a kernel.
- **Swiftide / swiftide-agents** — RAG-native option. MIT, ~628 stars. 10 lifecycle hooks, Docker-sandboxed tool executor, Langfuse observability. RAG-coupled.
- **AutoAgents** — actor-model option. Dual MIT/Apache. Ractor supervision, WASM tool sandbox, derive macros, published benchmarks. Heavy.
- **mistral.rs** — adjacent. Agent loop bundled with inference engine. Model-coupled.
- **OpenFang** — adjacent. ~138 kLoC "Agent Operating System." Too monolithic.
- **Kalosm** — adjacent. `#[derive(Parse, Schema)]` is the gold-standard structured output. Embedded-model-focused.
- **anchor-chain** (abandoned), **llm-chain** (fading), **rustformers/llm** (archived) — graveyard lessons.
- **The micro-crate population** (`mini-agent`, `yoagent`, `sgr-agent`, `agent-io`, `Synaptic`) — each <100 stars, each solving the kernel for one user, each duplicating ~80% of the structure. **This is the demand signal.**

**Commercial / non-Rust** (16 frameworks/platforms surveyed): LangGraph, LlamaIndex Workflows, Mastra, CrewAI, AutoGen/MAF, Semantic Kernel, Vercel AI SDK, Spring AI Agent Skills, DSPy, OpenAI Agents SDK, Anthropic Agent SDK, Bedrock AgentCore, Vertex AI Agent Builder, Langfuse/LangSmith/Helicone. Key facts:
- **MCP has won** as the tool interop protocol in 2026 (Anthropic, OpenAI, Microsoft, Spring AI, Vercel all integrate)
- **OpenAI Assistants API deprecated August 26, 2026** — direct adoption opportunity for a self-hostable runtime
- **LangChain `AgentExecutor` EOL December 2026** — public surface churn risk; keep yours small
- **Market converged on "Tool"** as the term — Plugin/Skill/Function all lost
- **Pricing pattern**: market charges for *operations*, not framework code; runtime is open, observability/sessions are paid

**Pattern literature** (13 canonical sources): ReAct, Reflexion, ReWOO, Plan-and-Solve, Toolformer, CoT, ToT, Constitutional AI, MCP spec, decorator pattern (GoF). All public, all citable. CoT-faithfulness critiques (Turpin, Lanham) flag a real failure mode: treat Thought tokens as suggestive, not authoritative.

## The Gap

There is no Rust crate that:

1. Ships a **minimal unopinionated kernel** — executor + tool + provider + ~5 hooks, no RAG, no vector store, no multi-agent, no workflow engine.
2. Integrates with **`tower::Service` + `Layer`** so cost / tracing / retry / rate-limit / dedup / caching / MCP are composable middleware, not hand-rolled hooks.
3. Treats **MCP** as first-class for both consuming and exposing tools.
4. Gets **typed tool args** right via derive macros (Kalosm-level ergonomics, but for tool input rather than output).
5. Is embeddable in **non-tokio hosts** via feature flags (matters for pgrx-style sync hosts, embedded targets, WASM, anyone in async-runtime religion conflict).
6. Has a public surface small enough to **fit on one screen**.

## Minimum Viable Scope

**v0.1 must have:**
- `Executor` trait + conversation-loop executor + ReAct executor
- `Tool` trait + `ToolRegistry` (typed args via `#[derive(Tool)]`)
- `LlmProvider` trait + OpenAI + Anthropic providers + mock provider
- Streaming responses (default)
- MCP client
- Cost cap with hard stop
- `tracing` integration
- 5-7 lifecycle hooks: `before_completion`, `after_completion`, `before_tool`, `after_tool`, `on_error`, `on_complete`
- Mock backends for unit tests
- README + crate-level rustdoc + 3 worked examples (`examples/{minimal,with_tools,with_tower_layers}.rs`)
- MIT or Apache-2.0 (match the ecosystem)

**v0.2 should have:**
- Reflection executor (generate→critique→revise)
- Plan-and-Solve executor
- MCP server
- WASM-sandboxed tool execution (feature flag)
- `HistoryStore` trait + in-memory impl
- `Scratchpad` trait
- Per-tool retry with backoff (as a tower Layer)
- Token budget / context-window management (as a tower Layer)

**Differentiators (the unfair advantage):**
- Tower middleware integration — every cross-cutting concern is a `Layer`
- Compile-time-typed tool args
- Non-leaking multi-provider abstraction (provider knobs surface via typed extension structs, not opaque maps)
- Feature-flag-able for non-tokio embedding

## Key Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Rig adds tower middleware before this ships | High | Ship fast; tower compat is not on Rig's roadmap |
| Tower turns out wrong-shaped for agent loops | Medium | Prototype the middleware story before committing to it |
| Micro-crate authors don't migrate | High | Ship migration shims; document the "from `mini-agent`" path explicitly |
| Solo-maintainer attrition (the anchor-chain failure) | High | Open-source from day one; recruit co-maintainers from the micro-crate community |
| Standard moves before maturity (LangChain v1 → v2 → v3 in ~24 months) | Medium | Keep surface small; let community own everything above kernel |
| Async runtime religion (sync hosts excluded) | Medium | Feature-flag tokio; provide sync executor variant |

## Novelty Verdict

**Better Implementation in a narrower scope.** Not novel R&D; not already solved. The space has a leader (Rig) and a long tail (micro-crates), and the gap between them is real and shaped like a kernel. The angle — tower middleware integration — is distinctive, Rust-idiomatic, and not copy-pasta-able from any other ecosystem.

## Recommended Next Steps

1. **`/brainstorming`** — scope the new crate from first principles using the Research-Report as the public-source anchor. Lock the name, the public surface (target one screen), and the v0.1 feature list explicitly. This step also locks the **what we explicitly do NOT do** list (no RAG, no vector store, no multi-agent, no workflow engine — those are downstream crates).
2. **`/mission-brief`** — convert the brainstorm output into Purpose / Success Criteria / Constraints / Out-of-Scope / Drift Checkpoints / Testing Requirements. This becomes the contract.
3. **`/research-and-design --refresh`** — only if the brainstorm surfaces a design choice this research didn't cover (e.g., the exact tower middleware shape, or a specific MCP feature). Defer unless needed.
4. **`/writing-plans`** — implementation plan that imports the mission brief and produces the build sequence. Recommend a 4-milestone shape:
   - M0: tower-middleware spike (prove the architecture works)
   - M1: kernel (Executor trait + Tool trait + LlmProvider trait + 1 executor + 1 provider + mock LLM)
   - M2: feature breadth (ReAct, MCP client, second provider, lifecycle hooks)
   - M3: docs + 3 examples + crates.io publish
5. **Decide repo + license + name** before any code: standalone repo (recommended for clean-room provenance), MIT or Apache-2.0, name (cargo search to verify availability — candidates from research: `agentkit-core` may collide; consider `agent-kernel`, `loop-rs`, `agentic-rs`, `rust-agent-kit`, `mcp-agent`).

## Clean-Room Notes for Implementation

When `/brainstorming` runs:
- The conversation must reference only the Research-Report and the cited public sources
- No reference to any private codebase, no comparisons "but downstream-product does X differently"
- Convergent design on the patterns listed in the Research-Report is fine and defensible — these are public-knowledge primitives
- The new repo's first commit should include `docs/RESEARCH.md` citing the four intermediate research files in this directory as the design provenance

## Files in This Research Package

| File | Words | Role |
|------|-------|------|
| `_pattern-literature.md` | 3,893 | Canonical pattern citations (ReAct, Reflexion, ReWOO, etc.) |
| `_commercial-landscape.md` | 5,690 | Non-Rust + commercial framework comparables |
| `_oss-landscape-rust.md` | 4,288 | Rust crate landscape (17 projects + graveyard) |
| `_user-workflow-pain.md` | 3,045 | Current pain points + workarounds + anti-finding |
| `Research-Report-rust-agent-runtime.md` | ~4,500 | Full synthesis report |
| `Research-Summary-rust-agent-runtime.md` | ~1,400 | This decision brief |
| **Total** | **~22,800** | |
