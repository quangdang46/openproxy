use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use crate::core::proxy::ProxyTarget;

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
        let key = client_key(provider_key, proxy);

        if let Some(client) = self.clients.get(&key) {
            return Ok(client.clone());
        }

        let mut builder = reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(8)
            .tcp_keepalive(Duration::from_secs(60));

        if let Some(proxy) = proxy {
            if !proxy.url.is_empty() {
                let proxy = reqwest::Proxy::all(&proxy.url)?
                    .no_proxy(reqwest::NoProxy::from_string(&proxy.no_proxy));
                builder = builder.proxy(proxy);
            }
        }

        let client = Arc::new(builder.build()?);
        let entry = self.clients.entry(key).or_insert_with(|| client.clone());
        Ok(entry.clone())
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }
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
