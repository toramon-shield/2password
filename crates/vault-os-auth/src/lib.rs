//! OS の本人確認（Windows Hello / Touch ID）を通ったときだけ、端末ごとの「解除用の鍵」を取り出す。
//!
//! - Windows：Windows Hello の鍵（TPM などに保管され、取り出せない RSA 鍵）で、決まった値に署名させる。
//!   署名は本人確認（顔・指紋・PIN）を通ったときだけ作られ、同じ値には毎回同じ署名になるので、
//!   そのハッシュを鍵にする。鍵そのものはどこにも保存しない。
//! - macOS：Touch ID（またはログインパスワード）で本人確認をしてから、キーチェーンに置いた乱数の鍵を読む。
//!
//! この鍵で何を暗号化するか（アカウントパスワードなど）は呼び出し側が決める。
//! OS の API を呼ぶ部分だけ `unsafe` を使うため、ほかのクレート（`forbid(unsafe_code)`）から分けてある。
#![deny(unsafe_op_in_unsafe_fn)]

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// 解除用の鍵（32 バイト）
pub type Key = Zeroizing<[u8; 32]>;

#[derive(Debug)]
pub enum Error {
    /// この端末では使えない（Windows Hello / Touch ID が設定されていない、対応していない OS）
    Unavailable,
    /// 利用者が取り消した
    Canceled,
    /// 鍵が見つからない（OS 側で消された、別の利用者・端末に移った）
    NotFound,
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unavailable => f.write_str("unavailable"),
            Error::Canceled => f.write_str("canceled"),
            Error::NotFound => f.write_str("not_found"),
            Error::Failed(m) => write!(f, "failed: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// 本人確認の種類（画面の表示に使う）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    WindowsHello,
    TouchId,
    None,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::WindowsHello => "windows-hello",
            Method::TouchId => "touch-id",
            Method::None => "none",
        }
    }
}

/// この OS で使う本人確認の種類
pub fn method() -> Method {
    if cfg!(windows) {
        Method::WindowsHello
    } else if cfg!(target_os = "macos") {
        Method::TouchId
    } else {
        Method::None
    }
}

const DOMAIN: &[u8] = b"2password os-auth key v1\0";

fn derive(material: &[u8]) -> Key {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(material);
    Zeroizing::new(h.finalize().into())
}

/// いま使えるか（設定済みか）。本人確認の画面は出さない。
pub fn available() -> bool {
    imp::available()
}

/// 新しく鍵を作る（同じ `label` の鍵があれば作り直す）。本人確認の画面が出る。
/// `salt` は呼び出し側が乱数で作って保存しておき、`open` に同じものを渡す。
pub fn create(label: &str, salt: &[u8], reason: &str) -> Result<Key, Error> {
    imp::create(label, salt, reason)
}

/// 作った鍵を取り出す。本人確認の画面が出る。
pub fn open(label: &str, salt: &[u8], reason: &str) -> Result<Key, Error> {
    imp::open(label, salt, reason)
}

/// 鍵を消す（以後 `open` は NotFound になる）。
pub fn remove(label: &str) {
    imp::remove(label)
}

// ---------------- Windows：Windows Hello（KeyCredentialManager） ----------------

#[cfg(windows)]
mod imp {
    use super::{Error, Key, derive};
    use windows::Security::Credentials::{
        KeyCredential, KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialStatus,
    };
    use windows::Security::Cryptography::CryptographicBuffer;
    use windows_core::HSTRING;

    fn failed(e: windows_core::Error) -> Error {
        Error::Failed(e.message())
    }

    /// WinRT を使う前に、このスレッドを MTA にする（すでに初期化済みなら何もしない）
    fn init_thread() {
        use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
        // SAFETY: 引数は定数。失敗（別の方式で初期化済みなど）は無視してよい
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    }

