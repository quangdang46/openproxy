use std::path::PathBuf;
use std::sync::Arc;

use std::collections::HashMap;
use tokio::sync::RwLock;

use crate::core::account_fallback::AccountRegistry;
use crate::core::executor::ClientPool;
use crate::core::tunnel::TunnelManager;
use crate::core::usage::UsageTracker;
use crate::db::Db;
use crate::oauth::pending::PendingFlowStore;
use crate::server::api::oauth::CodexProxyState;
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
            dashboard_sidecar_url: None,
            dashboard_client: None,
            web_dir: None,
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

    /// Returns a UsageTracker for tracking request/response usage.
    /// The tracker is created fresh each call to ensure it picks up
    /// the latest pricing configuration from the database.
    pub fn usage_tracker(&self) -> UsageTracker {
        UsageTracker::new(self.db.clone())
    }
}
