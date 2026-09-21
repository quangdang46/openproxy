//! DeepSeekHashV1 proof-of-work solver — port of OmniRoute
//! `open-sse/lib/deepseek-pow-hash.js` (uint32 optimized implementation) +
//! `open-sse/lib/deepseek-pow.ts` (challenge validation).
//!
//! DeepSeekHashV1 is the SHA3-256 sponge construction with KECCAK-p[1600,23]
//! (the last 23 rounds, round indices 1..=23) instead of FIPS-202's 24
//! rounds. A standard `sha3` crate **cannot** be used — hence this port.
//!
//! Nonce search: find the smallest `nonce` in `0..difficulty` such that
//! `DeepSeekHashV1(prefix + nonce) == challenge`. The prefix is
//! pre-absorbed once (`{salt}_{expire_at}_`) so each candidate only hashes
//! the nonce suffix + padding.

const SHA3_256_RATE_BYTES: usize = 136;
const SHA3_DOMAIN_SUFFIX: u32 = 0x06;
const SHA3_256_OUTPUT_BYTES: usize = 32;
const DEEPSEEK_HASH_ROUNDS: usize = 23;
pub const MAX_DEEPSEEK_POW_DIFFICULTY: u64 = 250_000;

const ROTATION_OFFSETS: [u32; 25] = [
    0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14,
];

/// Keccak round constants RC[0..24] (FIPS 202); DeepSeekHashV1 applies the
/// last 23 (indices 1..=23).
const ROUND_CONSTANTS: [u64; 24] = [
    0x0000_0000_0000_0001,
    0x0000_0000_0000_8082,
    0x8000_0000_0000_808a,
    0x8000_0000_8000_8000,
    0x0000_0000_0000_808b,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8009,
    0x0000_0000_0000_008a,
    0x0000_0000_0000_0088,
    0x0000_0000_8000_8009,
    0x0000_0000_8000_000a,
    0x0000_0000_8000_808b,
    0x8000_0000_0000_008b,
    0x8000_0000_0000_8089,
    0x8000_0000_0000_8003,
    0x8000_0000_0000_8002,
    0x8000_0000_0000_0080,
    0x0000_0000_0000_800a,
    0x8000_0000_8000_000a,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8080,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8008,
];

/// Rho+Pi destination word index for lane `lane` (each lane = 2 u32 words,
/// little-endian low/high).
fn rho_pi_destination_words() -> [usize; 25] {
    let mut out = [0usize; 25];
    for lane in 0..25 {
        let x = lane % 5;
        let y = lane / 5;
        out[lane] = 2 * (y + 5 * ((2 * x + 3 * y) % 5));
    }
    out
}

fn chi_next_words() -> [usize; 25] {
    let mut out = [0usize; 25];
    for lane in 0..25 {
        let x = lane % 5;
        let row = lane - x;
        out[lane] = 2 * (row + ((x + 1) % 5));
    }
    out
}

fn chi_next_next_words() -> [usize; 25] {
    let mut out = [0usize; 25];
    for lane in 0..25 {
        let x = lane % 5;
        let row = lane - x;
        out[lane] = 2 * (row + ((x + 2) % 5));
    }
    out
}

fn round_constants_low() -> [u32; 24] {
    let mut out = [0u32; 24];
    for (i, c) in ROUND_CONSTANTS.iter().enumerate() {
        out[i] = (c & 0xffff_ffff) as u32;
    }
    out
}

fn round_constants_high() -> [u32; 24] {
    let mut out = [0u32; 24];
    for (i, c) in ROUND_CONSTANTS.iter().enumerate() {
        out[i] = ((c >> 32) & 0xffff_ffff) as u32;
    }
    out
}

