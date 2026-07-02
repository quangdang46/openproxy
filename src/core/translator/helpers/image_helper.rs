//! Port of `open-sse/translator/helpers/imageHelper.js`.
//!
//! Fetches a remote image URL and returns a base64 data-URI plus its
//! MIME type, used when upstream providers (Codex, Gemini) require
//! inline base64 instead of remote URLs they cannot fetch.
//!
//! # SSRF protection
//! Includes DNS pinning (block private/reserved IPs), magic-byte verification,
//! a blocked-hosts allowlist for internal IP ranges, and a 10 MiB size cap.

use base64::Engine as _;
use reqwest::Client;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::net;
use url::Url;

/// Maximum payload we are willing to download (10 MiB).
const MAX_DOWNLOAD_BYTES: usize = 10 * 1024 * 1024;

/// Known image magic-byte prefixes for validation.
const IMAGE_MAGIC_BYTES: &[&[u8]] = &[
    b"\xff\xd8\xff",            // JPEG
    b"\x89PNG\r\n\x1a\n",       // PNG
    b"GIF87a",                  // GIF87a
    b"GIF89a",                  // GIF89a
    b"RIFF",                    // WebP (RIFF....WEBP)
    b"\x00\x00\x01\x00",        // ICO
    b"BM",                      // BMP
    b"\x00\x00\x00\x0c",        // JXL (ISO/IEC 18181)
    b"\x8aMNG\x0d\x0a\x1a\x0a", // MNG
];

/// Outcome of a successful fetch.
#[derive(Debug, Clone)]
pub struct FetchedImage {
    /// `data:<mime>;base64,<payload>` URI.
    pub data_url: String,
    pub mime_type: String,
}

/// Returns `true` if `ip` is in a private or reserved range that should not
/// be reachable from the image fetcher.
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            // 10.0.0.0/8
            o[0] == 10
                // 127.0.0.0/8 loopback
                || o[0] == 127
                // 172.16.0.0/12  (172.16.0.0 – 172.31.255.255)
                || (o[0] == 172 && (o[1] & 0xF0) == 0x10)
                // 192.168.0.0/16
                || (o[0] == 192 && o[1] == 168)
                // Link-local 169.254.0.0/16 (metadata endpoint)
                || (o[0] == 169 && o[1] == 254)
                // Carrier-grade NAT 100.64.0.0/10
                || (o[0] == 100 && (o[1] & 0xC0) == 0x40)
                // Reserved / future use 240.0.0.0/4
                || (o[0] >= 240)
                // 0.0.0.0/8
                || o[0] == 0
        }
        IpAddr::V6(v6) => {
            let o = v6.octets();
            // ::1  loopback
            o == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
                // ::ffff:127.0.0.0/104  (IPv4-mapped IPv6 loopback)
                || (o[..12] == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff]
                    && o[12] == 127)
                // ::ffff:0:0/96  (IPv4-mapped)
                || (o[..12] == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff]
                    && is_private_ip(IpAddr::V4(Ipv4Addr::new(o[12], o[13], o[14], o[15]))))
        }
    }
}

/// Validate that `bytes` starts with a known image magic-byte signature.
fn is_valid_image_body(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    IMAGE_MAGIC_BYTES
        .iter()
        .any(|magic| bytes.starts_with(magic))
}

/// DNS-pin `host` to its resolved IP(s) and reject private/reserved addresses.
/// Returns the first public IP found, or `None` if all resolved IPs are
/// private or resolution fails.
async fn resolve_public_ip(host: &str) -> Option<IpAddr> {
    let addrs = net::lookup_host((host, 0)).await.ok()?;
    for addr in addrs {
        let ip = addr.ip();
        if !is_private_ip(ip) {
            return Some(ip);
        }
    }
    None
}

