//! ブラウザ拡張との連絡路。
//!
//! ```text
//! ブラウザ拡張 ──(Native Messaging: 標準入出力)──▶ 2password --native-host
//!                                                      │ 127.0.0.1 + 合言葉（bridge.json）
//!                                                      ▼
//!                                            起動中のデスクトップアプリ（解除済みの保管庫）
//! ```
//!
//! - 保管庫の鍵とアカウントパスワードは、拡張にもブラウザにも渡さない
//! - パスワードを返すのは、ページのオリジンが項目の URL に一致するときだけ（`links::url_matches_origin`）
//! - ブラウザは、マニフェストの `allowed_origins` に書いた拡張だけにホストの起動を許す
//! - アプリとの接続には、設定フォルダの `bridge.json` にある合言葉が要る（同じ利用者のプロセスだけが読める）
use crate::links::{page_origin, url_matches_origin};
use crate::{ItemInput, R, Session, config};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;
use vault_core::Item;
use zeroize::Zeroizing;

/// Native Messaging ホストの名前（拡張の `connectNative` と一致させる）
pub const HOST_NAME: &str = "ai.toramon.twopassword";
/// リポジトリの `extension/manifest.json` の `key` から決まる、開発版（パッケージ化されていない拡張）の ID
pub const DEV_EXTENSION_ID: &str = "fikflmlcdfanbjpplbnhlhnhbmefokoe";
/// Chrome ウェブストアで公開した拡張の ID（公開後に追加する）
pub const STORE_EXTENSION_IDS: &[&str] = &[];

const MAX_FRAME: usize = 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_FIELD: usize = 1024;

// ---------------- 枠（4バイトの長さ + JSON。Native Messaging と同じ形式） ----------------

pub fn read_frame(r: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "メッセージが大きすぎます",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

pub fn write_frame(w: &mut impl Write, body: &[u8]) -> std::io::Result<()> {
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(body)?;
    w.flush()
}

// ---------------- 拡張からの要求 ----------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// アプリの状態（ロック中かどうか）
    Status,
    /// このページに使える項目の一覧（パスワードは含めない）
    List { origin: String },
    /// 利用者が選んだ項目のユーザー名とパスワード
    Fill { origin: String, id: String },
    /// 送信されたログイン情報が保存済みかどうか
    Check {
        origin: String,
        username: String,
        password: String,
    },
    /// 新しい項目として保存する
    Save {
        origin: String,
        #[serde(default)]
        title: String,
        username: String,
        password: String,
    },
    /// 既存の項目のパスワードを更新する
    Update {
        origin: String,
        id: String,
        password: String,
    },
    /// 強いパスワードを作る（保存はしない）
    Generate,
    /// アプリの画面を前面に出す（ロック解除のため）
    Focus,
}

impl Drop for Request {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        match self {
            Request::Check { password, .. }
            | Request::Save { password, .. }
            | Request::Update { password, .. } => password.zeroize(),
            _ => {}
        }
    }
}

/// 要求を処理した結果。`response` を拡張に返し、残りはアプリ側の後処理に使う。
pub struct Outcome {
    pub response: Value,
    /// 利用者の操作にあたる（自動ロックの計測をやり直す）
    pub activity: bool,
    /// 保管庫が変わった（アプリの一覧を更新する）
    pub changed: bool,
    /// アプリの画面を前面に出す
    pub focus: bool,
}

impl Outcome {
    fn ok(v: Value) -> Self {
        let mut v = v;
        v["ok"] = Value::Bool(true);
        Self {
            response: v,
            activity: false,
            changed: false,
            focus: false,
        }
    }
    fn err(code: &str, message: &str) -> Self {
        Self {
            response: json!({ "ok": false, "error": code, "message": message }),
            activity: false,
            changed: false,
            focus: false,
        }
    }
}

fn matches(item: &Item, origin: &reqwest::Url) -> bool {
    item.urls.iter().any(|u| url_matches_origin(u, origin))
}

fn too_long(fields: &[&str]) -> bool {
    fields.iter().any(|f| f.chars().count() > MAX_FIELD)
}

