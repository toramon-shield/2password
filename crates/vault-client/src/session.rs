//! 解除済みセッションと、アカウント作成・参加などの操作。
use secrecy::{ExposeSecret, SecretString};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vault_core::{AccountFile, Item, SecretKey, Vault, VaultKey, generator};
use zeroize::Zeroizing;

use crate::R;
use crate::config::{self, Backend, Config, PendingUpload};
use crate::store::{self, ACCOUNT_FILE, FolderStore, Store, vault_file};
use crate::{drive, kit};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn next_item_time(previous: u64) -> R<u64> {
    Ok(now_ms().max(previous.checked_add(1).ok_or("item timestamp exhausted")?))
}

/// UTC の簡易表示（依存を増やさないため）
pub fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    let (days, rem) = (s / 86400, s % 86400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

// ---------------- パスワードの強さ ----------------

/// 0〜4（zxcvbn）。3 未満は弱いとみなす。
pub fn password_score(pw: &str) -> u8 {
    zxcvbn::zxcvbn(pw, &["2password"]).score().into()
}

pub fn check_new_password(p1: &SecretString, p2: &SecretString, allow_weak: bool) -> R<u8> {
    if p1.expose_secret() != p2.expose_secret() {
        return Err("2回の入力が一致しません".into());
    }
    let score = password_score(p1.expose_secret());
    if score < 3 && !allow_weak {
        return Err(format!(
            "パスワードが弱すぎます（強度 {score}/4）。単語を4〜5個つなげた長いパスフレーズにしてください。"
        )
        .into());
    }
    Ok(score)
}

// ---------------- 保管場所と控え ----------------

/// 保管場所の選び方
#[derive(Debug, Clone)]
pub enum StoreChoice {
    /// 同期フォルダ（None なら Google ドライブ for デスクトップを自動検出）
    Folder(Option<PathBuf>),
    /// Google Drive API
    Drive,
}

pub fn choose_store(choice: StoreChoice) -> R<(Backend, PathBuf, Box<dyn Store>)> {
    match choice {
        StoreChoice::Drive => Ok((
            Backend::Drive,
            PathBuf::from(drive::folder_name()),
            Box::new(drive::DriveStore::new()?),
        )),
        StoreChoice::Folder(p) => {
            let dir = match p {
                Some(p) => p,
                None => config::detect_drive_folder()
                    .map(|d| d.join("2password"))
                    .ok_or("Google ドライブ for デスクトップのフォルダが見つかりません。保存先のフォルダを指定してください")?,
            };
            Ok((
                Backend::Folder,
                dir.clone(),
                Box::new(FolderStore::new(dir)),
            ))
        }
    }
}

pub fn read_account(store: &dyn Store, notices: &mut Vec<String>) -> R<AccountFile> {
    let bytes = match store.read(ACCOUNT_FILE)? {
        Some(b) => b,
        None => {
            let cached = config::cache_dir().join(ACCOUNT_FILE);
            notices.push(format!(
                "保管場所に {ACCOUNT_FILE} がありません。この端末の控えを使います。"
            ));
            store::read_file_bounded(&cached, store::MAX_ACCOUNT_BYTES)
                .map_err(|_| format!("{} が見つかりません", store.describe(ACCOUNT_FILE)))?
        }
    };
    Ok(AccountFile::from_json(&bytes)?)
}

fn read_cached_account(cache_dir: &Path) -> Option<AccountFile> {
    store::read_file_bounded(&cache_dir.join(ACCOUNT_FILE), store::MAX_ACCOUNT_BYTES)
        .ok()
        .and_then(|b| AccountFile::from_json(&b).ok())
}

/// 保管場所の account.2pw と端末の控えのうち世代の新しい方で解除する。
///
/// - 控えは過去に解除できた（認証済みの）箱なので、保管場所の方が古ければ控えを使って書き戻す。
/// - 既知の世代より古い箱（パスワード変更前の箱の書き戻し）は拒否する。
/// - 平文の `epoch` 欄を偽って大きくしても、認証済みヘッダと一致しないので解除できない（拒否されるだけ）。
///
/// 戻り値の bool は「保管場所へ account.2pw を書き戻すべきか」。
fn unlock_account(
    remote: Option<AccountFile>,
    cached: Option<AccountFile>,
    pw: &SecretString,
    sk: &SecretKey,
    cfg: &Config,
    notices: &mut Vec<String>,
) -> R<(AccountFile, VaultKey, bool)> {
    // 保管場所が「自分の書き込みが失敗した時点の版」のままなら、差し戻しではなく未送信
    let not_delivered = |remote: Option<u64>| {
        cfg.pending_account_upload
            .is_some_and(|p| p.remote == remote)
    };
    let (account, writeback) = match (remote, cached) {
        (Some(r), Some(c)) if c.epoch > r.epoch => {
            if not_delivered(Some(r.epoch)) {
                notices.push(format!(
                    "前回のパスワード変更が保管場所に届いていませんでした（通信エラーなど）。この端末の {ACCOUNT_FILE} を書き戻します。"
                ));
            } else {
                notices.push(format!(
                    "保管場所の {ACCOUNT_FILE} が古い世代（{}）に差し戻されています（この端末の控えは世代 {}）。改ざんの可能性があります。この端末の控えを使い、書き戻します。",
                    r.epoch, c.epoch
                ));
            }
            (c, true)
        }
        (Some(r), _) => (r, false),
        (None, Some(c)) => {
            if not_delivered(None) {
                notices.push(format!(
                    "前回の保存が保管場所に届いていませんでした（通信エラーなど）。この端末の {ACCOUNT_FILE} を書き戻します。"
                ));
            } else {
                notices.push(format!(
                    "保管場所に {ACCOUNT_FILE} がありません。この端末の控えを使い、書き戻します。"
                ));
            }
            (c, true)
        }
        (None, None) => return Err(format!("{ACCOUNT_FILE} が見つかりません").into()),
    };
    if account.aid != cfg.aid || account.vault_id != cfg.vault_id {
        return Err("保管場所のアカウントがこの端末の設定と一致しません".into());
    }
    let vk = match account.unlock_at_least(pw, sk, cfg.known_account_epoch) {
        Ok(vk) => vk,
        Err(vault_core::Error::Rollback { got, known }) => {
            return Err(format!(
                "{ACCOUNT_FILE} がパスワード変更前の古い世代（{got}）に差し戻されています（この端末は世代 {known} まで確認済み）。改ざんの可能性があるため拒否しました。"
            )
            .into());
        }
        Err(e) => return Err(e.into()),
    };
    Ok((account, vk, writeback))
}

fn write_cache(name: &str, bytes: &[u8]) -> R<()> {
    store::check_size(name, bytes)?;
    std::fs::create_dir_all(config::cache_dir())?;
    store::write_atomic(&config::cache_dir().join(name), bytes)
}

fn open_cache(cache_dir: &Path, vk: &VaultKey, aid: &str, vault_id: &str) -> R<Option<Vault>> {
    match std::fs::File::open(cache_dir.join(vault_file(vault_id))) {
        Ok(file) => {
            let bytes = store::read_bounded(file, store::MAX_VAULT_BYTES)?;
            Ok(Some(Vault::open(vk, &bytes, aid, vault_id, 0)?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Cache first: a crash before committing config may leave a newer cache, never
/// a durable counter whose recovery snapshot has not been written yet.
fn persist_checkpoint(
    cfg: &mut Config,
    vault: &Vault,
    vk: &VaultKey,
    cache_dir: &Path,
    commit: impl FnOnce(&Config) -> R<()>,
) -> R<()> {
    checkpoint_snapshot(cfg, vault, vk, cache_dir, false, commit).map(|_| ())
}

fn checkpoint_snapshot(
    cfg: &mut Config,
    vault: &Vault,
    vk: &VaultKey,
    cache_dir: &Path,
    advance_counter: bool,
    commit: impl FnOnce(&Config) -> R<()>,
) -> R<(Vault, Vec<u8>)> {
    std::fs::create_dir_all(cache_dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(cache_dir.join(".checkpoint.lock"))?;
    lock.lock()?;
    // Another process or unlock command can commit after our original load.
    // Reconcile again under one lock spanning both durable replacements.
    let mut snapshot = vault.clone();
    if let Some(cached) = open_cache(cache_dir, vk, &cfg.aid, &cfg.vault_id)? {
        snapshot.merge(&cached);
    }
    if snapshot.counter < cfg.known_counter {
        return Err("cannot checkpoint an older vault".into());
    }
    if advance_counter {
        snapshot.counter = snapshot
            .counter
            .checked_add(1)
            .ok_or("vault counter exhausted")?;
    }
    let name = vault_file(&cfg.vault_id);
    let bytes = snapshot.seal(vk)?;
    store::check_size(&name, &bytes)?;
    std::fs::create_dir_all(cache_dir)?;
    store::write_atomic(&cache_dir.join(name), &bytes)?;
    let mut next = cfg.clone();
    next.known_counter = snapshot.counter;
    commit(&next)?;
    *cfg = next;
    Ok((snapshot, bytes))
}

/// `load_vault` の結果
struct Loaded {
    vault: Vault,
    /// 保管場所へすぐ書き戻すべきか（削除・差し戻し・競合コピーを検知した場合）
    writeback: bool,
    /// 保管場所にあった保管庫の保存番号（None = ファイルがなかった）
    remote_counter: Option<u64>,
    /// マージできた競合コピー。本体の書き込みが成功した後で片付ける
    merged_copies: Vec<store::CopyRef>,
}

/// 保管場所の保管庫（＋競合コピー）を読み、復号してマージする。
/// `pending` は前回の保存が届かなかった記録（あれば、差し戻しと区別して案内する）。
#[allow(clippy::too_many_arguments)]
fn load_vault(
    store: &dyn Store,
    vk: &VaultKey,
    aid: &str,
    vault_id: &str,
    known: u64,
    pending: Option<&PendingUpload>,
    cache_dir: &Path,
    notices: &mut Vec<String>,
) -> R<Loaded> {
    const NOT_DELIVERED: &str = "前回の保存が保管場所に届いていませんでした（通信エラーなど）。この端末の控えから書き戻します。";
    let name = vault_file(vault_id);
    let mut writeback = false;
    // Even a successful remote read must be reconciled with trusted local
    // history: concurrent devices can legitimately produce the same counter.
    let cached = open_cache(cache_dir, vk, aid, vault_id)?;
    let (mut vault, remote_counter) = match store.read(&name)? {
        Some(bytes) => match Vault::open(vk, &bytes, aid, vault_id, known) {
            Ok(v) => {
                let c = v.counter;
                (v, Some(c))
            }
            Err(vault_core::Error::Rollback { got, known }) => {
                // 保管場所が「自分の保存が失敗した時点の版」のままなら、差し戻しではない
                if pending.is_some_and(|p| p.remote == Some(got)) {
                    notices.push(NOT_DELIVERED.into());
                } else {
                    notices.push(format!(
                        "保管場所の保管庫が古い版（番号 {got}）に差し戻されています（この端末は番号 {known} まで確認済み）。改ざんの可能性があります。この端末の控えを基準にし、最新版を書き戻しました。"
                    ));
                }
                let mut base = cached
                    .clone()
                    .filter(|v| v.counter >= known)
                    .ok_or("この端末に控えがないため復元できません")?;
                // 古い版も正規の暗号文なので項目単位でマージする（古い変更が新しい変更を上書きすることはない）
                base.merge(&Vault::open(vk, &bytes, aid, vault_id, 0)?);
                writeback = true;
                (base, Some(got))
            }
            Err(e) => return Err(e.into()),
        },
        None => match cached.clone().filter(|v| v.counter >= known) {
            Some(v) => {
                if pending.is_some_and(|p| p.remote.is_none()) {
                    notices.push(NOT_DELIVERED.into());
                } else {
                    notices.push(
                        "保管場所に保管庫ファイルがありません。この端末の控えから復元し、書き戻しました。".into(),
                    );
                }
                writeback = true;
                (v, None)
            }
            None if known == 0 => (Vault::new_empty(aid, vault_id), None),
            None => return Err("保管庫ファイルが見つからず、この端末にも控えがありません".into()),
        },
    };
    if let Some(cached) = cached {
        if vault.counter == cached.counter && !vault.same_contents(&cached) {
            notices.push(
                "同じ保存番号の異なる保管庫を検知しました。端末の控えとマージして復元します。"
                    .into(),
            );
            writeback = true;
        }
        let before = vault.counter;
        let changed = vault.merge(&cached);
        if changed > 0 || cached.counter > before {
            writeback = true;
        }
    }
    let mut merged_copies = vec![];
    for copy in store.conflict_copies(vault_id)? {
        // 競合コピーは古い番号でもよい（項目単位で新しい方だけが採用される）
        let label = &copy.id.label;
        match Vault::open(vk, &copy.bytes, aid, vault_id, 0) {
            Ok(other) => {
                let n = vault.merge(&other);
                notices.push(format!(
                    "同期の競合コピー {label} をマージしました（{n} 件）。"
                ));
                // 本体に書き込んでから片付けるため、必ず書き戻す
                writeback = true;
                merged_copies.push(copy.id);
            }
            Err(e) => notices.push(format!(
                "{label} を開けませんでした: {e}（このファイルは片付けずに残します）"
            )),
        }
    }
    Ok(Loaded {
        vault,
        writeback,
        remote_counter,
        merged_copies,
    })
}

// ---------------- 項目の入力 ----------------

#[derive(Debug, Clone, Default)]
pub struct ItemInput {
    pub title: String,
    pub urls: Vec<String>,
    pub username: String,
    /// None または空なら自動生成
    pub password: Option<Zeroizing<String>>,
    pub generate_length: usize,
    pub notes: String,
}

/// 表示用の ID 先頭 8 文字（短い ID や非 ASCII でもパニックしない）
pub fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn new_id() -> R<String> {
    Ok(uuid::Uuid::from_bytes(vault_core::rng::array::<16>()?)
        .simple()
        .to_string())
}

// ---------------- 解除済みセッション ----------------

pub struct Session {
    pub cfg: Config,
    store: Box<dyn Store>,
    pub account: AccountFile,
    sk: SecretKey,
    vk: VaultKey,
    pub vault: Vault,
    /// 利用者に伝えるべき注意（差し戻し検知など）
    pub notices: Vec<String>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("aid", &self.cfg.aid)
            .finish_non_exhaustive()
    }
}

pub struct SaveInfo {
    pub location: String,
    pub counter: u64,
    pub bytes: usize,
}

impl Session {
    pub fn unlock(pw: &SecretString) -> R<Self> {
        let mut cfg = Config::load()?;
        let store = cfg.open_store()?;
        let mut notices = vec![];
        let remote = match store.read(ACCOUNT_FILE)? {
            Some(b) => Some(AccountFile::from_json(&b)?),
            None => None,
        };
        let remote_epoch = remote.as_ref().map(|a| a.epoch);
        let cached = read_cached_account(&config::cache_dir());
        let sk = config::keychain_load(&cfg.aid)?;
        let (account, vk, account_writeback) =
            unlock_account(remote, cached, pw, &sk, &cfg, &mut notices)?;
        // 認証できた箱だけを控えにし、世代を進める（config は下の checkpoint/save で保存される）
        write_cache(ACCOUNT_FILE, &account.to_json())?;
        cfg.known_account_epoch = cfg.known_account_epoch.max(account.epoch);
        if account_writeback {
            match store.write(ACCOUNT_FILE, &account.to_json()) {
                Ok(()) => cfg.pending_account_upload = None,
                Err(e) => {
                    // 解除は続ける。次回もう一度書き戻す（そのときは差し戻しと区別して案内する）
                    cfg.pending_account_upload = Some(PendingUpload {
                        remote: remote_epoch,
                    });
                    notices.push(format!(
                        "{ACCOUNT_FILE} を保管場所に書き戻せませんでした（{e}）。次回の解除時にもう一度書き戻します。"
                    ));
                }
            }
        } else if remote_epoch == Some(account.epoch) {
            // 保管場所が最新の世代になっている（他の手段で届いた）なら、未送信の記録は不要
            cfg.pending_account_upload = None;
        }
        let loaded = load_vault(
            store.as_ref(),
            &vk,
            &cfg.aid,
            &cfg.vault_id,
            cfg.known_counter,
            cfg.pending_vault_upload.as_ref(),
            &config::cache_dir(),
            &mut notices,
        )?;
        let mut s = Self {
            cfg,
            store,
            account,
            sk,
            vk,
            vault: loaded.vault,
            notices,
        };
        if loaded.writeback {
            s.save()?;
        } else {
            s.checkpoint()?;
        }
        Ok(s)
    }

    /// 通知を重複なしで追加する（解除・同期の直後に save が同じ状態を読み直すため）
    fn push_notices(&mut self, notices: Vec<String>) {
        for n in notices {
            if !self.notices.contains(&n) {
                self.notices.push(n);
            }
        }
    }

    /// 保存直前に最新版を読み直してマージし、番号を +1 して書き込む。
    pub fn save(&mut self) -> R<SaveInfo> {
        let name = vault_file(&self.cfg.vault_id);
        let mut notices = vec![];
        let loaded = load_vault(
            self.store.as_ref(),
            &self.vk,
            &self.cfg.aid,
            &self.cfg.vault_id,
            self.cfg.known_counter,
            self.cfg.pending_vault_upload.as_ref(),
            &config::cache_dir(),
            &mut notices,
        )?;
        self.push_notices(notices);
        let n = self.vault.merge(&loaded.vault);
        if n > 0 {
            self.notices
                .push(format!("他の端末の変更を取り込みました（{n} 件）"));
        }
        // 端末の控えと既知の番号を先に確定させる（保管場所への書き込みが失敗しても変更は失われない）
        let (vault, bytes) = checkpoint_snapshot(
            &mut self.cfg,
            &self.vault,
            &self.vk,
            &config::cache_dir(),
            true,
            Config::save,
        )?;
        self.vault = vault;
        if let Err(e) = self.store.write(&name, &bytes) {
            // 次回、保管場所がこの版のままなら「届いていない」と案内し、改ざん警告を出さない
            let mut next = self.cfg.clone();
            next.pending_vault_upload = Some(PendingUpload {
                remote: loaded.remote_counter,
            });
            if next.save().is_ok() {
                self.cfg = next;
            }
            return Err(format!(
                "保管場所への保存に失敗しました（{e}）。変更はこの端末に暗号化して保存済みで、次の保存・同期・解除のときに書き戻します。"
            )
            .into());
        }
        if self.cfg.pending_vault_upload.is_some() {
            let mut next = self.cfg.clone();
            next.pending_vault_upload = None;
            next.save()?;
            self.cfg = next;
        }
        // 競合コピーの中身は、いま書き込んだ本体に入っている。読んだ後で変わっていなければ片付ける
        for copy in &loaded.merged_copies {
            let label = &copy.label;
            match self.store.retire_conflict_copy(copy) {
                Ok(true) => self.notices.push(format!(
                    "競合コピー {label} は本体に統合したため片付けました（元に戻せる形で残しています）。"
                )),
                Ok(false) => self.notices.push(format!(
                    "競合コピー {label} は読み込んだ後に変更されていたため、残しました。次の同期で再度マージします。"
                )),
                Err(e) => self
                    .notices
                    .push(format!("競合コピー {label} を片付けられませんでした: {e}")),
            }
        }
        Ok(SaveInfo {
            location: self.store.describe(&name),
            counter: self.vault.counter,
            bytes: bytes.len(),
        })
    }

    fn checkpoint(&mut self) -> R<()> {
        self.vault = checkpoint_snapshot(
            &mut self.cfg,
            &self.vault,
            &self.vk,
            &config::cache_dir(),
            false,
            Config::save,
        )?
        .0;
        Ok(())
    }

    /// 保管場所の最新版を読み直す（他の端末の変更を取り込む）。
    pub fn reload(&mut self) -> R<usize> {
        let mut notices = vec![];
        let loaded = load_vault(
            self.store.as_ref(),
            &self.vk,
            &self.cfg.aid,
            &self.cfg.vault_id,
            self.cfg.known_counter,
            self.cfg.pending_vault_upload.as_ref(),
            &config::cache_dir(),
            &mut notices,
        )?;
        let n = self.vault.merge(&loaded.vault);
        self.push_notices(notices);
        if loaded.writeback {
            self.save()?;
        } else {
            self.checkpoint()?;
        }
        Ok(n)
    }

    pub fn location(&self) -> String {
        self.store.describe("")
    }

    pub fn get(&self, id: &str) -> Option<&Item> {
        self.vault.items().find(|i| i.id == id)
    }

    /// 検索語で1件に絞る（複数一致ならエラーに候補を含める）
    pub fn pick(&self, query: &str) -> R<Item> {
        let found = self.vault.find(query);
        match found.len() {
            0 => Err(format!("「{query}」に一致する項目はありません").into()),
            1 => Ok(found[0].clone()),
            _ => {
                let list: Vec<String> = found
                    .iter()
                    .map(|i| format!("  {}  {}", short_id(&i.id), i.title))
                    .collect();
                Err(format!(
                    "複数の項目が一致しました。もう少し詳しく指定するか、ID の先頭8文字を指定してください\n{}",
                    list.join("\n")
                )
                .into())
            }
        }
    }

    fn resolve_password(input: &ItemInput) -> R<Zeroizing<String>> {
        match &input.password {
            Some(p) if !p.is_empty() => Ok(p.clone()),
            _ => Ok(generator::generate(input.generate_length.max(12))?),
        }
    }

    pub fn add(&mut self, input: ItemInput) -> R<String> {
        let password = Self::resolve_password(&input)?;
        let id = new_id()?;
        self.vault.upsert(Item {
            id: id.clone(),
            kind: "login".into(),
            title: input.title,
            urls: input.urls,
            username: input.username,
            password: password.to_string(),
            notes: input.notes,
            updated_at: now_ms(),
            deleted: false,
            passkey: None,
            totp: String::new(),
        });
        self.save()?;
        Ok(id)
    }

    /// 取り込んだ項目をまとめて追加し、保存は 1 回だけ行う。
    /// タイトル・ユーザー名・パスワード・URL がすべて同じ項目は、重複として飛ばす。
    /// 返り値は（追加した件数、飛ばした件数）。
    pub fn import(&mut self, items: Vec<crate::import::Imported>) -> R<(usize, usize)> {
        let (mut added, mut skipped) = (0, 0);
        for it in items {
            let duplicate = self.vault.items().any(|i| {
                i.title == it.title
                    && i.username == it.username
                    && i.password == *it.password
                    && i.urls == it.urls
            });
            if duplicate {
                skipped += 1;
                continue;
            }
            self.vault.upsert(Item {
                id: new_id()?,
                kind: "login".into(),
                title: it.title,
                urls: it.urls,
                username: it.username,
                password: it.password.to_string(),
                notes: it.notes,
                updated_at: now_ms(),
                deleted: false,
                passkey: None,
                totp: it.totp.to_string(),
            });
            added += 1;
        }
        if added > 0 {
            self.save()?;
        }
        Ok((added, skipped))
    }

    /// 既存の項目を書き換える（password が None ならそのまま）
    pub fn update(&mut self, id: &str, input: ItemInput) -> R<()> {
        let old = self.get(id).ok_or("項目が見つかりません")?.clone();
        let password = match &input.password {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => old.password.clone(),
        };
        self.vault.upsert(Item {
            id: id.into(),
            kind: old.kind.clone(),
            title: input.title,
            urls: input.urls,
            username: input.username,
            password,
            notes: input.notes,
            updated_at: next_item_time(old.updated_at)?,
            deleted: false,
            passkey: old.passkey.clone(),
            totp: old.totp.clone(),
        });
        self.save()?;
        Ok(())
    }

    /// パスキーだけの項目を新しく作る（パスワードは空）。
    pub fn add_passkey_item(
        &mut self,
        title: String,
        url: String,
        username: String,
        passkey: vault_core::Passkey,
    ) -> R<String> {
        let id = new_id()?;
        self.vault.upsert(Item {
            id: id.clone(),
            kind: "login".into(),
            title,
            urls: vec![url],
            username,
            password: String::new(),
            notes: String::new(),
            updated_at: now_ms(),
            deleted: false,
            passkey: Some(passkey),
            totp: String::new(),
        });
        self.save()?;
        Ok(id)
    }

    /// 項目にワンタイムパスワード（2 段階認証）の設定を付ける。None か空なら外す。
    /// `otpauth://` の URL か、設定キー（英数字の並び）を受け取る。
    pub fn set_totp(&mut self, id: &str, input: Option<&str>) -> R<()> {
        let mut item = self.get(id).ok_or("項目が見つかりません")?.clone();
        item.totp = match input.map(str::trim).filter(|v| !v.is_empty()) {
            Some(v) => crate::totp::normalize(v)?.to_string(),
            None => String::new(),
        };
        item.updated_at = next_item_time(item.updated_at)?;
        self.vault.upsert(item);
        self.save()?;
        Ok(())
    }

    /// 既存の項目にパスキーを付ける（None なら外す）。
    pub fn set_passkey(&mut self, id: &str, passkey: Option<vault_core::Passkey>) -> R<()> {
        let mut item = self.get(id).ok_or("項目が見つかりません")?.clone();
        item.passkey = passkey;
        item.updated_at = next_item_time(item.updated_at)?;
        self.vault.upsert(item);
        self.save()?;
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> R<()> {
        let previous = self.get(id).ok_or("項目が見つかりません")?.updated_at;
        if !self.vault.delete(id, next_item_time(previous)?) {
            return Err("項目が見つかりません".into());
        }
        self.save()?;
        Ok(())
    }

    /// パスワード変更：Vault Key を包み直すだけ（保管庫本体はそのまま）。
    /// 解除したまま席を離れた隙に変更されないよう、現在のパスワードを確かめてから行う。
    pub fn change_password(&mut self, current: &SecretString, new_pw: &SecretString) -> R<()> {
        self.account
            .unlock_at_least(current, &self.sk, self.cfg.known_account_epoch)
            .map_err(|_| "現在のアカウントパスワードが違います")?;
        // 他の端末が先に変更していたら上書きしない（その変更を差し戻すことになる）
        let remote_epoch = match self.store.read(ACCOUNT_FILE)? {
            Some(b) => Some(AccountFile::from_json(&b)?.epoch),
            None => None,
        };
        if remote_epoch.is_some_and(|e| e > self.account.epoch) {
            return Err("他の端末でアカウントパスワードが変更されています。いったんロックし、新しいパスワードで解除してからやり直してください".into());
        }
        let acct = self.account.change_password(&self.vk, new_pw, &self.sk)?;
        let json = acct.to_json();
        // 控え → 既知の世代 → 保管場所 の順に書く。保管場所への書き込みが失敗しても、
        // 次回の解除で控え（新しい世代）が使われて書き戻される。
        write_cache(ACCOUNT_FILE, &json)?;
        let mut next = self.cfg.clone();
        next.known_account_epoch = next.known_account_epoch.max(acct.epoch);
        next.save()?;
        self.cfg = next;
        self.account = acct;
        if let Err(e) = self.store.write(ACCOUNT_FILE, &json) {
            // 次回の解除で控えから書き戻す。そのとき差し戻し（改ざん）と区別して案内する
            let mut next = self.cfg.clone();
            next.pending_account_upload = Some(PendingUpload {
                remote: remote_epoch,
            });
            if next.save().is_ok() {
                self.cfg = next;
            }
            return Err(format!(
                "パスワードは変更しましたが、保管場所への保存に失敗しました（{e}）。この端末では新しいパスワードを使ってください。次回の解除時に自動で書き戻します。"
            )
            .into());
        }
        if self.cfg.pending_account_upload.is_some() {
            let mut next = self.cfg.clone();
            next.pending_account_upload = None;
            next.save()?;
            self.cfg = next;
        }
        Ok(())
    }

    /// 同期フォルダ版の保管庫を、暗号文のまま Drive API 版に移す（鍵・パスワードは変わらない）。
    pub fn migrate_to_drive(&mut self) -> R<SaveInfo> {
        if self.cfg.backend == Backend::Drive {
            return Err("すでに Google Drive API 版です".into());
        }
        let dst = drive::DriveStore::new()?;
        if let Some(b) = dst.read(ACCOUNT_FILE)? {
            let other = AccountFile::from_json(&b)?;
            if other.aid != self.cfg.aid {
                return Err(format!(
                    "{} には別のアカウント（{}）があります。移行を中止しました",
                    dst.describe(ACCOUNT_FILE),
                    other.aid
                )
                .into());
            }
        }
        dst.write(ACCOUNT_FILE, &self.account.to_json())?;
        self.store = Box::new(dst);
        self.cfg.backend = Backend::Drive;
        self.cfg.store = PathBuf::from(drive::folder_name());
        self.save()
    }
}

// ---------------- アカウントの作成・参加・削除 ----------------

pub struct InitResult {
    pub secret_key: Zeroizing<String>,
    pub location: String,
    pub kdf_secs: f64,
}

pub fn init_account(choice: StoreChoice, pw: &SecretString, kit_path: &Path) -> R<InitResult> {
    init_account_inner(choice, pw, kit_path, None)
}

/// `init_account` と同じ。Emergency Kit を、指定した言語の文言で書き出す
pub fn init_account_with_kit(
    choice: StoreChoice,
    pw: &SecretString,
    kit_path: &Path,
    kit_text: &kit::KitText,
) -> R<InitResult> {
    init_account_inner(choice, pw, kit_path, Some(kit_text))
}

fn init_account_inner(
    choice: StoreChoice,
    pw: &SecretString,
    kit_path: &Path,
    kit_text: Option<&kit::KitText>,
) -> R<InitResult> {
    if Config::exists() {
        return Err("この端末は設定済みです（やり直す場合は「この端末から消す」）".into());
    }
    let (backend, dir, store) = choose_store(choice)?;
    if store.read(ACCOUNT_FILE)?.is_some() {
        return Err(format!(
            "{} には既にアカウントがあります。2台目なら「既存の保管庫に参加」を使ってください",
            store.describe(ACCOUNT_FILE)
        )
        .into());
    }
    let t = std::time::Instant::now();
    let new = AccountFile::create(pw, now_ms())?;
    let kdf_secs = t.elapsed().as_secs_f64();

    let a = &new.account;
    let mut vault = Vault::new_empty(&a.aid, &a.vault_id);
    vault.bump();
    let vbytes = vault.seal(&new.vault_key)?;
    store.write(ACCOUNT_FILE, &a.to_json())?;
    store.write(&vault_file(&a.vault_id), &vbytes)?;
    write_cache(ACCOUNT_FILE, &a.to_json())?;
    write_cache(&vault_file(&a.vault_id), &vbytes)?;

    config::keychain_store(&new.secret_key)?;
    Config {
        backend,
        store: dir,
        aid: a.aid.clone(),
        vault_id: a.vault_id.clone(),
        known_counter: vault.counter,
        known_account_epoch: a.epoch,
        pending_vault_upload: None,
        pending_account_upload: None,
    }
    .save()?;
    let location = store.describe("");
    let created = fmt_time(now_ms());
    match kit_text {
        Some(text) => kit::write_with(kit_path, &new.secret_key, Path::new(&location), &created, text)?,
        None => kit::write(kit_path, &new.secret_key, Path::new(&location), &created)?,
    }
    Ok(InitResult {
        secret_key: new.secret_key.to_display(),
        location,
        kdf_secs,
    })
}

pub struct JoinResult {
    pub items: usize,
    pub location: String,
    pub notices: Vec<String>,
}

pub fn join_account(
    choice: StoreChoice,
    sk_text: &SecretString,
    pw: &SecretString,
) -> R<JoinResult> {
    if Config::exists() {
        return Err("この端末は設定済みです（やり直す場合は「この端末から消す」）".into());
    }
    let (backend, dir, store) = choose_store(choice)?;
    let mut notices = vec![];
    let account = read_account(store.as_ref(), &mut notices)?;
    let sk = SecretKey::parse(sk_text.expose_secret())?;
    let vk = account.unlock(pw, &sk)?;
    let vault = load_vault(
        store.as_ref(),
        &vk,
        &account.aid,
        &account.vault_id,
        0,
        None,
        &config::cache_dir(),
        &mut notices,
    )?
    .vault;
    config::keychain_store(&sk)?;
    let mut cfg = Config {
        backend,
        store: dir,
        aid: account.aid.clone(),
        vault_id: account.vault_id.clone(),
        known_counter: 0,
        known_account_epoch: account.epoch,
        pending_vault_upload: None,
        pending_account_upload: None,
    };
    write_cache(ACCOUNT_FILE, &account.to_json())?;
    persist_checkpoint(&mut cfg, &vault, &vk, &config::cache_dir(), Config::save)?;
    Ok(JoinResult {
        items: vault.items().count(),
        location: store.describe(""),
        notices,
    })
}

/// この端末から設定とキーチェーンの Secret Key を消す（保管場所のデータは残る）。
pub fn forget() -> R<()> {
    if let Ok(cfg) = Config::load() {
        config::keychain_delete(&cfg.aid)?;
    }
    Config::remove()
}

// ---------------- 状態表示 ----------------

pub struct StatusInfo {
    pub home: String,
    pub backend: Backend,
    pub location: String,
    pub aid: String,
    pub known_counter: u64,
    /// (ファイル名, サイズ)
    pub files: Vec<(String, Option<u64>)>,
    pub header_counter: Option<u64>,
    pub sample_hex: Option<String>,
}

pub fn status() -> R<StatusInfo> {
    let cfg = Config::load()?;
    let store = cfg.open_store()?;
    let mut files = vec![];
    for name in [ACCOUNT_FILE.to_string(), vault_file(&cfg.vault_id)] {
        files.push((name.clone(), store.size(&name)?));
    }
    let (mut header_counter, mut sample_hex) = (None, None);
    if let Some(b) = store.read(&vault_file(&cfg.vault_id))? {
        header_counter = Some(vault_core::envelope::peek_header(&b)?.counter);
        sample_hex = Some(
            b.iter()
                .rev()
                .take(48)
                .map(|x| format!("{x:02x}"))
                .collect(),
        );
    }
    Ok(StatusInfo {
        home: config::home().display().to_string(),
        backend: cfg.backend,
        location: store.describe(""),
        aid: cfg.aid,
        known_counter: cfg.known_counter,
        files,
        header_counter,
        sample_hex,
    })
}

// ---------------- クリップボード ----------------

fn set_clipboard(cb: &mut arboard::Clipboard, secret: &str) -> R<()> {
    #[cfg(windows)]
    {
        use arboard::SetExtWindows;
        // Windows のクリップボード履歴（Win+V）とクラウド同期に残さない
        cb.set()
            .exclude_from_history()
            .exclude_from_cloud()
            .text(secret.to_string())?;
    }
    #[cfg(not(windows))]
    cb.set_text(secret.to_string())?;
    Ok(())
}

fn clear_if_unchanged(cb: &mut arboard::Clipboard, secret: &str) -> bool {
    let still_ours = cb.get_text().map(|t| t == secret).unwrap_or(false);
    still_ours && cb.clear().is_ok()
}

/// コピーして、secs 秒後に（自分のコピーのままなら）消す。この関数は待ってから戻る。
pub fn copy_and_clear_blocking(secret: &str, secs: u64) -> R<bool> {
    let mut cb = arboard::Clipboard::new()?;
    set_clipboard(&mut cb, secret)?;
    std::thread::sleep(Duration::from_secs(secs));
    Ok(clear_if_unchanged(&mut cb, secret))
}

/// コピーして、別スレッドで secs 秒後に消す（GUI 用）。
pub fn copy_and_clear_later(secret: Zeroizing<String>, secs: u64) -> R<()> {
    let mut cb = arboard::Clipboard::new()?;
    set_clipboard(&mut cb, &secret)?;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(secs));
        if let Ok(mut cb) = arboard::Clipboard::new() {
            clear_if_unchanged(&mut cb, &secret);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Fixture {
        cache: tempfile::TempDir,
        remote: tempfile::TempDir,
        account: vault_core::NewAccount,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                cache: tempfile::tempdir().unwrap(),
                remote: tempfile::tempdir().unwrap(),
                account: AccountFile::create(&SecretString::from("test-only password"), 0).unwrap(),
            }
        }
        fn vault(&self, counter: u64) -> Vault {
            let a = &self.account.account;
            let mut vault = Vault::new_empty(&a.aid, &a.vault_id);
            vault.counter = counter;
            vault
        }
        fn cfg(&self, known_counter: u64) -> Config {
            Config {
                backend: Backend::Folder,
                store: self.remote.path().to_path_buf(),
                aid: self.account.account.aid.clone(),
                vault_id: self.account.account.vault_id.clone(),
                known_counter,
                known_account_epoch: 0,
                pending_vault_upload: None,
                pending_account_upload: None,
            }
        }
        fn write(&self, dir: &Path, vault: &Vault) {
            store::write_atomic(
                &dir.join(vault_file(&vault.vault_id)),
                &vault.seal(&self.account.vault_key).unwrap(),
            )
            .unwrap();
        }
        fn load(&self, known: u64) -> (Vault, bool, Vec<String>) {
            let (loaded, notices) = self.load_with(known, None);
            (loaded.vault, loaded.writeback, notices)
        }
        fn load_with(&self, known: u64, pending: Option<PendingUpload>) -> (Loaded, Vec<String>) {
            let mut notices = vec![];
            let loaded = load_vault(
                &FolderStore::new(self.remote.path()),
                &self.account.vault_key,
                &self.account.account.aid,
                &self.account.account.vault_id,
                known,
                pending.as_ref(),
                self.cache.path(),
                &mut notices,
            )
            .unwrap();
            (loaded, notices)
        }
    }

    fn item(id: &str, time: u64) -> Item {
        Item {
            id: id.into(),
            kind: "login".into(),
            title: id.into(),
            urls: vec![],
            username: "user".into(),
            password: "test password".into(),
            notes: String::new(),
            updated_at: time,
            deleted: false,
            passkey: None,
            totp: String::new(),
        }
    }

    #[test]
    fn equal_counter_replay_cannot_resurrect_cached_tombstones_or_omit_items() {
        let f = Fixture::new();
        let mut old = f.vault(10);
        old.upsert(item("deleted", 10));
        let mut local = old.clone();
        local.delete("deleted", 20);
        local.upsert(item("local-only", 20));
        local.bump();
        let mut fork = old;
        fork.upsert(item("remote-only", 21));
        fork.bump();
        f.write(f.cache.path(), &local);
        f.write(f.remote.path(), &fork);
        let (merged, writeback, notices) = f.load(11);
        assert!(writeback);
        assert!(notices.iter().any(|n| n.contains("同じ保存番号")));
        assert!(merged.items().all(|i| i.id != "deleted"));
        assert!(merged.items().any(|i| i.id == "local-only"));
        assert!(merged.items().any(|i| i.id == "remote-only"));
        let mut reverse = local;
        reverse.merge(&fork);
        assert!(merged.same_contents(&reverse));
    }

    #[test]
    fn higher_counter_fork_still_preserves_observed_local_state() {
        let f = Fixture::new();
        let mut local = f.vault(5);
        local.upsert(item("saved", 10));
        let mut remote = f.vault(6);
        remote.upsert(item("another", 11));
        f.write(f.cache.path(), &local);
        f.write(f.remote.path(), &remote);
        let (merged, writeback, _) = f.load(5);
        assert!(writeback);
        assert_eq!(merged.counter, 6);
        assert_eq!(merged.items().count(), 2);
    }

    #[test]
    fn readonly_sync_checkpoint_survives_remote_deletion_and_rollback() {
        let f = Fixture::new();
        f.write(f.cache.path(), &f.vault(1));
        let mut latest = f.vault(5);
        latest.upsert(item("read-on-another-device", 10));
        f.write(f.remote.path(), &latest);
        let (loaded, _, _) = f.load(1);
        let mut cfg = f.cfg(1);
        persist_checkpoint(
            &mut cfg,
            &loaded,
            &f.account.vault_key,
            f.cache.path(),
            |next| {
                let cached = open_cache(
                    f.cache.path(),
                    &f.account.vault_key,
                    &next.aid,
                    &next.vault_id,
                )?
                .unwrap();
                assert_eq!(cached.counter, next.known_counter);
                assert!(cached.same_contents(&loaded));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(cfg.known_counter, 5);
        std::fs::remove_file(f.remote.path().join(vault_file(&cfg.vault_id))).unwrap();
        let (recovered, writeback, _) = f.load(5);
        assert!(writeback);
        assert!(recovered.same_contents(&latest));
        f.write(f.remote.path(), &f.vault(4));
        let (recovered, writeback, _) = f.load(5);
        assert!(writeback);
        assert!(recovered.same_contents(&latest));
    }

    #[test]
    fn enrollment_checkpoint_does_not_require_a_preexisting_cache() {
        let f = Fixture::new();
        let mut cfg = f.cfg(0);
        let vault = f.vault(5);
        persist_checkpoint(
            &mut cfg,
            &vault,
            &f.account.vault_key,
            f.cache.path(),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(cfg.known_counter, 5);
        let (recovered, writeback, _) = f.load(5);
        assert!(writeback);
        assert_eq!(recovered.counter, 5);
    }

    #[test]
    fn checkpoint_failures_cannot_commit_a_counter_without_its_snapshot() {
        let f = Fixture::new();
        let blocked = f.cache.path().join("not-a-directory");
        std::fs::write(&blocked, b"file").unwrap();
        let committed = Cell::new(false);
        let mut cfg = f.cfg(1);
        let latest = f.vault(5);
        assert!(
            persist_checkpoint(&mut cfg, &latest, &f.account.vault_key, &blocked, |_| {
                committed.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(!committed.get());
        assert_eq!(cfg.known_counter, 1);
        assert!(
            persist_checkpoint(
                &mut cfg,
                &latest,
                &f.account.vault_key,
                f.cache.path(),
                |_| { Err("simulated config write failure".into()) }
            )
            .is_err()
        );
        assert_eq!(cfg.known_counter, 1);
        f.write(f.remote.path(), &f.vault(4));
        let (recovered, writeback, _) = f.load(1);
        assert!(writeback);
        assert_eq!(recovered.counter, 5);
    }

    #[test]
    fn rolled_back_account_file_is_replaced_by_newer_cache() {
        let f = Fixture::new();
        let old_pw = SecretString::from("test-only password");
        let new_pw = SecretString::from("test-only new password");
        let sk = &f.account.secret_key;
        let old = f.account.account.clone();
        let vk = old.unlock(&old_pw, sk).unwrap();
        let newer = old.change_password(&vk, &new_pw, sk).unwrap();
        let mut cfg = f.cfg(0);
        cfg.known_account_epoch = 1;

        // 保管場所に旧パスワードの箱、控えに新しい箱：控えが使われ、書き戻しが指示される
        let mut notices = vec![];
        let (acct, _, writeback) = unlock_account(
            Some(old.clone()),
            Some(newer.clone()),
            &new_pw,
            sk,
            &cfg,
            &mut notices,
        )
        .unwrap();
        assert_eq!(acct.epoch, 1);
        assert!(writeback);
        assert!(notices.iter().any(|n| n.contains("差し戻され")));
        // 旧パスワードでは開かない
        assert!(
            unlock_account(
                Some(old.clone()),
                Some(newer),
                &old_pw,
                sk,
                &cfg,
                &mut vec![]
            )
            .is_err()
        );
        // 控えがなくても、既知の世代より古い箱は拒否する
        let err = unlock_account(Some(old), None, &old_pw, sk, &cfg, &mut vec![])
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("差し戻され"), "{err}");
    }

    #[test]
    fn password_change_on_another_device_is_accepted() {
        let f = Fixture::new();
        let old_pw = SecretString::from("test-only password");
        let new_pw = SecretString::from("test-only new password");
        let sk = &f.account.secret_key;
        let old = f.account.account.clone();
        let vk = old.unlock(&old_pw, sk).unwrap();
        let newer = old.change_password(&vk, &new_pw, sk).unwrap();
        // この端末は世代 0 しか知らない。保管場所の新しい世代を新パスワードで開ける
        let (acct, _, writeback) =
            unlock_account(Some(newer), Some(old), &new_pw, sk, &f.cfg(0), &mut vec![]).unwrap();
        assert_eq!(acct.epoch, 1);
        assert!(!writeback);
    }

    #[test]
    fn failed_upload_is_not_reported_as_tampering() {
        let f = Fixture::new();
        let mut sent = f.vault(4);
        sent.upsert(item("sent", 10));
        let mut unsent = sent.clone();
        unsent.upsert(item("unsent", 20));
        unsent.bump();
        f.write(f.remote.path(), &sent); // 保管場所は失敗前の版 4 のまま
        f.write(f.cache.path(), &unsent); // 控えは版 5
        // 書き込み失敗の記録があり、保管場所がその版のまま → 「届いていない」と案内する
        let (loaded, notices) = f.load_with(5, Some(PendingUpload { remote: Some(4) }));
        assert!(loaded.writeback);
        assert!(loaded.vault.items().any(|i| i.id == "unsent"));
        assert!(notices.iter().any(|n| n.contains("届いていません")));
        assert!(!notices.iter().any(|n| n.contains("改ざん")));
        // 記録がなければ従来どおり差し戻しとして警告する
        let (_, notices) = f.load_with(5, None);
        assert!(notices.iter().any(|n| n.contains("改ざん")));
        // 記録と違う版（さらに古い版）に戻されていたら、記録があっても警告する
        f.write(f.remote.path(), &f.vault(3));
        let (_, notices) = f.load_with(5, Some(PendingUpload { remote: Some(4) }));
        assert!(notices.iter().any(|n| n.contains("改ざん")));
        // 初回保存が届かずファイルがない場合
        std::fs::remove_file(f.remote.path().join(vault_file(&unsent.vault_id))).unwrap();
        let (_, notices) = f.load_with(5, Some(PendingUpload { remote: None }));
        assert!(notices.iter().any(|n| n.contains("届いていません")));
    }

    #[test]
    fn failed_password_change_upload_is_not_reported_as_tampering() {
        let f = Fixture::new();
        let old_pw = SecretString::from("test-only password");
        let new_pw = SecretString::from("test-only new password");
        let sk = &f.account.secret_key;
        let old = f.account.account.clone();
        let vk = old.unlock(&old_pw, sk).unwrap();
        let newer = old.change_password(&vk, &new_pw, sk).unwrap();
        let mut cfg = f.cfg(0);
        cfg.known_account_epoch = 1;
        cfg.pending_account_upload = Some(PendingUpload { remote: Some(0) });
        let mut notices = vec![];
        let (_, _, writeback) =
            unlock_account(Some(old), Some(newer), &new_pw, sk, &cfg, &mut notices).unwrap();
        assert!(writeback);
        assert!(notices.iter().any(|n| n.contains("届いていません")));
        assert!(!notices.iter().any(|n| n.contains("改ざん")));
    }

    #[test]
    fn merged_conflict_copies_force_writeback_and_are_reported_for_cleanup() {
        let f = Fixture::new();
        let mut main = f.vault(3);
        main.upsert(item("main", 10));
        let mut copy = f.vault(2);
        copy.upsert(item("from-copy", 11));
        f.write(f.remote.path(), &main);
        f.write(f.cache.path(), &main);
        let copy_path = f
            .remote
            .path()
            .join(format!("vault-{} (1).2pw", main.vault_id));
        store::write_atomic(&copy_path, &copy.seal(&f.account.vault_key).unwrap()).unwrap();
        // 開けないコピーは片付け対象にしない
        let broken = f
            .remote
            .path()
            .join(format!("vault-{} (2).2pw", main.vault_id));
        std::fs::write(&broken, b"not a vault").unwrap();
        let (loaded, _) = f.load_with(3, None);
        assert!(loaded.writeback);
        assert!(loaded.vault.items().any(|i| i.id == "from-copy"));
        assert_eq!(loaded.merged_copies.len(), 1);
        assert_eq!(
            loaded.merged_copies[0].label,
            copy_path.display().to_string()
        );
    }

    #[test]
    fn short_id_never_panics() {
        assert_eq!(short_id("0123456789"), "01234567");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id("あいうえお"), "あいうえお");
    }

    #[test]
    fn local_edits_advance_timestamps_even_when_the_clock_goes_back() {
        let previous = now_ms() + 60_000;
        assert_eq!(next_item_time(previous).unwrap(), previous + 1);
        assert!(next_item_time(u64::MAX).is_err());
    }

    #[test]
    fn late_stale_checkpoint_cannot_erase_another_unlocks_observed_branch() {
        let f = Fixture::new();
        let mut a = f.vault(11);
        a.upsert(item("branch-a", 20));
        let mut b = f.vault(11);
        b.upsert(item("branch-b", 21));
        let mut cfg_b = f.cfg(10);
        persist_checkpoint(&mut cfg_b, &b, &f.account.vault_key, f.cache.path(), |_| {
            Ok(())
        })
        .unwrap();
        let mut stale_cfg_a = f.cfg(10);
        persist_checkpoint(
            &mut stale_cfg_a,
            &a,
            &f.account.vault_key,
            f.cache.path(),
            |_| Ok(()),
        )
        .unwrap();
        f.write(f.remote.path(), &a);
        let (merged, _, _) = f.load(11);
        assert!(merged.items().any(|i| i.id == "branch-a"));
        assert!(merged.items().any(|i| i.id == "branch-b"));
    }

    #[test]
    fn concurrent_checkpoint_writes_preserve_both_history_and_counter() {
        let f = Fixture::new();
        let mut a = f.vault(11);
        a.upsert(item("branch-a", 20));
        let mut b = f.vault(12);
        b.upsert(item("branch-b", 21));
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for vault in [&a, &b] {
                let barrier = &barrier;
                let f = &f;
                scope.spawn(move || {
                    let mut cfg = f.cfg(10);
                    barrier.wait();
                    persist_checkpoint(
                        &mut cfg,
                        vault,
                        &f.account.vault_key,
                        f.cache.path(),
                        |next| {
                            store::write_atomic(
                                &f.cache.path().join("config-test.json"),
                                &serde_json::to_vec(next)?,
                            )
                        },
                    )
                    .unwrap();
                });
            }
        });
        let cfg: Config = serde_json::from_slice(
            &std::fs::read(f.cache.path().join("config-test.json")).unwrap(),
        )
        .unwrap();
        let cached = open_cache(
            f.cache.path(),
            &f.account.vault_key,
            &cfg.aid,
            &cfg.vault_id,
        )
        .unwrap()
        .unwrap();
        assert_eq!(cfg.known_counter, 12);
        assert_eq!(cached.counter, cfg.known_counter);
        assert_eq!(cached.items().count(), 2);
    }
}
