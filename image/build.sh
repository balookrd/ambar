#!/usr/bin/env bash
# Builds the sklad-drbd image for one kernel version.
set -euo pipefail

KVER="${KVER:-6.12.116+deb13-arm64}"
IMAGE="${IMAGE:-sklad-drbd:dev}"
cd "$(dirname "$0")"

docker buildx build \
  --platform linux/arm64 \
  --provenance=false --sbom=false \
  --build-arg "KVER=${KVER}" \
  --tag "$IMAGE" \
  --load .
