# EXAMPLE_OUTPUT: local-llama-agent

This file is a template. It describes the expected output structure for a
successful run of `examples/local-llama-agent/run.sh` on a host where
`llama-server` is installed and the Granite GGUF has been downloaded. Actual
output will differ in execution IDs, timing, and token counts.

Replace this template with a captured run once the demo has been executed on
your host.

## Expected stdout structure

```text
>>> llama-server found: /usr/local/bin/llama-server
>>> GGUF already cached: /home/<user>/.cache/pg-synapse/models/lmstudio-community/granite-3.0-2b-instruct-GGUF/granite-3.0-2b-instruct-Q4_K_M.gguf
>>> Starting llama-server on port <FREE_PORT>
>>> Waiting for llama-server to become ready...
>>> llama-server ready (took <N>s)
>>> Starting pgrx-managed Postgres 17 (if not running)
>>> Verifying connection: /home/<user>/.pgrx:28817
>>> Resetting database: pg_synapse_demo
>>> Installing extension: pg_synapse_pgrx
>>> Applying seed.sql
>>> Initial demo.tasks (2 rows):
 id |     title       | status  | added_by
----+-----------------+---------+----------
  1 | Buy groceries   | pending | seed
  2 | Write tests     | done    | seed
(2 rows)


>>> Running agent workflow
>>>   endpoint: http://127.0.0.1:<FREE_PORT>/v1
>>>   model:    granite-3.0-2b-instruct-Q4_K_M.gguf
 llm_profile_set
-----------------

(1 row)

 agent_create
--------------

(1 row)

                                                 execute
---------------------------------------------------------
 {"output": "The task ... has been added.", "status": "completed", ...}
(1 row)

                                                 execute
---------------------------------------------------------
 {"output": "Here are the tasks in the table:\n...", "status": "completed", ...}
(1 row)

 id |              title               | status  | added_by
----+----------------------------------+---------+----------
  1 | Buy groceries                    | pending | seed
  2 | Write tests                      | done    | seed
  3 | Run the pg_synapse demo          | pending | agent
(3 rows)


>>> Final demo.tasks (3 rows):
 id |              title               | status  | added_by
----+----------------------------------+---------+----------
  1 | Buy groceries                    | pending | seed
  2 | Write tests                      | done    | seed
  3 | Run the pg_synapse demo          | pending | agent
(3 rows)


>>> synapse.executions:
             execution_id             | agent_name | status    | tokens_in | tokens_out | duration_ms
--------------------------------------+------------+-----------+-----------+------------+-------------
 <uuid-1>                             | task_agent | completed |      <N>  |      <N>   |     <N>
 <uuid-2>                             | task_agent | completed |      <N>  |      <N>   |     <N>
(2 rows)


>>> synapse.messages (role / tool_name / content preview):
             execution_id             | seq | role      | tool_name | content_preview
--------------------------------------+-----+-----------+-----------+-----------------
 <uuid-1>                             |   0 | system    |           | You are an assistant that manages a demo.tasks table.
 <uuid-1>                             |   1 | user      |           | Add a task called "Run the pg_synapse demo" with status pending
 <uuid-1>                             |   2 | assistant | sql_exec  |
 <uuid-1>                             |   3 | tool      | sql_exec  | {"rows_affected":1}
 <uuid-1>                             |   4 | assistant |           | The task has been added successfully.
 <uuid-2>                             |   0 | system    |           | You are an assistant that manages a demo.tasks table.
 <uuid-2>                             |   1 | user      |           | What tasks are in the table?
 <uuid-2>                             |   2 | assistant | sql_query |
 <uuid-2>                             |   3 | tool      | sql_query | [{"id":1,"title":"Buy groceries","status":"pending"},...
 <uuid-2>                             |   4 | assistant |           | Here are the tasks currently in the table:
(10 rows)


>>> Stopping llama-server (pid <PID>)
SUCCESS: demo.tasks grew from 2 to 3 rows.
```

## What a successful run proves

1. The `llama-cpp` provider factory builds a `LlamaCppProvider` from the
   `llama-local` profile, pointing at a running `llama-server` process.
2. `synapse.execute` boots the kernel from the SPI-backed `ProfileSource`,
   issues an OpenAI-compatible chat completion against `llama-server`, receives
   a `tool_calls` response, dispatches the call through `SpiSqlExecutor`, and
   returns the final summary as a JSONB envelope.
3. The agent picked the correct tool (`sql_exec` for the INSERT, `sql_query`
   for the SELECT) and used parameterized placeholders as instructed.
4. Per-execution rows land in `synapse.executions`; every message (system,
   user, assistant, tool) lands in `synapse.messages`.
5. The agent's INSERT is visible in `demo.tasks` by the time the
   `synapse.execute()` call returns.

## Notes on model choice

`granite-3.0-2b-instruct-Q4_K_M.gguf` is a ~1.5 GB quantized model that runs
on CPU without a GPU. It supports OpenAI-style tool calls when loaded by
`llama-server` (built from llama.cpp 2025 builds with tool-call support). If
tool calls are not produced, switch to a larger quant (`Q5_K_M` or `Q8_0`) or
to `granite-3.1-2b-instruct` via the `LLAMA_HF_*` env vars.

For faster inference, add `--gpu-layers N` to the `llama-server` invocation
in `run.sh` to offload `N` layers to a GPU.
