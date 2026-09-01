# Replicas and failover

What runs where. Closes O2, which noted that phase 2 rejected "batch only"
without ever saying which parts of the system can run on a standby.

Measured against a read-only server. A hot standby raises the same read-only
errors from the same code path, so the write/read split below is exact; items
marked *derived* are reasoned from the code rather than measured on a physical
standby, and are called out individually.

---

## Agents cannot run on a read replica

Not "run slower" or "run degraded". They fail:

```
SET default_transaction_read_only = on;
SELECT synapse.execute('some_agent', 'anything');
-- ERROR: cannot execute INSERT in a read-only transaction
```

Every run writes its audit row, so every run needs a writable server. There is
no trace level that turns this off; `synapse.executions` is written regardless
of what messages and events are persisted.

**It fails late, which is the part that costs money.** The audit write is the
last thing a run does, so the model was called, the tools ran, and the tokens
were spent before the failure. Measured at 660ms for a one-turn agent: long
enough to have done the work, and all of it discarded. Point a connection pool
at a standby by accident and you get a bill with nothing to show for it.

If you want the failure to be cheap, keep agent traffic on a connection that
cannot reach a standby, rather than relying on the error.

## What does work read-only

Measured on the same read-only server:

| Call | Read-only |
| --- | --- |
| `synapse.tool_call('sql_query', ...)` on a SELECT | works |
| `synapse.ensure_kernel()` | works |
| `synapse.agent_list()`, `synapse.tool_list()` | works |
| `synapse.version()` | works |
| `synapse.execute`, `synapse.execute_async` | `cannot execute INSERT` |
| `synapse.enqueue` | `cannot execute INSERT` |
| `synapse.drain_queue` | `cannot execute SELECT` |

So a standby is useful for reading what the system did and for introspection,
which is worth knowing when someone asks whether reporting can be moved off the
primary. It can. Running agents cannot.

`drain_queue` failing on a *SELECT* is not a typo. It claims jobs with
`SELECT ... FOR UPDATE SKIP LOCKED`, and a row lock is a write as far as a
read-only transaction is concerned. Worth knowing if you ever try to read the
queue from a standby: plain `SELECT`s against `synapse.agent_queue` are fine,
the claim is not.

`synapse.tool_call` on a writing statement fails the same way `execute` does,
for the same reason, at the statement rather than at the audit row.

## Failover

*Derived from the code, not measured on a promoted standby.*

Everything the kernel needs lives in ordinary tables in the `synapse` schema:
agents, both profile tables, secrets, tools, connections. They replicate like
any other table, so a promoted standby has the configuration already.

The kernel cache is a `OnceCell` in the backend process, so it is per
connection, not shared. Failover drops connections, and the backends that come
up afterwards build their cache from the promoted primary. There is no
cluster-wide cache to invalidate and no stale-config window to wait out. The
same property makes `synapse.rebuild_kernel()` necessary after editing config
tables by hand on a *running* server, which is the same mechanism seen from the
other side.

**In-flight runs are lost, and lost quietly.** A run interrupted by failover
never reaches its audit write, so it leaves no row: not a failed row, no row.
The same shape as the read-only case. Anything that must survive a failover
needs its own record written by the caller, not the audit trail.

## Scheduling

The schedule driver is a tokio task in the harness process, not in the
database. It is not `pg_cron` and does not survive the harness stopping.

Two consequences worth stating before a capacity conversation:

- **Scheduling stops when the harness stops.** There is no in-database timer to
  take over. A failover that leaves the harness pointed at a demoted server
  stops the schedule until it is repointed, and nothing raises an alarm about
  it.
- **pg_cron is the answer for a deployment you own**, and it needs
  `shared_preload_libraries` plus a restart, on the primary, arranged before
  you need it. A promoted standby that was never configured for pg_cron does
  not start running schedules because it got promoted.

## What this means for capacity planning

- Agent traffic is **primary-only** and holds a backend for the length of a
  model call. Budget connections against concurrent agent runs, not against
  request rate; a 30 second run occupies a backend for 30 seconds.
- Read replicas offload **reporting on agent activity**, not agent execution.
- Inline trigger agents additionally hold the writer's transaction open, which
  is a different and sharper cost. See `pg_synapse.inline_timeout_ms` and
  `docs/versus-a-cron-script.md`.
- There is no queue that survives a failover mid-run. `synapse.agent_queue`
  survives as a table, but a job already claimed and running does not.

## Not covered

Logical replication of the `synapse` schema, multi-primary, and connection
poolers that route by transaction rather than by session. Nothing here is known
to break under those; none of it has been tested.
