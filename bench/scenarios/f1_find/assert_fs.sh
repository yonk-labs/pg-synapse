#!/usr/bin/env bash
# f1_find assertion: found.txt must exist and contain "b.txt:42"
set -euo pipefail

FOUND="${FS_ROOT}/found.txt"

if [[ ! -f "$FOUND" ]]; then
    echo "FAIL: found.txt does not exist" >&2
    exit 1
fi

if grep -qF "b.txt:42" "$FOUND"; then
    exit 0
else
    echo "FAIL: found.txt does not contain 'b.txt:42'. Contents:" >&2
    cat "$FOUND" >&2
    exit 1
fi
