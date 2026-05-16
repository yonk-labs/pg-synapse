#!/usr/bin/env bash
# f2_edit seed: write config.ini with three settings
set -euo pipefail

mkdir -p "${FS_ROOT}"

cat > "${FS_ROOT}/config.ini" <<'EOF'
timeout=30
retries=3
mode=fast
EOF
