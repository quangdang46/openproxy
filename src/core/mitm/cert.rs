use std::path::{Path, PathBuf};

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, IsCa, KeyPair,
    KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
};
use rustls_pemfile;
use sha2::{Digest, Sha256};

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

    let mut dn = DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, "OpenProxy MITM CA");
    dn.push(rcgen::DnType::OrganizationName, "OpenProxy");
    dn.push(rcgen::DnType::CountryName, "US");
    params.distinguished_name = dn;

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

pub fn generate_ca_persisted(
    ca_dir: &Path,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(ca_dir)?;

    let cert_path = ca_dir.join(CA_CERT_FILENAME);
    let key_path = ca_dir.join(CA_KEY_FILENAME);

    if !cert_path.exists() || !key_path.exists() {
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
    let mut params = CertificateParams::new(vec![hostname.to_string()])?;
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;

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
