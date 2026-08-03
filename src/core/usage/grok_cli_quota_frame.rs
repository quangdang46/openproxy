//! gRPC-web frame decoder for xAI GetGrokCreditsConfig
//! (grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig).
//!
//! Ported from 9router v0.5.45 `open-sse/services/usage/grokCliQuotaFrame.js`
//! (feat(usage): fetch SuperGrok weekly pool via gRPC-web).
//!
//! Real response shape (live capture 2026-07-20):
//!   top-level field 1 (length-delimited) — nested credits info
//!     subfield 1  (fixed32 float)            — usage ratio 0..1
//!     subfield 5  (Timestamp{seconds,nanos}) — credit-pool reset time
//!
//! Fail-open: any malformed buffer returns None, never panics.

const FIELD_CREDITS_INFO: u64 = 1;
const CREDITS_FIELD_USAGE_RATIO: u64 = 1;
const CREDITS_FIELD_RESET_TIMESTAMP: u64 = 5;
const TIMESTAMP_FIELD_SECONDS: u64 = 1;
const TIMESTAMP_FIELD_NANOS: u64 = 2;

const WIRE_TYPE_VARINT: u64 = 0;
const WIRE_TYPE_FIXED64: u64 = 1;
const WIRE_TYPE_LENGTH_DELIMITED: u64 = 2;
const WIRE_TYPE_FIXED32: u64 = 5;

const GRPC_WEB_TRAILER_FLAG_BIT: u8 = 0x80;
const MAX_VARINT_SHIFT: u32 = 70;

/// Decoded weekly-pool credits config.
#[derive(Debug, Clone, PartialEq)]
pub struct GrokCreditsConfig {
    /// Percent of the weekly pool used, 0..=100.
    pub percent_used: f64,
    /// ISO-8601 reset time, if the server sent one.
    pub reset_at: Option<String>,
}

/// Validate a gRPC-web frame header at `offset`.
/// Returns `(flag, payload_start, payload_length)` or None.
fn probe_frame_header(buffer: &[u8], offset: usize) -> Option<(u8, usize, usize)> {
    if offset + 5 > buffer.len() {
        return None;
    }
    let flag = buffer[offset];
    if flag != 0x00 && flag != 0x01 && flag != 0x80 && flag != 0x81 {
        return None;
    }
    let payload_start = offset + 5;
    let payload_length = u32::from_be_bytes([
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
        buffer[offset + 4],
    ]) as usize;
    if payload_length > buffer.len() - payload_start {
        return None;
    }
    Some((flag, payload_start, payload_length))
}

fn read_varint(buffer: &[u8], offset: usize) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    let mut pos = offset;
    loop {
        if pos >= buffer.len() {
            return None;
        }
        let byte = buffer[pos];
        result |= u64::from(byte & 0x7f) << shift;
        pos += 1;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > MAX_VARINT_SHIFT {
            return None;
        }
    }
    Some((result, pos))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum WireValue {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    Bytes(&'static [u8]),
}

/// Read one protobuf field; returns (field_number, value, next_offset).
fn read_field(buffer: &[u8], offset: usize) -> Option<(u64, WireValue, usize)> {
    let (tag, pos1) = read_varint(buffer, offset)?;
    let field_number = tag >> 3;
    let wire_type = tag & 0x7;
    if field_number == 0 {
        return None;
    }
    match wire_type {
        WIRE_TYPE_VARINT => {
            let (value, next) = read_varint(buffer, pos1)?;
            Some((field_number, WireValue::Varint(value), next))
        }
        WIRE_TYPE_LENGTH_DELIMITED => {
            let (length, body_start) = read_varint(buffer, pos1)?;
            let length = length as usize;
            if body_start + length > buffer.len() {
                return None;
            }
            // SAFETY: the slice lives as long as `buffer`; we return a static
            // marker and re-borrow below via indices.
            let bytes: &'static [u8] = unsafe {
                std::slice::from_raw_parts(
                    buffer.as_ptr().add(body_start),
                    length,
                )
            };
            Some((field_number, WireValue::Bytes(bytes), body_start + length))
        }
        WIRE_TYPE_FIXED64 => {
            if pos1 + 8 > buffer.len() {
                return None;
            }
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&buffer[pos1..pos1 + 8]);
            Some((
                field_number,
                WireValue::Fixed64(u64::from_le_bytes(raw)),
                pos1 + 8,
            ))
        }
        WIRE_TYPE_FIXED32 => {
            if pos1 + 4 > buffer.len() {
                return None;
            }
            let mut raw = [0u8; 4];
            raw.copy_from_slice(&buffer[pos1..pos1 + 4]);
            Some((
                field_number,
                WireValue::Fixed32(u32::from_le_bytes(raw)),
                pos1 + 4,
            ))
        }
        _ => None,
    }
}

/// Decode a buffer into a field map (field number → first value).
fn decode_fields(buffer: &[u8]) -> Option<std::collections::HashMap<u64, WireValue>> {
    let mut fields = std::collections::HashMap::new();
    let mut offset = 0;
    while offset < buffer.len() {
        let (field_number, value, next) = read_field(buffer, offset)?;
        fields.entry(field_number).or_insert(value);
        offset = next;
    }
    Some(fields)
}

/// Find the first non-trailer data frame payload.
fn find_data_frame_payload(buffer: &[u8]) -> Option<&[u8]> {
    let mut offset = 0;
    while offset < buffer.len() {
        let (flag, payload_start, payload_length) = probe_frame_header(buffer, offset)?;
        let frame_end = payload_start + payload_length;
        let is_trailer = flag & GRPC_WEB_TRAILER_FLAG_BIT != 0;
        if !is_trailer {
            return Some(&buffer[payload_start..frame_end]);
        }
        offset = frame_end;
    }
    None
}

