use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rcgen::{Certificate, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use super::capture::CaptureSink;
use super::cert;
use crate::server::state::AppState;

pub struct MitmProxyHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    port: u16,
    bind_addr: String,
}

impl MitmProxyHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn bind_addr(&self) -> &str {
        &self.bind_addr
    }

    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for MitmProxyHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

pub struct MitmProxyConfig {
    pub ca_cert: Arc<Certificate>,
    pub ca_key: Arc<KeyPair>,
    pub ca_cert_pem: Vec<u8>,
    pub ca_key_pem: Vec<u8>,
    pub capture_dir: PathBuf,
    pub state: AppState,
}

pub async fn start_mitm_proxy(
    config: MitmProxyConfig,
) -> Result<MitmProxyHandle, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_addr = listener.local_addr()?;
    let port = local_addr.port();
    let bind_addr = format!("127.0.0.1:{port}");

    info!(addr = %bind_addr, "MITM proxy listening");

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let capture = CaptureSink::new(config.capture_dir.clone());
    let ca_cert = config.ca_cert.clone();
    let ca_key = config.ca_key.clone();
    let state = config.state.clone();
    // Keep PEM data available for per-host leaf cert generation
    // (loaded on demand from disk via the persisted CA files)

    tokio::spawn(async move {
        tokio::select! {
            _ = &mut shutdown_rx => {
                info!("MITM proxy shutdown signal received");
            }
            _ = accept_loop(listener, ca_cert, ca_key, capture, state) => {
                info!("MITM proxy accept loop exited");
            }
        }
    });

    Ok(MitmProxyHandle {
        shutdown_tx: Some(shutdown_tx),
        port,
        bind_addr,
    })
}

async fn accept_loop(
    listener: TcpListener,
    ca_cert: Arc<Certificate>,
    ca_key: Arc<KeyPair>,
    capture: CaptureSink,
    state: AppState,
) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                debug!(peer = %peer, "MITM proxy accepted connection");
                let ca_cert = ca_cert.clone();
                let ca_key = ca_key.clone();
                let capture = capture.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, ca_cert, ca_key, capture, state).await {
                        warn!(error = %e, "MITM client handler failed");
                    }
                });
            }
            Err(e) => {
                warn!(error = %e, "MITM proxy accept failed");
            }
        }
    }
}

async fn handle_client(
    client_stream: TcpStream,
    ca_cert: Arc<Certificate>,
    ca_key: Arc<KeyPair>,
    capture: CaptureSink,
    _state: AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let (method, target) = {
        let buf = peek_first_line(&client_stream).await?;
        match buf {
            Some(line) => parse_request_line(&line),
            None => return Ok(()),
        }
    };

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = match parse_host_port(&target) {
            Some(parts) => parts,
            None => {
                let mut stream = client_stream;
                let _ = stream
                    .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                    .await;
                return Ok(());
            }
        };
        handle_connect(client_stream, host, port, ca_cert, ca_key, capture).await
    } else {
        let mut stream = client_stream;
        let _ = stream
            .write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await;
        Ok(())
    }
}

/// Peek at the first line (request line) from a TcpStream without consuming
/// any bytes, so the stream can later be handed to the TLS acceptor.
async fn peek_first_line(
    stream: &TcpStream,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut buf = vec![0u8; 4096];
    let n = stream.peek(&mut buf).await?;
    if n == 0 {
        return Ok(None);
    }
    let end = buf[..n]
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(n);
    if end == 0 {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&buf[..end])
        .trim_end_matches('\r')
        .to_string();
    if line.is_empty() {
        return Ok(None);
    }
    Ok(Some(line))
}

