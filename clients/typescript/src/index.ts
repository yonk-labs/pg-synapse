/**
 * pg-synapse-client: a minimal TypeScript client for pg_synapse.
 *
 * pg_synapse exposes agents as plain SQL: a function, SELECT synapse.execute(agent, input),
 * and a handful of plain tables (synapse.executions, synapse.messages, synapse.agents). This
 * library is a thin, typed wrapper around those well-known statements. It has zero runtime
 * dependencies: pass in whatever Postgres client your app already uses (pg.Pool, pg.Client, a
 * pgBouncer-backed pool, Prisma's raw query client, etc.) as long as it exposes
 * query(text, params) => Promise<{ rows }>. Auth is whatever that connection already
 * authenticates as, a normal Postgres role and password, or however pg_hba.conf is configured.
 * There is no separate token or service to run.
 */

export interface PgQueryable {
  query<T = unknown>(text: string, params?: unknown[]): Promise<{ rows: T[] }>;
}

/**
 * pg_synapse's known terminal statuses, plus "queued" for a run still in
 * flight. The kernel can grow new statuses over time; treat any value other
 * than "queued" as terminal rather than enumerating an exhaustive union.
 */
export type ExecutionStatus =
  | "queued"
  | "completed"
  | "max_iterations"
  | "timed_out"
  | "cost_cap_exceeded"
  | "errored"
  | (string & {});

export interface ToolCallSummary {
  name: string;
  args: unknown;
}

/** The envelope returned by SELECT synapse.execute($1, $2). */
export interface ExecuteResult {
  execution_id: string;
  output: string | null;
  status: ExecutionStatus;
  tokens_in: number;
  tokens_out: number;
  cost_usd: number | null;
  duration_ms: number | null;
  tool_calls: ToolCallSummary[];
}

/** A row from synapse.executions. */
export interface ExecutionRow {
  execution_id: string;
  agent_name: string;
  input: string;
  output: string | null;
  status: ExecutionStatus;
  tokens_in: number;
  tokens_out: number;
  cost_usd: number | null;
  duration_ms: number | null;
  caller_role: string | null;
  started_at: Date;
  finished_at: Date | null;
}

/** A row from synapse.messages: one turn of an agent's message trace. */
export interface MessageRow {
  execution_id: string;
  seq: number;
  role: "system" | "user" | "assistant" | "tool";
  content: string | null;
  tool_call_id: string | null;
  tool_name: string | null;
  tool_input: unknown;
  tool_output: unknown;
  ts: Date;
}

/** A row from synapse.agents, for a picker or admin view. */
export interface AgentRow {
  name: string;
  executor_name: string;
  llm_profile_main: string | null;
  tools: string[];
  max_iterations: number;
  timeout_ms: number;
  cost_cap_usd: number | null;
  trace_level: string | null;
}

export class PgSynapseError extends Error {
  constructor(
    message: string,
    public readonly cause?: unknown,
  ) {
    super(message);
    this.name = "PgSynapseError";
  }
}

// Most Postgres drivers (node-postgres included) parse a jsonb column into a
// JS value already; a few return it as raw text. Handle both without forcing
// callers to care which driver they picked.
function parseJsonb<T>(value: T | string): T {
  return typeof value === "string" ? (JSON.parse(value) as T) : value;
}

export class PgSynapseClient {
  constructor(private readonly db: PgQueryable) {}

  /**
   * Run an agent and wait for it to finish.
   * SELECT synapse.execute($1, $2)
   */
  async execute(agent: string, input: string): Promise<ExecuteResult> {
    const { rows } = await this.db.query<{ result: ExecuteResult | string }>(
      "SELECT synapse.execute($1, $2) AS result",
      [agent, input],
    );
    if (!rows[0]) {
      throw new PgSynapseError(`synapse.execute returned no row for agent "${agent}"`);
    }
    return parseJsonb(rows[0].result);
  }

  /**
   * Start an agent and return its execution_id immediately.
   * SELECT synapse.execute_async($1, $2)
   *
   * Note: in the current kernel this still runs inline before returning (a
   * real background queue is a v0.2 refinement), so the row may already be
   * terminal by the time you first poll it. waitForCompletion() handles
   * that transparently either way.
   */
  async executeAsync(agent: string, input: string): Promise<string> {
    const { rows } = await this.db.query<{ execute_async: string }>(
      "SELECT synapse.execute_async($1, $2)",
      [agent, input],
    );
    if (!rows[0]) {
      throw new PgSynapseError(`synapse.execute_async returned no row for agent "${agent}"`);
    }
    return rows[0].execute_async;
  }

  /**
   * The current row for an execution, or null if it doesn't exist.
   * SELECT ... FROM synapse.executions WHERE execution_id = $1
   */
  async status(executionId: string): Promise<ExecutionRow | null> {
    const { rows } = await this.db.query<ExecutionRow>(
      `SELECT execution_id, agent_name, input, output, status, tokens_in, tokens_out,
              cost_usd, duration_ms, caller_role, started_at, finished_at
       FROM synapse.executions WHERE execution_id = $1`,
      [executionId],
    );
    return rows[0] ?? null;
  }

  /**
   * Poll status() until the execution leaves "queued", or throw once
   * timeoutMs elapses.
   */
  async waitForCompletion(
    executionId: string,
    opts: { pollIntervalMs?: number; timeoutMs?: number } = {},
  ): Promise<ExecutionRow> {
    const pollIntervalMs = opts.pollIntervalMs ?? 500;
    const timeoutMs = opts.timeoutMs ?? 120_000;
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const row = await this.status(executionId);
      if (row && row.status !== "queued") return row;
      if (Date.now() >= deadline) {
        throw new PgSynapseError(
          `execution ${executionId} did not finish within ${timeoutMs}ms ` +
            `(last status: ${row?.status ?? "not found"})`,
        );
      }
      await new Promise((resolve) => setTimeout(resolve, pollIntervalMs));
    }
  }

  /**
   * The full message trace for a run, oldest first.
   * SELECT ... FROM synapse.messages WHERE execution_id = $1 ORDER BY seq
   */
  async messages(executionId: string): Promise<MessageRow[]> {
    const { rows } = await this.db.query<MessageRow>(
      `SELECT execution_id, seq, role, content, tool_call_id, tool_name, tool_input, tool_output, ts
       FROM synapse.messages WHERE execution_id = $1 ORDER BY seq`,
      [executionId],
    );
    return rows;
  }

  /** Recent executions, most recent first, optionally filtered to one agent. */
  async recentExecutions(opts: { agent?: string; limit?: number } = {}): Promise<ExecutionRow[]> {
    const { rows } = await this.db.query<ExecutionRow>(
      `SELECT execution_id, agent_name, input, output, status, tokens_in, tokens_out,
              cost_usd, duration_ms, caller_role, started_at, finished_at
       FROM synapse.executions
       WHERE $1::text IS NULL OR agent_name = $1
       ORDER BY started_at DESC LIMIT $2`,
      [opts.agent ?? null, opts.limit ?? 20],
    );
    return rows;
  }

  /** The agents registered in this database. */
  async listAgents(): Promise<AgentRow[]> {
    const { rows } = await this.db.query<AgentRow>(
      `SELECT name, executor_name, llm_profile_main, tools, max_iterations, timeout_ms, cost_cap_usd, trace_level
       FROM synapse.agents ORDER BY name`,
    );
    return rows;
  }
}
