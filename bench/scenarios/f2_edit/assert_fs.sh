#!/usr/bin/env bash
# f2_edit assertion: config.ini must have timeout=99, retries=3, mode=fast (no clobber)
set -euo pipefail

CONFIG="${FS_ROOT}/config.ini"

if [[ ! -f "$CONFIG" ]]; then
    echo "FAIL: config.ini does not exist" >&2
    exit 1
fi

FAIL=0

if ! grep -qF "timeout=99" "$CONFIG"; then
    echo "FAIL: config.ini does not contain 'timeout=99'" >&2
    FAIL=1
fi

if ! grep -qF "retries=3" "$CONFIG"; then
    echo "FAIL: config.ini does not contain 'retries=3' (clobbered?)" >&2
    FAIL=1
fi

if ! grep -qF "mode=fast" "$CONFIG"; then
    echo "FAIL: config.ini does not contain 'mode=fast' (clobbered?)" >&2
    FAIL=1
fi

if [[ $FAIL -ne 0 ]]; then
    echo "Actual contents:" >&2
    cat "$CONFIG" >&2
    exit 1
fi

exit 0
