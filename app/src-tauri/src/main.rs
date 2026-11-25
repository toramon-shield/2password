//! 2password デスクトップアプリ（Tauri 2）
//!
//! - 暗号処理・保存はすべて vault-client / vault-core（CLI と同じコード）
//! - 画面（WebView）には、利用者が「表示」を押したときだけパスワードを渡す
//! - 無操作が続くと自動ロックし、鍵とパスワードをメモリから消す
//! - ブラウザ拡張からは Native Messaging 経由で呼ばれる（`vault_client::bridge`）。
//!   ブラウザが `chrome-extension://…` を引数にしてこの実行ファイルを起動したときは、
//!   画面を出さずに中継役（ホスト）として動く
//! - PC へのログイン時の自動起動（`--autostart`）では、画面を出さずに通知領域に常駐し、
//!   Windows Hello / Touch ID で解除する（`vault_client::os_unlock`）
#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use vault_client::config::{self, Backend, Config};
use vault_client::i18n::tr;
use vault_client::{ItemInput, Session, StoreChoice, bridge, drive, os_unlock, startup};
use zeroize::Zeroizing;

const AUTO_LOCK: Duration = Duration::from_secs(5 * 60);
const CLIPBOARD_CLEAR_SECS: u64 = 30;

struct AppState {
    session: Mutex<Option<Session>>,
    last_activity: Mutex<Instant>,
    /// このプロセスで作成した Emergency Kit のパス（open_kit はこれしか開かない）
    kit_path: Mutex<Option<std::path::PathBuf>>,
    /// ブラウザ拡張からの要求の見張り（初回の確認・連続要求・履歴）
    guard: Mutex<bridge::Guard>,
    /// Windows Hello / Touch ID の確認画面を出している（重ねて出さない）
    os_busy: AtomicBool,
}

type St = Arc<AppState>;
type Res<T> = Result<T, String>;

impl AppState {
    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }
    fn lock(&self) {
        // Session を drop すると Vault Key・AUK・平文が zeroize される
        self.session.lock().unwrap().take();
        self.guard.lock().unwrap().reset();
    }
}

/// 重い処理（Argon2id・通信・ブラウザ待ち）を UI スレッドの外で実行する
async fn run<T: Send + 'static>(f: impl FnOnce() -> Res<T> + Send + 'static) -> Res<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn with_session<T>(st: &St, f: impl FnOnce(&mut Session) -> Res<T>) -> Res<T> {
    st.touch();
    let mut g = st.session.lock().unwrap();
    let s = g.as_mut().ok_or("ロックされています")?;
    f(s)
}

// ---------------- 画面に渡すデータ（パスワードは含めない） ----------------

#[derive(Serialize)]
struct AppInfo {
    configured: bool,
    unlocked: bool,
    backend: Option<Backend>,
    location: Option<String>,
    oauth_ready: bool,
    detected_drive_folder: Option<String>,
    version: &'static str,
    /// Windows Hello / Touch ID での解除を、この端末で有効にしてあるか
    os_unlock: bool,
    /// "windows-hello" / "touch-id" / "none"
    os_method: &'static str,
}

#[derive(Serialize)]
struct ItemSummary {
    id: String,
    title: String,
    username: String,
    urls: Vec<String>,
    updated: String,
    has_passkey: bool,
    /// パスキーの一覧に出す情報（秘密鍵は含めない）
    passkey: Option<PasskeyInfo>,
}

/// 画面に出すパスキーの情報（秘密鍵は含めない）
#[derive(Serialize)]
struct PasskeyInfo {
    rp_id: String,
    user_name: String,
    user_display_name: String,
    created: String,
    /// 資格情報 ID の先頭（サイトの設定画面の一覧と見比べる手がかり）
    credential_short: String,
}

fn passkey_info(p: &vault_core::Passkey) -> PasskeyInfo {
    PasskeyInfo {
        rp_id: p.rp_id.clone(),
        user_name: p.user_name.clone(),
        user_display_name: p.user_display_name.clone(),
        created: vault_client::fmt_time(p.created_at),
        credential_short: p.credential_id.chars().take(12).collect(),
    }
}

#[derive(Serialize)]
struct ItemDetail {
    id: String,
    title: String,
    username: String,
    urls: Vec<String>,
    notes: String,
    updated: String,
    password_len: usize,
    passkey: Option<PasskeyInfo>,
    has_totp: bool,
}

