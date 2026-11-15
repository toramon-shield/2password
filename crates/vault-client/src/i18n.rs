//! コマンドライン（`2pw`）と、Google ログインの完了ページの表示言語。
//!
//! 訳はデスクトップアプリと同じ `app/ui/locales/<言語>.json` の `rust` 節を使う。
//! 節の中身は「日本語の原文 → 訳」で、原文の `{}` や `{name}` の部分に入った値（ファイル名など）は、
//! そのまま訳に差し込む（アプリの `i18n.js` の `tr` と同じ動き）。訳がない文は日本語のまま出す。
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

/// 対応している言語（アプリの `i18n.js` の `LANGS` と同じ）
pub const LANGS: &[&str] = &[
    "ja", "en", "zh", "pt", "es", "fr", "it", "de", "ru", "tr", "ar", "hi", "th", "vi", "ms",
];

macro_rules! locales {
    ($($code:literal),*) => {
        &[$(($code, include_str!(concat!("../../../app/ui/locales/", $code, ".json")))),*]
    };
}
static FILES: &[(&str, &str)] = locales!(
    "ja", "en", "zh", "pt", "es", "fr", "it", "de", "ru", "tr", "ar", "hi", "th", "vi", "ms"
);

/// 言語コード（`ja-JP`、`pt_BR.UTF-8` など）から、対応している言語を選ぶ
pub fn pick(tag: &str) -> Option<&'static str> {
    let base = tag
        .trim()
        .split(['-', '_', '.', '@'])
        .next()?
        .to_ascii_lowercase();
    LANGS.iter().copied().find(|l| *l == base)
}

/// このプロセスの表示言語。`TWOPW_LANG`、`LC_ALL`・`LC_MESSAGES`・`LANG`、OS の設定の順に見る。
/// 最初に見つかった指定が対応していない言語なら英語（`C`・`POSIX` は指定なしとみなす）
pub fn lang() -> &'static str {
    static LANG: OnceLock<&'static str> = OnceLock::new();
    LANG.get_or_init(|| {
        for var in ["TWOPW_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
            let Ok(v) = std::env::var(var) else { continue };
            let v = v.trim();
            let base = v.split(['.', '@']).next().unwrap_or_default();
            if v.is_empty() || base == "C" || base == "POSIX" {
                continue;
            }
            return pick(v).unwrap_or("en");
        }
        sys_locale::get_locales()
            .find_map(|l| pick(&l))
            .unwrap_or("en")
    })
}

/// ブラウザの `Accept-Language` から、対応している言語を選ぶ（q の大きい順）
pub fn from_accept_language(header: &str) -> Option<&'static str> {
    let mut tags: Vec<(f32, usize, &str)> = header
        .split(',')
        .enumerate()
        .filter_map(|(i, part)| {
            let mut it = part.split(';');
            let tag = it.next()?.trim();
            let q = it
                .find_map(|p| p.trim().strip_prefix("q="))
                .and_then(|q| q.parse::<f32>().ok())
                .unwrap_or(1.0);
            (q > 0.0).then_some((q, i, tag))
        })
        .collect();
    tags.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    tags.into_iter().find_map(|(_, _, t)| pick(t))
}

/// 右から左に書く言語
pub fn is_rtl(lang: &str) -> bool {
    lang == "ar"
}

/// 原文を、このプロセスの表示言語に訳す
pub fn tr(text: &str) -> String {
    tr_in(lang(), text)
}

/// 原文を、指定した言語に訳す
pub fn tr_in(lang: &str, text: &str) -> String {
    if lang == "ja" {
        return text.to_string();
    }
    match table(lang) {
        Some(t) => t.translate(text),
        None => text.to_string(),
    }
}

struct Pattern {
    /// 差し込み口で区切った原文（差し込み口の数 + 1 個）
    parts: Vec<String>,
    slots: Vec<String>,
    dst: String,
}

struct Table {
    exact: HashMap<String, String>,
    patterns: Vec<Pattern>,
}

