#!/usr/bin/env bash
# f3_collate seed: write SCALE*3 note files under notes/
# Each file: note_NN.txt, first line "TITLE: item NN"
# SCALE is passed as environment variable (default 1 -> 3 files)
set -euo pipefail

SCALE="${SCALE:-1}"
N_FILES=$(( SCALE * 3 ))

mkdir -p "${FS_ROOT}/notes"

for i in $(seq 1 "$N_FILES"); do
    NN=$(printf "%02d" "$i")
    cat > "${FS_ROOT}/notes/note_${NN}.txt" <<EOF
TITLE: item ${NN}
This is the body of note ${NN}.
It contains some filler text for testing purposes.
EOF
done
