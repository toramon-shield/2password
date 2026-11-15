//! 2pw — 2password のコマンドラインクライアント（処理の本体は vault-client）
#![forbid(unsafe_code)]

// 表示言語に訳して出力する（原文は日本語。訳は app/ui/locales の rust 節）
macro_rules! say {
    ($($a:tt)*) => { println!("{}", vault_client::i18n::tr(&format!($($a)*))) };
}
macro_rules! esay {
    ($($a:tt)*) => { eprintln!("{}", vault_client::i18n::tr(&format!($($a)*))) };
}

mod attack;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use secrecy::{ExposeSecret, SecretString};
use std::io::BufRead;
use std::path::PathBuf;
use vault_client::config::Backend;
use vault_client::i18n::tr;
use vault_client::{
    ItemInput, R, Session, StoreChoice, check_new_password, drive, fmt_time, join_account,
};
use vault_core::generator;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    name = "2pw",
    version,
    about = "2password — Google Drive に暗号文だけを置くパスワード保管庫"
)]
struct Cli {
    /// パスワードなどの入力を標準入力から1行ずつ読む（スクリプト・テスト用）
    #[arg(long, global = true)]
    password_stdin: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 新しいアカウントを作る（1台目）
    Init {
        /// 暗号文を置くフォルダ。省略時は Google ドライブ for デスクトップの「マイドライブ\2password」
        #[arg(long, conflicts_with = "drive")]
        store: Option<PathBuf>,
        /// 同期フォルダではなく Google Drive API に保存する（OAuth＋PKCE、drive.file）
        #[arg(long)]
        drive: bool,
        /// Emergency Kit の出力先
        #[arg(long, default_value = "emergency-kit.html")]
        kit: PathBuf,
        /// 弱いパスワードを許可する（攻撃デモ専用。実運用では使わない）
        #[arg(long, hide = true)]
        allow_weak: bool,
    },
    /// 既存のアカウントをこの端末で使う（2台目以降）
    Join {
        #[arg(long, conflicts_with = "drive")]
        store: Option<PathBuf>,
        /// Google Drive API 上のアカウントに参加する
        #[arg(long)]
        drive: bool,
    },
    /// Google にログインする（Drive API 版）
    Login,
    /// Google からログアウトし、トークンを無効化する
    Logout,
    /// 同期フォルダ版の保管庫を Google Drive API 版に移す
    MigrateToDrive,
    /// ログイン情報やメモを追加する
    Add {
        title: String,
        #[arg(long)]
        url: Vec<String>,
        #[arg(long, short)]
        username: Option<String>,
        /// パスワードを自動生成する
        #[arg(long, short)]
        generate: bool,
        #[arg(long, default_value_t = 24)]
        length: usize,
        #[arg(long, default_value = "")]
        notes: String,
    },
    /// 項目の一覧
    List,
    /// 項目を表示する
    Show {
        query: String,
        /// パスワードを画面に表示する
        #[arg(long)]
        reveal: bool,
        /// パスワードをクリップボードにコピーし、一定時間後に消す
        #[arg(long, short)]
        copy: bool,
        #[arg(long, default_value_t = 30)]
        clear_after: u64,
    },
    /// 項目を削除する
    Rm { query: String },
    /// ワンタイムパスワード（2 段階認証のコード）を表示する
    Totp {
        query: String,
        /// 設定キーか otpauth:// の URL を登録する（値は入力を求める）
        #[arg(long, conflicts_with = "clear")]
        set: bool,
        /// 登録を外す
        #[arg(long)]
        clear: bool,
    },
    /// 1Password・Chrome などが書き出した CSV を取り込む
    Import {
        /// CSV ファイル（1 行目が見出し）
        file: PathBuf,
    },
    /// ブラウザ拡張からの要求を 1 件処理して結果を表示する（テスト用）
    #[command(hide = true)]
    BridgeRequest {
        /// 要求の JSON
        json: String,
        /// アプリの画面での「許可」を押したものとして扱う
        #[arg(long)]
        approve: bool,
        /// 「入力のたびに確認する」設定で処理する
        #[arg(long)]
        confirm_each: bool,
    },
    /// アカウントパスワードを変更する
    Passwd,
    /// パスワードを生成して表示する（保存しない）
    Gen {
        #[arg(default_value_t = 24)]
        length: usize,
    },
    /// Drive 側から見えている情報を表示する
    Status,
    /// 攻撃者の視点で、Secret Key の有無による違いを体験する
    AttackDemo {
        /// 試すパスワード候補（1行1語）
        #[arg(long)]
        wordlist: Option<PathBuf>,
    },
    /// この端末から設定とキーチェーンの Secret Key を消す（Drive 上のデータは残る）
    Forget,
}

