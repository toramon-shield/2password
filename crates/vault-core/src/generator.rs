//! パスワード生成（OS の乱数＋棄却サンプリングで偏りなし）。
use crate::{Result, rng};
use zeroize::Zeroizing;

const LOWER: &str = "abcdefghijkmnopqrstuvwxyz";
const UPPER: &str = "ABCDEFGHJKLMNPQRSTUVWXYZ";
const DIGIT: &str = "23456789";
const SYMBOL: &str = "!#$%&*+-=?@_";

/// 長さ `len`、小文字・大文字・数字・記号を最低1文字ずつ含むパスワード。
pub fn generate(len: usize) -> Result<Zeroizing<String>> {
    let len = len.clamp(12, 128);
    let classes = [LOWER, UPPER, DIGIT, SYMBOL];
    let all: Vec<char> = classes.concat().chars().collect();
    loop {
        let mut s = Zeroizing::new(String::with_capacity(len));
        for _ in 0..len {
            s.push(all[rng::below(all.len() as u32)? as usize]);
        }
        if classes.iter().all(|c| s.chars().any(|x| c.contains(x))) {
            return Ok(s);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn length_and_classes() {
        let p = super::generate(20).unwrap();
        assert_eq!(p.chars().count(), 20);
        assert_ne!(super::generate(20).unwrap(), p);
    }
}
