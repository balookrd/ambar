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

# Start instances that exist but are not running (limactl stop, host reboot, failed first start).
for n in "${NODES[@]}"; do
    if [[ "$(limactl list --format '{{.Status}}' "$n")" != "Running" ]]; then
        limactl start --tty=false "$n"
    fi
done

for n in "${NODES[@]}"; do
    if [[ "$(limactl shell --workdir / "$n" uname -r)" == "$KVER" ]]; then
        continue
    fi
    vm "$n" "export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq linux-image-$KVER \
            || { echo \"FAIL: cannot install package linux-image-$KVER (KVER=$KVER): not in the apt archive; pin an older image in stand/lima/node.yaml\" >&2; exit 1; }
        mkdir -p /etc/default/grub.d
        cat > /etc/default/grub.d/99-ambar.cfg <<EOF
GRUB_DEFAULT='Advanced options for Debian GNU/Linux>Debian GNU/Linux, with Linux $KVER'
EOF
        update-grub"
    limactl stop "$n"
    limactl start --tty=false "$n"
done

# nftables is needed in every VM regardless of the kernel branch above.
for n in "${NODES[@]}"; do
    vm "$n" "dpkg-query -W -f='\${Status}' nftables 2>/dev/null | grep -q 'install ok installed' || {
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq nftables
    }"
done

"$(dirname "$0")/check.sh"
