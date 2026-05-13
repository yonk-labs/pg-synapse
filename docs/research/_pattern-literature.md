# Agent-Loop Pattern Literature (canonical public sources)

**Purpose.** This is a reference dossier of the publicly-published patterns that any credible agent runtime implements (or consciously chooses not to). Each entry has a citation, a plain-English description, the control flow, where the pattern shows up in public implementations, and known failure modes with critique sources. Use this as the provenance trail for the Rust agent-runtime crate's design decisions — every architectural choice can point at a paper or doc URL, not "I saw it in a private codebase."

Dates and arXiv IDs are quoted from primary sources. Direct abstract excerpts are in quotes; everything else is paraphrased.

---

## 1. ReAct (Reason + Act)

- **Citation:** Yao, Shunyu; Zhao, Jeffrey; Yu, Dian; Du, Nan; Shafran, Izhak; Narasimhan, Karthik; Cao, Yuan. *"ReAct: Synergizing Reasoning and Acting in Language Models."* arXiv:2210.03629, October 6 2022. ICLR 2023. https://arxiv.org/abs/2210.03629
- **Description.** The paper proposes interleaving reasoning traces with task-specific actions. From the abstract: *"reasoning traces help the model induce, track, and update action plans as well as handle exceptions, while actions allow it to interface with external sources, such as knowledge bases or environments, to gather additional information."* On HotpotQA and Fever the loop reduces hallucination relative to pure chain-of-thought; on ALFWorld and WebShop it beats imitation/RL baselines by 34% and 10% absolute success rate with only one or two in-context examples.
- **Control flow.**
  ```
  history := []
  for step in 1..max_iterations:
      thought    = llm(system_prompt, task, history)         # "Thought: ..."
      action     = parse_action(thought)                     # "Action: search[X]"
      if action.name == "Finish": return action.argument
      observation = tool_dispatch(action)                    # "Observation: ..."
      history.append((thought, action, observation))
  ```
- **Public implementations.**
  - LangChain `create_react_agent` / LangGraph prebuilt agent (`langchain-ai/react-agent` template; `langgraph.prebuilt.create_react_agent`). LangGraph models the loop as a state machine with `model` and `tools` nodes and conditional edges.
  - Google's Gemini API LangGraph example (`ai.google.dev/gemini-api/docs/langgraph-example`).
  - The original Princeton repo at `ysymyth/ReAct` (referenced by Yao et al).
  - Anthropic and OpenAI tool-use loops are ReAct in disguise — the assistant message acts as Thought, the tool_use block as Action, the tool_result as Observation.
- **Failure modes / critiques.**
  - **Infinite-loop and stuck cycles.** Agent repeats the same failing action when the observation channel returns no novel information. Mitigation: hard step cap + circuit breaker (Galileo, *"7 AI Agent Failure Modes"*; Towards Data Science, *"Your ReAct Agent Is Wasting 90% of Its Retries"*).
  - **Hallucinated tool names.** Letting the model emit free-form action strings instead of strict JSON-schema-validated tool calls. Modern function-calling APIs eliminate this class of bug at the protocol layer.
  - **Context blow-up.** History grows linearly with steps; long runs hit context limits. Mitigations: summarization nodes, compaction (see Reflexion / memory patterns below).
  - **Unfaithful Thoughts.** The Thought channel may rationalize rather than drive the Action — see CoT faithfulness critique under §6.

---

## 2. Reflexion

