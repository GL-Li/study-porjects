//! Email/password authentication.
//!
//! Passwords are hashed with argon2id and sessions are signed JWTs carried in
//! an `HttpOnly` cookie. User records live in a single JSON document, stored in
//! Google Cloud Storage when `USERS_BUCKET` is set (the Cloud Run case) or in a
//! local file otherwise (development).
//!
//! Accounts seeded for the first time start with a generated temporary
//! password and `must_change_password = true`; the app refuses every protected
//! route until the password is changed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::{Form, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Extension, Json};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

/// Current Unix time in whole seconds, or `0` if the clock predates the epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Shared authentication configuration: where user records live, the key used to
/// sign/verify session JWTs, how long a session lasts, and whether the session
/// cookie carries the `Secure` flag.
pub struct AuthState {
    pub storage: Storage,
    session_secret: Vec<u8>,
    session_ttl: u64,
    cookie_secure: bool,
}

impl AuthState {
    /// Build the state from the environment: `SESSION_SECRET` (required, at
    /// least 16 characters), `SESSION_TTL_SECS` (default 28 800), `COOKIE_SECURE`
    /// (default true), and the [`Storage`] backend.
    pub fn from_env() -> Result<Self, String> {
        let secret =
            std::env::var("SESSION_SECRET").map_err(|_| "SESSION_SECRET is not set".to_string())?;
        if secret.len() < 16 {
            return Err("SESSION_SECRET must be at least 16 characters".into());
        }
        let session_ttl = std::env::var("SESSION_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(28_800);
        let cookie_secure = std::env::var("COOKIE_SECURE")
            .map(|v| !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        Ok(Self {
            storage: Storage::from_env()?,
            session_secret: secret.into_bytes(),
            session_ttl,
            cookie_secure,
        })
    }

    /// Sign a session JWT for `email` with the configured TTL, embedding the
    /// `must_change` flag so middleware can gate protected routes.
    fn issue(&self, email: &str, must_change: bool) -> Result<String, String> {
        let claims = Claims {
            sub: email.to_string(),
            exp: now_secs() + self.session_ttl,
            must_change,
        };
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(&self.session_secret),
        )
        .map_err(|e| e.to_string())
    }

    /// Decode and verify the session JWT from the `session` cookie, returning
    /// its claims, or `None` when the cookie is missing, malformed, or expired.
    fn claims_from_headers(&self, headers: &HeaderMap) -> Option<Claims> {
        let token = session_cookie(headers)?;
        let validation = Validation::new(Algorithm::HS256);
        decode::<Claims>(
            &token,
            &DecodingKey::from_secret(&self.session_secret),
            &validation,
        )
        .ok()
        .map(|data| data.claims)
    }
}

/// Payload of a session JWT: the signed-in user, its expiry, and whether the
/// user must still change their password before protected routes are allowed.
#[derive(Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: u64,
    pub must_change: bool,
}

// ---------------------------------------------------------------------------
// User records and storage
// ---------------------------------------------------------------------------

/// The whole user database as persisted: a single JSON document listing every
/// account. Loaded and saved as one unit by [`Storage`].
#[derive(Serialize, Deserialize, Default)]
pub struct UserStore {
    pub users: Vec<User>,
}

/// One account: its email, the argon2id password hash, and whether a password
/// change is still required (true for freshly seeded accounts).
#[derive(Serialize, Deserialize)]
pub struct User {
    pub email: String,
    pub password_hash: String,
    #[serde(default)]
    pub must_change_password: bool,
}

/// Where the [`UserStore`] document is kept: a Google Cloud Storage object on
/// Cloud Run, or a local file during development.
#[derive(Clone)]
pub enum Storage {
    Gcs(Gcs),
    File(PathBuf),
}

impl Storage {
    /// Choose the backend from the environment: Google Cloud Storage when
    /// `USERS_BUCKET` is set (object name from `USERS_OBJECT`), otherwise a local
    /// file from `USERS_FILE`.
    pub fn from_env() -> Result<Self, String> {
        if let Ok(bucket) = std::env::var("USERS_BUCKET") {
            let object = std::env::var("USERS_OBJECT").unwrap_or_else(|_| "users.json".into());
            let client = reqwest::Client::builder()
                .build()
                .map_err(|e| e.to_string())?;
            Ok(Storage::Gcs(Gcs {
                client,
                bucket,
                object,
                token: Arc::new(Mutex::new(TokenCache::default())),
            }))
        } else {
            let path = std::env::var("USERS_FILE").unwrap_or_else(|_| "users.json".into());
            Ok(Storage::File(PathBuf::from(path)))
        }
    }

