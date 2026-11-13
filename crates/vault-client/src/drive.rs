//! Google Drive API 版の保管場所。
//!
//! - ログイン：OAuth 2.0 認可コード＋PKCE(S256)、ループバック（http://127.0.0.1:<空きポート>）、state 照合
//!   （RFC 8252「ネイティブアプリの OAuth」の推奨どおり、ログイン画面は OS 標準のブラウザで開く）
//! - 権限：`drive.file` だけ（このアプリが作ったファイルにしか触れない）
//! - トークン：リフレッシュトークンは OS キーチェーン、アクセストークンはメモリのみ
//! - Drive に送るのは暗号文（.2pw）だけ。中身は見ない
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use reqwest::Url;
use reqwest::blocking::{Client, RequestBuilder, Response};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use subtle_eq::ct_eq;

use crate::R;
use crate::config;
use crate::store::{self, Store};

const MAX_API_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CALLBACK_LINE: usize = 8 * 1024;
const CALLBACK_READ_TIME: Duration = Duration::from_secs(10);

fn response_json<T: serde::de::DeserializeOwned>(response: Response) -> R<T> {
    Ok(serde_json::from_slice(&store::read_bounded(
        response,
        MAX_API_RESPONSE_BYTES,
    )?)?)
}

pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3";
/// Drive 上のフォルダ名（同期フォルダ版の「2password」と区別する）。
/// `TWOPW_DRIVE_FOLDER` で変えられる（テスト用のフォルダを分けるため）。
pub fn folder_name() -> String {
    std::env::var("TWOPW_DRIVE_FOLDER")
        .ok()
        .filter(|n| {
            !n.is_empty()
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
        })
        .unwrap_or_else(|| "2password-vault".into())
}
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const KEYCHAIN_SERVICE: &str = "2password-google";

mod subtle_eq {
    /// state の比較を定数時間で行う（タイミングから推測されないように）
    pub fn ct_eq(a: &str, b: &str) -> bool {
        a.len() == b.len()
            && a.bytes()
                .zip(b.bytes())
                .fold(0u8, |acc, (x, y)| acc | (x ^ y))
                == 0
    }
}

// ---------------- OAuth クライアント設定 ----------------

#[derive(Deserialize)]
struct ClientFile {
    installed: Option<ClientInfo>,
    web: Option<serde_json::Value>,
}

#[derive(Deserialize, Clone)]
pub struct ClientInfo {
    client_id: String,
    /// デスクトップアプリでは「秘密」として扱えない値（配布物に含まれる前提）。だから PKCE を併用する
    #[serde(default)]
    client_secret: Option<String>,
}

/// `TWOPW_OAUTH_CLIENT` → 端末の設定フォルダ → 既定の設定フォルダ の順で oauth-client.json を探す。
pub fn client_path() -> PathBuf {
    if let Some(p) = std::env::var_os("TWOPW_OAUTH_CLIENT") {
        return PathBuf::from(p);
    }
    let local = config::home().join("oauth-client.json");
    if local.exists() {
        local
    } else {
        config::default_home().join("oauth-client.json")
    }
}

/// ビルド時に埋め込んだ OAuth クライアント（`TWOPW_EMBED_OAUTH_CLIENT` でファイルを指定してビルド）。
/// デスクトップアプリのクライアント シークレットは、Google の説明でも配布物に含まれる前提の値。
const EMBEDDED_CLIENT: &str = include_str!(concat!(env!("OUT_DIR"), "/embedded_oauth_client.json"));

/// OAuth クライアントが使えるか（設定ファイルか埋め込み）
pub fn client_available() -> bool {
    client_path().exists() || !EMBEDDED_CLIENT.trim().is_empty()
}

