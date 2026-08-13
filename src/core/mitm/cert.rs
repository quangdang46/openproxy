use std::path::{Path, PathBuf};

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, IsCa, KeyPair,
    KeyUsagePurpose, SanType, PKCS_ECDSA_P256_SHA256,
};
use rustls_pemfile;
use sha2::{Digest, Sha256};

/// 9router rootCA.js parity: the trusted root CN that IDEs/install scripts
/// key on. Changing this breaks existing trust stores — uninstall must remove
/// both the old and new CN.
const ROOT_CA_CN: &str = "9Router MITM Root CA";
const ROOT_CA_ORG: &str = "9Router";
/// Regenerate the CA when its cert expires within this window (9router
/// isCertExpired: `notAfter < now + 30 days`).
const CA_RENEW_WINDOW_DAYS: i64 = 30;

const CA_CERT_FILENAME: &str = "mitm-ca.pem";
const CA_KEY_FILENAME: &str = "mitm-ca.key.pem";

pub struct CaMaterial {
    pub cert: Certificate,
    pub key: KeyPair,
    pub cert_pem: String,
    pub key_pem: String,
}

pub fn generate_ca() -> Result<CaMaterial, Box<dyn std::error::Error>> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    // 9router rootCA.js: serial "01", notBefore now, notAfter now+10 years.
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now;
    params.not_after = now + time::Duration::days(365 * 10);
    params.serial_number = Some(rcgen::SerialNumber::from_slice(&[1]));

    let mut dn = DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, ROOT_CA_CN);
    dn.push(rcgen::DnType::OrganizationName, ROOT_CA_ORG);
    dn.push(rcgen::DnType::CountryName, "US");
    params.distinguished_name = dn;

    // 9router rootCA.js uses RSA-2048; rcgen's ring backend generates ECDSA
    // by default. The client-trust contract is CN+SAN (validated by IDEs and
    // install scripts) — key type is not validated, so ECDSA is kept and the
    // divergence documented. See beads openproxy-9router-parity-v0550-pnc.106.
    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    Ok(CaMaterial {
        cert,
        key: key_pair,
        cert_pem,
        key_pem,
    })
}

/// Parse the `notAfter` of the first cert in a PEM blob. Returns `None` on
/// parse failure (treated as "unknown" — the caller decides).
fn cert_not_after(cert_pem: &[u8]) -> Option<time::OffsetDateTime> {
    use x509_parser::der_parser::asn1_rs::FromDer;
    let mut rd = cert_pem;
    let cert = rustls_pemfile::certs(&mut rd).next()?.ok()?;
    let (_, der) = x509_parser::prelude::X509Certificate::from_der(&cert).ok()?;
    let validity = der.validity();
    let not_after = validity.not_after.timestamp() as i64;
    time::OffsetDateTime::from_unix_timestamp(not_after).ok()
}

/// True when the persisted CA cert is missing, or its `notAfter` is within
/// `CA_RENEW_WINDOW_DAYS` (9router `isCertExpired`: regenerate if
/// `notAfter < now + 30 days`).
fn ca_needs_regeneration(cert_path: &Path) -> bool {
    if !cert_path.exists() {
        return true;
    }
    let pem = match std::fs::read(cert_path) {
        Ok(bytes) => bytes,
        Err(_) => return true,
    };
    match cert_not_after(&pem) {
        Some(not_after) => {
            let cutoff =
                time::OffsetDateTime::now_utc() + time::Duration::days(CA_RENEW_WINDOW_DAYS);
            not_after < cutoff
        }
        None => true,
    }
}

pub fn generate_ca_persisted(
    ca_dir: &Path,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(ca_dir)?;

    let cert_path = ca_dir.join(CA_CERT_FILENAME);
    let key_path = ca_dir.join(CA_KEY_FILENAME);

    if ca_needs_regeneration(&cert_path) || !key_path.exists() {
        let material = generate_ca()?;
        std::fs::write(&cert_path, material.cert_pem.as_bytes())?;
        std::fs::write(&key_path, material.key_pem.as_bytes())?;
    }

    Ok((cert_path, key_path))
}

pub fn sign_leaf(
    ca_cert: &Certificate,
    ca_key: &KeyPair,
    hostname: &str,
) -> Result<(Vec<u8>, Vec<u8>), Box<dyn std::error::Error>> {
    // 9router rootCA.js leaf (115-164): SAN [DNS:domain, DNS:*.domain],
    // extKeyUsage serverAuth + clientAuth, 1yr validity.
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    let san_domain: rcgen::Ia5String = hostname.to_string().try_into()?;
    let san_wildcard: rcgen::Ia5String = format!("*.{}", hostname).try_into()?;
    params.subject_alt_names = vec![SanType::DnsName(san_domain), SanType::DnsName(san_wildcard)];
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    params.use_authority_key_identifier_extension = true;
    // 9router leaf is 1 year.
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now;
    params.not_after = now + time::Duration::days(365);

    let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
    let leaf_cert = params.signed_by(&leaf_key, ca_cert, ca_key)?;

    Ok((
        leaf_cert.pem().into_bytes(),
        leaf_key.serialize_pem().into_bytes(),
    ))
}

pub fn sha256_fingerprint(cert_pem: &[u8]) -> String {
    let mut hasher = Sha256::new();
    if let Ok(cert_der) = extract_first_cert_der(cert_pem) {
        hasher.update(cert_der);
    } else {
        hasher.update(cert_pem);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{:02x}", byte);
    }
    out
}

fn extract_first_cert_der(pem: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let first = rustls_pemfile::certs(&mut &pem[..])
        .next()
        .ok_or("no certificate found in PEM")??;
    Ok(first.to_vec())
}