/// 拡張からの要求を処理する。`session` が None ならロック中。
pub fn handle(req: Request, session: Option<&mut Session>) -> Outcome {
    match &req {
        Request::Status => {
            return Outcome::ok(json!({
                "configured": config::Config::exists(),
                "locked": session.is_none(),
                "version": env!("CARGO_PKG_VERSION"),
            }));
        }
        Request::Focus => {
            let mut o = Outcome::ok(json!({}));
            o.focus = true;
            return o;
        }
        Request::Generate => {
            return match vault_core::generator::generate(24) {
                Ok(p) => Outcome::ok(json!({ "password": p.as_str() })),
                Err(e) => Outcome::err("failed", &e.to_string()),
            };
        }
        _ => {}
    }

    let Some(s) = session else {
        return Outcome::err("locked", "2password がロックされています");
    };
    let origin_str = match &req {
        Request::List { origin }
        | Request::Fill { origin, .. }
        | Request::Check { origin, .. }
        | Request::Save { origin, .. }
        | Request::Update { origin, .. } => origin.as_str(),
        _ => unreachable!(),
    };
    let Some(origin) = page_origin(origin_str) else {
        return Outcome::err("bad_origin", "このページでは使えません");
    };

    match &req {
        Request::List { .. } => {
            let mut items: Vec<_> = s
                .vault
                .items()
                .filter(|i| matches(i, &origin))
                .map(|i| json!({ "id": i.id, "title": i.title, "username": i.username }))
                .collect();
            items.sort_by_key(|v| v["title"].as_str().unwrap_or_default().to_lowercase());
            Outcome::ok(json!({ "items": items }))
        }
        Request::Fill { id, .. } => match s.get(id) {
            Some(i) if matches(i, &origin) => {
                let mut o = Outcome::ok(json!({ "username": i.username, "password": i.password }));
                o.activity = true;
                o
            }
            // 項目があっても、URL が一致しないページには返さない（偽サイト対策）
            _ => Outcome::err("no_match", "このサイトに一致する項目がありません"),
        },
        Request::Check {
            username, password, ..
        } => {
            if password.is_empty() || too_long(&[username, password]) {
                return Outcome::ok(json!({ "result": "ignore" }));
            }
            let same_site: Vec<&Item> = s.vault.items().filter(|i| matches(i, &origin)).collect();
            if same_site
                .iter()
                .any(|i| i.password == *password && (username.is_empty() || i.username == *username))
            {
                return Outcome::ok(json!({ "result": "known" }));
            }
            match same_site
                .iter()
                .find(|i| !username.is_empty() && i.username == *username)
            {
                Some(i) => Outcome::ok(json!({ "result": "update", "id": i.id, "title": i.title })),
                None => Outcome::ok(json!({ "result": "new" })),
            }
        }
        Request::Save {
            title,
            username,
            password,
            ..
        } => {
            if password.is_empty() || too_long(&[title, username, password]) {
                return Outcome::err("bad_request", "保存できない内容です");
            }
            let host = origin.host_str().unwrap_or_default().to_string();
            let title = if title.trim().is_empty() {
                host.strip_prefix("www.").unwrap_or(&host).to_string()
            } else {
                title.trim().to_string()
            };
            let url = origin.origin().ascii_serialization();
            match s.add(ItemInput {
                title,
                urls: vec![url],
                username: username.clone(),
                password: Some(Zeroizing::new(password.clone())),
                generate_length: 24,
                notes: String::new(),
            }) {
                Ok(id) => {
                    let mut o = Outcome::ok(json!({ "id": id }));
                    o.activity = true;
                    o.changed = true;
                    o
                }
                Err(e) => Outcome::err("failed", &e.to_string()),
            }
        }
        Request::Update { id, password, .. } => {
            if password.is_empty() || too_long(&[password]) {
                return Outcome::err("bad_request", "保存できない内容です");
            }
            let Some(old) = s.get(id).filter(|i| matches(i, &origin)).cloned() else {
                return Outcome::err("no_match", "このサイトに一致する項目がありません");
            };
            match s.update(
                id,
                ItemInput {
                    title: old.title.clone(),
                    urls: old.urls.clone(),
                    username: old.username.clone(),
                    password: Some(Zeroizing::new(password.clone())),
                    generate_length: 24,
                    notes: old.notes.clone(),
                },
            ) {
                Ok(()) => {
                    let mut o = Outcome::ok(json!({ "id": id }));
                    o.activity = true;
                    o.changed = true;
                    o
                }
                Err(e) => Outcome::err("failed", &e.to_string()),
            }
        }
        _ => unreachable!(),
    }
}