/// いまのワンタイムパスワード
#[derive(Serialize)]
struct TotpOut {
    code: String,
    remaining: u64,
    period: u64,
}

#[derive(Serialize)]
struct CreateResult {
    secret_key: String,
    location: String,
    kit_path: String,
}

#[derive(Serialize)]
struct StatusOut {
    home: String,
    backend: Backend,
    location: String,
    aid: String,
    known_counter: u64,
    files: Vec<(String, Option<u64>)>,
    sample_hex: Option<String>,
    oauth_ready: bool,
}

#[derive(Deserialize)]
struct ItemForm {
    title: String,
    username: String,
    urls: Vec<String>,
    /// 空なら（新規）自動生成／（編集）変更なし
    password: String,
    notes: String,
}

fn choice(mode: &str, folder: Option<String>) -> StoreChoice {
    if mode == "drive" {
        StoreChoice::Drive
    } else {
        StoreChoice::Folder(folder.filter(|f| !f.trim().is_empty()).map(Into::into))
    }
}

// ---------------- コマンド ----------------

#[tauri::command]
fn app_info(st: State<'_, St>) -> AppInfo {
    let cfg = Config::load().ok();
    AppInfo {
        configured: cfg.is_some(),
        unlocked: st.session.lock().unwrap().is_some(),
        backend: cfg.as_ref().map(|c| c.backend),
        location: cfg.as_ref().map(|c| c.store.display().to_string()),
        oauth_ready: drive::client_available(),
        detected_drive_folder: config::detect_drive_folder()
            .map(|d| d.join("2password").display().to_string()),
        version: env!("CARGO_PKG_VERSION"),
        os_unlock: os_unlock::is_enabled(),
        os_method: os_unlock::method(),
    }
}

/// macOS の Touch ID の画面に出す説明
fn os_reason() -> String {
    tr("2password のロックを解除します")
}

// ---------------- Windows Hello / Touch ID ----------------

/// メインの画面を前面に出す
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// 本人確認で解除し、セッションを置く。返り値は利用者に伝える注意。
fn os_unlock_now(st: &St) -> Res<Vec<String>> {
    if st.session.lock().unwrap().is_some() {
        return Ok(vec![]);
    }
    if st.os_busy.swap(true, Ordering::SeqCst) {
        return Err("本人確認の画面がすでに開いています".into());
    }
    let r = os_unlock::unlock(&os_reason());
    st.os_busy.store(false, Ordering::SeqCst);
    let mut s = r.map_err(err)?;
    let notices = std::mem::take(&mut s.notices);
    let mut g = st.session.lock().unwrap();
    // 確認を待つあいだにパスワードで解除されていたら、そちらを使う
    if g.is_none() {
        *g = Some(s);
    }
    drop(g);
    st.touch();
    Ok(notices)
}

/// 画面の外から（起動時・ブラウザ拡張から）本人確認での解除を始める。
/// 使えないとき・失敗したときは、`show_on_fail` なら画面を出してパスワードでの解除に任せる。
fn start_os_unlock(app: &AppHandle, st: &St, show_on_fail: bool) {
    if st.session.lock().unwrap().is_some() || !os_unlock::is_enabled() {
        if show_on_fail {
            show_main(app);
        }
        return;
    }
    if st.os_busy.load(Ordering::SeqCst) {
        return;
    }
    let (app, st) = (app.clone(), st.clone());
    std::thread::spawn(move || {
        let _ = app.emit("os-unlock", "start");
        match os_unlock_now(&st) {
            Ok(notices) => {
                let _ = app.emit("unlocked", notices);
            }
            Err(e) => {
                let _ = app.emit("os-unlock-failed", e);
                if show_on_fail {
                    show_main(&app);
                }
            }
        }
    });
}

#[tauri::command]
fn password_strength(password: String) -> u8 {
    let p = Zeroizing::new(password);
    vault_client::password_score(&p)
}

