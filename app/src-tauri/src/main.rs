//! 2password デスクトップアプリ（Tauri 2）
//!
//! - 暗号処理・保存はすべて vault-client / vault-core（CLI と同じコード）
//! - 画面（WebView）には、利用者が「表示」を押したときだけパスワードを渡す
//! - 無操作が続くと自動ロックし、鍵とパスワードをメモリから消す
#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, State};
use vault_client::config::{self, Backend, Config};
use vault_client::{ItemInput, Session, StoreChoice, drive};
use zeroize::Zeroizing;

const AUTO_LOCK: Duration = Duration::from_secs(5 * 60);
const CLIPBOARD_CLEAR_SECS: u64 = 30;

struct AppState {
    session: Mutex<Option<Session>>,
    last_activity: Mutex<Instant>,
    /// このプロセスで作成した Emergency Kit のパス（open_kit はこれしか開かない）
    kit_path: Mutex<Option<std::path::PathBuf>>,
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
}

#[derive(Serialize)]
struct ItemSummary {
    id: String,
    title: String,
    username: String,
    urls: Vec<String>,
    updated: String,
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
    }
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
        let r = vault_client::init_account(choice(&mode, folder), &p1, &kit_path).map_err(err)?;
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
        })
    })
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
) -> Res<()> {
    let st = st.inner().clone();
    let cur = SecretString::from(current);
    let p1 = SecretString::from(password);
    let p2 = SecretString::from(password2);
    run(move || {
        vault_client::check_new_password(&p1, &p2, false).map_err(err)?;
        with_session(&st, |s| s.change_password(&cur, &p1).map_err(err))
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

fn main() {
    let state: St = Arc::new(AppState {
        session: Mutex::new(None),
        last_activity: Mutex::new(Instant::now()),
        kit_path: Mutex::new(None),
    });
    let watcher = state.clone();

    tauri::Builder::default()
        .manage(state)
        .setup(move |app| {
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
        ])
        .run(tauri::generate_context!())
        .expect("2password の起動に失敗しました");
}
