# EXAMPLE_OUTPUT: sql-agent-readwrite

Captured from a successful run of `examples/sql-agent-readwrite/run.sh` against
a vLLM endpoint serving `Intel/Qwen3-Coder-Next-int4-AutoRound` at
`http://192.168.1.193:8000/v1`. Date: 2026-05-14.

The full unedited stdout (ANSI stripped) follows. Total tokens across two
executions: 2,889 in, 152 out. Total wall time: ~2.7 s.

```text
>>> Starting pgrx-managed Postgres 17 (if not running)
Auto-detected pgrx extension crate: pg_synapse_pgrx (/home/yonk/yonk-tools/pg-synapse/crates/pg-synapse-pgrx/Cargo.toml)
Auto-detected pgrx extension crate: pg_synapse_pgrx (/home/yonk/yonk-tools/pg-synapse/crates/pg-synapse-pgrx/Cargo.toml)
>>> Verifying connection: /home/yonk/.pgrx:28817
>>> Resetting database: pg_synapse_demo
>>> Installing extension: pg_synapse_pgrx
>>> Applying seed.sql
>>> Initial demo.notes (2 rows):
 id |   body   | added_by
----+----------+----------
  1 | Buy milk | seed
  2 | Call mom | seed
(2 rows)


>>> Running agent workflow
>>>   endpoint: http://192.168.1.193:8000/v1
>>>   model:    Intel/Qwen3-Coder-Next-int4-AutoRound
 llm_profile_set
-----------------

(1 row)

 agent_create
--------------

(1 row)

                                                                                                                                          execute
-----------------------------------------------------------------------------------------------------------------------------------------------------------
 {"output": "The note \"Hello from pg_synapse!\" has been successfully added to the table.", "status": "completed", "cost_usd": null, "tokens_in": 1429, "tokens_out": 60, "tool_calls": [{"args": {"statement": "INSERT INTO demo.notes (body, added_by) VALUES ('Hello from pg_synapse!', 'agent')"}, "name": "sql_exec"}], "duration_ms": 1107, "execution_id": "3fab5de4-74ee-4b91-8cff-1b03a94fe1f7"}
(1 row)

                                                                                                                                          execute
-----------------------------------------------------------------------------------------------------------------------------------------------------------
 {"output": "Here are the notes currently in the table:\n\n1. **Buy milk** (added by: seed)\n2. **Call mom** (added by: seed)\n3. **Hello from pg_synapse!** (added by: agent)", "status": "completed", "cost_usd": null, "tokens_in": 1460, "tokens_out": 92, "tool_calls": [{"args": {"query": "SELECT id, body, added_by FROM demo.notes ORDER BY id", "params": []}, "name": "sql_query"}], "duration_ms": 1555, "execution_id": "d8170677-4759-4ff5-a6a5-d51ada6973d7"}
(1 row)

 id |          body          | added_by |           added_at
----+------------------------+----------+-------------------------------
  1 | Buy milk               | seed     | 2026-05-14 14:22:44.210315-04
  2 | Call mom               | seed     | 2026-05-14 14:22:44.210315-04
  3 | Hello from pg_synapse! | agent    | 2026-05-14 14:22:44.27342-04
(3 rows)


>>> Final demo.notes (3 rows):
 id |          body          | added_by
----+------------------------+----------
  1 | Buy milk               | seed
  2 | Call mom               | seed
  3 | Hello from pg_synapse! | agent
(3 rows)


>>> synapse.executions:
             execution_id             | agent_name  |  status   | tokens_in | tokens_out | duration_ms
--------------------------------------+-------------+-----------+-----------+------------+-------------
 3fab5de4-74ee-4b91-8cff-1b03a94fe1f7 | notes_agent | completed |      1429 |         60 |        1107
 d8170677-4759-4ff5-a6a5-d51ada6973d7 | notes_agent | completed |      1460 |         92 |        1555
(2 rows)


>>> synapse.messages (role / tool_name / content preview):
             execution_id             | seq |   role    | tool_name |                                 content_preview
--------------------------------------+-----+-----------+-----------+----------------------------------------------------------------------------------
 3fab5de4-74ee-4b91-8cff-1b03a94fe1f7 |   0 | system    |           | You are an assistant that manages a demo.notes table.                           +
                                      |     |           |           | You may use sql_query and
 3fab5de4-74ee-4b91-8cff-1b03a94fe1f7 |   1 | user      |           | Add a note that says "Hello from pg_synapse!"
 3fab5de4-74ee-4b91-8cff-1b03a94fe1f7 |   2 | assistant | sql_exec  |
 3fab5de4-74ee-4b91-8cff-1b03a94fe1f7 |   3 | tool      | sql_exec  | {"rows_affected":1}
 3fab5de4-74ee-4b91-8cff-1b03a94fe1f7 |   4 | assistant |           | The note "Hello from pg_synapse!" has been successfully added to the table.
 d8170677-4759-4ff5-a6a5-d51ada6973d7 |   0 | system    |           | You are an assistant that manages a demo.notes table.                           +
                                      |     |           |           | You may use sql_query and
 d8170677-4759-4ff5-a6a5-d51ada6973d7 |   1 | user      |           | What notes are in the table?
 d8170677-4759-4ff5-a6a5-d51ada6973d7 |   2 | assistant | sql_query |
 d8170677-4759-4ff5-a6a5-d51ada6973d7 |   3 | tool      | sql_query | [{"added_by":"seed","body":"Buy milk","id":1},{"added_by":"seed","body":"Call mo
 d8170677-4759-4ff5-a6a5-d51ada6973d7 |   4 | assistant |           | Here are the notes currently in the table:                                      +
                                      |     |           |           |                                                                                 +
                                      |     |           |           | 1. **Buy milk** (added by: seed)                                                +
                                      |     |           |           | 2.
(10 rows)


SUCCESS: demo.notes grew from 2 to 3 rows.
```

## What this proves

1. `synapse.execute` boots the kernel from the SPI-backed `ProfileSource`,
   issues an OpenAI-compatible chat completion against vLLM, receives a
   `tool_calls` response, dispatches the call through `SpiSqlExecutor`, and
   returns the assistant's final natural-language summary as a JSONB envelope.
2. The agent picked the correct tool (`sql_exec` for the INSERT, `sql_query`
   for the SELECT) and inlined literal values in the SQL string, exactly as
   the system prompt directs.
3. Per-execution rows land in `synapse.executions`; every message (system,
   user, assistant, tool) lands in `synapse.messages` with the matching
   `execution_id`.
4. The agent's INSERT is visible inside the same transaction as the
   `synapse.execute()` call: `demo.notes` grew from 2 rows to 3 rows by the
   time the call returned.

## Notes on the model

`Intel/Qwen3-Coder-Next-int4-AutoRound` honors OpenAI-style `tools` +
`tool_choice: auto` natively (no `--tool-call-parser` flag needed on the vLLM
side at the time of capture). Tool-arg deserialization matched the
`sql_query` / `sql_exec` schemas without prompt tuning beyond the v0.1
"inline literals, do not use params" guidance.