- **Citation:** Shinn, Noah; Cassano, Federico; Berman, Edward; Gopinath, Ashwin; Narasimhan, Karthik; Yao, Shunyu. *"Reflexion: Language Agents with Verbal Reinforcement Learning."* arXiv:2303.11366, March 20 2023. NeurIPS 2023. https://arxiv.org/abs/2303.11366 . Code: https://github.com/noahshinn/reflexion
- **Description.** From the abstract: *"We propose Reflexion, a novel framework to reinforce language agents not by updating weights, but instead through linguistic feedback... Reflexion agents verbally reflect on task feedback signals, then maintain their own reflective text in an episodic memory buffer to induce better decision-making in subsequent trials."* Three components: Actor (does the task), Evaluator (scores trajectory), Self-Reflection (turns score + trajectory into a textual lesson stored in episodic memory).
- **Control flow.**
  ```
  memory := []
  for trial in 1..max_trials:
      trajectory = actor.run(task, hints=memory)
      score      = evaluator.score(trajectory)
      if score >= success_threshold: return trajectory
      reflection = self_reflect(trajectory, score)
      memory.append(reflection)
  ```
- **Public implementations.** LangChain's "Reflexion" recipe; LangGraph reflection example; the original `noahshinn/reflexion` repo. Most production frameworks ship a "reflect" step optionally — it is not table-stakes.
- **Failure modes / critiques.**
  - Adds an extra full trajectory rerun per trial — token cost can be 2–5× a plain ReAct loop.
  - Quality of the Evaluator is the bottleneck; a noisy scorer produces useless reflections.
  - Episodic memory of stale tasks pollutes new tasks if not scoped.

---

## 3. ReWOO (Reasoning WithOut Observation)

- **Citation:** Xu, Binfeng; Peng, Zhiyuan; Lei, Bowen; Mukherjee, Subhabrata; Liu, Yinheng; Xu, Dongkuan. *"ReWOO: Decoupling Reasoning from Observations for Efficient Augmented Language Models."* arXiv:2305.18323, May 23 2023. https://arxiv.org/abs/2305.18323 . Code: https://github.com/billxbf/ReWOO
- **Description.** Three modules: **Planner** (writes a full plan up front, with placeholders for tool outputs), **Worker** (executes each tool call without calling the LLM in between), **Solver** (synthesizes final answer once all evidence is collected). The point: avoid the interleaved-prompt blow-up of ReAct, where every observation re-includes the entire prior trajectory. The paper reports *"5x token efficiency and 4% accuracy improvement on HotpotQA"* and demonstrates offloading reasoning from 175B GPT-3.5 into a 7B LLaMA.
- **Control flow.**
  ```
  plan       = planner.llm(task)               # ["E1=search[X]", "E2=lookup[#E1]", ...]
  evidence   = {}
  for step in plan:
      action      = resolve_placeholders(step, evidence)
      evidence[step.id] = tool_dispatch(action)   # NO llm call in this loop
  answer = solver.llm(task, plan, evidence)
  ```
- **Public implementations.** LangChain ReWOO recipe; LangGraph plan-execute-replan example. Anthropic's Programmatic Tool Calling and Code Execution with MCP push in the same direction (decouple reasoning from per-step observations to save tokens).
- **Failure modes / critiques.**
  - Plans written before any observation can't adapt — if step 1 fails or returns surprise data, the static plan is wrong.
  - Solved partially by combining with a Planner replan loop (Plan-and-Execute, §4).
  - Placeholder resolution is its own DSL — Worker has to safely substitute observations into later steps without prompt injection.

---

## 4. Plan-and-Solve / Plan-and-Execute

- **Citation (Plan-and-Solve).** Wang, Lei; Xu, Wanyu; Lan, Yihuai; Hu, Zhiqiang; Lan, Yunshi; Lee, Roy Ka-Wei; Lim, Ee-Peng. *"Plan-and-Solve Prompting: Improving Zero-Shot Chain-of-Thought Reasoning by Large Language Models."* arXiv:2305.04091, ACL 2023. https://arxiv.org/abs/2305.04091 . Code: https://github.com/AGI-Edgerunners/Plan-and-Solve-Prompting
- **Description.** From the paper: PS prompting *"consists of two components: first, devising a plan to divide the entire task into smaller subtasks, and then carrying out the subtasks according to the plan."* PS+ extends with more detailed instructions to address calculation, missing-step, and semantic-misunderstanding errors that plague zero-shot CoT.
- **Practitioner variant: Plan-and-Execute.** LangChain's "Plan-and-Execute Agents" recipe (derived from the BabyAGI / Plan-and-Solve lineage) is the production pattern: a Planner LLM emits a high-level plan, an Executor (often a ReAct sub-agent) executes each step, optionally with a Replanner after each step.
- **Control flow.**
  ```
  plan = planner.llm(task)
  results = []
  while plan not empty:
      step    = plan.pop(0)
      result  = executor.run(step)               # often a ReAct sub-agent
      results.append(result)
      plan    = replanner.llm(task, plan, results)  # may shorten/extend
  return synthesize(task, results)
  ```
