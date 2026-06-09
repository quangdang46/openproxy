use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rcgen::Certificate;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use super::capture::CaptureSink;
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
    let state = config.state.clone();
    let _ = (config.ca_cert_pem, config.ca_key_pem);

    tokio::spawn(async move {
        tokio::select! {
            _ = &mut shutdown_rx => {
                info!("MITM proxy shutdown signal received");
            }
            _ = accept_loop(listener, ca_cert, capture, state) => {
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
    capture: CaptureSink,
    state: AppState,
) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                debug!(peer = %peer, "MITM proxy accepted connection");
                let ca_cert = ca_cert.clone();
                let capture = capture.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, ca_cert, capture, state).await {
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
    _ca_cert: Arc<Certificate>,
    capture: CaptureSink,
    _state: AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let (read_half, mut write_half) = client_stream.into_split();
    let mut reader = BufReader::new(read_half);

    let request_line = match read_request_line(&mut reader).await? {
        Some(line) => line,
        None => {
            debug!("MITM client closed connection before sending a request");
            return Ok(());
        }
    };

    let (method, target) = parse_request_line(&request_line);

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = match parse_host_port(&target) {
            Some(parts) => parts,
            None => {
                let _ = write_half
                    .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                    .await;
                return Ok(());
            }
        };
        handle_connect(reader, write_half, host, port, capture).await
    } else {
        handle_plain_http(&mut reader, &mut write_half, &method, &target, capture).await
    }
}

async fn handle_connect(
    mut reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    mut client_write: tokio::net::tcp::OwnedWriteHalf,
    host: String,
    port: u16,
    capture: CaptureSink,
) -> Result<(), Box<dyn std::error::Error>> {
    drain_headers(&mut reader).await?;

    let target = match TcpStream::connect((host.as_str(), port)).await {
        Ok(stream) => stream,
        Err(e) => {
            warn!(host = %host, port, error = %e, "MITM upstream connect failed");
            let body = format!("MITM upstream connect failed: {e}");
            let response = format!(
                "HTTP/1.1 502 Bad Gateway\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = client_write.write_all(response.as_bytes()).await;
            return Ok(());
        }
    };

    let established = b"HTTP/1.1 200 Connection Established\r\n\r\n";
    if let Err(e) = client_write.write_all(established).await {
        warn!(error = %e, "Failed to send 200 Connection Established to client");
        return Ok(());
    }

    let (mut target_read, mut target_write) = target.into_split();
    let mut client_read = reader.into_inner();

    let timestamp = current_timestamp();
    let cap_a = capture.clone();
    let host_a = host.clone();
    let ts_a = timestamp.clone();

    let c_to_t = tokio::spawn(async move {
        pump_stream(
            &mut client_read,
            &mut target_write,
            &cap_a,
            &host_a,
            "req",
            &ts_a,
        )
        .await
    });

    let t_to_c = tokio::spawn(async move {
        pump_stream(
            &mut target_read,
            &mut client_write,
            &capture,
            &host,
            "resp",
            &timestamp,
        )
        .await
    });

    let _ = tokio::join!(c_to_t, t_to_c);
    Ok(())
}

async fn pump_stream<R, W>(
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

async fn handle_plain_http<R, W>(
    reader: &mut R,
    writer: &mut W,
    method: &str,
    target: &str,
    capture: CaptureSink,
) -> Result<(), Box<dyn std::error::Error>>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut header_buf = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        header_buf.extend_from_slice(line.as_bytes());
    }

    let body = match parse_content_length(&header_buf) {
        Some(content_length) => {
            let mut buf = vec![0u8; content_length.min(64 * 1024)];
            tokio::io::AsyncReadExt::read_exact(reader, &mut buf).await?;
            buf
        }
        None => Vec::new(),
    };

    let timestamp = current_timestamp();
    let captured = format!(
        "{} {}\n{}\n",
        method,
        target,
        String::from_utf8_lossy(&header_buf)
    );
    let mut payload = captured.into_bytes();
    payload.extend_from_slice(&body);
    let _ = capture.capture_request(target, &timestamp, &payload).await;

    let response = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    writer.write_all(response).await?;
    writer.shutdown().await?;
    Ok(())
}

async fn read_request_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<String>, std::io::Error> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
}

fn parse_request_line(line: &str) -> (String, String) {
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    (method, target)
}

fn parse_host_port(target: &str) -> Option<(String, u16)> {
    let (host, port) = match target.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()?),
        None => (target.to_string(), 443u16),
    };
    Some((host, port))
}

async fn drain_headers<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<(), std::io::Error> {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        if line == "\r\n" || line == "\n" {
            return Ok(());
        }
    }
}

fn parse_content_length(headers: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(headers);
    for line in text.lines() {
        let mut parts = line.splitn(2, ':');
        let name = parts.next()?.trim();
        let value = parts.next()?.trim();
        if name.eq_ignore_ascii_case("content-length") {
            return value.parse().ok();
        }
    }
    None
}

fn current_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{now}")
}
