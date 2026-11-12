#!/usr/bin/env bash
# Creates three VMs and boots them into the kernel version used on the k3s nodes.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
TEMPLATE="$(dirname "$0")/lima/node.yaml"

for n in "${NODES[@]}"; do
    if ! limactl list -q | grep -qx "$n"; then
        limactl start --tty=false --name "$n" "$TEMPLATE"
    fi
done

for n in "${NODES[@]}"; do
    if [[ "$(limactl shell --workdir / "$n" uname -r)" == "$KVER" ]]; then
        continue
    fi
    vm "$n" "export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq linux-image-$KVER nftables
        mkdir -p /etc/default/grub.d
        cat > /etc/default/grub.d/99-duovol.cfg <<EOF
GRUB_DEFAULT='Advanced options for Debian GNU/Linux>Debian GNU/Linux, with Linux $KVER'
EOF
        update-grub"
    limactl stop "$n"
    limactl start --tty=false "$n"
done

"$(dirname "$0")/check.sh"
