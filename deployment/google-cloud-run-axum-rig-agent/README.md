# axum-streaming

Minimal Axum SSE endpoint that streams a [Rig](https://github.com/0xPlaygrounds/rig) agent's reply token-by-token, behind email/password authentication.

## Requirements

- `DEEPSEEK_API_KEY` — the DeepSeek key the agent uses.
- `SESSION_SECRET` — at least 16 characters, used to sign session cookies.

Both can come from the environment or a local `.env` file.

## Build

```sh
cargo build --release
```

## Run

`PORT` is read at runtime (it is not baked into the binary), so set it when you
launch the process. For local plain-HTTP development disable the `Secure` cookie
flag and point the user store at a file:

```sh
export SESSION_SECRET="$(openssl rand -base64 32)"
export COOKIE_SECURE=false
export USERS_FILE=./users.json
PORT=3977 cargo run --release
```

On the first run with no store, the app seeds the accounts in `AUTH_EMAILS`,
prints each temporary password once, and forces a password change at first
login. Then open `http://127.0.0.1:3977/` and sign in.

If `PORT` is unset the server defaults to `3000`; `HOST` defaults to `0.0.0.0`
(set `HOST=127.0.0.1` to bind loopback only).

The process shuts itself down after `IDLE_TIMEOUT_SECS` (default `1800`, i.e.
30 min) with no request, and exits `0`. Health probes do not count as activity.
Set `IDLE_TIMEOUT_SECS=0` to disable.

## Authentication

- Email + password, hashed with **argon2id**.
- Sessions are signed JWTs (HS256) in an `HttpOnly`, `SameSite=Lax` cookie
  (`Secure` unless `COOKIE_SECURE=false`).
- Routes: `GET/POST /login`, `GET/POST /change-password`, `POST /logout`.
  `/` and `/chat` require an active session; a freshly seeded account is
  redirected to `/change-password` until it changes its password.
- Access is restricted to the emails in `AUTH_EMAILS` (default
  `liguanglai@gmail.com,lglstock@gmail.com`).
- User records live in one JSON document: a Google Cloud Storage object when
  `USERS_BUCKET` is set, otherwise a local file (`USERS_FILE`, default
  `users.json`). Password changes are written back to that document, so they
  persist across restarts and instances.
- CSRF is mitigated by `SameSite=Lax`; login attempts are not rate-limited.

### Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `DEEPSEEK_API_KEY` | — (required) | Provider key. |
| `SESSION_SECRET` | — (required) | HMAC key for session cookies. |
| `SESSION_TTL_SECS` | `28800` | Session lifetime. |
| `COOKIE_SECURE` | `true` | Set `false` for local plain HTTP. |
| `AUTH_EMAILS` | `liguanglai@gmail.com,lglstock@gmail.com` | Emails seeded on first run. |
| `USERS_BUCKET` | — | GCS bucket holding the user document. |
| `USERS_OBJECT` | `users.json` | Object name in that bucket. |
| `USERS_FILE` | `users.json` | Local store path when no bucket is set. |
| `IDLE_TIMEOUT_SECS` | `1800` | Idle self-shutdown; `0` disables. |
| `HOST` / `PORT` | `0.0.0.0` / `3000` | Bind address. |

## Docker

Build the image:

```sh
docker build -t axum-streaming .
```

Run it. Over plain HTTP locally, disable the secure cookie and pass a session
secret; the user store defaults to a file inside the container (ephemeral):

```sh
export SESSION_SECRET="$(openssl rand -base64 32)"
docker run --rm -p 3977:3977 \
  -e DEEPSEEK_API_KEY="$DEEPSEEK_API_KEY" \
  -e SESSION_SECRET="$SESSION_SECRET" \
  -e COOKIE_SECURE=false \
  axum-streaming
```

`HOST`, `PORT`, `USERS_FILE`, etc. can be overridden the same way. The image
does not contain a `.env` file; pass variables with `-e` or `--env-file`.

## Deploy to Google Cloud Run

Cloud Run runs the container as-is, injects `PORT` (default `8080`), and supports
SSE streaming. The `GET /health` route is used for startup/liveness probes.

### One-time setup

```sh
PROJECT=my-project
REGION=us-central1

gcloud config set project "$PROJECT"

gcloud services enable \
  run.googleapis.com artifactregistry.googleapis.com \
  cloudbuild.googleapis.com secretmanager.googleapis.com

# Artifact Registry repo for the image (name must match _REPO in cloudbuild.yaml).
gcloud artifacts repositories create axum \
  --repository-format=docker --location="$REGION"

# Secrets: the DeepSeek key and the session-signing key.
printf '%s' "$DEEPSEEK_API_KEY" | gcloud secrets create deepseek-api-key --data-file=-
openssl rand -base64 32 | tr -d '\n' | gcloud secrets create session-secret --data-file=-

# GCS bucket for the user records, writable by the Cloud Run runtime account.
PROJECT_NUMBER=$(gcloud projects describe "$PROJECT" --format='value(projectNumber)')
BUCKET="axum-streaming-users-${PROJECT}"
gcloud storage buckets create "gs://${BUCKET}" --location="$REGION"
gcloud storage buckets add-iam-policy-binding "gs://${BUCKET}" \
  --member="serviceAccount:${PROJECT_NUMBER}-compute@developer.gserviceaccount.com" \
  --role="roles/storage.objectAdmin"

# Let the Cloud Build service account deploy and read the secrets.
CB_SA="${PROJECT_NUMBER}@cloudbuild.gserviceaccount.com"
for ROLE in roles/run.admin roles/artifactregistry.writer \
            roles/iam.serviceAccountUser roles/secretmanager.secretAccessor; do
  gcloud projects add-iam-policy-binding "$PROJECT" \
    --member="serviceAccount:${CB_SA}" --role="$ROLE" --quiet
done
```

Leave the bucket empty: on first start the app seeds `AUTH_EMAILS` there and
prints the temporary passwords to the Cloud Run logs.

### Deploy

```sh
gcloud builds submit --config cloudbuild.yaml
```

`cloudbuild.yaml` builds the image, pushes it to Artifact Registry, and deploys
the service with the settings tuned for streaming (60 min timeout,
`--concurrency 20`, `0–5` instances, `/health` probes, key and session secret
from Secret Manager, user store in GCS). Override defaults with substitutions,
e.g. `--substitutions _REGION=europe-west1,_MAX_INSTANCES=2`. The image is
tagged with both `$BUILD_ID` and `latest`.

Retrieve the seeded temporary passwords:

```sh
gcloud logging read \
  'resource.type="cloud_run_revision" AND resource.labels.service_name="axum-streaming" AND textPayload:"Temporary passwords"' \
  --limit=1 --format='value(textPayload)' --freshness=1d
```

## Cost control

- `--min-instances=0` means Cloud Run scales to zero, so idle time costs nothing.
- The app also exits after `IDLE_TIMEOUT_SECS` (default 30 min) with no request.
- `--max-instances=5` caps burst instances.

Manual control:

```sh
scripts/stop.sh    # deletes the Cloud Run service (stops all billing)
scripts/start.sh   # re-deploys the existing :latest image
```

Both accept `SERVICE=`, `REGION=`, `REPO=`, `SECRET=`, `SESSION_SECRET=`,
`PROJECT=` overrides. `stop.sh` deletes the service because Cloud Run has no
paused state; the image remains in Artifact Registry for `start.sh`.

## Security notes

- The app enforces email/password auth, so `--allow-unauthenticated` at the
  platform level is what lets users reach the login page. Every request still
  spends your DeepSeek credits, so keep `--max-instances` low and add rate
  limiting (e.g. Cloud Armor) before wider sharing.
- Rotate `SESSION_SECRET` to invalidate all sessions.

## Troubleshooting

- **Container exits immediately** — `DEEPSEEK_API_KEY` or `SESSION_SECRET` missing.
- **Login says "Server error"** — the app could not read/write the user store;
  check the GCS bucket IAM for the runtime service account.
- **Cookie not kept over HTTP** — set `COOKIE_SECURE=false` for local runs.
- **Host port already in use** — publish a different host port, e.g.
  `-p 13977:3977`.
