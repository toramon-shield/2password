//! Emergency Kit（印刷用 HTML）
use std::path::Path;
use vault_core::SecretKey;

use crate::R;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Emergency Kit の文言。デスクトップアプリは表示言語の文言を渡す（CLI は日本語）
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct KitText {
    /// 言語コード（`ja`、`en` など）
    pub lang: String,
    /// 文字の向き（`ltr` か `rtl`）
    pub dir: String,
    pub created: String,
    pub account_id: String,
    pub store: String,
    pub password: String,
    /// 注意書き（1 行ずつ）
    pub notes: Vec<String>,
    /// 最後の、太字の注意書き
    pub delete_after: String,
}

impl Default for KitText {
    fn default() -> Self {
        Self {
            lang: "ja".into(),
            dir: "ltr".into(),
            created: "作成日".into(),
            account_id: "アカウントID".into(),
            store: "保管場所（暗号文フォルダ）".into(),
            password: "アカウントパスワード（ここには書かず、別の場所に保管してください）".into(),
            notes: vec![
                "この紙を印刷して、施錠できる場所に保管してください。".into(),
                "新しい端末では <b>2pw join</b> を実行し、この Secret Key とアカウントパスワードを入力します。".into(),
                "Secret Key とアカウントパスワードの両方を失うと、誰にも復旧できません。".into(),
            ],
            delete_after: "印刷したら、このファイルは削除してください。".into(),
        }
    }
}

impl KitText {
    /// 画面から渡された文言を、HTML に入れられる形にする（タグは CLI 既定の文言にだけ使う）
    fn sanitized(&self) -> KitText {
        let short = |s: &str, max: usize| esc(&s.chars().take(max).collect::<String>());
        let lang = if !self.lang.is_empty()
            && self.lang.len() <= 12
            && self.lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            self.lang.clone()
        } else {
            "en".into()
        };
        KitText {
            lang,
            dir: if self.dir == "rtl" { "rtl" } else { "ltr" }.into(),
            created: short(&self.created, 100),
            account_id: short(&self.account_id, 100),
            store: short(&self.store, 200),
            password: short(&self.password, 300),
            notes: self.notes.iter().take(6).map(|n| short(n, 400)).collect(),
            delete_after: short(&self.delete_after, 300),
        }
    }
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
        assert!(html.contains("2pw join") && html.contains("lang=\"ja\""));
    }

    #[test]
    fn kit_uses_the_given_language_and_escapes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kit.html");
        let sk = SecretKey::from_bytes(AccountId::parse("AB0123").unwrap(), [7; 16]);
        let text = KitText {
            lang: "ar".into(),
            dir: "rtl".into(),
            created: "<script>alert(1)</script>".into(),
            account_id: "Account ID".into(),
            notes: vec!["Keep it safe & <b>offline</b>".into()],
            delete_after: "Delete this file".into(),
            ..KitText::default()
        };
        write_with(&path, &sk, Path::new("store"), "today", &text).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains(r#"lang="ar" dir="rtl""#));
        assert!(html.contains("Account ID") && html.contains("Delete this file"));
        assert!(!html.contains("<script>") && html.contains("&lt;script&gt;"));
        assert!(html.contains("&amp; &lt;b&gt;offline"));
        assert!(html.contains(sk.to_display().as_str()));
        // 言語コードと向きは、決まった形でなければ使わない
        let bad = KitText {
            lang: "en\" onload=\"x".into(),
            dir: "up".into(),
            ..KitText::default()
        };
        write_with(&path, &sk, Path::new("store"), "today", &bad).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains(r#"lang="en" dir="ltr""#));
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
    write_html(path, &render(sk, store, created, &KitText::default())?)
}

/// 表示言語の文言で Emergency Kit を書き出す
pub fn write_with(path: &Path, sk: &SecretKey, store: &Path, created: &str, text: &KitText) -> R<()> {
    write_html(path, &render(sk, store, created, &text.sanitized())?)
}

fn render(sk: &SecretKey, store: &Path, created: &str, x: &KitText) -> R<String> {
    let notes: String = x.notes.iter().map(|n| format!("・{n}<br>
")).collect();
    let html = format!(
        r#"<!doctype html><html lang="{lang}" dir="{dir}"><head><meta charset="utf-8"><title>2password Emergency Kit</title>
<style>body{{font-family:system-ui,"Segoe UI",sans-serif;max-width:720px;margin:40px auto;color:#0f172a}}
:lang(ja) body{{font-family:"Yu Gothic UI","Hiragino Sans",sans-serif}}
h1{{font-size:28px}}.box{{border:2px solid #0f172a;border-radius:12px;padding:20px 24px;margin:18px 0}}
.lbl{{font-size:13px;color:#475569}}.v{{font:700 22px Consolas,monospace;letter-spacing:.03em;margin:4px 0 14px}}
.blank{{height:40px;border-bottom:2px dashed #94a3b8}}.warn{{background:#fef3c7;padding:12px 16px;border-radius:8px;font-size:14px;line-height:1.7}}</style></head>
<body><h1>2password Emergency Kit</h1><p>{l_created}: {created}</p>
<div class="box"><div class="lbl">{l_aid}</div><div class="v" dir="ltr">{aid}</div>
<div class="lbl">Secret Key</div><div class="v" dir="ltr">{sk}</div>
<div class="lbl">{l_store}</div><div class="v" dir="ltr" style="font-size:15px">{store}</div>
<div class="lbl">{l_password}</div><div class="blank"></div></div>
<div class="warn">{notes}・<b>{l_delete}</b></div></body></html>"#,
        lang = x.lang,
        dir = x.dir,
        l_created = x.created,
        l_aid = x.account_id,
        l_store = x.store,
        l_password = x.password,
        l_delete = x.delete_after,
        created = esc(created),
        aid = esc(sk.aid().as_str()),
        sk = esc(&sk.to_display()),
        store = esc(&store.display().to_string()),
    );
    Ok(html)
}

fn write_html(path: &Path, html: &str) -> R<()> {
    // A fresh private file also repairs permissions of an existing public Kit.
    // Atomic replacement does not follow an existing destination symlink.
    crate::store::write_atomic(path, html.as_bytes())?;
    Ok(())
}
