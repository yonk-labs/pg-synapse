#!/usr/bin/env bash
# f3_collate assertion: index.txt must have exactly N lines in sorted order,
# each matching "<filename> => <first line of that note file>"
set -euo pipefail

INDEX="${FS_ROOT}/index.txt"
NOTES_DIR="${FS_ROOT}/notes"

if [[ ! -f "$INDEX" ]]; then
    echo "FAIL: index.txt does not exist" >&2
    exit 1
fi

if [[ ! -d "$NOTES_DIR" ]]; then
    echo "FAIL: notes/ directory does not exist" >&2
    exit 1
fi

# Build expected content from the seeded files (ground truth).
EXPECTED="$(
    for f in $(ls "${NOTES_DIR}" | sort); do
        FIRST_LINE="$(head -1 "${NOTES_DIR}/${f}")"
        printf '%s => %s\n' "$f" "$FIRST_LINE"
    done
)"

# Read actual index (strip trailing whitespace/blank lines for robustness).
ACTUAL="$(sed 's/[[:space:]]*$//' "$INDEX" | grep -v '^$' || true)"

if [[ "$ACTUAL" == "$EXPECTED" ]]; then
    exit 0
else
    echo "FAIL: index.txt content does not match expected." >&2
    echo "--- expected ---" >&2
    echo "$EXPECTED" >&2
    echo "--- actual ---" >&2
    echo "$ACTUAL" >&2
    exit 1
fi
