//! DNS bypass for MITM-protected hosts.
//!
//! Ports `open-sse/utils/proxyFetch.js` (decolua/9router) — specifically the
//! `MITM_BYPASS_HOSTS` + `resolveRealIP` logic. For a small allowlist of
//! upstream provider endpoints that ship CLIs known to be intercepted by
//! on-host MITM proxies (Cursor, Codex via `cloudcode-pa.googleapis.com`,
//! GitHub Copilot, AWS CodeWhisperer, Cursor's own API), we resolve A
//! records via Google DNS (UDP/53) instead of the system resolver. The
//! response IP is then used as the connect address while reqwest still does
//! SNI + cert verification against the original hostname, so the bypass only
//! defeats name-based intercept — an on-path attacker that can present a
//! valid public-CA cert for the real hostname is still blocked.
//!
//! Plug-in point: `reqwest::ClientBuilder::dns_resolver`. The
//! [`MitmBypassResolver`] falls through to `tokio::net::lookup_host` for any
//! hostname not in the allowlist, so non-bypassed traffic uses the system
//! resolver exactly as before.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::net::UdpSocket;
use tokio::time::timeout;

/// Hosts whose `/etc/hosts` (or system-resolver) entries we mistrust because
/// known CLI MITM proxies (e.g. cherry-studio, claude-code-MITM, the
/// Cursor/Codex on-host shim) point them at 127.0.0.1 to intercept traffic.
/// Resolution for these names is short-circuited through Google DNS.
///
/// Mirrors the JS const of the same name in
/// `open-sse/utils/proxyFetch.js`.
pub const MITM_BYPASS_HOSTS: &[&str] = &[
    "cloudcode-pa.googleapis.com",
    "daily-cloudcode-pa.googleapis.com",
    "api.individual.githubcopilot.com",
    "q.us-east-1.amazonaws.com",
    "codewhisperer.us-east-1.amazonaws.com",
    "api2.cursor.sh",
];

/// Public DNS servers we query for `MITM_BYPASS_HOSTS`. Plain UDP/53 — we
/// only consume A-record answers and never the response RCODE/AA/AD bits, so
/// DoH/DoT would only add dependencies without changing the threat model
/// (the attacker we care about is local-host name-rewriting, not on-path).
const GOOGLE_DNS_SERVERS: &[&str] = &["8.8.8.8:53", "8.8.4.4:53"];

/// How long a successful Google-DNS lookup is reused. Matches the upstream
/// JS `MEMORY_CONFIG.dnsCacheTtlMs` default.
const DNS_CACHE_TTL: Duration = Duration::from_secs(300);

/// Hard ceiling for a single UDP DNS round-trip — keeps a stuck packet from
/// blocking a request's connect phase.
const DNS_QUERY_TIMEOUT: Duration = Duration::from_secs(3);

/// True iff the hostname matches one of the `MITM_BYPASS_HOSTS` entries.
/// Matching is by suffix (`endsWith`) so subdomains and trailing dots both
/// trigger the bypass, identical to the JS `.includes(host)` check on the
/// `URL.hostname`.
pub fn is_mitm_bypass_host(hostname: &str) -> bool {
    let host = hostname.trim_end_matches('.').to_ascii_lowercase();
    MITM_BYPASS_HOSTS.iter().any(|h| {
        let h = h.to_ascii_lowercase();
        host == h || host.ends_with(&format!(".{h}"))
    })
}

/// reqwest DNS resolver that short-circuits MITM-targeted hostnames through
/// Google DNS and falls through to `tokio::net::lookup_host` for everything
/// else.
#[derive(Default)]
pub struct MitmBypassResolver {
    cache: DashMap<String, CachedAddr>,
}

#[derive(Clone, Copy)]
struct CachedAddr {
    ip: IpAddr,
    expires_at: Instant,
}

impl MitmBypassResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a single A record for `host` via Google DNS, caching the
    /// answer for `DNS_CACHE_TTL`. Cache hits short-circuit the UDP query.
    pub async fn resolve_bypass_host(&self, host: &str) -> io::Result<IpAddr> {
        let key = host.trim_end_matches('.').to_ascii_lowercase();
        if let Some(cached) = self.cache.get(&key) {
            if cached.expires_at > Instant::now() {
                return Ok(cached.ip);
            }
        }
        let ip = query_google_dns(host).await?;
        self.cache.insert(
            key,
            CachedAddr {
                ip,
                expires_at: Instant::now() + DNS_CACHE_TTL,
            },
        );
        Ok(ip)
    }
}