- **Public implementations.** LangGraph's `plan-and-execute` template; CrewAI's hierarchical process; OpenAI Agents SDK handoffs; Anthropic's "orchestrator-worker" reference pattern.
- **Failure modes / critiques.**
  - Same brittleness as ReWOO if Replanner is omitted.
  - Replanner can thrash — repeatedly rewriting the same plan with cosmetic edits.
  - Planner needs strong task-decomposition; small models often produce shallow or redundant plans.

---

## 5. Toolformer

- **Citation:** Schick, Timo; Dwivedi-Yu, Jane; Dessì, Roberto; Raileanu, Roberta; Lomeli, Maria; Zettlemoyer, Luke; Cancedda, Nicola; Scialom, Thomas. *"Toolformer: Language Models Can Teach Themselves to Use Tools."* arXiv:2302.04761, February 9 2023. NeurIPS 2023. https://arxiv.org/abs/2302.04761
- **Description.** From the abstract: *"we show that LMs can teach themselves to use external tools via simple APIs... Toolformer, a model trained to decide which APIs to call, when to call them, what arguments to pass, and how to best incorporate the results into future token prediction. This is done in a self-supervised way, requiring nothing more than a handful of demonstrations for each API."* Tools covered include a calculator, Q&A system, two search engines, translation, and a calendar.
- **Relevance to a runtime.** Toolformer is a *training* method, not a runtime loop — but it's the canonical citation for the idea that "tool call generation" is a first-class capability of an LLM, which underwrites modern function-calling APIs.
- **Public implementations.** Not a runtime pattern directly. OpenAI function calling (GPT-3.5-turbo-0613, June 2023) and Anthropic tool_use are the productionized descendants of Toolformer-style training.
- **Failure modes / critiques.** The self-supervised filtering produces a small fraction of usable training examples (paper reports majority discarded). Modern function-calling models are fine-tuned via RLHF on multi-tool trajectories instead.

---

## 6. Chain-of-Thought (CoT)

- **Citation:** Wei, Jason; Wang, Xuezhi; Schuurmans, Dale; Bosma, Maarten; Ichter, Brian; Xia, Fei; Chi, Ed; Le, Quoc; Zhou, Denny. *"Chain-of-Thought Prompting Elicits Reasoning in Large Language Models."* arXiv:2201.11903, January 28 2022. NeurIPS 2022. https://arxiv.org/abs/2201.11903
- **Description.** From the abstract: *"generating a chain of thought — a series of intermediate reasoning steps — significantly improves the ability of large language models to perform complex reasoning."* The paper shows the capability *emerges* with scale (~62B+ parameters at the time) and that few-shot CoT exemplars unlock arithmetic, commonsense, and symbolic reasoning.
- **Why it matters for a runtime.** CoT is the substrate every reasoning pattern in this dossier builds on — ReAct's "Thought" channel is CoT; Tree of Thoughts and Plan-and-Solve are CoT generalizations. A runtime that exposes a reasoning-tokens stream needs to know the literature.
- **Failure modes / critiques.**
  - **Turpin et al, 2023.** *"Language Models Don't Always Say What They Think: Unfaithful Explanations in Chain-of-Thought Prompting."* arXiv:2305.04388. NeurIPS 2023. https://arxiv.org/abs/2305.04388 — Shows CoT explanations can systematically misrepresent the model's actual reasoning; biasing inputs (e.g., reordering MCQs so answer is always (A)) drops accuracy by up to 36% with the model never mentioning the bias.
  - **Lanham et al, 2023.** *"Measuring Faithfulness in Chain-of-Thought Reasoning."* arXiv:2307.13702. https://arxiv.org/abs/2307.13702 — Probes faithfulness directly via perturbation of CoT tokens.
  - **Implication for runtime design.** Treat reasoning tokens as suggestive, not authoritative — never make policy decisions purely on the Thought channel; ground in tool outputs.