#[tauri::command]
async fn create_account(
    app: tauri::AppHandle,
    st: State<'_, St>,
    mode: String,
    folder: Option<String>,
    password: String,
    password2: String,
    kit: Option<vault_client::kit::KitText>,
) -> Res<CreateResult> {
    let p1 = SecretString::from(password);
    let p2 = SecretString::from(password2);
    let docs = app
        .path()
        .document_dir()
        .or_else(|_| app.path().home_dir())
        .map_err(err)?;
    let st = st.inner().clone();
    run(move || {
        vault_client::check_new_password(&p1, &p2, false).map_err(err)?;
        let kit_path = docs.join("2password-emergency-kit.html");
        let r = match &kit {
            Some(text) => {
                vault_client::init_account_with_kit(choice(&mode, folder), &p1, &kit_path, text)
            }
            None => vault_client::init_account(choice(&mode, folder), &p1, &kit_path),
        }
        .map_err(err)?;
        // 画面側にパスワードを持たせないよう、ここで解除しておく
        *st.kit_path.lock().unwrap() = Some(kit_path.clone());
        *st.session.lock().unwrap() = Some(Session::unlock(&p1).map_err(err)?);
        st.touch();
        Ok(CreateResult {
            secret_key: r.secret_key.to_string(),
            location: r.location,
            kit_path: kit_path.display().to_string(),
        })
    })
    .await
}

#[tauri::command]
async fn join_account(
    st: State<'_, St>,
    mode: String,
    folder: Option<String>,
    secret_key: String,
    password: String,
) -> Res<Vec<String>> {
    let st = st.inner().clone();
    let sk = SecretString::from(secret_key);
    let pw = SecretString::from(password);
    run(move || {
        let r = vault_client::join_account(choice(&mode, folder), &sk, &pw).map_err(err)?;
        let mut s = Session::unlock(&pw).map_err(err)?;
        let mut notices = r.notices;
        notices.append(&mut s.notices);
        *st.session.lock().unwrap() = Some(s);
        st.touch();
        Ok(notices)
    })
    .await
}

#[tauri::command]
async fn unlock(st: State<'_, St>, password: String) -> Res<Vec<String>> {
    let st = st.inner().clone();
    let pw = SecretString::from(password);
    run(move || {
        let mut s = Session::unlock(&pw).map_err(err)?;
        let notices = std::mem::take(&mut s.notices);
        *st.session.lock().unwrap() = Some(s);
        st.touch();
        Ok(notices)
    })
    .await
}

#[tauri::command]
fn lock(st: State<'_, St>) {
    st.lock();
}

#[tauri::command]
fn list_items(st: State<'_, St>) -> Res<Vec<ItemSummary>> {
    with_session(&st, |s| {
        let mut v: Vec<_> = s
            .vault
            .items()
            .map(|i| ItemSummary {
                id: i.id.clone(),
                title: i.title.clone(),
                username: i.username.clone(),
                urls: i.urls.clone(),
                updated: vault_client::fmt_time(i.updated_at),
                has_passkey: i.passkey.is_some(),
                passkey: i.passkey.as_ref().map(passkey_info),
            })
            .collect();
        v.sort_by_key(|a| a.title.to_lowercase());
        Ok(v)
    })
}

#[tauri::command]
fn get_item(st: State<'_, St>, id: String) -> Res<ItemDetail> {
    with_session(&st, |s| {
        let i = s.get(&id).ok_or("項目が見つかりません")?;
        Ok(ItemDetail {
            id: i.id.clone(),
            title: i.title.clone(),
            username: i.username.clone(),
            urls: i.urls.clone(),
            notes: i.notes.clone(),
            updated: vault_client::fmt_time(i.updated_at),
            password_len: i.password.chars().count(),
            passkey: i.passkey.as_ref().map(passkey_info),
            has_totp: !i.totp.is_empty(),
        })
    })
}

/// 項目のいまのワンタイムパスワード（2 段階認証のコード）
#[tauri::command]
fn totp_code(st: State<'_, St>, id: String) -> Res<TotpOut> {
    with_session(&st, |s| {
        let i = s.get(&id).ok_or("項目が見つかりません")?;
        if i.totp.is_empty() {
            return Err("ワンタイムパスワードが登録されていません".into());
        }
        let c = vault_client::totp::current(&i.totp).map_err(err)?;
        Ok(TotpOut {
            code: c.code,
            remaining: c.remaining,
            period: c.period,
        })
    })
}

