//! パスキー（WebAuthn）の作成と署名。
//!
//! 2password は、ブラウザ拡張機能を通して「パスキーの保管場所（認証器）」としてふるまう。
//!
//! - 秘密鍵（P-256）は保管庫の暗号文の中に置き、署名はこのアプリの中だけで行う。拡張機能には渡さない
//! - 署名に含める「どのサイトか」（origin）は、ページの申告ではなくブラウザが教えるものを使う
//! - パスキーは作ったサイト（rpId）に結びつく。別のサイトの署名には使えない
//!
//! 形式は WebAuthn Level 2 の「none」アテステーション、アルゴリズムは ES256（COSE -7）。
use crate::R;
use crate::links::registrable;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use reqwest::Url;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};
use sha2::{Digest, Sha256};
use vault_core::Passkey;
use zeroize::Zeroizing;

/// 2password を表す認証器の識別子（AAGUID）。値そのものに意味はない
pub const AAGUID: [u8; 16] = [
    0x32, 0x70, 0x77, 0x2d, 0x74, 0x6f, 0x72, 0x61, 0x6d, 0x6f, 0x6e, 0x2d, 0x76, 0x31, 0x00, 0x01,
];

const FLAG_UP: u8 = 0x01; // 利用者がその場にいる
const FLAG_UV: u8 = 0x04; // 利用者を確認した（保管庫をアカウントパスワードで解除している）
const FLAG_BE: u8 = 0x08; // 複数の端末で同期できる
const FLAG_BS: u8 = 0x10; // 同期されている
const FLAG_AT: u8 = 0x40; // 資格情報のデータが続く

/// P-256 の公開鍵（非圧縮の 65 バイト）の前に付ける SubjectPublicKeyInfo の固定部分
const SPKI_P256_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

const MAX_CHALLENGE: usize = 1024;
const MAX_USER_HANDLE: usize = 64;

pub fn b64(bytes: &[u8]) -> String {
    B64URL.encode(bytes)
}

pub fn unb64(text: &str) -> R<Vec<u8>> {
    B64URL
        .decode(text.trim_end_matches('='))
        .map_err(|_| "base64url の形式が正しくありません".into())
}

/// そのページ（origin）が、その rpId のパスキーを使ってよいか。
///
/// - https のページだけ（開発用の `http://localhost` は除く）
/// - rpId は、ページのホスト名そのものか、その上位のドメインであること
/// - `com` や `co.jp` のような、他人と共有する範囲を rpId にはできない
pub fn rp_id_allowed(rp_id: &str, origin: &Url) -> bool {
    let Some(host) = origin.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    if rp_id.is_empty() || rp_id != rp_id.to_ascii_lowercase() || rp_id.contains([':', '/', '*']) {
        return false;
    }
    if host == "localhost" {
        return rp_id == "localhost";
    }
    if origin.scheme() != "https" {
        return false;
    }
    // IP アドレスや、ドットのないホスト名では使えない
    if registrable(&host).is_none() || registrable(rp_id).is_none() {
        return false;
    }
    host == rp_id || host.ends_with(&format!(".{rp_id}"))
}

fn client_data(kind: &str, challenge: &[u8], origin: &Url) -> R<Vec<u8>> {
    // 項目の順序は WebAuthn の仕様どおり（type, challenge, origin, crossOrigin）
    Ok(format!(
        r#"{{"type":{},"challenge":{},"origin":{},"crossOrigin":false}}"#,
        serde_json::to_string(kind)?,
        serde_json::to_string(&b64(challenge))?,
        serde_json::to_string(&origin.origin().ascii_serialization())?,
    )
    .into_bytes())
}

fn decode_challenge(challenge: &str) -> R<Vec<u8>> {
    let bytes = unb64(challenge)?;
    if bytes.is_empty() || bytes.len() > MAX_CHALLENGE {
        return Err("チャレンジの長さが正しくありません".into());
    }
    Ok(bytes)
}

