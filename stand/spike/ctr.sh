#!/usr/bin/env bash
# Runs a command in the toolbox container of a node through the node's
# container runtime (k3s crictl), without the Kubernetes API. Use it while etcd
# has no quorum and `tb` cannot reach the pod. The command is passed on stdin
# to `bash -s` inside the container, so it needs no extra quoting.
# usage: ctr.sh <node> <command...>
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

node="$1"; shift
id="$(vm "$node" "k3s crictl ps --name toolbox --state running -q" </dev/null | sed -n 1p)"
if [[ -z "$id" ]]; then
    echo "ctr: no running toolbox container on $node" >&2
    exit 1
fi
printf '%s\n' "$*" | limactl shell --workdir / "$node" sudo k3s crictl exec -i "$id" bash -s
