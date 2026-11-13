//! Secret Key：アカウント作成時に端末で生成する 128bit の乱数。
//!
//! 表示形式： `2P1-<アカウントID 6文字>-XXXXX-XXXXX-XXXXX-XXXXX-XXXXXX`
//! 128bit のエントロピーは後半 26 文字にだけ入っている。
use crate::{Error, Result, base32, rng};
use zeroize::Zeroizing;

pub const PREFIX: &str = "2P1";
const AID_LEN: usize = 6;
const KEY_CHARS: usize = 26;

/// アカウントID（秘密ではない。Drive のファイルにも書かれる）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountId(String);

impl AccountId {
    pub fn generate() -> Result<Self> {
        let r = rng::array::<AID_LEN>()?;
        // 256 は 32 で割り切れるので偏りなし
        Ok(Self(
            r.iter()
                .map(|b| base32::ALPHABET[(b & 31) as usize] as char)
                .collect(),
        ))
    }

    pub fn parse(s: &str) -> Result<Self> {
        let n = base32::normalize(s);
        if n.len() == AID_LEN && n.bytes().all(|c| base32::value(c).is_some()) {
            Ok(Self(n))
        } else {
            Err(Error::Format("アカウントID"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct SecretKey {
    aid: AccountId,
    bytes: Zeroizing<[u8; 16]>,
}

impl core::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "SecretKey({}-***)", self.aid.as_str())
    }
}

impl SecretKey {
    pub fn generate(aid: AccountId) -> Result<Self> {
        Ok(Self {
            aid,
            bytes: Zeroizing::new(rng::array::<16>()?),
        })
    }

    /// テストや既知の値からの復元用。
    pub fn from_bytes(aid: AccountId, bytes: [u8; 16]) -> Self {
        Self {
            aid,
            bytes: Zeroizing::new(bytes),
        }
    }

    pub fn aid(&self) -> &AccountId {
        &self.aid
    }

    pub fn expose(&self) -> &[u8; 16] {
        &self.bytes
    }

    /// 人が読む形式。戻り値も秘密なので Zeroizing で返す。
    pub fn to_display(&self) -> Zeroizing<String> {
        let k = Zeroizing::new(base32::encode(&self.bytes[..]));
        let mut s = Zeroizing::new(String::with_capacity(48));
        s.push_str(PREFIX);
        s.push('-');
        s.push_str(self.aid.as_str());
        for chunk in [&k[0..5], &k[5..10], &k[10..15], &k[15..20], &k[20..26]] {
            s.push('-');
            s.push_str(chunk);
        }
        s
    }

    pub fn parse(input: &str) -> Result<Self> {
        let n = Zeroizing::new(base32::normalize(input.trim()));
        // "2P1" は正規化後も "2P1"（I→1 なので "2PI" と入力しても通る）
        if n.len() != PREFIX.len() + AID_LEN + KEY_CHARS || !n.starts_with(PREFIX) {
            return Err(Error::SecretKeyFormat);
        }
        let aid = AccountId::parse(&n[3..3 + AID_LEN]).map_err(|_| Error::SecretKeyFormat)?;
        let v =
            Zeroizing::new(base32::decode(&n[3 + AID_LEN..], 16).ok_or(Error::SecretKeyFormat)?);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&v);
        let sk = Self::from_bytes(aid, bytes);
        zeroize::Zeroize::zeroize(&mut bytes);
        Ok(sk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_parse_roundtrip() {
        let sk = SecretKey::generate(AccountId::generate().unwrap()).unwrap();
        let shown = sk.to_display();
        // 2P1-AAAAAA-xxxxx-xxxxx-xxxxx-xxxxx-xxxxxx
        assert_eq!(shown.len(), 3 + 1 + 6 + 4 * 6 + 1 + 6);
        let back = SecretKey::parse(&shown).unwrap();
        assert_eq!(back.expose(), sk.expose());
        assert_eq!(back.aid(), sk.aid());
    }

    #[test]
    fn parse_is_forgiving_about_case_spaces_and_confusables() {
        let sk = SecretKey::from_bytes(AccountId::parse("AB0123").unwrap(), [0x5a; 16]);
        let shown = sk
            .to_display()
            .to_lowercase()
            .replace('-', " ")
            .replace('0', "o");
        assert_eq!(SecretKey::parse(&shown).unwrap().expose(), &[0x5a; 16]);
    }

    #[test]
    fn rejects_wrong_length_and_prefix() {
        assert!(SecretKey::parse("2P1-ABC123-AAAAA").is_err());
        assert!(SecretKey::parse("A3-ABC123-AAAAA-AAAAA-AAAAA-AAAAA-AAAAAA").is_err());
    }

    #[test]
    fn debug_does_not_leak() {
        let sk = SecretKey::from_bytes(AccountId::parse("AB0123").unwrap(), [0x5a; 16]);
        let d = format!("{sk:?}");
        assert!(!d.contains(&base32::encode(&[0x5a; 16])[..10]));
    }
}