    fn status(s: KeyCredentialStatus) -> Result<(), Error> {
        match s {
            KeyCredentialStatus::Success => Ok(()),
            KeyCredentialStatus::UserCanceled | KeyCredentialStatus::UserPrefersPassword => {
                Err(Error::Canceled)
            }
            KeyCredentialStatus::NotFound => Err(Error::NotFound),
            KeyCredentialStatus::SecurityDeviceLocked => {
                Err(Error::Failed("security device locked".into()))
            }
            _ => Err(Error::Failed(format!("status {}", s.0))),
        }
    }

    /// Windows Hello の確認画面は、デスクトップアプリから呼ぶと後ろに隠れることがあるので、
    /// 出てきたら前面に出す（数秒だけ探す）
    fn bring_prompt_to_front() {
        std::thread::spawn(|| {
            use windows::Win32::UI::WindowsAndMessaging::{
                FindWindowW, SetForegroundWindow,
            };
            use windows_core::w;
            for _ in 0..30 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                // SAFETY: 定数の文字列を渡すだけ。見つからなければ Err が返る
                if let Ok(hwnd) = unsafe { FindWindowW(w!("Credential Dialog Xaml Host"), None) } {
                    // SAFETY: FindWindowW が返した有効なウィンドウ
                    let _ = unsafe { SetForegroundWindow(hwnd) };
                    return;
                }
            }
        });
    }

    fn sign(cred: &KeyCredential, salt: &[u8]) -> Result<Key, Error> {
        let data = CryptographicBuffer::CreateFromByteArray(salt).map_err(failed)?;
        let op = cred.RequestSignAsync(&data).map_err(failed)?;
        bring_prompt_to_front();
        let r = op.join().map_err(failed)?;
        status(r.Status().map_err(failed)?)?;
        let sig = r.Result().map_err(failed)?;
        let mut bytes = windows_core::Array::<u8>::new();
        CryptographicBuffer::CopyToByteArray(&sig, &mut bytes).map_err(failed)?;
        let bytes = Zeroizing::new(bytes.to_vec());
        if bytes.is_empty() {
            return Err(Error::Failed("empty signature".into()));
        }
        Ok(derive(&bytes))
    }

    use zeroize::Zeroizing;

    pub fn available() -> bool {
        init_thread();
        KeyCredentialManager::IsSupportedAsync()
            .and_then(|op| op.join())
            .unwrap_or(false)
    }

    pub fn create(label: &str, salt: &[u8], _reason: &str) -> Result<Key, Error> {
        init_thread();
        if !available() {
            return Err(Error::Unavailable);
        }
        let op = KeyCredentialManager::RequestCreateAsync(
            &HSTRING::from(label),
            KeyCredentialCreationOption::ReplaceExisting,
        )
        .map_err(failed)?;
        bring_prompt_to_front();
        let r = op.join().map_err(failed)?;
        status(r.Status().map_err(failed)?)?;
        let cred = r.Credential().map_err(failed)?;
        sign(&cred, salt)
    }

    pub fn open(label: &str, salt: &[u8], _reason: &str) -> Result<Key, Error> {
        init_thread();
        let r = KeyCredentialManager::OpenAsync(&HSTRING::from(label))
            .map_err(failed)?
            .join()
            .map_err(failed)?;
        status(r.Status().map_err(failed)?)?;
        let cred = r.Credential().map_err(failed)?;
        sign(&cred, salt)
    }

    pub fn remove(label: &str) {
        init_thread();
        if let Ok(op) = KeyCredentialManager::DeleteAsync(&HSTRING::from(label)) {
            let _ = op.join();
        }
    }
}

// ---------------- macOS：Touch ID（LocalAuthentication）＋キーチェーン ----------------

#[cfg(target_os = "macos")]
mod imp {
    use super::{Error, Key, derive};
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;
    use zeroize::Zeroizing;

    const SERVICE: &str = "2password-os-auth";
    /// Touch ID が使えないとき（クラムシェルなど）はログインパスワードでも通す
    const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthentication;

