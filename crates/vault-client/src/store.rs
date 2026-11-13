//! 暗号文の置き場所。
//!
//! - `FolderStore`：Google ドライブ for デスクトップなどの同期フォルダ
//! - `DriveStore`（drive.rs）：Google Drive API（OAuth＋PKCE、スコープ drive.file）
//!
//! どちらも「暗号文のバイト列を名前で読み書きする」だけ。中身は見ない。
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::R;

pub const ACCOUNT_FILE: &str = "account.2pw";
pub const MAX_ACCOUNT_BYTES: usize = 64 * 1024;
pub const MAX_VAULT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CONFLICT_COPIES: usize = 32;
pub const MAX_CONFLICT_BYTES: usize = 64 * 1024 * 1024;

pub fn file_limit(name: &str) -> usize {
    if name == ACCOUNT_FILE {
        MAX_ACCOUNT_BYTES
    } else {
        MAX_VAULT_BYTES
    }
}

pub fn check_size(name: &str, bytes: &[u8]) -> R<()> {
    if bytes.len() > file_limit(name) {
        return Err(format!("{name}: file exceeds the supported size limit").into());
    }
    Ok(())
}

/// Read at most limit + 1 actual bytes, including streams with no trusted length.
pub fn read_bounded(reader: impl Read, limit: usize) -> R<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("file or response exceeds the supported size limit".into());
    }
    Ok(bytes)
}

pub fn read_file_bounded(path: &Path, limit: usize) -> R<Vec<u8>> {
    read_bounded(File::open(path)?, limit)
}

pub(crate) fn conflict_limit(count: usize, total: usize) -> R<usize> {
    if count >= MAX_CONFLICT_COPIES || total > MAX_CONFLICT_BYTES {
        return Err("too many conflict copies; resolve stored conflicts before retrying".into());
    }
    Ok(MAX_VAULT_BYTES.min(MAX_CONFLICT_BYTES - total))
}

pub fn vault_file(vault_id: &str) -> String {
    format!("vault-{vault_id}.2pw")
}

/// 競合コピーを指す情報。片付けるときに「読んだ後で変わっていない」ことを確かめるのに使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyRef {
    /// 表示用の名前
    pub label: String,
    /// FolderStore: パス／DriveStore: ファイル ID
    pub(crate) key: String,
    /// FolderStore: サイズと更新時刻／DriveStore: version
    pub(crate) stamp: String,
}

/// 同期の競合で生まれた別コピー
#[derive(Debug)]
pub struct ConflictCopy {
    pub id: CopyRef,
    pub bytes: Vec<u8>,
}

pub trait Store: Send {
    fn read(&self, name: &str) -> R<Option<Vec<u8>>>;
    fn write(&self, name: &str, bytes: &[u8]) -> R<()>;
    /// 同期の競合で生まれた別コピー
    fn conflict_copies(&self, vault_id: &str) -> R<Vec<ConflictCopy>>;
    /// マージして本体に書き込み済みの競合コピーを、元に戻せる形で片付ける。
    /// 読んだ後で変わっていたら何もせず false を返す。
    fn retire_conflict_copy(&self, copy: &CopyRef) -> R<bool>;
    /// 表示用の場所
    fn describe(&self, name: &str) -> String;
    fn size(&self, name: &str) -> R<Option<u64>>;
}

#[derive(Debug, Clone)]
pub struct FolderStore {
    pub dir: PathBuf,
}

