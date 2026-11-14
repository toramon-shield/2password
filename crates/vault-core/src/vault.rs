//! 保管庫の中身（平文モデル）とマージ。
use crate::envelope::{self, Expect, Header, Kind};
use crate::{Result, VaultKey};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// 平文の詰め物単位。項目数がファイルサイズから推測されにくくなる。
pub const PAD_BLOCK: u32 = 4096;

/// パスキー（WebAuthn の資格情報）。秘密鍵は保管庫の暗号文の中にだけある。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Passkey {
    /// 資格情報 ID（base64url）
    pub credential_id: String,
    /// このパスキーを使えるサイトの範囲（例：`example.com`）
    pub rp_id: String,
    /// サイトが決めた利用者の識別子（base64url）
    pub user_handle: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub user_display_name: String,
    /// P-256 の秘密鍵（PKCS#8、base64url）
    pub private_key: String,
    pub created_at: u64,
}

/// Debug 出力に秘密鍵を出さない。
impl core::fmt::Debug for Passkey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Passkey")
            .field("rp_id", &self.rp_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Item {
    pub id: String,
    /// "login" | "note"
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub notes: String,
    pub updated_at: u64,
    /// 削除は墓石で表す（同期で復活しないように）
    #[serde(default)]
    pub deleted: bool,
    /// このサイトのパスキー（ない項目では、ファイルにも書かない）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passkey: Option<Passkey>,
    /// ワンタイムパスワード（2 段階認証）の設定。`otpauth://totp/...` の形。ない項目では空
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub totp: String,
}

/// Debug 出力に秘密（ユーザー名・パスワード・メモ）を出さない。
impl core::fmt::Debug for Item {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Item")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("updated_at", &self.updated_at)
            .field("deleted", &self.deleted)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct VaultData {
    items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct Vault {
    pub aid: String,
    pub vault_id: String,
    pub counter: u64,
    data: VaultData,
}

impl Vault {
    pub fn new_empty(aid: &str, vault_id: &str) -> Self {
        Self {
            aid: aid.into(),
            vault_id: vault_id.into(),
            counter: 0,
            data: VaultData::default(),
        }
    }

    pub fn open(
        vk: &VaultKey,
        bytes: &[u8],
        aid: &str,
        vault_id: &str,
        min_counter: u64,
    ) -> Result<Self> {
        let (header, pt) = envelope::open(
            vk.expose(),
            bytes,
            &Expect {
                kind: Kind::Vault,
                aid,
                vault_id,
                min_counter,
            },
        )?;
        let data: VaultData =
            serde_json::from_slice(&pt).map_err(|_| crate::Error::Format("vault json"))?;
        Ok(Self {
            aid: aid.into(),
            vault_id: vault_id.into(),
            counter: header.counter,
            data,
        })
    }

    /// 現在の counter で暗号化する（呼び出し側が保存前に `bump` する）。
    pub fn seal(&self, vk: &VaultKey) -> Result<Vec<u8>> {
        let pt = Zeroizing::new(
            serde_json::to_vec(&self.data).map_err(|_| crate::Error::Internal("vault json"))?,
        );
        let header = Header {
            v: 1,
            kind: Kind::Vault,
            aid: self.aid.clone(),
            vault_id: self.vault_id.clone(),
            counter: self.counter,
            pad: PAD_BLOCK,
            kdf: None,
        };
        envelope::seal(vk.expose(), &header, &pt)
    }

    pub fn bump(&mut self) {
        self.counter += 1;
    }

    /// Compare authenticated item state, ignoring serialization order and counter.
    pub fn same_contents(&self, other: &Vault) -> bool {
        self.aid == other.aid
            && self.vault_id == other.vault_id
            && self.data.items.len() == other.data.items.len()
            && self
                .data
                .items
                .iter()
                .all(|item| other.data.items.iter().any(|other| item == other))
    }

    /// 同じ id は updated_at が新しい方を採用。削除（墓石）も「新しい変更」として扱う。
    pub fn merge(&mut self, other: &Vault) -> usize {
        let mut changed = 0;
        for theirs in &other.data.items {
            match self.data.items.iter_mut().find(|i| i.id == theirs.id) {
                Some(mine) if revision_order(theirs) > revision_order(mine) => {
                    *mine = theirs.clone();
                    changed += 1;
                }
                Some(_) => {}
                None => {
                    self.data.items.push(theirs.clone());
                    changed += 1;
                }
            }
        }
        self.counter = self.counter.max(other.counter);
        changed
    }

    pub fn upsert(&mut self, item: Item) {
        match self.data.items.iter_mut().find(|i| i.id == item.id) {
            Some(mine) => *mine = item,
            None => self.data.items.push(item),
        }
    }

    pub fn delete(&mut self, id: &str, now_ms: u64) -> bool {
        if let Some(i) = self
            .data
            .items
            .iter_mut()
            .find(|i| i.id == id && !i.deleted)
        {
            i.zeroize();
            i.id = id.into();
            i.kind = "deleted".into();
            i.deleted = true;
            i.updated_at = now_ms;
            true
        } else {
            false
        }
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.data.items.iter().filter(|i| !i.deleted)
    }

    /// ID（完全一致、または 8 文字以上の先頭一致）か、タイトル・URL・ユーザー名の部分一致（大文字小文字を無視）。
    pub fn find(&self, query: &str) -> Vec<&Item> {
        let q = query.to_lowercase();
        self.items()
            .filter(|i| {
                i.id == query
                    || (q.len() >= 8 && i.id.starts_with(&q))
                    || i.title.to_lowercase().contains(&q)
                    || i.username.to_lowercase().contains(&q)
                    || i.urls.iter().any(|u| u.to_lowercase().contains(&q))
            })
            .collect()
    }
}

// A total order makes concurrent equal-time revisions converge. A tombstone wins
// a timestamp tie; the remaining fields break live/live ties consistently.
#[allow(clippy::type_complexity)]
fn revision_order(
    item: &Item,
) -> (
    u64,
    bool,
    &str,
    &str,
    &[String],
    &str,
    &str,
    &str,
    Option<&str>,
    &str,
) {
    (
        item.updated_at,
        item.deleted,
        &item.kind,
        &item.title,
        &item.urls,
        &item.username,
        &item.password,
        &item.notes,
        item.passkey.as_ref().map(|p| p.credential_id.as_str()),
        &item.totp,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AccountFile;
    use secrecy::SecretString;

    fn item(id: &str, title: &str, t: u64) -> Item {
        Item {
            id: id.into(),
            kind: "login".into(),
            title: title.into(),
            urls: vec![],
            username: "u".into(),
            password: "p".into(),
            notes: String::new(),
            updated_at: t,
            deleted: false,
            passkey: None,
            totp: String::new(),
        }
    }

    #[test]
    fn seal_open_merge_tombstone() {
        let new = AccountFile::create(&SecretString::from("pw"), 0).unwrap();
        let (aid, vid) = (new.account.aid.clone(), new.account.vault_id.clone());
        let vk = &new.vault_key;

        let mut a = Vault::new_empty(&aid, &vid);
        a.upsert(item("1", "銀行", 10));
        a.upsert(item("2", "AWS", 10));
        a.bump();
        let bytes = a.seal(vk).unwrap();

        let mut b = Vault::open(vk, &bytes, &aid, &vid, 1).unwrap();
        assert_eq!(b.items().count(), 2);

        // 端末Aで「銀行」を削除、端末Bで「AWS」を更新、Bで新規追加
        a.delete("1", 20);
        b.upsert(item("2", "AWS (更新)", 21));
        b.upsert(item("3", "Cloudflare", 22));

        a.merge(&b);
        let titles: Vec<_> = a.items().map(|i| i.title.clone()).collect();
        assert_eq!(titles, vec!["AWS (更新)", "Cloudflare"]); // 墓石が勝ち、更新が反映される

        // 逆向きにマージしても同じ結果（収束する）
        b.merge(&a);
        let mut t2: Vec<_> = b.items().map(|i| i.title.clone()).collect();
        t2.sort();
        assert_eq!(t2, vec!["AWS (更新)", "Cloudflare"]);
    }

    #[test]
    fn opening_with_higher_known_counter_is_rollback() {
        let new = AccountFile::create(&SecretString::from("pw"), 0).unwrap();
        let mut v = Vault::new_empty(&new.account.aid, &new.account.vault_id);
        v.bump();
        let old = v.seal(&new.vault_key).unwrap();
        assert!(matches!(
            Vault::open(
                &new.vault_key,
                &old,
                &new.account.aid,
                &new.account.vault_id,
                5
            ),
            Err(crate::Error::Rollback { got: 1, known: 5 })
        ));
    }

    #[test]
    fn equal_timestamp_tombstone_wins_in_both_directions() {
        let mut live = Vault::new_empty("AB0123", "vault");
        live.upsert(item("1", "bank", 10));
        let mut deleted = live.clone();
        deleted.delete("1", 10);
        let mut a = live.clone();
        a.merge(&deleted);
        let mut b = deleted;
        b.merge(&live);
        assert_eq!(a.items().count(), 0);
        assert_eq!(b.items().count(), 0);
    }

    #[test]
    fn find_accepts_id_prefix_of_eight_or_more() {
        let mut v = Vault::new_empty("AB0123", "vault");
        v.upsert(item("0123456789abcdef", "bank", 1));
        v.upsert(item("fedcba9876543210", "mail", 1));
        assert_eq!(v.find("0123456789AB").len(), 1);
        assert_eq!(v.find("01234567")[0].title, "bank");
        assert!(v.find("0123").is_empty());
    }

    #[test]
    fn equal_timestamp_live_branches_converge() {
        let mut a = Vault::new_empty("AB0123", "vault");
        let mut b = a.clone();
        a.upsert(item("1", "alpha", 10));
        b.upsert(item("1", "beta", 10));
        let mut left = a.clone();
        left.merge(&b);
        b.merge(&a);
        assert_eq!(
            left.items().map(|i| i.title.clone()).collect::<Vec<_>>(),
            b.items().map(|i| i.title.clone()).collect::<Vec<_>>()
        );
    }
}
