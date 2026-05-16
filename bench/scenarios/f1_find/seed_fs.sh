#!/usr/bin/env bash
# f1_find seed: write 3 files under data/; exactly b.txt contains THE_SECRET_TOKEN=42
set -euo pipefail

mkdir -p "${FS_ROOT}/data"

cat > "${FS_ROOT}/data/a.txt" <<'EOF'
This is file a.
It contains miscellaneous text.
No tokens here.
EOF

cat > "${FS_ROOT}/data/b.txt" <<'EOF'
This is file b.
THE_SECRET_TOKEN=42
Some trailing content.
EOF

cat > "${FS_ROOT}/data/c.txt" <<'EOF'
This is file c.
It also contains miscellaneous text.
No tokens here either.
EOF
