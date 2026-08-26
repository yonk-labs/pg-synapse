# pg-synapse-client

A minimal TypeScript client for [pg_synapse](../../README.md). No HTTP, no
separate service, no custom auth scheme: pg_synapse's agents are plain SQL
functions and tables, so this library is a thin, typed wrapper around a
handful of well-known statements, run over whatever Postgres client your app
already has.

## Why no built-in HTTP transport?

The pg_synapse extension only exists inside Postgres: `SELECT
synapse.execute(agent, input)` plus a few plain tables
(`synapse.executions`, `synapse.messages`, `synapse.agents`). Any app that
already talks to that database (`pg`, Prisma's raw client, `postgres`, a
pooled pgBouncer connection...) can call those directly. Wrapping that in a
bespoke HTTP service and a custom token would just be a second thing to run,
deploy, and secure, for no functional gain over the Postgres connection the
app already authenticates with.

If your Postgres is managed (RDS, Cloud SQL, etc.) and can't load the
extension at all, pg_synapse's separate sidecar process
(`crates/pg-synapse-sidecar`) exposes the same functionality over a REST
API instead. That's a different transport for a different constraint, not
what this package is for.

## Install

```bash
npm install pg-synapse-client pg
```

(`pg` here is just the example driver below. Any object with `query(text,
params) => Promise<{ rows }>` works, including a `pg.Pool` you already have
for the rest of your app, a `pg.Client`, or Prisma's `$queryRawUnsafe`
wrapped in the same shape.)

## Quickstart

```ts
import { Pool } from "pg";
import { PgSynapseClient } from "pg-synapse-client";

const pool = new Pool({ connectionString: process.env.DATABASE_URL });
const synapse = new PgSynapseClient(pool);
```

Auth is exactly whatever `DATABASE_URL`'s role can do in Postgres, nothing
pg_synapse-specific. Grant that role `EXECUTE` on the `synapse` schema's
functions and `SELECT` on its tables, same as any other schema permission.

## Example: on-demand check ("any past-due accounts?")

```ts
// A button in the app, or a route your own cron hits.
app.post("/admin/run-collections-check", async (req, res) => {
  const result = await synapse.execute(
    "collections_agent",
    "Review the open invoices and chase the ones that are overdue.",
  );
  res.json({ status: result.status, summary: result.output });
});
```

## Example: triage on new support tickets / emails

```ts
// Your own ingestion path already writes to support.tickets after pulling
// from the mailbox/helpdesk API; hand the new row to the triage agent.
async function onNewTicket(ticketId: number) {
  const executionId = await synapse.executeAsync(
    "triage_agent",
    `Triage support ticket #${ticketId}.`,
  );
  const run = await synapse.waitForCompletion(executionId, { timeoutMs: 30_000 });
  if (run.status !== "completed") {
    logger.warn(`triage run ${executionId} ended with status ${run.status}`);
  }
  return run;
}
```

## API

- `execute(agent, input)` -> the full outcome envelope (`SELECT synapse.execute($1, $2)`)
- `executeAsync(agent, input)` -> an `execution_id` to poll (`SELECT synapse.execute_async($1, $2)`)
- `status(executionId)` -> the current `synapse.executions` row, or `null`
- `waitForCompletion(executionId, { pollIntervalMs?, timeoutMs? })` -> polls `status()` until the run leaves `"queued"`
- `messages(executionId)` -> the full message trace, oldest first (`synapse.messages`)
- `recentExecutions({ agent?, limit? })` -> recent runs, optionally filtered to one agent
- `listAgents()` -> the agents registered in this database

Every method is one documented SQL statement against the `synapse` schema.
Nothing here is magic; read `src/index.ts`, it's about 150 lines.

## What this does NOT do

- **Define or register agents.** That's `synapse.agent_create(...)` (see
  the main README), an admin/ops action, deliberately not wrapped here so
  an app request path can't accidentally reconfigure its own agents.
- **Poll or schedule anything on its own.** "Check every hour" is your
  app's cron or queue, calling `execute()`. This library is unopinionated
  about how or when you trigger a run.
- **Talk to a managed Postgres that can't load the extension.** Use the
  sidecar (`crates/pg-synapse-sidecar`) for that.

## Develop

```bash
npm install
npm run build   # tsc -> dist/
npm test        # builds, then runs dist/**/*.test.js with node --test

# to also run the live check against a real database:
PG_SYNAPSE_TEST_DATABASE_URL="postgres://postgres:postgres@localhost:55432/synapse_demo" npm test
```
