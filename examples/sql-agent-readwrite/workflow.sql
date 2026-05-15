-- Registers an LLM profile + agent and runs it twice: once to add a note,
-- once to list the notes.
--
-- Prereqs:
--   1. CREATE EXTENSION pg_synapse_pgrx;
--   2. \i examples/sql-agent-readwrite/seed.sql

-- Point at the local vLLM endpoint (OpenAI-compatible).
SELECT synapse.llm_profile_set(
  'vllm-default',
  'openai',
  'Intel/Qwen3-Coder-Next-int4-AutoRound',
  'http://192.168.1.193:8000/v1',
  NULL,
  '{}'::jsonb
);

-- An agent allowed to read + write via the built-in sql_query / sql_exec tools.
SELECT synapse.agent_create(
  'notes_agent',
  $$You are an assistant that manages a demo.notes table.
You may use sql_query and sql_exec to read and write that table.

When asked to add a note, call sql_exec with
  query: INSERT INTO demo.notes (body, added_by) VALUES ($1, $2)
  params: ["<the note text>", "agent"]
When asked what is in the table, call sql_query with
  query: SELECT id, body, added_by FROM demo.notes ORDER BY id
  params: []

Always pass values through the params array using $1, $2, ... placeholders.
Never inline literal values into the SQL string; parameter binding is the
supported and injection-safe path.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  5,
  60000
);

-- Ask the agent to add a note.
SELECT synapse.execute(
  'notes_agent',
  'Add a note that says "Hello from pg_synapse!"'
);

-- Ask the agent to list the notes.
SELECT synapse.execute('notes_agent', 'What notes are in the table?');

-- Inspect what actually landed in the table (should include the agent-added row).
SELECT * FROM demo.notes ORDER BY id;