async fn handle_connect(
    mut client_stream: TcpStream,
    host: String,
    port: u16,
    ca_cert: Arc<Certificate>,
    ca_key: Arc<KeyPair>,
    capture: CaptureSink,
) -> Result<(), Box<dyn std::error::Error>> {
    // Drain remaining CONNECT headers (everything after the request line)
    let mut byte = [0u8; 1];
    let mut prev_was_nl = false;
    loop {
        let n = client_stream.read(&mut byte).await?;
        if n == 0 {
            return Ok(());
        }
        if byte[0] == b'\n' {
            if prev_was_nl {
                break; // blank line ends CONNECT headers
            }
            prev_was_nl = true;
        } else if byte[0] != b'\r' {
            prev_was_nl = false;
        }
    }

    // Connect to the upstream server
    let upstream = match TcpStream::connect((host.as_str(), port)).await {
        Ok(stream) => stream,
        Err(e) => {
            warn!(host = %host, port, error = %e, "MITM upstream connect failed");
            let body = format!("MITM upstream connect failed: {e}");
            let response = format!(
                "HTTP/1.1 502 Bad Gateway\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = client_stream.write_all(response.as_bytes()).await;
            return Ok(());
        }
    };

    // Tell the client the CONNECT tunnel is established
    let established = b"HTTP/1.1 200 Connection Established\r\n\r\n";
    if let Err(e) = client_stream.write_all(established).await {
        warn!(error = %e, "Failed to send 200 Connection Established to client");
        return Ok(());
    }

    // Perform TLS accept on the client side using a forged leaf cert
    // for the target hostname, signed by the MITM CA.
    let acceptor = match cert::build_tls_acceptor(&ca_cert, &ca_key, &host) {
        Ok(a) => a,
        Err(e) => {
            warn!(host = %host, error = %e, "Failed to build TLS acceptor");
            return Ok(());
        }
    };

    let tls_stream = match acceptor.accept(client_stream).await {
        Ok(s) => s,
        Err(e) => {
            warn!(host = %host, error = %e, "TLS accept failed (client may not expect MITM)");
            return Ok(());
        }
    };

    // Split the TLS stream and upstream into read/write halves for pumping
    let (mut tls_read, mut tls_write) = tokio::io::split(tls_stream);
    let (mut up_read, mut up_write) = upstream.into_split();

    let timestamp = current_timestamp();
    let host_a = host.clone();
    let cap_a = capture.clone();
    let ts_a = timestamp.clone();

    let c_to_u = tokio::spawn(async move {
        pump_captured(
            &mut tls_read,
            &mut up_write,
            &cap_a,
            &host_a,
            "req",
            &ts_a,
        )
        .await
    });

    let u_to_c = tokio::spawn(async move {
        pump_captured(
            &mut up_read,
            &mut tls_write,
            &capture,
            &host,
            "resp",
            &timestamp,
        )
        .await
    });

    let _ = tokio::join!(c_to_u, u_to_c);
    Ok(())
}

async fn pump_captured<R, W>(
    reader: &mut R,
    writer: &mut W,
    capture: &CaptureSink,
    host: &str,
    direction: &str,
    timestamp: &str,
) -> std::io::Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(_) => return Ok(()),
        };
        if let Err(e) = writer.write_all(&buf[..n]).await {
            debug!(error = %e, direction, "MITM pump write failed");
            return Ok(());
        }
        let _ = writer.flush().await;

        if direction == "req" {
            let _ = capture.capture_request(host, timestamp, &buf[..n]).await;
        } else {
            let _ = capture.capture_response(host, timestamp, &buf[..n]).await;
        }
    }
}

fn parse_request_line(line: &str) -> (String, String) {
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    (method, target)
}

fn parse_host_port(target: &str) -> Option<(String, u16)> {
    // Handle IPv6 bracket notation: [::1]:443 or [::1]
    if let Some(rest) = target.strip_prefix('[') {
        let (host, port_str) = rest.split_once(']')?;
        let port = if let Some(p) = port_str.strip_prefix(':') {
            p.parse::<u16>().ok()?
        } else if port_str.is_empty() {
            443u16
        } else {
            return None;
        };
        return Some((format!("[{}]", host), port));
    }
    // Standard host:port or bare host
    match target.rsplit_once(':') {
        Some((h, p)) => Some((h.to_string(), p.parse::<u16>().ok()?)),
        None => Some((target.to_string(), 443u16)),
    }
}

fn current_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{now}")
}
