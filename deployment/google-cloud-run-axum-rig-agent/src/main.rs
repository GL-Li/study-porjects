//! Minimal Axum SSE endpoint that streams a Rig agent's reply token-by-token,
//! behind email/password authentication.
//!
//! Run it locally. It needs `DEEPSEEK_API_KEY` and a `SESSION_SECRET` of at
//! least 16 characters, read from the environment or a `.env` file. Over plain
//! HTTP you must also set `COOKIE_SECURE=false`, otherwise the browser rejects
//! the `Secure` session cookie:
//!
//! ```sh
//! set -a; . ~/.env; set +a
//! export SESSION_SECRET="$(openssl rand -base64 32)"
//! export COOKIE_SECURE=false
//! cargo run
//! ```
//!
//! On first run the accounts in `AUTH_EMAILS` (default
//! `liguanglai@gmail.com,lglstock@gmail.com`) are seeded and each temporary
//! password is printed once to the log. Open `http://127.0.0.1:3000/`, sign in,
//! and change the password when prompted; the UI then streams the reply.
//!
//! The same flow via `curl` (login stores the session cookie in `cookies.txt`,
//! and `/chat` stays locked until the password change is done):
//!
//! ```sh
//! curl -s -c cookies.txt \
//!   -d 'email=<email>&password=<temporary-password>' \
//!   http://127.0.0.1:3000/login
//! curl -s -b cookies.txt -c cookies.txt \
//!   -d 'current_password=<temporary-password>&new_password=<new-password>&confirm_password=<new-password>' \
//!   http://127.0.0.1:3000/change-password
//! curl -N -X POST http://127.0.0.1:3000/chat \
//!   -H 'content-type: application/json' \
//!   -b cookies.txt \
//!   -d '{"message":"Tell me a short joke"}'
//! ```

mod auth;

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::{FromRef, Request, State};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use rig::prelude::*;
use rig::providers::deepseek;
use rig::streaming::{Item, StreamEvent};
use serde::Deserialize;
use tokio::net::TcpListener;

use auth::AuthState;

#[derive(Deserialize)]
struct ChatRequest {
    message: String,
}

#[derive(Clone)]
struct AppState {
    agent: rig::Agent,
    auth: Arc<AuthState>,
    last_activity: Arc<AtomicU64>,
}

impl FromRef<AppState> for Arc<AuthState> {
    /// Let handlers extract the shared [`AuthState`] straight from [`AppState`].
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
}

/// Stamp every non-health request as user activity so the idle watchdog can
/// tell traffic apart from liveness probes.
async fn track_activity(State(state): State<AppState>, req: Request, next: Next) -> Response {
    // Health probes are not user activity, so they must not keep the idle
    // watchdog from shutting the process down.
    let is_activity = req.uri().path() != "/health";
    if is_activity {
        state
            .last_activity
            .store(auth::now_secs(), Ordering::Relaxed);
    }
    let response = next.run(req).await;
    if is_activity {
        state
            .last_activity
            .store(auth::now_secs(), Ordering::Relaxed);
    }
    response
}

/// Liveness probe: always reports `ok` without touching the agent or auth.
async fn health() -> &'static str {
    "ok"
}

/// Serve the single-page chat UI.
async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

/// Stream the agent's answer to `message` as Server-Sent Events, emitting
/// `delta`, `done`, and `error` events.
async fn chat(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let stream = state.agent.prompt(req.message).stream();

    let events = stream.filter_map(|item| async move {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                text,
                ..
            }))) => Some(Ok::<Event, Infallible>(
                Event::default().event("delta").data(text),
            )),
            Ok(MultiTurnStreamItem::FinalResponse(response)) => {
                Some(Ok(Event::default().event("done").data(response.output())))
            }
            Ok(_) => None,
            Err(err) => Some(Ok(Event::default().event("error").data(err.to_string()))),
        }
    });

    Sse::new(events).keep_alive(KeepAlive::default())
}

/// Start the server: load config, seed accounts, build the agent, mount the
/// routes behind auth, and shut down once the deployment goes idle.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    if std::env::var("DEEPSEEK_API_KEY")
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "DEEPSEEK_API_KEY is not set or empty. Pass it with `-e DEEPSEEK_API_KEY=...` \
             or `--env-file .env` when running the container.",
        )
        .into());
    }

    let auth = Arc::new(
        AuthState::from_env()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?,
    );

    let emails: Vec<String> = std::env::var("AUTH_EMAILS")
        .unwrap_or_else(|_| "liguanglai@gmail.com,lglstock@gmail.com".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    auth::ensure_seeded(&auth.storage, &emails).await;

    let agent = AgentBuilder::new(deepseek::from_env()?.completion(deepseek::DEEPSEEK_V4_FLASH))
        .preamble("You are a helpful assistant. Keep answers concise.")
        .build();

    let state = AppState {
        agent,
        auth: auth.clone(),
        last_activity: Arc::new(AtomicU64::new(auth::now_secs())),
    };

    let last_activity = state.last_activity.clone();

    let app = Router::new()
        .route("/health", get(health))
        .route("/login", get(auth::login_page).post(auth::login))
        .route("/logout", post(auth::logout))
        .route(
            "/change-password",
            get(auth::change_password_page)
                .post(auth::change_password)
                .route_layer(middleware::from_fn_with_state(
                    state.auth.clone(),
                    auth::require_session,
                )),
        )
        .route(
            "/",
            get(index).route_layer(middleware::from_fn_with_state(
                state.auth.clone(),
                auth::require_active,
            )),
        )
        .route(
            "/chat",
            post(chat).route_layer(middleware::from_fn_with_state(
                state.auth.clone(),
                auth::require_active,
            )),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            track_activity,
        ))
        .with_state(state);

    // Shut the process down after a period with no user activity, so an
    // idle test deployment stops (and stops costing) on its own. Set
    // IDLE_TIMEOUT_SECS=0 to disable. Health probes do not count as activity.
    let idle_timeout = std::env::var("IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1800);

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    if idle_timeout > 0 {
        let last_activity = last_activity.clone();
        let shutdown_tx = shutdown_tx.clone();
        let tick = idle_timeout.clamp(1, 30);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(tick));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let idle = auth::now_secs().saturating_sub(last_activity.load(Ordering::Relaxed));
                if idle >= idle_timeout {
                    eprintln!("no activity for {idle}s (>= {idle_timeout}s); shutting down");
                    let _ = shutdown_tx.send(true);
                    return;
                }
            }
        });
    }

    // Keep the sender alive for the life of the process: dropping every sender
    // would end the `changed()` wait below and shut the server down at once.
    let _shutdown_tx = shutdown_tx;

    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr).await?;
    println!("listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.changed().await;
        })
        .await?;

    Ok(())
}