/// 項目にワンタイムパスワードの設定を登録する（空なら外す）
#[tauri::command]
async fn set_totp(st: State<'_, St>, id: String, value: String) -> Res<()> {
    let st = st.inner().clone();
    let value = Zeroizing::new(value);
    run(move || with_session(&st, |s| s.set_totp(&id, Some(value.as_str())).map_err(err))).await
}

/// 項目からパスキーを外す（サイト側の登録は残るので、サイトの設定でも削除する）
#[tauri::command]
async fn remove_passkey(st: State<'_, St>, id: String) -> Res<()> {
    let st = st.inner().clone();
    run(move || with_session(&st, |s| s.set_passkey(&id, None).map_err(err))).await
}

/// 利用者が「表示」を押したときだけ、パスワードを画面に渡す
#[tauri::command]
fn reveal_password(st: State<'_, St>, id: String) -> Res<String> {
    with_session(&st, |s| {
        Ok(s.get(&id).ok_or("項目が見つかりません")?.password.clone())
    })
}

/// パスワードやユーザー名をコピーし、30秒後に消す（画面には渡さない）
#[tauri::command]
fn copy_field(st: State<'_, St>, id: String, field: String) -> Res<u64> {
    with_session(&st, |s| {
        let i = s.get(&id).ok_or("項目が見つかりません")?;
        let v = match field.as_str() {
            "password" => i.password.clone(),
            "username" => i.username.clone(),
            "totp" if !i.totp.is_empty() => vault_client::totp::current(&i.totp).map_err(err)?.code,
            _ => return Err("不明な項目です".into()),
        };
        vault_client::copy_and_clear_later(Zeroizing::new(v), CLIPBOARD_CLEAR_SECS).map_err(err)?;
        Ok(CLIPBOARD_CLEAR_SECS)
    })
}

#[tauri::command]
async fn save_item(st: State<'_, St>, id: Option<String>, form: ItemForm) -> Res<String> {
    let st = st.inner().clone();
    run(move || {
        with_session(&st, |s| {
            let input = ItemInput {
                title: form.title.trim().to_string(),
                urls: form
                    .urls
                    .into_iter()
                    .map(|u| u.trim().to_string())
                    .filter(|u| !u.is_empty())
                    .collect(),
                username: form.username,
                password: Some(Zeroizing::new(form.password)),
                generate_length: 24,
                notes: form.notes,
            };
            if input.title.is_empty() {
                return Err("タイトルを入力してください".into());
            }
            match id {
                Some(id) => {
                    s.update(&id, input).map_err(err)?;
                    Ok(id)
                }
                None => s.add(input).map_err(err),
            }
        })
    })
    .await
}

#[tauri::command]
async fn delete_item(st: State<'_, St>, id: String) -> Res<()> {
    let st = st.inner().clone();
    run(move || with_session(&st, |s| s.delete(&id).map_err(err))).await
}

#[tauri::command]
fn generate_password(length: usize) -> Res<String> {
    Ok(vault_core::generator::generate(length)
        .map_err(err)?
        .to_string())
}

#[tauri::command]
async fn sync_now(st: State<'_, St>) -> Res<Vec<String>> {
    let st = st.inner().clone();
    run(move || {
        with_session(&st, |s| {
            let n = s.reload().map_err(err)?;
            let mut out = std::mem::take(&mut s.notices);
            out.push(format!(
                "同期しました（取り込んだ変更 {n} 件、番号 {}）",
                s.vault.counter
            ));
            Ok(out)
        })
    })
    .await
}

#[tauri::command]
async fn change_password(
    st: State<'_, St>,
    current: String,
    password: String,
    password2: String,
) -> Res<Vec<String>> {
    let st = st.inner().clone();
    let cur = SecretString::from(current);
    let p1 = SecretString::from(password);
    let p2 = SecretString::from(password2);
    run(move || {
        vault_client::check_new_password(&p1, &p2, false).map_err(err)?;
        with_session(&st, |s| s.change_password(&cur, &p1).map_err(err))?;
        // Windows Hello / Touch ID で解除する設定なら、新しいパスワードで包み直す（確認の画面が出る）
        let mut notices = vec![];
        if os_unlock::is_enabled()
            && let Err(e) = os_unlock::rewrap(&p1, &os_reason())
        {
            os_unlock::disable();
            notices.push(format!(
                "Windows Hello / Touch ID での解除を外しました（{e}）。設定画面から、もう一度有効にしてください"
            ));
        }
        Ok(notices)
    })
    .await
}

