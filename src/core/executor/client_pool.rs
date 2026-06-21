use std::io;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use dashmap::DashMap;
use http_body_util::Full;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::{connect::HttpConnector, Client as HyperClient};
use hyper_util::rt::{TokioExecutor, TokioTimer};

use crate::core::dns::MitmBypassResolver;
use crate::core::proxy::ProxyTarget;
use crate::core::tls::ensure_rustls_provider;

pub const CLIENT_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
pub const CLIENT_POOL_MAX_IDLE_PER_HOST: usize = 8;
pub const CLIENT_POOL_TCP_KEEPALIVE: Duration = Duration::from_secs(60);
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(180);

/// Timeout configuration for HTTP clients built by [`build_reqwest_client`]
/// and [`build_hyper_client`].
#[derive(Clone, Copy, Debug)]
pub struct ClientTimeout {
    /// Timeout for establishing a TCP/TLS connection.
    pub connect: Duration,
    /// Timeout for the full request/response lifecycle.
    pub stream: Duration,
}

impl Default for ClientTimeout {
    fn default() -> Self {
        Self {
            connect: DEFAULT_CONNECT_TIMEOUT,
            stream: DEFAULT_STREAM_TIMEOUT,
        }
    }
}

pub struct ClientPool {
    reqwest_clients: DashMap<String, Arc<reqwest::Client>>,
    hyper_clients: DashMap<String, Arc<DirectHyperClient>>,
    timeout: ClientTimeout,
}

impl Default for ClientPool {
    fn default() -> Self {
        Self {
            reqwest_clients: DashMap::new(),
            hyper_clients: DashMap::new(),
            timeout: ClientTimeout::default(),
        }
    }
}

pub type DirectHyperClient = HyperClient<HttpsConnector<HttpConnector>, Full<Bytes>>;

impl ClientPool {
    pub fn new() -> Self {
        ensure_rustls_provider();
        Self::default()
    }

    /// Construct a pool with a custom timeout configuration.
    pub fn with_timeout(timeout: ClientTimeout) -> Self {
        ensure_rustls_provider();
        Self {
            timeout,
            ..Default::default()
        }
    }

    pub fn get(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Result<Arc<reqwest::Client>, reqwest::Error> {
        let timeout = self.timeout;
        self.get_or_insert_with(provider_key, proxy, || build_reqwest_client(proxy, timeout))
    }

    pub fn get_hyper_direct(
        &self,
        provider_key: &str,
    ) -> Result<Arc<DirectHyperClient>, io::Error> {
        let timeout = self.timeout;
        let entry = self
            .hyper_clients
            .entry(provider_key.to_string())
            .or_try_insert_with(|| build_hyper_client(timeout))?;
        Ok(entry.clone())
    }

    pub fn get_or_insert_with<F>(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
        build: F,
    ) -> Result<Arc<reqwest::Client>, reqwest::Error>
    where
        F: FnOnce() -> Result<Arc<reqwest::Client>, reqwest::Error>,
    {
        let key = client_key(provider_key, proxy);
        // Initialize the client while holding the per-key entry so same-key races
        // cannot build duplicate pools and then discard the extras.
        let entry = self.reqwest_clients.entry(key).or_try_insert_with(build)?;
        Ok(entry.clone())
    }

    pub fn len(&self) -> usize {
        self.reqwest_clients.len()
    }

    pub fn hyper_len(&self) -> usize {
        self.hyper_clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reqwest_clients.is_empty() && self.hyper_clients.is_empty()
    }
}

fn build_reqwest_client(
    proxy: Option<&ProxyTarget>,
    timeout: ClientTimeout,
) -> Result<Arc<reqwest::Client>, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .pool_idle_timeout(CLIENT_POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(CLIENT_POOL_MAX_IDLE_PER_HOST)
        .tcp_keepalive(CLIENT_POOL_TCP_KEEPALIVE)
        .connect_timeout(timeout.connect)
        .timeout(timeout.stream)
        // MITM-bypass: route MITM_BYPASS_HOSTS through Google DNS so a
        // hostile /etc/hosts entry can't redirect Codex/Cursor/Copilot/AWS
        // CodeWhisperer endpoints to a local interceptor. Other hostnames
        // fall through to the system resolver unchanged.
        .dns_resolver(Arc::new(MitmBypassResolver::new()));

    if let Some(proxy) = proxy {
        if !proxy.url.is_empty() {
            let proxy = reqwest::Proxy::all(&proxy.url)?
                .no_proxy(reqwest::NoProxy::from_string(&proxy.no_proxy));
            builder = builder.proxy(proxy);
        }
    }

    Ok(Arc::new(builder.build()?))
}

fn build_hyper_client(timeout: ClientTimeout) -> Result<Arc<DirectHyperClient>, io::Error> {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_keepalive(Some(CLIENT_POOL_TCP_KEEPALIVE));
    http.set_nodelay(true);
    http.set_connect_timeout(Some(timeout.connect));

    let https = HttpsConnectorBuilder::new()
        .with_native_roots()
        .map_err(io::Error::other)?
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(http);

    let mut builder = HyperClient::builder(TokioExecutor::new());
    builder.pool_idle_timeout(CLIENT_POOL_IDLE_TIMEOUT);
    builder.pool_max_idle_per_host(CLIENT_POOL_MAX_IDLE_PER_HOST);
    builder.pool_timer(TokioTimer::new());

    Ok(Arc::new(builder.build(https)))
}

fn client_key(provider_key: &str, proxy: Option<&ProxyTarget>) -> String {
    match proxy {
        Some(proxy) if !proxy.url.is_empty() => format!(
            "{provider_key}|{}|{}|{}|{}",
            proxy.url,
            proxy.no_proxy,
            proxy.strict_proxy,
            proxy.pool_id.as_deref().unwrap_or_default()
        ),
        _ => provider_key.to_string(),
    }
}
