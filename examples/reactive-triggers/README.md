# Reactive Triggers Example

This example demonstrates the two reactive trigger modes added in T1:
**queue mode** (async, fire-and-forget) and **inline mode** (sync, transactional gate).

## What it builds

- `demo.tickets` table with a **queue-mode** trigger that runs `triage_agent`
  after each INSERT. The INSERT commits immediately; the agent enriches
  `category` and `priority` later when `synapse.drain_queue()` is called.

- `demo.orders` table with an **inline-mode** trigger that runs `policy_agent`
  synchronously. If the agent emits `{"decision":"reject"}`, the INSERT rolls
  back with the agent's reason. A valid order commits normally.

## Files

| File | Purpose |
|------|---------|
| `seed.sql` | Creates demo schema, LLM profile, and both agents |
| `queue_demo.sql` | Attaches queue trigger, INSERTs, shows drain flow |
| `inline_demo.sql` | Attaches inline trigger, shows reject + accept paths |
| `run.sh` | Repeatable end-to-end harness (pgrx-managed Postgres) |

## Quick start

```bash
# From the repo root
PG_SYNAPSE_LLM_BASE_URL=http://192.168.1.193:8000/v1 \
PG_SYNAPSE_LLM_MODEL=Intel/Qwen3-Coder-Next-int4-AutoRound \
  examples/reactive-triggers/run.sh
```

Or run the SQL files manually against an existing pg_synapse database:

```sql
CREATE EXTENSION pg_synapse_pgrx;
\i examples/reactive-triggers/seed.sql
\i examples/reactive-triggers/queue_demo.sql
\i examples/reactive-triggers/inline_demo.sql
```

## How the modes work

### Queue mode

```sql
SELECT synapse.attach_agent_trigger(
  'demo.tickets',
  'triage_agent',
  'queue',       -- enqueue, don't block the writer
  'INSERT'
);

INSERT INTO demo.tickets (subject, body) VALUES (...);
-- commits instantly, job_id is in synapse.agent_queue with status='queued'

SELECT synapse.drain_queue(10);
-- agent runs here, writes result back, status becomes 'done'
```

### Inline mode

```sql
SELECT synapse.attach_agent_trigger(
  'demo.orders',
  'policy_agent',
  'inline',      -- run synchronously, raise on reject
  'INSERT'
);

INSERT INTO demo.orders (customer, amount) VALUES ('bad_actor', -50.00);
-- ERROR: synapse inline trigger rejected: <reason from agent>
-- The INSERT is rolled back.

INSERT INTO demo.orders (customer, amount) VALUES ('alice', 150.00);
-- Commits normally.
```

## Recursion guard

Both modes include `IF pg_trigger_depth() > 1 THEN RETURN NEW; END IF;`.
If the agent's `sql_exec` tool writes back to the same table, the trigger
does not re-fire. A `when_sql` row guard (e.g. `NEW.status = 'new'`) is the
recommended additional guard for production use.

## Drain scheduling

`synapse.drain_queue` is operator-driven in v0.1. Schedule it with pg_cron:

```sql
SELECT cron.schedule('synapse-drain', '* * * * *', 'SELECT synapse.drain_queue(50)');
```

A native background worker drain (LISTEN/NOTIFY + bgworker) is the v0.2 upgrade.