#[tauri::command]
async fn status(st: State<'_, St>) -> Res<StatusOut> {
    st.touch();
    run(move || {
        let s = vault_client::status().map_err(err)?;
        Ok(StatusOut {
            home: s.home,
            backend: s.backend,
            location: s.location,
            aid: s.aid,
            known_counter: s.known_counter,
            files: s.files,
            sample_hex: s.sample_hex,
            oauth_ready: drive::client_available(),
        })
    })
    .await
}

#[tauri::command]
async fn google_login() -> Res<()> {
    run(|| drive::login().map_err(err)).await
}

#[tauri::command]
async fn google_logout() -> Res<()> {
    run(|| drive::logout().map_err(err)).await
}

#[tauri::command]
async fn migrate_to_drive(st: State<'_, St>) -> Res<String> {
    let st = st.inner().clone();
    run(move || {
        with_session(&st, |s| {
            let info = s.migrate_to_drive().map_err(err)?;
            Ok(info.location)
        })
    })
    .await
}

#[tauri::command]
fn forget_device(st: State<'_, St>) -> Res<()> {
    st.lock();
    vault_client::forget().map_err(err)
}

/// 項目の URL を OS 標準のブラウザで開く
#[tauri::command]
fn open_url(url: String) -> Res<()> {
    let url = vault_client::links::safe_web_url(&url)
        .ok_or("開けない種類のリンクです（http/https のみ）")?;
    drive::open_browser(&url);
    Ok(())
}

/// このアプリが作成した Emergency Kit だけを開く（画面からパスは受け取らない）
#[tauri::command]
fn open_kit(st: State<'_, St>) -> Res<()> {
    let path = st
        .kit_path
        .lock()
        .unwrap()
        .clone()
        .ok_or("Emergency Kit はこの起動中に作成されていません")?;
    if !path.is_file() {
        return Err("Emergency Kit が見つかりません（削除済みの可能性があります）".into());
    }
    drive::open_browser(&path.display().to_string());
    Ok(())
}

#[tauri::command]
fn touch(st: State<'_, St>) {
    st.touch();
}

// ---------------- Windows Hello / Touch ID・自動起動 ----------------

#[tauri::command]
fn os_unlock_status() -> os_unlock::Status {
    os_unlock::status()
}

/// 有効にする。アカウントパスワードを確かめてから、本人確認の画面を出す
#[tauri::command]
async fn os_unlock_enable(st: State<'_, St>, password: String) -> Res<()> {
    let st = st.inner().clone();
    let pw = SecretString::from(password);
    run(move || {
        let aid = with_session(&st, |s| {
            s.verify_password(&pw).map_err(err)?;
            Ok(s.cfg.aid.clone())
        })?;
        os_unlock::enable(&aid, &pw, &os_reason()).map_err(err)
    })
    .await
}

#[tauri::command]
fn os_unlock_disable() {
    os_unlock::disable();
}

#[tauri::command]
fn os_unlock_set_at_start(on: bool) -> Res<()> {
    os_unlock::set_unlock_at_start(on).map_err(err)
}

/// ロック画面の「Windows Hello / Touch ID で解除」
#[tauri::command]
async fn unlock_with_os(st: State<'_, St>) -> Res<Vec<String>> {
    let st = st.inner().clone();
    run(move || os_unlock_now(&st)).await
}

#[derive(Serialize)]
struct StartupStatus {
    /// PC へのログイン時に自動で起動する
    enabled: bool,
    /// ブラウザと接続済み（Native Messaging の登録がいまのアプリを指している）
    browser: bool,
}

#[tauri::command]
fn startup_status() -> StartupStatus {
    StartupStatus {
        enabled: startup::enabled(),
        browser: bridge::native_host_installed(),
    }
}

/// 自動起動を切り替える。オンにするときは、ブラウザとの接続もまとめて行う。
/// 返り値は接続したブラウザの名前（接続済み・見つからないときは空）。
#[tauri::command]
async fn startup_set(on: bool) -> Res<Vec<String>> {
    run(move || {
        if !on {
            startup::disable().map_err(err)?;
            return Ok(vec![]);
        }
        startup::enable().map_err(err)?;
        if bridge::native_host_installed() {
            return Ok(vec![]);
        }
        // ブラウザが見つからなくても、自動起動の登録は済んでいるので失敗にしない
        Ok(bridge::install_native_host().unwrap_or_default())
    })
    .await
}