impl FolderStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Store for FolderStore {
    fn read(&self, name: &str) -> R<Option<Vec<u8>>> {
        match File::open(self.path(name)) {
            Ok(file) => Ok(Some(read_bounded(file, file_limit(name))?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> R<()> {
        check_size(name, bytes)?;
        std::fs::create_dir_all(&self.dir)?;
        write_atomic(&self.path(name), bytes)
    }

    /// 同期ツールが作る競合コピー（例: `vault-xxxx (1).2pw`）を探す。
    fn conflict_copies(&self, vault_id: &str) -> R<Vec<ConflictCopy>> {
        let main = vault_file(vault_id);
        let prefix = format!("vault-{vault_id}");
        let mut out = vec![];
        let mut total = 0;
        for e in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n != main && n.starts_with(&prefix) && n.ends_with(".2pw") {
                let limit = conflict_limit(out.len(), total)?;
                let path = e.path();
                let file = File::open(&path)?;
                // 読む前の状態を記録する（読んでいる間に変わっても、片付け時の照合で外れるだけ）
                let stamp = file_stamp(&file.metadata()?);
                let bytes = read_bounded(file, limit)?;
                total += bytes.len();
                let key = path.display().to_string();
                out.push(ConflictCopy {
                    id: CopyRef {
                        label: key.clone(),
                        key,
                        stamp,
                    },
                    bytes,
                });
            }
        }
        Ok(out)
    }

    /// 消さずに `<名前>.merged` へ改名する（拡張子が .2pw でなくなるので、次からは読まれない）。
    fn retire_conflict_copy(&self, copy: &CopyRef) -> R<bool> {
        let path = PathBuf::from(&copy.key);
        // 自分が列挙したこのフォルダの競合コピー以外には触れない
        if path.parent() != Some(self.dir.as_path()) {
            return Err("競合コピーの場所が保管場所のフォルダではありません".into());
        }
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() && file_stamp(&m) == copy.stamp => {}
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        let name = path
            .file_name()
            .ok_or("競合コピーのファイル名がありません")?
            .to_string_lossy()
            .to_string();
        for n in 0..100 {
            let suffix = if n == 0 {
                ".merged".to_string()
            } else {
                format!(".merged-{n}")
            };
            let dest = path.with_file_name(format!("{name}{suffix}"));
            if dest.exists() {
                continue;
            }
            std::fs::rename(&path, &dest)?;
            return Ok(true);
        }
        Err("競合コピーの改名先が見つかりません".into())
    }

    fn describe(&self, name: &str) -> String {
        self.path(name).display().to_string()
    }

    fn size(&self, name: &str) -> R<Option<u64>> {
        Ok(std::fs::metadata(self.path(name)).ok().map(|m| m.len()))
    }
}

fn file_stamp(m: &std::fs::Metadata) -> String {
    let modified = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{}:{modified}", m.len())
}

/// 一時ファイルに書いてから置き換える（書き込み途中のファイルが同期されるのを防ぐ）。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> R<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // tempfile creates a unique file exclusively (0600 on Unix). Never open a
    // predictable sibling path or the destination itself for writing.
    let mut tmp = tempfile::Builder::new()
        .prefix(".2pw-")
        .tempfile_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    // Windows では、同じファイルを別プロセス（同期クライアント・ウイルス対策・並行する保存）が
    // 一瞬開いていると置き換えが「アクセス拒否」になる。間隔を伸ばしながら数秒だけ再試行する。
    let mut wait = std::time::Duration::from_millis(10);
    let mut attempts = 0;
    let file = loop {
        match tmp.persist(path) {
            Ok(file) => break file,
            Err(error)
                if attempts < 12 && error.error.kind() == io::ErrorKind::PermissionDenied =>
            {
                tmp = error.file;
                attempts += 1;
                std::thread::sleep(wait);
                wait = (wait * 2).min(std::time::Duration::from_millis(500));
            }
            Err(error) => return Err(error.error.into()),
        }
    };
    file.sync_all()?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_rejects_oversized_account() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(ACCOUNT_FILE), vec![0; 64 * 1024 + 1]).unwrap();
        assert!(FolderStore::new(dir.path()).read(ACCOUNT_FILE).is_err());
    }

