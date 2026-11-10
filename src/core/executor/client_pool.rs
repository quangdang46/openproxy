use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use dashmap::DashMap;
use http_body_util::Full;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::{connect::HttpConnector, Client as HyperClient};
use hyper_util::rt::{TokioExecutor, TokioTimer};

use crate::core::config::runtime_config;
use crate::core::dns::MitmBypassResolver;
use crate::core::proxy::ProxyTarget;
use crate::core::tls::ensure_rustls_provider;

pub const CLIENT_POOL_MAX_IDLE_PER_HOST: usize = 8;
pub const CLIENT_POOL_TCP_KEEPALIVE: Duration = Duration::from_secs(60);
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(180);

/// Cache-key suffix for clients built with certificate verification
/// disabled. Kept separate from the secure key so the one-shot insecure
/// retry never shares a connection pool with (or poisons) the normal path.
const INSECURE_KEY_SUFFIX: &str = "|insecure-tls";
/// Cache-key suffix for clients that never follow redirects. SigV4-signed
/// providers (Bedrock) must not let reqwest replay a signed body +
/// `x-amz-security-token` at whatever origin a 3xx names.
const NO_REDIRECT_KEY_SUFFIX: &str = "|no-redirect";
/// Cache-key suffix for the no-keep-alive client used by the one-shot
/// stale-socket replay in [`ClientPool::send_with_tls_fallback`]. Kept
/// separate so the replay never draws (or poisons) the normal pool.
const FRESH_SOCKET_KEY_SUFFIX: &str = "|fresh-socket";

/// How often the stale-connection cleaner runs.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(300); // 5 minutes
/// Connections unused for longer than this are removed.
const CLEANUP_MAX_IDLE: Duration = Duration::from_secs(600); // 10 minutes

/// Idle TTL for pooled connections, capped below upstream/edge idle windows
/// (Cloudflare, AWS Global Accelerator / NVIDIA NIM) that silently drop
/// keep-alive sockets without a TCP RST. Port of upstream
/// `resolveKeepAliveTimeouts` (`FETCH_KEEPALIVE_TIMEOUT_MS`, 4s default) —
/// 9router v0.5.116 commit 963787f7.
fn pool_idle_timeout() -> Duration {
    Duration::from_millis(runtime_config::fetch_keepalive_timeout_ms())
}

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
    /// Tracks the last access time for each client key, used by the periodic
    /// cleaner to remove stale connections.
    last_accessed: Arc<DashMap<String, Instant>>,
    timeout: ClientTimeout,
}

impl Default for ClientPool {
    fn default() -> Self {
        Self {
            reqwest_clients: DashMap::new(),
            hyper_clients: DashMap::new(),
            last_accessed: Arc::new(DashMap::new()),
            timeout: ClientTimeout::default(),
        }
    }
}

pub type DirectHyperClient = HyperClient<HttpsConnector<HttpConnector>, Full<Bytes>>;

