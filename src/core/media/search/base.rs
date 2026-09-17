//! Common types and helpers for search providers.

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::types::ProviderConnection;

/// Request body shared across providers. Maps directly to OmniRoute's
/// `SearchRequestParams`.
#[derive(Debug, Clone)]
pub struct SearchRequest<'a> {
    pub query: String,
    pub search_type: SearchType,
    pub max_results: u32,
    pub token: Option<&'a str>,
    pub country: Option<String>,
    pub language: Option<String>,
    /// `"day" | "week" | "month" | "year" | "any"`.
    pub time_range: Option<String>,
    pub offset: Option<u32>,
    /// Optionally prefixed with `-` to indicate exclusion.
    pub domain_filter: Vec<String>,
    pub content_options: Option<Value>,
    /// Free-form per-provider knobs (`baseUrl`, `cx`, `depth`, …).
    pub provider_options: BTreeMap<String, Value>,
    pub provider_specific_data: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchType {
    Web,
    News,
    /// Tweet-search type for the xquik adapter (registry `searchTypes: ["x"]`).
    /// Behaves like `Web` everywhere except `as_str` round-trips `"x"`.
    X,
}

impl SearchType {
    pub fn as_str(self) -> &'static str {
        match self {
            SearchType::Web => "web",
            SearchType::News => "news",
            SearchType::X => "x",
        }
    }
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("news") => SearchType::News,
            Some("x") => SearchType::X,
            _ => SearchType::Web,
        }
    }
}

/// One unified search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_url: Option<String>,
    pub snippet: String,
    pub position: u32,
    pub score: Option<f64>,
    pub published_at: Option<String>,
    pub favicon_url: Option<String>,
    pub content: Option<Value>,
    pub metadata: Value,
    pub citation: Value,
    pub provider_raw: Option<Value>,
}

/// Response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultSet {
    pub results: Vec<SearchResult>,
    pub total_results: Option<u64>,
}

/// Chat-search outcome: the unified result set plus the LLM answer text and
/// token count that JS `handleChatSearch` returns as `data.answer` /
/// `data.usage` alongside `results` (chatSearch.js:534-549). Dedicated
/// adapters return a bare [`SearchResultSet`]; only the chat fallback path
/// produces this.
#[derive(Debug, Clone)]
pub struct ChatSearchResult {
    pub set: SearchResultSet,
    pub answer_text: String,
    pub model: String,
    pub llm_tokens: u64,
}

impl ChatSearchResult {
    pub fn new(set: SearchResultSet, answer_text: &str, model: &str, llm_tokens: u64) -> Self {
        Self {
            set,
            answer_text: answer_text.to_string(),
            model: model.to_string(),
            llm_tokens,
        }
    }
}

/// 9router `searchViaChat` fallback config: when the dedicated search
/// endpoint fails with a retriable error, fall back to a chat-completions
/// LLM search (port of `open-sse/handlers/search/chatSearch.js`).
#[derive(Debug, Clone, Copy)]
pub struct ChatSearchFallback {
    /// Chat model to use for the fallback LLM search.
    pub model: &'static str,
}

/// Trait implemented by every search provider. Builds the upstream
/// request and normalises the response.
pub trait SearchProvider: Send + Sync {
    fn id(&self) -> &'static str;

    /// 9router `searchViaChat` config. `Some` means the provider can fall
    /// back to a chat-completions LLM search when the dedicated endpoint
    /// fails with a retriable error. Defaults to `None`.
    fn chat_fallback(&self) -> Option<ChatSearchFallback> {
        None
    }

    /// Whether the upstream is no-auth (searxng with public instance, etc.).
    fn no_auth(&self) -> bool {
        false
    }

    /// Build the upstream URL.
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String>;

    /// Build the headers for the upstream call.
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String>;

    /// HTTP method for the upstream call.
    fn method(&self) -> reqwest::Method {
        reqwest::Method::GET
    }

    /// Optional JSON body (POST providers).
    fn build_body(&self, _request: &SearchRequest<'_>) -> Option<Value> {
        None
    }