/// `{}` や `{name}`、`{:.2}` などの差し込み口で区切る
fn split_slots(s: &str) -> (Vec<String>, Vec<String>) {
    let mut parts = vec![String::new()];
    let mut slots = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        match after.find(['{', '}']) {
            Some(end) if after.as_bytes()[end] == b'}' => {
                parts.last_mut().unwrap().push_str(&rest[..start]);
                slots.push(rest[start..start + end + 2].to_string());
                parts.push(String::new());
                rest = &after[end + 1..];
            }
            _ => {
                parts.last_mut().unwrap().push_str(&rest[..=start]);
                rest = after;
            }
        }
    }
    parts.last_mut().unwrap().push_str(rest);
    (parts, slots)
}

/// 原文の型に合えば、差し込み口に入った値を返す（値はできるだけ短く取る）
fn capture(text: &str, pos: usize, parts: &[String], out: &mut Vec<(usize, usize)>) -> bool {
    let lit = parts[0].as_str();
    if parts.len() == 1 {
        if text.len() >= pos + lit.len() && text.ends_with(lit) {
            out.push((pos, text.len() - lit.len()));
            return true;
        }
        return false;
    }
    let mut from = pos;
    while let Some(off) = text[from..].find(lit) {
        let at = from + off;
        out.push((pos, at));
        if capture(text, at + lit.len(), &parts[1..], out) {
            return true;
        }
        out.pop();
        match text[at..].chars().next() {
            Some(c) => from = at + c.len_utf8(),
            None => break,
        }
    }
    false
}

impl Pattern {
    fn apply(&self, text: &str) -> Option<String> {
        let first = self.parts[0].as_str();
        if !text.starts_with(first) {
            return None;
        }
        let mut spans = Vec::new();
        if !capture(text, first.len(), &self.parts[1..], &mut spans) {
            return None;
        }
        let values: Vec<&str> = spans.iter().map(|(a, b)| &text[*a..*b]).collect();
        let named: HashMap<&str, &str> = self
            .slots
            .iter()
            .zip(&values)
            .filter(|(s, _)| s.len() > 2)
            .map(|(s, v)| (s.as_str(), *v))
            .collect();
        // 訳の差し込み口を値で置き換える。名前つきは名前で、{} は順に
        let (dst_parts, dst_slots) = split_slots(&self.dst);
        let mut out = String::new();
        let mut i = 0;
        for (n, part) in dst_parts.iter().enumerate() {
            out.push_str(part);
            if let Some(slot) = dst_slots.get(n) {
                if slot.len() > 2
                    && let Some(v) = named.get(slot.as_str())
                {
                    out.push_str(v);
                } else if let Some(v) = values.get(i) {
                    out.push_str(v);
                    i += 1;
                } else {
                    out.push_str(slot);
                }
            }
        }
        Some(out)
    }
}

impl Table {
    fn translate(&self, text: &str) -> String {
        if let Some(t) = self.exact.get(text) {
            return t.clone();
        }
        self.patterns
            .iter()
            .find_map(|p| p.apply(text))
            .unwrap_or_else(|| text.to_string())
    }
}

/// コマンドラインで作る Emergency Kit の文言（アプリと同じ訳。日本語は `2pw join` を案内する従来の文言）
pub fn kit_text(lang: &str) -> crate::kit::KitText {
    if lang == "ja" || !LANGS.contains(&lang) {
        return crate::kit::KitText::default();
    }
    let ui: HashMap<String, String> = section("en", "ui")
        .into_iter()
        .chain(section(lang, "ui"))
        .collect();
    let t = |k: &str| ui.get(k).cloned().unwrap_or_default();
    crate::kit::KitText {
        lang: lang.to_string(),
        dir: if is_rtl(lang) { "rtl" } else { "ltr" }.to_string(),
        created: t("kit.created"),
        account_id: t("kit.account_id"),
        store: t("kit.store"),
        password: t("kit.password"),
        notes: vec![t("kit.warn1"), t("kit.warn2"), t("kit.warn3")],
        delete_after: t("kit.warn4"),
    }
}

