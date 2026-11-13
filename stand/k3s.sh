#!/usr/bin/env bash
# Installs k3s on the stand (the first three nodes of NODES are servers, the
# rest join as agents, so etcd keeps three members), blacklists the in-tree drbd
# module, imports the ambar-drbd image and starts the toolbox DaemonSet.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
IMAGE="${IMAGE:-ambar-drbd:dev}"
# k3s release installed on the nodes; get.k3s.io reads it as INSTALL_K3S_VERSION.
K3S_VERSION="${K3S_VERSION:-v1.37.2+k3s1}"
HERE="$(dirname "$0")"

first="${NODES[0]}"
first_ip="$(vm_ip "$first")"

for n in "${NODES[@]}"; do
    printf 'blacklist drbd\ninstall drbd /bin/false\n' \
        | vm_put "$n" /etc/modprobe.d/ambar-blacklist-drbd.conf
    vm "$n" "command -v curl >/dev/null || { apt-get update -qq && apt-get install -y -qq curl >/dev/null; }"
done

# The first node is installed before the loop: the others need its token.
if ! vm "$first" "test -x /usr/local/bin/k3s"; then
    vm "$first" "curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION='$K3S_VERSION' sh -s - server --cluster-init \
        --node-ip $first_ip --disable=traefik --disable=servicelb"
fi
token="$(vm "$first" "cat /var/lib/rancher/k3s/server/node-token")"

# A node that already has /usr/local/bin/k3s is skipped, so a repeated run on a
# running stand installs nothing.
index=0
for n in "${NODES[@]}"; do
    role="$(k3s_role "$index")"
    index=$((index + 1))
    [[ "$role" != init ]] || continue
    if ! vm "$n" "test -x /usr/local/bin/k3s"; then
        if [[ "$role" == server ]]; then
            vm "$n" "curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION='$K3S_VERSION' K3S_TOKEN='$token' sh -s - server \
                --server https://$first_ip:6443 --node-ip $(vm_ip "$n") \
                --disable=traefik --disable=servicelb"
        else
            vm "$n" "curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION='$K3S_VERSION' K3S_URL=https://$first_ip:6443 \
                K3S_TOKEN='$token' sh -s - agent --node-ip $(vm_ip "$n")"
        fi
    fi
done

kc wait --for=condition=Ready node --all --timeout=180s

for n in "${NODES[@]}"; do
    docker save "$IMAGE" | limactl shell --workdir / "$n" sudo k3s ctr images import -
done

# The agent never creates the volumes directory: set it up on every node,
# before the toolbox pods start (a hostPath mount would create it with the
# wrong mode).
for n in "${NODES[@]}"; do
    vm "$n" "install -d -m 0700 /var/lib/rancher/ambar/volumes && install -d -m 0755 /var/lib/rancher/ambar/res"
done

kc apply -f - < "$HERE/toolbox.yaml"
# The image is imported under the same tag after a rebuild: restart the pods so
# they run the new image.
kc -n ambar-spike rollout restart ds/toolbox
kc -n ambar-spike rollout status ds/toolbox --timeout=120s

for n in "${NODES[@]}"; do
    tb "$n" "/opt/ambar/load-module.sh"
done