// ---------------- アプリ側：127.0.0.1 で待ち受ける ----------------

#[derive(Serialize, Deserialize)]
struct BridgeFile {
    port: u16,
    token: String,
}

fn bridge_path() -> PathBuf {
    config::default_home().join("bridge.json")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn same_token(a: &str, b: &str) -> bool {
    // 長さ以外の情報を時間差から漏らさない
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn write_private(path: &std::path::Path, body: &[u8]) -> R<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(body)?;
    Ok(())
}

#[derive(Deserialize)]
struct Envelope {
    token: String,
    request: Value,
}

/// アプリの起動時に呼ぶ。接続ごとに `handler` を呼び、その結果を返す。
/// 返り値のスレッドはアプリの終了まで動き続ける。
pub fn serve(handler: impl Fn(Request) -> Value + Send + Sync + 'static) -> R<()> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let token = hex(&vault_core::rng::array::<32>()?);
    let file = BridgeFile {
        port: listener.local_addr()?.port(),
        token: token.clone(),
    };
    write_private(&bridge_path(), &serde_json::to_vec(&file)?)?;
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            // 要求は短く、処理も速いので1件ずつ順に処理する（保管庫への同時書き込みも避けられる）
            let _ = serve_one(&mut conn, &token, &handler);
        }
    });
    Ok(())
}

fn serve_one(
    conn: &mut TcpStream,
    token: &str,
    handler: &(impl Fn(Request) -> Value + Send + Sync),
) -> R<()> {
    conn.set_read_timeout(Some(IO_TIMEOUT))?;
    conn.set_write_timeout(Some(IO_TIMEOUT))?;
    let Some(frame) = read_frame(conn)? else {
        return Ok(());
    };
    let response = match serde_json::from_slice::<Envelope>(&frame) {
        Ok(env) if same_token(&env.token, token) => {
            match serde_json::from_value::<Request>(env.request) {
                Ok(req) => handler(req),
                Err(_) => json!({ "ok": false, "error": "bad_request", "message": "不明な要求です" }),
            }
        }
        // 合言葉が違う接続には何も返さない
        _ => return Ok(()),
    };
    write_frame(conn, &serde_json::to_vec(&response)?)?;
    Ok(())
}

// ---------------- ホスト側：ブラウザから起動され、アプリへ中継する ----------------

fn forward(request: Value) -> R<Value> {
    let file: BridgeFile = serde_json::from_slice(&std::fs::read(bridge_path())?)?;
    let mut conn = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, file.port)),
        Duration::from_secs(2),
    )?;
    conn.set_read_timeout(Some(Duration::from_secs(60)))?;
    conn.set_write_timeout(Some(IO_TIMEOUT))?;
    write_frame(
        &mut conn,
        &serde_json::to_vec(&json!({ "token": file.token, "request": request }))?,
    )?;
    let frame = read_frame(&mut conn)?.ok_or("アプリから応答がありません")?;
    Ok(serde_json::from_slice(&frame)?)
}

/// ブラウザが渡す引数（`chrome-extension://<ID>/`）から拡張の ID を取り出す
pub fn extension_id_from_arg(arg: &str) -> Option<&str> {
    let id = arg
        .strip_prefix("chrome-extension://")?
        .trim_end_matches('/');
    valid_extension_id(id).then_some(id)
}