    #[test]
    fn atomic_write_ignores_preplanted_temporary_hardlink() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("unrelated.txt");
        let output = dir.path().join(ACCOUNT_FILE);
        std::fs::write(&victim, b"keep this file").unwrap();
        std::fs::hard_link(&victim, output.with_extension("tmp-2pw")).unwrap();
        write_atomic(&output, b"new account data").unwrap();
        assert_eq!(std::fs::read(victim).unwrap(), b"keep this file");
        assert_eq!(std::fs::read(output).unwrap(), b"new account data");
    }

    #[test]
    fn bounded_reader_checks_bytes_without_length_metadata() {
        use std::io::Cursor;
        assert_eq!(read_bounded(Cursor::new(b"1234"), 4).unwrap(), b"1234");
        assert!(read_bounded(Cursor::new(b"12345"), 4).is_err());
        assert!(read_bounded(io::repeat(0), 4).is_err());
        assert!(read_bounded(Cursor::new(b"x"), 0).is_err());
    }

    #[test]
    fn folder_read_write_limits_agree() {
        let dir = tempfile::tempdir().unwrap();
        let store = FolderStore::new(dir.path());
        let bytes = vec![b'x'; MAX_ACCOUNT_BYTES];
        store.write(ACCOUNT_FILE, &bytes).unwrap();
        assert_eq!(store.read(ACCOUNT_FILE).unwrap().unwrap(), bytes);
        assert!(
            store
                .write(ACCOUNT_FILE, &vec![0; MAX_ACCOUNT_BYTES + 1])
                .is_err()
        );
        assert_eq!(
            store.read(ACCOUNT_FILE).unwrap().unwrap().len(),
            MAX_ACCOUNT_BYTES
        );
    }

    #[test]
    fn conflict_count_and_aggregate_bytes_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..=MAX_CONFLICT_COPIES {
            std::fs::write(dir.path().join(format!("vault-test ({i}).2pw")), b"copy").unwrap();
        }
        assert!(
            FolderStore::new(dir.path())
                .conflict_copies("test")
                .is_err()
        );
        assert_eq!(conflict_limit(1, MAX_CONFLICT_BYTES - 2).unwrap(), 2);
        assert!(
            read_bounded(
                io::repeat(0),
                conflict_limit(1, MAX_CONFLICT_BYTES).unwrap()
            )
            .is_err()
        );
        assert!(conflict_limit(MAX_CONFLICT_COPIES, 0).is_err());
    }

    #[test]
    fn merged_conflict_copies_are_renamed_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let store = FolderStore::new(dir.path());
        let copy = dir.path().join("vault-test (1).2pw");
        std::fs::write(&copy, b"copy").unwrap();
        let found = store.conflict_copies("test").unwrap();
        assert_eq!(found.len(), 1);
        assert!(store.retire_conflict_copy(&found[0].id).unwrap());
        assert!(!copy.exists());
        assert_eq!(
            std::fs::read(dir.path().join("vault-test (1).2pw.merged")).unwrap(),
            b"copy"
        );
        // 片付けたコピーは次から読まれず、件数の上限にも数えられない
        assert!(store.conflict_copies("test").unwrap().is_empty());
        // 同じ名前の競合コピーがまたできても、既存の .merged を上書きしない
        std::fs::write(&copy, b"again").unwrap();
        let found = store.conflict_copies("test").unwrap();
        assert!(store.retire_conflict_copy(&found[0].id).unwrap());
        assert!(dir.path().join("vault-test (1).2pw.merged-1").exists());
    }

    #[test]
    fn conflict_copy_changed_after_reading_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = FolderStore::new(dir.path());
        let copy = dir.path().join("vault-test (1).2pw");
        std::fs::write(&copy, b"copy").unwrap();
        let found = store.conflict_copies("test").unwrap();
        std::fs::write(&copy, b"changed by another device").unwrap();
        assert!(!store.retire_conflict_copy(&found[0].id).unwrap());
        assert!(copy.exists());
        // 自分が列挙していない場所のファイルには触れない
        let outside = CopyRef {
            label: "x".into(),
            key: std::env::temp_dir().join("x.2pw").display().to_string(),
            stamp: String::new(),
        };
        assert!(store.retire_conflict_copy(&outside).is_err());
    }

    #[test]
    fn concurrent_atomic_writers_leave_one_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ACCOUNT_FILE);
        let writers: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || write_atomic(&path, &vec![i; 4096]).unwrap())
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(bytes.len(), 4096);
        assert!(bytes.iter().all(|b| *b == bytes[0]));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_replaces_symlinks_without_touching_their_target() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("unrelated");
        let output = dir.path().join(ACCOUNT_FILE);
        std::fs::write(&victim, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&victim, &output).unwrap();
        std::os::unix::fs::symlink(&victim, output.with_extension("tmp-2pw")).unwrap();
        write_atomic(&output, b"account").unwrap();
        assert_eq!(std::fs::read(victim).unwrap(), b"unchanged");
        assert!(
            !std::fs::symlink_metadata(output)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
