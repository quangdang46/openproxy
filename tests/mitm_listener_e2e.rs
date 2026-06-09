//! End-to-end test for the MITM proxy's TLS interception path.
//!
//! Tests the full flow: CA generation → listener start → CONNECT tunnel
//! → TLS handshake → HTTP request round-trip → proxy shutdown.
//!
//! Uses a local plain-TCP upstream (no upstream TLS needed — the proxy
//! itself terminates the client-side TLS and relays decrypted bytes).

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// Spawn a minimal upstream TCP server that writes a canned HTTP response
/// once it receives any data (simulating a plain-HTTP backend behind the MITM).
async fn spawn_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        while let Ok((mut stream, _peer)) = listener.accept().await {
            // Read any request data
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;

            // Write a canned HTTP response
            let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nhello\n";
            let _ = stream.write_all(resp).await;
            let _ = stream.flush().await;
        }
    });
    addr
}

/// Build a rustls client config that trusts the MITM CA cert (so our test
/// client can complete the TLS handshake with the MITM proxy).
fn tls_client_config(ca_cert_pem: &[u8]) -> Arc<rustls::ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    let certs: Vec<rustls::pki_types::CertificateDer> =
        rustls_pemfile::certs(&mut &ca_cert_pem[..])
            .collect::<Result<Vec<_>, _>>()
            .expect("valid CA cert PEM");
    for cert in certs {
        root_store.add(cert).expect("CA cert added to root store");
    }
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth(),
    )
}

#[tokio::test]
async fn mitm_tls_e2e_round_trip() {
    // ── 1. Temp workspace ──
    let tmp = tempfile::tempdir().expect("temp dir");
    let capture_dir = tmp.path().join("captures");

    // ── 2. Generate CA ──
    let ca = openproxy::core::mitm::cert::generate_ca().expect("CA generation");
    let ca_cert = Arc::new(ca.cert);
    let ca_key = Arc::new(ca.key);
    let ca_cert_pem = ca.cert_pem.as_bytes().to_vec();
    let ca_key_pem = ca.key_pem.as_bytes().to_vec();

    // ── 3. Boot app state ──
    let (_router, state) = common::boot_test_app().await;

    // ── 4. Start MITM proxy ──
    let mut handle = openproxy::core::mitm::server::start_mitm_proxy(
        openproxy::core::mitm::server::MitmProxyConfig {
            ca_cert: ca_cert.clone(),
            ca_key: ca_key.clone(),
            ca_cert_pem: ca_cert_pem.clone(),
            ca_key_pem: ca_key_pem.clone(),
            capture_dir: capture_dir.clone(),
            state: state.clone(),
        },
    )
    .await
    .expect("MITM proxy start");
    let proxy_port = handle.port();

    // ── 5. Start upstream ──
    let upstream_addr = spawn_upstream().await;
    let upstream_host = "127.0.0.1";
    let upstream_port = upstream_addr.port();

    // ── 6. Connect to MITM proxy ──
    let mut proxy_stream = timeout(Duration::from_secs(5), async {
        TcpStream::connect((upstream_host, proxy_port)).await
    })
    .await
    .expect("connect to MITM proxy")
    .expect("TCP connect to proxy");

    // ── 7. Send CONNECT to upstream ──
    let connect_cmd = format!("CONNECT {upstream_host}:{upstream_port} HTTP/1.1\r\nHost: {upstream_host}:{upstream_port}\r\n\r\n");
    proxy_stream
        .write_all(connect_cmd.as_bytes())
        .await
        .expect("write CONNECT");

    // ── 8. Receive "200 Connection Established" ──
    let mut buf = [0u8; 256];
    let n = timeout(Duration::from_secs(5), proxy_stream.read(&mut buf))
        .await
        .expect("read response timeout")
        .expect("read response");
    let response_head = String::from_utf8_lossy(&buf[..n]);
    assert!(
        response_head.contains("200 Connection Established"),
        "expected Connection Established, got: {response_head:?}"
    );

    // ── 9. Wrap in TLS using MITM's CA cert ──
    let tls_connector = tokio_rustls::TlsConnector::from(tls_client_config(&ca_cert_pem));
    let server_name =
        rustls::pki_types::ServerName::try_from("127.0.0.1").expect("valid ServerName");
    let mut tls_stream = timeout(
        Duration::from_secs(5),
        tls_connector.connect(server_name, proxy_stream),
    )
    .await
    .expect("TLS handshake timeout")
    .expect("TLS handshake");

    // ── 10. Send HTTP GET through the tunnel ──
    let get_req = b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    tls_stream.write_all(get_req).await.expect("write HTTP GET");
    tls_stream.flush().await.expect("flush");

    // ── 11. Read HTTP response ──
    let mut resp_buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match timeout(Duration::from_secs(5), tls_stream.read(&mut chunk)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => resp_buf.extend_from_slice(&chunk[..n]),
            _ => break,
        }
    }
    let response_body = String::from_utf8_lossy(&resp_buf);
    assert!(
        response_body.contains("hello"),
        "expected 'hello' in response, got: {response_body:?}"
    );

    // ── 12. Stop proxy ──
    handle.stop().await;

    // ── 13. Verify capture files were written ──
    let mut has_req = false;
    let mut has_resp = false;
    if let Ok(entries) = capture_dir.read_dir() {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("127.0.0.1") {
                if let Ok(files) = entry.path().read_dir() {
                    for file in files.flatten() {
                        let fname = file.file_name().to_string_lossy().to_string();
                        if fname.starts_with("req-") {
                            has_req = true;
                        }
                        if fname.starts_with("resp-") {
                            has_resp = true;
                        }
                    }
                }
            }
        }
    }
    assert!(has_req, "expected request capture file, found none");
    assert!(has_resp, "expected response capture file, found none");

    if let Ok(entries) = capture_dir.read_dir() {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("127.0.0.1") {
                if let Ok(files) = entry.path().read_dir() {
                    for file in files.flatten() {
                        let fname = file.file_name().to_string_lossy().to_string();
                        if fname.starts_with("req-") {
                            let content = std::fs::read_to_string(file.path())
                                .expect("read req capture file");
                            assert!(
                                content.contains("GET"),
                                "captured request should contain GET, got: {content:?}"
                            );
                            assert!(
                                content.contains("HTTP/1.1"),
                                "captured request should contain HTTP/1.1, got: {content:?}"
                            );
                        }
                        if fname.starts_with("resp-") {
                            let content = std::fs::read_to_string(file.path())
                                .expect("read resp capture file");
                            assert!(
                                content.contains("200 OK"),
                                "captured response should contain 200 OK, got: {content:?}"
                            );
                            assert!(
                                content.contains("hello"),
                                "captured response should contain hello, got: {content:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}