fn load_client() -> R<ClientInfo> {
    let p = client_path();
    let from_file = std::fs::read(&p);
    let bytes = match from_file {
        Ok(b) => Ok(b),
        Err(_) if !EMBEDDED_CLIENT.trim().is_empty() => Ok(EMBEDDED_CLIENT.as_bytes().to_vec()),
        Err(e) => Err(e),
    };
    let bytes = bytes.map_err(|_| {
        format!(
            "OAuth クライアントの設定ファイルがありません: {}\n  Google Cloud で作った「デスクトップ アプリ」の JSON をこの名前で置いてください（docs/course/02-google-oauth-setup.md）",
            p.display()
        )
    })?;
    let f: ClientFile = serde_json::from_slice(&bytes)?;
    match (f.installed, f.web) {
        (Some(c), _) => Ok(c),
        (None, Some(_)) => Err("OAuth クライアントの種類が「ウェブ アプリケーション」です。「デスクトップ アプリ」で作り直してください".into()),
        _ => Err("oauth-client.json の形式が正しくありません".into()),
    }
}

fn http() -> R<Client> {
    // rustls の暗号プロバイダ（ring）をプロセスに1回だけ登録する
    let _ = rustls::crypto::ring::default_provider().install_default();
    Ok(Client::builder()
        .user_agent(concat!("2password-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .https_only(true)
        .build()?)
}

// ---------------- トークン（OS キーチェーン） ----------------

/// Google のトークンは「OS ユーザー × OAuth クライアント」ごとに1つ（端末の設定フォルダとは独立）。
fn token_entry() -> R<keyring::Entry> {
    let client = load_client()?;
    Ok(keyring::Entry::new(KEYCHAIN_SERVICE, &client.client_id)?)
}

/// 旧方式（設定フォルダごと）で保存したトークン
fn legacy_token_entry() -> R<keyring::Entry> {
    Ok(keyring::Entry::new(
        KEYCHAIN_SERVICE,
        &config::home().display().to_string(),
    )?)
}

fn load_refresh_token() -> Option<SecretString> {
    if let Ok(t) = token_entry().ok()?.get_password() {
        return Some(SecretString::from(t));
    }
    // 旧方式のトークンがあれば新方式へ移す
    let legacy = legacy_token_entry().ok()?;
    let t = legacy.get_password().ok()?;
    if store_refresh_token(&t).is_ok() {
        let _ = legacy.delete_credential();
    }
    Some(SecretString::from(t))
}

fn store_refresh_token(t: &str) -> R<()> {
    token_entry()?.set_password(t)?;
    Ok(())
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: String,
}

fn rand_b64(n: usize) -> R<String> {
    let mut b = vec![0u8; n];
    vault_core::rng::fill(&mut b)?;
    Ok(B64URL.encode(b))
}

/// URL（またはローカルファイル）を OS 標準のブラウザ／アプリで開く。
pub fn open_browser(url: &str) {
    let r = if cfg!(windows) {
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if r.is_err() {
        eprintln!("  （ブラウザを自動で開けませんでした。上の URL を手動で開いてください）");
    }
}

fn respond(stream: &mut TcpStream, deadline: Instant, status: &str, body: &str) {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return;
    };
    if stream
        .set_write_timeout(Some(remaining.min(Duration::from_secs(1))))
        .is_err()
    {
        return;
    }
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>2password</title><body style=\"font-family:sans-serif;padding:40px\"><h2>{body}</h2><p>このタブは閉じて構いません。</p>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
}

fn read_callback_line(stream: &mut TcpStream, login_deadline: Instant) -> io::Result<String> {
    let deadline = login_deadline.min(Instant::now() + CALLBACK_READ_TIME);
    let mut line = Vec::new();
    let mut buffer = [0u8; 512];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "callback deadline exceeded"))?;
        stream.set_read_timeout(Some(remaining))?;
        let n = stream.read(&mut buffer)?;
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "callback deadline exceeded",
            ));
        }
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete callback request",
            ));
        }
        let end = buffer[..n].iter().position(|b| *b == b'\n');
        let count = end.map_or(n, |i| i + 1);
        if line.len() + count > MAX_CALLBACK_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "callback request line too long",
            ));
        }
        line.extend_from_slice(&buffer[..count]);
        if end.is_some() {
            return String::from_utf8(line).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid callback encoding")
            });
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Callback {
    Code(String),
    Denied(String),
}

