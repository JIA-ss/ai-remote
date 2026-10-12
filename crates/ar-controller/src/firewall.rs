//! Whether the system firewall refuses inbound traffic for this program, which keeps hole punching
//! from forming direct (P2P) connections, and the one-step fix on Windows.
//!
//! On Windows the usual cause is an "allow access?" prompt that was dismissed: Windows then adds
//! inbound block rules for the program. The host's installer removes those for the host programs;
//! a controller (the desktop app, `ai-remote agent`) has no installer step, so it checks itself.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};

/// How long a firewall reading is reused: reading the rules takes a COM round trip.
const FRESH: Duration = Duration::from_secs(60);

/// Caches [`blocked`] for [`FRESH`].
#[derive(Default)]
pub(crate) struct FirewallCache(std::sync::Mutex<Option<(Instant, bool)>>);

impl FirewallCache {
    /// Whether inbound traffic for this program is blocked (cached; `false` when unknown).
    pub(crate) fn blocked(&self) -> bool {
        let mut cached = self.0.lock().unwrap();
        if let Some((at, blocked)) = *cached
            && at.elapsed() < FRESH
        {
            return blocked;
        }
        let blocked = blocked().unwrap_or(false);
        *cached = Some((Instant::now(), blocked));
        blocked
    }

    /// Forgets the reading, after a fix.
    pub(crate) fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }
}

/// Whether an enabled inbound rule blocks this program.
#[cfg(windows)]
pub fn blocked() -> Result<bool> {
    use windows_firewall::{Action, Profile, get_firewall_state, list_incoming_rules};
    let exe = std::env::current_exe()?;
    // The firewall's COM API wants a fresh multithreaded apartment, which a reused worker may not offer.
    std::thread::spawn(move || {
        let on = [Profile::Domain, Profile::Private, Profile::Public]
            .into_iter()
            .any(|p| get_firewall_state(p).unwrap_or(false));
        if !on {
            return Ok(false);
        }
        let rules = list_incoming_rules().map_err(|e| anyhow::anyhow!("read firewall rules: {e}"))?;
        Ok(rules.iter().any(|r| {
            *r.enabled()
                && *r.action() == Action::Block
                && r.application_name().as_deref().is_some_and(|p| same_program(p, &exe))
        }))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("reading the firewall rules panicked"))?
}

/// Whether the macOS application firewall blocks every incoming connection, or this program.
#[cfg(target_os = "macos")]
pub fn blocked() -> Result<bool> {
    let fw = |args: &[&str]| -> Result<String> {
        let out = std::process::Command::new("/usr/libexec/ApplicationFirewall/socketfilterfw").args(args).output()?;
        Ok(String::from_utf8_lossy(&out.stdout).to_lowercase())
    };
    if fw(&["--getblockall"])?.contains("enabled") {
        return Ok(true);
    }
    let exe = std::env::current_exe()?;
    Ok(fw(&["--getappblocked", &exe.to_string_lossy()])?.contains("is blocked"))
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn blocked() -> Result<bool> {
    Ok(false)
}

/// A rule's program (it may hold `%VARIABLES%`) names `exe`.
#[cfg(windows)]
fn same_program(rule: &str, exe: &std::path::Path) -> bool {
    let expanded = expand_env(rule, |v| std::env::var(v).ok());
    expanded.eq_ignore_ascii_case(&exe.to_string_lossy())
}

/// Replaces `%NAME%` with the value `lookup` gives; unknown names stay as written.
#[cfg_attr(not(windows), allow(dead_code))]
fn expand_env(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%').filter(|&end| end > 0).and_then(|end| Some((end, lookup(&after[..end])?))) {
            Some((end, value)) => {
                out.push_str(&value);
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The PowerShell that removes this program's inbound block rules and allows inbound UDP for it.
#[cfg_attr(not(windows), allow(dead_code))]
fn fix_script(exe: &str) -> String {
    let exe = exe.replace('\'', "''");
    format!(
        "$ErrorActionPreference = 'Stop'; $p = '{exe}'; \
         Get-NetFirewallApplicationFilter -Program $p -ErrorAction SilentlyContinue | Get-NetFirewallRule | \
         Where-Object {{ $_.Direction -eq 'Inbound' -and $_.Action -eq 'Block' }} | Remove-NetFirewallRule; \
         Get-NetFirewallRule -DisplayName 'ai-remote controller' -ErrorAction SilentlyContinue | Remove-NetFirewallRule; \
         New-NetFirewallRule -DisplayName 'ai-remote controller' -Direction Inbound -Action Allow -Program $p \
         -Protocol UDP -Profile Any -Description 'Peer-to-peer connections to paired ai-remote hosts' | Out-Null"
    )
}

/// Removes this program's inbound block rules and allows inbound UDP for it. Asks for an
/// administrator's consent (one UAC prompt) and waits for the answer.
#[cfg(windows)]
pub fn fix() -> Result<()> {
    let exe = std::env::current_exe()?;
    let utf16: Vec<u8> = fix_script(&exe.to_string_lossy()).encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = data_encoding::BASE64.encode(&utf16);
    // The outer PowerShell runs unelevated and starts the elevated one; -EncodedCommand needs no quoting.
    let launcher = format!(
        "$p = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden \
         -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{encoded}'; exit $p.ExitCode"
    );
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &launcher])
        .status()?;
    if !status.success() {
        bail!("the firewall was not changed (the administrator prompt was declined or the change failed)");
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn fix() -> Result<()> {
    bail!("allow incoming connections for ai-remote in the system firewall settings")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_variables_in_rule_programs() {
        let lookup = |v: &str| (v == "LOCALAPPDATA").then(|| r"C:\Users\a\AppData\Local".to_string());
        assert_eq!(
            expand_env(r"%LOCALAPPDATA%\ai-remote\app.exe", lookup),
            r"C:\Users\a\AppData\Local\ai-remote\app.exe"
        );
        assert_eq!(expand_env(r"%NOPE%\x.exe", lookup), r"%NOPE%\x.exe");
        assert_eq!(expand_env("100% sure", lookup), "100% sure");
    }

    /// Reads this machine's firewall (read-only; prints the answer).
    #[test]
    #[ignore]
    fn reads_this_machines_firewall() {
        println!("blocked: {:?}", blocked().unwrap());
    }

    #[test]
    fn the_fix_quotes_the_program_path() {
        let script = fix_script(r"C:\Users\o'neil\ai-remote.exe");
        assert!(script.contains(r"$p = 'C:\Users\o''neil\ai-remote.exe'"));
        assert!(script.contains("-Direction Inbound -Action Allow -Program $p"));
    }
}
