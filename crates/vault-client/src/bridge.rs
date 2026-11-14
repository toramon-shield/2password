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
//! - 項目をそのサイトで初めて使うときは、アプリの画面で利用者に確認する（`Guard`）
//! - 短い時間に要求が続いたら保管庫をロックする。http のページには返さない
//! - パスキーの秘密鍵は返さない。署名はアプリの中で行い、結果だけを返す（`passkey`）
//! - ブラウザは、マニフェストの `allowed_origins` に書いた拡張だけにホストの起動を許す
//! - アプリとの接続には、設定フォルダの `bridge.json` にある合言葉が要る（同じ利用者のプロセスだけが読める）
use crate::links::{
    Relation, has_idn_label, insecure_origin, item_host, page_origin, relation, url_matches_origin,
};
use crate::passkey::{self, rp_id_allowed};
use crate::{ItemInput, R, Session, config, now_ms};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};
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
    /// このサイトで使えるパスキーの一覧（鍵は含めない）
    PasskeyList {
        origin: String,
        rp_id: String,
        /// サイトが指定した資格情報 ID（空なら、このサイトのものすべて）
        #[serde(default)]
        allow: Vec<String>,
    },
    /// 新しいパスキーを作って保管庫に入れる
    PasskeyCreate {
        origin: String,
        rp_id: String,
        #[serde(default)]
        rp_name: String,
        challenge: String,
        user_id: String,
        #[serde(default)]
        user_name: String,
        #[serde(default)]
        user_display_name: String,
        /// すでに登録済みなら作らない資格情報 ID
        #[serde(default)]
        exclude: Vec<String>,
    },
    /// 利用者が選んだパスキーで、サイトのチャレンジに署名する（秘密鍵は返さない）
    PasskeyGet {
        origin: String,
        rp_id: String,
        id: String,
        challenge: String,
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
    /// 利用者の確認が要る（アプリの画面に確認を出す）
    pub approval: bool,
    /// 保管庫をロックする（要求が続いたとき）
    pub lock: bool,
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
            approval: false,
            lock: false,
        }
    }
    fn err(code: &str, message: &str) -> Self {
        Self {
            response: json!({ "ok": false, "error": code, "message": message }),
            activity: false,
            changed: false,
            focus: false,
            approval: false,
            lock: false,
        }
    }
}

// ---------------- 拡張を信用しきらないための見張り ----------------
//
// 拡張機能が乗っ取られると（配布元の汚染など）、好きなサイト名を名乗ってパスワードを要求できる。
// そこで、アプリ側で次の 3 つを行う。
//   1. 項目をそのサイトで初めて使うときは、アプリの画面で利用者に確認する
//   2. 短い時間にパスワードの要求が続いたら、保管庫をロックする
//   3. 要求の履歴を残し、利用者が見られるようにする（メモリの中だけ。ディスクには書かない）

/// 確認の待ち時間。過ぎたら無効にする
const APPROVAL_TTL: Duration = Duration::from_secs(120);
const FILL_WINDOW: Duration = Duration::from_secs(60);
/// `FILL_WINDOW` の間に受け付けるパスワード要求の数。超えたらロックする
pub const MAX_FILLS_PER_WINDOW: usize = 8;
const LOG_MAX: usize = 50;

/// 利用者の確認を待っている要求
#[derive(Serialize, Clone)]
pub struct PendingApproval {
    pub item_id: String,
    pub title: String,
    pub username: String,
    /// 入力先のページ（`https://example.com`）
    pub origin: String,
    pub host: String,
    /// パスワードではなく、パスキーでのログイン
    pub passkey: bool,
    #[serde(skip)]
    at: Option<Instant>,
}

/// 拡張からの要求の履歴（パスワードは含めない）
#[derive(Serialize, Clone)]
pub struct LogEntry {
    /// UNIX 時刻（秒）
    pub at: u64,
    pub host: String,
    pub title: String,
    /// filled（入力した）/ asked（確認を求めた）/ allowed / denied / blocked（連続要求でロック）/ saved / updated
    /// passkey_created（パスキーを作った）/ passkey_used（パスキーでログインした）
    pub event: &'static str,
}