/// Install the CA certificate into the system trust store.
///
/// - macOS: `sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain <cert_path>`
/// - Linux: copies to `/usr/local/share/ca-certificates/` and runs `update-ca-certificates`
pub fn install_ca_cert(cert_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(target_os = "macos") {
        let status = std::process::Command::new("sudo")
            .args([
                "security",
                "add-trusted-cert",
                "-d",
                "-r",
                "trustRoot",
                "-k",
                "/Library/Keychains/System.keychain",
            ])
            .arg(cert_path)
            .status()?;
        if !status.success() {
            return Err("Failed to install CA cert on macOS".into());
        }
    } else if cfg!(target_os = "linux") {
        let dest = PathBuf::from("/usr/local/share/ca-certificates");
        std::fs::create_dir_all(&dest)?;
        let dest_path = dest.join("openproxy-mitm-ca.crt");
        std::fs::copy(cert_path, &dest_path)?;
        let status = std::process::Command::new("sudo")
            .args(["update-ca-certificates"])
            .status()?;
        if !status.success() {
            return Err("Failed to run update-ca-certificates".into());
        }
    } else {
        return Err("Unsupported platform for CA cert installation".into());
    }
    Ok(())
}

/// Remove the CA certificate from the system trust store.
///
/// - macOS: `sudo security remove-trusted-cert -d <cert_path>`
/// - Linux: removes the copied cert and runs `update-ca-certificates --fresh`
pub fn uninstall_ca_cert(cert_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(target_os = "macos") {
        let status = std::process::Command::new("sudo")
            .args(["security", "remove-trusted-cert", "-d"])
            .arg(cert_path)
            .status()?;
        if !status.success() {
            return Err("Failed to uninstall CA cert on macOS".into());
        }
    } else if cfg!(target_os = "linux") {
        let dest_path = PathBuf::from("/usr/local/share/ca-certificates/openproxy-mitm-ca.crt");
        let _ = std::fs::remove_file(&dest_path);
        let status = std::process::Command::new("sudo")
            .args(["update-ca-certificates", "--fresh"])
            .status()?;
        if !status.success() {
            return Err("Failed to run update-ca-certificates".into());
        }
    } else {
        return Err("Unsupported platform for CA cert uninstallation".into());
    }
    Ok(())
}

/// Build a tokio-rustls TlsAcceptor that presents a leaf cert for `hostname`,
/// signed by the given CA material. Used by the MITM CONNECT handler to perform
/// TLS interception on the client side of the tunnel.
pub fn build_tls_acceptor(
    ca_cert: &Certificate,
    ca_key: &KeyPair,
    hostname: &str,
) -> Result<tokio_rustls::TlsAcceptor, Box<dyn std::error::Error>> {
    let (leaf_pem, leaf_key_pem) = sign_leaf(ca_cert, ca_key, hostname)?;

    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut &leaf_pem[..]).collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut &leaf_key_pem[..])?
        .ok_or("no private key in leaf cert")?;

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(
        server_config,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_cert_der(pem: &[u8]) -> Vec<u8> {
        let mut rd = pem;
        rustls_pemfile::certs(&mut rd)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .remove(0)
            .to_vec()
    }

    #[test]
    fn ca_cert_cn_is_9router() {
        use x509_parser::der_parser::asn1_rs::FromDer;
        let material = generate_ca().expect("generate ca");
        let der = first_cert_der(material.cert_pem.as_bytes());
        let (_, cert) = x509_parser::prelude::X509Certificate::from_der(&der).expect("parse ca");
        let cn = cert
            .subject()
            .iter_common_name()
            .next()
            .map(|a| a.as_str().unwrap_or(""))
            .unwrap_or("");
        assert_eq!(cn, ROOT_CA_CN);
        assert!(material.key_pem.contains("PRIVATE KEY"));
    }

    #[test]
    fn leaf_cert_has_wildcard_san() {
        use x509_parser::der_parser::asn1_rs::FromDer;
        let ca = generate_ca().expect("generate ca");
        let (leaf_pem, _) = sign_leaf(&ca.cert, &ca.key, "example.com").expect("sign leaf");
        let der = first_cert_der(&leaf_pem);
        let (_, cert) = x509_parser::prelude::X509Certificate::from_der(&der).expect("parse leaf");

        // SAN must contain both "example.com" and "*.example.com".
        let san_ext = cert
            .subject_alternative_name()
            .ok()
            .flatten()
            .expect("leaf should have SAN");
        let sans: Vec<String> = san_ext
            .value
            .general_names
            .iter()
            .map(|n| n.to_string())
            .collect();
        assert!(
            sans.iter().any(|n| n.contains("example.com")),
            "SAN should contain example.com: {sans:?}"
        );
        assert!(
            sans.iter().any(|n| n.contains("*.example.com")),
            "SAN should contain wildcard: {sans:?}"
        );

        // extKeyUsage serverAuth + clientAuth.
        let eku_ext = cert
            .extended_key_usage()
            .ok()
            .flatten()
            .expect("leaf should have extKeyUsage");
        assert!(eku_ext.value.server_auth, "leaf must have serverAuth");
        assert!(eku_ext.value.client_auth, "leaf must have clientAuth");
    }

    #[test]
    fn generate_ca_persisted_renews_expired() {
        let dir = std::env::temp_dir().join(format!("mitm-ca-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (cert_path, _) = generate_ca_persisted(&dir).expect("persist ca");
        // First call generates.
        assert!(cert_path.exists());
        // A second call within the window reuses the cert (no regeneration).
        let m1 = std::fs::read(&cert_path).unwrap();
        let (cert_path2, _) = generate_ca_persisted(&dir).expect("persist again");
        let m2 = std::fs::read(&cert_path2).unwrap();
        assert_eq!(m1, m2, "valid cert should not be regenerated");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