fn authenticator_data(rp_id: &str, flags: u8, attested: Option<&[u8]>) -> Vec<u8> {
    let mut out = Sha256::digest(rp_id.as_bytes()).to_vec();
    out.push(flags);
    // 署名回数は常に 0（同期するパスキーでは、端末ごとの回数に意味がないため）
    out.extend_from_slice(&[0, 0, 0, 0]);
    if let Some(a) = attested {
        out.extend_from_slice(a);
    }
    out
}

/// CBOR のバイト列（major type 2）の見出し
fn cbor_bytes_head(len: usize) -> Vec<u8> {
    match len {
        0..=23 => vec![0x40 | len as u8],
        24..=255 => vec![0x58, len as u8],
        _ => vec![0x59, (len >> 8) as u8, len as u8],
    }
}

/// 公開鍵を COSE_Key（EC2 / P-256 / ES256）で表す
fn cose_key(public: &[u8]) -> Vec<u8> {
    let (x, y) = (&public[1..33], &public[33..65]);
    let mut out = vec![
        0xa5, // 5 項目の map
        0x01, 0x02, // kty: EC2
        0x03, 0x26, // alg: ES256 (-7)
        0x20, 0x01, // crv: P-256
        0x21, 0x58, 0x20, // x
    ];
    out.extend_from_slice(x);
    out.extend_from_slice(&[0x22, 0x58, 0x20]); // y
    out.extend_from_slice(y);
    out
}

fn key_pair(private_key: &str) -> R<EcdsaKeyPair> {
    let pkcs8 = Zeroizing::new(unb64(private_key)?);
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &SystemRandom::new())
        .map_err(|_| "パスキーの秘密鍵を読めません".into())
}

/// サイトに返す、パスキー作成の結果（すべて base64url）
pub struct Registration {
    pub credential_id: String,
    pub client_data_json: String,
    pub attestation_object: String,
    pub authenticator_data: String,
    /// SubjectPublicKeyInfo（DER）
    pub public_key: String,
}

pub struct NewUser<'a> {
    pub handle: &'a str,
    pub name: &'a str,
    pub display_name: &'a str,
}

/// 新しいパスキーを作る。返り値は（保管庫に入れるもの、サイトに返すもの）。
pub fn create(
    rp_id: &str,
    origin: &Url,
    challenge: &str,
    user: &NewUser<'_>,
    now_ms: u64,
) -> R<(Passkey, Registration)> {
    if !rp_id_allowed(rp_id, origin) {
        return Err("このページでは、このサイト名のパスキーを作れません".into());
    }
    let challenge = decode_challenge(challenge)?;
    let handle = unb64(user.handle)?;
    if handle.is_empty() || handle.len() > MAX_USER_HANDLE {
        return Err("利用者の識別子の長さが正しくありません".into());
    }

    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
        .map_err(|_| "鍵を作れませんでした")?;
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
        .map_err(|_| "鍵を作れませんでした")?;
    let public = pair.public_key().as_ref();
    if public.len() != 65 || public[0] != 0x04 {
        return Err("公開鍵の形式が正しくありません".into());
    }
    let credential_id = vault_core::rng::array::<32>()?;

    let mut attested = AAGUID.to_vec();
    attested.extend_from_slice(&(credential_id.len() as u16).to_be_bytes());
    attested.extend_from_slice(&credential_id);
    attested.extend_from_slice(&cose_key(public));
    let auth_data = authenticator_data(
        rp_id,
        FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS | FLAG_AT,
        Some(&attested),
    );

    // attestationObject = { "fmt": "none", "attStmt": {}, "authData": bytes }
    let mut att = vec![0xa3];
    att.extend_from_slice(&[0x63, b'f', b'm', b't', 0x64, b'n', b'o', b'n', b'e']);
    att.extend_from_slice(&[0x67, b'a', b't', b't', b'S', b't', b'm', b't', 0xa0]);
    att.extend_from_slice(&[0x68, b'a', b'u', b't', b'h', b'D', b'a', b't', b'a']);
    att.extend_from_slice(&cbor_bytes_head(auth_data.len()));
    att.extend_from_slice(&auth_data);

    let mut spki = SPKI_P256_PREFIX.to_vec();
    spki.extend_from_slice(public);

    let passkey = Passkey {
        credential_id: b64(&credential_id),
        rp_id: rp_id.to_string(),
        user_handle: b64(&handle),
        user_name: user.name.to_string(),
        user_display_name: user.display_name.to_string(),
        private_key: b64(pkcs8.as_ref()),
        created_at: now_ms,
    };
    let registration = Registration {
        credential_id: passkey.credential_id.clone(),
        client_data_json: b64(&client_data("webauthn.create", &challenge, origin)?),
        attestation_object: b64(&att),
        authenticator_data: b64(&auth_data),
        public_key: b64(&spki),
    };
    Ok((passkey, registration))
}