    /// Normalise the upstream JSON to [`SearchResultSet`].
    fn normalize(&self, body: &Value, request: &SearchRequest<'_>) -> SearchResultSet;

    /// Extra top-level envelope fields to merge into the `/v1/search`
    /// response (e.g. xquik `pagination`). Defaults to none.
    fn extra_envelope(&self, _body: &Value) -> Option<Vec<(String, Value)>> {
        None
    }

    /// Per-provider upstream timeout in ms (9router registry `timeoutMs`).
    /// `None` → the global 15s timeout applies.
    fn timeout_ms(&self) -> Option<u64> {
        None
    }

    /// Per-provider maximum result count (9router registry `maxMaxResults`,
    /// default 100). Applied after the request's max_results is resolved.
    fn max_max_results(&self) -> u32 {
        100
    }
}

/// Split `domain_filter` into `(includes, excludes)` where excludes are
/// prefixed with `-` in the input.
pub fn parse_domain_filter(filter: &[String]) -> (Vec<String>, Vec<String>) {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    for d in filter {
        if let Some(rest) = d.strip_prefix('-') {
            excludes.push(rest.to_string());
        } else {
            includes.push(d.clone());
        }
    }
    (includes, excludes)
}

/// Read a string setting from `provider_options` or `provider_specific_data`.
pub fn get_provider_setting(request: &SearchRequest<'_>, key: &str) -> Option<String> {
    let value = request
        .provider_options
        .get(key)
        .or_else(|| request.provider_specific_data.get(key))?;
    let s = value.as_str()?.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Port of 9router `search/index.js sanitizeQuery` (index.js:17-25):
/// reject control characters, NFKC-normalize, trim, and collapse whitespace.
///
/// The control-char set is NOT the full 0x00-0x1F range — tab (0x09), LF
/// (0x0A), and CR (0x0D) are excluded (they're valid whitespace).
pub fn sanitize_query(raw: &str) -> Result<String, String> {
    let has_control_char = raw
        .bytes()
        .any(|b| matches!(b, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F));
    if has_control_char {
        return Err("Query contains invalid control characters".to_string());
    }
    let nfkc = unicode_normalization::UnicodeNormalization::nfkc(raw);
    let normalized: String = nfkc.collect();
    let clean: String = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        return Err("Query is empty after normalization".to_string());
    }
    Ok(clean)
}

/// Build a SearchRequest from credentials + JSON body. Useful when the
/// caller has the inbound `/v1/search` request body in hand.
pub fn request_from_body<'a>(
    body: &'a Value,
    credentials: Option<&'a ProviderConnection>,
) -> Result<SearchRequest<'a>, String> {
    let raw_query = body
        .get("query")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Missing required field: query".to_string())?;
    let query = sanitize_query(raw_query)?;
    let search_type = SearchType::parse(body.get("search_type").and_then(|v| v.as_str()));
    let max_results = body
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .min(100) as u32;
    let token = credentials
        .and_then(|c| c.api_key.as_deref().or(c.access_token.as_deref()))
        .filter(|s| !s.is_empty());
    let country = body
        .get("country")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let language = body
        .get("language")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let time_range = body
        .get("time_range")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let offset = body
        .get("offset")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let domain_filter = body
        .get("domain_filter")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let content_options = body.get("content_options").cloned();
    let provider_options = body
        .get("provider_options")
        .and_then(|v| v.as_object())
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let provider_specific_data = credentials
        .map(|c| c.provider_specific_data.clone())
        .unwrap_or_default();

    Ok(SearchRequest {
        query,
        search_type,
        max_results,
        token,
        country,
        language,
        time_range,
        offset,
        domain_filter,
        content_options,
        provider_options,
        provider_specific_data,
    })
}

/// Strip the URL scheme + `www.` prefix (used by `display_url`).
pub fn make_display_url(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    let stripped = url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_start_matches("www.");
    let cleaned = stripped.split('?').next().unwrap_or(stripped);
    Some(cleaned.to_string())
}

