//! Windows Hello / Touch ID での解除。
//!
//! 有効にすると、アカウントパスワードを「OS の本人確認を通ったときだけ取り出せる鍵」
//! （`vault_os_auth`）で暗号化して、この端末の設定フォルダに置く。解除するときは、
//! 本人確認 → パスワードを復号 → いつもどおり `Session::unlock`（Argon2id・差し戻し検知もそのまま）。
//!
//! - 置くのは暗号文だけ。鍵は Windows では TPM などの中の鍵の署名から毎回作り、保存しない
//!   （macOS ではキーチェーンに置き、Touch ID の確認を通ったときだけ読む）
//! - 保管場所（Google Drive など）には何も置かない。端末ごとに設定する
//! - ほかの端末でアカウントパスワードを変えたら、この端末の設定は使えなくなる（解除時に案内して外す）
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use zeroize::Zeroizing;

use crate::config::{self, Config};
use crate::{R, Session};

const FILE: &str = "os-unlock.json";
const AAD_PREFIX: &[u8] = b"2password os-unlock v1\0";

#[derive(Serialize, Deserialize)]
struct Stored {
    v: u32,
    aid: String,
    salt: String,
    nonce: String,
    ct: String,
    /// アプリの起動時（PC へのログイン時の自動起動を含む）に、すぐ本人確認を出す
    #[serde(default = "yes")]
    unlock_at_start: bool,
}

fn yes() -> bool {
    true
}

fn path() -> PathBuf {
    config::home().join(FILE)
}

/// OS 側の鍵の名前。同じ PC で設定フォルダを変えて試せるよう、フォルダごとに分ける
fn label(aid: &str) -> String {
    let h = Sha256::digest(format!("{aid}@{}", config::home().display()).as_bytes());
    let short: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("2password-unlock-{short}")
}

fn load() -> Option<Stored> {
    let bytes = std::fs::read(path()).ok()?;
    serde_json::from_slice::<Stored>(&bytes).ok().filter(|s| s.v == 1)
}

fn save(s: &Stored) -> R<()> {
    std::fs::create_dir_all(config::home())?;
    crate::store::write_atomic(&path(), &serde_json::to_vec_pretty(s)?)
}

fn aad(aid: &str) -> Vec<u8> {
    [AAD_PREFIX, aid.as_bytes()].concat()
}

fn aead_key(key: &[u8; 32]) -> R<LessSafeKey> {
    Ok(LessSafeKey::new(
        UnboundKey::new(&CHACHA20_POLY1305, key).map_err(|_| "内部エラー: aead")?,
    ))
}

fn seal(key: &[u8; 32], aid: &str, pw: &SecretString) -> R<(String, String)> {
    let nonce = vault_core::rng::array::<12>()?;
    let mut buf = Zeroizing::new(pw.expose_secret().as_bytes().to_vec());
    aead_key(key)?
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad(aid)),
            &mut *buf,
        )
        .map_err(|_| "内部エラー: aead")?;
    Ok((B64.encode(nonce), B64.encode(&*buf)))
}

fn open_sealed(key: &[u8; 32], s: &Stored) -> Option<SecretString> {
    let nonce: [u8; 12] = B64.decode(&s.nonce).ok()?.try_into().ok()?;
    let mut buf = Zeroizing::new(B64.decode(&s.ct).ok()?);
    let plain = aead_key(key)
        .ok()?
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad(&s.aid)),
            &mut buf,
        )
        .ok()?;
    let text = Zeroizing::new(String::from_utf8(plain.to_vec()).ok()?);
    Some(SecretString::from(text.as_str()))
}

fn os_error(e: vault_os_auth::Error) -> Box<dyn std::error::Error + Send + Sync> {
    match e {
        vault_os_auth::Error::Unavailable => {
            "Windows Hello / Touch ID を使えません。OS の設定で、顔・指紋・PIN のいずれかを設定してください".into()
        }
        vault_os_auth::Error::Canceled => "本人確認が取り消されました".into(),
        vault_os_auth::Error::NotFound => {
            "Windows Hello / Touch ID の鍵が見つかりません。アカウントパスワードで解除してから、設定し直してください".into()
        }
        vault_os_auth::Error::Failed(m) => format!("本人確認に失敗しました（{m}）").into(),
    }
}

// ---------------- 公開する操作 ----------------

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// "windows-hello" / "touch-id" / "none"
    pub method: &'static str,
    /// この端末で使えるか（OS で顔・指紋・PIN が設定済みか）
    pub available: bool,
    /// この端末で有効にしてあるか
    pub enabled: bool,
    pub unlock_at_start: bool,
}

/// この OS で使う本人確認の種類（"windows-hello" / "touch-id" / "none"）
pub fn method() -> &'static str {
    vault_os_auth::method().as_str()
}

