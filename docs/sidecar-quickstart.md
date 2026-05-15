# Sidecar Quickstart

> Status: the sidecar binary (`pg-synapse-sidecar`) is planned for v0.1.x and
> is **not yet shipped**. The pgrx extension is the canonical install today;
> see [extension-quickstart.md](./extension-quickstart.md). This document
> describes the architecture and the decision criteria so you can plan ahead.

## The two hosts

pg_synapse splits cleanly into a host and a kernel. The kernel
(`pg-synapse-core`) does no I/O. A host supplies the tokio runtime, the
database connection, and the network surface, then drives the kernel's
`Runtime` facade. v0.1 ships one host (the pgrx extension); the sidecar is the
second.

```
+----------------------------------------------------------------+
|  SQL surface (the operator's view)                             |
|    synapse.execute(...) / .embed(...)                          |
|    synapse.agent_create / .secret_set / .llm_profile_set       |
+----------------------------------------------------------------+
                              |
              +---------------+---------------+
              |                               |
              v                               v
  +-------------------------+    +-----------------------------+
  |  pg-synapse-pgrx        |    |  pg-synapse-sidecar         |
  |  (in-process extension) |    |  (out-of-process binary)    |
  |  - One tokio runtime    |    |  - HTTP/JSON on /v1/...     |
  |    built in _PG_init    |    |  - libpq to Postgres        |
  |  - SPI for table reads  |    |  - install.sql wraps via    |
  |  - block_on for async   |    |    pg_net.http_post(...)    |
  +-----------+-------------+    +-------------+---------------+
              |                                |
              +---------------+----------------+
                              |
                              v
              +-----------------------------+
              |  pg-synapse-core (kernel)   |
              |  6 traits, 3 executors      |
              |  Runtime facade             |
              |  tower integration          |
              |  MCP client + mock LLM      |
              +-----------------------------+
```

## Which host to pick

Pick the **pgrx extension** when:

- You manage the Postgres server yourself and can install a `.so`.
- You want the lowest latency: the kernel runs in-process, SPI talks to the
  backend directly, no extra network hop.
- You want SQL tools (`sql_query`, `sql_exec`) to run under the caller's
  Postgres role with zero extra wiring.

Pick the **sidecar** when:

- You run on managed Postgres (RDS, Cloud SQL, Neon, Supabase) where you
  cannot install arbitrary extensions, but you *can* install `pg_net`.
- You want the agent runtime isolated from the database process (separate
  failure domain, separate resource limits, independent restarts).
- You want to scale the agent runtime independently of the database.

The trade is a network hop and slightly more operational surface (a process
to run and monitor) in exchange for working on locked-down managed Postgres.

## Sidecar install steps (coming in v0.1.x)

The sidecar will expose the v1 HTTP API documented in
[decisions.md](./decisions.md) D7: `POST /v1/execute`,
`POST /v1/execute_async`, `GET /v1/status/{id}`, `POST /v1/embed`, plus
`/v1/admin/*` endpoints gated by a shared-secret header. An
`sidecar-install.sql` template will create SQL wrapper functions in the
`synapse` schema that call out to the sidecar using `pg_net.http_post(...)`,
so the operator-facing SQL surface stays identical to the extension's. Startup
failures are logged to stderr in v0.1 (LISTEN/NOTIFY signalling is deferred to
v0.2 per D8).

Until the binary ships, use the pgrx extension.