    pub fn available() -> bool {
        // SAFETY: LAContext の生成と問い合わせだけ。画面は出ない
        unsafe { LAContext::new().canEvaluatePolicy_error(POLICY).is_ok() }
    }

    fn verify(reason: &str) -> Result<(), Error> {
        if !available() {
            return Err(Error::Unavailable);
        }
        let (tx, rx) = mpsc::channel::<Result<(), Error>>();
        let tx = Mutex::new(Some(tx));
        let reply = RcBlock::new(move |ok: Bool, err: *mut NSError| {
            let result = if ok.as_bool() {
                Ok(())
            } else {
                // SAFETY: 失敗したときに渡される NSError（null のこともある）
                let code = unsafe { err.as_ref() }.map(|e| e.code()).unwrap_or(0);
                match code {
                    // LAErrorUserCancel / SystemCancel / AppCancel / UserFallback
                    -2 | -4 | -9 | -3 => Err(Error::Canceled),
                    // BiometryNotEnrolled / PasscodeNotSet / BiometryNotAvailable
                    -5 | -6 | -7 => Err(Error::Unavailable),
                    c => Err(Error::Failed(format!("LAError {c}"))),
                }
            };
            if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
                let _ = tx.send(result);
            }
        });
        let ctx = unsafe { LAContext::new() };
        // SAFETY: reply は呼び出しが終わるまで（RcBlock がコピーされて）保持される
        unsafe {
            ctx.evaluatePolicy_localizedReason_reply(POLICY, &NSString::from_str(reason), &reply)
        };
        rx.recv_timeout(Duration::from_secs(180))
            .map_err(|_| Error::Canceled)?
    }

    fn entry(label: &str) -> Result<keyring::Entry, Error> {
        keyring::Entry::new(SERVICE, label).map_err(|e| Error::Failed(e.to_string()))
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn unhex(s: &str) -> Option<Zeroizing<Vec<u8>>> {
        if s.len() % 2 != 0 {
            return None;
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()
            .map(Zeroizing::new)
    }

    pub fn create(label: &str, salt: &[u8], reason: &str) -> Result<Key, Error> {
        verify(reason)?;
        let mut secret = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *secret).map_err(|e| Error::Failed(e.to_string()))?;
        let text = Zeroizing::new(hex(&*secret));
        entry(label)?
            .set_password(&text)
            .map_err(|e| Error::Failed(e.to_string()))?;
        let mut m = Zeroizing::new(secret.to_vec());
        m.extend_from_slice(salt);
        Ok(derive(&m))
    }

    pub fn open(label: &str, salt: &[u8], reason: &str) -> Result<Key, Error> {
        verify(reason)?;
        let text = Zeroizing::new(entry(label)?.get_password().map_err(|e| match e {
            keyring::Error::NoEntry => Error::NotFound,
            e => Error::Failed(e.to_string()),
        })?);
        let mut m = unhex(&text).ok_or(Error::NotFound)?;
        m.extend_from_slice(salt);
        Ok(derive(&m))
    }

    pub fn remove(label: &str) {
        if let Ok(e) = entry(label) {
            let _ = e.delete_credential();
        }
    }
}

// ---------------- そのほかの OS：使えない ----------------

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::{Error, Key};
    pub fn available() -> bool {
        false
    }
    pub fn create(_: &str, _: &[u8], _: &str) -> Result<Key, Error> {
        Err(Error::Unavailable)
    }
    pub fn open(_: &str, _: &[u8], _: &str) -> Result<Key, Error> {
        Err(Error::Unavailable)
    }
    pub fn remove(_: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_is_deterministic_and_domain_separated() {
        assert_eq!(*derive(b"abc"), *derive(b"abc"));
        assert_ne!(*derive(b"abc"), *derive(b"abd"));
        let plain: [u8; 32] = Sha256::digest(b"abc").into();
        assert_ne!(*derive(b"abc"), plain);
    }
}
