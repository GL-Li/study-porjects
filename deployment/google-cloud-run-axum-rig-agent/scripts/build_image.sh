#!/usr/bin/env bash
# Build the container image locally. `scripts/push_to_registry.sh` retags it
# for Artifact Registry and pushes it. No GCP access needed here.
#
# Overrides: LOCAL_IMAGE= PLATFORM=.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

LOCAL_IMAGE="${LOCAL_IMAGE:-axum-streaming:latest}"

# Cloud Run runs linux/amd64. Pin it so the image still boots when built on a
# different architecture (e.g. an Apple Silicon or ARM machine), where Docker
# would otherwise produce an arm64 image that Cloud Run rejects at startup.
PLATFORM="${PLATFORM:-linux/amd64}"

echo "Building ${LOCAL_IMAGE} for ${PLATFORM} ..."
docker build --platform "$PLATFORM" -t "$LOCAL_IMAGE" .

echo "Built: ${LOCAL_IMAGE}"
echo "Push with: scripts/push_to_registry.sh"
