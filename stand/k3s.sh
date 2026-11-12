#!/usr/bin/env bash
# Installs a three-server k3s cluster on the stand, blacklists the in-tree drbd
# module, imports the ambar-drbd image and starts the toolbox DaemonSet.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
IMAGE="${IMAGE:-ambar-drbd:dev}"
# k3s release installed on the nodes; get.k3s.io reads it as INSTALL_K3S_VERSION.
K3S_VERSION="${K3S_VERSION:-v1.37.2+k3s1}"
HERE="$(dirname "$0")"

first_ip="$(vm_ip dv-1)"

for n in "${NODES[@]}"; do
    printf 'blacklist drbd\ninstall drbd /bin/false\n' \
        | vm_put "$n" /etc/modprobe.d/ambar-blacklist-drbd.conf
    vm "$n" "command -v curl >/dev/null || { apt-get update -qq && apt-get install -y -qq curl >/dev/null; }"
done

if ! vm dv-1 "test -x /usr/local/bin/k3s"; then
    vm dv-1 "curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION='$K3S_VERSION' sh -s - server --cluster-init \
        --node-ip $first_ip --disable=traefik --disable=servicelb"
fi
token="$(vm dv-1 "cat /var/lib/rancher/k3s/server/node-token")"

for n in dv-2 dv-3; do
    if ! vm "$n" "test -x /usr/local/bin/k3s"; then
        vm "$n" "curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION='$K3S_VERSION' K3S_TOKEN='$token' sh -s - server \
            --server https://$first_ip:6443 --node-ip $(vm_ip "$n") \
            --disable=traefik --disable=servicelb"
    fi
done

kc wait --for=condition=Ready node --all --timeout=180s

for n in "${NODES[@]}"; do
    docker save "$IMAGE" | limactl shell --workdir / "$n" sudo k3s ctr images import -
done

kc apply -f - < "$HERE/toolbox.yaml"
# The image is imported under the same tag after a rebuild: restart the pods so
# they run the new image.
kc -n ambar-spike rollout restart ds/toolbox
kc -n ambar-spike rollout status ds/toolbox --timeout=120s

for n in "${NODES[@]}"; do
    tb "$n" "/opt/ambar/load-module.sh"
done
