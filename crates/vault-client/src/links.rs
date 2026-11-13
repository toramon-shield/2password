//! 項目の URL を OS に渡す前の検査。
use reqwest::Url;

/// 項目の URL を検査して正規化する。http/https でホスト名があり、認証情報を含まないものだけ。
/// （UNC パス・file:・独自スキームなどを OS に渡さない。スキームがなければ https とみなす）
pub fn safe_web_url(input: &str) -> Option<String> {
    let s = input.trim();
    // バックスラッシュは Windows のパス（UNC・ドライブ）とみなして拒否する
    if s.is_empty() || s.chars().any(char::is_control) || s.contains('\\') {
        return None;
    }
    let with_scheme = if s.contains("://") {
        s.to_string()
    } else {
        format!("https://{s}")
    };
    let url = Url::parse(&with_scheme).ok()?;
    let ok = matches!(url.scheme(), "https" | "http")
        && url.host_str().is_some_and(|h| !h.is_empty())
        && url.username().is_empty()
        && url.password().is_none();
    ok.then(|| url.to_string())
}

/// ブラウザ拡張から届いたページのオリジン（`https://example.com` など）を検査して正規化する。
pub fn page_origin(input: &str) -> Option<Url> {
    let s = input.trim();
    if s.is_empty() || s.chars().any(char::is_control) || !s.contains("://") {
        return None;
    }
    let url = Url::parse(s).ok()?;
    let ok = matches!(url.scheme(), "https" | "http")
        && url.host_str().is_some_and(|h| !h.is_empty())
        && url.username().is_empty()
        && url.password().is_none();
    ok.then_some(url)
}

fn strip_www(host: &str) -> &str {
    host.strip_prefix("www.").unwrap_or(host)
}

fn is_ip(host: &str) -> bool {
    host.starts_with('[') || host.parse::<std::net::Ipv4Addr>().is_ok()
}

/// 他人がサブドメインを持てる接尾辞（公開サフィックスの一部）。
/// 完全な Public Suffix List ではないので、`*.` の指定は利用者が信頼できるドメインに限ること。
const SHARED_SUFFIXES: &[&str] = &[
    "co.jp",
    "ne.jp",
    "or.jp",
    "ac.jp",
    "go.jp",
    "ed.jp",
    "gr.jp",
    "lg.jp",
    "co.uk",
    "org.uk",
    "ac.uk",
    "gov.uk",
    "com.au",
    "net.au",
    "org.au",
    "co.nz",
    "co.kr",
    "com.cn",
    "com.tw",
    "com.hk",
    "com.sg",
    "com.br",
    "com.mx",
    "co.in",
    "co.za",
    "github.io",
    "gitlab.io",
    "pages.dev",
    "workers.dev",
    "vercel.app",
    "netlify.app",
    "web.app",
    "firebaseapp.com",
    "herokuapp.com",
    "azurewebsites.net",
    "cloudfront.net",
    "amazonaws.com",
    "blogspot.com",
    "wordpress.com",
    "hatenablog.com",
    "hatenablog.jp",
    "sakura.ne.jp",
    "fc2.com",
    "wixsite.com",
    "appspot.com",
    "onrender.com",
    "fly.dev",
    "glitch.me",
    "r2.dev",
    "notion.site",
];

/// 同じ運営者のものとみなせる範囲（登録ドメイン）。`a.b.example.co.jp` → `example.co.jp`
pub fn registrable(host: &str) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if is_ip(&host) || !host.contains('.') {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let suffix_len = (1..=labels.len())
        .rev()
        .find(|&n| SHARED_SUFFIXES.contains(&labels[labels.len() - n..].join(".").as_str()))
        .unwrap_or(1);
    (labels.len() > suffix_len).then(|| labels[labels.len() - suffix_len - 1..].join("."))
}

/// 項目の URL から取り出した、自動入力の宛先
struct Target {
    scheme: String,
    port: Option<u16>,
    host: String,
    /// サブドメインにも使う（URL のホストを `*.example.com` と書いた項目）
    wildcard: bool,
}

