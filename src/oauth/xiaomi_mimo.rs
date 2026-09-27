//! Xiaomi MiMo's encrypted-callback sign-in.
//!
//! This is not OAuth2. There is no authorization code and no token endpoint:
//! before the browser opens we generate an X25519 keypair and hand the public
//! half to the platform, which redirects back to a loopback URL carrying one
//! opaque `u` parameter. That parameter is AES-256-GCM sealed under
//! `SHA-256(ECDH(our_private, their_ephemeral))` and contains the session
//! material in the clear inside the ciphertext.
//!
//! Port of `.tmp/9router/src/lib/oauth/providers/xiaomi-mimo.js`, which is the
//! authoritative reference — the legacy JS 9router has no equivalent file.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

/// Platform authorize endpoint. Overridable so a test can point the browser
/// flow at a local server.
pub const MIMO_PLATFORM_URL_DEFAULT: &str = "https://platform.xiaomimimo.com";

/// Base URL a decrypted callback falls back to when it carries no `url`.
pub const MIMO_DEFAULT_BASE_URL: &str = "https://api.xiaomimimo.com/v1";

/// The `kn` authorize parameter — the key namespace the platform files the key
/// under. Not a client id and not a secret.
pub const MIMO_KEY_NAMESPACE: &str = "mimocode";

/// Path component of the loopback redirect the platform redirects to.
pub const MIMO_CALLBACK_PATH: &str = "/";

/// Whole-handshake budget, matching `XIAOMI_MIMO_CONFIG.timeoutMs`.
pub const MIMO_OAUTH_TIMEOUT_MS: u64 = 300_000;

/// DER prefix of an X25519 `SubjectPublicKeyInfo`.
///
/// Fixed by RFC 8410 — the algorithm identifier carries no parameters, so an
/// X25519 SPKI is always this 12-byte prefix followed by the 32-byte raw key.
/// That makes the 44-byte encoding the platform expects a concatenation rather
/// than something a DER builder has to produce, which is also why this port
/// needs no ASN.1 dependency.
const X25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x03, 0x21, 0x00,
];

/// Layout of the `u` parameter, after base64 decoding:
///
/// | bytes        | meaning                                    |
/// |--------------|--------------------------------------------|
/// | `0..12`      | AES-GCM nonce                              |
/// | `12..44`     | ephemeral X25519 public key (raw)          |
/// | `44..len-16` | ciphertext                                 |
/// | `len-16..`   | 16-byte GCM auth tag                       |
///
/// A payload shorter than this cannot hold the tag plus a single byte of
/// ciphertext, so the reference rejects it up front.
const CALLBACK_NONCE_LEN: usize = 12;
const CALLBACK_EPHEMERAL_LEN: usize = 32;
const CALLBACK_TAG_LEN: usize = 16;
const CALLBACK_MIN_LEN: usize = CALLBACK_NONCE_LEN + CALLBACK_EPHEMERAL_LEN + CALLBACK_TAG_LEN + 1;

/// Why an encrypted callback could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MimoCryptoError {
    /// `u` was not valid base64.
    NotBase64,
    /// The blob is too short to contain a nonce, a key, a tag and any payload.
    TooShort(usize),
    /// The GCM tag did not verify — wrong key, or the blob was tampered with.
    Authentication,
    /// The plaintext decrypted cleanly but was not a JSON object.
    NotAnObject,
}

impl std::fmt::Display for MimoCryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotBase64 => write!(f, "encrypted callback is not valid base64"),
            Self::TooShort(n) => write!(f, "encrypted payload too short: {n} bytes"),
            Self::Authentication => write!(f, "encrypted callback failed authentication"),
            Self::NotAnObject => write!(f, "decrypted payload is not a valid object"),
        }
    }
}

impl std::error::Error for MimoCryptoError {}

/// What the platform sealed into the callback.
///
/// `uid` and `sk` are optional because the reference substitutes `null` for a
/// missing field rather than rejecting the payload — a callback with a
/// session key but no uid is still usable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MimoCallback {
    pub uid: Option<String>,
    pub sk: Option<String>,
    pub url: String,
}

/// A freshly generated handshake keypair.
///
/// The secret half never leaves the process: it is stashed in the pending-flow
/// store when the authorize URL is built and read back when the callback
/// arrives.
pub struct MimoKeyPair {
    /// base64 SPKI, sent as the `pk` authorize parameter.
    pub public_key: String,
    secret: StaticSecret,
}

impl MimoKeyPair {
    /// The retained private half, for handing to `decrypt_callback`.
    pub fn secret(&self) -> &StaticSecret {
        &self.secret
    }

    /// Consume the pair and take the private half, for handing to
    /// `register_session` — which owns it for the life of the flow.
    pub fn into_secret(self) -> StaticSecret {
        self.secret
    }
}

/// Generate the X25519 keypair the handshake is built on.
///
/// The seed comes from the OS CSPRNG, not a seeded PRNG: a predictable
/// private key would let anyone who saw one `pk` recover the session key from
/// a captured callback.
pub fn generate_keypair() -> MimoKeyPair {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let secret = StaticSecret::from(seed);
    MimoKeyPair {
        public_key: encode_public_key(PublicKey::from(&secret)),
        secret,
    }
}

