#!/usr/bin/env bash
# Deletes the stand VMs and their disks.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

for n in "${NODES[@]}"; do
    limactl delete --force "$n" || true
done
