//! Xiaomi MiMo account-service session: the five-step SSO handshake and the
//! weekly-quota read that depends on it.
//!
//! Port of `open-sse/shared/mimoAccount.js` from 9router.
//!
//! The weekly quota lives on the account domain and is authorised by a
//! *session cookie*, not by the `sk-` API key. Acquiring that cookie mirrors
//! MiMo Desktop: a `passToken` is exchanged through the passportapi SSO,
//! authorised for the `mimopc` service, and finally stamped by the mimo-server
//! `/api/sts` callback into a `serviceToken` cookie.
//!
//! Two properties of the port are load-bearing and are asserted by tests here:
//!
//! * **The jar is threaded through the whole chain.** Cookies are minted
//!   mid-handshake — the reference absorbs `Set-Cookie` after steps 3, 4 and 5
//!   — and a later step must send what an earlier one minted. A draft that
//!   absorbed only at the end compiles, looks complete, and fails at the real
//!   account service with no local symptom.
//! * **Headers are read before the body.** Steps 2 and 4 return their session
//!   material on `Set-Cookie` *headers* while the body carries the JSON. A
//!   body-first read loses the headers.
//!
//! The pass jar is a parameter rather than something read from disk inside, so
//! the handshake is exercisable against a mock. Reading MiMo Desktop's locked
//! Chromium cookie DB is the caller's job, and the only caller of that is
//! [`resolve_pass_jar`].

use crate::server::api::oauth::{
    absorb_set_cookie, cookie_header, mimo_api_ua, signature_client_sign, MIMO_API_BASE,
};
use crate::types::ProviderConnection;
use parking_lot::Mutex as LockMutex;
use reqwest::header::{ACCEPT, COOKIE, LOCATION, SET_COOKIE, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::Client;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// Account SSO host the passportapi and mimopc logins live on
/// (`mimoAccount.js:25`).
pub const MIMO_ACCOUNT_HOST: &str = "account.xiaomi.com";

/// User-Agent the passportapi SSO endpoints expect (`mimoAccount.js:27`).
pub const MIMO_SSO_UA: &str = "MiClaw/1.0";

/// How long a minted session cookie is reused before the handshake re-runs
/// (`COOKIE_TTL_MS`, `mimoAccount.js:29`).
pub const MIMO_COOKIE_TTL: Duration = Duration::from_secs(30 * 60);

/// Budget for the weekly-quota read (`AbortSignal.timeout(10000)`).
const MIMO_USAGE_TIMEOUT: Duration = Duration::from_secs(10);

/// Cookies the finished handshake is allowed to carry upstream. The jar
/// accumulates far more than this over the chain; only the service-relevant
/// names go on the wire (`mimoAccount.js:172`).
const MIMO_OUTGOING_COOKIES: [&str; 4] = ["serviceToken", "mimopc_ph", "mimopc_slh", "userId"];

/// Why a session could not be produced, mirroring the reference's `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MimoSessionFailure {
    /// No `passToken` available — not configured and none readable from
    /// MiMo Desktop.
    NoPassToken,
    /// The handshake ran and failed, or could not run.
    SsoFailed,
}

impl MimoSessionFailure {
    /// The wire string the reference reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoPassToken => "no-pass-token",
            Self::SsoFailed => "sso-failed",
        }
    }
}

/// Outcome of a session-cookie request: either a cookie, or a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MimoServiceCookie {
    pub cookie: Option<String>,
    pub failure: Option<MimoSessionFailure>,
}

impl MimoServiceCookie {
    fn ok(cookie: String) -> Self {
        Self {
            cookie: Some(cookie),
            failure: None,
        }
    }

    fn failed(failure: MimoSessionFailure) -> Self {
        Self {
            cookie: None,
            failure: Some(failure),
        }
    }
}

/// Hosts the handshake talks to.
///
/// Split out from the handshake itself so a test can point the whole chain at
/// one local server. Production always uses [`MimoEndpoints::default`].
#[derive(Clone, Debug)]
pub struct MimoEndpoints {
    /// Base for the mimo-server account API (`MIMO_API_BASE`).
    pub api_base: String,
    /// Base for the passportapi/mimopc SSO endpoints (`https://{ACCOUNT_HOST}`).
    pub account_base: String,
}

impl Default for MimoEndpoints {
    fn default() -> Self {
        Self {
            api_base: MIMO_API_BASE.to_string(),
            account_base: format!("https://{MIMO_ACCOUNT_HOST}"),
        }
    }
}

/// A client that surfaces `3xx` instead of following them.
///
/// Steps 1, 3 and 5 exist to *observe* a redirect — the `Location` header is
/// the only place the next URL appears. A client that follows redirects
/// transparently turns the whole chain into a sequence of `200`s and the
/// handshake fails closed with [`MimoSessionFailure::SsoFailed`].
pub fn mimo_sso_client() -> Result<Client, reqwest::Error> {
    Client::builder().redirect(Policy::none()).build()
}

/// Everything a handshake step's response carries that we care about.
///
/// Captured in one pass because `Response::text()` consumes the response, and
/// a body-first read silently discards the `Set-Cookie` headers that steps 2
/// and 4 depend on.
struct CapturedResponse {
    status: reqwest::StatusCode,
    location: Option<String>,
    set_cookie: Vec<String>,
    body: String,
}

impl CapturedResponse {
    async fn capture(response: reqwest::Response) -> Result<Self, reqwest::Error> {
        let status = response.status();
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let set_cookie = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(str::to_string)
            .collect();
        let body = response.text().await?;
        Ok(Self {
            status,
            location,
            set_cookie,
            body,
        })
    }

    /// The `&&&START&&&` prefix the SSO JSON endpoints prepend, stripped the way
    /// the reference's `.replace(/^&&&START&&&/, "")` does.
    fn sso_json(&self) -> Option<Value> {
        serde_json::from_str(self.body.strip_prefix("&&&START&&&").unwrap_or(&self.body)).ok()
    }
}