fn item_target(item_url: &str) -> Option<Target> {
    let url = safe_web_url(item_url).and_then(|u| Url::parse(&u).ok())?;
    let host = url.host_str()?.to_ascii_lowercase();
    let (host, wildcard) = match host.strip_prefix("*.") {
        // 登録ドメインそのもの（`*.example.com`）か、その下だけ。`*.com` や `*.co.jp` は認めない
        Some(rest) if registrable(rest).is_some() => (rest.to_string(), true),
        Some(_) => return None,
        None => (strip_www(&host).to_string(), false),
    };
    if host.contains('*') {
        return None;
    }
    Some(Target {
        scheme: url.scheme().to_string(),
        port: url.port_or_known_default(),
        host,
        wildcard,
    })
}

/// 項目の URL が、いま開いているページのオリジンに自動入力してよいものか判定する。
///
/// - スキーム（http / https）とポートが同じであること。https で保存した項目は http のページに入力しない
/// - ホスト名が同じであること（先頭の `www.` の有無だけは無視する）
/// - サブドメインにも使うのは、項目の URL を `*.example.com` と書いたときだけ
///
/// 似た名前の偽サイト（`example.com.evil.test` など）にも、同じドメインの別ホスト
/// （`example.com` で保存した項目に対する `blog.example.com`）にも一致しない。
pub fn url_matches_origin(item_url: &str, origin: &Url) -> bool {
    let Some(t) = item_target(item_url) else {
        return false;
    };
    if t.scheme != origin.scheme() || t.port != origin.port_or_known_default() {
        return false;
    }
    let Some(page) = origin.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    let page = strip_www(&page);
    page == t.host || (t.wildcard && !is_ip(page) && page.ends_with(&format!(".{}", t.host)))
}

/// 暗号化されていない（http の）ページか。自分の PC や社内の機器（localhost・プライベート IP など）は除く。
pub fn insecure_origin(origin: &Url) -> bool {
    if origin.scheme() != "http" {
        return false;
    }
    let Some(host) = origin.host_str().map(str::to_ascii_lowercase) else {
        return true;
    };
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return !(ip.is_loopback() || ip.is_private() || ip.is_link_local());
    }
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return match v6.parse::<std::net::Ipv6Addr>() {
            Ok(ip) => {
                let first = ip.segments()[0];
                !(ip.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80)
            }
            Err(_) => true,
        };
    }
    let local = !host.contains('.')
        || [".localhost", ".local", ".test", ".internal", ".lan", ".home.arpa"]
            .iter()
            .any(|s| host.ends_with(s));
    !local
}

