//! `.2pw` 暗号文フォーマット（共通エンベロープ）
//!
//! ```text
//! magic  "2PW\x01"         4 bytes
//! hlen   u32 little-endian 4 bytes
//! header JSON (UTF-8)      hlen bytes   ┐ ここまで全体を AEAD の AAD にする
//! nonce  24 bytes                        ┘（平文だが改ざんは検知される）
//! ct+tag XChaCha20-Poly1305
//! ```
use crate::{Error, KdfParams, Result, rng};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const MAGIC: &[u8; 4] = b"2PW\x01";
const NONCE_LEN: usize = 24;
const MAX_HEADER: usize = 16 * 1024;
const MAX_PAD: u32 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// 保管庫本体（Vault Key で暗号化）
    Vault,
    /// AUK で包んだ Vault Key
    VaultKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub v: u32,
    pub kind: Kind,
    pub aid: String,
    pub vault_id: String,
    /// 保存のたびに +1。端末は見たことのある最大値を覚え、小さい版を拒否する。
    pub counter: u64,
    /// 平文をこのバイト数の倍数に詰め物する（0 = なし）。サイズから項目数を推測されにくくする。
    pub pad: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kdf: Option<KdfParams>,
}

/// 復号時に「何が来るはずか」を指定する。AAD で認証された header と突き合わせる。
#[derive(Debug, Clone)]
pub struct Expect<'a> {
    pub kind: Kind,
    pub aid: &'a str,
    pub vault_id: &'a str,
    pub min_counter: u64,
}

fn pad(plain: &[u8], block: u32) -> Result<Zeroizing<Vec<u8>>> {
    let len = u32::try_from(plain.len()).map_err(|_| Error::Format("too large"))?;
    let mut buf = Zeroizing::new(Vec::with_capacity(plain.len() + 4 + block as usize));
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(plain);
    if block > 0 {
        let target = buf.len().div_ceil(block as usize) * block as usize;
        buf.resize(target, 0);
    }
    Ok(buf)
}

fn unpad(buf: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if buf.len() < 4 {
        return Err(Error::Format("padding"));
    }
    let len = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
    if len > buf.len() - 4 {
        return Err(Error::Format("padding"));
    }
    Ok(Zeroizing::new(buf[4..4 + len].to_vec()))
}

fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(key.into())
}

pub fn seal(key: &[u8; 32], header: &Header, plaintext: &[u8]) -> Result<Vec<u8>> {
    if header.pad > MAX_PAD || (header.pad != 0 && !header.pad.is_power_of_two()) {
        return Err(Error::Format("pad"));
    }
    let hjson = serde_json::to_vec(header).map_err(|_| Error::Internal("header json"))?;
    if hjson.len() > MAX_HEADER {
        return Err(Error::Format("header too large"));
    }
    let nonce_bytes = rng::array::<NONCE_LEN>()?;

    let mut out = Vec::with_capacity(8 + hjson.len() + NONCE_LEN + plaintext.len() + 64);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(hjson.len() as u32).to_le_bytes());
    out.extend_from_slice(&hjson);
    let aad_end = out.len();
    out.extend_from_slice(&nonce_bytes);

    let padded = pad(plaintext, header.pad)?;
    let nonce = XNonce::from(nonce_bytes);
    let ct = cipher(key)
        .encrypt(
            &nonce,
            Payload {
                msg: &padded,
                aad: &out[..aad_end],
            },
        )
        .map_err(|_| Error::Internal("encrypt"))?;
    out.extend_from_slice(&ct);
    Ok(out)
}

/// 認証前のヘッダを読む（どの鍵・どの保管庫かを判断するためだけに使う。中身を信用しない）。
pub fn peek_header(bytes: &[u8]) -> Result<Header> {
    let (h, _) = split(bytes)?;
    serde_json::from_slice(h).map_err(|_| Error::Format("header"))
}

fn split(bytes: &[u8]) -> Result<(&[u8], usize)> {
    if bytes.len() < 8 || &bytes[..4] != MAGIC {
        return Err(Error::Format("magic"));
    }
    let hlen = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if hlen > MAX_HEADER || bytes.len() < 8 + hlen + NONCE_LEN + 16 {
        return Err(Error::Format("length"));
    }
    Ok((&bytes[8..8 + hlen], 8 + hlen))
}

