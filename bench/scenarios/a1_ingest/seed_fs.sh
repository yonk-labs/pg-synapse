#!/usr/bin/env bash
# a1_ingest seed_fs: write incoming/customers.csv and incoming/orders.json
# into the per-run sandbox. FS_ROOT is set by the harness.
set -euo pipefail

mkdir -p "${FS_ROOT}/incoming"

cat > "${FS_ROOT}/incoming/customers.csv" <<'EOF'
id,name,email,country
1,Alice Nguyen,alice@example.com,US
2,Bob Patel,bob@example.com,IN
3,Carmen Lopez,carmen@example.com,MX
4,David Kim,david@example.com,KR
5,Eva Rossi,eva@example.com,IT
EOF

cat > "${FS_ROOT}/incoming/orders.json" <<'EOF'
[
  {"order_id": 101, "customer_id": 1, "amount": 49.99, "status": "completed"},
  {"order_id": 102, "customer_id": 2, "amount": 129.00, "status": "pending"},
  {"order_id": 103, "customer_id": 1, "amount": 19.50, "status": "completed"},
  {"order_id": 104, "customer_id": 3, "amount": 75.25, "status": "shipped"},
  {"order_id": 105, "customer_id": 4, "amount": 200.00, "status": "completed"},
  {"order_id": 106, "customer_id": 5, "amount": 33.10, "status": "pending"}
]
EOF
