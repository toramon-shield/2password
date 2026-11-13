//! account.2pw：Drive に置く「鍵の箱」。秘密は含まない（Vault Key は AUK で包んである）。
use crate::envelope::{self, Expect, Header, Kind};
use crate::kdf::{self, KdfParams};
use crate::{AccountId, Error, Result, SecretKey, rng};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountFile {
    pub v: u32,
    pub aid: String,
    pub vault_id: String,
    pub kdf: KdfParams,
    /// kind = vault-key のエンベロープ（Base64）
    pub wrapped_vault_key: String,
    pub created_at: u64,
    /// パスワード変更のたびに +1 する世代番号。包んだエンベロープの認証済み counter と一致させ、
    /// 端末は見たことのある最大値より古い account.2pw（旧パスワードの箱）を拒否する。
    /// この欄がない旧形式のファイルは 0 として読む（旧形式の counter も 0 なので互換）。
    #[serde(default)]
    pub epoch: u64,
}

/// vault_id は 32 桁の小文字16進数（UUID の simple 形式）。ファイル名や Drive の検索式に入るので形式を固定する。
pub fn is_valid_vault_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// 保管庫を暗号化する 256bit のランダム鍵。
pub struct VaultKey(Zeroizing<[u8; 32]>);

impl core::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("VaultKey(***)")
    }
}

impl VaultKey {
    pub(crate) fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug)]
pub struct NewAccount {
    pub account: AccountFile,
    pub secret_key: SecretKey,
    pub vault_key: VaultKey,
}

fn new_vault_id() -> Result<String> {
    // 乱数の失敗は握りつぶさない（予測できる ID で続行しない）
    Ok(uuid::Builder::from_random_bytes(rng::array::<16>()?)
        .into_uuid()
        .simple()
        .to_string())
}

fn wrap(
    vk: &VaultKey,
    password: &SecretString,
    sk: &SecretKey,
    vault_id: &str,
    kdf: &KdfParams,
    epoch: u64,
) -> Result<String> {
    let auk = kdf::derive_auk(password, sk, kdf)?;
    let header = Header {
        v: 1,
        kind: Kind::VaultKey,
        aid: sk.aid().as_str().into(),
        vault_id: vault_id.into(),
        counter: epoch,
        pad: 0,
        kdf: Some(kdf.clone()),
    };
    Ok(B64.encode(envelope::seal(auk.expose(), &header, vk.expose())?))
}

impl AccountFile {
    /// 新しいアカウント：アカウントID・Secret Key・Vault Key を OS の乱数で生成する。
    pub fn create(password: &SecretString, now_ms: u64) -> Result<NewAccount> {
        let aid = AccountId::generate()?;
        let secret_key = SecretKey::generate(aid.clone())?;
        let vault_key = VaultKey(Zeroizing::new(rng::array::<32>()?));
        let vault_id = new_vault_id()?;
        let kdf = KdfParams::new_default()?;
        let wrapped_vault_key = wrap(&vault_key, password, &secret_key, &vault_id, &kdf, 0)?;
        Ok(NewAccount {
            account: AccountFile {
                v: 1,
                aid: aid.as_str().into(),
                vault_id,
                kdf,
                wrapped_vault_key,
                created_at: now_ms,
                epoch: 0,
            },
            secret_key,
            vault_key,
        })
    }

    /// パスワードと Secret Key から Vault Key を取り出す（世代番号の下限なし。初回参加・デモ用）。
    pub fn unlock(&self, password: &SecretString, sk: &SecretKey) -> Result<VaultKey> {
        self.unlock_at_least(password, sk, 0)
    }

    /// `min_epoch` より古い世代（パスワード変更前の箱）は `Error::Rollback` で拒否する。
    pub fn unlock_at_least(
        &self,
        password: &SecretString,
        sk: &SecretKey,
        min_epoch: u64,
    ) -> Result<VaultKey> {
        if self.v != 1 {
            return Err(Error::Format("account version"));
        }
        if sk.aid().as_str() != self.aid {
            return Err(Error::AccountMismatch);
        }
        let auk = kdf::derive_auk(password, sk, &self.kdf)?;
        let bytes = B64
            .decode(&self.wrapped_vault_key)
            .map_err(|_| Error::Format("wrapped_vault_key"))?;
        let (header, pt) = envelope::open(
            auk.expose(),
            &bytes,
            &Expect {
                kind: Kind::VaultKey,
                aid: &self.aid,
                vault_id: &self.vault_id,
                min_counter: min_epoch,
            },
        )?;
        // 認証済みヘッダの KDF パラメータと世代番号がファイル本体と一致すること
        if header.kdf.as_ref() != Some(&self.kdf) || header.counter != self.epoch || pt.len() != 32
        {
            return Err(Error::Unlock);
        }
        let mut k = Zeroizing::new([0u8; 32]);
        k.copy_from_slice(&pt);
        Ok(VaultKey(k))
    }

