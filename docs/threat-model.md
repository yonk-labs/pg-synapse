# Threat model

What pg_synapse defends against, what it does not, and which of those is a
decision rather than an oversight. Closes O8, which asked for this to be stated
rather than implied.

Scope: the pgrx extension, with a section on the sidecar at the end. The two
have genuinely different postures and the difference is the point.

---

## The shape of the problem

An agent is a language model given tools and pointed at your database. Two
things follow that ordinary application security does not have to think about.

**The agent's instructions come from data.** A row it reads, a web page it
fetches, a filename a user chose. Anything that reaches the model can try to
redirect it, and no amount of prompt engineering makes that untrue. So the
useful question is never "will the agent behave" but "what can it reach when it
does not".

**The agent runs inside a Postgres backend.** Not beside the database, in it.
That is the point of the project and it is also the source of every entry
below: the process running the agent is the process holding the database's
memory.

---

## What is defended, and how

Each of these is enforced by Postgres, not by the agent choosing correctly.

**The agent's SQL is bounded by the invoking role.** `synapse.execute`,
`synapse.execute_async` and `synapse.tool_call` are `SECURITY INVOKER`, so an
agent reaches exactly what its caller may reach. Grants, row-level security and
column privileges apply unchanged. An agent talked into reading a table its
caller lacks gets `permission denied`, not the table.

**Agent SQL does not run as a superuser.** Every function is owned by
`synapse_owner`, a plain role. This is what stops `COPY t FROM PROGRAM 'sh'`,
which was verified reachable before the ownership change and returns
`permission denied to COPY to or from an external program` after it. A role,
not a check we have to remember to write.

**Secrets are not readable through the SQL surface.** `synapse.secrets` is
granted to no user role, and the function that returns secret values
(`synapse.config_secrets`) is granted to nobody. It is called only from inside
`synapse.ensure_kernel`, whose signature takes no argument and returns no
value, so a caller can cause the read but cannot steer it or see it.

**The audit trail cannot be forged.** The audit writers are granted to caller
roles, because an entry point running as its caller has to reach them, but the
grant is not the authorisation: they refuse any call without a capability token
that an entry point minted for a run in progress and retires when it ends.

**Inline trigger mode cannot reach the network.** An outbound call inside a
write transaction holds locks for the length of somebody else's outage, so
agents holding network tools are refused inline before any lock is taken.

**Egress goes through an allowlist.** Outbound HTTP is checked against the
configured allowlist rather than left open.

---

## What is not defended

### A superuser caller gets a superuser's agent

Everything above is enforced by the caller's role. Invoke an agent as a
superuser and the agent has a superuser's reach, because that is what
`SECURITY INVOKER` means. This is not a bug to fix; it is the same rule as
every other statement that role runs.

**Run agents as a restricted role.** If a database is reachable only as
`postgres`, none of the isolation above is doing anything for you.

### The encryption key lives in the backend the agent runs in

`pg_synapse.master_key` holds the pgcrypto key for `synapse.secrets`. The agent
executes inside the process that holds it. That is O8, and it is structural: no
arrangement of grants moves a GUC out of the memory of the process reading it.

What is fixed: the GUC is registered `SUPERUSER_ONLY`, so a restricted caller
asking for it by name is refused.

```
-- as synapse_user, through an ordinary tool call
SELECT current_setting('pg_synapse.master_key')
-- ERROR: permission denied to examine "pg_synapse.master_key"
```

Before that flag this returned the key, in one line, to any caller who could
run an agent. Worth being precise about why per-caller isolation did not help:
a GUC read is not a table read, and no grant governs it, so F2 was irrelevant
here. A privilege boundary only covers the objects the privilege system knows
about.

### That flag only holds if the library is preloaded

**`shared_preload_libraries = 'pg_synapse_pgrx'` is a security setting here,
not a performance one.** Set it.

A custom GUC belonging to a library that has not loaded yet is not protected,
it is a **placeholder**: Postgres accepts `pg_synapse.master_key` from the
configuration, attaches no flags to it, and hands the value to anyone who asks.
The `SUPERUSER_ONLY` flag comes into existence only when this library loads,
which by default is the first time something in a session touches `synapse.*`.

Measured on a correctly installed database with the key set the way an operator
would set it:

```
ALTER SYSTEM SET pg_synapse.master_key = 'REAL-DEPLOYED-KEY';
SELECT pg_reload_conf();

-- a fresh session, as synapse_user, that never touches synapse.*
SET ROLE synapse_user;
SELECT current_setting('pg_synapse.master_key');
--  REAL-DEPLOYED-KEY
```

