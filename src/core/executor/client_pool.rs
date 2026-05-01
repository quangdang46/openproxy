use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use crate::core::proxy::ProxyTarget;

pub const CLIENT_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
pub const CLIENT_POOL_MAX_IDLE_PER_HOST: usize = 8;
pub const CLIENT_POOL_TCP_KEEPALIVE: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct ClientPool {
    clients: DashMap<String, Arc<reqwest::Client>>,
}

impl ClientPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Result<Arc<reqwest::Client>, reqwest::Error> {
        self.get_or_insert_with(provider_key, proxy, || build_client(proxy))
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
        let entry = self.clients.entry(key).or_try_insert_with(build)?;
        Ok(entry.clone())
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}

fn build_client(proxy: Option<&ProxyTarget>) -> Result<Arc<reqwest::Client>, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .pool_idle_timeout(CLIENT_POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(CLIENT_POOL_MAX_IDLE_PER_HOST)
        .tcp_keepalive(CLIENT_POOL_TCP_KEEPALIVE);

    if let Some(proxy) = proxy {
        if !proxy.url.is_empty() {
            let proxy = reqwest::Proxy::all(&proxy.url)?
                .no_proxy(reqwest::NoProxy::from_string(&proxy.no_proxy));
            builder = builder.proxy(proxy);
        }
    }

    Ok(Arc::new(builder.build()?))
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
