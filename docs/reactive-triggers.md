# Reactive triggers: queue vs inline agent invocation

A Postgres trigger can fire a pg_synapse agent on INSERT/UPDATE. The
operator chooses, per attached trigger, one of two modes. They have
opposite transactional semantics on purpose.

## The two modes

### Queue mode (async, default)

Trigger writes a job row and returns immediately. The triggering
INSERT/UPDATE commits without waiting for the LLM. A drainer runs the
agent later, out of band.

- Fast writes; LLM latency never blocks the writer.
- The agent runs in its own transaction later. It **cannot** roll back
  the row that triggered it (that row already committed).
- Failure isolation: a failing/slow LLM never aborts the original write.
- Use for: enrichment, classification, notification, anything where the
  write is authoritative and the agent reacts after the fact.

### Inline mode (sync, opt-in)

Trigger calls `synapse.execute` synchronously, inside the writing
transaction. If the agent errors or returns a reject decision, the
trigger `RAISE`s, so the original INSERT/UPDATE **rolls back**.

- The agent is a transactional gate: it can veto the write.
- Slow LLM blocks the writer and holds locks for the duration.
- Use for: validation/guarding ("an agent must approve this row"),
  policy enforcement, anything where the write should not stand if the
  agent rejects it. This is the "agent wants to rollback" case.

The reject signal (inline mode): the trigger RAISEs when either
(a) the execution status is not `completed` (error / max_iterations /
cost_cap / timeout), or (b) the agent's final output JSON contains
`{"decision":"reject", ...}`. The agent's system prompt opts into the
convention by being told to emit that object to veto. The reason string
is surfaced in the RAISE message so the rejecting agent explains itself.

## SQL surface (new in the pgrx host)

```
synapse.agent_queue(
  job_id      uuid primary key default gen_random_uuid(),
  agent       text not null,
  input       text not null,
  status      text not null default 'queued',   -- queued|running|done|error
  result      jsonb,
  error       text,
  source      text,                              -- "trigger:<schema>.<table>"
  enqueued_at timestamptz not null default now(),
  started_at  timestamptz,
  finished_at timestamptz
)

synapse.enqueue(agent text, input text, source text default null) -> uuid
  -- inserts a 'queued' row, returns job_id. Fire-and-forget.

synapse.drain_queue(max_jobs int default 10) -> int
  -- claims up to max_jobs 'queued' rows (FOR UPDATE SKIP LOCKED),
  -- runs synapse.execute for each, writes result/status, returns the
  -- number processed. Idempotent and concurrency-safe. v0.1 drain path:
  -- the operator calls this on a schedule (pg_cron, or a sidecar
  -- poller, or `psql` in a loop). A true background worker
  -- (LISTEN/NOTIFY + bgworker) is the v0.2 upgrade (design spec D8).

synapse.attach_agent_trigger(
  target_table text,        -- e.g. 'support.tickets'
  agent        text,
  mode         text,        -- 'queue' (default) | 'inline'
  events       text default 'INSERT',   -- 'INSERT' | 'UPDATE' | 'INSERT OR UPDATE'
  when_sql     text default null,        -- optional row guard, e.g. "NEW.status = 'new'"
  input_expr   text default 'NEW::text' -- expression passed as the agent input
) -> void
  -- generates a row-level AFTER trigger + trigger function on
  -- target_table. queue mode: the function calls synapse.enqueue.
  -- inline mode: it calls synapse.execute and RAISEs on reject/error.

synapse.detach_agent_trigger(target_table text) -> void
```

## Recursion / safety (both modes)

- The generated trigger function guards with `pg_trigger_depth() > 1`
  (skip if we are already inside a pg_synapse-induced write) so an
  agent's `sql_exec` writing back to the same table does not re-fire
  endlessly. A `when_sql` row guard (e.g. only fire when
  `NEW.status = 'new'`) is the recommended additional guard.
- Inline mode wraps the `synapse.execute` call so a tool failure rolls
  back to a savepoint (N1.2 SAVEPOINT-per-tool already gives per-tool
  isolation); a hard reject RAISEs past the savepoint to abort the
  whole triggering statement on purpose.
- Queue mode never recurses dangerously: enqueue is a single cheap
  INSERT into `synapse.agent_queue`; the drainer runs outside the
  triggering transaction.

## Example: support ticket triage, both ways

Queue (enrich after the fact, write always succeeds):

```sql
SELECT synapse.attach_agent_trigger(
  'support.tickets', 'triage_agent',
  mode => 'queue', events => 'INSERT',
  when_sql => "NEW.category IS NULL");
-- drained by:  SELECT synapse.drain_queue(50);   (pg_cron every minute)
```

Inline (a guard agent can reject a non-compliant ticket):

```sql
SELECT synapse.attach_agent_trigger(
  'support.tickets', 'policy_gate_agent',
  mode => 'inline', events => 'INSERT OR UPDATE',
  when_sql => "NEW.status = 'submitted'");
-- if policy_gate_agent emits {"decision":"reject","reason":"PII in body"}
-- the INSERT/UPDATE rolls back with that reason.
```

## v0.1 vs v0.2 boundary (honest)

- v0.1 (build now): the table, `enqueue`, `drain_queue` (operator-driven
  drain via pg_cron/poller), `attach_agent_trigger`/`detach`, both
  modes, recursion guards, a `reactive-triggers` example exercising
  queue and inline against the live LLM.
- v0.2: a real background worker (Postgres bgworker + `LISTEN/NOTIFY`)
  so the queue drains itself with no external scheduler; partial-result
  streaming. Tracked in `BACKLOG.md`.
