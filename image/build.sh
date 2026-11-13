#!/usr/bin/env bash
# Builds the ambar-drbd image for one kernel version.
set -euo pipefail

KVER="${KVER:-6.12.116+deb13-arm64}"
IMAGE="${IMAGE:-ambar-drbd:dev}"
cd "$(dirname "$0")"

# The agent is cross-compiled on the host and copied into the build context.
(cd .. && cargo zigbuild --release --target aarch64-unknown-linux-musl)
cp ../target/aarch64-unknown-linux-musl/release/ambar ambar

docker buildx build \
  --platform linux/arm64 \
  --provenance=false --sbom=false \
  --build-arg "KVER=${KVER}" \
  --tag "$IMAGE" \
  --load .
