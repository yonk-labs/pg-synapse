-- Registers a llama-cpp LLM profile + agent and runs it twice:
--   once to add a task, once to list pending tasks.
--
-- Prereqs:
--   1. CREATE EXTENSION pg_synapse_pgrx;
--   2. \i examples/local-llama-agent/seed.sql
--   3. llama-server running on LLAMA_BASE_URL (default: http://127.0.0.1:8080/v1)

-- Point at the local llama-server endpoint (OpenAI-compatible).
-- The model name is stored for trait conformance; llama-server ignores it.
SELECT synapse.llm_profile_set(
  'llama-local',
  'llama-cpp',
  'granite-3.0-2b-instruct',
  'http://127.0.0.1:8080/v1',
  NULL,
  '{}'::jsonb
);

-- An agent allowed to read + write via the built-in sql_query / sql_exec tools.
SELECT synapse.agent_create(
  'task_agent',
  $$You are an assistant that manages a demo.tasks table.
You may use sql_query and sql_exec to read and write that table.

When asked to add a task, call sql_exec with
  query: INSERT INTO demo.tasks (title, status, added_by) VALUES ($1, $2, $3)
  params: ["<task title>", "pending", "agent"]

When asked to list tasks, call sql_query with
  query: SELECT id, title, status FROM demo.tasks ORDER BY id
  params: []

Always pass values through the params array using $1, $2, ... placeholders.
Never inline literal values into the SQL string; parameter binding is the
supported and injection-safe path.$$,
  'conversation',
  'llama-local',
  ARRAY['sql_query', 'sql_exec'],
  5,
  60000
);

-- Ask the agent to add a task.
SELECT synapse.execute(
  'task_agent',
  'Add a task called "Run the pg_synapse demo" with status pending'
);

-- Ask the agent to list all tasks.
SELECT synapse.execute('task_agent', 'What tasks are in the table?');

-- Inspect what actually landed in the table (should include the agent-added row).
SELECT id, title, status, added_by FROM demo.tasks ORDER BY id;