    /// Read and parse the [`UserStore`] document, returning `Ok(None)` when it
    /// has not been created yet.
    pub async fn load(&self) -> Result<Option<UserStore>, String> {
        match self {
            Storage::File(path) => match tokio::fs::read(path).await {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|e| format!("parse {}: {e}", path.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("read {}: {e}", path.display())),
            },
            Storage::Gcs(gcs) => match gcs.get().await? {
                Some(bytes) => serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|e| format!("parse gs://{}/{}: {e}", gcs.bucket, gcs.object)),
                None => Ok(None),
            },
        }
    }

    /// Serialize `store` and overwrite the whole document.
    pub async fn save(&self, store: &UserStore) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(store).map_err(|e| e.to_string())?;
        match self {
            Storage::File(path) => tokio::fs::write(path, &bytes)
                .await
                .map_err(|e| format!("write {}: {e}", path.display())),
            Storage::Gcs(gcs) => gcs.put(&bytes, false).await.map(|_| ()),
        }
    }

    /// Store the document only if it does not exist yet. Returns `Ok(true)` when
    /// this call created it, `Ok(false)` when another instance already had.
    async fn save_if_absent(&self, store: &UserStore) -> Result<bool, String> {
        let bytes = serde_json::to_vec_pretty(store).map_err(|e| e.to_string())?;
        match self {
            Storage::File(path) => {
                if tokio::fs::try_exists(path).await.unwrap_or(false) {
                    return Ok(false);
                }
                if let Some(parent) = path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                tokio::fs::write(path, &bytes)
                    .await
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
                Ok(true)
            }
            Storage::Gcs(gcs) => gcs.put(&bytes, true).await,
        }
    }
}

/// Minimal Google Cloud Storage client for reading/writing a single object,
/// using an OAuth token fetched from the instance metadata server.
#[derive(Clone)]
pub struct Gcs {
    client: reqwest::Client,
    bucket: String,
    object: String,
    token: Arc<Mutex<TokenCache>>,
}

/// Cached metadata-server OAuth token together with its expiry, so consecutive
/// GCS calls do not refetch it every time.
#[derive(Default)]
struct TokenCache {
    token: Option<String>,
    expires_at: u64,
}

impl Gcs {
    /// Access token for the runtime service account, from the metadata server.
    async fn token(&self) -> Result<String, String> {
        let mut cache = self.token.lock().await;
        let now = now_secs();
        if let Some(token) = &cache.token && now + 60 < cache.expires_at {
            return Ok(token.clone());
        }
        let resp = self
            .client
            .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .map_err(|e| format!("metadata token request: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("metadata token status {}", resp.status()));
        }
        let value: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        let token = value
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or("metadata response missing access_token")?
            .to_string();
        let expires_in = value
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(300);
        *cache = TokenCache {
            token: Some(token.clone()),
            expires_at: now + expires_in,
        };
        Ok(token)
    }

    /// Download the object's raw bytes, or `Ok(None)` when it does not exist.
    async fn get(&self) -> Result<Option<Vec<u8>>, String> {
        let token = self.token().await?;
        let mut url = reqwest::Url::parse(&format!(
            "https://storage.googleapis.com/storage/v1/b/{}/o/{}",
            self.bucket,
            self.object
        )) .map_err(|e| e.to_string())?;
        url.query_pairs_mut().append_pair("alt", "media");
        let resp = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| format!("GCS get: {e}"))?;
        match resp.status() {
            StatusCode::NOT_FOUND => Ok(None),
            s if s.is_success() => Ok(Some(
                resp.bytes().await.map_err(|e| e.to_string())?.to_vec(),
            )),
            s => Err(format!(
                "GCS get status {s}: {}",
                resp.text().await.unwrap_or_default()
            )),
        }
    }