// ---------------------------------------------------------------------------
// SSRF guard for client-supplied base URLs (ported from 9router
// src/shared/utils/ssrfGuard.js, hardened in v0.5.75 commit b870b5d4 "fix(security):
// close SSRF guard bypasses in ssrfGuard.js (#3714)").
//
// Three layers, mirroring the JS module, each closing a distinct bypass class:
//   1. `assert_public_url`         - synchronous literal-IP/hostname checks (cheap,
//                                     for immediate rejection of obviously-bad input
//                                     at request-build time).
//   2. `assert_public_url_resolved` - adds DNS resolution so a hostname that merely
//                                      *resolves* to a private/loopback/metadata
//                                      address (e.g. a nip.io/sslip.io wildcard-DNS
//                                      domain, or an attacker's own domain pointed at
//                                      127.0.0.1) is also rejected.
//   3. `fetch_public`              - wraps a request with manual redirect handling so
//                                      a validated public URL can't 30x its way to an
//                                      internal target without the redirect target
//                                      being re-validated through layer 2 first.
// ---------------------------------------------------------------------------

/// Blocked hostname suffixes for SSRF protection.
const BLOCKED_SUFFIXES: &[&str] = &[".internal", ".local", ".localhost"];

/// Blocked hostnames for SSRF protection.
const BLOCKED_HOSTNAMES: &[&str] = &["localhost", "ip6-localhost", "ip6-loopback"];

/// IPv4 CIDR blocks blocked for SSRF protection, as `(base_octets, prefix_bits)`.
const BLOCKED_V4_RANGES: &[([u8; 4], u32)] = &[
    ([0, 0, 0, 0], 8),
    ([10, 0, 0, 0], 8),
    ([100, 64, 0, 0], 10), // CGNAT — also used by some cloud metadata proxies
    ([127, 0, 0, 0], 8),
    ([169, 254, 0, 0], 16), // includes 169.254.169.254 cloud metadata
    ([172, 16, 0, 0], 12),
    ([192, 168, 0, 0], 16),
];

fn ipv4_to_u32(octets: [u8; 4]) -> u32 {
    u32::from_be_bytes(octets)
}

/// Numeric range check (mirrors JS `isBlockedIpv4Int`).
fn is_blocked_ipv4_int(ip: u32) -> bool {
    BLOCKED_V4_RANGES.iter().any(|(base, bits)| {
        let mask: u32 = if *bits == 0 {
            0
        } else {
            (0xffff_ffffu32) << (32 - bits)
        };
        (ip & mask) == (ipv4_to_u32(*base) & mask)
    })
}

fn is_blocked_ipv4(host: &str) -> bool {
    match host.parse::<std::net::Ipv4Addr>() {
        Ok(v4) => is_blocked_ipv4_int(u32::from_be_bytes(v4.octets())),
        Err(_) => false,
    }
}

/// Parse any textual IPv6 representation (including an embedded dotted-IPv4
/// tail, `::` compression in any position, and full/partial forms) into 8
/// 16-bit groups. Returns `None` if the string isn't a valid IPv6 literal.
///
/// Reasoning about the numeric value (groups) rather than pattern-matching the
/// source string is what makes this immune to "which textual form did the URL
/// parser pick" bugs: `::ffff:127.0.0.1` and `::ffff:7f00:1` produce identical
/// groups. Mirrors JS `parseIPv6ToGroups`.
fn parse_ipv6_to_groups(raw_host: &str) -> Option<[u16; 8]> {
    let host_lower = raw_host.to_lowercase();
    let mut host: &str = &host_lower;

    // Extract a trailing dotted-IPv4 tail, if any (e.g. "::ffff:127.0.0.1").
    let v4_tail_re_match = {
        // Find the longest trailing run of `[0-9.]` that parses as an IPv4 addr.
        let bytes = host.as_bytes();
        let mut start = bytes.len();
        while start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
            start -= 1;
        }
        let candidate = &host[start..];
        candidate.parse::<std::net::Ipv4Addr>().ok()
    };

    let mut v4_groups: Option<[u16; 2]> = None;
    if let Some(v4) = v4_tail_re_match {
        let v4_int = u32::from_be_bytes(v4.octets());
        v4_groups = Some([((v4_int >> 16) & 0xffff) as u16, (v4_int & 0xffff) as u16]);
        // Trim the dotted tail off the host string.
        let bytes = host.as_bytes();
        let mut start = bytes.len();
        while start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
            start -= 1;
        }
        host = &host[..start];
        if let Some(stripped) = host.strip_suffix("::") {
            // "::" compression marker itself — leave both colons, the removed
            // IPv4 fills the gap it represents.
            host = &host[..stripped.len() + 2];
        } else if let Some(stripped) = host.strip_suffix(':') {
            // was just the "prevgroup:ipv4" separator
            host = stripped;
        }
    }

    let parse_hextets = |s: &str| -> Option<Vec<u16>> {
        if s.is_empty() {
            return Some(Vec::new());
        }
        s.split(':')
            .map(|seg| {
                if seg.is_empty() || seg.len() > 4 || !seg.bytes().all(|b| b.is_ascii_hexdigit()) {
                    None
                } else {
                    u16::from_str_radix(seg, 16).ok()
                }
            })
            .collect()
    };

    let double_colon_parts: Vec<&str> = host.splitn(3, "::").collect();
    let groups: Vec<u16> = if double_colon_parts.len() >= 2 {
        // splitn(3, "::") on a string with more than one "::" yields 3 parts;
        // treat that as invalid (mirrors JS `doubleColonParts.length > 2`).
        if host.matches("::").count() > 1 {
            return None;
        }
        let head = parse_hextets(double_colon_parts[0])?;
        let tail = parse_hextets(double_colon_parts.get(1).copied().unwrap_or(""))?;
        let v4_len = v4_groups.map(|_| 2).unwrap_or(0);
        let missing = 8i32
            .checked_sub(head.len() as i32)?
            .checked_sub(tail.len() as i32)?
            .checked_sub(v4_len)?;
        if missing < 0 {
            return None;
        }
        let mut out = head;
        out.extend(std::iter::repeat_n(0u16, missing as usize));
        out.extend(tail);
        if let Some(v4) = v4_groups {
            out.extend(v4);
        }
        out
    } else {
        let mut all = parse_hextets(host)?;
        if let Some(v4) = v4_groups {
            all.extend(v4);
        }
        all
    };

    if groups.len() == 8 {
        let mut arr = [0u16; 8];
        arr.copy_from_slice(&groups);
        Some(arr)
    } else {
        None
    }
}

