# Why not a Python script on a cron?

You can write one this afternoon, in a language you already know, with a
debugger that works. That is a real argument and this document does not
pretend otherwise.

It answers a narrower question: **what can pg_synapse do that a cron script
cannot be made to do**, whatever effort you spend on it. Three things qualify.
Everything else on the list is a convenience, and conveniences do not justify
a database extension.

Every claim below is verified. Where a claim has a caveat that would annoy you
to discover later, the caveat is here.

---

## 1. It can stop the write, not just react to it

A cron script runs after the transaction committed. By then the row is real,
other sessions can see it, and downstream triggers have fired. The script can
compensate: update a status column, emit a reversal, page someone. It cannot
prevent, because there is nothing left to prevent.

An agent attached in inline mode runs *inside* the writing transaction, sees
the uncommitted row, and can abort it:

```sql
SELECT synapse.attach_agent_trigger(
  'public.orders', 'order_screener', 'inline', 'INSERT', NULL, 'NEW::text');

INSERT INTO orders (note) VALUES ('...');
-- ERROR: synapse inline trigger rejected: <the agent's reason>
```

The `INSERT` never happened. Not compensated, not flagged: rolled back, by the
same mechanism that rolls back a constraint violation.

This is the one capability with no equivalent outside the database. Every
application-tier design, cron scripts included, is downstream of the commit.

**The caveats, which are large.**

Inline mode holds the writer's transaction open for the whole agent run, so
every session touching those rows waits behind it. That is bounded by
`pg_synapse.inline_timeout_ms`, default 2000, enforced rather than advised, and
a ceiling that only lowers an agent's own budget.

Two seconds is what a hot table can lend, not a prediction that your model will
answer in time. Six single-turn runs of a no-tool agent against a 36B model on
our reference endpoint took **1316, 1758, 1873, 2129, 4085 and 11162 ms**: the
median sits on the cap and the tail is well past it. A model that makes the
budget half the time has no business in a write path. Inline mode wants a small
fast model, and if you do not have one, use queue mode and accept that you are
back to reacting.

Network tools are refused inline outright. An outbound HTTP call inside a write
transaction holds locks for the duration of somebody else's outage.

And one that will bite you: **a rejected write takes the audit trail down with
it.** The agent ran, spent tokens, and made a decision, but the `RAISE` rolls
back the whole transaction including the `synapse.executions` row that recorded
it. Verified: zero rows after a rejection. Recording rejections needs a write
that escapes the transaction, which this does not yet have.

## 2. The agent's SQL is bounded by whoever invoked it

A cron script authenticates once, as itself. Its connection string names one
role, and every agent it runs has that role's privileges no matter who or what
the work is for. Multi-tenant isolation becomes something your Python enforces,
which means it holds until someone writes a query that forgets to.

`synapse.execute` is `SECURITY INVOKER`. The agent's SQL runs with the calling
role's privileges and Postgres enforces the boundary:

```
-- as synapse_user
SELECT synapse.tool_call('sql_query',
  '{"query":"SELECT count(*) FROM synapse.secrets","params":[]}');
-- {"error": "... permission denied for table secrets", "status": "errored"}

-- the identical call as a privileged role
-- [{"n": 2}]
```

The denial comes from the caller's rights, which is why the same statement
succeeds for someone entitled to it. Grants, row-level security and column
privileges you already wrote apply to the agent for free, and an agent talked
into an unwise query fails at the privilege check rather than at your review.

`synapse.executions.caller_role` records who actually asked, so the audit trail
answers "who ran this", not just "the service account ran this".

**The caveat.** This bounds the agent's *SQL*. A tool that reaches outside the
database is bounded by that tool's own controls, not by Postgres.

## 3. The work and its record are one transaction

A cron script does its work, then writes "I did the work" as a separate
statement. Those can disagree. The process dies between them and you have
untracked work; the write fails after the work succeeded and you retry
something already done.

An agent's run and its audit rows commit or roll back together, because they
are the same transaction. The `synapse.executions` row, every message and every
trace event land through one call, and a completed run's record is exactly as
durable as the work it describes.

**The caveat** is the same one as above, from the other side: for an inline
rejection this cuts against you, because rolling back the write rolls back the
record of why.

---

## What a cron script is genuinely better at

If none of the three above is load-bearing for your problem, stop reading and
write the script.

- **Any library you want.** An agent's tools are what the extension exposes.
  Your script imports whatever is on PyPI.
- **Debugging.** Breakpoints, a REPL, a stack trace in a language you know,
  versus reading `synapse.traces` and restarting Postgres to reload the
  extension.
- **Long work.** A script can run for an hour. An agent occupies a Postgres
  backend for its whole run, and inline mode gets two seconds.
- **Installation.** This is a C extension. Plenty of managed Postgres services
  will not let you install one at all, and that decision is not yours to make.
  A cron script runs anywhere.
- **Scaling out.** Run twenty workers on twenty machines. Agents run in
  database backends, competing with your query workload for the resource you
  are least able to add more of.
- **Blast radius.** A bad deploy of a script breaks the script. A bad extension
  can take down the database.

## The honest summary

Reach for pg_synapse when the agent's decision has to be **part of** a database
transaction rather than a consequence of one: gating a write, running under the
caller's own privileges, or keeping work and audit atomic. That is a real
category and nothing in the application tier can enter it.

For everything else, the cron script is the right answer, and this document
would rather you knew that now than after an install.
