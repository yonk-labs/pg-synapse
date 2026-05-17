# Framework Parity: Google ADK (Python) vs pg_synapse

**Sources:**
- Quickstart pattern: https://google.github.io/adk-docs/get-started/ (redirects to adk.dev; the get-started page was redirect-blocked during authoring, so the `adk create` / `root_agent` shapes below are from stable public knowledge of ADK and flagged as such).
- A2A protocol: https://adk.dev/a2a/intro/ (fetched and verified by the controller; the A2A section below is grounded in that page, not training data).

---

## 1. The canonical ADK get-started example

ADK scaffolds a project with a CLI and centers on a `root_agent`:

```bash
adk create my_agent        # scaffolds my_agent/agent.py + __init__.py + .env
```

```python
# my_agent/agent.py
from google.adk.agents import Agent

def get_current_time(city: str) -> dict:
    """Return the current time for a city."""
    # plain Python function; ADK derives the tool schema from the
    # signature + docstring, like our #[derive(Tool)] macro.
    return {"status": "ok", "time": "..."}

root_agent = Agent(
    model="gemini-2.0-flash",
    name="time_agent",
    description="Tells the current time.",
    instruction="Answer time questions using the get_current_time tool.",
    tools=[get_current_time],
)
```

Run it with `adk run my_agent` (terminal), `adk web` (dev UI), or a
`Runner` in code. The multi-agent walkthrough (orchestrator + child
agents, e.g. the community sokart/adk-walkthrough) composes a parent
`Agent` whose `sub_agents=[...]` are themselves `Agent`s; the parent
delegates by transferring control to a child.

## 2. A2A protocol (verified, factual)

Per https://adk.dev/a2a/intro/ : A2A (Agent-to-Agent) lets independent
agents running as separate network services collaborate. It targets the
case where an agent is "maintained by a different team or organization,"
written in "different programming languages or agent frameworks," and
needs "a strong, formal contract." ADK exposes an agent over the network
with an `A2AServer` component, and consumes a remote one with
`RemoteA2aAgent`, "a client-side proxy" so "interacting with the remote
agent feels just like interacting with a local tool." The wire spec lives
at a2a-protocol.org (the intro page does not restate transport details).

The key distinction for pg_synapse: A2A is agent-to-agent (one agent
calls another agent as a peer service), whereas MCP (which pg_synapse
already ships a client for) is agent-to-tool (an agent calls a tool
server). They are complementary, not the same.

## 3. Mapping to pg_synapse

| ADK primitive | pg_synapse | Verdict |
|---|---|---|
| `root_agent = Agent(model, name, instruction, tools)` | `synapse.agent_create(name, system_prompt, executor, llm_profile, tools, ...)` row + a Tool plugin | REPLICABLE |
| plain-Python-function tool (`get_current_time`) | `#[derive(Tool)]` plugin (e.g. a `clock`/`get_current_time` tool) | REPLICABLE |
| `adk create` CLI scaffold | SQL surface (`synapse.agent_create`, `synapse.llm_profile_set`), no CLI scaffold | DIFFERENT SURFACE, same outcome |
| orchestrator + `sub_agents=[...]` delegation | the agent-as-tool `call_agent` delegation tool (re-enters `Runtime::execute` on a named agent; see openai-agents-sdk.md for the full spec + depth-guard) | REPLICABLE via one new tool |
| `adk run` / `adk web` dev UX | `SELECT synapse.execute(agent, input)` from psql; no dev web UI | DIFFERENT SURFACE |
| A2A (`A2AServer`, `RemoteA2aAgent`, cross-service agents) | NOT IMPLEMENTED today | HONEST GAP, future plugin |

## 4. Runnable plan (specified, not built)

- `adk_root`: a `get_current_time` Tool plugin + an agent row + a
  `bench/scenarios/adk_root/` scenario asserting the agent calls the
  tool and returns a time. Direct, replicable now.
- `adk_orchestrator`: 3 agent rows (orchestrator + 2 children) plus the
  `call_agent` delegation tool; entry `synapse.execute('orchestrator',
  ...)`. Same delegation primitive that covers OpenAI SDK handoffs.

## 5. Honest why-not

Two ADK things pg_synapse does not match:

1. **`adk create` developer experience.** Deliberate design difference,
   not a gap. pg_synapse's surface is SQL: an operator runs
   `synapse.agent_create(...)`, not a project scaffold. Same outcome
   (a registered agent), different ergonomics. A CLI could be a thin
   future convenience wrapper; it is not core.

2. **A2A protocol.** A genuine unbuilt capability, stated plainly. ADK
   has first-class cross-service agent collaboration; pg_synapse has an
   MCP client (agent-to-tool) but no agent-to-agent wire protocol. A
   future `pg-synapse-a2a` plugin (client + server, exactly the way the
   MCP client is a plugin) would close this and maps cleanly onto a
   gateway-level intercept model: an A2A server in front of
   `synapse.execute` would let external agents call a pg_synapse agent
   as a governed peer, and a `RemoteA2aAgent`-style tool would let a
   pg_synapse agent call out. This is a v0.2+ design direction grounded
   in a real protocol, not a quickstart-sized example, and is recorded
   here so it is not mistaken for a shipped feature.
