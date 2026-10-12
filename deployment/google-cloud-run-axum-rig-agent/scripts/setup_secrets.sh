#!/usr/bin/env bash
# One-time setup: create the Secret Manager secrets the Cloud Run service
# mounts (the DeepSeek API key and the session-signing key). Safe to re-run:
# existing secrets are left unchanged. Run once per project before the first
# `scripts/start.sh`.
#
# The DeepSeek key is read from DEEPSEEK_API_KEY, falling back to ~/.env.
#
# Overrides: PROJECT= API_SECRET= SESSION_SECRET=.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

# Fall back to ~/.env when the key is not already exported.
if [[ -z "${DEEPSEEK_API_KEY:-}" && -f "$HOME/.env" ]]; then
  set -a
  # shellcheck disable=SC1090,SC1091
  . "$HOME/.env"
  set +a
fi

PROJECT="${PROJECT:-$(gcloud config get-value project 2>/dev/null)}"
API_SECRET="${API_SECRET:-deepseek-api-key}"
SESSION_SECRET="${SESSION_SECRET:-session-secret}"

if [[ -z "$PROJECT" || "$PROJECT" == "(unset)" ]]; then
  echo "No GCP project set. Pass PROJECT=<id> or run 'gcloud config set project <id>'." >&2
  exit 1
fi

if gcloud secrets describe "$API_SECRET" --project "$PROJECT" >/dev/null 2>&1; then
  echo "Secret '$API_SECRET' already exists; leaving it unchanged."
else
  : "${DEEPSEEK_API_KEY:?set DEEPSEEK_API_KEY, or add it to ~/.env}"
  printf '%s' "$DEEPSEEK_API_KEY" | gcloud secrets create "$API_SECRET" \
    --data-file=- --project "$PROJECT"
  echo "Created secret '$API_SECRET'."
fi

if gcloud secrets describe "$SESSION_SECRET" --project "$PROJECT" >/dev/null 2>&1; then
  echo "Secret '$SESSION_SECRET' already exists; leaving it unchanged."
else
  openssl rand -base64 32 | tr -d '\n' | gcloud secrets create "$SESSION_SECRET" \
    --data-file=- --project "$PROJECT"
  echo "Created secret '$SESSION_SECRET'."
fi

echo "Done. Deploy with: scripts/start.sh"
