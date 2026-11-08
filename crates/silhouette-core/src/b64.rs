//! Minimal standard base64 decoder (no external dependencies).

/// Decode standard base64 (`A-Z a-z 0-9 + /` with `=` padding).
///
/// Returns `None` on any invalid byte. Trailing data after `=` is ignored.
pub(crate) fn decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;

    for &b in input {
        let v = match b {
            b'A'..=b'Z' => u32::from(b - b'A'),
            b'a'..=b'z' => u32::from(b - b'a') + 26,
            b'0'..=b'9' => u32::from(b - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1u32 << bits).wrapping_sub(1);
        }
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn decodes_known_string() {
        assert_eq!(decode(b"eyJhIjoxfQ==").unwrap(), br#"{"a":1}"#);
        assert_eq!(decode(b"AAEC").unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn rejects_invalid() {
        assert!(decode(b"!!!!").is_none());
        assert!(decode(b"ab cd").is_none());
    }
}