fn parse_callback(line: &str, state: &str) -> Option<Callback> {
    let parts: Vec<_> = line.split_whitespace().collect();
    if parts.len() != 3
        || parts[0] != "GET"
        || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1")
        || !parts[1].starts_with('/')
        || parts[1].starts_with("//")
    {
        return None;
    }
    let url = Url::parse(&format!("http://127.0.0.1{}", parts[1])).ok()?;
    let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
    if !ct_eq(q.get("state").map(String::as_str).unwrap_or(""), state) {
        return None;
    }
    if let Some(error) = q.get("error") {
        return Some(Callback::Denied(error.clone()));
    }
    q.get("code")
        .filter(|s| !s.is_empty())
        .map(|code| Callback::Code(code.clone()))
}

/// ブラウザで Google にログインし、リフレッシュトークンを OS キーチェーンに保存する。
pub fn login() -> R<()> {
    let client = load_client()?;
    let http = http()?;

    // PKCE：使い捨ての code_verifier と、そのハッシュ code_challenge
    let verifier = rand_b64(32)?; // 43 文字
    let challenge = B64URL.encode(Sha256::digest(verifier.as_bytes()));
    let state = rand_b64(16)?;

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr()?.port());

    let mut url = Url::parse(AUTH_URL)?;
    url.query_pairs_mut()
        .append_pair("client_id", &client.client_id)
        .append_pair("redirect_uri", &redirect)
        .append_pair("response_type", "code")
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");

    println!(
        "ブラウザで Google にログインしてください（権限は「このアプリが作ったファイルだけ」です）"
    );
    println!("  {url}");
    open_browser(url.as_str());

    // ブラウザからのリダイレクトを1回だけ受け取る（5分で打ち切り。タブを閉じられても固まらない）
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    let code = loop {
        if Instant::now() >= deadline {
            return Err("Google へのログインが5分以内に完了しなかったため中止しました。もう一度お試しください".into());
        }
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    return Err("Google へのログインが5分以内に完了しなかったため中止しました。もう一度お試しください".into());
                }
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        stream.set_nonblocking(false)?;
        let callback = read_callback_line(&mut stream, deadline)
            .ok()
            .and_then(|line| parse_callback(&line, &state));
        match callback {
            Some(Callback::Code(code)) => {
                respond(
                    &mut stream,
                    deadline,
                    "200 OK",
                    "✓ 2password のログインが完了しました",
                );
                break code;
            }
            Some(Callback::Denied(error)) => {
                respond(
                    &mut stream,
                    deadline,
                    "200 OK",
                    "ログインが取り消されました",
                );
                return Err(format!("Google から拒否されました: {error}").into());
            }
            None => {
                // Bad, oversized, timed-out, and favicon requests cannot cancel
                // another user's login. Keep waiting for the real callback.
                respond(
                    &mut stream,
                    deadline,
                    "400 Bad Request",
                    "有効なログイン応答ではありません",
                );
            }
        }
    };

    let mut form = vec![
        ("code", code.as_str()),
        ("client_id", client.client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("grant_type", "authorization_code"),
        ("code_verifier", verifier.as_str()),
    ];
    if let Some(s) = &client.client_secret {
        form.push(("client_secret", s.as_str()));
    }
    let resp = http.post(TOKEN_URL).form(&form).send()?;
    let tok: TokenResponse = response_json(check(resp)?)?;
    if !tok.scope.split(' ').any(|s| s == SCOPE) {
        return Err("Google Drive へのアクセスが許可されませんでした（同意画面でチェックを外した可能性があります）。もう一度 `2pw login` してください".into());
    }
    let refresh = tok
        .refresh_token
        .ok_or("リフレッシュトークンが返されませんでした。もう一度 `2pw login` してください")?;
    store_refresh_token(&refresh)?;
    Ok(())
}

