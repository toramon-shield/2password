//! 2pw — 2password のコマンドラインクライアント（処理の本体は vault-client）
#![forbid(unsafe_code)]

mod attack;

use clap::{Parser, Subcommand};
use secrecy::{ExposeSecret, SecretString};
use std::io::BufRead;
use std::path::PathBuf;
use vault_client::config::Backend;
use vault_client::{
    ItemInput, R, Session, StoreChoice, check_new_password, drive, fmt_time, init_account,
    join_account,
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
        let p1 = self.secret("新しいアカウントパスワード: ")?;
        let p2 = self.secret("もう一度入力: ")?;
        let score = check_new_password(&p1, &p2, allow_weak)?;
        println!("✓ 強度: {score}/4");
        Ok(p1)
    }
}

fn print_notices(n: &mut Vec<String>) {
    for m in n.drain(..) {
        eprintln!("ℹ {m}");
    }
}

fn unlock(p: &Prompter) -> R<Session> {
    let pw = p.secret("アカウントパスワード: ")?;
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
    println!("… Secret Key を OS の乱数で生成（128bit）");
    println!("… Argon2id で鍵を導出し、Vault Key を包みます");
    let r = init_account(choice(store, drive), &pw, &kit_path)?;
    println!("  （鍵の導出に {:.2} 秒）", r.kdf_secs);
    println!("✓ 保管場所: {}", r.location);
    println!("✓ Secret Key を OS のキーチェーンに保存しました");
    println!("✓ Emergency Kit を出力しました: {}", kit_path.display());
    println!();
    println!("  Secret Key: {}", r.secret_key.as_str());
    println!();
    println!(
        "  → Emergency Kit を印刷して施錠できる場所に保管し、HTML ファイルは削除してください。"
    );
    Ok(())
}

fn cmd_join(p: &Prompter, store: Option<PathBuf>, drive: bool) -> R<()> {
    let sk = p.secret("Secret Key（Emergency Kit に記載）: ")?;
    let pw = p.secret("アカウントパスワード: ")?;
    let mut r = join_account(choice(store, drive), &sk, &pw)?;
    print_notices(&mut r.notices);
    println!(
        "✓ 解除しました（{}、項目 {} 件）。Secret Key をこの端末のキーチェーンに保存しました",
        r.location, r.items
    );
    Ok(())
}