/// Native Messaging ホストとして動く（ブラウザが標準入出力でつなぐ）。標準入力が閉じたら終わる。
pub fn run_native_host(caller: &str) -> R<()> {
    let allowed = allowed_extension_ids();
    let permitted = extension_id_from_arg(caller).is_some_and(|id| allowed.iter().any(|a| a == id));
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let (mut stdin, mut stdout) = (stdin.lock(), stdout.lock());
    while let Some(frame) = read_frame(&mut stdin)? {
        let response = if !permitted {
            json!({ "ok": false, "error": "not_allowed", "message": "この拡張機能は 2password に登録されていません" })
        } else {
            match serde_json::from_slice::<Value>(&frame) {
                Ok(request) => forward(request).unwrap_or_else(|_| {
                    json!({ "ok": false, "error": "app_not_running", "message": "2password アプリが起動していません" })
                }),
                Err(_) => json!({ "ok": false, "error": "bad_request", "message": "不明な要求です" }),
            }
        };
        write_frame(&mut stdout, &serde_json::to_vec(&response)?)?;
    }
    Ok(())
}

// ---------------- ブラウザへの登録 ----------------

pub fn valid_extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

fn extra_ids_path() -> PathBuf {
    config::default_home().join("extension-ids.json")
}

/// 利用者が設定画面で追加した拡張の ID
pub fn extra_extension_ids() -> Vec<String> {
    std::fs::read(extra_ids_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|id| valid_extension_id(id))
        .collect()
}

pub fn allowed_extension_ids() -> Vec<String> {
    let mut ids: Vec<String> = std::iter::once(DEV_EXTENSION_ID)
        .chain(STORE_EXTENSION_IDS.iter().copied())
        .map(String::from)
        .collect();
    for id in extra_extension_ids() {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

fn manifest_path() -> PathBuf {
    config::default_home()
        .join("native-host")
        .join(format!("{HOST_NAME}.json"))
}

#[cfg(windows)]
const REGISTRY_ROOTS: &[(&str, &str)] = &[
    ("Chrome", r"HKCU\Software\Google\Chrome"),
    ("Edge", r"HKCU\Software\Microsoft\Edge"),
    ("Brave", r"HKCU\Software\BraveSoftware\Brave-Browser"),
    ("Chromium", r"HKCU\Software\Chromium"),
];

#[cfg(windows)]
fn reg(args: &[&str]) -> R<bool> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Ok(std::process::Command::new("reg")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?
        .success())
}

#[cfg(not(windows))]
fn browser_dirs() -> Vec<(&'static str, PathBuf)> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return vec![];
    };
    let list: &[(&str, &str)] = if cfg!(target_os = "macos") {
        &[
            ("Chrome", "Library/Application Support/Google/Chrome"),
            ("Edge", "Library/Application Support/Microsoft Edge"),
            (
                "Brave",
                "Library/Application Support/BraveSoftware/Brave-Browser",
            ),
            ("Chromium", "Library/Application Support/Chromium"),
        ]
    } else {
        &[
            ("Chrome", ".config/google-chrome"),
            ("Edge", ".config/microsoft-edge"),
            ("Brave", ".config/BraveSoftware/Brave-Browser"),
            ("Chromium", ".config/chromium"),
        ]
    };
    list.iter().map(|(n, p)| (*n, home.join(p))).collect()
}

fn manifest_json(exe: &std::path::Path) -> R<Vec<u8>> {
    let origins: Vec<String> = allowed_extension_ids()
        .iter()
        .map(|id| format!("chrome-extension://{id}/"))
        .collect();
    Ok(serde_json::to_vec_pretty(&json!({
        "name": HOST_NAME,
        "description": "2password browser integration",
        "path": exe,
        "type": "stdio",
        "allowed_origins": origins,
    }))?)
}

/// このアプリを Native Messaging ホストとしてブラウザに登録する。登録したブラウザの名前を返す。
/// 管理者権限は使わず、いまの利用者の分だけ登録する。
pub fn install_native_host() -> R<Vec<String>> {
    let exe = std::env::current_exe()?;
    let body = manifest_json(&exe)?;
    let mut done = vec![];
    #[cfg(windows)]
    {
        let path = manifest_path();
        write_private(&path, &body)?;
        let path = path.display().to_string();
        for (name, root) in REGISTRY_ROOTS {
            let key = format!(r"{root}\NativeMessagingHosts\{HOST_NAME}");
            if reg(&["add", &key, "/ve", "/t", "REG_SZ", "/d", &path, "/f"])? {
                done.push((*name).to_string());
            }
        }
    }
    #[cfg(not(windows))]
    {
        write_private(&manifest_path(), &body)?;
        for (name, dir) in browser_dirs() {
            // そのブラウザを使ったことがある場合だけ登録する
            if dir.is_dir() {
                write_private(
                    &dir.join("NativeMessagingHosts")
                        .join(format!("{HOST_NAME}.json")),
                    &body,
                )?;
                done.push(name.to_string());
            }
        }
    }
    if done.is_empty() {
        return Err("対応するブラウザ（Chrome / Edge / Brave）が見つかりませんでした".into());
    }
    Ok(done)
}