    /// パスワード変更：新しい salt で AUK を作り直し、Vault Key だけを包み直す（保管庫本体はそのまま）。
    /// 世代番号を +1 するので、古い account.2pw を書き戻されても新しい世代を知る端末は受け付けない。
    pub fn change_password(
        &self,
        vk: &VaultKey,
        new_password: &SecretString,
        sk: &SecretKey,
    ) -> Result<AccountFile> {
        let kdf = KdfParams {
            salt: KdfParams::new_default()?.salt,
            ..self.kdf.clone()
        };
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or(Error::Internal("account epoch exhausted"))?;
        Ok(AccountFile {
            kdf: kdf.clone(),
            wrapped_vault_key: wrap(vk, new_password, sk, &self.vault_id, &kdf, epoch)?,
            epoch,
            ..self.clone()
        })
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("serializable")
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let a: Self = serde_json::from_slice(bytes).map_err(|_| Error::Format("account.2pw"))?;
        // 正規化前後で値が変わる表記（小文字など）も受け付けない
        if AccountId::parse(&a.aid)?.as_str() != a.aid {
            return Err(Error::Format("アカウントID"));
        }
        if !is_valid_vault_id(&a.vault_id) {
            return Err(Error::Format("vault_id"));
        }
        a.kdf.validate()?;
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_unlock_and_failures() {
        let pw = SecretString::from("とても長いパスフレーズ 2026");
        let new = AccountFile::create(&pw, 0).unwrap();
        let acct = AccountFile::from_json(&new.account.to_json()).unwrap();

        let vk = acct.unlock(&pw, &new.secret_key).unwrap();
        assert_eq!(vk.expose(), new.vault_key.expose());

        // パスワード違い
        assert_eq!(
            acct.unlock(&SecretString::from("wrong"), &new.secret_key)
                .unwrap_err(),
            Error::Unlock
        );
        // Secret Key 違い（同じアカウントID・別の乱数）
        let other = SecretKey::from_bytes(new.secret_key.aid().clone(), [0u8; 16]);
        assert_eq!(acct.unlock(&pw, &other).unwrap_err(), Error::Unlock);

        // パスワード変更後：新しいパスワードで開け、古いパスワードでは開けない
        let pw2 = SecretString::from("新しいパスフレーズ 2027");
        let acct2 = acct.change_password(&vk, &pw2, &new.secret_key).unwrap();
        assert_eq!(
            acct2.unlock(&pw2, &new.secret_key).unwrap().expose(),
            vk.expose()
        );
        assert_eq!(
            acct2.unlock(&pw, &new.secret_key).unwrap_err(),
            Error::Unlock
        );
        assert_ne!(acct2.kdf.salt, acct.kdf.salt);
    }

    #[test]
    fn old_account_file_is_rejected_after_password_change() {
        let pw = SecretString::from("old password");
        let new = AccountFile::create(&pw, 0).unwrap();
        let old = new.account.clone();
        assert_eq!(old.epoch, 0);
        let vk = old.unlock(&pw, &new.secret_key).unwrap();
        let changed = old
            .change_password(&vk, &SecretString::from("new password"), &new.secret_key)
            .unwrap();
        assert_eq!(changed.epoch, 1);
        // 新しい世代を知る端末は、旧パスワードの箱を書き戻されても受け付けない
        assert_eq!(
            old.unlock_at_least(&pw, &new.secret_key, 1).unwrap_err(),
            Error::Rollback { got: 0, known: 1 }
        );
        assert!(
            changed
                .unlock_at_least(&SecretString::from("new password"), &new.secret_key, 1)
                .is_ok()
        );
    }

    #[test]
    fn unauthenticated_epoch_field_cannot_be_raised() {
        let pw = SecretString::from("pw");
        let new = AccountFile::create(&pw, 0).unwrap();
        let mut forged = new.account.clone();
        forged.epoch = 99; // 平文の欄だけ書き換えても、認証済みヘッダと一致しない
        assert_eq!(
            forged.unlock(&pw, &new.secret_key).unwrap_err(),
            Error::Unlock
        );
    }

    #[test]
    fn legacy_account_without_epoch_still_opens() {
        let pw = SecretString::from("pw");
        let new = AccountFile::create(&pw, 0).unwrap();
        let mut json: serde_json::Value = serde_json::from_slice(&new.account.to_json()).unwrap();
        json.as_object_mut().unwrap().remove("epoch");
        let legacy = AccountFile::from_json(&serde_json::to_vec(&json).unwrap()).unwrap();
        assert_eq!(legacy.epoch, 0);
        assert!(legacy.unlock(&pw, &new.secret_key).is_ok());
    }

    #[test]
    fn malformed_vault_id_is_rejected() {
        let new = AccountFile::create(&SecretString::from("pw"), 0).unwrap();
        for bad in [
            "../../evil",
            "abc",
            "x' or name contains '",
            &"A".repeat(32),
        ] {
            let mut a = new.account.clone();
            a.vault_id = bad.into();
            assert_eq!(
                AccountFile::from_json(&a.to_json()).unwrap_err(),
                Error::Format("vault_id")
            );
        }
        assert!(is_valid_vault_id(&new.account.vault_id));
    }

    #[test]
    fn tampered_kdf_params_are_rejected() {
        let pw = SecretString::from("pw");
        let new = AccountFile::create(&pw, 0).unwrap();
        let mut acct = new.account.clone();
        acct.kdf.m_kib = 8; // 攻撃者が弱くしようとする
        assert!(AccountFile::from_json(&acct.to_json()).is_err());
        let mut acct = new.account.clone();
        acct.kdf.t = 4; // 範囲内でも、包んだときの値と違えば開かない
        assert_eq!(
            acct.unlock(&pw, &new.secret_key).unwrap_err(),
            Error::Unlock
        );
    }
}
