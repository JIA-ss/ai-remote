//! "Open at login" for the app itself, so the tray icon is there after a restart. Off until the
//! person turns it on in Settings > This device; it starts the app with `--hidden` (tray only).
//!
//! macOS: a per-user LaunchAgent in `~/Library/LaunchAgents` (listed by System Settings under Login
//! Items, where it can be turned off too). Windows: a value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (listed in Settings > Apps > Startup and
//! Task Manager). Nothing is loaded now: it takes effect at the next login.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

/// The argument that starts the app in the tray, without its window.
pub const HIDDEN_ARG: &str = "--hidden";

#[cfg(target_os = "macos")]
const LABEL: &str = "io.github.jia-ss.ai-remote.app";

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The LaunchAgent that starts `exe --hidden` at login, in the GUI session only.
#[cfg(target_os = "macos")]
fn plist(exe: &std::path::Path) -> String {
    let exe = xml_escape(&exe.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>{HIDDEN_ARG}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#
    )
}

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_VALUE: &str = "ai-remote";

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg")
        .args(args)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .context("run reg.exe")
}

/// Whether the app opens at login.
pub fn enabled() -> Result<bool> {
    #[cfg(target_os = "macos")]
    return Ok(plist_path()?.is_file());
    #[cfg(windows)]
    return Ok(reg(&["query", RUN_KEY, "/v", RUN_VALUE])?.status.success());
    #[cfg(not(any(windows, target_os = "macos")))]
    Ok(false)
}

/// Turns opening at login on (for this copy of the app) or off.
pub fn set(on: bool) -> Result<()> {
    let exe = std::env::current_exe().context("locate this app")?;
    #[cfg(target_os = "macos")]
    {
        let path = plist_path()?;
        if on {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
            }
            std::fs::write(&path, plist(&exe)).with_context(|| format!("write {}", path.display()))?;
        } else if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let out = if on {
            let command = format!("\"{}\" {HIDDEN_ARG}", exe.display());
            reg(&["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &command, "/f"])?
        } else {
            if !enabled()? {
                return Ok(());
            }
            reg(&["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])?
        };
        if !out.status.success() {
            return Err(anyhow!("reg.exe failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (on, exe);
        Err(anyhow!("opening at login is available on macOS and Windows"))
    }
}

/// Whether this process was started by the login item (or asked to start in the tray).
pub fn started_hidden() -> bool {
    std::env::args().skip(1).any(|a| a == HIDDEN_ARG)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn the_launch_agent_runs_this_app_hidden_in_the_gui_session() {
        let xml = plist(std::path::Path::new("/Applications/a & b.app/Contents/MacOS/ai-remote"));
        assert!(xml.contains("<string>/Applications/a &amp; b.app/Contents/MacOS/ai-remote</string>"));
        assert!(xml.contains("<string>--hidden</string>"));
        assert!(xml.contains("<string>Aqua</string>"));
        assert!(xml.contains(LABEL));
    }
}