pub fn open(
    key: &[u8; 32],
    bytes: &[u8],
    expect: &Expect<'_>,
) -> Result<(Header, Zeroizing<Vec<u8>>)> {
    let (_, aad_end) = split(bytes)?;
    let nonce_bytes: [u8; NONCE_LEN] = bytes[aad_end..aad_end + NONCE_LEN].try_into().unwrap();
    let nonce = XNonce::from(nonce_bytes);
    let padded = Zeroizing::new(
        cipher(key)
            .decrypt(
                &nonce,
                Payload {
                    msg: &bytes[aad_end + NONCE_LEN..],
                    aad: &bytes[..aad_end],
                },
            )
            .map_err(|_| Error::Unlock)?,
    );
    // ここから先の header は AEAD で認証済み
    let header = peek_header(bytes)?;
    if header.v != 1
        || header.kind != expect.kind
        || header.aid != expect.aid
        || header.vault_id != expect.vault_id
    {
        return Err(Error::Unlock);
    }
    if header.counter < expect.min_counter {
        return Err(Error::Rollback {
            got: header.counter,
            known: expect.min_counter,
        });
    }
    Ok((header, unpad(&padded)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const KEY: [u8; 32] = [9u8; 32];

    fn header(counter: u64) -> Header {
        Header {
            v: 1,
            kind: Kind::Vault,
            aid: "AB0123".into(),
            vault_id: "v1".into(),
            counter,
            pad: 256,
            kdf: None,
        }
    }
    fn expect(min: u64) -> Expect<'static> {
        Expect {
            kind: Kind::Vault,
            aid: "AB0123",
            vault_id: "v1",
            min_counter: min,
        }
    }

    #[test]
    fn roundtrip_and_padding_hides_length() {
        let a = seal(&KEY, &header(1), b"short").unwrap();
        let b = seal(&KEY, &header(1), b"a little bit longer text").unwrap();
        assert_eq!(a.len(), b.len());
        let (h, pt) = open(&KEY, &a, &expect(0)).unwrap();
        assert_eq!(h.counter, 1);
        assert_eq!(&pt[..], b"short");
    }

    #[test]
    fn nonce_is_random_each_time() {
        assert_ne!(
            seal(&KEY, &header(1), b"x").unwrap(),
            seal(&KEY, &header(1), b"x").unwrap()
        );
    }

    #[test]
    fn every_single_byte_flip_is_detected() {
        let c = seal(&KEY, &header(1), b"secret").unwrap();
        for i in 8..c.len() {
            let mut t = c.clone();
            t[i] ^= 0x01;
            assert!(
                open(&KEY, &t, &expect(0)).is_err(),
                "byte {i} flip not detected"
            );
        }
    }

    #[test]
    fn wrong_key_fails_as_unlock() {
        let c = seal(&KEY, &header(1), b"secret").unwrap();
        assert_eq!(open(&[8u8; 32], &c, &expect(0)).unwrap_err(), Error::Unlock);
    }

    #[test]
    fn header_cannot_be_swapped_to_other_vault_or_kind() {
        let mut h = header(1);
        h.vault_id = "other".into();
        let c = seal(&KEY, &h, b"secret").unwrap();
        assert_eq!(open(&KEY, &c, &expect(0)).unwrap_err(), Error::Unlock);
        let mut h = header(1);
        h.kind = Kind::VaultKey;
        let c = seal(&KEY, &h, b"secret").unwrap();
        assert_eq!(open(&KEY, &c, &expect(0)).unwrap_err(), Error::Unlock);
    }

    #[test]
    fn rollback_is_rejected() {
        let c = seal(&KEY, &header(40), b"old").unwrap();
        assert_eq!(
            open(&KEY, &c, &expect(42)).unwrap_err(),
            Error::Rollback { got: 40, known: 42 }
        );
    }

    #[test]
    fn truncated_input_is_rejected() {
        let c = seal(&KEY, &header(1), b"secret").unwrap();
        for n in [0, 4, 8, 20, c.len() - 1] {
            assert!(open(&KEY, &c[..n], &expect(0)).is_err());
        }
    }

    proptest! {
        #[test]
        fn roundtrip_any(v in proptest::collection::vec(any::<u8>(), 0..5000)) {
            let c = seal(&KEY, &header(3), &v).unwrap();
            let (_, pt) = open(&KEY, &c, &expect(3)).unwrap();
            prop_assert_eq!(&pt[..], &v[..]);
        }
    }
}