pub struct Guard {
    /// 許可済みの組み合わせの保存先。None ならメモリの中だけ
    path: Option<PathBuf>,
    /// SHA-256(項目 ID + オリジン)。ファイルを読まれても、どのサイトを使っているかは分からない
    approved: HashSet<String>,
    pending: Option<PendingApproval>,
    fills: VecDeque<Instant>,
    log: VecDeque<LogEntry>,
}

fn approval_key(item_id: &str, origin: &reqwest::Url) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(item_id.as_bytes());
    h.update(b"\n");
    h.update(origin.origin().ascii_serialization().as_bytes());
    hex(&h.finalize())
}

impl Guard {
    /// この端末の許可済みの一覧を読み込む
    pub fn load() -> Self {
        let path = config::home().join("approved-sites.json");
        let approved = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        Self {
            path: Some(path),
            approved,
            ..Self::in_memory()
        }
    }

    pub fn in_memory() -> Self {
        Self {
            path: None,
            approved: HashSet::new(),
            pending: None,
            fills: VecDeque::new(),
            log: VecDeque::new(),
        }
    }

    fn persist(&self) -> R<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut list: Vec<&String> = self.approved.iter().collect();
        list.sort();
        write_private(path, &serde_json::to_vec(&list)?)
    }

    fn is_approved(&self, item_id: &str, origin: &reqwest::Url) -> bool {
        self.approved.contains(&approval_key(item_id, origin))
    }

    fn approve(&mut self, item_id: &str, origin: &reqwest::Url) {
        if self.approved.insert(approval_key(item_id, origin)) {
            // 書けなくても、このプロセスの間は有効
            let _ = self.persist();
        }
    }

    fn record(&mut self, event: &'static str, host: &str, title: &str) {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if self.log.len() >= LOG_MAX {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            at,
            host: host.to_string(),
            title: title.to_string(),
            event,
        });
    }

    /// パスワードの要求を 1 回数える。多すぎたら true
    fn too_many_fills(&mut self) -> bool {
        let now = Instant::now();
        while self
            .fills
            .front()
            .is_some_and(|t| now.duration_since(*t) > FILL_WINDOW)
        {
            self.fills.pop_front();
        }
        self.fills.push_back(now);
        self.fills.len() > MAX_FILLS_PER_WINDOW
    }

    /// 利用者の確認を待っている要求（期限が切れていたら消す）
    pub fn pending(&mut self) -> Option<PendingApproval> {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.at.is_some_and(|t| t.elapsed() > APPROVAL_TTL))
        {
            self.pending = None;
        }
        self.pending.clone()
    }

    /// 利用者の答えを反映する。許可なら、次の要求からパスワードを返す
    pub fn resolve(&mut self, allow: bool) -> Option<PendingApproval> {
        let p = self.pending()?;
        self.pending = None;
        if allow && let Some(origin) = page_origin(&p.origin) {
            self.approve(&p.item_id, &origin);
        }
        self.record(if allow { "allowed" } else { "denied" }, &p.host, &p.title);
        Some(p)
    }

    /// ロックしたときに呼ぶ。確認待ちと要求の数え直し
    pub fn reset(&mut self) {
        self.pending = None;
        self.fills.clear();
    }

    /// 許可済みの組み合わせをすべて取り消す（次に使うとき、もう一度確認する）
    pub fn forget_approvals(&mut self) -> R<()> {
        self.approved.clear();
        self.pending = None;
        self.persist()
    }

    pub fn approved_count(&self) -> usize {
        self.approved.len()
    }

    /// 新しいものから順に並べた履歴
    pub fn log(&self) -> Vec<LogEntry> {
        self.log.iter().rev().cloned().collect()
    }
}

fn matches(item: &Item, origin: &reqwest::Url) -> bool {
    item.urls.iter().any(|u| url_matches_origin(u, origin))
}

fn too_long(fields: &[&str]) -> bool {
    fields.iter().any(|f| f.chars().count() > MAX_FIELD)
}

fn display_host(origin: &reqwest::Url) -> String {
    let host = origin.host_str().unwrap_or_default();
    match origin.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    }
}