---

## 7. Tree of Thoughts (ToT)

- **Citation:** Yao, Shunyu; Yu, Dian; Zhao, Jeffrey; Shafran, Izhak; Griffiths, Thomas L.; Cao, Yuan; Narasimhan, Karthik. *"Tree of Thoughts: Deliberate Problem Solving with Large Language Models."* arXiv:2305.10601, May 17 2023. NeurIPS 2023. https://arxiv.org/abs/2305.10601 . Code: https://github.com/princeton-nlp/tree-of-thought-llm
- **Description.** From the abstract: *"ToT allows LMs to perform deliberate decision making by considering multiple different reasoning paths and self-evaluating choices to decide the next course of action, as well as looking ahead or backtracking when necessary to make global choices."* Generalizes CoT from a linear chain to a tree, with BFS or DFS over thought nodes and an evaluator pruning branches.
- **Control flow.**
  ```
  frontier = [root_thought(task)]
  while frontier:
      candidates = expand(frontier)                # k children per node via llm
      scored     = evaluator.llm(candidates)
      frontier   = top_b(scored, beam_width)       # beam search / DFS / BFS
      if any(c.is_terminal_and_correct() for c in frontier): return c
  ```
- **Public implementations.** Princeton reference repo; LangGraph "ToT" recipe. Rare in production — most agents do not justify the 5–50× token cost over linear CoT.
- **Failure modes / critiques.**
  - Token cost scales with branching factor × depth.
  - Evaluator quality is the bottleneck (same as Reflexion).
  - Heavily task-specific search heuristics; not a universal-purpose pattern.

---

## 8. OpenAI Function Calling / Tool Use

- **Citation.** OpenAI Platform docs, *"Function calling"* and *"Using tools"* guides: https://developers.openai.com/api/docs/guides/function-calling and https://developers.openai.com/api/docs/guides/tools . Assistants tool-use lifecycle: https://platform.openai.com/docs/assistants/tools/function-calling . First shipped June 13 2023 with `gpt-3.5-turbo-0613`.
- **Description.** Tool calls are JSON-Schema-defined function specs sent in the request. The model emits a structured `tool_calls` array; the caller executes; the caller sends back `tool` role messages containing results. Five-step lifecycle: define tools → request with tools → model emits tool_calls → execute → send tool results → model replies. `strict: true` enforces JSON Schema conformance.
- **Lifecycle (Assistants API).**
  ```
  run.status: queued → in_progress → requires_action
                                         ↓
                           caller submits tool_outputs
                                         ↓
                       in_progress → completed | failed | cancelled
  ```
- **Public implementations.** Native in the OpenAI Python/Node SDKs; OpenAI Agents SDK uses it as the substrate; ports in nearly every framework (LangChain, LlamaIndex, Vercel AI SDK).
- **Failure modes / critiques.** Schema drift between model and caller; tool name collisions across parallel agents; cost of strict-mode JSON Schema validation on hot paths.

---

## 9. Anthropic Tool Use

