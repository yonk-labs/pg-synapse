# pg-one

The app builder. You give it a sentence; it builds a working database
application inside Postgres and leaves it there.

```
An app that tracks gym visits, with a note per session.
```

Half a minute or so later there is a `gym_tracker` schema with real tables, a
`gym_tracker` agent that knows how to work with them, and some sample rows so
the thing is not empty. You can then talk to it, ask it saved questions, put it
on a schedule, or throw it away.

Builds on the reference stack took 21 to 81 seconds, and **they do not all
finish**: of six observed builds, four completed, one hit its turn cap and one
timed out. That is why Revise and the Activity page exist, and why the builder
registers the agent before it seeds any data.

`http://localhost:8080/pgone` once the stack is up.

---

## Running it

```bash
docker compose up -d
open http://localhost:8080/pgone
```

The first build compiles the Rust extension inside the database image and takes
10 to 25 minutes cold. Later builds hit the Docker cache.

**You supply the LLM.** No endpoint is baked into the repo. On first load the
Connect panel asks for an OpenAI-compatible base URL and model; **Test** probes
`GET /models` through the harness, **Save** writes it to
`synapse.llm_profiles`. To avoid retyping it, put `DEFAULT_LLM_BASE_URL` and
`DEFAULT_LLM_MODEL` in a gitignored `.env` at the repo root.

The model must support function calling. Without it the agent loop cannot make
a tool call and nothing works.

## What an app actually is

Three things with the same name, which is the whole trick:

| | |
| --- | --- |
| A **schema** | `gym_tracker`, holding the app's tables |
| An **agent** | a row in `synapse.agents`, with a system prompt about that schema and the tools it needs |
| An **app record** | a row in `synapse.apps` tying them together, plus a title and description |

Nothing about an app lives outside the database. There is no deploy, no
service, no config file. `pg_dump` takes the app with it.

## The pages

**Dashboard** lists your apps as cards. Each card opens a detail view with the
app's tables, its agent, its saved questions, its schedules, and its run
history.

**Build** is the front door: a sentence in, an app out. You can attach a file
here (see Files) and the built app will be told where it is and what to do with
it. Example prompts are one click away.

**Browse** is a schema explorer for every table the extension can see, with
editable rows. Useful for checking what an agent actually wrote rather than
taking its word.

**Connections** registers external Postgres databases by name, so an agent can
use `remote_query` / `remote_exec` against them. It also does a **brownfield
scan**: point it at an existing database and it proposes apps for what it finds
there. Proposals are gated behind an explicit human review before any agent is
created.

**Samples** ships ready-made prompts and datasets, so a cold demo has something
to build from.

**Activity** is every run, failures included, with the transcript and the tool
calls. This is the page to open when an agent did something surprising.

**Files** lists what you have uploaded, lets you hand a file to an app to
process, and lets you delete it. CSV and JSON go into tables through `load_csv`
/ `load_json` rather than through the model, and `export_csv` writes results
back out to a file that shows up here.

There is an **admin** toggle in the corner. It reveals the harness's own agents
and the `synapse` schema. It is a view filter and not a security boundary:
everything it hides is still reachable over SQL and over the API with
`?admin=1`.

## The agents that ship with it

Four, all `conversation` executor. They are machinery rather than your apps, so
the app list hides them unless admin is on.

| Agent | What it does |
| --- | --- |
| `app_builder` | Turns your sentence into a schema, an agent and seed data. The one doing the work on the Build page. |
| `db_architect` | Schema design and DDL questions in natural language. |
| `data_analyst` | Answers questions about data with `sql_query`. |
| `index_tuner` | Looks at a schema and suggests indexes. |

`app_builder` is the one worth understanding, because everything you build is
downstream of its prompt. It picks a name, creates the schema and tables,
registers the new agent with `synapse.agent_create`, verifies the agent exists,
and only then seeds data. That order is deliberate: if it runs out of turns
part way, you still have a usable app rather than tables nobody can talk to.

It also decides which tools your new app gets, from `sql_query`, `sql_exec`,
`describe_schema`, `load_csv`, `load_json`, `load_url`, `export_csv`,
`read_file`, `remote_query`, `remote_exec`, `search_news`, `fetch_feed`,
`read_article` and `lede_compress`. It grants only what the app needs.

## What you can do to an app once it exists

- **Run it.** Type at it in the detail view. It has its own prompt and its own
  tools.
- **Save a question.** A question you ask often becomes a named, durable thing
  whose SQL is compiled once and re-run by Postgres afterwards, so asking it
  again costs no model call. The compiled SQL is shown, not hidden.
- **Schedule it.** Apps can run on a cadence. The driver is a tokio task in the
  harness, so scheduling stops when the harness stops (see Limits below).
- **Revise it.** Tell it what to change and it edits the app rather than
  building a new one. This is what makes a wrong first build recoverable.
- **Tune its limits.** Turns, wall-clock timeout and cost cap, per agent, from
  the UI.
- **Use it from another agent.** The harness exposes an MCP server at `/mcp`,
  so an outside agent can call your app as a tool.
- **Delete it.** Drops the agent, the app record, its schedules and its
  questions, with its data optional.

## Limits worth knowing before you demo it

- **Agents need a writable database.** Every run writes an audit row, so
  nothing works against a read replica. See `docs/replicas-and-failover.md`.
- **Scheduling lives in the harness, not in Postgres.** Stop the harness and
  schedules stop. `pg_cron` is the answer for a deployment you own and needs
  `shared_preload_libraries` plus a restart.
- **A build is a model call, so it varies and it can fail.** The same sentence
  twice gives two slightly different schemas, and a build can run out of turns
  or time (`app_builder` ships with 24 turns and a 120s budget). A build that
  stopped early usually still left a usable app, because the agent is
  registered before seeding; check Activity, then Revise.
- **The UI is compiled into the harness binary** (`include_str!`), so editing
  `pgone.html` needs a rebuild and a container restart, not a browser reload.
- **Rebuilding the database image needs `docker build --network=host`**;
  BuildKit has no DNS here and `cargo` fails to resolve names without it.
- **`docker compose up --force-recreate db` destroys your apps.** There is no
  volume on `PGDATA`. To iterate on the extension, copy the new `.so` into the
  running container and restart it instead.

## Where the rest is written down

| | |
| --- | --- |
| Is this the right tool at all | `docs/versus-a-cron-script.md` |
| What is defended and what is not | `docs/threat-model.md` |
| What runs on a standby, what failover loses | `docs/replicas-and-failover.md` |
| The `synapse.*` SQL surface | `docs/reference.md` |
| Running the containers | `demo/README.md` |