// ---------------- 入力 ----------------

pub struct Prompter {
    stdin_mode: bool,
}

impl Prompter {
    pub fn secret(&self, label: &str) -> R<SecretString> {
        if self.stdin_mode {
            let mut s = Zeroizing::new(String::new());
            std::io::stdin().lock().read_line(&mut s)?;
            return Ok(SecretString::from(
                s.trim_end_matches(['\r', '\n']).to_string(),
            ));
        }
        Ok(SecretString::from(rpassword::prompt_password(label)?))
    }

    fn new_password(&self, allow_weak: bool) -> R<SecretString> {
        let p1 = self.secret(&tr("新しいアカウントパスワード: "))?;
        let p2 = self.secret(&tr("もう一度入力: "))?;
        let score = check_new_password(&p1, &p2, allow_weak)?;
        say!("✓ 強度: {score}/4");
        Ok(p1)
    }
}

fn print_notices(n: &mut Vec<String>) {
    for m in n.drain(..) {
        eprintln!("ℹ {m}");
    }
}

fn unlock(p: &Prompter) -> R<Session> {
    let pw = p.secret(&tr("アカウントパスワード: "))?;
    let mut s = Session::unlock(&pw)?;
    print_notices(&mut s.notices);
    Ok(s)
}

fn choice(store: Option<PathBuf>, drive: bool) -> StoreChoice {
    if drive {
        StoreChoice::Drive
    } else {
        StoreChoice::Folder(store)
    }
}

// ---------------- コマンド ----------------

fn cmd_init(
    p: &Prompter,
    store: Option<PathBuf>,
    drive: bool,
    kit_path: PathBuf,
    allow_weak: bool,
) -> R<()> {
    let pw = p.new_password(allow_weak)?;
    say!("… Secret Key を OS の乱数で生成（128bit）");
    say!("… Argon2id で鍵を導出し、Vault Key を包みます");
    let kit = vault_client::i18n::kit_text(vault_client::i18n::lang());
    let r = vault_client::init_account_with_kit(choice(store, drive), &pw, &kit_path, &kit)?;
    say!("  （鍵の導出に {:.2} 秒）", r.kdf_secs);
    say!("✓ 保管場所: {}", r.location);
    say!("✓ Secret Key を OS のキーチェーンに保存しました");
    say!("✓ Emergency Kit を出力しました: {}", kit_path.display());
    println!();
    println!("  Secret Key: {}", r.secret_key.as_str());
    println!();
    say!("  → Emergency Kit を印刷して施錠できる場所に保管し、HTML ファイルは削除してください。");
    Ok(())
}

fn cmd_join(p: &Prompter, store: Option<PathBuf>, drive: bool) -> R<()> {
    let sk = p.secret(&tr("Secret Key（Emergency Kit に記載）: "))?;
    let pw = p.secret(&tr("アカウントパスワード: "))?;
    let mut r = join_account(choice(store, drive), &sk, &pw)?;
    print_notices(&mut r.notices);
    say!(
        "✓ 解除しました（{}、項目 {} 件）。Secret Key をこの端末のキーチェーンに保存しました",
        r.location,
        r.items
    );
    Ok(())
}

fn print_saved(s: &Session) {
    say!(
        "✓ 暗号文を保存しました（番号 {}）: {}",
        s.vault.counter,
        s.location()
    );
}

fn cmd_add(
    p: &Prompter,
    title: String,
    urls: Vec<String>,
    username: Option<String>,
    generate: bool,
    length: usize,
    notes: String,
) -> R<()> {
    let mut s = unlock(p)?;
    let password = if generate {
        None
    } else {
        let given = p.secret(&tr("保存するパスワード（空なら自動生成）: "))?;
        Some(Zeroizing::new(given.expose_secret().to_string()))
    };
    s.add(ItemInput {
        title: title.clone(),
        urls,
        username: username.unwrap_or_default(),
        password,
        generate_length: length,
        notes,
    })?;
    print_notices(&mut s.notices);
    print_saved(&s);
    say!("✓ 「{title}」を追加しました");
    Ok(())
}