- **Citation.** Claude API docs, *"Tool use with Claude — Overview"*: https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview . *"How tool use works"*: https://platform.claude.com/docs/en/agents-and-tools/tool-use/how-tool-use-works . *"Introducing advanced tool use on the Claude Developer Platform"*: https://www.anthropic.com/engineering/advanced-tool-use .
- **Description.** Tools are declared in the `tools` array on a Messages API request. The model emits content blocks of type `tool_use`. Caller sends a follow-up message with role `user` and a `tool_result` content block. Server-side tools (web_search, code_execution, web_fetch, tool_search) execute on Anthropic's infrastructure. Common-operations tools (`bash`, `text_editor`, `computer`, `memory`) have published schemas.
- **Lifecycle.** stop_reason transitions: `end_turn` (done), `tool_use` (caller must execute and continue), `max_tokens`, `stop_sequence`.
- **Public implementations.** Anthropic SDKs; Claude Code; the Claude Agent SDK reference loop. Recent additions: Tool Search Tool (lazy-load tool catalogs), Programmatic Tool Calling (let Claude write code that calls tools), Tool Use Examples.
- **Failure modes / critiques.** Same class as OpenAI — schema drift, tool-name collisions, prompt-injection via tool_result content.

---

## 10. Model Context Protocol (MCP)

- **Citation.** *"Specification — Model Context Protocol."* Current spec revision: https://modelcontextprotocol.io/specification/2025-11-25 . Announcement: *"Introducing the Model Context Protocol"*, Anthropic, November 2024, https://www.anthropic.com/news/model-context-protocol . Reference repo: https://github.com/modelcontextprotocol/modelcontextprotocol . Now a Linux Foundation project. Originally authored by David Soria Parra and Justin Spahr-Summers at Anthropic.
- **Description.** Open protocol for clients (host applications / agent runtimes) to discover and call **tools**, **resources**, and **prompts** exposed by independent **servers**. Transport: JSON-RPC 2.0 over stdio or HTTP/SSE. The point: decouple tool inventories from agent runtimes, so any MCP-aware host can use any MCP server.
- **Relevance to a Rust crate.** MCP is the table-stakes interop layer for 2025–2026. A new agent runtime that doesn't speak MCP can't be plugged into Claude Desktop, Cursor, Continue, Windsurf, the OpenAI Apps SDK, or the dozens of other MCP-aware hosts.
- **Public implementations.** `modelcontextprotocol/servers` reference servers; Rust SDK: `modelcontextprotocol/rust-sdk`; Python and TypeScript SDKs are first-tier.

---

## 11. Decorator Pattern in Agent Runtimes

- **Citation (classical).** Gamma, Erich; Helm, Richard; Johnson, Ralph; Vlissides, John. *Design Patterns: Elements of Reusable Object-Oriented Software.* Addison-Wesley, 1994. ISBN 0-201-63361-2. The "Gang of Four" book. Decorator is one of the seven structural patterns (with Adapter, Bridge, Composite, Facade, Flyweight, Proxy). Intent: *"attach additional responsibilities to an object dynamically. Decorators provide a flexible alternative to subclassing for extending functionality."*
- **Modern agent-runtime instances.**
  - **LangChain callbacks.** `BaseCallbackHandler` wraps an LLM/Chain/Tool call and observes lifecycle events (`on_llm_start`, `on_tool_end`, etc.) — pure decorator-over-interface.
  - **OpenAI Assistants run-step lifecycle.** Each Run advances through steps that can be intercepted by the caller (tool_outputs submission, status polling), behaving as a stack of decorators around the underlying message exchange.
  - **LangGraph node middleware.** Wraps node execution with retry, logging, rate-limit, and tracing layers.
  - **Vercel AI SDK `experimental_telemetry` and step middleware.** Decorator layer over `generateText` / `streamText`.
  - **OpenAI Agents SDK lifecycle hooks (`on_handoff`, `on_tool_start`, `on_tool_end`, guardrails).**
- **Why this matters.** A Rust agent runtime should expose tool calls and LLM calls as composable middleware (a stack of decorators), so concerns like retry, redaction, rate limiting, audit logging, prompt-injection guards, and tracing can be added without modifying the core loop.

---

## 12. Multi-Agent Patterns