// ---------------- ブラウザ拡張・取り込み ----------------

#[derive(Serialize)]
struct BrowserStatus {
    installed: bool,
    dev_extension_id: &'static str,
    extra_extension_ids: Vec<String>,
    /// 確認済みの「項目とサイト」の組み合わせの数
    approved_sites: usize,
    /// 入力のたびに確認する（AI エージェントにブラウザを操作させるとき向け）
    confirm_each: bool,
}

#[tauri::command]
fn browser_status(st: State<'_, St>) -> BrowserStatus {
    let guard = st.guard.lock().unwrap();
    BrowserStatus {
        installed: bridge::native_host_installed(),
        dev_extension_id: bridge::DEV_EXTENSION_ID,
        extra_extension_ids: bridge::extra_extension_ids(),
        approved_sites: guard.approved_count(),
        confirm_each: guard.confirm_each(),
    }
}

/// 「入力のたびに確認する」を切り替える
#[tauri::command]
fn browser_set_confirm_each(st: State<'_, St>, on: bool) -> Res<()> {
    st.guard
        .lock()
        .unwrap()
        .set_confirm_each(on, true)
        .map_err(err)
}

/// 利用者の確認を待っている、ブラウザ拡張からの要求
#[tauri::command]
fn browser_pending(st: State<'_, St>) -> Option<bridge::PendingApproval> {
    if st.session.lock().unwrap().is_none() {
        return None;
    }
    st.guard.lock().unwrap().pending()
}

/// 確認への答え。許可すると、その項目をそのサイトで使えるようになる
#[tauri::command]
fn browser_approve(st: State<'_, St>, allow: bool, item_id: String, origin: String) -> Res<()> {
    if st.session.lock().unwrap().is_none() {
        return Err("ロックされています".into());
    }
    st.touch();
    let mut guard = st.guard.lock().unwrap();
    // 画面に出した確認と、いま待っている要求が同じものであること（表示中に差し替えられた要求を許可しない）
    let same = guard
        .pending()
        .is_some_and(|p| p.item_id == item_id && p.origin == origin);
    if !same {
        return Err("確認の期限が切れました。ブラウザでもう一度選んでください".into());
    }
    guard.resolve(allow);
    Ok(())
}

/// ブラウザ拡張からの要求の履歴（新しい順。パスワードは含まない）
#[tauri::command]
fn browser_log(st: State<'_, St>) -> Vec<bridge::LogEntry> {
    if st.session.lock().unwrap().is_none() {
        return vec![];
    }
    st.guard.lock().unwrap().log()
}

/// 確認済みのサイトをすべて取り消す（次に使うとき、もう一度確認する）
#[tauri::command]
fn browser_forget_approvals(st: State<'_, St>) -> Res<()> {
    st.guard.lock().unwrap().forget_approvals().map_err(err)
}

/// ブラウザ拡張がこのアプリを呼び出せるように登録する（いまの利用者の分だけ）
#[tauri::command]
async fn browser_install(extension_id: Option<String>) -> Res<Vec<String>> {
    run(move || {
        if let Some(id) = extension_id.filter(|i| !i.trim().is_empty()) {
            bridge::add_extension_id(&id).map_err(err)?;
        }
        bridge::install_native_host().map_err(err)
    })
    .await
}

#[tauri::command]
async fn browser_uninstall() -> Res<()> {
    run(|| bridge::uninstall_native_host().map_err(err)).await
}

/// 1Password・Chrome などが書き出した CSV の中身を取り込む。返り値は（追加、重複で飛ばした）件数
#[tauri::command]
async fn import_csv(st: State<'_, St>, text: String) -> Res<(usize, usize)> {
    let st = st.inner().clone();
    let text = Zeroizing::new(text);
    run(move || {
        let items = vault_client::import::parse(&text).map_err(err)?;
        with_session(&st, |s| s.import(items).map_err(err))
    })
    .await
}

