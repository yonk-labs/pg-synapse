#!/usr/bin/env bash
# p1_pipeline fs seed: ensure sandbox dir exists. No pre-seeded files.
mkdir -p "${FS_ROOT:-/tmp/pg_synapse_fs/p1_pipeline}"
