-- Scenario: inventory reorder. Seeds a product catalog with stock levels, some
-- below their reorder point; the agent finds them and raises a restock order
-- for each. Business value: an ops/supply-chain reorder pass. Reload-safe.
-- Assumes the UI configured the 'vllm-default' LLM profile.

CREATE SCHEMA IF NOT EXISTS ops;

DROP TABLE IF EXISTS ops.restock_orders;
DROP TABLE IF EXISTS ops.products;

CREATE TABLE ops.products (
  id            SERIAL PRIMARY KEY,
  sku           TEXT NOT NULL,
  name          TEXT NOT NULL,
  stock         INT NOT NULL,
  reorder_point INT NOT NULL,
  reorder_qty   INT NOT NULL
);

CREATE TABLE ops.restock_orders (
  id         SERIAL PRIMARY KEY,
  product_id INT NOT NULL REFERENCES ops.products(id),
  qty        INT NOT NULL,
  note       TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO ops.products (sku, name, stock, reorder_point, reorder_qty) VALUES
  ('WID-001', 'Widget, standard',      4,  25, 100),
  ('WID-002', 'Widget, heavy-duty',   60,  20,  50),
  ('GAD-010', 'Gadget, mini',          0,  15, 200),
  ('GAD-011', 'Gadget, pro',          18,  10,  40),
  ('CBL-100', 'Cable, 2m',           130, 100, 300),
  ('CBL-101', 'Cable, 5m',            12,  30, 150);

SELECT synapse.agent_create(
  'reorder_agent',
  $$You are an inventory planner. You keep products in stock by raising restock
orders before anything runs out.

The data:
- ops.products(id, sku, name, stock, reorder_point, reorder_qty).
- ops.restock_orders(product_id, qty, note) is where you raise orders.

How to work:
- Find the products whose stock is at or below their reorder_point.
- For each, raise one restock order for that product's reorder_qty, with a
  short note saying the current stock and reorder point.
- Do NOT reorder products that are comfortably above their reorder point.
- Finish with one line per order: "<sku> <name>: ordered <qty> (stock <n> <=
  reorder point <p>)".

Pass values through the params array ($1, $2, ...); never inline them. Run ONE
statement per tool call and never end it with a semicolon.$$,
  'conversation',
  'vllm-default',
  ARRAY['sql_query', 'sql_exec'],
  14,
  150000
);

SELECT synapse.agent_set_trace_level('reorder_agent', 'debug');
