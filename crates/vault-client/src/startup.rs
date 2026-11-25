//! PC へのログイン時に、2password を自動で起動する（いまの利用者の分だけ。管理者権限は使わない）。
//!
//! - Windows：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` に登録する
//! - macOS：`~/Library/LaunchAgents/` に LaunchAgent を置く
//! - Linux：`~/.config/autostart/` に .desktop を置く
//!
//! 自動起動のときは引数 `--autostart` を付ける。アプリは画面を出さずに通知領域（メニューバー）に常駐する。
use crate::R;
#[cfg(not(windows))]
use std::path::PathBuf;

/// 自動起動のときに付ける引数
pub const ARG: &str = "--autostart";

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const VALUE: &str = "2password";

/// 登録する起動コマンド
fn command() -> R<String> {
    let exe = std::env::current_exe()?;
    Ok(format!("\"{}\" {ARG}", exe.display()))
}

#[cfg(windows)]
fn reg_output(args: &[&str]) -> R<(bool, String)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("reg")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stderr(std::process::Stdio::null())
        .output()?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

#[cfg(target_os = "macos")]
const AGENT_LABEL: &str = "dev.twopassword.desktop";

#[cfg(not(windows))]
fn entry_path() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    if cfg!(target_os = "macos") {
        #[cfg(target_os = "macos")]
        return Some(
            home.join("Library/LaunchAgents")
                .join(format!("{AGENT_LABEL}.plist")),
        );
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    Some(base.join("autostart").join("2password.desktop"))
}

#[cfg(not(windows))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(not(windows))]
fn entry_body() -> R<String> {
    let exe = std::env::current_exe()?.display().to_string();
    if cfg!(target_os = "macos") {
        Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.twopassword.desktop</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>{ARG}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>ProcessType</key>
  <string>Interactive</string>
</dict>
</plist>
"#,
            xml_escape(&exe)
        ))
    } else {
        Ok(format!(
            "[Desktop Entry]\nType=Application\nName=2password\nExec=\"{}\" {ARG}\nX-GNOME-Autostart-enabled=true\n",
            exe.replace('"', "\\\"")
        ))
    }
}

/// いま動いているアプリが、自動起動に登録されているか（アプリを移動・更新すると false になる）
pub fn enabled() -> bool {
    let Ok(cmd) = command() else { return false };
    #[cfg(windows)]
    {
        reg_output(&["query", RUN_KEY, "/v", VALUE])
            .is_ok_and(|(ok, out)| ok && out.lines().any(|l| l.trim_end().ends_with(&cmd)))
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
        let (Some(path), Ok(body)) = (entry_path(), entry_body()) else {
            return false;
        };
        std::fs::read_to_string(path).is_ok_and(|b| b == body)
    }
}

/// 自動起動に登録されているが、別の場所のアプリを指している（更新・移動のあと）
pub fn stale() -> bool {
    #[cfg(windows)]
    let registered = reg_output(&["query", RUN_KEY, "/v", VALUE]).is_ok_and(|(ok, _)| ok);
    #[cfg(not(windows))]
    let registered = entry_path().is_some_and(|p| p.is_file());
    registered && !enabled()
}

/// 自動起動に登録する（登録済みなら、いま動いているアプリの場所に書き直す）
pub fn enable() -> R<()> {
    #[cfg(windows)]
    {
        let cmd = command()?;
        let (ok, _) = reg_output(&[
            "add", RUN_KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &cmd, "/f",
        ])?;
        if !ok {
            return Err("自動起動に登録できませんでした".into());
        }
    }
    #[cfg(not(windows))]
    {
        let path = entry_path().ok_or("自動起動に登録できませんでした")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::store::write_atomic(&path, entry_body()?.as_bytes())?;
    }
    Ok(())
}

/// 自動起動の登録を消す
pub fn disable() -> R<()> {
    #[cfg(windows)]
    {
        let _ = reg_output(&["delete", RUN_KEY, "/v", VALUE, "/f"]);
    }
    #[cfg(not(windows))]
    if let Some(path) = entry_path() {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// この起動が、ログイン時の自動起動か
pub fn launched_at_login() -> bool {
    std::env::args().any(|a| a == ARG)
}
