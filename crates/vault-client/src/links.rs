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

/// 項目の URL が、いま開いているページのオリジンに自動入力してよいものか判定する。
///
/// - スキーム（http / https）とポートが同じであること。https で保存した項目は http のページに入力しない
/// - ホスト名が同じ（先頭の `www.` は無視）、またはページが項目のホストのサブドメインであること
/// - IP アドレスとドットのないホスト名（localhost など）は完全一致だけ
///
/// 似た名前の偽サイト（`example.com.evil.test` など）には一致しない。
pub fn url_matches_origin(item_url: &str, origin: &Url) -> bool {
    let Some(item) = safe_web_url(item_url).and_then(|u| Url::parse(&u).ok()) else {
        return false;
    };
    if item.scheme() != origin.scheme()
        || item.port_or_known_default() != origin.port_or_known_default()
    {
        return false;
    }
    let (Some(a), Some(b)) = (item.host_str(), origin.host_str()) else {
        return false;
    };
    let a = strip_www(a).to_ascii_lowercase();
    let b = strip_www(b).to_ascii_lowercase();
    if a == b {
        return true;
    }
    // サブドメインの一致は、ふつうのドメイン名どうしに限る
    let is_ip = |h: &str| h.starts_with('[') || h.parse::<std::net::Ipv4Addr>().is_ok();
    !is_ip(&a) && !is_ip(&b) && a.contains('.') && b.ends_with(&format!(".{a}"))
}

#[cfg(test)]
mod tests {
    use super::{page_origin, safe_web_url, url_matches_origin};

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
        assert!(m("https://example.com", "https://login.example.com"));
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
}