/// 一致する項目がないページについて、保存済みの項目との関係を調べる。
/// 似た名前の別サイト（偽サイトの疑い）を優先して 1 件だけ返す。
fn related_site(s: &Session, origin: &reqwest::Url) -> Option<Value> {
    let mut same_site = None;
    for item in s.vault.items() {
        for url in &item.urls {
            // スキームやポートだけが違う場合も「同じサイトの別の入口」として知らせる
            let rel = relation(url, origin).or_else(|| {
                (item_host(url).as_deref() == origin.host_str().map(|h| h.strip_prefix("www.").unwrap_or(h)))
                    .then_some(Relation::SameSite)
            });
            let Some(host) = item_host(url) else { continue };
            match rel {
                Some(Relation::Lookalike) => {
                    return Some(json!({ "kind": "lookalike", "host": host, "title": item.title }));
                }
                Some(Relation::SameSite) if same_site.is_none() => {
                    same_site = Some(json!({ "kind": "same_site", "host": host, "title": item.title }));
                }
                _ => {}
            }
        }
    }
    same_site
}

/// 秘密（パスワード・パスキーの署名）の要求を 1 回数える。多すぎたらロックする
fn burst_check(guard: &mut Guard, host: &str) -> Option<Outcome> {
    if !guard.too_many_fills() {
        return None;
    }
    guard.record("blocked", host, "");
    guard.reset();
    let mut o = Outcome::err(
        "rate_limited",
        "短い時間に要求が続いたため、2password をロックしました",
    );
    o.lock = true;
    o.focus = true;
    Some(o)
}

/// 項目をこのサイトで初めて使うなら、アプリの画面での確認を求める
fn first_use_check(
    guard: &mut Guard,
    item: &Item,
    origin: &reqwest::Url,
    host: &str,
    passkey: bool,
) -> Option<Outcome> {
    if guard.is_approved(&item.id, origin) {
        return None;
    }
    guard.pending = Some(PendingApproval {
        item_id: item.id.clone(),
        title: item.title.clone(),
        username: item.username.clone(),
        origin: origin.origin().ascii_serialization(),
        host: host.to_string(),
        passkey,
        at: Some(Instant::now()),
    });
    guard.record("asked", host, &item.title);
    let mut o = Outcome::err(
        "needs_approval",
        "このサイトで使うのは初めてです。2password アプリで許可してから、もう一度選んでください",
    );
    o.activity = true;
    o.focus = true;
    o.approval = true;
    Some(o)
}

fn same_b64(a: &str, b: &str) -> bool {
    a.trim_end_matches('=') == b.trim_end_matches('=')
}

