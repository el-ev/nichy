use sha2::{Digest, Sha256};

pub fn content_hash(is_type_expr: bool, content: &str, target: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"nichy.v1\x00");
    h.update([u8::from(is_type_expr)]);
    h.update((target.len() as u32).to_le_bytes());
    h.update(target.as_bytes());
    h.update((content.len() as u32).to_le_bytes());
    h.update(content.as_bytes());
    base32(&h.finalize())
}

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

fn base32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 8 / 5 + 1);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buf >> bits) & 0x1f) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buf << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic() {
        let a = content_hash(false, "struct X;", "x86");
        let b = content_hash(false, "struct X;", "x86");
        assert_eq!(a, b);
    }

    #[test]
    fn content_hash_differs_by_kind() {
        let a = content_hash(false, "Option<u8>", "");
        let b = content_hash(true, "Option<u8>", "");
        assert_ne!(a, b);
    }

    #[test]
    fn content_hash_differs_by_target() {
        let a = content_hash(false, "code", "x86_64-unknown-linux-gnu");
        let b = content_hash(false, "code", "wasm32-unknown-unknown");
        assert_ne!(a, b);
    }

    #[test]
    fn content_hash_resists_field_boundary_confusion() {
        // Without length prefixes, target="a"+content="bc" and target="ab"+content="c"
        // would hash the same. With length prefixes they must differ.
        let a = content_hash(false, "bc", "a");
        let b = content_hash(false, "c", "ab");
        assert_ne!(a, b);
    }

    #[test]
    fn base32_outputs_only_alphabet_chars() {
        let s = content_hash(false, "x", "");
        assert!(s.chars().all(|c| ALPHABET.contains(&(c as u8))));
    }
}
