use serde_json::Value;

use crate::types::{AppDb, ProviderConnection, ProxyPool, Settings};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyTarget {
    pub url: String,
    pub no_proxy: String,
    pub strict_proxy: bool,
    pub pool_id: Option<String>,
}

pub fn resolve_proxy_target(
    db: &AppDb,
    connection: &ProviderConnection,
    settings: &Settings,
) -> Option<ProxyTarget> {
    let provider_data = &connection.provider_specific_data;
    let direct_enabled = bool_field(provider_data.get("connectionProxyEnabled")).unwrap_or(false);
    let direct_url = string_field(provider_data.get("connectionProxyUrl"));
    let direct_no_proxy = string_field(provider_data.get("connectionNoProxy")).unwrap_or_default();
    let direct_pool_id = connection_proxy_pool_id(provider_data);
    let direct_strict = bool_field(provider_data.get("strictProxy")).unwrap_or(false);

    if direct_enabled {
        if let Some(pool_id) = direct_pool_id.clone() {
            if let Some(pool) = db
                .proxy_pools
                .iter()
                .find(|pool| pool.id == pool_id && pool.is_active.unwrap_or(true))
            {
                return Some(build_pool_target(pool));
            }
        }

        if let Some(url) = direct_url {
            return Some(ProxyTarget {
                url: normalize_proxy_url(&url),
                no_proxy: direct_no_proxy,
                strict_proxy: direct_strict,
                pool_id: direct_pool_id,
            });
        }
    }

    if settings.outbound_proxy_enabled && !settings.outbound_proxy_url.trim().is_empty() {
        return Some(ProxyTarget {
            url: normalize_proxy_url(&settings.outbound_proxy_url),
            no_proxy: settings.outbound_no_proxy.clone(),
            strict_proxy: false,
            pool_id: None,
        });
    }

    None
}

fn build_pool_target(pool: &ProxyPool) -> ProxyTarget {
    ProxyTarget {
        url: normalize_proxy_url_for_kind(&pool.proxy_url, Some(pool.r#type.as_str())),
        no_proxy: pool.no_proxy.clone(),
        strict_proxy: pool.strict_proxy.unwrap_or(false),
        pool_id: Some(pool.id.clone()),
    }
}

pub fn normalize_proxy_url(url: &str) -> String {
    normalize_proxy_url_for_kind(url, None)
}

fn normalize_proxy_url_for_kind(url: &str, kind: Option<&str>) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("socks5://")
        || trimmed.starts_with("socks5h://")
    {
        trimmed.to_string()
    } else {
        format!("{}://{trimmed}", default_scheme(kind))
    }
}

fn string_field(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn bool_field(value: Option<&Value>) -> Option<bool> {
    value.and_then(Value::as_bool)
}

fn connection_proxy_pool_id(
    provider_data: &std::collections::BTreeMap<String, Value>,
) -> Option<String> {
    string_field(provider_data.get("connectionProxyPoolId"))
        .or_else(|| string_field(provider_data.get("proxyPoolId")))
}

fn default_scheme(kind: Option<&str>) -> &'static str {
    match kind.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("https") => "https",
        Some("socks5") => "socks5",
        Some("socks5h") => "socks5h",
        _ => "http",
    }
}