/// Mirrors JS `isBlockedIpv6Groups`: loopback, unspecified, link-local,
/// unique-local, IPv4-mapped (`::ffff:0:0/96`), NAT64 (`64:ff9b::/96`), and
/// IPv4-compatible (`::a.b.c.d/96`, deprecated but still parseable) forms —
/// each checked against the same IPv4 blocklist for the embedded address.
fn is_blocked_ipv6_groups(g: [u16; 8]) -> bool {
    let is_zero = |n: usize| g[n] == 0;
    // loopback ::1
    if (0..=6).all(is_zero) && g[7] == 1 {
        return true;
    }
    // unspecified ::
    if g.iter().all(|&x| x == 0) {
        return true;
    }
    // link-local fe80::/10
    if (g[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // unique local fc00::/7
    if (g[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    let low32 = ((g[6] as u32) << 16) | (g[7] as u32);
    // IPv4-mapped ::ffff:0:0/96
    if (0..=4).all(is_zero) && g[5] == 0xffff {
        return is_blocked_ipv4_int(low32);
    }
    // NAT64 well-known prefix 64:ff9b::/96
    if g[0] == 0x0064 && g[1] == 0xff9b && (2..=5).all(is_zero) {
        return is_blocked_ipv4_int(low32);
    }
    // IPv4-compatible ::a.b.c.d/96 (deprecated) — excludes :: and ::1 already
    // matched above.
    if (0..=5).all(is_zero) && low32 != 0 && low32 != 1 {
        return is_blocked_ipv4_int(low32);
    }
    false
}

/// A trailing dot marks an FQDN and is semantically insignificant
/// (`"localhost."` and `"localhost"` are the same host) but was being
/// compared as a literal character, letting it slip past every string-based
/// check. Mirrors JS `normalizeHost`.
fn normalize_host(hostname: &str) -> String {
    hostname.to_lowercase().trim_end_matches('.').to_string()
}

/// Mirrors JS `isBlockedHost`: hostname/suffix table, IPv4 literal, and (for
/// any host containing a `:`) the full numeric-groups IPv6 check.
fn is_blocked_host(host: &str) -> bool {
    if BLOCKED_HOSTNAMES.contains(&host) {
        return true;
    }
    if BLOCKED_SUFFIXES.iter().any(|s| host.ends_with(s)) {
        return true;
    }
    if is_blocked_ipv4(host) {
        return true;
    }
    if host.contains(':') {
        let bracketless = host.trim_start_matches('[').trim_end_matches(']');
        if let Some(groups) = parse_ipv6_to_groups(bracketless) {
            if is_blocked_ipv6_groups(groups) {
                return true;
            }
        }
    }
    false
}

/// Validate that a URL is a public HTTP(S) address suitable for server-side
/// fetching, by literal hostname/IP alone (no DNS resolution — see
/// [`assert_public_url_resolved`] for that). Rejects private IPs, loopback,
/// link-local, cloud metadata, and internal hostnames.
///
/// Returns `Ok(())` if the URL is safe, or `Err(message)` if blocked.
///
/// NOTE: In test mode (`#[cfg(test)]`), this validation is skipped to allow
/// mock server URLs (localhost/127.0.0.1) to be used in tests.
#[cfg(not(test))]
pub fn assert_public_url(raw_url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(raw_url).map_err(|e| format!("Invalid baseUrl: {e}"))?;

    // Only allow http/https protocols.
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("Invalid baseUrl protocol: {other}")),
    }

    let host = normalize_host(
        parsed
            .host_str()
            .ok_or_else(|| "baseUrl has no host".to_string())?,
    );

    if is_blocked_host(&host) {
        return Err("Blocked URL: internal host".to_string());
    }

    Ok(())
}

/// Test-only stub that always allows the URL (for mock server URLs in tests).
#[cfg(test)]
pub fn assert_public_url(_raw_url: &str) -> Result<(), String> {
    Ok(())
}

/// Async: [`assert_public_url`] plus DNS resolution of non-literal hostnames,
/// so a domain that merely *resolves* to a private/loopback/metadata address
/// (wildcard-DNS services like nip.io/sslip.io, or an attacker-controlled
/// domain with an A/AAAA record pointed at 127.0.0.1) is rejected too, not
/// just IPs typed directly into the URL. Mirrors JS `assertPublicUrlResolved`.
///
/// NOTE: In test mode (`#[cfg(test)]`), this validation is skipped to allow
/// mock server URLs (localhost/127.0.0.1) to be used in tests.
#[cfg(not(test))]
pub async fn assert_public_url_resolved(raw_url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(raw_url).map_err(|e| format!("Invalid baseUrl: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("Invalid baseUrl protocol: {other}")),
    }
    let host = normalize_host(
        parsed
            .host_str()
            .ok_or_else(|| "baseUrl has no host".to_string())?,
    );
    if is_blocked_host(&host) {
        return Err("Blocked URL: internal host".to_string());
    }

    // Already a literal IPv4/IPv6 address — `is_blocked_host` above already
    // covered it, no DNS lookup applies.
    let bracketless = host.trim_start_matches('[').trim_end_matches(']');
    if bracketless.parse::<std::net::IpAddr>().is_ok() {
        return Ok(());
    }

    // Resolution failure isn't an SSRF signal by itself — let the subsequent
    // fetch fail with its own (clearer) network error, matching the JS
    // `catch { return; }` behavior.
    let Ok(addrs) = tokio::net::lookup_host((bracketless, 0)).await else {
        return Ok(());
    };
    for addr in addrs {
        let blocked = match addr.ip() {
            std::net::IpAddr::V4(v4) => is_blocked_ipv4_int(u32::from_be_bytes(v4.octets())),
            std::net::IpAddr::V6(v6) => is_blocked_ipv6_groups(v6.segments()),
        };
        if blocked {
            return Err("Blocked URL: hostname resolves to an internal host".to_string());
        }
    }
    Ok(())
}

/// Test-only stub that always allows the URL (for mock server URLs in tests).
#[cfg(test)]
pub async fn assert_public_url_resolved(_raw_url: &str) -> Result<(), String> {
    Ok(())
}

/// `fetch()` with SSRF-safe manual redirect handling: each hop's target is
/// re-validated through [`assert_public_url_resolved`] before being followed,
/// so a validated public URL can't 30x its way to an internal target.
/// Bounded to `max_redirects` hops. Mirrors JS `fetchPublic`.
///
/// Only needed for client-supplied override URLs (`resolve_base_url`); the
/// provider's own configured base URL is admin-controlled and callers should
/// send it through the normal `client.request(...).send()` path instead.
///
/// IMPORTANT: this builds its redirect logic on a `Policy::none()` client
/// (mirroring the JS `{ redirect: "manual" }` option). Passing a client whose
/// redirect policy follows redirects (reqwest's default, 10 hops) would let
/// the underlying transport follow a 302 to an internal target *before* the
/// loop below ever sees the 3xx status, silently defeating the whole guard.
pub async fn fetch_public(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    headers: reqwest::header::HeaderMap,
    body: Option<&serde_json::Value>,
    timeout: std::time::Duration,
) -> Result<reqwest::Response, String> {
    const MAX_REDIRECTS: u32 = 5;
    assert_public_url_resolved(url).await?;
    // Enforce manual redirect handling ourselves (mirrors JS
    // `{ redirect: "manual" }`) — the pooled/call-site client may follow
    // redirects by default, which would bypass the per-hop re-validation.
    let manual_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .unwrap_or_else(|_| client.clone());
    let mut current_url = url.to_string();
    for hop in 0..=MAX_REDIRECTS {
        let mut builder = manual_client
            .request(method.clone(), &current_url)
            .headers(headers.clone())
            .timeout(timeout);
        if let Some(b) = body {
            builder = builder.json(b);
        }
        let res = builder
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = res.status();
        if !(300..400).contains(&status.as_u16()) {
            return Ok(res);
        }
        let Some(location) = res
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
        else {
            return Ok(res);
        };
        if hop >= MAX_REDIRECTS {
            return Err("Blocked URL: too many redirects".to_string());
        }
        let next_url = url::Url::parse(&current_url)
            .and_then(|base| base.join(location))
            .map_err(|e| format!("invalid redirect location: {e}"))?
            .to_string();
        assert_public_url_resolved(&next_url).await?;
        current_url = next_url;
    }
    unreachable!("loop always returns before exceeding MAX_REDIRECTS")
}

/// Resolve the base URL with optional `provider_options.baseUrl` override.
/// Client-supplied overrides are SSRF-hardened: only public http(s) URLs are
/// accepted (private/loopback/metadata addresses rejected). The provider's own
/// configured baseUrl is trusted as-is (admin-controlled).
pub fn resolve_base_url(default: &str, request: &SearchRequest<'_>) -> Result<String, String> {
    if let Some(override_url) = get_provider_setting(request, "baseUrl") {
        // SSRF guard: client-supplied base URLs must be public http(s) only.
        assert_public_url(&override_url)?;
        Ok(override_url.trim_end_matches('/').to_string())
    } else {
        Ok(default.trim_end_matches('/').to_string())
    }
}

/// Build a unified [`SearchResult`].
pub fn make_result(
    provider_id: &str,
    title: Option<&str>,
    url: Option<&str>,
    snippet: Option<&str>,
    score: Option<f64>,
    published_at: Option<&str>,
    favicon_url: Option<&str>,
    full_text: Option<&str>,
    text_format: Option<&str>,
    image_url: Option<&str>,
    author: Option<&str>,
    source_type: Option<&str>,
    index: u32,
    now_iso: &str,
) -> SearchResult {
    let url = url.unwrap_or("").to_string();
    let display = make_display_url(&url);
    let content = full_text.map(|t| {
        serde_json::json!({
            "format": text_format.unwrap_or("text"),
            "text": t,
            "length": t.chars().count(),
        })
    });
    SearchResult {
        title: title.unwrap_or("").to_string(),
        url,
        display_url: display,
        snippet: snippet.unwrap_or("").to_string(),
        position: index + 1,
        score: score.map(|s| s.clamp(0.0, 1.0)),
        published_at: published_at.map(str::to_string),
        favicon_url: favicon_url.map(str::to_string),
        content,
        metadata: serde_json::json!({
            "author": author,
            "language": serde_json::Value::Null,
            "source_type": source_type,
            "image_url": image_url,
        }),
        citation: serde_json::json!({
            "provider": provider_id,
            "retrieved_at": now_iso,
            "rank": index + 1,
        }),
        provider_raw: None,
    }
}

/// `chrono::Utc::now().to_rfc3339()` shortcut.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_domain_filter_splits_on_dash() {
        let (inc, exc) = parse_domain_filter(&[
            "example.com".to_string(),
            "-spam.com".to_string(),
            "good.com".to_string(),
        ]);
        assert_eq!(inc, vec!["example.com", "good.com"]);
        assert_eq!(exc, vec!["spam.com"]);
    }

    #[test]
    fn make_display_url_strips_scheme_and_www() {
        assert_eq!(
            make_display_url("https://www.example.com/path?q=x"),
            Some("example.com/path".into())
        );
    }

    #[test]
    fn request_from_body_validates_query() {
        let body = serde_json::json!({});
        assert!(request_from_body(&body, None).is_err());
        let body = serde_json::json!({"query": "  "});
        assert!(request_from_body(&body, None).is_err());
    }

    #[test]
    fn request_from_body_caps_max_results() {
        let body = serde_json::json!({"query": "x", "max_results": 1000});
        let r = request_from_body(&body, None).unwrap();
        assert_eq!(r.max_results, 100);
    }

    #[test]
    fn search_query_rejects_control_chars() {
        assert!(sanitize_query("hello\x07").is_err());
        assert!(sanitize_query("\x00abc").is_err());
        assert!(sanitize_query("a\x7fb").is_err());
        // The error message mentions control characters.
        assert!(sanitize_query("x\x01")
            .unwrap_err()
            .contains("control characters"));
        // Tab / LF / CR are allowed (valid whitespace).
        assert!(sanitize_query("a\tb").is_ok());
        assert!(sanitize_query("a\nb").is_ok());
    }

    #[test]
    fn search_query_collapses_whitespace() {
        assert_eq!(sanitize_query("a  b").unwrap(), "a b");
        assert_eq!(
            sanitize_query("  leading and   trailing  ").unwrap(),
            "leading and trailing"
        );
        // NFKC: full-width digits normalize to ASCII.
        assert_eq!(sanitize_query("１２３").unwrap(), "123");
        // Empty after normalization errors.
        assert!(sanitize_query("   ")
            .unwrap_err()
            .contains("empty after normalization"));
    }

    #[test]
    fn request_from_body_uses_sanitized_query() {
        let body = serde_json::json!({"query": "a  b\tc"});
        let r = request_from_body(&body, None).unwrap();
        assert_eq!(r.query, "a b c");
        // Control char → Err.
        let body = serde_json::json!({"query": "bad\x07query"});
        assert!(request_from_body(&body, None).is_err());
    }

    // -----------------------------------------------------------------------
    // SSRF guard tests (non-#[cfg(test)] — these test the production impl)
    // -----------------------------------------------------------------------

    // NOTE: assert_public_url is stubbed in test mode, so these tests
    // validate the logic of the production implementation by calling the
    // underlying checks directly. In production, assert_public_url would
    // reject private IPs.

    fn make_request(query: &str) -> SearchRequest<'_> {
        SearchRequest {
            query: query.to_string(),
            search_type: SearchType::Web,
            max_results: 5,
            token: None,
            country: None,
            language: None,
            time_range: None,
            offset: None,
            domain_filter: vec![],
            content_options: None,
            provider_options: BTreeMap::new(),
            provider_specific_data: BTreeMap::new(),
        }
    }

    #[test]
    fn resolve_base_url_uses_default_when_no_override() {
        let req = make_request("test");
        let url = resolve_base_url("https://api.example.com/v1", &req).unwrap();
        assert_eq!(url, "https://api.example.com/v1");
    }

    #[test]
    fn resolve_base_url_trims_trailing_slash() {
        let req = make_request("test");
        let url = resolve_base_url("https://api.example.com/v1/", &req).unwrap();
        assert_eq!(url, "https://api.example.com/v1");
    }

    #[test]
    fn resolve_base_url_uses_override_when_provided() {
        let mut req = make_request("test");
        req.provider_options.insert(
            "baseUrl".to_string(),
            serde_json::json!("https://custom.api.com/search"),
        );
        let url = resolve_base_url("https://api.example.com/v1", &req).unwrap();
        assert_eq!(url, "https://custom.api.com/search");
    }

    // -----------------------------------------------------------------------
    // SSRF hardening (#3714 / 9router b870b5d4): exercised via the internal
    // helpers since `assert_public_url` is stubbed in test mode.
    // -----------------------------------------------------------------------

    #[test]
    fn ssrf_normalize_host_strips_trailing_dots() {
        assert_eq!(normalize_host("LOCALHOST."), "localhost");
        assert_eq!(normalize_host("Example.COM..."), "example.com");
        assert_eq!(normalize_host("8.8.8.8"), "8.8.8.8");
    }

    #[test]
    fn ssrf_blocks_internal_hostnames_and_suffixes() {
        assert!(is_blocked_host("localhost"));
        // Trailing-dot FQDN bypass: "localhost." must block the same as
        // "localhost" (see `normalize_host` in the non-test impl).
        assert!(is_blocked_host(&normalize_host("localhost.")));
        assert!(is_blocked_host(&normalize_host("LOCALHOST.")));
        assert!(is_blocked_host("foo.internal"));
        assert!(is_blocked_host("foo.local"));
        assert!(is_blocked_host("foo.localhost"));
        assert!(!is_blocked_host("api.openai.com"));
        assert!(!is_blocked_host("8.8.8.8"));
    }

    #[test]
    fn ssrf_blocks_private_ipv4_ranges() {
        for host in [
            "10.0.0.1",
            "100.64.0.1", // CGNAT (new in #3714)
            "127.0.0.1",
            "169.254.169.254", // cloud metadata
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "0.0.0.0",
        ] {
            assert!(is_blocked_host(host), "{host} should be blocked");
        }
        for host in ["8.8.8.8", "1.1.1.1", "172.32.0.1", "93.184.216.34"] {
            assert!(!is_blocked_host(host), "{host} should be allowed");
        }
    }

    #[test]
    fn ssrf_blocks_ipv6_mapped_forms_regardless_of_representation() {
        // `::ffff:127.0.0.1` and `::ffff:7f00:1` are the same address —
        // the numeric-groups parse must reject both.
        assert!(is_blocked_host("[::ffff:127.0.0.1]"));
        assert!(is_blocked_host("[::ffff:7f00:1]"));
        assert!(is_blocked_host("[0000::ffff:127.0.0.1]"));
        // Mapped metadata address, dotted and hex forms.
        assert!(is_blocked_host("[::ffff:169.254.169.254]"));
        assert!(is_blocked_host("[::ffff:a9fe:a9fe]"));
        // Loopback / unspecified / link-local / ULA / NAT64 / compat forms.
        for host in [
            "[::1]",
            "[::]",
            "[::127.0.0.1]",
            "[fe80::1]",
            "[fc00::1]",
            "[fd12:3456::1]",
            "[64:ff9b::127.0.0.1]",
        ] {
            assert!(is_blocked_host(host), "{host} should be blocked");
        }
        // Public IPv6 stays allowed.
        assert!(!is_blocked_host("[2001:4860:4860::8888]"));
    }

    #[test]
    fn ssrf_parse_ipv6_to_groups_equivalence() {
        // Same address, two textual forms → identical groups.
        let a = parse_ipv6_to_groups("::ffff:127.0.0.1");
        let b = parse_ipv6_to_groups("::ffff:7f00:1");
        assert!(a.is_some() && b.is_some());
        assert_eq!(a.unwrap(), b.unwrap());
        // Compression in head/tail/only.
        assert!(parse_ipv6_to_groups("fe80::1").is_some());
        assert!(parse_ipv6_to_groups("2001:db8::1").is_some());
        assert!(parse_ipv6_to_groups("::").is_some());
        // Invalid forms rejected.
        assert!(parse_ipv6_to_groups(":::").is_none());
        assert!(parse_ipv6_to_groups("gggg::1").is_none());
    }

    #[test]
    fn ssrf_ipv4_int_range_check_matches_blocklist() {
        assert!(is_blocked_ipv4_int(ipv4_to_u32([127, 0, 0, 1])));
        assert!(is_blocked_ipv4_int(ipv4_to_u32([169, 254, 169, 254])));
        assert!(is_blocked_ipv4_int(ipv4_to_u32([100, 64, 0, 1])));
        assert!(!is_blocked_ipv4_int(ipv4_to_u32([8, 8, 8, 8])));
    }

    #[tokio::test]
    async fn ssrf_resolved_stub_always_allows_in_test_mode() {
        // The test-mode stub must not block normal traffic (DNS is not
        // exercised in unit tests — see bead openproxy-fp8l acceptance
        // criteria about DNS-behavior tests requiring a stubbed resolver).
        assert!(assert_public_url_resolved("http://127.0.0.1/")
            .await
            .is_ok());
    }
}
