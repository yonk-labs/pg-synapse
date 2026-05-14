# Captured run: customer-support-triage

Captured live against vLLM at `http://192.168.1.193:8000/v1` running
`Intel/Qwen3-Coder-Next-int4-AutoRound`.

## Setup

```text
>>> Starting pgrx-managed Postgres 17 (if not running)
>>> Verifying connection: /home/yonk/.pgrx:28817
>>> Resetting database: pg_synapse_triage
>>> Installing extension: pg_synapse_pgrx
>>> Applying seed.sql
>>> Initial support.tickets:
 id |     subject      | category | priority | escalated
----+------------------+----------+----------+-----------
  1 | API is down      |          |          | f
  2 | Billing question |          |          | f
  3 | How do I export? |          |          | f
(3 rows)
```

## Three live `synapse.execute()` calls

```text
>>> Running triage workflow
>>>   endpoint: http://192.168.1.193:8000/v1
>>>   model:    Intel/Qwen3-Coder-Next-int4-AutoRound
```

Per-call envelope (pretty-printed; the raw column is one line of JSON):

```json
{
  "output": "Triaged ticket 1 as api/urgent with escalation due to enterprise tier.",
  "status": "completed",
  "cost_usd": null,
  "tokens_in": 2466,
  "tokens_out": 117,
  "tool_calls": [
    {"name": "sql_query",
     "args": {"query": "SELECT t.id, t.subject, t.body, c.email, c.tier FROM support.tickets t JOIN support.customers c ON c.id=t.customer_id WHERE t.id = 1"}},
    {"name": "sql_exec",
     "args": {"statement": "UPDATE support.tickets SET category='api', priority='urgent', escalated=true WHERE id=1"}}
  ],
  "duration_ms": 2144,
  "execution_id": "57f49660-9261-4c1f-9c1f-827c66234d9f"
}
```

```json
{
  "output": "Triaged ticket 2 as billing priority normal.",
  "status": "completed",
  "tokens_in": 2452,
  "tokens_out": 112,
  "tool_calls": [
    {"name": "sql_query",
     "args": {"query": "SELECT t.id, t.subject, t.body, c.email, c.tier FROM support.tickets t JOIN support.customers c ON c.id=t.customer_id WHERE t.id = 2"}},
    {"name": "sql_exec",
     "args": {"statement": "UPDATE support.tickets SET category='billing', priority='normal', escalated=false WHERE id=2"}}
  ],
  "duration_ms": 2061
}
```

```json
{
  "output": "Triage ticket 3: categorized as \"howto\", priority \"normal\", no escalation (free tier).",
  "status": "completed",
  "tokens_in": 2481,
  "tokens_out": 181,
  "tool_calls": [
    {"name": "sql_query",
     "args": {"query": "SELECT t.id, t.subject, t.body, c.email, c.tier FROM support.tickets t JOIN support.customers c ON c.id=t.customer_id WHERE t.id = 3"}},
    {"name": "sql_exec",
     "args": {"statement": "UPDATE support.tickets SET category='howto', priority='normal', escalated=false WHERE id=3"}}
  ],
  "duration_ms": 3028
}
```

## Final state

```text
>>> Final support.tickets:
 id |     subject      | category | priority | escalated
----+------------------+----------+----------+-----------
  1 | API is down      | api      | urgent   | t
  2 | Billing question | billing  | normal   | f
  3 | How do I export? | howto    | normal   | f
(3 rows)

>>> synapse.executions:
  agent_name  |  status   | tokens_in | tokens_out | duration_ms
--------------+-----------+-----------+------------+-------------
 triage_agent | completed |      2466 |        117 |        2144
 triage_agent | completed |      2452 |        112 |        2061
 triage_agent | completed |      2481 |        181 |        3028
(3 rows)


SUCCESS: all 3 tickets have category + priority assigned.
```

## Observations

- The model correctly escalated only the enterprise + urgent ticket (id 1).
- Token counts are stable around 2.5k input + 110 to 180 output per call.
  Most of the input is the system prompt plus the tool schema.
- Each call ran in 2.0 to 3.0 seconds end-to-end.
- The free-tier howto ticket got priority `normal` instead of `low` on this
  particular run. The model picks within the allowed set but the exact
  point inside the set is not deterministic, which is fine for triage.