/// ブラウザ拡張からの要求を受け付ける
fn start_bridge(app: &tauri::AppHandle, st: St) {
    let handle = app.clone();
    let result = bridge::serve(move |req| {
        let out = {
            let mut g = st.session.lock().unwrap();
            let mut guard = st.guard.lock().unwrap();
            bridge::handle(req, g.as_mut(), &mut guard)
        };
        if out.lock {
            st.lock();
            let _ = handle.emit("locked", "browser");
        }
        if out.approval {
            let _ = handle.emit("browser-approval", ());
        }
        if out.activity {
            st.touch();
        }
        if out.changed {
            let _ = handle.emit("vault-changed", ());
        }
        if out.focus {
            // ロック中で Windows Hello / Touch ID が使えるなら、画面を出さずに本人確認だけ求める
            start_os_unlock(&handle, &st, true);
        }
        out.response
    });
    if let Err(e) = result {
        eprintln!("ブラウザ拡張との連絡路を開けませんでした: {e}");
    }
}

/// 通知領域（macOS はメニューバー）のアイコン。画面を閉じても、ここから開ける
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", tr("2password を開く"), true, None::<&str>)?;
    let lock = MenuItem::with_id(app, "lock", tr("ロック"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", tr("終了"), true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &lock, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("2password")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id().as_ref() {
            "open" => show_main(app),
            "lock" => {
                app.state::<St>().lock();
                let _ = app.emit("locked", "manual");
            }
            "quit" => {
                app.state::<St>().lock();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn main() {
    // ブラウザが Native Messaging ホストとして起動した場合は、画面を出さずに中継だけ行う
    if let Some(caller) = std::env::args().find(|a| a.starts_with("chrome-extension://")) {
        let _ = bridge::run_native_host(&caller);
        return;
    }

    let state: St = Arc::new(AppState {
        session: Mutex::new(None),
        last_activity: Mutex::new(Instant::now()),
        kit_path: Mutex::new(None),
        guard: Mutex::new(bridge::Guard::load()),
        os_busy: AtomicBool::new(false),
    });
    let autostart = startup::launched_at_login();
    let watcher = state.clone();
    let bridge_state = state.clone();

    tauri::Builder::default()
        // 2 つ目を起動したときは、新しく開かずに、動いている方の画面を出す
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .manage(state)
        .on_window_event(|window, event| {
            // 自動起動に登録しているときは、閉じても終了せずに通知領域に残す（ブラウザ拡張を使えるように）
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && startup::enabled()
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(move |app| {
            let unlock_state = bridge_state.clone();
            start_bridge(app.handle(), bridge_state);
            setup_tray(app.handle())?;
            if !autostart {
                show_main(app.handle());
            }
            // アプリを更新・移動したあとも、ブラウザとの接続と自動起動がいまのアプリを指すようにする
            std::thread::spawn(|| {
                let _ = bridge::ensure_native_host();
                if startup::stale() {
                    let _ = startup::enable();
                }
            });
            // 起動時に Windows Hello / Touch ID で解除する（画面の読み込みを少し待つ）
            if os_unlock::unlock_at_start() {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(if autostart { 3000 } else { 800 }));
                    start_os_unlock(&handle, &unlock_state, false);
                });
            }
            // 自動ロック：無操作が AUTO_LOCK 続いたら鍵をメモリから消す
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(10));
                    let idle = watcher.last_activity.lock().unwrap().elapsed();
                    let unlocked = watcher.session.lock().unwrap().is_some();
                    if unlocked && idle > AUTO_LOCK {
                        watcher.lock();
                        let _ = handle.emit("locked", "auto");
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            password_strength,
            create_account,
            join_account,
            unlock,
            lock,
            list_items,
            get_item,
            reveal_password,
            copy_field,
            save_item,
            delete_item,
            remove_passkey,
            totp_code,
            set_totp,
            generate_password,
            sync_now,
            change_password,
            status,
            google_login,
            google_logout,
            migrate_to_drive,
            forget_device,
            open_url,
            open_kit,
            touch,
            browser_status,
            browser_install,
            browser_uninstall,
            browser_pending,
            browser_approve,
            browser_log,
            browser_forget_approvals,
            browser_set_confirm_each,
            import_csv,
            os_unlock_status,
            os_unlock_enable,
            os_unlock_disable,
            os_unlock_set_at_start,
            unlock_with_os,
            startup_status,
            startup_set,
        ])
        .run(tauri::generate_context!())
        .expect("2password の起動に失敗しました");
}