/// トークンを Google 側で無効化し、キーチェーンからも消す。
pub fn logout() -> R<()> {
    if let Some(t) = load_refresh_token() {
        let _ = http()?
            .post(REVOKE_URL)
            .form(&[("token", t.expose_secret())])
            .send();
    }
    if let Ok(e) = token_entry() {
        let _ = e.delete_credential();
    }
    Ok(())
}

fn check(resp: Response) -> R<Response> {
    let st = resp.status();
    if st.is_success() {
        return Ok(resp);
    }
    let bytes = store::read_bounded(resp, MAX_API_RESPONSE_BYTES)?;
    let body = String::from_utf8_lossy(&bytes);
    let hint = if body.contains("accessNotConfigured") || body.contains("SERVICE_DISABLED") {
        "\n  → Google Cloud で Google Drive API を有効にしてください"
    } else if body.contains("invalid_grant") {
        "\n  → ログインの有効期限が切れています（テスト中のアプリは7日で失効）。`2pw login` してください"
    } else {
        ""
    };
    let short: String = body.chars().take(300).collect();
    Err(format!("Google API エラー {st}: {short}{hint}").into())
}

// ---------------- Drive を保管場所として使う ----------------

#[derive(Deserialize)]
struct FileList {
    files: Vec<FileMeta>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct FileMeta {
    id: String,
    #[serde(default)]
    size: Option<String>,
    /// 内容やメタデータが変わるたびに増える番号（競合コピーを片付ける前の照合に使う）
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    trashed: bool,
}

pub struct DriveStore {
    http: Client,
    client: ClientInfo,
    access: RefCell<Option<SecretString>>,
    folder_id: RefCell<Option<String>>,
}

impl DriveStore {
    pub fn new() -> R<Self> {
        Ok(Self {
            http: http()?,
            client: load_client()?,
            access: RefCell::new(None),
            folder_id: RefCell::new(None),
        })
    }

    fn refresh(&self) -> R<()> {
        let rt = match load_refresh_token() {
            Some(t) => t,
            None => {
                eprintln!("ℹ Google にまだログインしていません。ログインを始めます。");
                login()?;
                load_refresh_token().ok_or("ログイン後のトークンを読めませんでした")?
            }
        };
        let mut form = vec![
            ("refresh_token", rt.expose_secret()),
            ("client_id", self.client.client_id.as_str()),
            ("grant_type", "refresh_token"),
        ];
        if let Some(s) = &self.client.client_secret {
            form.push(("client_secret", s.as_str()));
        }
        let tok: TokenResponse =
            response_json(check(self.http.post(TOKEN_URL).form(&form).send()?)?)?;
        *self.access.borrow_mut() = Some(SecretString::from(tok.access_token));
        Ok(())
    }

    /// アクセストークンを付けて送る。401 なら1回だけ更新して再送。
    fn send(&self, build: impl Fn(&str) -> RequestBuilder) -> R<Response> {
        if self.access.borrow().is_none() {
            self.refresh()?;
        }
        let tok = self
            .access
            .borrow()
            .as_ref()
            .unwrap()
            .expose_secret()
            .to_string();
        let resp = build(&tok).send()?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.refresh()?;
            let tok = self
                .access
                .borrow()
                .as_ref()
                .unwrap()
                .expose_secret()
                .to_string();
            return check(build(&tok).send()?);
        }
        check(resp)
    }

    fn search(&self, q: &str, fields: &str) -> R<Vec<FileMeta>> {
        let fields = format!("{fields},nextPageToken");
        let r = self.send(|t| {
            self.http
                .get(format!("{API}/files"))
                .bearer_auth(t)
                .query(&[
                    ("q", q),
                    ("fields", fields.as_str()),
                    ("orderBy", "modifiedTime desc"),
                    ("spaces", "drive"),
                    ("pageSize", "100"),
                ])
        })?;
        let list: FileList = response_json(r)?;
        if list.next_page_token.is_some() {
            return Err(
                "too many matching Drive files; resolve stored conflicts before retrying".into(),
            );
        }
        Ok(list.files)
    }

