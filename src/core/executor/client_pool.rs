use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// Cache-key suffix for clients built with certificate verification
/// disabled. Kept separate from the secure key so the one-shot insecure
/// retry never shares a connection pool with (or poisons) the normal path.
const INSECURE_KEY_SUFFIX: &str = "|insecure-tls";
/// Cache-key suffix for clients that never follow redirects. SigV4-signed
/// providers (Bedrock) must not let reqwest replay a signed body +
/// `x-amz-security-token` at whatever origin a 3xx names.
const NO_REDIRECT_KEY_SUFFIX: &str = "|no-redirect";

/// How often the stale-connection cleaner runs.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(300); // 5 minutes
/// Connections unused for longer than this are removed.
const CLEANUP_MAX_IDLE: Duration = Duration::from_secs(600); // 10 minutes

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
        self.get_inner(provider_key, proxy, false)
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
        self.get_inner(provider_key, proxy, true, false)
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
        self.get_inner(provider_key, proxy, false, true)
    }

    fn get_inner(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
        insecure: bool,
        no_redirect: bool,
    ) -> Result<Arc<reqwest::Client>, reqwest::Error> {
        let timeout = self.timeout;
        let mut key = client_key(provider_key, proxy);
        if insecure {
            key = format!("{key}{INSECURE_KEY_SUFFIX}");
        }
        if no_redirect {
            key = format!("{key}{NO_REDIRECT_KEY_SUFFIX}");
        }
        let result = self
            .reqwest_clients
            .entry(key.clone())
            .or_try_insert_with(|| build_reqwest_client(proxy, timeout, insecure, no_redirect));
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
    /// retry. A non-replayable body (streaming upload) skips the fallback
    /// and sends once, matching upstream's locked-body guard.
    pub async fn send_with_tls_fallback(
        &self,
        provider_key: &str,
        proxy: Option<&ProxyTarget>,
        request: reqwest::Request,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let Some(retry_request) = request.try_clone() else {
            let client = self.get(provider_key, proxy)?;
            return client.execute(request).await;
        };

        let secure_client = self.get(provider_key, proxy)?;
        send_once_with_insecure_retry(
            strict_ssl_enabled(),
            || secure_client.execute(request),
            || async {
                let insecure_client = self.get_insecure(provider_key, proxy)?;
                insecure_client.execute(retry_request).await
            },
            is_tls_cert_error,
        )
        .await
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
) -> Result<Arc<reqwest::Client>, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        // SigV4-signed requests (Bedrock) must surface 3xx to the caller
        // rather than have reqwest replay the signed body/headers.
        .redirect(if no_redirect {
            reqwest::redirect::Policy::none()
        } else {
            reqwest::redirect::Policy::default()
        })
        .pool_idle_timeout(CLIENT_POOL_IDLE_TIMEOUT)
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
