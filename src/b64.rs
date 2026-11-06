//! Base64 codecs with no external dependency.
//!
//! Two spellings are needed and both are implemented here rather than pulled
//! from a crate:
//!
//! * **standard, padded** — the Azure `SharedKey` account key and request
//!   signature are base64 in the standard alphabet (RFC 4648 §4).
//! * **URL-safe, unpadded** — JWS/JWT segments and the `n`/`e`/`d`… members of a
//!   service-account JSON key are base64url without padding (RFC 7515 §2).
//!
//! Decoders are strict. A malformed character, a bad length, non-canonical
//! trailing bits, or padding where the variant forbids it is an error; nothing
//! is silently skipped. Decoding never allocates more than `4/3` of the input.

/// RFC 4648 §1 standard alphabet.
const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// RFC 4648 §5 URL-safe alphabet.
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn encode_with(bytes: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let word = (b0 << 16) | (b1 << 8) | b2;
        out.push(alphabet[(word >> 18) as usize & 0x3f] as char);
        out.push(alphabet[(word >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(alphabet[(word >> 6) as usize & 0x3f] as char);
        } else if pad {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(alphabet[word as usize & 0x3f] as char);
        } else if pad {
            out.push('=');
        }
    }
    out
}

/// Standard base64 with `=` padding (RFC 4648 §4).
pub fn encode_standard(bytes: &[u8]) -> String {
    encode_with(bytes, STANDARD, true)
}

/// URL-safe base64 with no padding (RFC 4648 §5 as used by RFC 7515 §2).
pub fn encode_url_nopad(bytes: &[u8]) -> String {
    encode_with(bytes, URL_SAFE, false)
}

fn decode_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

/// Decode either spelling. Padding is optional and, when present, must be
/// correct. Every other character is rejected, including embedded whitespace.
pub fn decode_any(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b'=' {
        end -= 1;
    }
    let padding = bytes.len() - end;
    if padding > 2 {
        return Err(format!(
            "base64 `{padding}` padding bytes; at most two are allowed"
        ));
    }
    let remainder = end % 4;
    if remainder == 1 {
        return Err("base64 has a trailing 6-bit character with no complete byte".to_owned());
    }
    let expected_padding = match remainder {
        0 => 0,
        2 => 2,
        _ => 1,
    };
    if padding > 0 && padding != expected_padding {
        return Err(format!(
            "base64 padding of {padding} bytes is wrong for {end} characters; expected {\
             expected_padding}"
        ));
    }
    let mut out = Vec::with_capacity(end / 4 * 3 + 3);
    let mut accumulator: u32 = 0;
    let mut bits = 0u32;
    for &byte in &bytes[..end] {
        let value = decode_value(byte).ok_or_else(|| {
            format!("base64 byte 0x{byte:02x} is outside both RFC 4648 alphabets")
        })?;
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
            accumulator &= (1 << bits) - 1;
        }
    }
    // Non-canonical encodings would let two different strings decode to the
    // same bytes; reject the leftover bits instead of ignoring them.
    if bits > 0 && accumulator != 0 {
        return Err("base64 trailing bits are not zero (non-canonical encoding)".to_owned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_round_trips_the_rfc_vectors() {
        let cases: &[(&[u8], &str)] = &[
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
            (&[0xff, 0x00, 0xfe], "/wD+"),
        ];
        for (bytes, text) in cases {
            assert_eq!(encode_standard(bytes), *text);
            assert_eq!(decode_any(text).unwrap(), bytes.to_vec());
        }
    }

    #[test]
    fn url_safe_is_unpadded_and_decodable() {
        assert_eq!(encode_url_nopad(&[0xfb, 0xff]), "-_8");
        assert_eq!(decode_any("-_8").unwrap(), vec![0xfb, 0xff]);
        assert_eq!(encode_url_nopad(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn malformed_inputs_are_rejected() {
        for text in [
            "a", "abcde", "Zm9v!", "Zm 9v", "Z===", "Zg=", "Zh==", "Zg===",
        ] {
            assert!(
                decode_any(text).is_err(),
                "`{text}` must be rejected, got {:?}",
                decode_any(text)
            );
        }
    }

    #[test]
    fn a_two_hundred_and_fifty_six_bit_key_round_trips() {
        let key: Vec<u8> = (0..32u8).collect();
        let text = encode_standard(&key);
        assert_eq!(text.len(), 44);
        assert!(text.ends_with('='));
        assert_eq!(decode_any(&text).unwrap(), key);
    }
}