/// The per-read idle deadline for a response body.
///
/// Deliberately the SAME shared constant the stream stall watchdog uses, so the
/// transport layer and the handler cannot disagree about how long silence is
/// acceptable. Exposed as a function so a test can pin the relationship —
/// a bare literal in the builder would drift from the watchdog the moment
/// either side is tuned.
fn stream_read_timeout() -> Duration {
    Duration::from_millis(crate::core::config::runtime_config::STREAM_STALL_TIMEOUT_MS)
}

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
        self.get_inner(provider_key, proxy, false, false, false)
    }

    /// A pooled client that skips TLS certificate verification. Used ONLY by
    /// the one-shot insecure retry in [`Self::send_with_tls_fallback`] after
    /// a self-signed/unverifiable-cert failure — never for the primary
    /// attempt (port of 9router `proxyFetch.js` `getDispatcher(_, true)` /
    /// `rejectUnauthorized: false`, open-sse/utils/proxyFetch.js:233-253).
    pub fn get_insecure(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Result<Arc<reqwest::Client>, reqwest::Error> {
        self.get_inner(provider_key, proxy, true, false, false)
    }

    /// A pooled client whose connections are never reused
    /// (`pool_idle_timeout` ≈ [`runtime_config::FRESH_RETRY_KEEPALIVE_TIMEOUT_MS`]).
    /// Used ONLY by the one-shot stale-socket replay in
    /// [`Self::send_with_tls_fallback`] — port of upstream
    /// `getFreshRetryDispatcher` (`keepAliveTimeout: 1`), guaranteeing a
    /// fresh TCP/TLS connection after a silently-dropped keep-alive socket.
    pub fn get_fresh(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Result<Arc<reqwest::Client>, reqwest::Error> {
        self.get_inner(provider_key, proxy, false, false, true)
    }

    /// A pooled client that never follows redirects
    /// (`reqwest::redirect::Policy::none()`). Required for SigV4-signed
    /// requests: reqwest strips `Authorization` on cross-host redirects but
    /// still replays the body and the `x-amz-*` signature headers, which
    /// would leak a signed payload to whatever origin the 3xx names.
    pub fn get_no_redirect(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Result<Arc<reqwest::Client>, reqwest::Error> {
        self.get_inner(provider_key, proxy, false, true, false)
    }

    fn get_inner(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
        insecure: bool,
        no_redirect: bool,
        fresh: bool,
    ) -> Result<Arc<reqwest::Client>, reqwest::Error> {
        let timeout = self.timeout;
        let mut key = client_key(provider_key, proxy);
        if insecure {
            key = format!("{key}{INSECURE_KEY_SUFFIX}");
        }
        if no_redirect {
            key = format!("{key}{NO_REDIRECT_KEY_SUFFIX}");
        }
        if fresh {
            key = format!("{key}{FRESH_SOCKET_KEY_SUFFIX}");
        }
        let result = self
            .reqwest_clients
            .entry(key.clone())
            .or_try_insert_with(|| {
                build_reqwest_client(proxy, timeout, insecure, no_redirect, fresh)
            });
        match result {
            Ok(entry) => {
                self.touch(key);
                Ok(entry.clone())
            }
            Err(e) => Err(e),
        }
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
        self.touch(provider_key.to_string());
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

    /// Execute `request`, retrying ONCE with certificate verification
    /// disabled when the failure is a TLS certificate error (self-signed
    /// chain, expired cert, bad altname, …).
    ///
    /// Port of 9router `fetchWithTlsFallback`
    /// (`open-sse/utils/proxyFetch.js:254-274`, cert set at :102-115):
    /// `STRICT_SSL=true`/`1` disables the fallback, matching upstream's
    /// `options.strictTls === true || process.env.STRICT_SSL`. The insecure
    /// attempt uses a SEPARATE pooled client (`danger_accept_invalid_certs`)
    /// keyed with [`INSECURE_KEY_SUFFIX`], so only that attempt skips
    /// verification — the primary path stays strict.
    ///
    /// The request must be replayable: callers build it via
    /// `RequestBuilder::build()` and this method uses `try_clone()` for the
    /// retries. A non-replayable body (streaming upload) skips the fallbacks
    /// and sends once, matching upstream's locked-body guard.
    ///
    /// Stale keep-alive recovery (9router v0.5.116, commit 963787f7 —
    /// `fetchWithTlsFallback`'s `forceFreshSocket` replay): when the pooled
    /// socket the edge silently dropped surfaces as a transport error before
    /// response headers, the request is replayed ONCE on
    /// [`Self::get_fresh`], which never reuses connections. The replay only
    /// fires on errors that prove the socket died ([`is_stale_socket_error`])
    /// — never on DNS/connect-refused/TLS failures — and is skipped for
    /// configured proxies and when `FETCH_FRESH_RETRY=off`.
    pub async fn send_with_tls_fallback(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
        request: reqwest::Request,
    ) -> Result<reqwest::Response, reqwest::Error> {
        self.send_with_tls_fallback_opts(provider_key, proxy, request, false)
            .await
    }

    /// [`Self::send_with_tls_fallback`] with the executor/base.js
    /// `forceFreshSocket` escalation (9router v0.5.116, commit 963787f7):
    /// after a connect timeout, network error, or edge 502, the retry must
    /// not draw another possibly-poisoned pooled socket — the primary client
    /// is [`Self::get_fresh`], whose ~zero idle TTL guarantees a new
    /// TCP/TLS connection.
    pub async fn send_with_tls_fallback_opts(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
        request: reqwest::Request,
        force_fresh: bool,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let Some(retry_request) = request.try_clone() else {
            let client = if force_fresh {
                self.get_fresh(provider_key, proxy)?
            } else {
                self.get(provider_key, proxy)?
            };
            return client.execute(request).await;
        };
        // The stale-socket replay needs a second replayable copy; a request
        // that clones once but not twice still gets the TLS fallback.
        let fresh_request = request.try_clone();

        let secure_client = if force_fresh {
            self.get_fresh(provider_key, proxy)?
        } else {
            self.get(provider_key, proxy)?
        };
        let result = send_once_with_insecure_retry(
            strict_ssl_enabled(),
            || secure_client.execute(request),
            || async {
                let insecure_client = self.get_insecure(provider_key, proxy)?;
                insecure_client.execute(retry_request).await
            },
            is_tls_cert_error,
        )
        .await;

        match (result, fresh_request) {
            (Err(err), Some(req))
                if fresh_socket_replay_allowed(proxy) && is_stale_socket_error(&err) =>
            {
                let fresh_client = self.get_fresh(provider_key, proxy)?;
                fresh_client.execute(req).await
            }
            (result, _) => result,
        }
    }

    /// Record that a key was just accessed (for staleness tracking).
    fn touch(&self, key: String) {
        self.last_accessed.insert(key, Instant::now());
    }

    /// Remove all clients that have not been accessed in more than
    /// [`CLEANUP_MAX_IDLE`]. Runs synchronously — call from a background
    /// task or spawn_blocking.
    pub fn cleanup_stale(&self) {
        let now = Instant::now();
        let mut stale = Vec::new();

        // Check reqwest clients
        for entry in self.last_accessed.iter() {
            if now.duration_since(*entry.value()) > CLEANUP_MAX_IDLE {
                stale.push(entry.key().clone());
            }
        }
        for key in &stale {
            self.reqwest_clients.remove(key);
            self.hyper_clients.remove(key);
            self.last_accessed.remove(key);
        }
    }

    /// Spawn a background task that periodically removes stale connections.
    /// See [`CLEANUP_INTERVAL`] and [`CLEANUP_MAX_IDLE`].
    pub fn start_periodic_cleanup(self: &Arc<Self>) {
        let pool = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(CLEANUP_INTERVAL).await;
                pool.cleanup_stale();
                tracing::trace!(
                    target: "openproxy::client_pool",
                    "cleaned stale connections: {} reqwest, {} hyper remaining",
                    pool.reqwest_clients.len(),
                    pool.hyper_clients.len(),
                );
            }
        });
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
    insecure: bool,
    no_redirect: bool,
    fresh: bool,
) -> Result<Arc<reqwest::Client>, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        // SigV4-signed requests (Bedrock) must surface 3xx to the caller
        // rather than have reqwest replay the signed body/headers.
        .redirect(if no_redirect {
            reqwest::redirect::Policy::none()
        } else {
            reqwest::redirect::Policy::default()
        })
        // Fresh-socket clients get a ~zero idle TTL so the pool can never
        // hand out a connection the upstream edge already dropped (port of
        // undici `keepAliveTimeout`/`keepAliveMaxTimeout` = 1ms). Normal
        // clients use the capped keep-alive TTL (`FETCH_KEEPALIVE_TIMEOUT_MS`).
        .pool_idle_timeout(if fresh {
            Duration::from_millis(runtime_config::FRESH_RETRY_KEEPALIVE_TIMEOUT_MS)
        } else {
            pool_idle_timeout()
        })
        .pool_max_idle_per_host(CLIENT_POOL_MAX_IDLE_PER_HOST)
        .tcp_keepalive(CLIENT_POOL_TCP_KEEPALIVE)
        .connect_timeout(timeout.connect)
        // Per-READ idle timeout, not a whole-lifecycle cap.
        //
        // reqwest's `.timeout()` runs from request start until the response
        // BODY finishes, not until headers. send_one returns Ok at headers
        // (default.rs), so a healthy SSE stream that legitimately runs for
        // minutes was killed at this value regardless of the stall watchdog —
        // the watchdog never got a chance to be the thing that decided. 9router
        // has no whole-body deadline at all: a connect timer for headers
        // (base.js:134-137, cleared once headers arrive) plus per-chunk stall
        // handling. `.read_timeout()` is the per-read equivalent, and it bounds
        // a non-streaming body that never arrives, which `.timeout()` only did by
        // accident and at the cost of every long stream.
        //
        // The value is the SAME shared constant the stall watchdog uses, so
        // there is one clock and not two that can disagree.
        .read_timeout(stream_read_timeout())
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

    if insecure {
        // Port of `rejectUnauthorized: false` (proxyFetch.js:244-247). Both
        // flags are needed: upstream's retry set includes
        // ERR_TLS_CERT_ALTNAME_INVALID, which is a hostname check, not a
        // chain-validation failure.
        builder = builder
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true);
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
    builder.pool_idle_timeout(pool_idle_timeout());
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

/// Upstream gate: `options.strictTls === true || STRICT_SSL === "true" || "1"`
/// (proxyFetch.js:262). Case-sensitive, matching Node's `===`.
fn strict_ssl_enabled() -> bool {
    matches!(
        std::env::var("STRICT_SSL").ok().as_deref(),
        Some("true") | Some("1")
    )
}

/// Port of `isTlsCertError` (proxyFetch.js:112-115): the TLS_CERT_ERRORS set
/// covers self-signed chains, unverifiable leaf signatures, missing issuers,
/// expired certs, and bad altnames. rustls surfaces the same failures as
/// `rustls::Error::InvalidCertificate(CertificateError::…)` somewhere in the
/// `reqwest::Error` source chain, so classify by walking that chain.
pub fn is_tls_cert_error(err: &reqwest::Error) -> bool {
    tls_cert_error_in_chain(err)
}

/// Walk an error chain looking for a rustls certificate-validation failure.
/// Generic over `dyn Error` so tests can exercise it without a live TLS
/// handshake (a `reqwest::Error` cannot be constructed synthetically).
pub fn tls_cert_error_in_chain(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(err);
    while let Some(e) = current {
        if let Some(rustls_err) = e.downcast_ref::<rustls::Error>() {
            if is_cert_rustls_error(rustls_err) {
                return true;
            }
        }
        current = e.source();
    }
    false
}

/// Eligibility gate for the stale-socket replay, mirroring upstream's
/// conditions in `fetchWithTlsFallback` (proxyFetch.js): the replay is
/// skipped for proxies (`!proxyUrl`), for non-replayable bodies (handled by
/// the caller's `try_clone`), for aborts, and when `FETCH_FRESH_RETRY=off`.
pub fn fresh_socket_replay_allowed(proxy: Option<&ProxyTarget>) -> bool {
    runtime_config::fresh_socket_replay_enabled() && fresh_socket_replay_direct_only(proxy)
}

/// The proxy half of the gate, split out so it can be unit-tested without
/// mutating the process-wide `FETCH_FRESH_RETRY` env var.
fn fresh_socket_replay_direct_only(proxy: Option<&ProxyTarget>) -> bool {
    proxy.map(|p| p.url.trim().is_empty()).unwrap_or(true)
}

/// Port of `isStaleSocketError` (proxyFetch.js, v0.5.116 commit 963787f7):
/// a pooled keep-alive socket that an upstream edge (NVIDIA NIM behind AWS
/// Global Accelerator, Cloudflare) silently dropped surfaces as a transport
/// error before response headers. DNS, connect-refused, connect-timeout and
/// TLS failures are excluded — those prove the host is unreachable, never a
/// stale pool entry — matching upstream's NON_STALE_SOCKET_ERROR_CODES.
pub fn is_stale_socket_error(err: &reqwest::Error) -> bool {
    if err.is_connect() || err.is_redirect() || is_tls_cert_error(err) {
        return false;
    }
    // A read/headers timeout is upstream's UND_ERR_HEADERS_TIMEOUT analogue:
    // the socket was accepted but never answered — the classic dead-socket
    // signature. Connect timeouts are already excluded via `is_connect`.
    if err.is_timeout() {
        return true;
    }
    stale_socket_error_in_chain(err)
}

/// Message fragments that identify a socket that died before the response
/// (hyper "connection closed before message completed", io broken pipe /
/// reset / unexpected EOF, undici-style "other side closed" / "socket hang
/// up"). Deliberately excludes a bare ambiguous "request failed" — the
/// upstream may already hold that request and a replay would bill it twice.
const STALE_SOCKET_MESSAGE_HINTS: &[&str] = &[
    "other side closed",
    "socket hang up",
    "connection closed",
    "incomplete message",
    "broken pipe",
    "connection reset",
    "unexpected eof",
    "channel closed",
];

/// Walk an error chain looking for a dead-socket signature. Generic over
/// `dyn Error` so unit tests can exercise it without a live connection (a
/// `reqwest::Error` cannot be constructed synthetically).
pub fn stale_socket_error_in_chain(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(err);
    while let Some(e) = current {
        if let Some(io_err) = e.downcast_ref::<io::Error>() {
            if matches!(
                io_err.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::UnexpectedEof
            ) {
                return true;
            }
        }
        if let Some(hyper_err) = e.downcast_ref::<hyper::Error>() {
            if hyper_err.is_incomplete_message() || hyper_err.is_closed() {
                return true;
            }
        }
        let msg = e.to_string().to_lowercase();
        if STALE_SOCKET_MESSAGE_HINTS
            .iter()
            .any(|hint| msg.contains(hint))
        {
            return true;
        }
        current = e.source();
    }
    false
}

/// Map rustls certificate errors onto upstream's TLS_CERT_ERRORS set:
/// self-signed / unverifiable leaf / missing issuer → UnknownIssuer,
/// BadSignature; expired → Expired; altname → NotValidForName. Revocation
/// and other failures are NOT retried, matching upstream (CERT_REVOKED is
/// absent from the set).
fn is_cert_rustls_error(err: &rustls::Error) -> bool {
    use rustls::CertificateError as CertErr;
    matches!(
        err,
        rustls::Error::InvalidCertificate(
            CertErr::UnknownIssuer
                | CertErr::BadSignature
                | CertErr::Expired
                | CertErr::NotValidForName
        )
    )
}

/// The retry-once core, generic over response and error types so unit tests
/// can drive it without constructing a `reqwest::Error`. `send_insecure` is
/// invoked at most once and only when `is_cert_error` accepts the secure
/// attempt's error and `strict_ssl` is off.
async fn send_once_with_insecure_retry<R, E, FS, FI, RFut, IFut>(
    strict_ssl: bool,
    send_secure: FS,
    send_insecure: FI,
    is_cert_error: impl Fn(&E) -> bool,
) -> Result<R, E>
where
    FS: FnOnce() -> RFut,
    FI: FnOnce() -> IFut,
    RFut: std::future::Future<Output = Result<R, E>>,
    IFut: std::future::Future<Output = Result<R, E>>,
{
    match send_secure().await {
        Ok(resp) => Ok(resp),
        Err(err) if !strict_ssl && is_cert_error(&err) => send_insecure().await,
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod stream_deadline_tests {
    use super::stream_read_timeout;
    use std::time::Duration;

    /// Bead openproxy-05cz. reqwest exposes two deadline knobs whose names look
    /// interchangeable and mean opposites:
    ///   .timeout(d)      — request start until the response BODY finishes
    ///   .read_timeout(d) — max gap between two reads of the body
    /// send_one returns Ok at headers, so `.timeout()` capped the WHOLE stream
    /// and the 360s stall watchdog never got to decide. 9router has no
    /// whole-body deadline (base.js:134-137 arms the connect timer for headers,
    /// then clears it).
    ///
    /// The first version of this test asserted `per_read * 2 > per_read` and
    /// compared a value with the expression it was defined from — tautologies
    /// that stay green if `.read_timeout` is reverted to `.timeout`. This
    /// version pins the one thing that is actually at risk, which is the same
    /// class this bead's sibling bugs were: the value drifting from the shared
    /// constant, the way the 504 delay did.
    #[test]
    fn the_read_timeout_tracks_the_shared_stall_clock() {
        assert_eq!(
            stream_read_timeout(),
            Duration::from_millis(crate::core::config::runtime_config::STREAM_STALL_TIMEOUT_MS),
            "a hard-coded literal here would silently disagree with the watchdog"
        );
    }
}

#[cfg(test)]
mod tls_fallback_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    /// A stand-in for `reqwest::Error`: the classifier only needs an error
    /// chain, and `reqwest::Error` cannot be constructed in a unit test.
    #[derive(Debug)]
    struct TestError {
        kind: &'static str,
        source: Option<Box<dyn std::error::Error>>,
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "test error: {}", self.kind)
        }
    }

    impl std::error::Error for TestError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source.as_deref()
        }
    }

    fn cert_error(cert: rustls::CertificateError) -> TestError {
        TestError {
            kind: "connect",
            source: Some(Box::new(rustls::Error::InvalidCertificate(cert))),
        }
    }

    #[test]
    fn self_signed_chain_error_is_classified_as_cert_error() {
        // SELF_SIGNED_CERT_IN_CHAIN / DEPTH_ZERO_SELF_SIGNED_CERT /
        // UNABLE_TO_VERIFY_LEAF_SIGNATURE / UNABLE_TO_GET_ISSUER_CERT* all
        // surface as UnknownIssuer (or BadSignature for the leaf case) in
        // rustls — there is no IncompleteChain variant in rustls 0.23.
        for cert in [
            rustls::CertificateError::UnknownIssuer,
            rustls::CertificateError::BadSignature,
            rustls::CertificateError::Expired,
            rustls::CertificateError::NotValidForName,
        ] {
            let err = cert_error(cert.clone());
            assert!(
                tls_cert_error_in_chain(&err),
                "{cert:?} should trigger the insecure retry"
            );
        }
    }

    #[test]
    fn non_cert_tls_and_plain_errors_do_not_retry() {
        // A revoked cert is NOT in upstream's TLS_CERT_ERRORS set.
        let revoked = cert_error(rustls::CertificateError::Revoked);
        assert!(!tls_cert_error_in_chain(&revoked));

        // A rustls error that is not certificate validation.
        let handshake = TestError {
            kind: "connect",
            source: Some(Box::new(rustls::Error::NoCertificatesPresented)),
        };
        assert!(!tls_cert_error_in_chain(&handshake));

        // A plain IO failure with no rustls source.
        let io_err = TestError {
            kind: "connect",
            source: Some(Box::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "refused",
            ))),
        };
        assert!(!tls_cert_error_in_chain(&io_err));
    }

    #[tokio::test]
    async fn cert_error_triggers_exactly_one_insecure_retry() {
        let secure_calls = StdArc::new(AtomicUsize::new(0));
        let insecure_calls = StdArc::new(AtomicUsize::new(0));
        let (sc, ic) = (secure_calls.clone(), insecure_calls.clone());

        let result = send_once_with_insecure_retry(
            false,
            move || {
                sc.fetch_add(1, Ordering::SeqCst);
                async move { Err::<u8, _>(cert_error(rustls::CertificateError::UnknownIssuer)) }
            },
            move || {
                ic.fetch_add(1, Ordering::SeqCst);
                async move { Ok::<u8, TestError>(42) }
            },
            |e: &TestError| tls_cert_error_in_chain(e),
        )
        .await;

        assert!(matches!(result, Ok(42)));
        assert_eq!(secure_calls.load(Ordering::SeqCst), 1);
        assert_eq!(insecure_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn non_cert_error_never_retries() {
        let insecure_calls = StdArc::new(AtomicUsize::new(0));
        let ic = insecure_calls.clone();

        let result = send_once_with_insecure_retry(
            false,
            || async move {
                Err::<u8, _>(TestError {
                    kind: "dns",
                    source: None,
                })
            },
            move || {
                ic.fetch_add(1, Ordering::SeqCst);
                async move { Ok::<u8, TestError>(7) }
            },
            |e: &TestError| tls_cert_error_in_chain(e),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(insecure_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn strict_ssl_disables_the_insecure_retry() {
        let insecure_calls = StdArc::new(AtomicUsize::new(0));
        let ic = insecure_calls.clone();

        let result = send_once_with_insecure_retry(
            true, // STRICT_SSL=true
            || async move { Err::<u8, _>(cert_error(rustls::CertificateError::UnknownIssuer)) },
            move || {
                ic.fetch_add(1, Ordering::SeqCst);
                async move { Ok::<u8, TestError>(9) }
            },
            |e: &TestError| tls_cert_error_in_chain(e),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(insecure_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn secure_success_never_touches_insecure_path() {
        let insecure_calls = StdArc::new(AtomicUsize::new(0));
        let ic = insecure_calls.clone();

        let result = send_once_with_insecure_retry(
            false,
            || async move { Ok::<u8, TestError>(1) },
            move || {
                ic.fetch_add(1, Ordering::SeqCst);
                async move { Ok::<u8, TestError>(2) }
            },
            |e: &TestError| tls_cert_error_in_chain(e),
        )
        .await;

        assert!(matches!(result, Ok(1)));
        assert_eq!(insecure_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn insecure_retry_failure_surfaces_the_second_error() {
        // The retry runs once; if it also fails, that error is returned
        // (upstream does not loop).
        let result = send_once_with_insecure_retry(
            false,
            || async move { Err::<u8, _>(cert_error(rustls::CertificateError::Expired)) },
            || async move {
                Err::<u8, _>(TestError {
                    kind: "insecure attempt failed",
                    source: None,
                })
            },
            |e: &TestError| tls_cert_error_in_chain(e),
        )
        .await;

        let err = result.unwrap_err();
        assert_eq!(err.kind, "insecure attempt failed");
    }
}

#[cfg(test)]
mod stale_socket_tests {
    use super::*;

    /// A stand-in for `reqwest::Error` in the chain walker, same trick as
    /// `tls_fallback_tests::TestError`.
    #[derive(Debug)]
    struct ChainError {
        msg: String,
        source: Option<Box<dyn std::error::Error>>,
    }

    impl std::fmt::Display for ChainError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.msg)
        }
    }

    impl std::error::Error for ChainError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source.as_deref()
        }
    }

    fn leaf(msg: &str) -> ChainError {
        ChainError {
            msg: msg.to_string(),
            source: None,
        }
    }

    #[test]
    fn dead_socket_io_kinds_are_stale() {
        for kind in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::UnexpectedEof,
        ] {
            let err = ChainError {
                msg: "error sending request".into(),
                source: Some(Box::new(io::Error::new(kind, "dead socket"))),
            };
            assert!(stale_socket_error_in_chain(&err), "{kind:?} is stale");
        }
    }

    #[test]
    fn dead_socket_messages_are_stale() {
        // hyper's "connection closed before message completed" and the
        // undici-style substrings upstream matches on.
        for msg in [
            "connection closed before message completed",
            "error: other side closed",
            "socket hang up",
            "channel closed",
            "connection reset by peer",
        ] {
            assert!(stale_socket_error_in_chain(&leaf(msg)), "{msg} is stale");
        }
    }

    #[test]
    fn unreachable_host_errors_are_not_stale() {
        // DNS / refused / TLS-style failures must never trigger a replay —
        // the upstream may legitimately be down, and a replay would just
        // bill the same request twice.
        for msg in [
            "dns error: failed to lookup address information",
            "error trying to connect: tcp connect error: Connection refused",
            "invalid peer certificate: UnknownIssuer",
            "request failed",
            "upstream returned 502",
        ] {
            let err = ChainError {
                msg: "error sending request".into(),
                source: Some(Box::new(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    msg,
                ))),
            };
            // ConnectionRefused kind is not in the stale set; the message
            // must not contain a stale hint either.
            assert!(!stale_socket_error_in_chain(&err), "{msg} not stale");
        }
    }

    #[test]
    fn fresh_replay_is_direct_only() {
        // Pure proxy gate — no env mutation, so it can't race the
        // FETCH_FRESH_RETRY test in runtime_config.
        assert!(fresh_socket_replay_direct_only(None));
        assert!(fresh_socket_replay_direct_only(Some(&ProxyTarget {
            url: String::new(),
            ..Default::default()
        })));
        assert!(fresh_socket_replay_direct_only(Some(&ProxyTarget {
            url: "   ".into(),
            ..Default::default()
        })));
        assert!(!fresh_socket_replay_direct_only(Some(&ProxyTarget {
            url: "http://127.0.0.1:7890".into(),
            ..Default::default()
        })));
    }
}
