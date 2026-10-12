#!/usr/bin/env bash
# Stop the test deployment by deleting the Cloud Run service. Deleting it is
# the only way to fully stop a Cloud Run service (there is no "pause"); the
# image stays in Artifact Registry and `start.sh` deploys it again.
set -euo pipefail

SERVICE="${SERVICE:-axum-streaming}"
REGION="${REGION:-us-central1}"

if gcloud run services describe "$SERVICE" --region "$REGION" >/dev/null 2>&1; then
  gcloud run services delete "$SERVICE" --region "$REGION" --quiet
  echo "Stopped: deleted Cloud Run service '$SERVICE' in $REGION."
else
  echo "Service '$SERVICE' not found in $REGION; nothing to stop."
fi