/// 保存済みのホストと、いま開いているページのホストの関係
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Relation {
    /// 同じ登録ドメインの別ホスト（`example.com` と `login.example.com`）
    SameSite,
    /// 名前が似ているだけの別サイト（偽サイトの疑い）
    Lookalike,
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let cur = row[j];
            row[j] = (prev + usize::from(a[i - 1] != b[j - 1]))
                .min(row[j] + 1)
                .min(row[j - 1] + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// となり合う 2 文字を入れ替えただけか（exampel と example）
fn swapped(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len() != b.len() {
        return false;
    }
    let diff: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
    diff.len() == 2 && diff[1] == diff[0] + 1 && a[diff[0]] == b[diff[1]] && a[diff[1]] == b[diff[0]]
}

/// 登録ドメインの先頭のラベル（`www.pay-pal.co.jp` → `pay-pal`）
fn brand(host: &str) -> Option<String> {
    registrable(host).and_then(|r| r.split('.').next().map(str::to_string))
}

/// 項目の URL のホスト名（表示用。`www.` と `*.` は除く）
pub fn item_host(item_url: &str) -> Option<String> {
    item_target(item_url).map(|t| t.host)
}

/// 一致しなかったページについて、保存済みの URL との関係を調べる（注意の表示に使う）。
pub fn relation(item_url: &str, origin: &Url) -> Option<Relation> {
    let saved = item_target(item_url)?.host;
    let page = origin.host_str().map(str::to_ascii_lowercase)?;
    let page = strip_www(&page).to_string();
    if page == saved || is_ip(&page) || is_ip(&saved) {
        return None;
    }
    if let (Some(a), Some(b)) = (registrable(&saved), registrable(&page))
        && a == b
    {
        return Some(Relation::SameSite);
    }
    // 保存済みのホスト名をまるごと含む（example.com.evil.test、example.com-login.test）
    if page.starts_with(&format!("{saved}."))
        || page.contains(&format!(".{saved}."))
        || page.starts_with(&format!("{saved}-"))
    {
        return Some(Relation::Lookalike);
    }
    let (sb, pb) = (brand(&saved)?, brand(&page)?);
    // 短い名前は偶然の一致が多いので対象にしない
    if sb.chars().count() < 5 {
        return None;
    }
    // 名前は同じで、末尾（.com と .net など）だけが違う
    if sb == pb {
        return Some(Relation::Lookalike);
    }
    // 1〜2 文字違い（打ち間違いを狙ったもの）
    let limit = if sb.chars().count() >= 8 { 2 } else { 1 };
    if edit_distance(&sb, &pb) <= limit || swapped(&sb, &pb) {
        return Some(Relation::Lookalike);
    }
    // 名前を部品として含む（example-login.com、secure.example.evil.test）
    if page.split(['.', '-']).any(|t| t == sb) {
        return Some(Relation::Lookalike);
    }
    None
}

/// アドレスに国際化文字（Punycode の `xn--`）を含むか。見た目が同じ別の文字を使った偽サイトに使われる。
pub fn has_idn_label(origin: &Url) -> bool {
    origin.host_str().is_some_and(|h| {
        h.to_ascii_lowercase()
            .split('.')
            .any(|l| l.starts_with("xn--"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_web_urls_are_opened() {
        assert_eq!(
            safe_web_url("https://bank.example/login").as_deref(),
            Some("https://bank.example/login")
        );
        assert_eq!(
            safe_web_url("example.com").as_deref(),
            Some("https://example.com/")
        );
        for bad in [
            r"\\attacker\share\2password-emergency-kit.html",
            "file:///C:/Windows/System32/calc.exe",
            "C:\\Users\\x\\2password-emergency-kit.html",
            "javascript:alert(1)",
            "ms-settings://display",
            "https://user:pass@example.com/",
            "https://",
            "https://exa\nmple.com",
            "",
        ] {
            assert_eq!(safe_web_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn autofill_only_matches_the_saved_site() {
        let m = |item: &str, page: &str| url_matches_origin(item, &page_origin(page).unwrap());
        assert!(m("https://example.com/login", "https://example.com"));
        assert!(m("example.com", "https://www.example.com"));
        assert!(m("https://www.example.com", "https://example.com"));
        // 同じドメインの別ホストには、`*.` と書いた項目だけが一致する
        assert!(!m("https://example.com", "https://login.example.com"));
        assert!(m("https://*.example.com", "https://login.example.com"));
        assert!(m("*.example.com", "https://example.com"));
        assert!(m("*.example.co.jp", "https://a.b.example.co.jp"));
        assert!(!m("*.example.com", "https://example.com.evil.test"));
        assert!(!m("*.example.com", "https://evilexample.com"));
        assert!(!m("*.example.com", "http://login.example.com"));
        // 他人がサブドメインを持てる範囲には `*.` を使えない
        assert!(!m("*.com", "https://example.com"));
        assert!(!m("*.co.jp", "https://example.co.jp"));
        assert!(!m("*.github.io", "https://someone.github.io"));
        assert!(!m("https://a.*.example.com", "https://a.b.example.com"));
        assert!(m("http://localhost:3000", "http://localhost:3000"));
        assert!(m("http://192.168.0.1", "http://192.168.0.1"));
        // 偽サイト・別サイト
        assert!(!m("https://example.com", "https://example.com.evil.test"));
        assert!(!m("https://example.com", "https://evilexample.com"));
        assert!(!m("https://login.example.com", "https://example.com"));
        assert!(!m("https://login.example.com", "https://other.example.com"));
        // https で保存した項目は http のページに入力しない
        assert!(!m("https://example.com", "http://example.com"));
        assert!(!m("example.com", "http://example.com"));
        // ポート違い・IP・ドットなしのホスト
        assert!(!m("http://localhost:3000", "http://localhost:8080"));
        assert!(!m("https://example.com", "https://example.com:8443"));
        assert!(!m("http://0.1", "http://10.0.0.1"));
        assert!(!m("https://com", "https://example.com"));
        assert!(!m("", "https://example.com"));
    }

    #[test]
    fn page_origin_rejects_non_web_pages() {
        assert!(page_origin("https://example.com").is_some());
        for bad in ["chrome://settings", "file:///C:/x.html", "example.com", "https://u:p@example.com", ""] {
            assert!(page_origin(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn registrable_domain() {
        assert_eq!(registrable("www.example.com").as_deref(), Some("example.com"));
        assert_eq!(
            registrable("a.b.example.co.jp").as_deref(),
            Some("example.co.jp")
        );
        assert_eq!(
            registrable("someone.github.io").as_deref(),
            Some("someone.github.io")
        );
        for none in ["com", "co.jp", "github.io", "localhost", "192.168.0.1"] {
            assert_eq!(registrable(none), None, "{none}");
        }
    }

    #[test]
    fn http_is_allowed_only_for_local_hosts() {
        let insecure = |o: &str| insecure_origin(&page_origin(o).unwrap());
        for ok in [
            "https://example.com",
            "http://localhost:3000",
            "http://127.0.0.1:8080",
            "http://192.168.0.1",
            "http://10.1.2.3",
            "http://[::1]:3000",
            "http://nas",
            "http://printer.local",
            "http://app.test",
        ] {
            assert!(!insecure(ok), "{ok}");
        }
        for bad in [
            "http://example.com",
            "http://8.8.8.8",
            "http://[2001:db8::1]",
            "http://localhost.evil.com",
        ] {
            assert!(insecure(bad), "{bad}");
        }
    }

    #[test]
    fn related_and_lookalike_hosts() {
        let r = |item: &str, origin: &str| relation(item, &page_origin(origin).unwrap());
        assert_eq!(
            r("https://example.com", "https://login.example.com"),
            Some(Relation::SameSite)
        );
        assert_eq!(
            r("https://login.example.com", "https://example.com"),
            Some(Relation::SameSite)
        );
        assert_eq!(
            r("https://shop.example.co.jp", "https://pay.example.co.jp"),
            Some(Relation::SameSite)
        );
        for fake in [
            "https://example.com.evil.test",
            "https://example.net",
            "https://examp1e.com",
            "https://exampel.com",
            "https://example-login.com",
            "https://secure.example.evil.test",
            "https://example.com-signin.test",
        ] {
            assert_eq!(
                r("https://example.com", fake),
                Some(Relation::Lookalike),
                "{fake}"
            );
        }
        for other in [
            "https://example.com",
            "https://www.example.com",
            "https://wikipedia.org",
            "https://exams.com",
        ] {
            assert_eq!(r("https://example.com", other), None, "{other}");
        }
        // 他人どうしの無料ホスティングは「同じサイト」にしない
        assert_ne!(
            r("https://alice.github.io", "https://bob.github.io"),
            Some(Relation::SameSite)
        );
        // 短い名前は似ていても対象にしない
        assert_eq!(r("https://x.com", "https://y.com"), None);
        assert!(has_idn_label(&page_origin("https://xn--pple-43d.com").unwrap()));
        assert!(!has_idn_label(&page_origin("https://apple.com").unwrap()));
    }
}