/// 32-bit Keccak-p[1600] permutation over the last `round_count` rounds.
/// State: 50 u32 words (25 lanes × low/high). Scratch buffers are
/// caller-provided so the hot loop allocates nothing.
fn keccak_p1600_u32(
    state: &mut [u32; 50],
    rho_pi_state: &mut [u32; 50],
    column_parity: &mut [u32; 10],
    theta_mix: &mut [u32; 10],
    round_count: usize,
) {
    let rho_pi_dest = rho_pi_destination_words();
    let chi_next = chi_next_words();
    let chi_next_next = chi_next_next_words();
    let rc_low = round_constants_low();
    let rc_high = round_constants_high();
    let first_round = ROUND_CONSTANTS.len() - round_count;

    for round in first_round..ROUND_CONSTANTS.len() {
        // Theta.
        for x in 0..5 {
            let word = 2 * x;
            column_parity[word] = state[word]
                ^ state[word + 10]
                ^ state[word + 20]
                ^ state[word + 30]
                ^ state[word + 40];
            column_parity[word + 1] = state[word + 1]
                ^ state[word + 11]
                ^ state[word + 21]
                ^ state[word + 31]
                ^ state[word + 41];
        }
        for x in 0..5 {
            let previous = 2 * ((x + 4) % 5);
            let next = 2 * ((x + 1) % 5);
            let rotated_low = (column_parity[next] << 1) | (column_parity[next + 1] >> 31);
            let rotated_high = (column_parity[next + 1] << 1) | (column_parity[next] >> 31);
            theta_mix[2 * x] = column_parity[previous] ^ rotated_low;
            theta_mix[2 * x + 1] = column_parity[previous + 1] ^ rotated_high;
        }
        for x in 0..5 {
            let word = 2 * x;
            let (low, high) = (theta_mix[word], theta_mix[word + 1]);
            state[word] ^= low;
            state[word + 1] ^= high;
            state[word + 10] ^= low;
            state[word + 11] ^= high;
            state[word + 20] ^= low;
            state[word + 21] ^= high;
            state[word + 30] ^= low;
            state[word + 31] ^= high;
            state[word + 40] ^= low;
            state[word + 41] ^= high;
        }
        // Rho + Pi.
        rho_pi_state[0] = state[0];
        rho_pi_state[1] = state[1];
        for lane in 1..25 {
            let source = 2 * lane;
            let destination = rho_pi_dest[lane];
            let amount = ROTATION_OFFSETS[lane];
            let (low, high) = (state[source], state[source + 1]);
            if amount < 32 {
                rho_pi_state[destination] = (low << amount) | (high >> (32 - amount));
                rho_pi_state[destination + 1] = (high << amount) | (low >> (32 - amount));
            } else {
                let reduced = amount - 32;
                rho_pi_state[destination] = (high << reduced) | (low >> (32 - reduced));
                rho_pi_state[destination + 1] = (low << reduced) | (high >> (32 - reduced));
            }
        }
        // Chi.
        for lane in 0..25 {
            let word = 2 * lane;
            let next_word = chi_next[lane];
            let next_next_word = chi_next_next[lane];
            state[word] =
                rho_pi_state[word] ^ (!rho_pi_state[next_word] & rho_pi_state[next_next_word]);
            state[word + 1] = rho_pi_state[word + 1]
                ^ (!rho_pi_state[next_word + 1] & rho_pi_state[next_next_word + 1]);
        }
        // Iota.
        state[0] ^= rc_low[round];
        state[1] ^= rc_high[round];
    }
}

fn absorb_full_block(
    state: &mut [u32; 50],
    bytes: &[u8],
    offset: usize,
    rho_pi_state: &mut [u32; 50],
    column_parity: &mut [u32; 10],
    theta_mix: &mut [u32; 10],
    round_count: usize,
) {
    for index in 0..SHA3_256_RATE_BYTES {
        let word = index >> 2;
        state[word] ^= (bytes[offset + index] as u32) << ((index & 3) * 8);
    }
    keccak_p1600_u32(state, rho_pi_state, column_parity, theta_mix, round_count);
}

fn digest_state_to_hex(state: &[u32; 50]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(SHA3_256_OUTPUT_BYTES * 2);
    for index in 0..SHA3_256_OUTPUT_BYTES {
        let byte = ((state[index >> 2] >> ((index & 3) * 8)) & 0xff) as usize;
        out.push(HEX[byte >> 4] as char);
        out.push(HEX[byte & 0x0f] as char);
    }
    out
}

