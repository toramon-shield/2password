//! 攻撃者の視点を体験するデモ。
//!
//! 1. Drive から盗めるもの（account.2pw と保管庫の暗号文）だけで何ができるか
//! 2. 端末ごと盗まれて Secret Key も知られた場合、パスワードの強さがどう効くか
use secrecy::SecretString;
use std::path::PathBuf;
use std::time::Instant;
use vault_core::{AccountId, SecretKey};

use crate::Prompter;
use vault_client::config::{self, Config};
use vault_client::store::ACCOUNT_FILE;
use vault_client::{R, read_account};

const BUILTIN: &[&str] = &[
    "123456",
    "password",
    "qwerty",
    "iloveyou",
    "sakura",
    "tokyo2020",
    "abc123",
    "111111",
    "dragon",
    "sakura2024",
    "password1",
    "letmein",
];

pub fn run(p: &Prompter, wordlist: Option<PathBuf>) -> R<()> {
    let cfg = Config::load()?;
    let store = cfg.open_store()?;
    let account = read_account(store.as_ref(), &mut vec![])?;

    println!("━━ 攻撃者が Google Drive から盗めたもの ━━");
    println!(
        "  {}: salt={} / Argon2id m={}MiB t={} p={}",
        ACCOUNT_FILE,
        account.kdf.salt,
        account.kdf.m_kib / 1024,
        account.kdf.t,
        account.kdf.p
    );
    println!("  保管庫の暗号文（中身は読めない）");
    println!();

    // ── 1. Secret Key なし ──
    println!("━━ ケース1：Drive のデータだけ（Secret Key を知らない）━━");
    println!("  たとえ本物のパスワードを当てても、Secret Key が違えば開かないことを確かめます。");
    let pw = p.secret("  本物のアカウントパスワードを入力（攻撃者が偶然当てたと仮定）: ")?;
    let aid = AccountId::parse(&account.aid)?;
    for n in 1..=3 {
        let guess_sk = SecretKey::generate(aid.clone())?;
        let t = Instant::now();
        let r = account.unlock(&pw, &guess_sk);
        println!(
            "  試行{n}: Secret Key をランダムに推測 → {}（{:.2} 秒）",
            if r.is_ok() { "開いた！" } else { "失敗" },
            t.elapsed().as_secs_f64()
        );
    }
    let years = 2f64.powi(128) / 1e12 / 31_557_600.0;
    println!(
        "  Secret Key は 2^128 ≒ 3.4×10^38 通り。毎秒1兆回試せても約 {years:.1e} 年かかります。"
    );
    println!("  → Drive の流出だけなら、パスワードの総当たりを始めることすらできません。");
    println!();

    // ── 2. Secret Key あり（端末ごと盗まれた）──
    println!("━━ ケース2：端末ごと盗まれ、Secret Key も知られた ━━");
    let sk = config::keychain_load(&cfg.aid)?;
    let words: Vec<String> = match &wordlist {
        Some(path) => std::fs::read_to_string(path)?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        None => BUILTIN.iter().map(|s| s.to_string()).collect(),
    };
    println!(
        "  辞書 {} 語で総当たりします（1回ごとに Argon2id が走ります）",
        words.len()
    );
    let start = Instant::now();
    let mut found = None;
    for (i, w) in words.iter().enumerate() {
        let ok = account.unlock(&SecretString::from(w.clone()), &sk).is_ok();
        println!(
            "  {:>3}: {:<16} {}",
            i + 1,
            w,
            if ok { "← 一致！" } else { "×" }
        );
        if ok {
            found = Some(w.clone());
            break;
        }
    }
    let tried = found
        .as_ref()
        .map(|f| words.iter().position(|w| w == f).unwrap() + 1)
        .unwrap_or(words.len());
    let per = start.elapsed().as_secs_f64() / tried as f64;
    println!();
    match found {
        Some(w) => println!(
            "  ✕ パスワード「{w}」が見つかりました。辞書にある弱いパスワードは、端末を奪われると破られます。"
        ),
        None => println!("  ✓ 辞書の中にパスワードはありませんでした。"),
    }
    println!(
        "  このPCでは1回あたり約 {per:.2} 秒（Argon2id のおかげで遅い）。1億語の辞書なら約 {:.0} 日。",
        per * 1e8 / 86400.0
    );
    println!(
        "  → 端末の盗難に備えて、アカウントパスワードは長く推測されにくいものにしてください。"
    );
    Ok(())
}