With the library preloaded the same session gets `permission denied to
examine`, because the GUC is defined with its flag at postmaster start, before
any session exists.

The demo stack sets this. If you install the extension yourself and configure a
master key, you must set it too: the extension emits a warning when it notices
the combination, but by then a session that never called an agent has already
been able to read the key.

The general shape is worth remembering beyond this setting: **a flag on a GUC
is only as real as the library that declares it**, so any protection expressed
that way has a window before load unless the library is preloaded.

What remains: an agent whose **caller is a superuser** still reads it, by the
rule in the previous section.

**So state plainly what encryption at rest buys.** It protects the secret
values against someone who obtains the table contents without the running
server: a stolen backup, a copied data directory, a filesystem read. It does
not protect them against anyone who can run an agent as a superuser. If that
distinction does not match your requirement, the key does not belong in a GUC
in this process, and the answer is an external secret store the extension calls
out to rather than a better flag.

### Prompt injection is not solved, it is contained

Nothing here stops a model being talked into trying something. The defence is
entirely that the attempt fails at a privilege check. That is a deliberate
posture: it is testable, and it does not degrade as prompts get cleverer. But
an agent granted wide privileges and pointed at untrusted text is dangerous no
matter what the prompt says, and the containment is only as narrow as the role
you invoke it with.

### A tool that leaves the database leaves the model

Postgres bounds the agent's SQL. It does not bound what a tool does once it is
outside: `read_file` is bounded by its sandbox, HTTP by the egress allowlist,
`remote_query` by the credentials in that connection row. Each is its own
control, and adding a tool adds a surface that the database's privilege system
does not see.

### Resource exhaustion is bounded, not prevented

An agent holds a backend for its run. Iteration caps, cost caps and wall-clock
budgets bound one run; nothing bounds how many callers start one at once except
`max_connections`. Inline mode is the sharp case and has its own ceiling
(`pg_synapse.inline_timeout_ms`, default 2000).

### A rejected inline write leaves no record

An inline agent that aborts a write takes its own audit row down with the
rollback. The refusal happened and nothing records that it did. This matters
for a security review specifically because the audit trail is silent about
exactly the events an auditor would most want: the ones where the agent said
no.

---

## The sidecar is a different posture, and a weaker one

`pg-synapse-sidecar` runs the same kernel outside Postgres, over HTTP. None of
the guarantees above transfer, because all of them are expressed in Postgres
privileges and the sidecar has exactly one database connection with one role.

**There is no caller identity at all.** The pgrx host's central property, that
an agent reaches what its caller may reach, has no analogue here: every request
runs as whatever role the connection string names.

**And the run surface is unauthenticated.** The admin token gates the five
`/v1/admin/*` routes and nothing else. These seven are open:

```
POST /v1/execute        POST /v1/execute_async   GET /v1/status/{id}
POST /v1/embed          POST /v1/tool_call       GET /v1/health
GET  /v1/version
```

`POST /v1/tool_call` reaches `sql_query` and `sql_exec`. So anyone who can
reach the port can read and write the database with the sidecar's full
privileges, without a token.

The server binds `0.0.0.0`, not loopback, so "who can reach the port" is
everyone on the network unless something outside this process is stopping
them.

**If you run the sidecar, put it behind something.** A reverse proxy that
authenticates, a loopback bind plus an SSH tunnel, or a network policy. Do not
put it on a network you do not control and do not treat the admin token as
protecting anything but the five admin routes.

Verified by reading `crates/pg-synapse-sidecar/src/api.rs` and confirmed by the
process's own startup line (`pg-synapse-sidecar listening on 0.0.0.0:8088`).
Not confirmed by an end to end unauthenticated call: the sidecar in this
environment cannot resolve its database, so it never came up. The route table
is unambiguous, but that distinction is recorded rather than glossed.

## The short version for a security review

| | |
| --- | --- |
| Agent SQL privileges | The invoking role's, enforced by Postgres |
| Superuser callers | Get superuser agents. Do not invoke as one |
| Encryption key | Superuser-readable in-process. Protects backups, not runtime. **Requires `shared_preload_libraries`, or any role can read it** |
| Audit trail | Unforgeable, and silent on inline rejections |
| Prompt injection | Contained by privileges, not prevented |
| Tools leaving the database | Each carries its own control; Postgres does not see them |
| The sidecar | No caller identity, run surface unauthenticated, binds 0.0.0.0. Put it behind something |

The single most load-bearing operational decision is the role agents are
invoked as. Almost every guarantee above is stated relative to it.
