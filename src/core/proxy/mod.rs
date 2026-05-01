use std::collections::BTreeMap;

use serde_json::Value;

use crate::types::{AppDb, ProviderConnection, ProxyPool, Settings};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyTarget {
    pub url: String,
    pub no_proxy: String,
    pub strict_proxy: bool,
    pub pool_id: Option<String>,
    pub label: Option<String>,
    pub rtt_ms: Option<u64>,
}

const MIN_SUCCESS_RATE: f64 = 0.5;
const MAX_RTT_MS: u64 = 5000;

pub fn resolve_proxy_target(
    db: &AppDb,
    connection: &ProviderConnection,
    settings: &Settings,
) -> Option<ProxyTarget> {
    if let Some(url) = connection.proxy_url.as_ref().filter(|u| !u.trim().is_empty()) {
        if let Some(label) = connection.proxy_label.as_ref() {
            if let Some(pool) = find_pool_by_label(db, label) {
                if is_healthy(pool) {
                    let mut target = build_target(pool);
                    target.label = connection.proxy_label.clone();
                    return Some(target);
                }
            }
        }
        return Some(ProxyTarget {
            url: normalize(url),
            no_proxy: String::new(),
            strict_proxy: false,
            pool_id: None,
            label: connection.proxy_label.clone(),
            rtt_ms: None,
        });
    }

    if connection.use_connection_proxy.unwrap_or(false) {
        let data = &connection.provider_specific_data;
        let enabled = data.get("connectionProxyEnabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let url = str_field(data.get("connectionProxyUrl"));
        let no_proxy = str_field(data.get("connectionNoProxy")).unwrap_or_default();
        let pool_id = data.get("connectionProxyPoolId").or_else(|| data.get("proxyPoolId")).and_then(|v| str_field(Some(v)));
        let strict = data.get("strictProxy").and_then(|v| v.as_bool()).unwrap_or(false);

        if enabled {
            if let Some(pid) = pool_id.clone() {
                if let Some(pool) = db.proxy_pools.iter().find(|p| p.id == pid && p.is_active.unwrap_or(true)) {
                    if is_healthy(pool) {
                        return Some(build_target(pool));
                    }
                }
            }
            if let Some(u) = url {
                return Some(ProxyTarget {
                    url: normalize(&u),
                    no_proxy,
                    strict_proxy: strict,
                    pool_id,
                    label: None,
                    rtt_ms: None,
                });
            }
        }
    }

    if settings.outbound_proxy_enabled && !settings.outbound_proxy_url.trim().is_empty() {
        return Some(ProxyTarget {
            url: normalize(&settings.outbound_proxy_url),
            no_proxy: settings.outbound_no_proxy.clone(),
            strict_proxy: false,
            pool_id: None,
            label: None,
            rtt_ms: None,
        });
    }

    None
}

pub fn is_healthy(pool: &ProxyPool) -> bool {
    if !pool.is_active.unwrap_or(true) {
        return false;
    }
    if let Some(rate) = pool.success_rate {
        if rate < MIN_SUCCESS_RATE {
            return false;
        }
    }
    if let Some(rtt) = pool.rtt_ms {
        if rtt > MAX_RTT_MS {
            return false;
        }
    }
    true
}

fn find_pool_by_label<'a>(db: &'a AppDb, label: &str) -> Option<&'a ProxyPool> {
    db.proxy_pools.iter().find(|p| p.name == label || p.id == label)
}

pub fn update_health(pool: &mut ProxyPool, success: bool, rtt: Option<u64>) {
    pool.total_requests = Some(pool.total_requests.unwrap_or(0).saturating_add(1));
    if !success {
        pool.failed_requests = Some(pool.failed_requests.unwrap_or(0).saturating_add(1));
    }
    let total = pool.total_requests.unwrap();
    let failed = pool.failed_requests.unwrap();
    if total > 0 {
        pool.success_rate = Some((total - failed) as f64 / total as f64);
    }
    if let Some(r) = rtt {
        pool.rtt_ms = Some(if let Some(e) = pool.rtt_ms {
            (e as f64 * 0.7 + r as f64 * 0.3) as u64
        } else {
            r
        });
    }
}

pub fn best_proxy(proxies: &[&ProxyPool]) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut score = f64::MIN;
    for (i, p) in proxies.iter().enumerate() {
        if !is_healthy(p) {
            continue;
        }
        let s = p.success_rate.unwrap_or(1.0) * 1000.0 - p.rtt_ms.unwrap_or(0) as f64;
        if s > score {
            score = s;
            best = Some(i);
        }
    }
    best
}