/// ブラウザへの登録を消す
pub fn uninstall_native_host() -> R<()> {
    #[cfg(windows)]
    for (_, root) in REGISTRY_ROOTS {
        let key = format!(r"{root}\NativeMessagingHosts\{HOST_NAME}");
        let _ = reg(&["delete", &key, "/f"]);
    }
    #[cfg(not(windows))]
    for (_, dir) in browser_dirs() {
        let _ = std::fs::remove_file(
            dir.join("NativeMessagingHosts")
                .join(format!("{HOST_NAME}.json")),
        );
    }
    let _ = std::fs::remove_file(manifest_path());
    Ok(())
}

/// いま動いているアプリが登録されているか（アプリを移動・更新すると false になる）
pub fn native_host_installed() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    std::fs::read(manifest_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|v| {
            v["path"].as_str().map(PathBuf::from) == Some(exe)
                && allowed_extension_ids().iter().all(|id| {
                    v["allowed_origins"].as_array().is_some_and(|a| {
                        a.iter()
                            .any(|o| o.as_str() == Some(&format!("chrome-extension://{id}/")))
                    })
                })
        })
}

/// 拡張の ID を追加する（ストア版の ID が決まる前や、自分でビルドした拡張を使うとき）
pub fn add_extension_id(id: &str) -> R<()> {
    let id = id.trim();
    if !valid_extension_id(id) {
        return Err("拡張機能の ID は a〜p の 32 文字です".into());
    }
    let mut ids = extra_extension_ids();
    if !ids.iter().any(|i| i == id) {
        ids.push(id.to_string());
    }
    write_private(&extra_ids_path(), &serde_json::to_vec(&ids)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_reject_oversize() {
        let mut buf = vec![];
        write_frame(&mut buf, b"{\"a\":1}").unwrap();
        assert_eq!(
            read_frame(&mut buf.as_slice()).unwrap().as_deref(),
            Some(&b"{\"a\":1}"[..])
        );
        assert!(read_frame(&mut &b""[..]).unwrap().is_none());
        // ブラウザのページが HTTP で話しかけてきた場合など（"GET " は巨大な長さとして読まれる）
        assert!(read_frame(&mut &b"GET / HTTP/1.1\r\n"[..]).is_err());
    }

    #[test]
    fn extension_ids_are_validated() {
        assert!(valid_extension_id(DEV_EXTENSION_ID));
        assert!(!valid_extension_id("abc"));
        assert!(!valid_extension_id("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"));
        assert_eq!(
            extension_id_from_arg(&format!("chrome-extension://{DEV_EXTENSION_ID}/")),
            Some(DEV_EXTENSION_ID)
        );
        assert_eq!(extension_id_from_arg("https://example.com/"), None);
    }

    #[test]
    fn token_comparison() {
        assert!(same_token("abc", "abc"));
        assert!(!same_token("abc", "abd"));
        assert!(!same_token("abc", "abcd"));
    }

    #[test]
    fn locked_app_answers_status_but_not_items() {
        let v = |r: &str| handle(serde_json::from_str(r).unwrap(), None).response;
        assert_eq!(v(r#"{"type":"status"}"#)["locked"], true);
        assert_eq!(
            v(r#"{"type":"list","origin":"https://example.com"}"#)["error"],
            "locked"
        );
        assert_eq!(
            v(r#"{"type":"fill","origin":"https://example.com","id":"x"}"#)["error"],
            "locked"
        );
        assert!(serde_json::from_str::<Request>(r#"{"type":"unlock","password":"x"}"#).is_err());
    }
}