    fn folder(&self, create: bool) -> R<Option<String>> {
        if let Some(id) = self.folder_id.borrow().clone() {
            return Ok(Some(id));
        }
        let q = format!(
            "name = '{}' and mimeType = '{FOLDER_MIME}' and trashed = false",
            folder_name()
        );
        let found = self.search(&q, "files(id)")?;
        let id = match found.into_iter().next() {
            Some(f) => f.id,
            None if create => {
                let meta = serde_json::json!({ "name": folder_name(), "mimeType": FOLDER_MIME });
                let r = self.send(|t| {
                    self.http
                        .post(format!("{API}/files"))
                        .bearer_auth(t)
                        .query(&[("fields", "id")])
                        .json(&meta)
                })?;
                response_json::<FileMeta>(r)?.id
            }
            None => return Ok(None),
        };
        *self.folder_id.borrow_mut() = Some(id.clone());
        Ok(Some(id))
    }

    /// 名前が一致するファイル（新しい順）。複数あれば先頭が本体、残りは競合コピー扱い。
    fn files(&self, name: &str, fields: &str) -> R<Vec<FileMeta>> {
        let Some(folder) = self.folder(false)? else {
            return Ok(vec![]);
        };
        // 検索式に埋め込むので、英数字とハイフン・ドット・下線以外は拒否する（リリースビルドでも検査）
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-._".contains(c))
        {
            return Err("Drive のファイル名に使えない文字が含まれています".into());
        }
        let q = format!("name = '{name}' and '{folder}' in parents and trashed = false");
        self.search(&q, fields)
    }

    fn download(&self, id: &str, limit: usize) -> R<Vec<u8>> {
        let r = self.send(|t| {
            self.http
                .get(format!("{API}/files/{id}"))
                .bearer_auth(t)
                .query(&[("alt", "media")])
        })?;
        store::read_bounded(r, limit)
    }
}