/// DeepSeekHashV1 digest of `input` (hex string).
pub fn deepseek_hash_v1(input: &[u8]) -> String {
    let mut state = [0u32; 50];
    let mut rho_pi_state = [0u32; 50];
    let mut column_parity = [0u32; 10];
    let mut theta_mix = [0u32; 10];
    let mut offset = 0;
    while offset + SHA3_256_RATE_BYTES <= input.len() {
        absorb_full_block(
            &mut state,
            input,
            offset,
            &mut rho_pi_state,
            &mut column_parity,
            &mut theta_mix,
            DEEPSEEK_HASH_ROUNDS,
        );
        offset += SHA3_256_RATE_BYTES;
    }
    let remaining = input.len() - offset;
    for index in 0..remaining {
        state[index >> 2] ^= (input[offset + index] as u32) << ((index & 3) * 8);
    }
    state[remaining >> 2] ^= SHA3_DOMAIN_SUFFIX << ((remaining & 3) * 8);
    state[(SHA3_256_RATE_BYTES - 1) >> 2] ^= 0x80 << 24;
    keccak_p1600_u32(
        &mut state,
        &mut rho_pi_state,
        &mut column_parity,
        &mut theta_mix,
        DEEPSEEK_HASH_ROUNDS,
    );
    digest_state_to_hex(&state)
}

fn parse_digest_words(digest_hex: &str) -> Option<[u32; 8]> {
    if digest_hex.len() != 64 || !digest_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut words = [0u32; 8];
    for index in 0..SHA3_256_OUTPUT_BYTES {
        let byte = u8::from_str_radix(&digest_hex[index * 2..index * 2 + 2], 16).ok()?;
        words[index >> 2] |= (byte as u32) << ((index & 3) * 8);
    }
    Some(words)
}

/// Validate a PoW challenge per `deepseek-pow.ts validateChallenge`.
/// Returns the search prefix (`{salt}_{expire_at}_`) on success.
pub fn validate_challenge(
    algorithm: &str,
    challenge: &str,
    salt: &str,
    difficulty: u64,
) -> Result<String, String> {
    if algorithm != "DeepSeekHashV1" {
        return Err(format!("Unsupported DeepSeek PoW algorithm: {algorithm}"));
    }
    if parse_digest_words(&challenge.to_lowercase()).is_none() {
        return Err("DeepSeek PoW challenge must be a 64-character hex digest".to_string());
    }
    if salt.is_empty() || salt.len() > 1024 {
        return Err("DeepSeek PoW salt must contain 1-1024 characters".to_string());
    }
    if difficulty < 1 || difficulty > MAX_DEEPSEEK_POW_DIFFICULTY {
        return Err(format!(
            "DeepSeek PoW difficulty must be an integer from 1 to {MAX_DEEPSEEK_POW_DIFFICULTY}"
        ));
    }
    Ok(challenge.to_lowercase())
}