fn print_saved(s: &Session) {
    println!(
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
        let given = p.secret("保存するパスワード（空なら自動生成）: ")?;
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
    println!("✓ 「{title}」を追加しました");
    Ok(())
}

fn cmd_import(p: &Prompter, file: PathBuf) -> R<()> {
    let text = Zeroizing::new(std::fs::read_to_string(&file)?);
    let items = vault_client::import::parse(&text)?;
    println!("… {} 件を読み込みました", items.len());
    let mut s = unlock(p)?;
    let (added, skipped) = s.import(items)?;
    print_notices(&mut s.notices);
    if added > 0 {
        print_saved(&s);
    }
    println!("✓ {added} 件を取り込みました（重複で飛ばした項目 {skipped} 件）");
    println!(
        "  → {} には平文のパスワードが入っています。取り込みを確かめたら削除してください。",
        file.display()
    );
    Ok(())
}

fn cmd_bridge_request(p: &Prompter, json: String, approve: bool) -> R<()> {
    use vault_client::bridge::{Guard, handle};
    let mut s = unlock(p)?;
    let mut guard = Guard::load();
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
    println!("項目 {} 件（番号 {}）", items.len(), s.vault.counter);
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
    println!("タイトル  : {}", it.title);
    println!("ユーザー名: {}", it.username);
    println!("URL       : {}", it.urls.join(" "));
    println!(
        "パスワード: {}",
        if reveal {
            it.password.as_str()
        } else {
            "••••••••（--reveal で表示 / --copy でコピー）"
        }
    );
    if !it.notes.is_empty() {
        println!("メモ      : {}", it.notes);
    }
    println!("更新      : {}", fmt_time(it.updated_at));
    if copy {
        println!(
            "✓ クリップボードにコピーしました。{clear_after} 秒後に消去します（Ctrl+C で中断すると消去されません）"
        );
        if vault_client::copy_and_clear_blocking(&it.password, clear_after)? {
            println!("✓ クリップボードを消去しました");
        }
    }
    Ok(())
}

fn cmd_rm(p: &Prompter, query: String) -> R<()> {
    let mut s = unlock(p)?;
    let it = s.pick(&query)?;
    s.delete(&it.id)?;
    print_saved(&s);
    println!("✓ 「{}」を削除しました", it.title);
    Ok(())
}

fn cmd_passwd(p: &Prompter) -> R<()> {
    let current = p.secret("現在のアカウントパスワード: ")?;
    let mut s = Session::unlock(&current)?;
    print_notices(&mut s.notices);
    let pw = p.new_password(false)?;
    s.change_password(&current, &pw)?;
    println!(
        "✓ アカウントパスワードを変更しました（Vault Key を包み直しただけで、保管庫本体はそのままです）"
    );
    println!("  他の端末でも次回から新しいパスワードを使います");
    Ok(())
}

fn cmd_migrate(p: &Prompter) -> R<()> {
    let mut s = unlock(p)?;
    let info = s.migrate_to_drive()?;
    println!(
        "✓ {} 件を {} に移しました（番号 {}）",
        s.vault.items().count(),
        info.location,
        info.counter
    );
    println!("  元の同期フォルダのファイルは残っています。動作を確認してから削除してください。");
    println!("  他の端末では `2pw forget` → `2pw join --drive` で切り替えます。");
    Ok(())
}

fn cmd_status() -> R<()> {
    let st = vault_client::status()?;
    println!("この端末の設定 : {}", st.home);
    println!(
        "保管方式       : {}",
        match st.backend {
            Backend::Drive => "Google Drive API",
            Backend::Folder => "同期フォルダ",
        }
    );
    println!("保管場所       : {}", st.location);
    println!("アカウントID   : {}", st.aid);
    println!("既知の最大番号 : {}", st.known_counter);
    println!();
    println!("── Google Drive（保管場所）から見えるもの ──");
    for (name, size) in &st.files {
        match size {
            Some(n) => println!("  {name}  {n} バイト"),
            None => println!("  {name}  （なし）"),
        }
    }
    if let (Some(c), Some(hex)) = (st.header_counter, &st.sample_hex) {
        println!("  保管庫ヘッダ（平文・改ざんは検知）: 番号={c}");
        println!("  中身の例: {hex}…");
    }
    println!("  ※ 項目名・URL・ユーザー名・パスワードはすべて暗号文の中です");
    Ok(())
}

fn run() -> R<()> {
    let cli = Cli::parse();
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
            println!("✓ Google にログインしました（トークンは OS のキーチェーンに保存）");
            Ok(())
        }
        Cmd::Logout => {
            drive::logout()?;
            println!("✓ Google からログアウトしました（トークンを無効化しました）");
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
        Cmd::Import { file } => cmd_import(&p, file),
        Cmd::BridgeRequest { json, approve } => cmd_bridge_request(&p, json, approve),
        Cmd::Passwd => cmd_passwd(&p),
        Cmd::Gen { length } => {
            println!("{}", generator::generate(length)?.as_str());
            Ok(())
        }
        Cmd::Status => cmd_status(),
        Cmd::AttackDemo { wordlist } => attack::run(&p, wordlist),
        Cmd::Forget => {
            vault_client::forget()?;
            println!(
                "✓ この端末の設定と Secret Key を消しました（Drive 上の暗号文は残っています）"
            );
            Ok(())
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("エラー: {e}");
        std::process::exit(1);
    }
}
