//! ワンタイムパスワード（TOTP、RFC 6238）。
//!
//! 2 段階認証（2FA）の設定で表示される QR コードの中身（`otpauth://totp/...`）か、
//! その下に書かれた設定キー（英数字の並び）を保管庫に入れておき、
//! 認証アプリ（Google Authenticator など）と同じ 6 桁のコードを作る。
//!
//! - 設定キーは保管庫の暗号文の中にだけ置く。ブラウザ拡張に渡すのは、その時刻のコードだけ
//! - SMS やメールで届くコードは、この方式ではないので扱えない
use crate::R;
use reqwest::Url;
use ring::hmac;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha1,
    Sha256,
    Sha512,
}

impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Algorithm::Sha1 => "SHA1",
            Algorithm::Sha256 => "SHA256",
            Algorithm::Sha512 => "SHA512",
        }
    }
    fn ring(self) -> hmac::Algorithm {
        match self {
            Algorithm::Sha1 => hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
            Algorithm::Sha256 => hmac::HMAC_SHA256,
            Algorithm::Sha512 => hmac::HMAC_SHA512,
        }
    }
}

pub struct Totp {
    secret: Zeroizing<Vec<u8>>,
    pub algorithm: Algorithm,
    pub digits: u32,
    pub period: u64,
}

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 の Base32 を読む。空白・ハイフン・末尾の `=`、大文字小文字の違いは無視する
fn base32_decode(text: &str) -> R<Zeroizing<Vec<u8>>> {
    let mut out = Zeroizing::new(Vec::new());
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in text.chars() {
        if c.is_whitespace() || c == '-' || c == '=' {
            continue;
        }
        let up = c.to_ascii_uppercase();
        let Some(v) = BASE32.iter().position(|&b| b as char == up) else {
            return Err("設定キーに使えない文字が含まれています".into());
        };
        buf = (buf << 5) | v as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn base32_encode(bytes: &[u8]) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::new());
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32[((buf >> bits) & 31) as usize] as char);
        }
        buf &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(BASE32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// 利用者が入力したもの（`otpauth://` の URL か、設定キー）を読む。
pub fn parse(input: &str) -> R<Totp> {
    let input = input.trim();
    if input.is_empty() {
        return Err("設定キーが空です".into());
    }
    let (mut algorithm, mut digits, mut period) = (Algorithm::Sha1, 6u32, 30u64);
    let secret_text: Zeroizing<String> = if input.to_ascii_lowercase().starts_with("otpauth://") {
        let url = Url::parse(input).map_err(|_| "otpauth:// の URL を読めません")?;
        if url.host_str().map(str::to_ascii_lowercase).as_deref() != Some("totp") {
            return Err("時刻で変わる方式（totp）だけに対応しています".into());
        }
        let mut secret = None;
        for (k, v) in url.query_pairs() {
            match k.to_ascii_lowercase().as_str() {
                "secret" => secret = Some(Zeroizing::new(v.into_owned())),
                "algorithm" => {
                    algorithm = match v.to_ascii_uppercase().replace('-', "").as_str() {
                        "SHA1" => Algorithm::Sha1,
                        "SHA256" => Algorithm::Sha256,
                        "SHA512" => Algorithm::Sha512,
                        _ => return Err("対応していないアルゴリズムです".into()),
                    }
                }
                "digits" => digits = v.parse().map_err(|_| "桁数を読めません")?,
                "period" => period = v.parse().map_err(|_| "更新間隔を読めません")?,
                _ => {}
            }
        }
        secret.ok_or("URL に secret がありません")?
    } else {
        Zeroizing::new(input.to_string())
    };
    let secret = base32_decode(&secret_text)?;
    if secret.len() < 8 || secret.len() > 128 {
        return Err("設定キーの長さが正しくありません".into());
    }
    if !(6..=8).contains(&digits) || !(10..=300).contains(&period) {
        return Err("桁数か更新間隔が、対応している範囲の外です".into());
    }
    Ok(Totp {
        secret,
        algorithm,
        digits,
        period,
    })
}

/// 保管庫に入れる形（必要な項目だけの `otpauth://` URL）にそろえる。
pub fn normalize(input: &str) -> R<Zeroizing<String>> {
    let t = parse(input)?;
    Ok(Zeroizing::new(format!(
        "otpauth://totp/?secret={}&algorithm={}&digits={}&period={}",
        base32_encode(&t.secret).as_str(),
        t.algorithm.name(),
        t.digits,
        t.period
    )))
}

impl Totp {
    /// 指定した時刻（UNIX 秒）のコード
    pub fn code_at(&self, unix_secs: u64) -> String {
        let counter = (unix_secs / self.period).to_be_bytes();
        let key = hmac::Key::new(self.algorithm.ring(), &self.secret);
        let mac = hmac::sign(&key, &counter);
        let mac = mac.as_ref();
        let offset = (mac[mac.len() - 1] & 0x0f) as usize;
        let bin = u32::from_be_bytes([mac[offset], mac[offset + 1], mac[offset + 2], mac[offset + 3]])
            & 0x7fff_ffff;
        format!(
            "{:0width$}",
            bin % 10u32.pow(self.digits),
            width = self.digits as usize
        )
    }

    /// いまのコードが切り替わるまでの秒数
    pub fn remaining_at(&self, unix_secs: u64) -> u64 {
        self.period - unix_secs % self.period
    }
}

/// いまのコード
pub struct Current {
    pub code: String,
    /// 切り替わるまでの秒数
    pub remaining: u64,
    pub period: u64,
}

pub fn current(stored: &str) -> R<Current> {
    let t = parse(stored)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "パソコンの時計が正しくありません")?
        .as_secs();
    Ok(Current {
        code: t.code_at(now),
        remaining: t.remaining_at(now),
        period: t.period,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rfc(secret: &[u8], alg: &str) -> Totp {
        parse(&format!(
            "otpauth://totp/Test?secret={}&algorithm={alg}&digits=8",
            base32_encode(secret).as_str()
        ))
        .unwrap()
    }

    /// RFC 6238 の付録 B にある値と一致すること
    #[test]
    fn matches_rfc_6238_test_vectors() {
        let sha1 = rfc(b"12345678901234567890", "SHA1");
        let sha256 = rfc(b"12345678901234567890123456789012", "SHA256");
        let sha512 = rfc(
            b"1234567890123456789012345678901234567890123456789012345678901234",
            "SHA512",
        );
        for (t, a, b, c) in [
            (59, "94287082", "46119246", "90693936"),
            (1111111109, "07081804", "68084774", "25091201"),
            (1111111111, "14050471", "67062674", "99943326"),
            (1234567890, "89005924", "91819424", "93441116"),
            (2000000000, "69279037", "90698825", "38618901"),
            (20000000000, "65353130", "77737706", "47863826"),
        ] {
            assert_eq!(sha1.code_at(t), a, "sha1 {t}");
            assert_eq!(sha256.code_at(t), b, "sha256 {t}");
            assert_eq!(sha512.code_at(t), c, "sha512 {t}");
        }
    }

    #[test]
    fn reads_setup_keys_and_urls() {
        // 認証アプリの設定画面に出る形（空白入り、小文字）
        let a = parse("jbsw y3dp ehpk 3pxp").unwrap();
        let b = parse("JBSWY3DPEHPK3PXP").unwrap();
        let c = parse("otpauth://totp/Example:alice@example.com?secret=JBSWY3DPEHPK3PXP&issuer=Example")
            .unwrap();
        assert_eq!((a.digits, a.period, a.algorithm), (6, 30, Algorithm::Sha1));
        assert_eq!(a.code_at(1_700_000_000), b.code_at(1_700_000_000));
        assert_eq!(a.code_at(1_700_000_000), c.code_at(1_700_000_000));
        assert_eq!(a.code_at(1_700_000_000).len(), 6);
        assert_eq!(a.remaining_at(1_700_000_000), 10);

        let n = normalize("jbsw y3dp ehpk 3pxp").unwrap();
        assert_eq!(
            n.as_str(),
            "otpauth://totp/?secret=JBSWY3DPEHPK3PXP&algorithm=SHA1&digits=6&period=30"
        );
        assert_eq!(normalize(&n).unwrap().as_str(), n.as_str());
    }

    #[test]
    fn rejects_what_it_cannot_generate() {
        for bad in [
            "",
            "abc",
            "not base32 !!!",
            "otpauth://hotp/x?secret=JBSWY3DPEHPK3PXP&counter=1",
            "otpauth://totp/x?issuer=Example",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&digits=12",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&period=0",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&algorithm=MD5",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