fn rust_section(code: &str) -> Vec<(String, String)> {
    section(code, "rust")
}

fn section(code: &str, name: &str) -> Vec<(String, String)> {
    let Some((_, json)) = FILES.iter().find(|(c, _)| *c == code) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    v.get(name)
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn table(lang: &str) -> Option<&'static Table> {
    static TABLES: [OnceLock<Table>; 15] = [const { OnceLock::new() }; 15];
    let i = LANGS.iter().position(|l| *l == lang)?;
    Some(TABLES[i].get_or_init(|| {
        // 訳のない文は英語で出す（アプリと同じ）
        let mut merged: HashMap<String, String> = rust_section("en").into_iter().collect();
        merged.extend(rust_section(lang));
        let mut patterns: Vec<Pattern> = merged
            .iter()
            .filter_map(|(src, dst)| {
                let (parts, slots) = split_slots(src);
                (!slots.is_empty()).then(|| Pattern {
                    parts,
                    slots,
                    dst: dst.clone(),
                })
            })
            .collect();
        // 決まった文字の多い原文から試す（短い型が、長い文を先に取らないように）
        patterns.sort_by_key(|p| std::cmp::Reverse(p.parts.iter().map(String::len).sum::<usize>()));
        Table {
            exact: merged,
            patterns,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_supported_languages() {
        assert_eq!(pick("ja-JP"), Some("ja"));
        assert_eq!(pick("pt_BR.UTF-8"), Some("pt"));
        assert_eq!(pick("zh-Hans-CN"), Some("zh"));
        assert_eq!(pick("C"), None);
        assert_eq!(pick("nb-NO"), None);
        assert_eq!(from_accept_language("de-DE,de;q=0.9,en;q=0.8"), Some("de"));
        assert_eq!(
            from_accept_language("sv;q=1, fr-CA;q=0.7, ar;q=0.9"),
            Some("ar")
        );
        assert_eq!(from_accept_language("xx, yy"), None);
        assert_eq!(from_accept_language("ja;q=0, en"), Some("en"));
    }

    #[test]
    fn every_locale_file_is_valid_and_has_the_rust_section() {
        let base = rust_section("ja");
        assert!(!base.is_empty());
        for l in LANGS {
            assert_eq!(rust_section(l).len(), base.len(), "{l}");
        }
    }

    #[test]
    fn kit_text_follows_the_language() {
        assert_eq!(kit_text("ja").created, "作成日");
        let ar = kit_text("ar");
        assert_eq!((ar.lang.as_str(), ar.dir.as_str()), ("ar", "rtl"));
        for l in LANGS.iter().filter(|l| **l != "ja") {
            let k = kit_text(l);
            assert!(
                !k.created.is_empty() && k.notes.iter().all(|n| !n.is_empty()),
                "{l}"
            );
        }
    }

    #[test]
    fn translates_with_slots() {
        let (parts, slots) = split_slots("a {} b {name} c {:.2} d");
        assert_eq!(parts, ["a ", " b ", " c ", " d"]);
        assert_eq!(slots, ["{}", "{name}", "{:.2}"]);
        let p = Pattern {
            parts: split_slots("✓ 「{title}」を追加しました").0,
            slots: vec!["{title}".into()],
            dst: "✓ Added “{title}”".into(),
        };
        assert_eq!(
            p.apply("✓ 「メモ」を追加しました").unwrap(),
            "✓ Added “メモ”"
        );
        assert!(p.apply("別の文").is_none());
        let p = Pattern {
            parts: split_slots("{} 件を読み込み（{} 件）").0,
            slots: vec!["{}".into(), "{}".into()],
            dst: "{} loaded ({})".into(),
        };
        assert_eq!(p.apply("3 件を読み込み（1 件）").unwrap(), "3 loaded (1)");
        // 実際の訳：日本語はそのまま、英語は訳ファイルのとおり
        assert_eq!(tr_in("ja", "エラー: x"), "エラー: x");
        for (src, dst) in rust_section("en") {
            assert_eq!(tr_in("en", &src), dst);
        }
    }
}
