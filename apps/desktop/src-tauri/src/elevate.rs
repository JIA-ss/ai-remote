//! Runs the bundled `ai-remote` command with administrator rights, after the system's own prompt:
//! macOS's administrator password dialog (`osascript … with administrator privileges`), Windows'
//! UAC consent (`Start-Process -Verb RunAs`). Used for what only an administrator may do: installing
//! or removing the host in its service form (the macOS system LaunchAgent that serves the login
//! window, the Windows service, the Windows logon task with highest privileges and its firewall
//! rules). The person at this computer approves every run in the system's dialog; nothing is
//! remembered.

use std::path::Path;

use anyhow::{Context, Result, anyhow};

/// The person dismissed the system's prompt.
pub const CANCELLED: &str = "cancelled";

/// `'it'\''s'`: one argument for /bin/sh.
#[cfg(any(target_os = "macos", test))]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A string literal for AppleScript.
#[cfg(any(target_os = "macos", test))]
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

/// The AppleScript that runs `program args…` as root after the administrator prompt, stderr folded
/// into the output it returns.
#[cfg(any(target_os = "macos", test))]
fn mac_script(program: &Path, args: &[String], prompt: &str) -> String {
    let mut line = sh_quote(&program.to_string_lossy());
    for a in args {
        line.push(' ');
        line.push_str(&sh_quote(a));
    }
    line.push_str(" 2>&1");
    format!(
        "do shell script {} with prompt {} with administrator privileges",
        applescript_string(&line),
        applescript_string(prompt)
    )
}

/// One argument for cmd.exe's command line (paths on Windows never contain `"`).
#[cfg(any(windows, test))]
fn cmd_quote(s: &str) -> String {
    if !s.is_empty() && !s.contains([' ', '\t', '&', '(', ')', '^', '|', '<', '>']) {
        s.to_string()
    } else {
        format!("\"{s}\"")
    }
}

/// The PowerShell that runs `program args…` elevated through UAC, waits, writes its output to
/// `out` and exits with its exit code.
#[cfg(any(windows, test))]
fn windows_script(program: &Path, args: &[String], out: &Path) -> String {
    let mut line = cmd_quote(&program.to_string_lossy());
    for a in args {
        line.push(' ');
        line.push_str(&cmd_quote(a));
    }
    // cmd /c ""prog" args > "out" 2>&1": cmd drops the outer quotes.
    let inner = format!("/c \"{line} > {} 2>&1\"", cmd_quote(&out.to_string_lossy()));
    let ps = |s: &str| format!("'{}'", s.replace('\'', "''"));
    format!(
        "$p = Start-Process -FilePath 'cmd.exe' -ArgumentList {} -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode",
        ps(&inner)
    )
}

/// Runs `program args…` as an administrator after the system's prompt; its output (stdout and
/// stderr). `Err(CANCELLED)` when the person declined.
pub async fn run(program: &Path, args: &[String], prompt: &str) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        let out = tokio::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(mac_script(program, args, prompt))
            .output()
            .await
            .context("run osascript")?;
        let text = String::from_utf8_lossy(&out.stdout).replace('\r', "\n");
        if out.status.success() {
            return Ok(text);
        }
        let err = String::from_utf8_lossy(&out.stderr).replace('\r', "\n");
        // -128: "User canceled."
        if err.contains("(-128)") {
            return Err(anyhow!(CANCELLED));
        }
        // `do shell script` reports a failing command as "<its output> (<exit code>)".
        let err = err.trim().trim_start_matches(|c: char| c.is_ascii_digit() || c == ':').trim();
        Err(anyhow!("{}", err.strip_prefix("execution error:").unwrap_or(err).trim()))
    }
    #[cfg(windows)]
    {
        let _ = prompt; // UAC names the program itself.
        let out = std::env::temp_dir().join(format!("ai-remote-elevated-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &windows_script(program, args, &out)]);
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let status = cmd.output().await.context("run powershell")?;
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        let _ = std::fs::remove_file(&out);
        if status.status.success() {
            return Ok(text);
        }
        let err = String::from_utf8_lossy(&status.stderr);
        if err.contains("canceled by the user")
            || err.contains("cancelled by the user")
            || (text.is_empty() && err.contains("InvalidOperation"))
        {
            return Err(anyhow!(CANCELLED));
        }
        let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(err.trim());
        Err(anyhow!("{}", last.trim_start_matches("Error: ")))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (program, args, prompt);
        Err(anyhow!("running as an administrator is available on macOS and Windows"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mac_command_is_quoted_for_the_shell_and_for_applescript() {
        let s = mac_script(
            Path::new("/Applications/ai remote.app/Contents/MacOS/ai-remote"),
            &["--home".into(), "/Users/me/it's \"here\"".into(), "host".into()],
            "ai-remote wants \"admin\"",
        );
        assert_eq!(
            s,
            r#"do shell script "'/Applications/ai remote.app/Contents/MacOS/ai-remote' '--home' '/Users/me/it'\\''s \"here\"' 'host' 2>&1" with prompt "ai-remote wants \"admin\"" with administrator privileges"#
        );
    }

    #[test]
    fn the_windows_command_goes_through_cmd_and_powershell_quoting() {
        let s = windows_script(
            Path::new(r"C:\Program Files\ai-remote\ai-remote.exe"),
            &["--home".into(), r"C:\Users\o'neil\ai".into(), "host".into(), "install".into()],
            Path::new(r"C:\Temp\out.txt"),
        );
        assert_eq!(
            s,
            r#"$p = Start-Process -FilePath 'cmd.exe' -ArgumentList '/c ""C:\Program Files\ai-remote\ai-remote.exe" --home C:\Users\o''neil\ai host install > C:\Temp\out.txt 2>&1"' -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode"#
        );
    }
}