/// サイトに返す、ログインの署名（すべて base64url）
pub struct Assertion {
    pub credential_id: String,
    pub client_data_json: String,
    pub authenticator_data: String,
    pub signature: String,
    pub user_handle: String,
}

/// パスキーで、サイトが出したチャレンジに署名する。
pub fn assert(passkey: &Passkey, rp_id: &str, origin: &Url, challenge: &str) -> R<Assertion> {
    // 作ったときと違うサイト名、そのサイト名を使えないページには署名しない（偽サイト対策）
    if passkey.rp_id != rp_id || !rp_id_allowed(rp_id, origin) {
        return Err("このページでは、このパスキーを使えません".into());
    }
    let challenge = decode_challenge(challenge)?;
    let client = client_data("webauthn.get", &challenge, origin)?;
    let auth_data = authenticator_data(rp_id, FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS, None);

    let mut signed = auth_data.clone();
    signed.extend_from_slice(&Sha256::digest(&client));
    let signature = key_pair(&passkey.private_key)?
        .sign(&SystemRandom::new(), &signed)
        .map_err(|_| "署名できませんでした")?;

    Ok(Assertion {
        credential_id: passkey.credential_id.clone(),
        client_data_json: b64(&client),
        authenticator_data: b64(&auth_data),
        signature: b64(signature.as_ref()),
        user_handle: passkey.user_handle.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::page_origin;
    use ring::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};

    fn origin(s: &str) -> Url {
        page_origin(s).unwrap()
    }
    fn user() -> NewUser<'static> {
        NewUser {
            handle: "dXNlci0x",
            name: "alice@example.com",
            display_name: "Alice",
        }
    }

    #[test]
    fn rp_id_must_belong_to_the_page() {
        let ok = |rp: &str, o: &str| rp_id_allowed(rp, &origin(o));
        assert!(ok("example.com", "https://example.com"));
        assert!(ok("example.com", "https://login.example.com"));
        assert!(ok("login.example.com", "https://login.example.com"));
        assert!(ok("example.co.jp", "https://www.example.co.jp"));
        assert!(ok("localhost", "http://localhost:3000"));
        // 別のサイト、下位のホスト、共有の範囲、暗号化されていないページ
        assert!(!ok("example.com", "https://example.com.evil.test"));
        assert!(!ok("example.com", "https://evilexample.com"));
        assert!(!ok("login.example.com", "https://example.com"));
        assert!(!ok("com", "https://example.com"));
        assert!(!ok("co.jp", "https://example.co.jp"));
        assert!(!ok("github.io", "https://someone.github.io"));
        assert!(!ok("example.com", "http://example.com"));
        assert!(!ok("192.168.0.1", "https://192.168.0.1"));
        assert!(!ok("Example.com", "https://example.com"));
        assert!(!ok("", "https://example.com"));
    }

    #[test]
    fn registration_has_the_expected_layout() {
        let o = origin("https://login.example.com");
        let (pk, reg) = create("example.com", &o, "Y2hhbGxlbmdl", &user(), 1).unwrap();
        assert_eq!(pk.rp_id, "example.com");
        assert_eq!(pk.user_name, "alice@example.com");

        let client = String::from_utf8(unb64(&reg.client_data_json).unwrap()).unwrap();
        assert_eq!(
            client,
            r#"{"type":"webauthn.create","challenge":"Y2hhbGxlbmdl","origin":"https://login.example.com","crossOrigin":false}"#
        );

        let auth = unb64(&reg.authenticator_data).unwrap();
        assert_eq!(&auth[..32], Sha256::digest(b"example.com").as_slice());
        assert_eq!(auth[32], 0x5d);
        assert_eq!(&auth[33..37], &[0, 0, 0, 0]);
        assert_eq!(&auth[37..53], &AAGUID);
        assert_eq!(&auth[53..55], &[0, 32]);
        assert_eq!(b64(&auth[55..87]), reg.credential_id);
        // COSE の鍵（77 バイト）で終わる
        assert_eq!(auth.len(), 87 + 77);
        assert_eq!(&auth[87..97], &[0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x58, 0x20]);

        let att = unb64(&reg.attestation_object).unwrap();
        assert!(att.ends_with(&auth));
        assert_eq!(att[0], 0xa3);

        // 公開鍵（SPKI）の中身は、COSE の鍵の x と y と同じ
        let spki = unb64(&reg.public_key).unwrap();
        assert_eq!(spki.len(), 91);
        assert_eq!(&spki[27..59], &auth[97..129]);
        assert_eq!(&spki[59..91], &auth[132..164]);
    }

    #[test]
    fn assertion_verifies_with_the_registered_public_key() {
        let o = origin("https://example.com");
        let (pk, reg) = create("example.com", &o, "Y2hhbGxlbmdl", &user(), 1).unwrap();
        let a = assert(&pk, "example.com", &o, "bG9naW4tY2hhbGxlbmdl").unwrap();
        assert_eq!(a.credential_id, reg.credential_id);
        assert_eq!(a.user_handle, "dXNlci0x");

        let auth = unb64(&a.authenticator_data).unwrap();
        assert_eq!(auth.len(), 37);
        assert_eq!(auth[32], 0x1d);
        let client = unb64(&a.client_data_json).unwrap();
        assert!(String::from_utf8_lossy(&client).contains(r#""type":"webauthn.get""#));

        let mut signed = auth.clone();
        signed.extend_from_slice(&Sha256::digest(&client));
        let spki = unb64(&reg.public_key).unwrap();
        let key = UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, &spki[26..]);
        key.verify(&signed, &unb64(&a.signature).unwrap()).unwrap();
        // 別のデータには通らない
        signed[0] ^= 1;
        assert!(key.verify(&signed, &unb64(&a.signature).unwrap()).is_err());
    }

    #[test]
    fn passkey_is_bound_to_its_site() {
        let o = origin("https://example.com");
        let (pk, _) = create("example.com", &o, "Y2hhbGxlbmdl", &user(), 1).unwrap();
        // 偽サイトが、本物のサイト名を名乗っても署名しない
        let fake = origin("https://example.com.evil.test");
        assert!(assert(&pk, "example.com", &fake, "Y2hhbGxlbmdl").is_err());
        // 偽サイトが、自分のサイト名で求めても署名しない
        assert!(assert(&pk, "example.com.evil.test", &fake, "Y2hhbGxlbmdl").is_err());
        // 暗号化されていないページには署名しない
        assert!(assert(&pk, "example.com", &origin("http://example.com"), "Y2hhbGxlbmdl").is_err());
        // 作るときも、ページに合わないサイト名は拒否する
        assert!(create("example.com", &fake, "Y2hhbGxlbmdl", &user(), 1).is_err());
        assert!(create("example.com", &o, "", &user(), 1).is_err());
        assert!(create("example.com", &o, "!!!", &user(), 1).is_err());
    }
}