/// 拡張からの要求を処理する。`session` が None ならロック中。
pub fn handle(req: Request, session: Option<&mut Session>, guard: &mut Guard) -> Outcome {
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
        | Request::Update { origin, .. }
        | Request::PasskeyList { origin, .. }
        | Request::PasskeyCreate { origin, .. }
        | Request::PasskeyGet { origin, .. } => origin.as_str(),
        _ => unreachable!(),
    };
    let Some(origin) = page_origin(origin_str) else {
        return Outcome::err("bad_origin", "このページでは使えません");
    };
    // 暗号化されていないページでは、通信の途中でパスワードを読まれる
    if insecure_origin(&origin) {
        return Outcome::err(
            "insecure",
            "暗号化されていないページ（http）では使えません",
        );
    }
    let host = display_host(&origin);

    match &req {
        Request::List { .. } => {
            let mut items: Vec<_> = s
                .vault
                .items()
                .filter(|i| !i.password.is_empty() && matches(i, &origin))
                .map(|i| json!({ "id": i.id, "title": i.title, "username": i.username }))
                .collect();
            items.sort_by_key(|v| v["title"].as_str().unwrap_or_default().to_lowercase());
            let mut out = json!({ "items": items, "idn": has_idn_label(&origin) });
            if items_is_empty(&out)
                && let Some(rel) = related_site(s, &origin)
            {
                out["related"] = rel;
            }
            Outcome::ok(out)
        }
        Request::Fill { id, .. } => {
            if let Some(blocked) = burst_check(guard, &host) {
                return blocked;
            }
            match s.get(id) {
                Some(i) if !i.password.is_empty() && matches(i, &origin) => {
                    if let Some(ask) = first_use_check(guard, i, &origin, &host, false) {
                        return ask;
                    }
                    guard.record("filled", &host, &i.title);
                    let mut o =
                        Outcome::ok(json!({ "username": i.username, "password": i.password }));
                    o.activity = true;
                    o
                }
                // 項目があっても、URL が一致しないページには返さない（偽サイト対策）
                _ => Outcome::err("no_match", "このサイトに一致する項目がありません"),
            }
        }
        Request::PasskeyList { rp_id, allow, .. } => {
            if !rp_id_allowed(rp_id, &origin) {
                return Outcome::err("bad_rp", "このページでは、このサイト名のパスキーを使えません");
            }
            let mut items: Vec<_> = s
                .vault
                .items()
                .filter_map(|i| {
                    let p = i.passkey.as_ref()?;
                    let wanted = allow.is_empty()
                        || allow.iter().any(|a| same_b64(a, &p.credential_id));
                    (p.rp_id == *rp_id && wanted).then(|| {
                        let name = if p.user_name.is_empty() { &i.username } else { &p.user_name };
                        json!({ "id": i.id, "title": i.title, "username": name })
                    })
                })
                .collect();
            items.sort_by_key(|v| v["title"].as_str().unwrap_or_default().to_lowercase());
            Outcome::ok(json!({ "items": items }))
        }
        Request::PasskeyCreate {
            rp_id,
            rp_name,
            challenge,
            user_id,
            user_name,
            user_display_name,
            exclude,
            ..
        } => {
            if too_long(&[rp_id, rp_name, user_id, user_name, user_display_name]) {
                return Outcome::err("bad_request", "保存できない内容です");
            }
            // サイトが「登録済みなら作らない」と指定したパスキーを、すでに持っている
            let exists = s.vault.items().any(|i| {
                i.passkey.as_ref().is_some_and(|p| {
                    p.rp_id == *rp_id && exclude.iter().any(|e| same_b64(e, &p.credential_id))
                })
            });
            if exists {
                return Outcome::err("exists", "このサイトのパスキーは、すでに保存されています");
            }
            let user = passkey::NewUser {
                handle: user_id,
                name: user_name,
                display_name: user_display_name,
            };
            let (pk, reg) = match passkey::create(rp_id, &origin, challenge, &user, now_ms()) {
                Ok(v) => v,
                Err(e) => return Outcome::err("bad_request", &e.to_string()),
            };
            // 同じサイト・同じユーザー名の項目があれば、そこに付ける。なければ新しい項目にする
            let existing = s
                .vault
                .items()
                .find(|i| {
                    i.passkey.is_none()
                        && !user_name.is_empty()
                        && i.username == *user_name
                        && matches(i, &origin)
                })
                .map(|i| (i.id.clone(), i.title.clone()));
            let saved = match existing {
                Some((id, title)) => s.set_passkey(&id, Some(pk)).map(|()| (id, title)),
                None => {
                    let bare = origin.host_str().unwrap_or_default();
                    let title = if rp_name.trim().is_empty() {
                        bare.strip_prefix("www.").unwrap_or(bare).to_string()
                    } else {
                        rp_name.trim().to_string()
                    };
                    s.add_passkey_item(
                        title.clone(),
                        origin.origin().ascii_serialization(),
                        user_name.clone(),
                        pk,
                    )
                    .map(|id| (id, title))
                }
            };
            match saved {
                Ok((id, title)) => {
                    // 利用者がこのページで作ったものなので、このサイトでの利用は確認済みとする
                    guard.approve(&id, &origin);
                    guard.record("passkey_created", &host, &title);
                    let mut o = Outcome::ok(json!({
                        "id": id,
                        "credential_id": reg.credential_id,
                        "client_data_json": reg.client_data_json,
                        "attestation_object": reg.attestation_object,
                        "authenticator_data": reg.authenticator_data,
                        "public_key": reg.public_key,
                    }));
                    o.activity = true;
                    o.changed = true;
                    o
                }
                Err(e) => Outcome::err("failed", &e.to_string()),
            }
        }
        Request::PasskeyGet {
            rp_id,
            id,
            challenge,
            ..
        } => {
            if let Some(blocked) = burst_check(guard, &host) {
                return blocked;
            }
            let Some((item, pk)) = s
                .get(id)
                .and_then(|i| i.passkey.as_ref().map(|p| (i, p)))
                .filter(|(_, p)| p.rp_id == *rp_id && rp_id_allowed(rp_id, &origin))
            else {
                // 作ったときと違うサイトには署名しない（偽サイト対策）
                return Outcome::err("no_match", "このサイトで使えるパスキーがありません");
            };
            if let Some(ask) = first_use_check(guard, item, &origin, &host, true) {
                return ask;
            }
            match passkey::assert(pk, rp_id, &origin, challenge) {
                Ok(a) => {
                    guard.record("passkey_used", &host, &item.title);
                    let mut o = Outcome::ok(json!({
                        "credential_id": a.credential_id,
                        "client_data_json": a.client_data_json,
                        "authenticator_data": a.authenticator_data,
                        "signature": a.signature,
                        "user_handle": a.user_handle,
                    }));
                    o.activity = true;
                    o
                }
                Err(e) => Outcome::err("bad_request", &e.to_string()),
            }
        }
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
            let bare = origin.host_str().unwrap_or_default().to_string();
            let title = if title.trim().is_empty() {
                bare.strip_prefix("www.").unwrap_or(&bare).to_string()
            } else {
                title.trim().to_string()
            };
            let url = origin.origin().ascii_serialization();
            match s.add(ItemInput {
                title: title.clone(),
                urls: vec![url],
                username: username.clone(),
                password: Some(Zeroizing::new(password.clone())),
                generate_length: 24,
                notes: String::new(),
            }) {
                Ok(id) => {
                    // 利用者がこのページで入力して保存したものなので、このサイトでの利用は確認済みとする
                    guard.approve(&id, &origin);
                    guard.record("saved", &host, &title);
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
                    guard.record("updated", &host, &old.title);
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

fn items_is_empty(out: &Value) -> bool {
    out["items"].as_array().is_none_or(Vec::is_empty)
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
        let mut g = Guard::in_memory();
        let mut v = |r: &str| handle(serde_json::from_str(r).unwrap(), None, &mut g).response;
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

    #[test]
    fn guard_asks_once_per_item_and_site() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approved-sites.json");
        let site = page_origin("https://example.com").unwrap();
        let other = page_origin("https://login.example.com").unwrap();
        let mut g = Guard { path: Some(path.clone()), ..Guard::in_memory() };
        assert!(!g.is_approved("item-1", &site));
        g.pending = Some(PendingApproval {
            item_id: "item-1".into(),
            title: "Example".into(),
            username: "alice".into(),
            origin: "https://example.com".into(),
            host: "example.com".into(),
            passkey: false,
            at: Some(Instant::now()),
        });
        assert_eq!(g.resolve(true).unwrap().title, "Example");
        assert!(g.pending().is_none());
        assert!(g.is_approved("item-1", &site));
        // 許可は「項目とサイトの組み合わせ」ごと
        assert!(!g.is_approved("item-1", &other));
        assert!(!g.is_approved("item-2", &site));
        // 保存先には、サイト名も項目 ID も平文では残さない
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("example") && !saved.contains("item-1"));
        let approved = serde_json::from_str::<Vec<String>>(&saved).unwrap();
        let mut again = Guard { path: Some(path), approved: approved.into_iter().collect(), ..Guard::in_memory() };
        assert!(again.is_approved("item-1", &site));
        again.forget_approvals().unwrap();
        assert!(!again.is_approved("item-1", &site));
    }

    #[test]
    fn guard_denial_and_expiry_do_not_approve() {
        let site = page_origin("https://example.com").unwrap();
        let pending = |at| PendingApproval {
            item_id: "item-1".into(),
            title: "Example".into(),
            username: String::new(),
            origin: "https://example.com".into(),
            host: "example.com".into(),
            passkey: false,
            at,
        };
        let mut g = Guard::in_memory();
        g.pending = Some(pending(Some(Instant::now())));
        g.resolve(false);
        assert!(!g.is_approved("item-1", &site));
        assert_eq!(g.log()[0].event, "denied");
        // 期限が切れた確認には答えられない
        if let Some(old) = Instant::now().checked_sub(APPROVAL_TTL + Duration::from_secs(1)) {
            g.pending = Some(pending(Some(old)));
            assert!(g.resolve(true).is_none());
            assert!(!g.is_approved("item-1", &site));
        }
    }

    #[test]
    fn guard_counts_bursts_of_password_requests() {
        let mut g = Guard::in_memory();
        for _ in 0..MAX_FILLS_PER_WINDOW {
            assert!(!g.too_many_fills());
        }
        assert!(g.too_many_fills());
        g.reset();
        assert!(!g.too_many_fills());
    }
}