    /// Upload `bytes`, overwriting the object. When `create_only` is set the
    /// write is rejected if the object already exists, returning `Ok(false)`.
    async fn put(&self, bytes: &[u8], create_only: bool) -> Result<bool, String> {
        let token = self.token().await?;
        let mut url = reqwest::Url::parse(&format!(
            "https://storage.googleapis.com/upload/storage/v1/b/{}/o",
            self.bucket
        ))
        .map_err(|e| e.to_string())?;
        url.query_pairs_mut()
            .append_pair("uploadType", "media")
            .append_pair("name", &self.object);
        if create_only {
            url.query_pairs_mut().append_pair("ifGenerationMatch", "0");
        }
        let resp = self
            .client
            .post(url)
            .bearer_auth(token)
            .header(header::CONTENT_TYPE, "application/json")
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|e| format!("GCS put: {e}"))?;
        match resp.status() {
            StatusCode::PRECONDITION_FAILED => Ok(false),
            s if s.is_success() => Ok(true),
            s => Err(format!(
                "GCS put status {s}: {}",
                resp.text().await.unwrap_or_default()
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Passwords and seeding
// ---------------------------------------------------------------------------

/// Hash `password` with argon2id and a fresh random salt, returning its encoded
/// PHC string.
fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| e.to_string())
}

/// Check `password` against a stored argon2id `hash`, returning `false` when the
/// hash cannot be parsed.
fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Generate a random `len`-character password from an unambiguous alphabet.
fn random_password(len: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut bytes = vec![0u8; len];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| CHARS[*b as usize % CHARS.len()] as char)
        .collect()
}

/// Seed the store with `emails` on first run, each with a generated temporary
/// password, and print those passwords once so the operator can hand them out.
pub async fn ensure_seeded(storage: &Storage, emails: &[String]) {
    match storage.load().await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(e) => {
            eprintln!("could not read user store: {e}");
            return;
        }
    }

    let mut users = Vec::new();
    let mut temporary = Vec::new();
    for email in emails {
        let password = random_password(12);
        match hash_password(&password) {
            Ok(password_hash) => {
                users.push(User {
                    email: email.clone(),
                    password_hash,
                    must_change_password: true,
                });
                temporary.push((email.clone(), password));
            }
            Err(e) => eprintln!("hashing temporary password for {email} failed: {e}"),
        }
    }
    if users.is_empty() {
        return;
    }

