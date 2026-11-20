//! Lowercase hex encoding of digests and ids.

/// `bytes` as lowercase hex, two digits per byte.
///
/// The one encoder for the codebase's digests (content and embed digests,
/// project and group ids, layout keys): `sha2` 0.11's `finalize` returns a
/// `hybrid_array::Array` with no `LowerHex`, and the `format!("{b:02x}")`
/// per byte that replaced `{:x}` allocated a `String` per byte.
pub fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::lower_hex;

    #[test]
    fn matches_the_two_digit_format() {
        let bytes: Vec<u8> = (0..=255).collect();
        let expected: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(lower_hex(&bytes), expected);
        assert_eq!(lower_hex(&[]), "");
    }
}