impl Resolve for MitmBypassResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        // DashMap is cheap to clone (Arc inside) so we can move into the future.
        let cache = self.cache.clone();
        Box::pin(async move {
            if is_mitm_bypass_host(&host) {
                let resolver = MitmBypassResolver { cache };
                match resolver.resolve_bypass_host(&host).await {
                    Ok(ip) => {
                        let addr = SocketAddr::new(ip, 0);
                        let iter: Addrs = Box::new(std::iter::once(addr));
                        return Ok(iter);
                    }
                    Err(err) => {
                        tracing::warn!(
                            target: "openproxy::dns",
                            "mitm-bypass DNS for {host} failed, falling back to system resolver: {err}"
                        );
                    }
                }
            }
            system_resolve(&host).await
        })
            as Pin<
                Box<
                    dyn Future<Output = Result<Addrs, Box<dyn std::error::Error + Send + Sync>>>
                        + Send,
                >,
            >
    }
}

async fn system_resolve(host: &str) -> Result<Addrs, Box<dyn std::error::Error + Send + Sync>> {
    let lookup = tokio::net::lookup_host(format!("{host}:0")).await?;
    let addrs: Vec<SocketAddr> = lookup.collect();
    let iter: Addrs = Box::new(addrs.into_iter());
    Ok(iter)
}

async fn query_google_dns(host: &str) -> io::Result<IpAddr> {
    let mut last_err: Option<io::Error> = None;
    for server in GOOGLE_DNS_SERVERS {
        match timeout(DNS_QUERY_TIMEOUT, query_one(server, host)).await {
            Ok(Ok(ip)) => return Ok(ip),
            Ok(Err(err)) => last_err = Some(err),
            Err(_) => {
                last_err = Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("DNS query to {server} timed out"),
                ))
            }
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("no Google DNS server produced an answer")))
}

async fn query_one(server: &str, host: &str) -> io::Result<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(server).await?;
    let query = build_query(host)?;
    socket.send(&query).await?;
    let mut buf = [0u8; 1500];
    let n = socket.recv(&mut buf).await?;
    parse_first_a(&buf[..n])
}

/// Build a minimal DNS query frame for an A record. RFC 1035 §4.1: 12-byte
/// header (txn id 0x1234, RD=1, qdcount=1) followed by the QNAME labels,
/// QTYPE=A (1), QCLASS=IN (1).
fn build_query(host: &str) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(64);
    // Transaction ID — random enough for our purposes; we never multiplex
    // queries on the same socket so collision risk is zero.
    out.extend_from_slice(&[0x12, 0x34]);
    out.extend_from_slice(&[0x01, 0x00]); // flags: RD=1
    out.extend_from_slice(&[0x00, 0x01]); // qdcount=1
    out.extend_from_slice(&[0x00, 0x00]); // ancount
    out.extend_from_slice(&[0x00, 0x00]); // nscount
    out.extend_from_slice(&[0x00, 0x00]); // arcount

    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty DNS label",
            ));
        }
        if label.len() > 63 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DNS label exceeds 63 bytes",
            ));
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0x00); // root label
    out.extend_from_slice(&[0x00, 0x01]); // QTYPE A
    out.extend_from_slice(&[0x00, 0x01]); // QCLASS IN
    Ok(out)
}

/// Parse the first A record (TYPE=1, CLASS=1, RDLENGTH=4) from a DNS
/// response payload. Strict on bounds — every read is checked against the
/// buffer length so a malformed/truncated response can't OOB-read.
fn parse_first_a(buf: &[u8]) -> io::Result<IpAddr> {
    if buf.len() < 12 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "DNS reply too short",
        ));
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    if an == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "DNS reply has no answer",
        ));
    }
    let mut idx = 12usize;
    for _ in 0..qd {
        idx = skip_name(buf, idx)?;
        // QTYPE + QCLASS
        idx = idx
            .checked_add(4)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "qsection overflow"))?;
        if idx > buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated qsection",
            ));
        }
    }
    for _ in 0..an {
        idx = skip_name(buf, idx)?;
        if idx + 10 > buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated rr header",
            ));
        }
        let r#type = u16::from_be_bytes([buf[idx], buf[idx + 1]]);
        let class = u16::from_be_bytes([buf[idx + 2], buf[idx + 3]]);
        let rdlen = u16::from_be_bytes([buf[idx + 8], buf[idx + 9]]) as usize;
        idx += 10;
        if idx + rdlen > buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated rdata",
            ));
        }
        if r#type == 1 && class == 1 && rdlen == 4 {
            return Ok(IpAddr::V4(Ipv4Addr::new(
                buf[idx],
                buf[idx + 1],
                buf[idx + 2],
                buf[idx + 3],
            )));
        }
        idx += rdlen;
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no A record in DNS reply",
    ))
}

