#!/usr/bin/env bash
# Retag the locally built image for Artifact Registry and push it, ready to be
# deployed to Cloud Run. Build it first with `scripts/build_image.sh`; roll it
# out with `scripts/start.sh`. Authenticate Docker first if needed:
#   gcloud auth configure-docker ${REGION}-docker.pkg.dev
#
# Overrides: SERVICE= REGION= REPO= PROJECT= TAG= LOCAL_IMAGE=.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

SERVICE="${SERVICE:-axum-streaming}"
REGION="${REGION:-us-central1}"
REPO="${REPO:-axum}"
PROJECT="${PROJECT:-$(gcloud config get-value project 2>/dev/null)}"
TAG="${TAG:-$(date +%Y%m%d-%H%M%S)}"
LOCAL_IMAGE="${LOCAL_IMAGE:-axum-streaming:latest}"
REMOTE_IMAGE="${REGION}-docker.pkg.dev/${PROJECT}/${REPO}/${SERVICE}"

if [[ -z "$PROJECT" || "$PROJECT" == "(unset)" ]]; then
  echo "No GCP project set. Pass PROJECT=<id> or run 'gcloud config set project <id>'." >&2
  exit 1
fi

if ! docker image inspect "$LOCAL_IMAGE" >/dev/null 2>&1; then
  echo "Local image $LOCAL_IMAGE not found. Build it first: scripts/build_image.sh" >&2
  exit 1
fi

# The push target must exist in Artifact Registry before the first push.
if ! gcloud artifacts repositories describe "$REPO" --location="$REGION" >/dev/null 2>&1; then
  echo "Creating Artifact Registry repo '$REPO' in $REGION..."
  gcloud artifacts repositories create "$REPO" \
    --repository-format=docker --location="$REGION"
fi

echo "Tagging $LOCAL_IMAGE -> ${REMOTE_IMAGE}:latest ..."
docker tag "$LOCAL_IMAGE" "${REMOTE_IMAGE}:latest"

echo "Pushing ${REMOTE_IMAGE}:latest ..."
docker push "${REMOTE_IMAGE}:latest"

# Keep a unique tag beside :latest so a specific build can be pinned later.
gcloud artifacts docker tags add "${REMOTE_IMAGE}:latest" "${REMOTE_IMAGE}:${TAG}"

echo "Pushed: ${REMOTE_IMAGE}:latest (also tagged :${TAG})"
echo "Deploy with: scripts/start.sh"