fn cmd_import(p: &Prompter, file: PathBuf) -> R<()> {
    let text = Zeroizing::new(std::fs::read_to_string(&file)?);
    let items = vault_client::import::parse(&text)?;
    say!("… {} 件を読み込みました", items.len());
    let mut s = unlock(p)?;
    let (added, skipped) = s.import(items)?;
    print_notices(&mut s.notices);
    if added > 0 {
        print_saved(&s);
    }
    say!("✓ {added} 件を取り込みました（重複で飛ばした項目 {skipped} 件）");
    say!(
        "  → {} には平文のパスワードが入っています。取り込みを確かめたら削除してください。",
        file.display()
    );
    Ok(())
}

fn cmd_bridge_request(p: &Prompter, json: String, approve: bool, confirm_each: bool) -> R<()> {
    use vault_client::bridge::{Guard, handle};
    let mut s = unlock(p)?;
    let mut guard = Guard::load();
    if confirm_each {
        guard.set_confirm_each(true, false)?;
    }
    let mut out = handle(serde_json::from_str(&json)?, Some(&mut s), &mut guard);
    if approve && out.response["error"] == "needs_approval" {
        guard.resolve(true);
        out = handle(serde_json::from_str(&json)?, Some(&mut s), &mut guard);
    }
    println!("{}", out.response);
    Ok(())
}

fn cmd_list(p: &Prompter) -> R<()> {
    let s = unlock(p)?;
    let mut items: Vec<_> = s.vault.items().collect();
    items.sort_by(|a, b| a.title.cmp(&b.title));
    say!("項目 {} 件（番号 {}）", items.len(), s.vault.counter);
    for i in items {
        println!(
            "  {}  {:<24} {:<20} {}",
            vault_client::short_id(&i.id),
            i.title,
            i.username,
            i.urls.join(" ")
        );
    }
    Ok(())
}

fn cmd_show(p: &Prompter, query: String, reveal: bool, copy: bool, clear_after: u64) -> R<()> {
    let s = unlock(p)?;
    let it = s.pick(&query)?;
    say!("タイトル  : {}", it.title);
    say!("ユーザー名: {}", it.username);
    println!("URL       : {}", it.urls.join(" "));
    let hidden = tr("••••••••（--reveal で表示 / --copy でコピー）");
    // パスワードは訳の処理に通さない（見出しだけを訳す）
    println!(
        "{}{}",
        tr("パスワード: "),
        if reveal {
            it.password.as_str()
        } else {
            &hidden
        }
    );
    if !it.notes.is_empty() {
        say!("メモ      : {}", it.notes);
    }
    say!("更新      : {}", fmt_time(it.updated_at));
    if copy {
        say!(
            "✓ クリップボードにコピーしました。{clear_after} 秒後に消去します（Ctrl+C で中断すると消去されません）"
        );
        if vault_client::copy_and_clear_blocking(&it.password, clear_after)? {
            say!("✓ クリップボードを消去しました");
        }
    }
    Ok(())
}

fn cmd_rm(p: &Prompter, query: String) -> R<()> {
    let mut s = unlock(p)?;
    let it = s.pick(&query)?;
    s.delete(&it.id)?;
    print_saved(&s);
    say!("✓ 「{}」を削除しました", it.title);
    Ok(())
}

fn cmd_totp(p: &Prompter, query: String, set: bool, clear: bool) -> R<()> {
    let mut s = unlock(p)?;
    let it = s.pick(&query)?;
    if set || clear {
        let value = if set {
            Some(p.secret(&tr("設定キー（または otpauth:// の URL）: "))?)
        } else {
            None
        };
        s.set_totp(&it.id, value.as_ref().map(|v| v.expose_secret()))?;
        print_saved(&s);
        if set {
            say!("✓ 「{}」のワンタイムパスワードを登録しました", it.title);
        } else {
            say!("✓ 「{}」のワンタイムパスワードを外しました", it.title);
        }
        if clear {
            return Ok(());
        }
    }
    let item = s.get(&it.id).ok_or("項目が見つかりません")?;
    if item.totp.is_empty() {
        return Err("この項目には、ワンタイムパスワードが登録されていません".into());
    }
    let c = vault_client::totp::current(&item.totp)?;
    say!("{}（あと {} 秒）", c.code, c.remaining);
    Ok(())
}

fn cmd_passwd(p: &Prompter) -> R<()> {
    let current = p.secret(&tr("現在のアカウントパスワード: "))?;
    let mut s = Session::unlock(&current)?;
    print_notices(&mut s.notices);
    let pw = p.new_password(false)?;
    s.change_password(&current, &pw)?;
    say!(
        "✓ アカウントパスワードを変更しました（Vault Key を包み直しただけで、保管庫本体はそのままです）"
    );
    say!("  他の端末でも次回から新しいパスワードを使います");
    Ok(())
}