/// Advance past a (possibly compressed) RFC 1035 §4.1.4 DNS name.
/// Pointers (high two bits = 11) are followed transparently — but we don't
/// recurse into the pointed-at region because we only care about the index
/// past the current label run.
fn skip_name(buf: &[u8], mut idx: usize) -> io::Result<usize> {
    loop {
        if idx >= buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated DNS name",
            ));
        }
        let len = buf[idx];
        if len == 0 {
            return Ok(idx + 1);
        }
        if len & 0xC0 == 0xC0 {
            // 2-byte pointer
            if idx + 2 > buf.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated name pointer",
                ));
            }
            return Ok(idx + 2);
        }
        idx = idx + 1 + len as usize;
    }
}

/// Convenience: a pre-configured Arc resolver for `reqwest::ClientBuilder`.
pub fn shared_resolver() -> Arc<MitmBypassResolver> {
    Arc::new(MitmBypassResolver::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_host_matches_exact_and_subdomain() {
        for &host in MITM_BYPASS_HOSTS {
            assert!(is_mitm_bypass_host(host), "{host} should match");
            assert!(
                is_mitm_bypass_host(&format!("sub.{host}")),
                "sub.{host} should match"
            );
            assert!(
                is_mitm_bypass_host(&format!("{host}.")),
                "{host}. should match (trailing dot)"
            );
        }
    }

    #[test]
    fn bypass_host_rejects_lookalikes() {
        // suffix-only attacker registrations like "googleapis.com.attacker.tld"
        assert!(!is_mitm_bypass_host(
            "cloudcode-pa.googleapis.com.attacker.tld"
        ));
        assert!(!is_mitm_bypass_host("not-cloudcode-pa.googleapis.com"));
        assert!(!is_mitm_bypass_host("evil.com"));
        assert!(!is_mitm_bypass_host(""));
    }

    #[test]
    fn build_query_encodes_qname_and_qtype() {
        let q = build_query("example.com").unwrap();
        // header is 12 bytes
        assert_eq!(q[2], 0x01); // flags RD=1
        assert_eq!(q[5], 0x01); // qdcount=1
                                // qname: 7 example 3 com 0
        assert_eq!(&q[12..20], b"\x07example");
        assert_eq!(&q[20..24], b"\x03com");
        assert_eq!(q[24], 0x00);
        // QTYPE A, QCLASS IN
        assert_eq!(&q[25..29], &[0x00, 0x01, 0x00, 0x01]);
    }

    #[test]
    fn build_query_rejects_oversize_label() {
        let long = "a".repeat(64);
        assert!(build_query(&format!("{long}.example.com")).is_err());
    }

    #[test]
    fn parse_first_a_extracts_ipv4() {
        // Hand-built reply: header(qd=1,an=1) + question + answer w/ A 8.8.8.8
        let mut buf: Vec<u8> = vec![
            0x12, 0x34, 0x81, 0x80, // id, flags
            0x00, 0x01, 0x00, 0x01, // qd=1, an=1
            0x00, 0x00, 0x00, 0x00, // ns=0, ar=0
        ];
        // question: dns.google
        buf.extend_from_slice(b"\x03dns\x06google\x00");
        buf.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]); // QTYPE A, QCLASS IN
                                                          // answer: name pointer to offset 12, TYPE=A, CLASS=IN, TTL=60, RDLENGTH=4, RDATA=8.8.8.8
        buf.extend_from_slice(&[0xC0, 0x0C]);
        buf.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C]);
        buf.extend_from_slice(&[0x00, 0x04]);
        buf.extend_from_slice(&[8, 8, 8, 8]);

        let ip = parse_first_a(&buf).unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[test]
    fn parse_first_a_rejects_truncated() {
        assert!(parse_first_a(&[]).is_err());
        assert!(parse_first_a(&[0u8; 11]).is_err());
    }

    #[test]
    fn parse_first_a_rejects_no_answer() {
        let buf: [u8; 12] = [0x12, 0x34, 0x81, 0x80, 0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0];
        assert!(parse_first_a(&buf).is_err());
    }
}
