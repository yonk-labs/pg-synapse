# EXAMPLE_OUTPUT: sql-agent-readwrite

Captured 2026-05-15 against vLLM serving `Intel/Qwen3-Coder-Next-int4-AutoRound`
at `http://192.168.1.193:8000/v1`, on the v0.1.1 security-hardened pgrx host
(SECURITY DEFINER functions, SAVEPOINT-per-tool-call, typed positional bind
params).

## Setup

```
CREATE EXTENSION
INSERT 0 2          -- seed: demo.notes has "Buy milk", "Call mom"
```

## Agent run (parameterized tool calls)

The agent was asked to add a note, then to list the table. It used the
parameterized form (`$1, $2` placeholders plus a `params` array), which is
the supported, injection-safe path as of v0.1.1.

Add-note execution issued:

```json
{"name": "sql_exec",
 "args": {"query": "INSERT INTO demo.notes (body, added_by) VALUES ($1, $2)",
          "params": ["Hello from pg_synapse!", "agent"]}}
```

List execution result envelope:

```json
{"output": "The table currently contains the following notes:\n\n1. Buy milk, added by \"seed\"\n2. Call mom, added by \"seed\"\n3. Hello from pg_synapse!, added by \"agent\"",
 "status": "completed",
 "cost_usd": null,
 "tokens_in": 1476,
 "tokens_out": 91,
 "tool_calls": [{"name": "sql_query",
   "args": {"query": "SELECT id, body, added_by FROM demo.notes ORDER BY id", "params": []}}],
 "duration_ms": 1551,
 "execution_id": "fc9d7435-77dc-4fc4-851f-d66350a84747"}
```

## Final table state

```
 id |          body          | added_by
----+------------------------+----------
  1 | Buy milk               | seed
  2 | Call mom               | seed
  3 | Hello from pg_synapse! | agent
(3 rows)
```

## Result

The agent inserted row 3 through the full v0.1.1 path: a `SECURITY DEFINER`
SQL function, a per-tool-call SAVEPOINT (Postgres internal subtransaction),
and typed positional parameter binding (no string interpolation). It then
read all three rows back. The security hardening is transparent to the
workflow: behavior is identical to v0.1.0, with the injection footgun closed.