    match storage.save_if_absent(&UserStore { users }).await {
        Ok(true) => {
            eprintln!("==========================================================================");
            eprintln!(
                " Seeded {} account(s). Temporary passwords (change on first login):",
                temporary.len()
            );
            for (email, password) in &temporary {
                eprintln!("   {email}  ->  {password}");
            }
            eprintln!("==========================================================================");
        }
        Ok(false) => eprintln!("user store already seeded by another instance"),
        Err(e) => eprintln!("failed to seed user store: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Cookies
// ---------------------------------------------------------------------------

/// Extract the value of the `session` cookie from the request headers.
fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        if let Some((name, value)) = pair.split_once('=')
            && name.trim() == "session"
        {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Build the `Set-Cookie` value that stores `token`, adding `Secure` when
/// `secure` is set.
fn session_set_cookie(token: &str, secure: bool) -> String {
    format!(
        "session={token}; Path=/; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Build the `Set-Cookie` value that expires the session cookie.
fn session_clear_cookie(secure: bool) -> String {
    format!(
        "session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Append a `Set-Cookie` header to `response`, ignoring an invalid value.
fn attach_cookie(mut response: Response, cookie: String) -> Response {
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

/// Redirect to `path` with its `error` query parameter set to `code`.
fn error_redirect(path: &str, code: &str) -> Response {
    Redirect::to(&format!("{path}?error={code}")).into_response()
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Allow only an active session (password already changed).
pub async fn require_active(
    State(auth): State<Arc<AuthState>>,
    mut req: Request,
    next: Next,
) -> Response {
    match auth.claims_from_headers(req.headers()) {
        Some(claims) if !claims.must_change => {
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        Some(_) => {
            if req.method() == Method::GET {
                Redirect::to("/change-password").into_response()
            } else {
                (
                    StatusCode::FORBIDDEN,
                    Json(json!({ "error": "password change required" })),
                )
                    .into_response()
            }
        }
        None => {
            if req.method() == Method::GET {
                Redirect::to("/login").into_response()
            } else {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "unauthorized" })),
                )
                    .into_response()
            }
        }
    }
}

/// Allow any valid session, including one that must still change its password.
pub async fn require_session(
    State(auth): State<Arc<AuthState>>,
    mut req: Request,
    next: Next,
) -> Response {
    match auth.claims_from_headers(req.headers()) {
        Some(claims) => {
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        None => Redirect::to("/login").into_response(),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Submitted credentials for `POST /login`.
#[derive(Deserialize)]
pub struct LoginForm {
    email: String,
    password: String,
}

/// Submitted fields for `POST /change-password`: the old password plus the new
/// one entered twice for confirmation.
#[derive(Deserialize)]
pub struct ChangePasswordForm {
    current_password: String,
    new_password: String,
    confirm_password: String,
}

/// Serve the login form.
pub async fn login_page() -> Html<&'static str> {
    Html(include_str!("../static/login.html"))
}

/// Serve the change-password form.
pub async fn change_password_page() -> Html<&'static str> {
    Html(include_str!("../static/change_password.html"))
}

/// Handle `POST /login`: verify the credentials and, on success, redirect with
/// a freshly issued session cookie.
pub async fn login(State(auth): State<Arc<AuthState>>, Form(form): Form<LoginForm>) -> Response {
    let email = form.email.trim().to_lowercase();
    let store = match auth.storage.load().await {
        Ok(Some(store)) => store,
        Ok(None) => return error_redirect("/login", "nousers"),
        Err(e) => {
            eprintln!("login: reading user store failed: {e}");
            return error_redirect("/login", "storage");
        }
    };
    let Some(user) = store
        .users
        .iter()
        .find(|u| u.email.eq_ignore_ascii_case(&email))
    else {
        return error_redirect("/login", "credentials");
    };
    if !verify_password(&form.password, &user.password_hash) {
        return error_redirect("/login", "credentials");
    }
    let token = match auth.issue(&user.email, user.must_change_password) {
        Ok(token) => token,
        Err(e) => {
            eprintln!("login: issuing session failed: {e}");
            return error_redirect("/login", "storage");
        }
    };
    let destination = if user.must_change_password {
        "/change-password"
    } else {
        "/"
    };
    attach_cookie(
        Redirect::to(destination).into_response(),
        session_set_cookie(&token, auth.cookie_secure),
    )
}

/// Handle `POST /change-password`: validate the new password, persist its hash,
/// clear the `must_change` flag, and reissue the session cookie.
pub async fn change_password(
    State(auth): State<Arc<AuthState>>,
    Extension(claims): Extension<Claims>,
    Form(form): Form<ChangePasswordForm>,
) -> Response {
    if form.new_password.len() < 8 {
        return error_redirect("/change-password", "short");
    }
    if form.new_password != form.confirm_password {
        return error_redirect("/change-password", "mismatch");
    }
    let mut store = match auth.storage.load().await {
        Ok(Some(store)) => store,
        Ok(None) => return error_redirect("/login", "nousers"),
        Err(e) => {
            eprintln!("change_password: reading user store failed: {e}");
            return error_redirect("/change-password", "storage");
        }
    };
    let Some(index) = store
        .users
        .iter()
        .position(|u| u.email.eq_ignore_ascii_case(&claims.sub))
    else {
        return error_redirect("/login", "credentials");
    };
    if !verify_password(&form.current_password, &store.users[index].password_hash) {
        return error_redirect("/change-password", "current");
    }
    let new_hash = match hash_password(&form.new_password) {
        Ok(hash) => hash,
        Err(e) => {
            eprintln!("change_password: hashing failed: {e}");
            return error_redirect("/change-password", "storage");
        }
    };
    let email = store.users[index].email.clone();
    store.users[index].password_hash = new_hash;
    store.users[index].must_change_password = false;
    if let Err(e) = auth.storage.save(&store).await {
        eprintln!("change_password: saving user store failed: {e}");
        return error_redirect("/change-password", "storage");
    }
    let token = match auth.issue(&email, false) {
        Ok(token) => token,
        Err(e) => {
            eprintln!("change_password: issuing session failed: {e}");
            return error_redirect("/change-password", "storage");
        }
    };
    attach_cookie(
        Redirect::to("/").into_response(),
        session_set_cookie(&token, auth.cookie_secure),
    )
}

/// Handle `POST /logout`: clear the session cookie and send the user to login.
pub async fn logout(State(auth): State<Arc<AuthState>>) -> Response {
    attach_cookie(
        Redirect::to("/login").into_response(),
        session_clear_cookie(auth.cookie_secure),
    )
}
