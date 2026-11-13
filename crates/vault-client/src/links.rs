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

#[cfg(test)]
mod tests {
    use super::safe_web_url;

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
}