impl Store for DriveStore {
    fn read(&self, name: &str) -> R<Option<Vec<u8>>> {
        match self.files(name, "files(id)")?.first() {
            Some(f) => Ok(Some(self.download(&f.id, store::file_limit(name))?)),
            None => Ok(None),
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> R<()> {
        store::check_size(name, bytes)?;
        if let Some(f) = self.files(name, "files(id)")?.first() {
            // 既存ファイルの中身だけを置き換える（Drive 側に版の履歴が残る）
            self.send(|t| {
                self.http
                    .patch(format!("{UPLOAD}/files/{}", f.id))
                    .bearer_auth(t)
                    .query(&[("uploadType", "media")])
                    .header("Content-Type", "application/octet-stream")
                    .body(bytes.to_vec())
            })?;
            return Ok(());
        }
        let folder = self.folder(true)?.expect("created");
        let boundary = format!("2pw-{}", rand_b64(12)?.replace(['-', '_'], "x"));
        let meta = serde_json::json!({ "name": name, "parents": [folder] });
        let mut body = Vec::with_capacity(bytes.len() + 512);
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        self.send(|t| {
            self.http
                .post(format!("{UPLOAD}/files"))
                .bearer_auth(t)
                .query(&[("uploadType", "multipart"), ("fields", "id")])
                .header(
                    "Content-Type",
                    format!("multipart/related; boundary={boundary}"),
                )
                .body(body.clone())
        })?;
        Ok(())
    }

    /// 2台が同時に「新規作成」した場合などに、同じ名前のファイルが複数できることがある。
    fn conflict_copies(&self, vault_id: &str) -> R<Vec<store::ConflictCopy>> {
        let name = crate::store::vault_file(vault_id);
        let mut out = vec![];
        let mut total = 0;
        for f in self.files(&name, "files(id,version)")?.into_iter().skip(1) {
            let limit = store::conflict_limit(out.len(), total)?;
            // version は一覧を取った時点のもの。ダウンロードまでに変わっても、片付け時の照合で外れるだけ
            let bytes = self.download(&f.id, limit)?;
            total += bytes.len();
            out.push(store::ConflictCopy {
                id: store::CopyRef {
                    label: format!("Drive のファイル {}", f.id),
                    stamp: f.version.unwrap_or_default(),
                    key: f.id,
                },
                bytes,
            });
        }
        Ok(out)
    }

    /// 完全には消さず、ゴミ箱へ移す（Drive のゴミ箱から 30 日間は復元できる）。
    fn retire_conflict_copy(&self, copy: &store::CopyRef) -> R<bool> {
        if copy.stamp.is_empty()
            || copy.key.is_empty()
            || !copy
                .key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
        {
            return Ok(false);
        }
        let r = self.send(|t| {
            self.http
                .get(format!("{API}/files/{}", copy.key))
                .bearer_auth(t)
                .query(&[("fields", "id,version,trashed")])
        })?;
        let now: FileMeta = response_json(r)?;
        if now.trashed || now.version.as_deref() != Some(copy.stamp.as_str()) {
            return Ok(false);
        }
        let body = serde_json::json!({ "trashed": true });
        self.send(|t| {
            self.http
                .patch(format!("{API}/files/{}", copy.key))
                .bearer_auth(t)
                .query(&[("fields", "id")])
                .json(&body)
        })?;
        Ok(true)
    }

    fn describe(&self, name: &str) -> String {
        format!("Google Drive: {}/{name}", folder_name())
    }

    fn size(&self, name: &str) -> R<Option<u64>> {
        Ok(self
            .files(name, "files(id,size)")?
            .first()
            .and_then(|f| f.size.as_ref()?.parse().ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_request(bytes: Vec<u8>) -> io::Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            let _ = stream.write_all(&bytes);
        });
        let (mut stream, _) = listener.accept().unwrap();
        let result = read_callback_line(&mut stream, Instant::now() + Duration::from_secs(2));
        sender.join().unwrap();
        result
    }

    #[test]
    fn callback_accepts_real_codes_and_state_bound_denials() {
        let line = read_request(
            b"GET /?code=abc%2B123&state=expected HTTP/1.1\r\nHost: localhost\r\n".to_vec(),
        )
        .unwrap();
        assert_eq!(
            parse_callback(&line, "expected"),
            Some(Callback::Code("abc+123".into()))
        );
        assert_eq!(
            parse_callback(
                "GET /?error=access_denied&state=expected HTTP/1.1\r\n",
                "expected"
            ),
            Some(Callback::Denied("access_denied".into()))
        );
    }

    #[test]
    fn callback_ignores_malformed_unrelated_and_wrong_state_requests() {
        for line in [
            "GET /favicon.ico HTTP/1.1\r\n",
            "GET /?code=abc&state=other HTTP/1.1\r\n",
            "GET /?error=access_denied HTTP/1.1\r\n",
            "POST /?code=abc&state=expected HTTP/1.1\r\n",
            "GET http://elsewhere/?code=abc&state=expected HTTP/1.1\r\n",
            "garbage\r\n",
        ] {
            assert_eq!(parse_callback(line, "expected"), None);
        }
        assert!(
            parse_callback("GET /?code=good&state=expected HTTP/1.1\r\n", "expected").is_some()
        );
    }

    #[test]
    fn callback_rejects_oversized_incomplete_and_invalid_utf8_lines() {
        assert!(read_request(vec![b'A'; MAX_CALLBACK_LINE + 1]).is_err());
        assert!(read_request(b"GET / HTTP/1.1".to_vec()).is_err());
        assert!(read_request(vec![0xff, b'\n']).is_err());
        let mut exact = vec![b'A'; MAX_CALLBACK_LINE];
        exact[MAX_CALLBACK_LINE - 1] = b'\n';
        assert_eq!(read_request(exact).unwrap().len(), MAX_CALLBACK_LINE);
    }

    #[test]
    fn drip_feeding_cannot_extend_absolute_callback_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            for _ in 0..20 {
                if stream.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let (mut stream, _) = listener.accept().unwrap();
        let start = Instant::now();
        let result = read_callback_line(&mut stream, start + Duration::from_millis(180));
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        drop(stream);
        sender.join().unwrap();
    }
}