/// Read one query parameter out of a URL, decoding it.
///
/// Mirrors `new URL(...).searchParams.get(...)`, including form-decoding `+`
/// as a space.
fn query_param(url: &str, name: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    parsed
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// Run the five-step handshake and return the outgoing cookie header.
///
/// Returns `Ok(None)` at every point the reference's `return null` can fire.
/// `Err` is reserved for transport failures, which the caller degrades to
/// [`MimoSessionFailure::SsoFailed`] exactly as the reference's `catch` does.
///
/// `client` must not follow redirects — see [`mimo_sso_client`].
pub async fn acquire_service_cookie(
    client: &Client,
    endpoints: &MimoEndpoints,
    pass_jar: &HashMap<String, String>,
) -> Result<Option<String>, reqwest::Error> {
    // The chain mutates its own copy: the caller's passToken must survive a
    // failed handshake untouched.
    let mut jar = pass_jar.clone();

    // ── 1. Unauthenticated API call -> 302 carrying the sts callback ───────
    let step1 = CapturedResponse::capture(
        client
            .get(format!("{}/api/user/xiaomi/me", endpoints.api_base))
            .header(USER_AGENT, mimo_api_ua())
            .header(COOKIE, cookie_header(&jar))
            .send()
            .await?,
    )
    .await?;
    let Some(location) = step1.location.as_deref() else {
        return Ok(None);
    };
    let Some(sts_callback) = query_param(location, "callback") else {
        return Ok(None);
    };

    // ── 2. passportapi SSO phase 1 -> nonce + ssecurity ────────────────────
    let sso1 = CapturedResponse::capture(
        client
            .get(format!(
                "{}/pass/serviceLogin?sid=passportapi&_json=true",
                endpoints.account_base
            ))
            .header(COOKIE, cookie_header(&jar))
            .header(USER_AGENT, MIMO_SSO_UA)
            .header(ACCEPT, "application/json")
            .send()
            .await?,
    )
    .await?;
    let Some(j1) = sso1.sso_json() else {
        return Ok(None);
    };
    let j1_location = j1
        .get("location")
        .and_then(Value::as_str)
        .map(str::to_string);
    let nonce = j1
        .get("nonce")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| j1_location.as_deref().and_then(|l| query_param(l, "nonce")));
    let (Some(nonce), Some(j1_location)) = (nonce, j1_location) else {
        return Ok(None);
    };
    let ssecurity = j1.get("ssecurity").and_then(Value::as_str);

    // ── 3. passportapi SSO phase 2 -> account-level serviceToken ───────────
    // The location already carries a query string, hence the bare `&`.
    let step3 = CapturedResponse::capture(
        client
            .get(format!(
                "{j1_location}&clientSign={}",
                signature_client_sign(&nonce, ssecurity)
            ))
            .header(COOKIE, cookie_header(&jar))
            .header(USER_AGENT, MIMO_SSO_UA)
            .send()
            .await?,
    )
    .await?;
    absorb_set_cookie(&mut jar, &step3.set_cookie);

    // ── 4. mimopc SSO -> sts callback carrying a ticket ───────────────────
    let sso3 = CapturedResponse::capture(
        client
            .get(format!(
                "{}/pass/serviceLogin?sid=mimopc&callback={}&_json=true",
                endpoints.account_base,
                encode_uri_component(&sts_callback)
            ))
            .header(COOKIE, cookie_header(&jar))
            .header(USER_AGENT, MIMO_SSO_UA)
            .header(ACCEPT, "application/json")
            .send()
            .await?,
    )
    .await?;
    absorb_set_cookie(&mut jar, &sso3.set_cookie);
    let sts_location = sso3
        .sso_json()
        .and_then(|j| {
            j.get("location")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|loc| loc.contains("/api/sts"));
    let Some(sts_location) = sts_location else {
        return Ok(None);
    };

    // ── 5. sts callback -> Set-Cookie: serviceToken (mimopc scope) ─────────
    let step5 = CapturedResponse::capture(
        client
            .get(&sts_location)
            .header(USER_AGENT, mimo_api_ua())
            .header(COOKIE, cookie_header(&jar))
            .send()
            .await?,
    )
    .await?;
    absorb_set_cookie(&mut jar, &step5.set_cookie);

    // The handshake succeeded only if step 5 actually minted a serviceToken;
    // without that check a chain that stopped short would return a header
    // holding nothing useful.
    if jar
        .get("serviceToken")
        .map(String::as_str)
        .unwrap_or_default()
        .is_empty()
    {
        return Ok(None);
    }

    let outgoing: HashMap<String, String> = MIMO_OUTGOING_COOKIES
        .iter()
        .filter_map(|name| {
            jar.get(*name)
                .filter(|value| !value.is_empty())
                .map(|value| ((*name).to_string(), value.clone()))
        })
        .collect();
    Ok(Some(cookie_header(&outgoing)))
}

/// `encodeURIComponent`, used for the step-4 `callback` parameter.
fn encode_uri_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(*byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// One cached session and when it was minted.
struct CachedSession {
    cookie: String,
    at: Instant,
}

/// A `watch` receiver standing in for the reference's `_inflight` promise map.
///
/// The leader performs the handshake and publishes the result; concurrent
/// callers subscribe to the same channel and read that result rather than each
/// running the full five-step chain.
type InflightEntry = watch::Receiver<Option<Option<String>>>;

/// Per-account session cache, keyed by a hash of the passToken so several
/// Xiaomi accounts or connections rotate without clobbering each other.
///
/// Separate from the static because a test needs an isolated one — a shared
/// cache would let one test's handshake satisfy another's.
pub struct MimoCookieCache {
    sessions: LockMutex<HashMap<String, CachedSession>>,
    inflight: tokio::sync::Mutex<HashMap<String, InflightEntry>>,
}

impl Default for MimoCookieCache {
    fn default() -> Self {
        Self::new()
    }
}

impl MimoCookieCache {
    pub fn new() -> Self {
        Self {
            sessions: LockMutex::new(HashMap::new()),
            inflight: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// A live session for `key`, or `None` once it ages past the TTL.
    fn cached(&self, key: &str) -> Option<String> {
        let sessions = self.sessions.lock();
        let entry = sessions.get(key)?;
        (Instant::now().duration_since(entry.at) < MIMO_COOKIE_TTL).then(|| entry.cookie.clone())
    }

    fn store(&self, key: &str, cookie: &str) {
        self.sessions.lock().insert(
            key.to_string(),
            CachedSession {
                cookie: cookie.to_string(),
                at: Instant::now(),
            },
        );
    }

    /// Drop every cached session so the next call re-runs the handshake — the
    /// reference's `invalidateMimoAccountCookieCache`, used after a 401.
    pub fn invalidate(&self) {
        self.sessions.lock().clear();
    }
}

/// The process-wide cache used by the API routes.
static MIMO_COOKIE_CACHE: std::sync::LazyLock<MimoCookieCache> =
    std::sync::LazyLock::new(MimoCookieCache::new);

/// The process-wide session cache.
pub fn global_cache() -> &'static MimoCookieCache {
    &MIMO_COOKIE_CACHE
}

/// Cache key: a hash of the passToken, so the token itself is never a map key.
fn session_key(pass_token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pass_token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Resolve a session cookie, serving from cache and de-duplicating concurrent
/// handshakes for the same account.
///
/// De-duplication matters because the reference is explicit that "a burst of
/// requests must not each run the full 5-step SSO chain" — a burst would
/// otherwise send five requests per step to the account service.
pub async fn get_service_cookie(
    cache: &MimoCookieCache,
    client: &Client,
    endpoints: &MimoEndpoints,
    pass_jar: &HashMap<String, String>,
) -> MimoServiceCookie {
    let Some(pass_token) = pass_jar.get("passToken").filter(|t| !t.is_empty()) else {
        return MimoServiceCookie::failed(MimoSessionFailure::NoPassToken);
    };
    let key = session_key(pass_token);

    // What this caller will do, decided under a single lock so the cache check
    // and the in-flight claim cannot be separated.
    //
    // The reference gets this for free: its cache lookup, its in-flight lookup
    // and its claim are all synchronous, so no leader can finish in between.
    // Splitting them across an await here would open exactly that window — a
    // caller would miss the cache, then miss the in-flight entry because the
    // leader had just retired, and start a second full handshake. The leader
    // side closes the same window by storing its result and retiring its claim
    // under one lock (below), so "no claim" never coexists with "no cache"
    // once a handshake has completed.
    enum Claim {
        Cached(String),
        Join(watch::Receiver<Option<Option<String>>>),
        Lead(watch::Sender<Option<Option<String>>>),
    }

    let claim = {
        let mut inflight = cache.inflight.lock().await;
        if let Some(cookie) = cache.cached(&key) {
            Claim::Cached(cookie)
        } else if let Some(existing) = inflight.get(&key) {
            Claim::Join(existing.clone())
        } else {
            let (publisher, subscriber) = watch::channel(None);
            inflight.insert(key.clone(), subscriber);
            Claim::Lead(publisher)
        }
    };

    let mut subscriber = match claim {
        Claim::Cached(cookie) => return MimoServiceCookie::ok(cookie),
        Claim::Join(subscriber) => subscriber,
        // The leader keeps the sender: it is the only thing that can publish
        // the result its followers are waiting on. Dropping it here would
        // close the channel and leave every follower reading "nothing yet".
        Claim::Lead(publisher) => {
            // Transport and parse failures degrade to "no session" rather
            // than propagating: the reference catches everything here for the
            // same reason — a quota panel must not take a request down.
            let outcome = acquire_service_cookie(client, endpoints, pass_jar)
                .await
                .unwrap_or(None);

            // Publish to the followers first, then retire the claim under one
            // lock together with the cache write, so a caller arriving from
            // here on finds a cache hit rather than an empty claim.
            let _ = publisher.send(Some(outcome.clone()));
            {
                let mut inflight = cache.inflight.lock().await;
                if let Some(cookie) = outcome.clone() {
                    cache.store(&key, &cookie);
                }
                inflight.remove(&key);
            }
            drop(publisher);

            return match outcome {
                Some(cookie) => MimoServiceCookie::ok(cookie),
                None => MimoServiceCookie::failed(MimoSessionFailure::SsoFailed),
            };
        }
    };

    // Follower: the leader publishes before dropping its sender, so either the
    // value is already visible or `changed()` resolves. If the leader died
    // without publishing, `changed()` errors and the borrow yields the initial
    // value, which is the same "no session" outcome.
    let already_published = subscriber.borrow().clone();
    let published = match already_published {
        Some(value) => Some(value),
        None => {
            let _ = subscriber.changed().await;
            subscriber.borrow().clone()
        }
    };

    match published.flatten() {
        Some(cookie) => MimoServiceCookie::ok(cookie),
        None => MimoServiceCookie::failed(MimoSessionFailure::SsoFailed),
    }
}

/// Build the pass jar the handshake starts from.
///
/// A `mimoPassToken` on the connection wins over Desktop's cookie store — the
/// same precedence as the reference, and what makes multi-account rotation
/// possible for a connection that has no Desktop profile behind it.
pub fn resolve_pass_jar(
    connection: Option<&ProviderConnection>,
) -> Option<HashMap<String, String>> {
    let data = connection.map(|c| &c.provider_specific_data);

    let configured = data
        .and_then(|d| d.get("mimoPassToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty());

    if let Some(pass_token) = configured {
        let mut jar = HashMap::from([("passToken".to_string(), pass_token.to_string())]);
        for (key, name) in [("mimoUserId", "userId"), ("mimoCUserId", "cUserId")] {
            if let Some(value) = data
                .and_then(|d| d.get(key))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                jar.insert(name.to_string(), value.to_string());
            }
        }
        return Some(jar);
    }

    let (pass_token, user_id, c_user_id) =
        crate::server::api::oauth::read_mimo_desktop_pass_token()?;
    let mut jar = HashMap::from([("passToken".to_string(), pass_token)]);
    if let Some(user_id) = user_id {
        jar.insert("userId".to_string(), user_id);
    }
    if let Some(c_user_id) = c_user_id {
        jar.insert("cUserId".to_string(), c_user_id);
    }
    Some(jar)
}

/// A weekly-quota reading, or the reason there isn't one.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct MimoUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    #[serde(rename = "resetDate", skip_serializing_if = "Option::is_none")]
    pub reset_date: Option<String>,
    #[serde(rename = "resetAt", skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Read the weekly quota for an existing session cookie.
///
/// Deliberate divergence from the reference: it reports the raw fetch error
/// message, which under `reqwest` embeds the request URL. The strings here are
/// stable and URL-free so a failed probe is safe to surface in the dashboard.
pub async fn get_mimo_account_usage(
    client: &Client,
    endpoints: &MimoEndpoints,
    cookie: &str,
) -> MimoUsage {
    let failed = |error: String| MimoUsage {
        error: Some(error),
        ..Default::default()
    };

    let response = client
        .get(format!("{}/api/user/usage", endpoints.api_base))
        .header(USER_AGENT, mimo_api_ua())
        .header(COOKIE, cookie)
        .header(ACCEPT, "application/json")
        .timeout(MIMO_USAGE_TIMEOUT)
        .send()
        .await;

    let response = match response {
        Ok(response) => response,
        Err(err) if err.is_timeout() => return failed("timeout".to_string()),
        Err(_) => return failed("request-failed".to_string()),
    };

    if !response.status().is_success() {
        return failed(format!("http-{}", response.status().as_u16()));
    }

    let Ok(payload) = response.json::<Value>().await else {
        return failed("bad-response".to_string());
    };
    let Some(data) = payload.get("data") else {
        return failed("bad-response".to_string());
    };
    // The envelope carries a business code alongside the HTTP status; a 200
    // with a non-zero code is a failure, not an empty quota.
    if payload.get("code").and_then(Value::as_i64) != Some(0) {
        return failed("bad-response".to_string());
    }

    MimoUsage {
        percent: data.get("percent").and_then(Value::as_f64),
        reset_date: data
            .get("resetDate")
            .and_then(Value::as_str)
            .map(str::to_string),
        reset_at: data.get("resetAt").and_then(Value::as_i64),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// One request the mock received, kept so a test can assert on what was
    /// actually sent rather than only on what came back.
    #[derive(Clone, Debug)]
    struct Hit {
        path: String,
        cookie: String,
    }

    /// A canned response.
    struct Reply {
        status: u16,
        location: Option<String>,
        set_cookie: Vec<String>,
        body: String,
    }

    impl Reply {
        /// A bare response with no body, cookies or redirect.
        fn plain(status: u16) -> Self {
            Self {
                status,
                location: None,
                set_cookie: vec![],
                body: String::new(),
            }
        }

        /// A `3xx` whose `Location` is `location`.
        fn redirect(location: &str) -> Self {
            Self {
                location: Some(location.to_string()),
                ..Self::plain(302)
            }
        }

        /// A `Set-Cookie` on the headers and nothing else — how the reference's
        /// steps 3 and 5 mint their session cookies.
        fn cookie(name: &str, value: &str) -> Self {
            Self {
                set_cookie: vec![format!("{name}={value}; Path=/; HttpOnly")],
                ..Self::plain(200)
            }
        }

        /// An SSO JSON reply, carrying the `&&&START&&&` prefix the real
        /// service prepends.
        fn sso(body: &str) -> Self {
            Self {
                body: format!("&&&START&&&{body}"),
                ..Self::plain(200)
            }
        }
    }

    /// Serve `build(base)`'s replies in order, recording every request.
    ///
    /// `build` receives the mock's own base URL so a test can point the
    /// handshake's cross-step URLs (the phase-2 location, the sts callback) at
    /// the same server.
    ///
    /// Deliberately raw HTTP on a loopback socket: the handshake exists to
    /// *observe* `3xx` responses, and a test client that followed redirects
    /// would hide the exact behaviour under test.
    async fn serve<F>(build: F) -> (String, Arc<StdMutex<Vec<Hit>>>)
    where
        F: FnOnce(&str) -> Vec<Reply>,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let base = format!("http://{}", listener.local_addr().expect("mock addr"));
        let replies = build(&base);

        let hits: Arc<StdMutex<Vec<Hit>>> = Arc::new(StdMutex::new(Vec::new()));
        let recorded = Arc::clone(&hits);
        tokio::spawn(async move {
            for reply in replies {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 8192];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();

                let path = request
                    .lines()
                    .next()
                    .and_then(|first| first.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                let cookie = request
                    .lines()
                    .find(|line| line.to_ascii_lowercase().starts_with("cookie:"))
                    .map(|line| line[7..].trim().to_string())
                    .unwrap_or_default();
                recorded
                    .lock()
                    .expect("hits lock")
                    .push(Hit { path, cookie });

                let mut response = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status);
                if let Some(location) = &reply.location {
                    response.push_str(&format!("Location: {location}\r\n"));
                }
                for cookie in &reply.set_cookie {
                    response.push_str(&format!("Set-Cookie: {cookie}\r\n"));
                }
                response.push_str(&format!("Content-Length: {}\r\n\r\n", reply.body.len()));
                response.push_str(&reply.body);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        (base, hits)
    }

    /// Both handshake bases pointed at one mock server.
    fn endpoints_at(base: &str) -> MimoEndpoints {
        MimoEndpoints {
            api_base: base.to_string(),
            account_base: base.to_string(),
        }
    }

    /// The five replies a successful handshake gets, in order, with
    /// `serviceToken` minted at step 3 as the reference does.
    fn happy_path(base: &str, service_token: &str) -> Vec<Reply> {
        {
            let sts = format!("{base}/api/sts?ticket=t-1");
            vec![
                // 1 -> 302 carrying the sts callback.
                Reply::redirect(&format!(
                    "{base}/sso?callback={}",
                    encode_uri_component(&sts)
                )),
                // 2 -> nonce + ssecurity + the phase-2 location.
                Reply::sso(&format!(
                    r#"{{"nonce":"n-1","ssecurity":"s-1","location":"{base}/pass/second?nonce=n-1"}}"#
                )),
                // 3 -> mints the account-level serviceToken.
                Reply::cookie("serviceToken", service_token),
                // 4 -> the sts location, plus a cookie minted mid-chain.
                Reply {
                    set_cookie: vec!["mimopc_ph=ph-1; Path=/".to_string()],
                    ..Reply::sso(&format!(r#"{{"location":"{sts}"}}"#))
                },
                // 5 -> stamps the mimopc-scoped serviceToken.
                Reply::cookie("serviceToken", service_token),
            ]
        }
    }

    fn pass_jar() -> HashMap<String, String> {
        HashMap::from([
            ("passToken".to_string(), "pt-1".to_string()),
            ("userId".to_string(), "u-1".to_string()),
        ])
    }

    fn hits_of(hits: &Arc<StdMutex<Vec<Hit>>>) -> Vec<Hit> {
        hits.lock().expect("hits lock").clone()
    }

    /// THE PORT, END TO END. Every step returns what the reference expects and
    /// the chain still produces a cookie, carrying only the four
    /// service-relevant names the reference filters down to.
    #[tokio::test]
    async fn a_complete_handshake_yields_the_outgoing_cookie() {
        let (base, hits) = serve(|base| happy_path(base, "svc-final")).await;
        let cookie = acquire_service_cookie(
            &mimo_sso_client().unwrap(),
            &endpoints_at(&base),
            &pass_jar(),
        )
        .await
        .expect("mock is reachable")
        .expect("a complete handshake yields a cookie");

        assert_eq!(
            cookie, "mimopc_ph=ph-1; serviceToken=svc-final; userId=u-1",
            "only the four service-relevant cookies go upstream"
        );
        assert_eq!(
            hits_of(&hits).len(),
            5,
            "the chain is exactly five requests"
        );
    }

    /// The `callback` the step-1 redirect carries must survive the round trip
    /// through step 4 intact — decoded on the way out of the `Location`, then
    /// re-encoded into the mimopc request. Getting this wrong points the
    /// service authorisation at the wrong callback and it fails remotely.
    #[tokio::test]
    async fn the_sts_callback_round_trips_through_step_four() {
        let (base, hits) = serve(|base| happy_path(base, "svc-cb")).await;
        acquire_service_cookie(
            &mimo_sso_client().unwrap(),
            &endpoints_at(&base),
            &pass_jar(),
        )
        .await
        .expect("mock is reachable")
        .expect("handshake completes");

        let hits = hits_of(&hits);
        let expected = encode_uri_component(&format!("{base}/api/sts?ticket=t-1"));
        assert!(
            hits[3]
                .path
                .contains(&format!("callback={expected}&_json=true")),
            "step 4 must re-encode the decoded callback, got {:?}",
            hits[3].path
        );
    }

    /// THE BUG THIS MODULE EXISTS TO PREVENT.
    ///
    /// The reference mints `serviceToken` at step 3 and re-mints it at step 5.
    /// A handshake that only absorbed cookies at the end would send steps 4 and
    /// 5 without what step 3 produced, and would fail at the real account
    /// service while looking complete here — the failure mode that made the
    /// first attempt at this port worse than absent.
    #[tokio::test]
    async fn cookies_minted_mid_chain_are_sent_by_later_steps() {
        // Step 5 mints nothing, so the only serviceToken in play is step 3's.
        let (base, hits) = serve(|base| {
            let mut replies = happy_path(base, "svc-3");
            replies[4] = Reply::plain(200);
            replies
        })
        .await;
        let cookie = acquire_service_cookie(
            &mimo_sso_client().unwrap(),
            &endpoints_at(&base),
            &pass_jar(),
        )
        .await
        .expect("mock is reachable")
        .expect("a serviceToken minted at step 3 must survive to the end");

        assert!(cookie.contains("serviceToken=svc-3"), "got {cookie}");

        let hits = hits_of(&hits);
        // Steps 4 and 5 run after step 3, so both must already carry the token.
        for (index, label) in [(3, "step 4"), (4, "step 5")] {
            assert!(
                hits[index].cookie.contains("serviceToken=svc-3"),
                "{label} must send the step-3 serviceToken, sent {:?}",
                hits[index].cookie
            );
        }
        // And the passToken is still there — the jar is added to, not replaced.
        assert!(
            hits[4].cookie.contains("passToken=pt-1"),
            "the passToken must survive alongside the minted cookies"
        );
    }

    /// Steps 2 and 4 carry session material on `Set-Cookie` *headers* while
    /// the body carries the JSON. Reading the body first would discard those
    /// headers, so this pins that they survive a body read.
    #[tokio::test]
    async fn set_cookie_on_a_json_response_is_not_lost_to_the_body_read() {
        let (base, _) = serve(|base| {
            let mut replies = happy_path(base, "svc-h");
            replies[3] = Reply {
                set_cookie: vec!["mimopc_ph=from-header; Path=/".to_string()],
                ..Reply::sso(&format!(r#"{{"location":"{base}/api/sts?ticket=t"}}"#))
            };
            replies
        })
        .await;
        let cookie = acquire_service_cookie(
            &mimo_sso_client().unwrap(),
            &endpoints_at(&base),
            &pass_jar(),
        )
        .await
        .expect("mock is reachable")
        .expect("handshake completes");

        assert!(
            cookie.contains("mimopc_ph=from-header"),
            "a Set-Cookie header on a JSON reply must be absorbed, got {cookie}"
        );
    }

    /// The `&&&START&&&` prefix is stripped before parsing. Without this the
    /// body would not be JSON and step 2 would abort the chain.
    #[tokio::test]
    async fn the_sso_json_prefix_is_stripped() {
        let (base, _) = serve(|base| {
            let mut replies = happy_path(base, "svc-p");
            replies[1].body = format!(
                "&&&START&&&{{\"nonce\":\"n-1\",\"ssecurity\":\"s\",\"location\":\"{base}/pass/second?nonce=n-1\"}}"
            );
            replies
        })
        .await;
        assert!(
            acquire_service_cookie(
                &mimo_sso_client().unwrap(),
                &endpoints_at(&base),
                &pass_jar()
            )
            .await
            .expect("mock is reachable")
            .is_some(),
            "a prefixed JSON body must parse"
        );
    }

    /// The nonce can arrive directly or inside the phase-2 location; the
    /// reference accepts either, so both must work.
    #[tokio::test]
    async fn the_nonce_is_read_from_either_position_the_reference_accepts() {
        // Directly on the object.
        let (base, hits) = serve(|base| happy_path(base, "svc-direct")).await;
        acquire_service_cookie(
            &mimo_sso_client().unwrap(),
            &endpoints_at(&base),
            &pass_jar(),
        )
        .await
        .expect("mock is reachable")
        .expect("a direct nonce completes the chain");

        // Only inside the location's query.
        let (base, hits2) = serve(|base| {
            let mut replies = happy_path(base, "svc-query");
            replies[1] = Reply::sso(&format!(
                r#"{{"ssecurity":"s-1","location":"{base}/pass/second?nonce=n-9"}}"#
            ));
            replies
        })
        .await;
        acquire_service_cookie(
            &mimo_sso_client().unwrap(),
            &endpoints_at(&base),
            &pass_jar(),
        )
        .await
        .expect("mock is reachable")
        .expect("a location-borne nonce completes the chain");

        for (hits, expected) in [(&hits, "n-1"), (&hits2, "n-9")] {
            let step3 = &hits_of(hits)[2].path;
            let sign = signature_client_sign(expected, Some("s-1"));
            assert!(
                step3.ends_with(&format!("&clientSign={sign}")),
                "step 3 must sign the nonce it found, got {step3}"
            );
        }
    }

    /// The clientSign on step 3 is the one value that has to match the
    /// reference byte for byte; a mismatch is rejected by the account service
    /// with no local symptom.
    #[tokio::test]
    async fn step_three_carries_the_reference_client_sign() {
        let (base, hits) = serve(|base| happy_path(base, "svc-s")).await;
        acquire_service_cookie(
            &mimo_sso_client().unwrap(),
            &endpoints_at(&base),
            &pass_jar(),
        )
        .await
        .expect("mock is reachable");

        let step3 = &hits_of(&hits)[2].path;
        let expected = signature_client_sign("n-1", Some("s-1"));
        assert!(
            step3.contains("/pass/second?nonce=n-1"),
            "step 3 must reuse the phase-2 location, got {step3}"
        );
        assert!(
            step3.ends_with(&format!("&clientSign={expected}")),
            "step 3 must end with clientSign={expected}, got {step3}"
        );
    }

    /// Every `return null` in the reference, each asserted at the step that
    /// produces it. A chain that runs to completion but yields nothing must be
    /// indistinguishable, to the caller, from one that never started.
    #[tokio::test]
    async fn every_null_return_in_the_reference_is_mapped_to_none() {
        let client = mimo_sso_client().unwrap();
        let jar = pass_jar();

        // Step 1 answering without a Location.
        let (base, _) = serve(|_| vec![Reply::plain(200)]).await;
        assert_eq!(
            acquire_service_cookie(&client, &endpoints_at(&base), &jar)
                .await
                .expect("reachable"),
            None,
            "no Location on step 1"
        );

        // Step 1 whose Location carries no callback parameter.
        let (base, _) = serve(|base| vec![Reply::redirect(&format!("{base}/login"))]).await;
        assert_eq!(
            acquire_service_cookie(&client, &endpoints_at(&base), &jar)
                .await
                .expect("reachable"),
            None,
            "a Location with no callback parameter"
        );

        // Step 2 yielding no usable nonce, or no location, or no JSON.
        // Note the first case must be a location with *no* nonce in its query:
        // a location that carries one satisfies the reference's fallback and
        // the chain correctly continues.
        for (label, second) in [
            ("a location with no nonce anywhere", "loc-only"),
            ("a nonce but no location", "nonce-only"),
            ("no JSON at all", "garbage"),
        ] {
            let (base, _) = serve(move |base| {
                let sts = format!("{base}/api/sts?ticket=t-1");
                let body = match second {
                    "loc-only" => format!(r#"{{"location":"{base}/pass/second"}}"#),
                    "nonce-only" => r#"{"nonce":"n-1"}"#.to_string(),
                    _ => "<html>error</html>".to_string(),
                };
                vec![
                    Reply::redirect(&format!(
                        "{base}/sso?callback={}",
                        encode_uri_component(&sts)
                    )),
                    Reply::sso(&body),
                ]
            })
            .await;
            assert_eq!(
                acquire_service_cookie(&client, &endpoints_at(&base), &jar)
                    .await
                    .expect("reachable"),
                None,
                "step 2 with {label} must abort the chain"
            );
        }

        // Step 4 pointing somewhere that is not the sts callback.
        let (base, _) = serve(|base| {
            let sts = format!("{base}/api/sts?ticket=t-1");
            vec![
                Reply::redirect(&format!(
                    "{base}/sso?callback={}",
                    encode_uri_component(&sts)
                )),
                Reply::sso(&format!(
                    r#"{{"nonce":"n-1","location":"{base}/pass/second?nonce=n-1"}}"#
                )),
                Reply::cookie("serviceToken", "svc"),
                Reply::sso(&format!(r#"{{"location":"{base}/somewhere/else"}}"#)),
            ]
        })
        .await;
        assert_eq!(
            acquire_service_cookie(&client, &endpoints_at(&base), &jar)
                .await
                .expect("reachable"),
            None,
            "a step-4 location that is not /api/sts must abort"
        );

        // A chain that runs to completion but never mints a serviceToken.
        let (base, _) = serve(|base| {
            let mut replies = happy_path(base, "unused");
            replies[2] = Reply::plain(200);
            replies[4] = Reply::plain(200);
            replies
        })
        .await;
        assert_eq!(
            acquire_service_cookie(&client, &endpoints_at(&base), &jar)
                .await
                .expect("reachable"),
            None,
            "a chain that never mints serviceToken must not return a header"
        );
    }

    /// A handshake that fails must leave the caller's pass jar untouched —
    /// otherwise a failed refresh would poison the stored credentials.
    #[tokio::test]
    async fn a_failed_handshake_does_not_mutate_the_caller_s_jar() {
        let (base, _) = serve(|_| vec![Reply::plain(200)]).await;
        let jar = pass_jar();
        let before = jar.clone();
        let _ = acquire_service_cookie(&mimo_sso_client().unwrap(), &endpoints_at(&base), &jar)
            .await
            .expect("reachable");
        assert_eq!(jar, before, "the pass jar is copied, not consumed");
    }

    /// A cached session must be reused rather than re-handshaked, and the
    /// second call must not touch the network at all.
    #[tokio::test]
    async fn a_second_call_is_served_from_cache() {
        let (base, hits) = serve(|base| happy_path(base, "svc-cache")).await;
        let endpoints = endpoints_at(&base);
        let cache = MimoCookieCache::new();
        let client = mimo_sso_client().unwrap();

        let first = get_service_cookie(&cache, &client, &endpoints, &pass_jar()).await;
        let second = get_service_cookie(&cache, &client, &endpoints, &pass_jar()).await;

        assert_eq!(first, second);
        assert_eq!(first.failure, None);
        assert_eq!(
            hits_of(&hits).len(),
            5,
            "the second call must be served from cache, not re-handshaked"
        );
    }

    /// A burst for one account must run the chain once. The reference is
    /// explicit that concurrent handshakes share a single promise, because
    /// "a burst of requests must not each run the full 5-step SSO chain".
    #[tokio::test]
    async fn concurrent_calls_for_one_account_share_a_single_handshake() {
        let (base, hits) = serve(|base| happy_path(base, "svc-burst")).await;
        let endpoints = endpoints_at(&base);
        let cache = Arc::new(MimoCookieCache::new());
        let jar = Arc::new(pass_jar());

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let cache = Arc::clone(&cache);
            let jar = Arc::clone(&jar);
            let endpoints = endpoints.clone();
            let client = mimo_sso_client().unwrap();
            tasks.push(tokio::spawn(async move {
                get_service_cookie(&cache, &client, &endpoints, &jar).await
            }));
        }

        let mut results = Vec::new();
        for task in tasks {
            results.push(task.await.expect("task joins"));
        }

        for result in &results {
            assert_eq!(
                result.cookie.as_deref(),
                Some("mimopc_ph=ph-1; serviceToken=svc-burst; userId=u-1"),
                "every caller must get the same cookie"
            );
        }
        assert_eq!(
            hits_of(&hits).len(),
            5,
            "8 concurrent callers must produce one 5-step handshake, not 8"
        );
    }

    /// A follower that arrives while the leader is still running must wait for
    /// the leader's result rather than starting a second chain. Serving a
    /// second mock-backed server would make a wrongly-de-duplicated
    /// implementation visible as extra requests.
    #[tokio::test]
    async fn a_caller_arriving_mid_handshake_joins_the_one_in_flight() {
        let (base, hits) = serve(|base| happy_path(base, "svc-join")).await;
        let endpoints = endpoints_at(&base);
        let cache = Arc::new(MimoCookieCache::new());
        let jar = pass_jar();

        let leader_cache = Arc::clone(&cache);
        let leader_client = mimo_sso_client().unwrap();
        let leader_endpoints = endpoints.clone();
        let leader_jar = jar.clone();
        let leader = tokio::spawn(async move {
            get_service_cookie(
                &leader_cache,
                &leader_client,
                &leader_endpoints,
                &leader_jar,
            )
            .await
        });

        // Let the leader register its in-flight entry, then join it.
        for _ in 0..50 {
            if !cache.inflight.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let follower =
            get_service_cookie(&cache, &mimo_sso_client().unwrap(), &endpoints, &jar).await;
        let leader = leader.await.expect("leader joins");

        assert_eq!(leader, follower);
        assert_eq!(leader.failure, None);
        assert_eq!(
            hits_of(&hits).len(),
            5,
            "the follower must join the in-flight handshake, not start its own"
        );
    }

    /// Invalidation must force a fresh handshake — this is what a 401 recovery
    /// calls, and it is useless if the stale cookie is still served.
    #[tokio::test]
    async fn invalidation_forces_a_new_handshake() {
        let (base, hits) = serve(|base| {
            let mut first = happy_path(base, "svc-i");
            first.extend(happy_path(base, "svc-i2"));
            first
        })
        .await;
        let endpoints = endpoints_at(&base);
        let cache = MimoCookieCache::new();
        let client = mimo_sso_client().unwrap();

        get_service_cookie(&cache, &client, &endpoints, &pass_jar()).await;
        assert_eq!(hits_of(&hits).len(), 5);
        cache.invalidate();
        let refreshed = get_service_cookie(&cache, &client, &endpoints, &pass_jar()).await;
        assert_eq!(
            hits_of(&hits).len(),
            10,
            "after invalidation the chain re-runs"
        );
        assert_eq!(
            refreshed.cookie.as_deref(),
            Some("mimopc_ph=ph-1; serviceToken=svc-i2; userId=u-1"),
            "the refreshed session must be the new one, not the cached one"
        );
    }

    /// A missing passToken is distinguishable from a failed handshake — the
    /// difference is what the quota endpoint reports to the user. This also
    /// pins the early return: the real endpoints are in use and nothing is
    /// listening, so any request would fail.
    #[tokio::test]
    async fn a_missing_pass_token_is_reported_before_any_request() {
        let cache = MimoCookieCache::new();
        let client = mimo_sso_client().unwrap();
        let endpoints = MimoEndpoints::default();

        assert_eq!(
            get_service_cookie(&cache, &client, &endpoints, &HashMap::new()).await,
            MimoServiceCookie::failed(MimoSessionFailure::NoPassToken)
        );
        assert_eq!(
            get_service_cookie(
                &cache,
                &client,
                &endpoints,
                &HashMap::from([("passToken".to_string(), String::new())]),
            )
            .await,
            MimoServiceCookie::failed(MimoSessionFailure::NoPassToken),
            "an empty passToken is no passToken"
        );
    }

    /// A transport failure must degrade, not propagate: a quota panel cannot
    /// be allowed to take a request down.
    #[tokio::test]
    async fn a_transport_failure_degrades_to_no_session() {
        let cache = MimoCookieCache::new();
        // Nothing is listening on this port.
        let endpoints = MimoEndpoints {
            api_base: "http://127.0.0.1:1".to_string(),
            account_base: "http://127.0.0.1:1".to_string(),
        };
        let result =
            get_service_cookie(&cache, &mimo_sso_client().unwrap(), &endpoints, &pass_jar()).await;
        assert_eq!(
            result,
            MimoServiceCookie::failed(MimoSessionFailure::SsoFailed)
        );
    }

    /// The pass jar is a parameter rather than something read from disk — so
    /// the handshake is exercisable. This is that claim, and it is why the
    /// tests above can run at all: no MiMo Desktop cookie DB exists here.
    #[test]
    fn the_handshake_takes_its_pass_jar_as_an_argument() {
        let mut connection = ProviderConnection::default();
        connection.provider = "xiaomi-mimo".into();
        connection
            .provider_specific_data
            .insert("mimoPassToken".into(), serde_json::json!("pt-configured"));
        connection
            .provider_specific_data
            .insert("mimoUserId".into(), serde_json::json!("u-9"));

        let jar = resolve_pass_jar(Some(&connection)).expect("a configured token is usable");
        assert_eq!(
            jar.get("passToken").map(String::as_str),
            Some("pt-configured")
        );
        assert_eq!(jar.get("userId").map(String::as_str), Some("u-9"));
        assert!(!jar.contains_key("cUserId"), "an unset id must stay absent");
    }

    /// A blank configured token must fall through to the Desktop store rather
    /// than start a handshake that cannot succeed.
    #[test]
    fn a_blank_configured_token_is_treated_as_absent() {
        let mut connection = ProviderConnection::default();
        connection
            .provider_specific_data
            .insert("mimoPassToken".into(), serde_json::json!("   "));

        // On a machine with no MiMo Desktop profile this is None; the point is
        // that it is never a jar holding the blank token.
        if let Some(jar) = resolve_pass_jar(Some(&connection)) {
            assert_ne!(
                jar.get("passToken").map(|token| token.trim()),
                Some(""),
                "a blank token must never reach the handshake"
            );
        }
    }

    /// Serve one JSON payload and hand back the base URL, for the quota tests.
    async fn serve_json(status: u16, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let body = body.to_string();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
        base
    }

    #[tokio::test]
    async fn the_usage_endpoint_maps_a_successful_payload() {
        let base = serve_json(
            200,
            r#"{"code":0,"data":{"percent":42.5,"resetDate":"2026-10-04","resetAt":1760000000}}"#,
        )
        .await;
        let usage =
            get_mimo_account_usage(&Client::new(), &endpoints_at(&base), "serviceToken=svc").await;

        assert_eq!(usage.error, None);
        assert_eq!(usage.percent, Some(42.5));
        assert_eq!(usage.reset_date.as_deref(), Some("2026-10-04"));
        assert_eq!(usage.reset_at, Some(1_760_000_000));
    }

    /// A 200 carrying a non-zero business code is a failure, not an empty
    /// quota — otherwise the dashboard would render "0% used" on rejection.
    #[tokio::test]
    async fn a_non_zero_business_code_is_a_failure_not_an_empty_quota() {
        let base = serve_json(200, r#"{"code":401,"data":null}"#).await;
        let usage =
            get_mimo_account_usage(&Client::new(), &endpoints_at(&base), "serviceToken=svc").await;
        assert_eq!(usage.error.as_deref(), Some("bad-response"));
        assert_eq!(usage.percent, None);
    }

    /// A non-2xx is reported with its status so the dashboard can distinguish
    /// "not authorised" from "service is down".
    #[tokio::test]
    async fn a_non_2xx_is_reported_with_its_status() {
        let base = serve_json(503, "").await;
        let usage =
            get_mimo_account_usage(&Client::new(), &endpoints_at(&base), "serviceToken=svc").await;
        assert_eq!(usage.error.as_deref(), Some("http-503"));
    }

    /// The failure string must not embed the request URL. The reference
    /// reports the raw fetch error, which under `reqwest` would carry the URL
    /// and the cookie; this is the one place the port deliberately diverges.
    #[tokio::test]
    async fn a_transport_failure_does_not_leak_the_url_or_the_cookie() {
        let endpoints = MimoEndpoints {
            api_base: "http://127.0.0.1:1".to_string(),
            account_base: "http://127.0.0.1:1".to_string(),
        };
        let usage = get_mimo_account_usage(&Client::new(), &endpoints, "serviceToken=secret").await;
        let error = usage.error.expect("a failure is reported");
        assert_eq!(error, "request-failed");
        assert!(!error.contains("127.0.0.1"), "the URL must not leak");
        assert!(!error.contains("secret"), "the cookie must not leak");
    }
}