/// Search `prefix + nonce` for the challenge digest.
/// Returns the winning nonce, or `None` when exhausted (JS returns -1).
pub fn find_pow_nonce(prefix: &str, challenge_hex: &str, difficulty: u64) -> Option<u64> {
    let target_words = parse_digest_words(&challenge_hex.to_lowercase())?;
    let prefix_bytes = prefix.as_bytes();

    let mut base_state = [0u32; 50];
    let mut rho_pi_state = [0u32; 50];
    let mut column_parity = [0u32; 10];
    let mut theta_mix = [0u32; 10];
    let mut prefix_offset = 0;
    while prefix_offset + SHA3_256_RATE_BYTES <= prefix_bytes.len() {
        absorb_full_block(
            &mut base_state,
            prefix_bytes,
            prefix_offset,
            &mut rho_pi_state,
            &mut column_parity,
            &mut theta_mix,
            DEEPSEEK_HASH_ROUNDS,
        );
        prefix_offset += SHA3_256_RATE_BYTES;
    }
    let tail_length = prefix_bytes.len() - prefix_offset;
    let mut tail_words = vec![0u32; tail_length.div_ceil(4)];
    for index in 0..tail_length {
        tail_words[index >> 2] ^= (prefix_bytes[prefix_offset + index] as u32) << ((index & 3) * 8);
    }

    let mut state = [0u32; 50];
    'nonce_loop: for nonce in 0..difficulty {
        state.copy_from_slice(&base_state);
        for (word, tw) in tail_words.iter().enumerate() {
            state[word] ^= tw;
        }
        let mut position = tail_length;
        let nonce_text = nonce.to_string();
        for ch in nonce_text.bytes() {
            state[position >> 2] ^= (ch as u32) << ((position & 3) * 8);
            position += 1;
            if position == SHA3_256_RATE_BYTES {
                keccak_p1600_u32(
                    &mut state,
                    &mut rho_pi_state,
                    &mut column_parity,
                    &mut theta_mix,
                    DEEPSEEK_HASH_ROUNDS,
                );
                position = 0;
            }
        }
        state[position >> 2] ^= SHA3_DOMAIN_SUFFIX << ((position & 3) * 8);
        state[(SHA3_256_RATE_BYTES - 1) >> 2] ^= 0x80 << 24;
        keccak_p1600_u32(
            &mut state,
            &mut rho_pi_state,
            &mut column_parity,
            &mut theta_mix,
            DEEPSEEK_HASH_ROUNDS,
        );
        for word in 0..8 {
            if state[word] != target_words[word] {
                continue 'nonce_loop;
            }
        }
        return Some(nonce);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_vectors_match_js_reference() {
        // Generated with node against deepseek-pow-hash.js (see /tmp/pow_xcheck.mjs).
        let cases = [
            (
                "",
                "e594808bc5b7151ac160c6d39a02e0a8e261ed588578403099e3561dc40c26b3",
            ),
            (
                "hello",
                "50605e468e6d6ead913d7d7ccc4687b83ded157cf0a0c5e011eefece12712fa5",
            ),
            (
                &"a".repeat(200),
                "c396d2681a0d7a5498f39922282034b164b3494c243d4c4d2e277a13686704fd",
            ),
            (
                "salt_12345_0",
                "fc04a0e7945fffcc7f9bbf2bc90ba6636308eb28535a144678cd706e55b86c14",
            ),
            (
                "prefix_999_42",
                "7d6b4ae453f4c70f123f958804503a99486fdf44dfc4f580a4059854db99ab42",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                deepseek_hash_v1(input.as_bytes()),
                expected,
                "input {input:?}"
            );
        }
        // JS: findDeepSeekPowNonce('salty_1700000000_', H('salty_1700000000_7'), 1000) === 7.
        let prefix = "salty_1700000000_";
        let challenge = deepseek_hash_v1(format!("{prefix}7").as_bytes());
        assert_eq!(find_pow_nonce(prefix, &challenge, 1000), Some(7));
    }

    #[test]
    fn hash_differs_from_fips_sha3_256() {
        // 23 rounds ≠ 24 rounds: must NOT equal standard SHA3-256("").
        assert_ne!(
            deepseek_hash_v1(b""),
            "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a"
        );
    }

    #[test]
    fn nonce_search_roundtrip() {
        // A challenge derived from a known (prefix, nonce) pair must resolve
        // to that same nonce (JS parity: findDeepSeekPowNonce('test_1_', H, 100) === 0).
        let prefix = "test_1_";
        let challenge = deepseek_hash_v1(format!("{prefix}0").as_bytes());
        assert_eq!(find_pow_nonce(prefix, &challenge, 100), Some(0));
        let challenge5 = deepseek_hash_v1(format!("{prefix}5").as_bytes());
        assert_eq!(find_pow_nonce(prefix, &challenge5, 100), Some(5));
    }

    #[test]
    fn nonce_search_exhaustion_returns_none() {
        let challenge = deepseek_hash_v1(b"something-else-entirely-xyz");
        assert_eq!(find_pow_nonce("nope_", &challenge, 10), None);
    }

    #[test]
    fn challenge_validation_rejects_bad_input() {
        assert!(validate_challenge("SHA3-256", &"a".repeat(64), "s", 10).is_err());
        assert!(validate_challenge("DeepSeekHashV1", "zzzz", "s", 10).is_err());
        assert!(validate_challenge("DeepSeekHashV1", &"a".repeat(64), "", 10).is_err());
        assert!(validate_challenge("DeepSeekHashV1", &"a".repeat(64), "s", 0).is_err());
        assert!(validate_challenge("DeepSeekHashV1", &"a".repeat(64), "s", 10).is_ok());
    }
}
