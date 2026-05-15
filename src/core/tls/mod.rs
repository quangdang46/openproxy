// Global rustls crypto provider initialization.
use std::sync::Once;
static INIT_RUSTLS: Once = Once::new();

/// Install the ring crypto provider for rustls. Idempotent.
pub fn ensure_rustls_provider() {
    INIT_RUSTLS.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .unwrap();
    });
}
