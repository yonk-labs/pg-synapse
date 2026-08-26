import assert from "node:assert/strict";
import { test } from "node:test";
import { PgSynapseClient, PgSynapseError, type ExecutionRow, type PgQueryable } from "./index.js";

function fakeExecutionRow(overrides: Partial<ExecutionRow>): ExecutionRow {
  return {
    execution_id: "x",
    agent_name: "a",
    input: "i",
    output: null,
    status: "queued",
    tokens_in: 0,
    tokens_out: 0,
    cost_usd: null,
    duration_ms: null,
    caller_role: null,
    started_at: new Date(),
    finished_at: null,
    ...overrides,
  };
}

test("execute() parses a jsonb result whether the driver hands back an object or raw text", async () => {
  const envelope = {
    execution_id: "x",
    output: "ok",
    status: "completed",
    tokens_in: 1,
    tokens_out: 1,
    cost_usd: null,
    duration_ms: 1,
    tool_calls: [],
  };
  const fake: PgQueryable = {
    async query<T>(text: string, params?: unknown[]) {
      assert.match(text, /synapse\.execute\(\$1, \$2\)/);
      assert.deepEqual(params, ["agent", "input"]);
      // simulate a driver that returns jsonb as raw text instead of a parsed object
      return { rows: [{ result: JSON.stringify(envelope) }] as unknown as T[] };
    },
  };
  const result = await new PgSynapseClient(fake).execute("agent", "input");
  assert.deepEqual(result, envelope);
});

test("execute() throws PgSynapseError when the query returns no row", async () => {
  const fake: PgQueryable = {
    async query() {
      return { rows: [] };
    },
  };
  await assert.rejects(() => new PgSynapseClient(fake).execute("agent", "input"), PgSynapseError);
});

test("waitForCompletion() polls status() until it leaves 'queued'", async () => {
  let calls = 0;
  const fake: PgQueryable = {
    async query<T>() {
      calls++;
      return { rows: [fakeExecutionRow({ status: calls < 3 ? "queued" : "completed" })] as unknown as T[] };
    },
  };
  const row = await new PgSynapseClient(fake).waitForCompletion("x", { pollIntervalMs: 1 });
  assert.equal(row.status, "completed");
  assert.equal(calls, 3);
});

test("waitForCompletion() throws after timeoutMs if still queued", async () => {
  const fake: PgQueryable = {
    async query<T>() {
      return { rows: [fakeExecutionRow({ status: "queued" })] as unknown as T[] };
    },
  };
  await assert.rejects(
    () => new PgSynapseClient(fake).waitForCompletion("x", { pollIntervalMs: 1, timeoutMs: 5 }),
    PgSynapseError,
  );
});

// Live check against a real pg_synapse database. Skips cleanly when no
// connection string is provided, mirroring how the Rust workspace gates its
// own live-tests behind an env var (see CLAUDE.md).
const liveUrl = process.env.PG_SYNAPSE_TEST_DATABASE_URL;
test(
  "live: listAgents() reads real rows from a running pg_synapse database",
  { skip: liveUrl ? false : "PG_SYNAPSE_TEST_DATABASE_URL not set" },
  async () => {
    const { Pool } = await import("pg");
    const pool = new Pool({ connectionString: liveUrl });
    try {
      const agents = await new PgSynapseClient(pool).listAgents();
      assert.ok(Array.isArray(agents));
    } finally {
      await pool.end();
    }
  },
);
