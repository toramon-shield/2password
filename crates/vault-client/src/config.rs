//! 端末ごとのローカル設定（秘密は含まない）と OS キーチェーン。
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use vault_core::SecretKey;
use zeroize::Zeroizing;

use crate::R;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    /// 同期フォルダ（Google ドライブ for デスクトップなど）
    #[default]
    Folder,
    /// Google Drive API（OAuth＋PKCE、drive.file）
    Drive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub backend: Backend,
    /// Folder: 暗号文を置くフォルダ（例: G:\マイドライブ\2password）／ Drive: Drive 上のフォルダ名
    pub store: PathBuf,
    pub aid: String,
    pub vault_id: String,
    /// この端末が見たことのある最大の保存番号（差し戻し攻撃の検知に使う）
    pub known_counter: u64,
    /// この端末が見たことのある account.2pw の最大世代（パスワード変更前の箱の書き戻しを拒否する）
    #[serde(default)]
    pub known_account_epoch: u64,
    /// 保管庫の保存が保管場所に届かなかった（通信エラーなど）。次回の書き戻しで消える。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_vault_upload: Option<PendingUpload>,
    /// account.2pw（パスワード変更）の保存が保管場所に届かなかった。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_account_upload: Option<PendingUpload>,
}

/// 書き込みに失敗したとき、保管場所にあった版。次回、保管場所が「ちょうどこの版のまま」なら
/// 差し戻し（改ざん）ではなく、自分の保存が届いていないだけだと判断できる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUpload {
    /// 保管庫なら保存番号、account.2pw なら世代。None はファイルがなかったことを表す。
    pub remote: Option<u64>,
}

impl Config {
    pub fn open_store(&self) -> R<Box<dyn crate::store::Store>> {
        Ok(match self.backend {
            Backend::Folder => Box::new(crate::store::FolderStore::new(&self.store)),
            Backend::Drive => Box::new(crate::drive::DriveStore::new()?),
        })
    }
}

/// 端末ローカルの設定フォルダ。`TWOPW_HOME` で上書きできる（同じPCで「2台目」を試すときに使う）。
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("TWOPW_HOME") {
        return PathBuf::from(h);
    }
    default_home()
}

/// `TWOPW_HOME` を無視した既定の設定フォルダ。
pub fn default_home() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("2password")
}

fn config_path() -> PathBuf {
    home().join("config.json")
}

pub fn cache_dir() -> PathBuf {
    home().join("cache")
}

impl Config {
    pub fn exists() -> bool {
        config_path().exists()
    }

    pub fn load() -> R<Self> {
        let p = config_path();
        let bytes = std::fs::read(&p).map_err(|_| {
            format!("この端末はまだ設定されていません。`2pw init`（新規）か `2pw join`（2台目）を実行してください。（{}）", p.display())
        })?;
        let cfg: Self = serde_json::from_slice(&bytes)?;
        // aid / vault_id はファイル名・キーチェーン名・Drive の検索式に入るので形式を確かめる
        if vault_core::AccountId::parse(&cfg.aid)?.as_str() != cfg.aid
            || !vault_core::is_valid_vault_id(&cfg.vault_id)
        {
            return Err(format!("設定ファイルの形式が正しくありません（{}）", p.display()).into());
        }
        Ok(cfg)
    }

    pub fn save(&self) -> R<()> {
        std::fs::create_dir_all(home())?;
        crate::store::write_atomic(&config_path(), &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn remove() -> R<()> {
        let _ = std::fs::remove_file(config_path());
        let _ = std::fs::remove_dir_all(cache_dir());
        Ok(())
    }
}

/// Google ドライブ for デスクトップの既定の場所を探す。
pub fn detect_drive_folder() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = vec![];
    if cfg!(windows) {
        for d in ['G', 'H', 'I'] {
            cands.push(PathBuf::from(format!("{d}:\\マイドライブ")));
            cands.push(PathBuf::from(format!("{d}:\\My Drive")));
        }
    } else if let Some(h) = std::env::var_os("HOME") {
        let cs = Path::new(&h).join("Library/CloudStorage");
        if let Ok(rd) = std::fs::read_dir(&cs) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("GoogleDrive-") {
                    cands.push(e.path().join("My Drive"));
                    cands.push(e.path().join("マイドライブ"));
                }
            }
        }
    }
    cands.into_iter().find(|p| p.is_dir())
}

// ---------------- OS キーチェーン ----------------

const SERVICE: &str = "2password";

fn keychain_user(aid: &str) -> String {
    // 同じPCで TWOPW_HOME を変えて「2台目」を試せるよう、設定フォルダごとに別エントリにする
    format!("{aid}@{}", home().display())
}

fn entry(aid: &str) -> R<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, &keychain_user(aid))?)
}

pub fn keychain_store(sk: &SecretKey) -> R<()> {
    entry(sk.aid().as_str())?.set_password(&sk.to_display())?;
    Ok(())
}

pub fn keychain_load(aid: &str) -> R<SecretKey> {
    let s = Zeroizing::new(entry(aid)?.get_password().map_err(|e| {
        format!("OS のキーチェーンから Secret Key を読めませんでした（{e}）。`2pw join` で Emergency Kit の Secret Key を入れ直してください。")
    })?);
    Ok(SecretKey::parse(&s)?)
}

pub fn keychain_delete(aid: &str) -> R<()> {
    let _ = entry(aid)?.delete_credential();
    Ok(())
}
