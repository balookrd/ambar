#!/usr/bin/env bash
# Verifies kernel version and VM-to-VM connectivity.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

for n in "${NODES[@]}"; do
    k="$(limactl shell --workdir / "$n" uname -r)"
    [[ "$k" == "$KVER" ]] || { echo "FAIL: $n runs $k, want $KVER" >&2; exit 1; }
    nftv="$(vm "$n" "nft --version")" || { echo "FAIL: nft does not work in $n" >&2; exit 1; }
    echo "$n: $nftv"
    echo "$n: kernel $k, host $(vm_host "$n"), ip $(vm_ip "$n")"
done

for a in "${NODES[@]}"; do
    for b in "${NODES[@]}"; do
        [[ "$a" == "$b" ]] && continue
        vm "$a" "ping -c1 -W2 $(vm_ip "$b") >/dev/null" \
            || { echo "FAIL: $a cannot reach $b" >&2; exit 1; }
    done
done
echo "OK: stand is up"
