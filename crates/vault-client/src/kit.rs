//! Emergency Kit（印刷用 HTML）
use std::path::Path;
use vault_core::SecretKey;

use crate::R;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vault_core::AccountId;

    #[test]
    fn kit_can_be_created_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kit.html");
        let sk = SecretKey::from_bytes(AccountId::parse("AB0123").unwrap(), [7; 16]);
        write(&path, &sk, Path::new("encrypted-store"), "today").unwrap();
        write(&path, &sk, Path::new("new-store"), "tomorrow").unwrap();
        let html = std::fs::read_to_string(path).unwrap();
        assert!(html.contains(sk.to_display().as_str()));
        assert!(html.contains("new-store"));
    }

    #[cfg(unix)]
    #[test]
    fn kit_is_private_even_when_replacing_a_public_export() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kit.html");
        let sk = SecretKey::from_bytes(AccountId::parse("AB0123").unwrap(), [7; 16]);
        write(&path, &sk, Path::new("store"), "today").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write(&path, &sk, Path::new("store"), "today").unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
            0
        );
    }
}

pub fn write(path: &Path, sk: &SecretKey, store: &Path, created: &str) -> R<()> {
    let html = format!(
        r#"<!doctype html><html lang="ja"><head><meta charset="utf-8"><title>2password Emergency Kit</title>
<style>body{{font-family:"Yu Gothic UI","Hiragino Sans",sans-serif;max-width:720px;margin:40px auto;color:#0f172a}}
h1{{font-size:28px}}.box{{border:2px solid #0f172a;border-radius:12px;padding:20px 24px;margin:18px 0}}
.lbl{{font-size:13px;color:#475569}}.v{{font:700 22px Consolas,monospace;letter-spacing:.03em;margin:4px 0 14px}}
.blank{{height:40px;border-bottom:2px dashed #94a3b8}}.warn{{background:#fef3c7;padding:12px 16px;border-radius:8px;font-size:14px;line-height:1.7}}</style></head>
<body><h1>2password Emergency Kit</h1><p>作成日: {created}</p>
<div class="box"><div class="lbl">アカウントID</div><div class="v">{aid}</div>
<div class="lbl">Secret Key</div><div class="v">{sk}</div>
<div class="lbl">保管場所（暗号文フォルダ）</div><div class="v" style="font-size:15px">{store}</div>
<div class="lbl">アカウントパスワード（ここには書かず、別の場所に保管してください）</div><div class="blank"></div></div>
<div class="warn">・この紙を印刷して、施錠できる場所に保管してください。<br>
・新しい端末では <b>2pw join</b> を実行し、この Secret Key とアカウントパスワードを入力します。<br>
・Secret Key とアカウントパスワードの両方を失うと、誰にも復旧できません。<br>
・<b>印刷したら、このファイルは削除してください。</b></div></body></html>"#,
        created = esc(created),
        aid = esc(sk.aid().as_str()),
        sk = esc(&sk.to_display()),
        store = esc(&store.display().to_string()),
    );
    // A fresh private file also repairs permissions of an existing public Kit.
    // Atomic replacement does not follow an existing destination symlink.
    crate::store::write_atomic(path, html.as_bytes())?;
    Ok(())
}