fn cmd_migrate(p: &Prompter) -> R<()> {
    let mut s = unlock(p)?;
    let info = s.migrate_to_drive()?;
    say!(
        "✓ {} 件を {} に移しました（番号 {}）",
        s.vault.items().count(),
        info.location,
        info.counter
    );
    say!("  元の同期フォルダのファイルは残っています。動作を確認してから削除してください。");
    say!("  他の端末では `2pw forget` → `2pw join --drive` で切り替えます。");
    Ok(())
}

fn cmd_status() -> R<()> {
    let st = vault_client::status()?;
    say!("この端末の設定 : {}", st.home);
    say!(
        "保管方式       : {}",
        match st.backend {
            Backend::Drive => "Google Drive API".to_string(),
            Backend::Folder => tr("同期フォルダ"),
        }
    );
    say!("保管場所       : {}", st.location);
    say!("アカウントID   : {}", st.aid);
    say!("既知の最大番号 : {}", st.known_counter);
    println!();
    say!("── Google Drive（保管場所）から見えるもの ──");
    for (name, size) in &st.files {
        match size {
            Some(n) => say!("  {name}  {n} バイト"),
            None => say!("  {name}  （なし）"),
        }
    }
    if let (Some(c), Some(hex)) = (st.header_counter, &st.sample_hex) {
        say!("  保管庫ヘッダ（平文・改ざんは検知）: 番号={c}");
        say!("  中身の例: {hex}…");
    }
    say!("  ※ 項目名・URL・ユーザー名・パスワードはすべて暗号文の中です");
    Ok(())
}

/// コマンドと引数の説明を、表示言語に訳す（原文は上の /// のコメント）
fn localize(mut c: clap::Command) -> clap::Command {
    if let Some(about) = c.get_about().map(|a| a.to_string()) {
        c = c.about(tr(&about));
    }
    let ids: Vec<_> = c.get_arguments().map(|a| a.get_id().clone()).collect();
    for id in ids {
        c = c.mut_arg(id, |a| match a.get_help().map(|h| h.to_string()) {
            Some(h) => a.help(tr(&h)),
            None => a,
        });
    }
    let subs: Vec<String> = c
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    for name in subs {
        c = c.mut_subcommand(name, localize);
    }
    c
}

fn run() -> R<()> {
    let matches = localize(Cli::command()).get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let p = Prompter {
        stdin_mode: cli.password_stdin,
    };
    match cli.cmd {
        Cmd::Init {
            store,
            drive,
            kit,
            allow_weak,
        } => cmd_init(&p, store, drive, kit, allow_weak),
        Cmd::Join { store, drive } => cmd_join(&p, store, drive),
        Cmd::Login => {
            drive::login()?;
            say!("✓ Google にログインしました（トークンは OS のキーチェーンに保存）");
            Ok(())
        }
        Cmd::Logout => {
            drive::logout()?;
            say!("✓ Google からログアウトしました（トークンを無効化しました）");
            Ok(())
        }
        Cmd::MigrateToDrive => cmd_migrate(&p),
        Cmd::Add {
            title,
            url,
            username,
            generate,
            length,
            notes,
        } => cmd_add(&p, title, url, username, generate, length, notes),
        Cmd::List => cmd_list(&p),
        Cmd::Show {
            query,
            reveal,
            copy,
            clear_after,
        } => cmd_show(&p, query, reveal, copy, clear_after),
        Cmd::Rm { query } => cmd_rm(&p, query),
        Cmd::Totp { query, set, clear } => cmd_totp(&p, query, set, clear),
        Cmd::Import { file } => cmd_import(&p, file),
        Cmd::BridgeRequest {
            json,
            approve,
            confirm_each,
        } => cmd_bridge_request(&p, json, approve, confirm_each),
        Cmd::Passwd => cmd_passwd(&p),
        Cmd::Gen { length } => {
            println!("{}", generator::generate(length)?.as_str());
            Ok(())
        }
        Cmd::Status => cmd_status(),
        Cmd::AttackDemo { wordlist } => attack::run(&p, wordlist),
        Cmd::Forget => {
            vault_client::forget()?;
            say!("✓ この端末の設定と Secret Key を消しました（Drive 上の暗号文は残っています）");
            Ok(())
        }
    }
}

fn main() {
    if let Err(e) = run() {
        esay!("エラー: {}", tr(&e.to_string()));
        std::process::exit(1);
    }
}