- **Citation (canonical survey).** Guo, Taicheng et al. *"Large Language Model based Multi-Agents: A Survey of Progress and Challenges."* arXiv:2402.01680, IJCAI 2024. https://arxiv.org/abs/2402.01680 . Companion repo: https://github.com/taichengguo/LLM_MultiAgents_Survey_Papers .
- **Citation (architecture survey).** Masterman, Tyler; Besen, Sandi; Sawtell, Mason; Chao, Alex. *"The Landscape of Emerging AI Agent Architectures for Reasoning, Planning, and Tool Calling: A Survey."* arXiv:2404.11584. https://arxiv.org/abs/2404.11584 .
- **Citation (pattern catalogue).** Liu et al. *"Agent design pattern catalogue: A collection of architectural patterns for foundation model based agents."* Journal of Systems and Software, 2024. https://www.sciencedirect.com/science/article/pii/S0164121224003224 .
- **Single-agent vs crew — the practitioner debate.**
  - **Against multi-agent.** Cognition AI. *"Don't Build Multi-Agents."* 2025. https://cognition.ai/blog/dont-build-multi-agents . Core argument: running multiple agents in collaboration produces fragile systems because decision-making is dispersed and context is not shared thoroughly enough. *"Context engineering... is effectively the #1 job of engineers building AI agents."* Claude Code's design — main agent spawns subtask agents only for question-answering, never for parallel work — is cited as the right pattern.
  - **For multi-agent (carefully).** Anthropic's "How we built our multi-agent research system" (2025), CrewAI documentation, and Cognition's own follow-up *"Multi-Agents: What's Actually Working"* (https://cognition.ai/blog/multi-agents-working) all argue multi-agent works when sub-agents are *read-mostly* (research, retrieval, evaluation) and the orchestrator owns all writes.
  - **When the survey literature recommends multi-agent.** Tasks that decompose cleanly into role-based subtasks, where parallel exploration of independent branches gives speedup, and where a single agent's context would overflow. Avoid for high-frequency request/response systems (per-crew kickoff overhead is non-trivial) and for tasks that don't decompose by role.
- **Canonical pattern names from the catalogues.** Orchestrator-Worker, Evaluator-Optimizer, Hierarchical (CrewAI's `Process.hierarchical`), Debate / Multi-Agent Debate, Round-Robin / Group Chat (AutoGen), Handoff (OpenAI Agents SDK).
- **Failure modes.** Anthropic's published list — sub-agent context divergence, role confusion, message-passing prompt injection, runaway cost on debate patterns. Medium piece *"The Dark Psychology of Multi-Agent AI: 30 Failure Modes"* catalogues practitioner-reported failures.

---

## 13. Constitutional AI / RLAIF

- **Citation.** Bai, Yuntao; et al. (Anthropic). *"Constitutional AI: Harmlessness from AI Feedback."* arXiv:2212.08073, December 15 2022. https://arxiv.org/abs/2212.08073 . Author PDF mirror: https://www-cdn.anthropic.com/7512771452629584566b6303311496c262da1006/Anthropic_ConstitutionalAI_v2.pdf
- **Description.** From the abstract: *"we experiment with methods for training a harmless AI assistant through self-improvement, without any human labels identifying harmful outputs. The only human oversight is provided through a list of rules or principles, and we refer to the method as 'Constitutional AI'."* Two phases: (a) SL phase — sample from initial model, generate self-critiques against the constitution, generate revisions, fine-tune on revisions; (b) RL phase — sample pairs, score with a model-based judge against the constitution, train a preference model, run RL with that preference model as reward (RLAIF).
- **Relevance to a runtime as a guardrail pattern.** Constitutional AI is a *training* method, but the **critique-and-revise** sub-loop transfers cleanly to runtime as a guardrail decorator:
  ```
  response = agent.run(task)
  critique = critic.llm(response, constitution)
  if critique.violates: response = reviser.llm(response, critique)
  return response
  ```
  This is the basis of Anthropic's published prompt-injection-mitigation guidance and shows up in every modern safety/guardrail layer (LangChain `ConstitutionalChain`, NVIDIA NeMo Guardrails, Guardrails AI).
- **Failure modes / critiques.** The constitution document is itself an attack surface; ambiguous or contradictory principles produce ambiguous revisions. Llama-3 reproduction paper *"Constitution or Collapse? Exploring Constitutional AI with Llama 3-8B"* (arXiv:2504.04918) shows the method degrades sharply on smaller base models.

---

## Table-stakes vs differentiator

| Pattern | Table-stakes for a credible runtime? | Differentiator opportunity? |
|---|---|---|
| ReAct loop | **Yes** — every runtime ships this | No — ubiquitous |
| Provider tool-use (OpenAI + Anthropic schemas) | **Yes** | No — interop requirement |
| MCP client + server support | **Yes (2025+)** | No — interop requirement, but quality of impl matters |
| Decorator-style middleware (retry, audit, redact, trace) | **Yes** | Partially — quality and ergonomics differ |
| Plan-and-Execute / Plan-and-Solve | **Yes** for orchestration-class workloads | Quality of replanner is differentiating |
| Multi-agent (orchestrator-worker) | **Yes** as an option, not as a default | "When NOT to multi-agent" guidance is a differentiator |
| Reflexion (episodic verbal memory) | No — most frameworks omit | **Yes** — clean-room implementation with bounded memory cost |
| ReWOO (decoupled reasoning) | No | **Yes** — token efficiency on multi-step retrieval |
| Tree of Thoughts | No | **Marginal** — niche search-heavy tasks |
| Toolformer-style self-supervised tool training | No — not a runtime concern | No |
| Chain-of-Thought | **Yes** implicitly (via reasoning models) | No |
| Constitutional / critique-and-revise guardrail | Partially — most ship a basic version | **Yes** — composable, programmable constitution layer |
| Faithfulness probes / reasoning-token audit | No | **Yes** — observability differentiator |

---

## What a new Rust crate must support out of the box

Based on the literature:

1. **A ReAct-shape inner loop** — Thought / Action / Observation, with strict-schema tool calls. Max-iteration cap and per-tool circuit breakers are non-negotiable (Galileo, agentwiki.org failure-mode catalogues).
2. **Provider-native tool use for OpenAI and Anthropic** — JSON-Schema tool definitions, `tool_calls` / `tool_use` / `tool_result` lifecycle, `stop_reason` handling, parallel tool calls.
3. **MCP client + server traits** — stdio and HTTP/SSE transports, JSON-RPC 2.0. This is the interop floor for 2025+.
4. **Decorator/middleware stack** — composable layers for retry, redaction, rate limiting, audit logging, prompt-injection guards, OpenTelemetry tracing. Modelled on the GoF Decorator and validated by LangChain callbacks, OpenAI Agents SDK hooks, Vercel AI SDK middleware.
5. **A Plan-and-Execute orchestrator option** — Planner / Executor (ReAct sub-agent) / Replanner. Used when tasks decompose; the Replanner step is what separates this from naive ReWOO.
6. **Step-bounded execution with explicit history compaction** — long-running agents must summarize or evict, not just truncate.
7. **A critique-and-revise guardrail decorator** — runtime application of the Constitutional AI sub-loop, with a programmable constitution.

## What a new Rust crate could differentiate on

Based on what the literature *publishes* but most frameworks *don't ship cleanly*:

1. **Bounded Reflexion memory.** Episodic verbal-RL memory with explicit eviction policies (token budget, recency, task-scope). Most frameworks treat this as a recipe, not a primitive.
2. **Token-efficient ReWOO mode.** First-class plan-with-placeholders evaluator that doesn't re-prompt the LLM per tool call. Pairs naturally with MCP's batchable tool semantics.
3. **Faithfulness audit hooks.** Surface reasoning tokens as a separate observable stream and provide a "faithfulness probe" middleware (perturbation-style, per Lanham 2023) for high-stakes deployments.
4. **Multi-agent only where it earns its weight.** Default single-agent with subtask-handoff (Claude Code / Cognition pattern), opt-in orchestrator-worker. Ship explicit documentation citing Cognition's *"Don't Build Multi-Agents"* on when *not* to use the multi-agent primitives.
5. **Strict-by-default middleware ergonomics.** Tower-style `Service` + `Layer` for tools — Rust's tower ecosystem is the canonical decorator stack, and no agent runtime has executed on this cleanly.
6. **Provenance for every reasoning step.** Each Action carries the citation chain — Thought tokens, tool inputs, tool outputs — so the audit log is the agent's full causal history. Underwrites the "defensible public provenance" goal for the design itself.

---

## Data sources

Primary papers (arXiv):

- ReAct — https://arxiv.org/abs/2210.03629
- Reflexion — https://arxiv.org/abs/2303.11366
- ReWOO — https://arxiv.org/abs/2305.18323
- Plan-and-Solve — https://arxiv.org/abs/2305.04091
- Toolformer — https://arxiv.org/abs/2302.04761
- Chain-of-Thought — https://arxiv.org/abs/2201.11903
- CoT Faithfulness (Turpin) — https://arxiv.org/abs/2305.04388
- CoT Faithfulness (Lanham) — https://arxiv.org/abs/2307.13702
- Tree of Thoughts — https://arxiv.org/abs/2305.10601
- Constitutional AI — https://arxiv.org/abs/2212.08073
- LLM Multi-Agents Survey — https://arxiv.org/abs/2402.01680
- AI Agent Architectures Survey — https://arxiv.org/abs/2404.11584

Vendor docs:

- OpenAI Function Calling — https://developers.openai.com/api/docs/guides/function-calling
- OpenAI Using Tools — https://developers.openai.com/api/docs/guides/tools
- OpenAI Assistants tool lifecycle — https://platform.openai.com/docs/assistants/tools/function-calling
- Anthropic Tool Use Overview — https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview
- Anthropic How Tool Use Works — https://platform.claude.com/docs/en/agents-and-tools/tool-use/how-tool-use-works
- Anthropic Advanced Tool Use — https://www.anthropic.com/engineering/advanced-tool-use
- MCP Specification — https://modelcontextprotocol.io/specification/2025-11-25
- MCP Announcement — https://www.anthropic.com/news/model-context-protocol
- MCP Spec Repo — https://github.com/modelcontextprotocol/modelcontextprotocol

Practitioner critiques & pattern catalogues:

- Cognition AI, *"Don't Build Multi-Agents"* — https://cognition.ai/blog/dont-build-multi-agents
- Cognition AI, *"Multi-Agents: What's Actually Working"* — https://cognition.ai/blog/multi-agents-working
- Liu et al, *"Agent design pattern catalogue"* — https://www.sciencedirect.com/science/article/pii/S0164121224003224
- Galileo, *"7 AI Agent Failure Modes"* — https://galileo.ai/blog/agent-failure-modes-guide
- AgentWiki failure modes — https://agentwiki.org/common_agent_failure_modes
- Towards Data Science, *"Your ReAct Agent Is Wasting 90% of Its Retries"* — https://towardsdatascience.com/your-react-agent-is-wasting-90-of-its-retries-heres-how-to-stop-it/

Public implementations referenced:

- LangChain ReAct agent — https://github.com/langchain-ai/react-agent
- LangGraph `create_react_agent` — https://reference.langchain.com/python/langgraph.prebuilt/chat_agent_executor/create_react_agent
- Princeton Tree of Thoughts — https://github.com/princeton-nlp/tree-of-thought-llm
- Reflexion reference — https://github.com/noahshinn/reflexion
- ReWOO reference — https://github.com/billxbf/ReWOO

Foundational software-engineering reference:

- Gamma, Helm, Johnson, Vlissides. *Design Patterns: Elements of Reusable Object-Oriented Software.* Addison-Wesley, 1994. ISBN 0-201-63361-2.
