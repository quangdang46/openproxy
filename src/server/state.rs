use std::path::PathBuf;
use std::sync::Arc;

use std::collections::HashMap;
use tokio::sync::{Notify, RwLock};

use crate::core::a2a::TaskStore;
use crate::core::account_fallback::AccountRegistry;
use crate::core::circuit_breaker::CircuitBreakerRegistry;
use crate::core::executor::ClientPool;
use crate::core::mitm::server::MitmProxyHandle;
use crate::core::tunnel::TunnelManager;
use crate::core::usage::UsageTracker;
use crate::db::Db;
use crate::oauth::pending::PendingFlowStore;
use crate::server::api::oauth::CodexProxyState;
use crate::server::auth::login_limiter::LoginLimiter;
use crate::server::auth::oidc::OidcClient;
use crate::server::console_logs::{shared_console_log_buffer, ConsoleLogBuffer};
use crate::server::usage_live::UsageLiveState;

/// Session info stored server-side
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub api_key_id: String,
    pub created_at: i64,
    pub last_active: i64,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub client_pool: Arc<ClientPool>,
    pub tunnel_manager: Arc<TunnelManager>,
    pub pending_flows: PendingFlowStore,
    pub account_registry: Arc<AccountRegistry>,
    pub console_logs: Arc<ConsoleLogBuffer>,
    pub usage_live: Arc<UsageLiveState>,
    pub sessions: Arc<RwLock<HashMap<String, SessionInfo>>>,
    pub codex_proxy: Arc<CodexProxyState>,

    /// Progressive lockout store for `POST /api/auth/login`. Tracks failed
    /// attempts per client IP and escalates lockout duration on repeat
    /// offenders (see `login_limiter.rs` for the exact schedule).
    pub login_limiter: Arc<LoginLimiter>,

    /// OIDC SSO client. `Some` when `OIDC_ISSUER`, `OIDC_CLIENT_ID`,
    /// and `OIDC_CLIENT_SECRET` are all set at boot; `None` otherwise
    /// (the `/api/auth/oidc/*` routes return 400 in that case).
    pub oidc_client: Option<Arc<OidcClient>>,

    /// Optional reverse-proxy target for the dashboard.
    ///
    /// When `Some`, all dashboard fallback requests are forwarded to this URL
    /// instead of being served from the embedded `web/dist/` assets. Used in
    /// development against the Astro/Vite dev server (`pnpm --dir web run dev`).
    pub dashboard_sidecar_url: Option<String>,

    /// HTTP client used by the dashboard reverse proxy. `Some` iff
    /// `dashboard_sidecar_url` is set — there is no point allocating a
    /// reqwest client in the embedded-only path.
    pub dashboard_client: Option<Arc<reqwest::Client>>,

    /// Optional on-disk override for the dashboard. When set, the embedded
    /// assets are bypassed and files are served from this directory via
    /// `tower-http::services::ServeDir`. Useful for UI iteration without
    /// rebuilding the Rust binary.
    ///
    /// Precedence (first match wins):
    ///   1. `dashboard_sidecar_url` — reverse proxy
    ///   2. `web_dir` — disk
    ///   3. embedded assets (default)
    pub web_dir: Option<PathBuf>,

    /// Live MITM proxy listener handle, when one is running. Drop or
    /// `stop()` to shut it down. `None` while the proxy is not running.
    pub mitm_handle: Arc<tokio::sync::Mutex<Option<MitmProxyHandle>>>,

    /// Triggered on graceful shutdown (SIGTERM, SIGINT, or API call).
    /// Await `.notified()` to block until shutdown is requested.
    pub shutdown_signal: Arc<Notify>,

    /// Circuit breaker registry for provider endpoint resilience.
    /// Tracked per provider+endpoint to fast-fail when upstreams are down.
    pub circuit_breaker: Arc<CircuitBreakerRegistry>,

    /// A2A (Agent-to-Agent) task store. Used by the A2A protocol endpoints
    /// to track task lifecycle across agent interactions.
    pub a2a_task_store: TaskStore,
}