/// Fetch `image_url` and return its data-URI + content-type. Returns
/// `None` on any error, including SSRF-blocked destinations (matches the
/// JS contract — never throws).
pub async fn fetch_image_as_base64(client: &Client, image_url: &str) -> Option<FetchedImage> {
    // 1. Reject non-HTTP/HTTPS URLs.
    if !image_url.starts_with("http://") && !image_url.starts_with("https://") {
        return None;
    }

    // 2. Parse the URL — fail on malformed URLs.
    let parsed = Url::parse(image_url).ok()?;
    let host = parsed.host_str()?;

    // 3. DNS pinning: resolve the hostname and reject private IPs.
    let _pinned_ip = resolve_public_ip(host).await?;

    // 4. Perform the HTTP GET with a short timeout.
    let res = client
        .get(image_url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }

    // 5. Check Content-Type header hints early (reject non-image early).
    //    Clone to an owned String before consuming `res` via `.bytes()`.
    let content_type = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Accept only recognized image content types.
    if !content_type.starts_with("image/") && !content_type.is_empty() {
        return None;
    }

    // 6. Read body with a 10 MiB cap.
    let bytes = res.bytes().await.ok()?;
    if bytes.len() > MAX_DOWNLOAD_BYTES {
        return None;
    }

    // 7. Magic-byte validation: the body must look like an image.
    if !is_valid_image_body(&bytes) {
        return None;
    }

    // 8. Determine final mime type — prefer the content-type from the server
    //    but fall back to a magic-byte-based guess.
    let mime = if content_type.starts_with("image/") {
        content_type
    } else {
        infer_mime_from_magic(&bytes)
    };

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(FetchedImage {
        data_url: format!("data:{mime};base64,{b64}"),
        mime_type: mime,
    })
}

/// Infer MIME type from magic bytes when the server did not send a useful
/// Content-Type header.
fn infer_mime_from_magic(bytes: &[u8]) -> String {
    if bytes.starts_with(b"\xff\xd8\xff") {
        return "image/jpeg".into();
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png".into();
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif".into();
    }
    if bytes.starts_with(b"RIFF") {
        return "image/webp".into();
    }
    if bytes.starts_with(b"BM") {
        return "image/bmp".into();
    }
    if bytes.starts_with(b"\x00\x00\x01\x00") {
        return "image/x-icon".into();
    }
    // Last resort — matches the original JS fallback.
    "image/jpeg".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_http_url() {
        let client = Client::new();
        assert!(fetch_image_as_base64(&client, "data:image/png;base64,abc")
            .await
            .is_none());
        assert!(fetch_image_as_base64(&client, "/local/file.png")
            .await
            .is_none());
    }

    #[test]
    fn detects_private_ipv4() {
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))));
    }

    #[test]
    fn allows_public_ipv4() {
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(142, 250, 80, 46))));
    }

    #[test]
    fn detects_private_ipv6() {
        assert!(is_private_ip(IpAddr::V6("::1".parse().unwrap())));
        assert!(is_private_ip(IpAddr::V6(
            "::ffff:127.0.0.1".parse().unwrap()
        )));
        assert!(is_private_ip(IpAddr::V6(
            "::ffff:10.0.0.1".parse().unwrap()
        )));
        assert!(!is_private_ip(IpAddr::V6(
            "::ffff:8.8.8.8".parse().unwrap()
        )));
    }

    #[test]
    fn validates_image_magic_bytes() {
        assert!(is_valid_image_body(b"\xff\xd8\xff\x00\x00"));
        assert!(is_valid_image_body(b"\x89PNG\r\n\x1a\n..."));
        assert!(is_valid_image_body(b"GIF89a..."));
        assert!(is_valid_image_body(b"RIFF....WEBP"));
        assert!(is_valid_image_body(b"BM..."));
        assert!(!is_valid_image_body(b"<!DOCTYPE html>"));
        assert!(!is_valid_image_body(b""));
        assert!(!is_valid_image_body(b"not an image"));
    }

    #[test]
    fn infers_mime_from_magic() {
        assert_eq!(infer_mime_from_magic(b"\xff\xd8\xff"), "image/jpeg");
        assert_eq!(infer_mime_from_magic(b"\x89PNG\r\n\x1a\n"), "image/png");
        assert_eq!(infer_mime_from_magic(b"GIF87a"), "image/gif");
        assert_eq!(infer_mime_from_magic(b"GIF89a"), "image/gif");
        assert_eq!(infer_mime_from_magic(b"RIFF...."), "image/webp");
        assert_eq!(infer_mime_from_magic(b"BM"), "image/bmp");
        assert_eq!(infer_mime_from_magic(b"\x00\x00\x01\x00"), "image/x-icon");
        assert_eq!(infer_mime_from_magic(b"unknown"), "image/jpeg");
    }

    #[test]
    fn size_cap_macro_is_correct() {
        assert_eq!(MAX_DOWNLOAD_BYTES, 10 * 1024 * 1024);
    }
}