fn extract_nested_message(field: &WireValue) -> Option<std::collections::HashMap<u64, WireValue>> {
    match field {
        WireValue::Bytes(bytes) => decode_fields(bytes),
        _ => None,
    }
}

fn extract_usage_ratio(field: &WireValue) -> Option<f64> {
    match field {
        // protobuf fixed32/fixed64 are little-endian on the wire.
        WireValue::Fixed32(raw) => Some(f32::from_bits(*raw) as f64),
        WireValue::Fixed64(raw) => Some(f64::from_bits(*raw)),
        _ => None,
    }
}

fn extract_reset_at(field: &WireValue) -> Option<String> {
    let bytes = match field {
        WireValue::Bytes(b) => b,
        _ => return None,
    };
    let timestamp_fields = decode_fields(bytes)?;
    let seconds = match timestamp_fields.get(&TIMESTAMP_FIELD_SECONDS) {
        Some(WireValue::Varint(v)) => *v,
        _ => 0,
    };
    let nanos = match timestamp_fields.get(&TIMESTAMP_FIELD_NANOS) {
        Some(WireValue::Varint(v)) => *v,
        _ => 0,
    };
    let millis = seconds
        .checked_mul(1000)
        .and_then(|s| s.checked_add(nanos / 1_000_000))?;
    let dt = chrono::DateTime::from_timestamp_millis(millis as i64)?;
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Decode GetGrokCreditsConfig response → `{ percent_used, reset_at }` or None.
///
/// Fail-open: any malformed buffer returns None, never throws.
pub fn decode_grok_credits_frame(buffer: &[u8]) -> Option<GrokCreditsConfig> {
    if buffer.is_empty() {
        return None;
    }
    let payload = if probe_frame_header(buffer, 0).is_some() {
        find_data_frame_payload(buffer)?
    } else {
        buffer
    };
    let top_level_fields = decode_fields(payload)?;
    let credits_info = extract_nested_message(top_level_fields.get(&FIELD_CREDITS_INFO)?)?;
    let usage_ratio = extract_usage_ratio(credits_info.get(&CREDITS_FIELD_USAGE_RATIO)?)?;
    if !usage_ratio.is_finite() || usage_ratio < 0.0 {
        return None;
    }
    let reset_at = credits_info
        .get(&CREDITS_FIELD_RESET_TIMESTAMP)
        .and_then(extract_reset_at);
    Some(GrokCreditsConfig {
        percent_used: usage_ratio.min(1.0) * 100.0,
        reset_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-build a valid response: frame(0, payload) where payload =
    /// field 1 (LEN) → credits info { field 1 (FIXED32) ratio, field 5 (LEN) → timestamp }.
    fn encode_field(field_num: u64, wire_type: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let tag = (field_num << 3) | wire_type;
        // varint tag
        let mut t = tag;
        loop {
            let byte = (t & 0x7f) as u8;
            t >>= 7;
            if t > 0 {
                out.push(byte | 0x80);
            } else {
                out.push(byte);
                break;
            }
        }
        if wire_type == WIRE_TYPE_LENGTH_DELIMITED {
            let mut len = payload.len() as u64;
            loop {
                let byte = (len & 0x7f) as u8;
                len >>= 7;
                if len > 0 {
                    out.push(byte | 0x80);
                } else {
                    out.push(byte);
                    break;
                }
            }
            out.extend_from_slice(payload);
        } else if wire_type == WIRE_TYPE_FIXED32 {
            out.extend_from_slice(payload);
        }
        out
    }

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn decodes_valid_credits_frame() {
        // ratio 0.35 as f32 LE
        let ratio = 0.35f32.to_le_bytes();
        // sanity: 0.35 LE = 33 33 b3 3e
        assert_eq!(ratio.to_vec(), vec![0x33, 0x33, 0xb3, 0x3e]);
        let timestamp = {
            let secs = 1_752_400_000u64;
            let mut ts = encode_field(TIMESTAMP_FIELD_SECONDS, WIRE_TYPE_VARINT, &[]);
            let mut s = secs;
            let mut vb = Vec::new();
            loop {
                let byte = (s & 0x7f) as u8;
                s >>= 7;
                if s > 0 {
                    vb.push(byte | 0x80);
                } else {
                    vb.push(byte);
                    break;
                }
            }
            ts.extend_from_slice(&vb);
            ts
        };
        let credits = [
            encode_field(CREDITS_FIELD_USAGE_RATIO, WIRE_TYPE_FIXED32, &ratio),
            encode_field(CREDITS_FIELD_RESET_TIMESTAMP, WIRE_TYPE_LENGTH_DELIMITED, &timestamp),
        ]
        .concat();
        let payload = encode_field(FIELD_CREDITS_INFO, WIRE_TYPE_LENGTH_DELIMITED, &credits);
        let decoded = decode_grok_credits_frame(&frame(&payload)).expect("decodes");
        assert!((decoded.percent_used - 35.0).abs() < 0.01);
        assert!(decoded.reset_at.is_some());
    }

    #[test]
    fn fails_open_on_garbage() {
        assert!(decode_grok_credits_frame(&[0xde, 0xad, 0xbe, 0xef]).is_none());
        assert!(decode_grok_credits_frame(&[]).is_none());
    }
}