impl AppState {
    /// Construct an AppState with the embedded dashboard as the default
    /// fallback. No reverse-proxy client is allocated until
    /// `with_dashboard_sidecar_url` is called.
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db: db.clone(),
            client_pool: Arc::new(ClientPool::new()),
            tunnel_manager: Arc::new(TunnelManager::new(db.clone())),
            pending_flows: PendingFlowStore::new(),
            account_registry: Arc::new(AccountRegistry::default()),
            console_logs: shared_console_log_buffer(),
            usage_live: Arc::new(UsageLiveState::new()),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            codex_proxy: Arc::new(CodexProxyState::new()),
            login_limiter: Arc::new(LoginLimiter::new(&db.data_dir)),
            oidc_client: None,
            dashboard_sidecar_url: None,
            dashboard_client: None,
            web_dir: None,
            mitm_handle: Arc::new(tokio::sync::Mutex::new(None)),
            shutdown_signal: Arc::new(Notify::new()),
            circuit_breaker: Arc::new(CircuitBreakerRegistry::default()),
            a2a_task_store: TaskStore::new(),
        }
    }

    /// Enable dashboard reverse proxy mode: all dashboard fallback requests
    /// are forwarded to `url`. Allocates a reqwest client lazily.
    ///
    /// Pass `None` to disable. Empty/whitespace strings are also treated as
    /// disabled — that matches the behaviour CLI flag handling expects from
    /// `clap`'s default empty string.
    pub fn with_dashboard_sidecar_url(mut self, url: Option<String>) -> Self {
        let normalized = url.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if normalized.is_some() {
            self.dashboard_client = Some(Arc::new(reqwest::Client::new()));
        } else {
            self.dashboard_client = None;
        }
        self.dashboard_sidecar_url = normalized;
        self
    }

    /// Serve the dashboard from `path` on disk instead of the embedded
    /// assets. Sidecar mode (if set) still wins.
    pub fn with_web_dir(mut self, path: Option<PathBuf>) -> Self {
        self.web_dir = path;
        self
    }

    /// Initialize the OIDC SSO client from `OIDC_ISSUER`, `OIDC_CLIENT_ID`,
    /// `OIDC_CLIENT_SECRET`, and `OIDC_REDIRECT_URI` env vars. Runs
    /// `OidcClient::discover` (an HTTP round-trip to the IdP's
    /// `/.well-known/openid-configuration` document) — that's why this
    /// is async and a separate call from `AppState::new`.
    ///
    /// Missing env vars, network errors, and malformed discovery
    /// documents all leave `oidc_client = None` and log a warning.
    /// The OIDC routes will then return 400.
    pub async fn init_oidc_from_env(mut self) -> Self {
        let issuer = std::env::var("OIDC_ISSUER").ok();
        let client_id = std::env::var("OIDC_CLIENT_ID").ok();
        let client_secret = std::env::var("OIDC_CLIENT_SECRET").ok();
        let redirect_uri = std::env::var("OIDC_REDIRECT_URI")
            .unwrap_or_else(|_| "http://127.0.0.1:4623/api/auth/oidc/callback".to_string());

        self.oidc_client = match (issuer, client_id, client_secret) {
            (Some(issuer), Some(client_id), Some(client_secret))
                if !issuer.is_empty() && !client_id.is_empty() && !client_secret.is_empty() =>
            {
                match OidcClient::discover(&issuer, &client_id, &client_secret, &redirect_uri).await
                {
                    Ok(client) => {
                        tracing::info!(issuer = %client.issuer, "OIDC SSO client initialized");
                        Some(Arc::new(client))
                    }
                    Err(error) => {
                        tracing::error!(?error, "failed to initialize OIDC SSO client");
                        None
                    }
                }
            }
            _ => None,
        };
        self
    }

    /// Inject a pre-built [`OidcClient`]. Used by tests and by callers
    /// that want to bypass the env-var contract.
    pub fn with_oidc_client(mut self, client: Option<Arc<OidcClient>>) -> Self {
        self.oidc_client = client;
        self
    }

    /// Returns a UsageTracker for tracking request/response usage.
    /// The tracker is created fresh each call to ensure it picks up
    /// the latest pricing configuration from the database.
    pub fn usage_tracker(&self) -> UsageTracker {
        UsageTracker::new(self.db.clone())
    }

    /// Trigger graceful shutdown. Notifies all waiters and
    /// signals axum to stop accepting new connections.
    pub fn signal_shutdown(&self) {
        self.shutdown_signal.notify_waiters();
    }
}