/// Encode a raw X25519 public key as the base64 SPKI the `pk` parameter wants.
fn encode_public_key(public: PublicKey) -> String {
    let mut spki = Vec::with_capacity(X25519_SPKI_PREFIX.len() + 32);
    spki.extend_from_slice(&X25519_SPKI_PREFIX);
    spki.extend_from_slice(public.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(spki)
}

/// Open the `u` parameter the platform redirected back with.
///
/// # Errors
///
/// Returns [`MimoCryptoError::Authentication`] when the tag does not verify.
/// That is the expected outcome for a callback belonging to a different
/// handshake, and it is deliberately indistinguishable from tampering.
pub fn decrypt_callback(
    secret: &StaticSecret,
    encrypted_b64: &str,
) -> Result<MimoCallback, MimoCryptoError> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encrypted_b64.trim())
        .map_err(|_| MimoCryptoError::NotBase64)?;

    if raw.len() < CALLBACK_MIN_LEN {
        return Err(MimoCryptoError::TooShort(raw.len()));
    }

    let nonce = &raw[..CALLBACK_NONCE_LEN];
    let ephemeral = PublicKey::from(
        <[u8; CALLBACK_EPHEMERAL_LEN]>::try_from(
            &raw[CALLBACK_NONCE_LEN..CALLBACK_NONCE_LEN + CALLBACK_EPHEMERAL_LEN],
        )
        .expect("slice is exactly CALLBACK_EPHEMERAL_LEN bytes"),
    );

    // ciphertext || tag — aes-gcm expects the tag appended, which is how the
    // platform lays it out, so this is passed through unsplit.
    let ciphertext_and_tag = &raw[CALLBACK_NONCE_LEN + CALLBACK_EPHEMERAL_LEN..];

    let shared = secret.diffie_hellman(&ephemeral);
    let mut hasher = Sha256::new();
    hasher.update(shared.as_bytes());
    let derived: [u8; 32] = hasher.finalize().into();

    let cipher = Aes256Gcm::new_from_slice(&derived).expect("derived key is 32 bytes");
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext_and_tag)
        .map_err(|_| MimoCryptoError::Authentication)?;

    let parsed: serde_json::Value =
        serde_json::from_slice(&plaintext).map_err(|_| MimoCryptoError::NotAnObject)?;
    let object = parsed.as_object().ok_or(MimoCryptoError::NotAnObject)?;

    let field = |key: &str| {
        object
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    Ok(MimoCallback {
        uid: field("uid"),
        sk: field("sk"),
        // The reference falls back to the default base URL, so a callback that
        // omits `url` still yields a usable connection.
        url: object
            .get("url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| MIMO_DEFAULT_BASE_URL.to_string()),
    })
}

/// Platform base URL, honouring the `MIMO_PLATFORM_URL` override the
/// reference reads from the environment.
pub fn platform_url() -> String {
    std::env::var("MIMO_PLATFORM_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| MIMO_PLATFORM_URL_DEFAULT.to_string())
}

/// Build the URL the browser opens to begin the handshake.
///
/// `public_key` is the base64 SPKI from [`generate_keypair`]; `key_name` is
/// the stable per-installation label the reference derives from the hostname,
/// which lets the platform show the user a recognisable key.
pub fn build_authorize_url(public_key: &str, redirect_uri: &str, key_name: Option<&str>) -> String {
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    params.append_pair("pk", public_key);
    params.append_pair("redirect_uri", redirect_uri);
    params.append_pair("kn", MIMO_KEY_NAMESPACE);
    if let Some(name) = key_name.filter(|n| !n.is_empty()) {
        params.append_pair("key_name", name);
    }
    format!("{}/authorize?{}", platform_url(), params.finish())
}

/// Stable per-installation key label, so a re-auth reuses the same name
/// rather than accumulating keys in the platform's UI.
///
/// Derived from the hostname rather than written to disk: the platform treats
/// `key_name` as a label only, and a label that needs no filesystem write
/// cannot fail on a read-only home directory.
pub fn key_name() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    let mut hasher = Sha256::new();
    hasher.update(std::env::consts::OS.as_bytes());
    hasher.update(b"-");
    hasher.update(host.as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    format!("openproxy-xmd-{}", hex::encode(&digest[..4]))
}

#[cfg(test)]
mod tests {
    use super::tests_support::seal_for;
    use super::*;
    /// THE PORT. A round trip proves the layout, the KDF and the key encoding
    /// agree with each other, but not with the platform — so this is paired
    /// with the structural assertions below, which pin the parts the platform
    /// actually constrains.
    #[test]
    fn round_trip_recovers_the_session_key() {
        let client = generate_keypair();
        let sealed = seal_for(
            &client,
            br#"{"uid":"u-42","sk":"sk-secret","url":"https://api.xiaomimimo.com/v1"}"#,
        );

        let opened =
            decrypt_callback(client.secret(), &sealed).expect("sealed by the matching handshake");
        assert_eq!(opened.uid.as_deref(), Some("u-42"));
        assert_eq!(opened.sk.as_deref(), Some("sk-secret"));
        assert_eq!(opened.url, "https://api.xiaomimimo.com/v1");
    }

    /// The platform validates the shape of `pk` before it will use it, so the
    /// SPKI length and prefix are part of the contract, not an implementation
    /// detail. 44 bytes = 12-byte RFC 8410 prefix + 32-byte key.
    #[test]
    fn public_key_is_a_44_byte_x25519_spki() {
        let pair = generate_keypair();
        let der = base64::engine::general_purpose::STANDARD
            .decode(&pair.public_key)
            .expect("public key is base64");

        assert_eq!(der.len(), 44, "X25519 SPKI must be 44 bytes");
        assert_eq!(
            &der[..12],
            &X25519_SPKI_PREFIX,
            "prefix must be the RFC 8410 X25519 SubjectPublicKeyInfo header"
        );

        // The trailing 32 bytes must be the public half of the retained secret,
        // or the ECDH would agree on a key the platform never received.
        assert_eq!(
            &der[12..],
            PublicKey::from(pair.secret()).as_bytes(),
            "pk must carry this keypair's public half"
        );
    }

    /// A missing `url` falls back to the default base URL, matching
    /// `parsed.url || XIAOMI_MIMO_CONFIG.defaultBaseUrl`. Without this the
    /// connection would be created with an empty base and fail at request time.
    #[test]
    fn missing_url_falls_back_to_the_default_base() {
        let client = generate_keypair();
        let opened = decrypt_callback(
            client.secret(),
            &seal_for(&client, br#"{"uid":"u-1","sk":"s"}"#),
        )
        .expect("payload without url still decrypts");
        assert_eq!(opened.url, MIMO_DEFAULT_BASE_URL);
    }

    /// A present-but-empty field is treated as absent, not as a real value.
    /// The reference's `||` treats "" as falsy, so an empty `sk` must not
    /// become a connection credential.
    #[test]
    fn empty_fields_are_normalised_to_absent() {
        let client = generate_keypair();
        let opened = decrypt_callback(
            client.secret(),
            &seal_for(&client, br#"{"uid":"","sk":"","url":""}"#),
        )
        .expect("payload decrypts");
        assert_eq!(opened.uid, None);
        assert_eq!(opened.sk, None);
        assert_eq!(opened.url, MIMO_DEFAULT_BASE_URL);
    }

    /// A callback sealed for a different handshake must fail closed. This is
    /// the load-bearing security property: accepting it would hand one MiMo
    /// account's session key to another connection.
    #[test]
    fn a_callback_for_another_keypair_is_rejected() {
        let mine = generate_keypair();
        let theirs = generate_keypair();
        let sealed = seal_for(&theirs, br#"{"uid":"u-2","sk":"stolen"}"#);
        assert_eq!(
            decrypt_callback(mine.secret(), &sealed),
            Err(MimoCryptoError::Authentication),
            "an unauthenticated callback must never yield a session key"
        );
    }

    /// Tampering anywhere in the blob — including the nonce and the embedded
    /// public key, not just the ciphertext — must fail the tag check.
    #[test]
    fn tampering_with_any_region_fails_authentication() {
        let client = generate_keypair();
        let sealed = seal_for(&client, br#"{"uid":"u-3","sk":"s"}"#);
        let mut raw = base64::engine::general_purpose::STANDARD
            .decode(&sealed)
            .expect("sealed is base64");

        for offset in [0usize, 20, 50] {
            let mut tampered = raw.clone();
            tampered[offset] ^= 0x01;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&tampered);
            assert_eq!(
                decrypt_callback(client.secret(), &b64),
                Err(MimoCryptoError::Authentication),
                "flipping a bit at offset {offset} must not authenticate"
            );
        }
        raw.clear();
    }

    /// The up-front length guard, including the boundary. One byte below the
    /// minimum there is no room for a tag plus a single byte of ciphertext, so
    /// accepting it would mean handing a zero-length plaintext to AES-GCM.
    #[test]
    fn payloads_shorter_than_the_layout_are_rejected() {
        let client = generate_keypair();
        for len in [0usize, 1, CALLBACK_MIN_LEN - 1] {
            let b64 = base64::engine::general_purpose::STANDARD.encode(vec![0u8; len]);
            assert_eq!(
                decrypt_callback(client.secret(), &b64),
                Err(MimoCryptoError::TooShort(len)),
                "{len} bytes cannot hold nonce+key+tag+payload"
            );
        }
        // Exactly the minimum is structurally valid — it reaches AES-GCM and
        // fails there, which is a different error and proves the guard is
        // bounded where the reference bounds it.
        let b64 = base64::engine::general_purpose::STANDARD.encode(vec![0u8; CALLBACK_MIN_LEN]);
        assert_ne!(
            decrypt_callback(client.secret(), &b64),
            Err(MimoCryptoError::TooShort(0)),
            "the minimum length must pass the length guard"
        );
    }

    #[test]
    fn non_base64_is_reported_as_such() {
        let client = generate_keypair();
        assert_eq!(
            decrypt_callback(client.secret(), "not base64 !!!"),
            Err(MimoCryptoError::NotBase64)
        );
    }

    /// Well-formed ciphertext whose plaintext is not a JSON object must be
    /// rejected — otherwise a `null` or an array would sail through and be
    /// silently read as an empty session.
    #[test]
    fn plaintext_that_is_not_an_object_is_rejected() {
        let client = generate_keypair();
        for payload in [&b"not json"[..], b"null", b"[1,2,3]"] {
            assert_eq!(
                decrypt_callback(client.secret(), &seal_for(&client, payload)),
                Err(MimoCryptoError::NotAnObject),
                "payload {:?} must not be read as a session",
                String::from_utf8_lossy(payload)
            );
        }
    }

    /// The authorize URL must percent-encode the base64 `pk`. It routinely
    /// contains `+`, `/` and `=`, all of which are meaningful in a query
    /// string — an unencoded `+` decodes as a space and the platform rejects
    /// the handshake.
    #[test]
    fn authorize_url_percent_encodes_the_public_key() {
        // A key whose SPKI base64 is known to contain '+' and/or '/'.
        let pk = base64::engine::general_purpose::STANDARD.encode([0xfb; 44]);
        let url = build_authorize_url(
            &pk,
            "http://127.0.0.1:56121/",
            Some("openproxy-xmd-deadbeef"),
        );

        assert!(url.starts_with(&format!("{}/authorize?", platform_url())));
        let query = url
            .split_once('?')
            .expect("authorize URL carries a query")
            .1;

        for reserved in ['+', '/', '='] {
            let raw = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("pk="))
                .expect("pk parameter is present");
            assert!(
                !raw.contains(reserved),
                "raw {reserved} survived in the pk value: {raw}"
            );
        }
        assert!(query.contains("kn=mimocode"), "kn is always sent");
        assert!(query.contains("key_name=openproxy-xmd-deadbeef"));
        assert!(query.contains("redirect_uri="));
    }

    /// Omitting the key name must drop the parameter entirely rather than send
    /// an empty one, which the platform would treat as a real empty label.
    #[test]
    fn authorize_url_omits_an_absent_key_name() {
        let url = build_authorize_url("cGtx", "http://127.0.0.1:1/", None);
        assert!(!url.contains("key_name"), "no empty key_name parameter");
        assert!(build_authorize_url("cGtx", "http://x/", Some("")).contains("kn=mimocode"));
    }

    /// The key name is a label, but it must still be stable for an
    /// installation — a fresh name on every re-auth would pile up entries in
    /// the platform's key list.
    #[test]
    fn key_name_is_stable_and_well_formed() {
        let first = key_name();
        assert_eq!(first, key_name(), "the label must not change between calls");
        assert!(first.starts_with("openproxy-xmd-"));
        assert_eq!(first.len(), "openproxy-xmd-".len() + 8);
        assert!(first["openproxy-xmd-".len()..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit()));
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use aes_gcm::aead::AeadCore;

    /// The pending-session map is process-wide — the callback carries no
    /// state, so a listener has to search every session — which means tests
    /// that register sessions cannot run concurrently. Each test owns its own
    /// listener, so nothing else needs serialising.
    static SESSION_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) struct SessionTest(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl Drop for SessionTest {
        fn drop(&mut self) {
            clear_all_sessions();
        }
    }

    pub(crate) fn serialise() -> SessionTest {
        SessionTest(
            SESSION_TEST_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// Seal a payload the way the platform does, so `decrypt_callback` can be
    /// exercised end to end without a live MiMo account.
    ///
    /// The platform holds an ephemeral keypair and derives the shared secret
    /// from *its* private half and the client's public key — the mirror image
    /// of what `decrypt_callback` does. Generating a real ephemeral key here is
    /// what makes this a round trip through the actual ECDH rather than a
    /// fixture replay.
    pub(super) fn seal(client_public: &PublicKey, plaintext: &[u8]) -> String {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let ephemeral = StaticSecret::from(seed);

        let shared = ephemeral.diffie_hellman(client_public);
        let mut hasher = Sha256::new();
        hasher.update(shared.as_bytes());
        let derived: [u8; 32] = hasher.finalize().into();

        let cipher = Aes256Gcm::new_from_slice(&derived).expect("derived key is 32 bytes");
        let nonce = Aes256Gcm::generate_nonce(OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .expect("AES-GCM encrypt cannot fail for a 256-bit key");

        let mut blob = Vec::new();
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(PublicKey::from(&ephemeral).as_bytes());
        blob.extend_from_slice(&ciphertext);
        base64::engine::general_purpose::STANDARD.encode(blob)
    }

    /// Seal against the handshake `keypair` started, i.e. for the account that
    /// keypair would actually authenticate.
    pub(super) fn seal_for(keypair: &MimoKeyPair, plaintext: &[u8]) -> String {
        seal(&PublicKey::from(keypair.secret()), plaintext)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Callback proxy
//
// The platform redirects the browser to `http://127.0.0.1:<port>/?u=<blob>`,
// so the flow needs a loopback listener of its own — the main API router
// cannot receive it, because the platform is redirected to a port it chose at
// authorize time.
//
// Port of `startXiaomiMimoProxy` / `registerXiaomiMimoSession` /
// `getXiaomiMimoSessionStatus` in 9router's `lib/oauth/utils/server.js`.
// ───────────────────────────────────────────────────────────────────────────

use once_cell::sync::Lazy;
use parking_lot::Mutex as LockMutex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The account a completed handshake yields.
///
/// `access_token` is the `sk-` value the platform sealed into the callback;
/// it is stored as the connection's access token, not as an API key.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MimoLinkedAccount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    pub access_token: String,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
}

/// Where a sign-in has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MimoSessionStatus {
    /// Waiting for the platform to redirect back.
    Pending,
    /// The callback decrypted; [`MimoSessionView::result`] is populated.
    Done,
    /// Decryption failed. [`MimoSessionView::error`] says why.
    Error,
}

/// A sign-in in progress.
struct MimoSession {
    secret: StaticSecret,
    status: MimoSessionStatus,
    result: Option<MimoLinkedAccount>,
    error: Option<String>,
}

/// A session as the API reports it.
///
/// The private key is deliberately absent — the reference makes the same
/// omission, and a status endpoint that returned the key that decrypts the
/// account session would be a credential leak.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MimoSessionView {
    pub status: MimoSessionStatus,
    pub result: Option<MimoLinkedAccount>,
    pub error: Option<String>,
}

static MIMO_SESSIONS: Lazy<LockMutex<HashMap<String, MimoSession>>> =
    Lazy::new(|| LockMutex::new(HashMap::new()));

/// Register a handshake key against the `state` the flow is tracked by.
pub fn register_session(state: &str, secret: StaticSecret) -> bool {
    if state.trim().is_empty() {
        return false;
    }
    MIMO_SESSIONS.lock().insert(
        state.to_string(),
        MimoSession {
            secret,
            status: MimoSessionStatus::Pending,
            result: None,
            error: None,
        },
    );
    true
}

/// Current state of a sign-in, or `None` if it is not one we know.
pub fn session_status(state: &str) -> Option<MimoSessionView> {
    let sessions = MIMO_SESSIONS.lock();
    let session = sessions.get(state)?;
    Some(MimoSessionView {
        status: session.status,
        result: session.result.clone(),
        error: session.error.clone(),
    })
}

/// Forget a sign-in, dropping its private key.
pub fn clear_session(state: &str) {
    MIMO_SESSIONS.lock().remove(state);
}

/// Forget every sign-in, dropping every private key.
fn clear_all_sessions() {
    MIMO_SESSIONS.lock().clear();
}

/// The stop signal handed to the accept loop.
type ProxyStop = tokio::sync::watch::Receiver<bool>;

/// A running loopback callback listener.
///
/// Owns its socket, so retiring it is unambiguous — there is no shared port
/// number that a dying listener can leave advertised.
pub struct MimoProxy {
    port: u16,
    stop: tokio::sync::watch::Sender<bool>,
    retired: Arc<tokio::sync::Notify>,
}

impl MimoProxy {
    /// Bind an ephemeral loopback port and start accepting callbacks.
    pub async fn spawn() -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("failed to bind callback server: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("failed to read callback port: {e}"))?
            .port();

        // A watch rather than a Notify: the loop is normally parked in
        // `accept()` when the stop arrives, and `notify_waiters` only wakes
        // waiters registered at that instant — the signal would be dropped and
        // the listener would outlive its own shutdown. A watch is
        // level-triggered, so a stop set at any point is still seen.
        let (stop, stop_rx) = tokio::sync::watch::channel(false);
        let retired = Arc::new(tokio::sync::Notify::new());
        let signalled = Arc::clone(&retired);
        tokio::spawn(async move {
            serve_callbacks(listener, stop_rx).await;
            signalled.notify_waiters();
        });

        Ok(Self {
            port,
            stop,
            retired,
        })
    }

    /// The port the platform must redirect the browser to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The redirect URI to hand the platform as `redirect_uri`.
    pub fn callback_url(&self) -> String {
        callback_url(self.port)
    }

    /// Retire the listener and drop every pending session.
    ///
    /// A callback cannot arrive once the socket is closed, and each session
    /// holds an X25519 private key, so the sessions must not outlive it.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(5), self.retired.notified()).await;
        clear_all_sessions();
    }
}

/// The process-wide listener, started on demand by [`start_proxy`].
static MIMO_PROXY: Lazy<tokio::sync::Mutex<Option<MimoProxy>>> =
    Lazy::new(|| tokio::sync::Mutex::new(None));

/// Start the loopback callback listener, or return the port of the running one.
///
/// The listener is a singleton, matching the reference: a second `authorize`
/// for MiMo reuses the same callback URL rather than orphaning a port.
pub async fn start_proxy() -> Result<u16, String> {
    let mut slot = MIMO_PROXY.lock().await;
    if let Some(running) = slot.as_ref() {
        return Ok(running.port());
    }
    let proxy = MimoProxy::spawn().await?;
    let port = proxy.port();
    *slot = Some(proxy);
    Ok(port)
}

/// Stop the process-wide listener, if one is running.
pub async fn stop_proxy() {
    let running = MIMO_PROXY.lock().await.take();
    match running {
        Some(proxy) => proxy.stop().await,
        None => clear_all_sessions(),
    }
}

/// The redirect URI the platform must send the browser back to.
pub fn callback_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}{MIMO_CALLBACK_PATH}")
}

/// Accept callbacks until the handshake budget expires or the proxy is stopped.
async fn serve_callbacks(listener: TcpListener, mut stop: ProxyStop) {
    let budget = tokio::time::sleep(std::time::Duration::from_millis(MIMO_OAUTH_TIMEOUT_MS));
    tokio::pin!(budget);

    loop {
        // Checked before parking, so a stop that arrived while the previous
        // iteration was serving a request is not missed.
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            _ = &mut budget => break,
            _ = stop.changed() => break,
            accepted = listener.accept() => {
                let Ok((mut socket, _)) = accepted else { break };
                let mut buf = vec![0u8; 16 * 1024];
                let read = match socket.read(&mut buf).await {
                    Ok(read) => read,
                    Err(_) => continue,
                };
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let response = handle_callback_request(&request);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            }
        }
    }
}

/// The redirect target of a platform navigation carries no `Origin`; a
/// cross-site `fetch` does. Anything but loopback is refused.
fn is_loopback_origin(origin: Option<&str>) -> bool {
    let Some(origin) = origin.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    let Some(rest) = origin.strip_prefix("http://") else {
        return false;
    };
    let Some((host, port)) = rest.split_once(':') else {
        return matches!(rest, "127.0.0.1" | "localhost");
    };
    matches!(host, "127.0.0.1" | "localhost")
        && !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
}

/// Handle one callback request and render the page the user sees.
fn handle_callback_request(request: &str) -> String {
    let request_line = request.lines().next().unwrap_or_default();
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let origin = request.lines().find_map(|line| {
        line.split_once(':')
            .filter(|(name, _)| name.trim().eq_ignore_ascii_case("origin"))
            .map(|(_, value)| value.trim())
    });

    if !is_loopback_origin(origin) {
        return http_response(403, "text/plain", "Forbidden");
    }

    let Some(encrypted) = url::Url::parse(&format!("http://127.0.0.1{path}"))
        .ok()
        .and_then(|parsed| {
            parsed
                .query_pairs()
                .find(|(key, _)| key == "u")
                .map(|(_, value)| value.into_owned())
        })
        .filter(|value| !value.is_empty())
    else {
        return result_page(false, "Missing encrypted payload (u parameter).");
    };

    // The callback URL carries no state, so the matching session is found by
    // trying each pending key in turn. Exactly one can authenticate the tag.
    let mut sessions = MIMO_SESSIONS.lock();
    let mut matched: Option<(String, MimoCallback)> = None;
    for (state_value, session) in sessions.iter() {
        if session.status != MimoSessionStatus::Pending {
            continue;
        }
        if let Ok(callback) = decrypt_callback(&session.secret, &encrypted) {
            matched = Some((state_value.clone(), callback));
            break;
        }
    }

    let Some((state_value, callback)) = matched else {
        let message = "Could not decrypt with any pending session key";
        for session in sessions.values_mut() {
            if session.status == MimoSessionStatus::Pending {
                session.status = MimoSessionStatus::Error;
                session.error = Some(message.to_string());
            }
        }
        return result_page(false, &format!("Decryption failed: {message}"));
    };

    // A payload without a session key decrypts fine but is unusable, so it is
    // a failure rather than a half-linked account.
    let Some(access_token) = callback.sk.clone().filter(|sk| !sk.is_empty()) else {
        let message = "Decrypted payload missing sk (API key)";
        for session in sessions.values_mut() {
            if session.status == MimoSessionStatus::Pending {
                session.status = MimoSessionStatus::Error;
                session.error = Some(message.to_string());
            }
        }
        return result_page(false, &format!("Decryption failed: {message}"));
    };

    if let Some(session) = sessions.get_mut(&state_value) {
        session.status = MimoSessionStatus::Done;
        session.result = Some(MimoLinkedAccount {
            uid: callback.uid,
            access_token,
            base_url: callback.url,
        });
    }
    result_page(true, "Xiaomi account linked. You can close this tab.")
}

fn http_response(status: u16, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// The page the browser lands on. Auto-closes on success, as the reference's
/// does, so a user who opened the authorize URL in a popup lands back where
/// they started.
fn result_page(success: bool, message: &str) -> String {
    let color = if success { "#22c55e" } else { "#ef4444" };
    let icon = if success { "&#10003;" } else { "&#10007;" };
    let title = if success {
        "Authentication Successful"
    } else {
        "Authentication Failed"
    };
    let auto_close = if success {
        "<script>setTimeout(() => window.close(), 3000);</script>"
    } else {
        ""
    };
    let body = format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>{title}</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;
height:100vh;margin:0;background:#fafafa}}.icon{{color:{color};font-size:3rem}}
h1{{font-size:1.25rem;margin:1rem 0}}p{{color:#666;font-size:0.875rem}}</style></head>
<body><div style="text-align:center"><div class="icon">{icon}</div>
<h1>{title}</h1><p>{message}</p>{auto_close}</div></body></html>"#
    );
    http_response(
        if success { 200 } else { 400 },
        "text/html; charset=utf-8",
        &body,
    )
}

#[cfg(test)]
mod proxy_tests {
    use super::tests_support::seal_for;
    use super::tests_support::serialise;
    use super::*;

    /// Percent-encode a callback payload the way the platform does.
    ///
    /// Raw base64 in a query string is wrong: `+` decodes as a space, so an
    /// unencoded payload would be corrupted before the proxy ever saw it.
    fn encoded_callback(plaintext: &[u8], keypair: &MimoKeyPair) -> String {
        let sealed = seal_for(keypair, plaintext);
        sealed
            .bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (byte as char).to_string()
                }
                _ => format!("%{byte:02X}"),
            })
            .collect()
    }

    /// A running proxy, and a key registered against `state`.
    async fn proxy_with_session(state_value: &str) -> (MimoProxy, MimoKeyPair) {
        let proxy = MimoProxy::spawn().await.expect("proxy starts");
        let port = proxy.port();
        let pair = generate_keypair();
        assert!(
            register_session(state_value, pair.secret().clone()),
            "the handshake key registers under the flow's state"
        );
        (proxy, pair)
    }

    #[test]
    fn registering_a_session_never_exposes_the_private_key() {
        let _serialised = serialise();
        clear_all_sessions();
        assert!(register_session("st-1", generate_keypair().into_secret()));

        let view = session_status("st-1").expect("the session is known");
        assert_eq!(view.status, MimoSessionStatus::Pending);
        assert!(view.result.is_none());
        assert!(view.error.is_none());
        assert!(
            !serde_json::to_string(&view)
                .expect("view serialises")
                .contains("private"),
            "the status payload must not mention the private key"
        );

        assert!(
            session_status("nope").is_none(),
            "an unknown state is not a session"
        );
        clear_session("st-1");
        assert!(
            session_status("st-1").is_none(),
            "cleared sessions are forgotten"
        );
    }

    #[test]
    fn a_blank_state_is_refused() {
        let _serialised = serialise();
        clear_all_sessions();
        assert!(!register_session("", generate_keypair().into_secret()));
        assert!(!register_session("   ", generate_keypair().into_secret()));
    }

    /// THE PORT, END TO END. A real browser round trip: start the proxy,
    /// redirect the platform's callback at it, and require the session to
    /// become `done` carrying the account.
    #[tokio::test]
    async fn a_callback_for_the_registered_key_completes_the_session() {
        let _serialised = serialise();
        let (proxy, pair) = proxy_with_session("st-happy").await;
        let port = proxy.port();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");

        let u = encoded_callback(
            br#"{"uid":"u-77","sk":"sk-live","url":"https://api.xiaomimimo.com/v1"}"#,
            &pair,
        );
        let response = client
            .get(format!("http://127.0.0.1:{port}/?u={u}"))
            .send()
            .await
            .expect("proxy answers");

        assert_eq!(
            response.status(),
            200,
            "a decrypted callback is a success page"
        );
        let page = response.text().await.expect("page body");
        assert!(page.contains("Authentication Successful"));

        let view = session_status("st-happy").expect("session survives the callback");
        assert_eq!(view.status, MimoSessionStatus::Done);
        let account = view
            .result
            .expect("a completed session carries the account");
        assert_eq!(account.uid.as_deref(), Some("u-77"));
        assert_eq!(account.access_token, "sk-live");
        assert_eq!(account.base_url, "https://api.xiaomimimo.com/v1");
        proxy.stop().await;
    }

    /// The callback carries no state, so the proxy tries every pending key.
    /// With two sessions in flight, the right one must be the one that
    /// completes — and the other must be left pending, not failed.
    #[tokio::test]
    async fn the_matching_session_is_found_among_several_pending_ones() {
        let _serialised = serialise();
        let (proxy, pair) = proxy_with_session("st-second").await;
        let port = proxy.port();
        let other = generate_keypair();
        assert!(register_session("st-first", other.into_secret()));

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        let u = encoded_callback(br#"{"uid":"u-2","sk":"sk-second"}"#, &pair);
        let response = client
            .get(format!("http://127.0.0.1:{port}/?u={u}"))
            .send()
            .await
            .expect("proxy answers");
        assert_eq!(response.status(), 200);

        assert_eq!(
            session_status("st-second").expect("second").status,
            MimoSessionStatus::Done
        );
        assert_eq!(
            session_status("st-first").expect("first").status,
            MimoSessionStatus::Pending,
            "an unrelated pending session must be untouched"
        );
        proxy.stop().await;
    }

    /// A callback nobody can decrypt must fail closed and mark every pending
    /// session as errored, so the dashboard does not wait on a flow that is
    /// never going to arrive.
    #[tokio::test]
    async fn an_undecryptable_callback_fails_every_pending_session() {
        let _serialised = serialise();
        let (proxy, _pair) = proxy_with_session("st-bad").await;
        let port = proxy.port();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");

        let u = encoded_callback(br#"{"uid":"u-9","sk":"sk-other"}"#, &generate_keypair());
        let response = client
            .get(format!("http://127.0.0.1:{port}/?u={u}"))
            .send()
            .await
            .expect("proxy answers");

        assert_eq!(
            response.status(),
            400,
            "an unauthenticated callback is a failure page"
        );
        let view = session_status("st-bad").expect("session still known");
        assert_eq!(view.status, MimoSessionStatus::Error);
        assert!(
            view.result.is_none(),
            "a failed session must not carry an account"
        );
        assert!(view.error.expect("a reason").contains("decrypt"));
        proxy.stop().await;
    }

    /// A payload that decrypts but carries no `sk` is unusable, and must not
    /// be recorded as a linked account.
    #[tokio::test]
    async fn a_callback_without_a_session_key_fails_rather_than_half_links() {
        let _serialised = serialise();
        let (proxy, pair) = proxy_with_session("st-nokey").await;
        let port = proxy.port();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");

        let u = encoded_callback(br#"{"uid":"u-1"}"#, &pair);
        let response = client
            .get(format!("http://127.0.0.1:{port}/?u={u}"))
            .send()
            .await
            .expect("proxy answers");

        assert_eq!(response.status(), 400);
        let view = session_status("st-nokey").expect("session still known");
        assert_eq!(view.status, MimoSessionStatus::Error);
        assert!(view.result.is_none());
        assert!(view.error.expect("a reason").contains("missing sk"));
        proxy.stop().await;
    }

    /// A callback with no `u` is a misdirected request, not a failed
    /// handshake — and it must not fail the sessions, which may still be
    /// waiting for the real callback.
    #[tokio::test]
    async fn a_callback_without_the_payload_leaves_sessions_pending() {
        let _serialised = serialise();
        let (proxy, _pair) = proxy_with_session("st-nou").await;
        let port = proxy.port();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");

        let response = client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect("proxy answers");
        assert_eq!(response.status(), 400);
        assert_eq!(
            session_status("st-nou")
                .expect("session still known")
                .status,
            MimoSessionStatus::Pending,
            "a request with no payload says nothing about the handshake"
        );
        proxy.stop().await;
    }

    /// The listener is loopback-only by construction, but a page on another
    /// origin must still be refused: that is what stops a malicious site
    /// reading the decrypted account out of the response.
    #[tokio::test]
    async fn a_cross_origin_callback_is_refused() {
        let _serialised = serialise();
        let (proxy, pair) = proxy_with_session("st-origin").await;
        let port = proxy.port();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");

        let u = encoded_callback(br#"{"uid":"u-1","sk":"sk-1"}"#, &pair);
        let response = client
            .get(format!("http://127.0.0.1:{port}/?u={u}"))
            .header("Origin", "http://evil.example")
            .send()
            .await
            .expect("proxy answers");

        assert_eq!(response.status(), 403);
        assert_eq!(
            session_status("st-origin")
                .expect("session still known")
                .status,
            MimoSessionStatus::Pending,
            "a refused request must not consume the handshake"
        );
        proxy.stop().await;
    }

    /// The origin guard accepts the shapes a real navigation produces and
    /// refuses everything else.
    #[test]
    fn the_origin_guard_admits_only_loopback_or_absent() {
        for allowed in [
            None,
            Some(""),
            Some("   "),
            Some("http://127.0.0.1"),
            Some("http://127.0.0.1:56121"),
            Some("http://localhost"),
            Some("http://localhost:8080"),
        ] {
            assert!(
                is_loopback_origin(allowed),
                "{allowed:?} is a loopback origin and must be allowed"
            );
        }
        for refused in [
            Some("http://evil.example"),
            Some("https://127.0.0.1"),
            Some("http://127.0.0.1.evil.example"),
            Some("http://127.0.0.1:notaport"),
            Some("http://localhost:80/path"),
            Some("null"),
        ] {
            assert!(!is_loopback_origin(refused), "{refused:?} must be refused");
        }
    }

    /// A second start must reuse the running listener rather than orphan a
    /// port — the reference makes the proxy a singleton for the same reason, so
    /// a second `authorize` reuses the callback URL the first one was given.
    #[tokio::test]
    async fn the_process_proxy_is_a_singleton() {
        let _serialised = serialise();
        stop_proxy().await;
        let first = start_proxy().await.expect("proxy starts");
        let second = start_proxy().await.expect("proxy is reusable");
        assert_eq!(first, second, "a second start must not bind a second port");
        stop_proxy().await;
    }

    /// Stopping must clear the port so the next start binds fresh, and must
    /// drop the pending private keys with it.
    #[tokio::test]
    async fn stopping_clears_the_port_and_the_pending_keys() {
        let _serialised = serialise();
        stop_proxy().await;
        let first = start_proxy().await.expect("proxy starts");
        assert!(register_session(
            "st-stop",
            generate_keypair().into_secret()
        ));

        stop_proxy().await;

        assert!(
            session_status("st-stop").is_none(),
            "pending keys do not outlive the listener"
        );
        let second = start_proxy().await.expect("proxy restarts");
        assert_ne!(first, second, "a restarted proxy binds a new port");
        stop_proxy().await;
    }

    /// Stopping must actually retire the listener. Clearing the port alone
    /// would leave the socket accepting, so a sign-in that began before the
    /// stop would still be able to complete afterwards.
    #[tokio::test]
    async fn stopping_retires_the_listener_not_just_the_port() {
        let _serialised = serialise();
        let proxy = MimoProxy::spawn().await.expect("proxy starts");
        let port = proxy.port();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");

        assert!(
            client
                .get(format!("http://127.0.0.1:{port}/"))
                .send()
                .await
                .is_ok(),
            "a running proxy answers"
        );

        proxy.stop().await;
        assert!(
            client
                .get(format!("http://127.0.0.1:{port}/"))
                .send()
                .await
                .is_err(),
            "a stopped proxy must refuse connections, not just forget its port"
        );
    }

    /// The callback URL is the loopback root, which is what gets handed to the
    /// platform as `redirect_uri` — a path other than `/` would never match.
    #[test]
    fn the_callback_url_is_the_loopback_root() {
        assert_eq!(callback_url(56121), "http://127.0.0.1:56121/");
    }
}