fn build_target(pool: &ProxyPool) -> ProxyTarget {
    ProxyTarget {
        url: normalize_url(&pool.proxy_url, Some(&pool.r#type)),
        no_proxy: pool.no_proxy.clone(),
        strict_proxy: pool.strict_proxy.unwrap_or(false),
        pool_id: Some(pool.id.clone()),
        label: Some(pool.name.clone()),
        rtt_ms: pool.rtt_ms,
    }
}

pub fn normalize(url: &str) -> String {
    normalize_url(url, None)
}

fn normalize_url(url: &str, kind: Option<&str>) -> String {
    let t = url.trim();
    if t.is_empty() {
        return String::new();
    }
    if t.starts_with("http://") || t.starts_with("https://") || t.starts_with("socks5") {
        t.to_string()
    } else {
        format!("{}://{t}", match kind.map(|k| k.trim().to_ascii_lowercase()).as_deref() {
            Some("https") => "https",
            Some("socks5") | Some("socks5h") => "socks5",
            _ => "http",
        })
    }
}

fn str_field(value: Option<&Value>) -> Option<String> {
    value.and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(id: &str) -> ProxyPool {
        ProxyPool {
            id: id.into(),
            name: id.into(),
            proxy_url: format!("http://proxy-{id}"),
            no_proxy: String::new(),
            r#type: "http".into(),
            is_active: Some(true),
            strict_proxy: None,
            test_status: None,
            last_tested_at: None,
            last_error: None,
            success_rate: None,
            rtt_ms: None,
            total_requests: None,
            failed_requests: None,
            created_at: None,
            updated_at: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn healthy_defaults() {
        assert!(is_healthy(&pool("p1")));
    }

    #[test]
    fn low_success_rate_unhealthy() {
        let mut p = pool("p1");
        p.success_rate = Some(0.3);
        assert!(!is_healthy(&p));
    }

    #[test]
    fn high_rtt_unhealthy() {
        let mut p = pool("p1");
        p.rtt_ms = Some(6000);
        assert!(!is_healthy(&p));
    }

    #[test]
    fn inactive_unhealthy() {
        let mut p = pool("p1");
        p.is_active = Some(false);
        assert!(!is_healthy(&p));
    }

    #[test]
    fn health_update_success() {
        let mut p = pool("p1");
        p.total_requests = Some(10);
        p.failed_requests = Some(2);
        p.success_rate = Some(0.8);
        p.rtt_ms = Some(100);
        update_health(&mut p, true, Some(80));
        assert_eq!(p.total_requests, Some(11));
        assert_eq!(p.failed_requests, Some(2));
        let s = p.success_rate.unwrap();
        assert!((s - 0.818).abs() < 0.01);
        assert_eq!(p.rtt_ms, Some(94));
    }

    #[test]
    fn health_update_failure() {
        let mut p = pool("p1");
        p.total_requests = Some(10);
        p.failed_requests = Some(2);
        update_health(&mut p, false, Some(200));
        assert_eq!(p.total_requests, Some(11));
        assert_eq!(p.failed_requests, Some(3));
    }

    #[test]
    fn select_best() {
        let pools: Vec<ProxyPool> = vec![pool("fast"), pool("med"), pool("slow")];
        let refs: Vec<&ProxyPool> = pools.iter().collect();
        assert_eq!(best_proxy(&refs), Some(0));
    }

    #[test]
    fn select_skips_unhealthy() {
        let mut bad = pool("bad");
        bad.success_rate = Some(0.3);
        let pools = vec![bad, pool("good")];
        let refs: Vec<&ProxyPool> = pools.iter().collect();
        assert_eq!(best_proxy(&refs), Some(1));
    }

    #[test]
    fn select_none_all_unhealthy() {
        let mut a = pool("a");
        a.success_rate = Some(0.3);
        let mut b = pool("b");
        b.rtt_ms = Some(6000);
        let refs: Vec<&ProxyPool> = vec![&a, &b];
        assert_eq!(best_proxy(&refs), None);
    }

    #[test]
    fn account_proxy() {
        let db = AppDb::default();
        let conn = ProviderConnection {
            proxy_url: Some("http://acc-proxy:8080".into()),
            proxy_label: Some("acc-pool".into()),
            ..Default::default()
        };
        let result = resolve_proxy_target(&db, &conn, &Settings::default());
        assert!(result.is_some());
        let t = result.unwrap();
        assert_eq!(t.url, "http://acc-proxy:8080");
        assert_eq!(t.label, Some("acc-pool".into()));
    }

    #[test]
    fn no_proxy() {
        let result = resolve_proxy_target(&AppDb::default(), &ProviderConnection::default(), &Settings::default());
        assert!(result.is_none());
    }

    #[test]
    fn global_fallback() {
        let mut s = Settings::default();
        s.outbound_proxy_enabled = true;
        s.outbound_proxy_url = "http://global:3128".into();
        s.outbound_no_proxy = "localhost".into();
        let result = resolve_proxy_target(&AppDb::default(), &ProviderConnection::default(), &s);
        assert!(result.is_some());
        let t = result.unwrap();
        assert_eq!(t.url, "http://global:3128");
        assert_eq!(t.no_proxy, "localhost");
    }
}
