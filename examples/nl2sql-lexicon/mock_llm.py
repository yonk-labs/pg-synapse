#!/usr/bin/env python3
"""Deterministic OpenAI-compatible mock LLM for the pg-lexicon NL2SQL demo.

This stands in for a real model so the demo runs with no API key. It is NOT a
model: it scripts a fixed 3-step agent loop. But it is honest about the point
of the integration: on the SQL-writing turn it PARSES the get_schema_context
tool result (the real pg-lexicon ContextPackage) and builds the JOIN from the
foreign-key relationship pg-lexicon supplied. If that relationship were absent
from the context, this mock would fall back to a single-table query and the
demo's JOIN assertion would fail. So a passing demo proves the lexicon context
is what enables the correct JOIN.

Everything else in the loop is real: the synapse agent runtime, the
get_schema_context plugin, the HTTP call to pg-lexicon, and the sql_query
execution against the seeded ecommerce database.
"""
import json
import re
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8991


def called_tools(messages):
    """Names of tools already invoked (from assistant tool_calls)."""
    names = set()
    for m in messages:
        for tc in (m.get("tool_calls") or []):
            fn = (tc.get("function") or {}).get("name")
            if fn:
                names.add(fn)
    return names


def user_question(messages):
    for m in messages:
        if m.get("role") == "user":
            return (m.get("content") or "").strip()
    return ""


def find_context_result(messages):
    """The content of the tool message answering get_schema_context."""
    # map tool_call_id -> function name
    id_to_fn = {}
    for m in messages:
        for tc in (m.get("tool_calls") or []):
            id_to_fn[tc.get("id")] = (tc.get("function") or {}).get("name")
    for m in messages:
        if m.get("role") == "tool" and id_to_fn.get(m.get("tool_call_id")) == "get_schema_context":
            return m.get("content") or ""
    return ""


def build_join_sql(context_text, question):
    """Build revenue-per-customer SQL FROM the lexicon context. The JOIN is
    constructed from the FK relationship the context supplied, not hardcoded."""
    try:
        ctx = json.loads(context_text)
    except Exception:
        # context may be double-wrapped as a JSON string
        try:
            ctx = json.loads(json.loads(context_text))
        except Exception:
            ctx = {}
    schema = ctx.get("schema", "ecommerce")
    rels = ctx.get("relationships", []) or []
    # find orders -> customers (the relationship revenue-per-customer needs)
    rel = None
    for r in rels:
        if r.get("source_table") == "orders" and r.get("target_table") == "customers":
            rel = r
            break
    if not rel:
        # No JOIN available from context: degrade to a single-table query.
        # (This is the "lexicon didn't help" branch the demo asserts against.)
        return f"SELECT SUM(total_amount) AS revenue FROM {schema}.orders"
    sc, scol = rel["source_table"], rel["source_column"]
    tc, tcol = rel["target_table"], rel["target_column"]
    # Cast the NUMERIC aggregate to float8: synapse's sql_query serializes
    # Postgres NUMERIC as JSON null, so an NL2SQL model targeting it should
    # emit a JSON-friendly numeric type.
    return (
        f"SELECT c.name AS customer, SUM(o.total_amount)::float8 AS revenue "
        f"FROM {schema}.{sc} o "
        f"JOIN {schema}.{tc} c ON o.{scol} = c.{tcol} "
        f"GROUP BY c.name ORDER BY revenue DESC"
    )


def tool_call_response(call_id, name, arguments_obj):
    return {
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": None,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": json.dumps(arguments_obj)},
                }],
            },
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 10},
    }


def final_response(text):
    return {
        "choices": [{
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": text},
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 10},
    }


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass  # quiet

    def _send(self, obj):
        body = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        # /v1/models probe
        self._send({"data": [{"id": "mock-nl2sql", "object": "model"}]})

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        req = json.loads(self.rfile.read(n) or b"{}")
        messages = req.get("messages", [])
        called = called_tools(messages)
        # Trace every tool result synapse feeds back, so the demo can prove the
        # real context + real query rows flowed through the loop.
        with open("/tmp/nl2sql-trace.log", "a") as f:
            f.write(f"\n===== LLM CALL (tools already called: {sorted(called)}) =====\n")
            for m in messages:
                if m.get("role") == "tool":
                    f.write(f"[tool result for {m.get('tool_call_id')}]: "
                            f"{(m.get('content') or '')[:1200]}\n")

        if "get_schema_context" not in called:
            # Step 1: fetch schema context for the user's question.
            self._send(tool_call_response(
                "call_ctx", "get_schema_context",
                {"question": user_question(messages)}))
        elif "sql_query" not in called:
            # Step 2: build SQL from the lexicon context and run it.
            ctx_text = find_context_result(messages)
            sql = build_join_sql(ctx_text, user_question(messages))
            self._send(tool_call_response("call_sql", "sql_query", {"query": sql}))
        else:
            # Step 3: summarize.
            self._send(final_response(
                "Computed total revenue per customer by joining orders to "
                "customers on the pg-lexicon-provided foreign key."))


if __name__ == "__main__":
    print(f"mock LLM on http://127.0.0.1:{PORT}/v1", file=sys.stderr)
    HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
