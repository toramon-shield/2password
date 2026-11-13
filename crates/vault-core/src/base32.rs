//! Crockford Base32（人が読み書きしやすい表記。暗号処理ではなく表示用）。
pub const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 入力のゆれを吸収する：大文字化、O→0、I/L→1、区切り（- と空白）を除去。
pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '-' | ' ' | '\t' | '\u{3000}'))
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            u => u,
        })
        .collect()
}

pub fn value(c: u8) -> Option<u8> {
    ALPHABET.iter().position(|&a| a == c).map(|p| p as u8)
}

/// バイト列を MSB から 5bit ずつ符号化（末尾は 0 で埋める）。
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 8 / 5 + 1);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &b in bytes {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
        acc &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// `encode` の逆。長さが合わない・余りビットが 0 でない場合は None。
pub fn decode(s: &str, out_len: usize) -> Option<Vec<u8>> {
    if s.len() != (out_len * 8).div_ceil(5) {
        return None;
    }
    let mut out = Vec::with_capacity(out_len);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        acc = (acc << 5) | value(c)? as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (acc == 0 && out.len() == out_len).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn normalize_confusables() {
        assert_eq!(normalize("ab-cO i l"), "ABC011");
    }

    proptest! {
        #[test]
        fn roundtrip(v in proptest::collection::vec(any::<u8>(), 0..64)) {
            prop_assert_eq!(decode(&encode(&v), v.len()).unwrap(), v);
        }
    }
}
