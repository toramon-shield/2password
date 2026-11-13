//! Two-Secret Key Derivation（2SKD）
//!
//! ```text
//! P'   = NFKC(password)
//! k_pw = Argon2id(P', salt = salt16 || AID, m, t, p, 32 bytes)
//! k_sk = HKDF-SHA256(ikm = SecretKey, salt = AID, info = "2password/secret-key/v1", 32 bytes)
//! AUK  = k_pw XOR k_sk
//! ```
use crate::{Error, Result, SecretKey, rng};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use hkdf::Hkdf;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

pub const HKDF_INFO_SECRET_KEY: &[u8] = b"2password/secret-key/v1";

/// ファイルにどんな値が書かれていても、クライアントはこの下限を強制する（弱体化攻撃の防止）。
pub const MIN_M_KIB: u32 = 64 * 1024;
pub const MIN_T: u32 = 3;
pub const MIN_P: u32 = 1;
/// 悪意あるファイルでメモリを使い果たさせる攻撃（DoS）を防ぐ上限。
pub const MAX_M_KIB: u32 = 1024 * 1024;
pub const MAX_T: u32 = 20;
pub const MAX_P: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub alg: String,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    /// 16 バイトの乱数（Base64）
    pub salt: String,
}

impl KdfParams {
    pub fn new_default() -> Result<Self> {
        Ok(Self {
            alg: "argon2id".into(),
            m_kib: MIN_M_KIB,
            t: MIN_T,
            p: MIN_P,
            salt: B64.encode(rng::array::<16>()?),
        })
    }

    pub fn validate(&self) -> Result<()> {
        let ok = self.alg == "argon2id"
            && (MIN_M_KIB..=MAX_M_KIB).contains(&self.m_kib)
            && (MIN_T..=MAX_T).contains(&self.t)
            && (MIN_P..=MAX_P).contains(&self.p);
        if !ok {
            return Err(Error::WeakKdf);
        }
        match B64.decode(&self.salt) {
            Ok(s) if s.len() == 16 => Ok(()),
            _ => Err(Error::Format("salt")),
        }
    }
}

/// Account Unlock Key。メモリ上にだけ存在し、Drop 時に 0 で上書きされる。
pub struct Auk(Zeroizing<[u8; 32]>);

impl core::fmt::Debug for Auk {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Auk(***)")
    }
}

impl Auk {
    pub(crate) fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

/// パスワードを NFKC 正規化する（全角/半角カナ・合成文字の入力ゆれで開けなくなる事故を防ぐ）。
pub fn normalize_password(password: &SecretString) -> Zeroizing<String> {
    Zeroizing::new(password.expose_secret().nfkc().collect())
}

pub fn derive_auk(password: &SecretString, sk: &SecretKey, params: &KdfParams) -> Result<Auk> {
    params.validate()?;
    let aid = sk.aid().as_str().as_bytes();

    // ① パスワード → Argon2id
    let pw = normalize_password(password);
    let mut salt = Zeroizing::new(
        B64.decode(&params.salt)
            .map_err(|_| Error::Format("salt"))?,
    );
    salt.extend_from_slice(aid);
    let argon = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(params.m_kib, params.t, params.p, Some(32)).map_err(|_| Error::WeakKdf)?,
    );
    let mut k_pw = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(pw.as_bytes(), &salt, &mut *k_pw)
        .map_err(|_| Error::Internal("argon2"))?;

    // ② Secret Key → HKDF-SHA256
    let mut k_sk = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(aid), sk.expose())
        .expand(HKDF_INFO_SECRET_KEY, &mut *k_sk)
        .map_err(|_| Error::Internal("hkdf"))?;

    // ③ XOR で合成
    let mut auk = Zeroizing::new([0u8; 32]);
    for i in 0..32 {
        auk[i] = k_pw[i] ^ k_sk[i];
    }
    Ok(Auk(auk))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AccountId;

    fn fixed() -> (SecretKey, KdfParams) {
        let sk = SecretKey::from_bytes(AccountId::parse("AB0123").unwrap(), [7u8; 16]);
        let params = KdfParams {
            alg: "argon2id".into(),
            m_kib: MIN_M_KIB,
            t: MIN_T,
            p: MIN_P,
            salt: B64.encode([1u8; 16]),
        };
        (sk, params)
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Known-Answer Test：実装やクレート更新で導出結果が変わったら気づけるようにする。
    #[test]
    fn known_answer() {
        let (sk, params) = fixed();
        let auk = derive_auk(
            &SecretString::from("correct horse battery staple"),
            &sk,
            &params,
        )
        .unwrap();
        assert_eq!(hex(auk.expose()), KAT_AUK, "AUK の導出結果が変わりました");
    }
    /// 独立実装（Python: argon2-cffi + cryptography の HKDF）で計算した値と一致することを確認済み。
    const KAT_AUK: &str = "3aafa26aafeeee62ac129204d45294de66e5f627ffc93449a3fbf71b297a1f8b";

    #[test]
    fn nfkc_makes_halfwidth_and_fullwidth_equal() {
        let (sk, params) = fixed();
        let a = derive_auk(&SecretString::from("ｶﾞｷﾞ１２３"), &sk, &params).unwrap();
        let b = derive_auk(&SecretString::from("ガギ123"), &sk, &params).unwrap();
        assert_eq!(a.expose(), b.expose());
    }

    #[test]
    fn different_secret_key_gives_different_auk() {
        let (sk, params) = fixed();
        let sk2 = SecretKey::from_bytes(sk.aid().clone(), [8u8; 16]);
        let pw = SecretString::from("pw");
        assert_ne!(
            derive_auk(&pw, &sk, &params).unwrap().expose(),
            derive_auk(&pw, &sk2, &params).unwrap().expose()
        );
    }

    #[test]
    fn rejects_weakened_params() {
        let (sk, mut params) = fixed();
        params.m_kib = 1024;
        assert_eq!(
            derive_auk(&SecretString::from("pw"), &sk, &params).unwrap_err(),
            Error::WeakKdf
        );
        let (_, mut params) = fixed();
        params.t = 1;
        assert_eq!(params.validate().unwrap_err(), Error::WeakKdf);
        let (_, mut params) = fixed();
        params.m_kib = u32::MAX;
        assert_eq!(params.validate().unwrap_err(), Error::WeakKdf);
    }
}