/// 状態を調べる（本人確認の画面は出さない）
pub fn status() -> Status {
    let stored = enabled_for_current_account();
    Status {
        method: vault_os_auth::method().as_str(),
        available: vault_os_auth::available(),
        enabled: stored.is_some(),
        unlock_at_start: stored.is_some_and(|s| s.unlock_at_start),
    }
}

fn enabled_for_current_account() -> Option<Stored> {
    let cfg = Config::load().ok()?;
    load().filter(|s| s.aid == cfg.aid)
}

pub fn is_enabled() -> bool {
    enabled_for_current_account().is_some()
}

pub fn unlock_at_start() -> bool {
    enabled_for_current_account().is_some_and(|s| s.unlock_at_start)
}

/// 有効にする。呼び出し側は、解除済みのセッションで `Session::verify_password` を通してから呼ぶ
/// （本人確認の画面を待つあいだ、セッションのロックを握らないよう分けてある）。
/// `reason` は macOS の Touch ID の画面に出す文。
pub fn enable(aid: &str, pw: &SecretString, reason: &str) -> R<()> {
    let cfg = Config::load()?;
    if cfg.aid != aid {
        return Err("保管場所のアカウントがこの端末の設定と一致しません".into());
    }
    let aid = cfg.aid;
    let salt = vault_core::rng::array::<32>()?;
    let key = vault_os_auth::create(&label(&aid), &salt, reason).map_err(os_error)?;
    let (nonce, ct) = seal(&key, &aid, pw)?;
    save(&Stored {
        v: 1,
        aid,
        salt: B64.encode(salt),
        nonce,
        ct,
        unlock_at_start: true,
    })
}

/// アカウントパスワードを変えたときに、新しいパスワードで包み直す（本人確認の画面が出る）。
/// 有効にしていなければ何もしない。
pub fn rewrap(new_pw: &SecretString, reason: &str) -> R<()> {
    let Some(mut s) = enabled_for_current_account() else {
        return Ok(());
    };
    let salt = B64.decode(&s.salt)?;
    let key = vault_os_auth::open(&label(&s.aid), &salt, reason).map_err(os_error)?;
    let (nonce, ct) = seal(&key, &s.aid, new_pw)?;
    s.nonce = nonce;
    s.ct = ct;
    save(&s)
}

pub fn set_unlock_at_start(on: bool) -> R<()> {
    let mut s = enabled_for_current_account()
        .ok_or("Windows Hello / Touch ID での解除が設定されていません")?;
    s.unlock_at_start = on;
    save(&s)
}

/// 無効にする（OS 側の鍵と、この端末の暗号文を消す）
pub fn disable() {
    if let Some(s) = load() {
        vault_os_auth::remove(&label(&s.aid));
    }
    let _ = std::fs::remove_file(path());
}

/// 本人確認をして保管庫を解除する。
///
/// パスワードが古い（ほかの端末で変更された）ときと、OS 側の鍵が変わったときは、
/// 設定を外して、アカウントパスワードでの解除を案内する。
pub fn unlock(reason: &str) -> R<Session> {
    let s = enabled_for_current_account()
        .ok_or("Windows Hello / Touch ID での解除が設定されていません")?;
    let salt = B64.decode(&s.salt)?;
    let key = match vault_os_auth::open(&label(&s.aid), &salt, reason) {
        Ok(k) => k,
        Err(vault_os_auth::Error::NotFound) => {
            disable();
            return Err(os_error(vault_os_auth::Error::NotFound));
        }
        Err(e) => return Err(os_error(e)),
    };
    let Some(pw) = open_sealed(&key, &s) else {
        disable();
        return Err("Windows Hello / Touch ID の鍵が変わったため、解除できません。アカウントパスワードで解除してから、設定し直してください".into());
    };
    match Session::unlock(&pw) {
        Ok(session) => Ok(session),
        Err(e) if e.downcast_ref::<vault_core::Error>() == Some(&vault_core::Error::Unlock) => {
            disable();
            Err("アカウントパスワードが変更されたため、Windows Hello / Touch ID では解除できません。新しいアカウントパスワードで解除してから、設定し直してください".into())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_round_trip_and_binds_account() {
        let key = [9u8; 32];
        let pw = SecretString::from("correct horse battery staple");
        let (nonce, ct) = seal(&key, "A1B2C3", &pw).unwrap();
        let mut s = Stored {
            v: 1,
            aid: "A1B2C3".into(),
            salt: String::new(),
            nonce,
            ct,
            unlock_at_start: true,
        };
        assert_eq!(
            open_sealed(&key, &s).unwrap().expose_secret(),
            "correct horse battery staple"
        );
        // 鍵が違う・アカウントが違う（暗号文の付け替え）なら開かない
        assert!(open_sealed(&[8u8; 32], &s).is_none());
        s.aid = "ZZZZZZ".into();
        assert!(open_sealed(&key, &s).is_none());
    }
}
