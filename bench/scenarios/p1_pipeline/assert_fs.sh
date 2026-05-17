#!/usr/bin/env bash
# p1_pipeline assertion: research.log exists, contains a timestamp line and a Brief line.
set -euo pipefail
LOG="${FS_ROOT:-/tmp/pg_synapse_fs/p1_pipeline}/research.log"
[[ -f "$LOG" ]] || { echo "FAIL: research.log not found"; exit 1; }
grep -q 'Research log started:' "$LOG" || { echo "FAIL: missing timestamp line"; exit 1; }
grep -qi 'Brief:' "$LOG" || { echo "FAIL: missing Brief line"; exit 1; }
echo "PASS"
