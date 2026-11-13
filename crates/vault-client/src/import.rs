//! 他のパスワード管理ツールが書き出した CSV の取り込み。
//!
//! 1 行目の見出しから列を判別するので、1Password・Chrome（Google パスワード マネージャー）・
//! Edge・Bitwarden などの CSV をそのまま読める。
use crate::R;
use zeroize::Zeroizing;

/// 取り込む 1 件分（まだ保管庫には入っていない）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub title: String,
    pub urls: Vec<String>,
    pub username: String,
    pub password: Zeroizing<String>,
    pub notes: String,
}

/// RFC 4180 形式の CSV を行と列に分ける（引用符の中の改行・カンマ・`""` に対応）
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = vec![];
    let mut row = vec![];
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                '"' => quoted = false,
                _ => cell.push(c),
            }
            continue;
        }
        match c {
            '"' if cell.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut cell)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            _ => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows.retain(|r| r.iter().any(|c| !c.trim().is_empty()));
    rows
}

fn column(header: &[String], names: &[&str]) -> Option<usize> {
    header.iter().position(|h| {
        let h = h.trim().to_lowercase().replace([' ', '-'], "_");
        names.contains(&h.as_str())
    })
}

/// CSV の本文を項目の一覧にする。パスワードもメモもない行は読み飛ばす。
pub fn parse(text: &str) -> R<Vec<Imported>> {
    let rows = parse_csv(text);
    let Some((header, body)) = rows.split_first() else {
        return Err("CSV が空です".into());
    };
    let title = column(header, &["title", "name", "account", "タイトル", "名前"]);
    let url = column(
        header,
        &["url", "urls", "website", "login_uri", "uri", "web_site"],
    );
    let username = column(
        header,
        &["username", "login_username", "user", "login", "email", "ユーザー名"],
    );
    let password = column(header, &["password", "login_password", "パスワード"]);
    let notes = column(header, &["notes", "note", "comment", "comments", "メモ"]);
    if password.is_none() {
        return Err(
            "password の列が見つかりません（1 行目が見出しの CSV を指定してください）".into(),
        );
    }
    let get = |row: &[String], col: Option<usize>| -> String {
        col.and_then(|c| row.get(c))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let mut out = vec![];
    for row in body {
        let password = Zeroizing::new(get(row, password));
        let notes = get(row, notes);
        if password.is_empty() && notes.is_empty() {
            continue;
        }
        // 1Password などは、複数の URL を改行やカンマで 1 つの列に入れる
        let urls: Vec<String> = get(row, url)
            .split(['\n', ',', ';', ' '])
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(String::from)
            .collect();
        let username = get(row, username);
        let mut title = get(row, title);
        if title.is_empty() {
            title = urls
                .first()
                .and_then(|u| crate::links::safe_web_url(u))
                .and_then(|u| reqwest::Url::parse(&u).ok())
                .and_then(|u| u.host_str().map(String::from))
                .unwrap_or_else(|| "（名前なし）".to_string());
        }
        out.push(Imported {
            title,
            urls,
            username,
            password,
            notes,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_1password_export() {
        let csv = "Title,Url,Username,Password,OTPAuth,Favorite,Archived,Tags,Notes\r\n\
                   Example,https://example.com/login,alice,\"p,a\"\"ss\nword\",,false,false,,\"line1\nline2\"\r\n\
                   Empty,https://empty.example,bob,,,false,false,,\r\n";
        let items = parse(csv).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Example");
        assert_eq!(items[0].urls, ["https://example.com/login"]);
        assert_eq!(items[0].username, "alice");
        assert_eq!(items[0].password.as_str(), "p,a\"ss\nword");
        assert_eq!(items[0].notes, "line1\nline2");
    }

    #[test]
    fn reads_chrome_and_bitwarden_exports() {
        let chrome = "\u{feff}name,url,username,password,note\nexample.com,https://example.com/,alice,pw1,\n";
        let items = parse(chrome).unwrap();
        assert_eq!(
            (items[0].title.as_str(), items[0].password.as_str()),
            ("example.com", "pw1")
        );

        let bitwarden = "folder,favorite,type,name,notes,fields,reprompt,login_uri,login_username,login_password,login_totp\n\
                         ,,login,,memo,,0,\"https://a.example,https://b.example\",carol,pw2,\n";
        let items = parse(bitwarden).unwrap();
        assert_eq!(items[0].title, "a.example");
        assert_eq!(items[0].urls, ["https://a.example", "https://b.example"]);
        assert_eq!(items[0].username, "carol");
        assert_eq!(items[0].notes, "memo");
    }

    #[test]
    fn rejects_files_without_a_password_column() {
        assert!(parse("").is_err());
        assert!(parse("a,b,c\n1,2,3\n").is_err());
    }
}
