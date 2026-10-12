#!/usr/bin/env bash
# Start the test deployment from the already-built image (no rebuild). Run
# `gcloud builds submit --config cloudbuild.yaml` first if the image is missing.
set -euo pipefail

SERVICE="${SERVICE:-axum-streaming}"
REGION="${REGION:-us-central1}"
REPO="${REPO:-axum}"
SECRET="${SECRET:-deepseek-api-key}"
SESSION_SECRET="${SESSION_SECRET:-session-secret}"
PROJECT="${PROJECT:-$(gcloud config get-value project 2>/dev/null)}"
IMAGE="${REGION}-docker.pkg.dev/${PROJECT}/${REPO}/${SERVICE}:latest"

if ! gcloud artifacts docker images describe "$IMAGE" >/dev/null 2>&1; then
  echo "Image not found: $IMAGE" >&2
  echo "Build and push it first: gcloud builds submit --config cloudbuild.yaml" >&2
  exit 1
fi

gcloud run deploy "$SERVICE" \
  --image "$IMAGE" \
  --region "$REGION" \
  --platform managed \
  --allow-unauthenticated \
  --set-secrets "DEEPSEEK_API_KEY=${SECRET}:latest,SESSION_SECRET=${SESSION_SECRET}:latest" \
  --set-env-vars "IDLE_TIMEOUT_SECS=1800,USERS_BUCKET=axum-streaming-users-${PROJECT},USERS_OBJECT=users.json,COOKIE_SECURE=true" \
  --port 8080 \
  --timeout 3600 \
  --concurrency 20 \
  --min-instances 0 \
  --max-instances 5 \
  --cpu 1 \
  --memory 512Mi \
  --startup-probe=httpGet.path=/health,httpGet.port=8080,initialDelaySeconds=2,periodSeconds=5,failureThreshold=10,timeoutSeconds=2 \
  --liveness-probe=httpGet.path=/health,httpGet.port=8080,periodSeconds=30,timeoutSeconds=2

echo "Started: '$SERVICE' in $REGION. URL:"
gcloud run services describe "$SERVICE" --region "$REGION" --format='value(status.url)'
