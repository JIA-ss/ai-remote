//! Turning a command string into a process, shared by `shell` and background jobs, so both tools
//! quote, encode and launch commands the same way.
//!
//! - **PowerShell** (`powershell`, `pwsh`): the script travels base64-encoded (UTF-8) in an
//!   environment variable, and a fixed bootstrap passed with `-Command` decodes, parses and
//!   dot-sources it, so no quoting can break it. `-EncodedCommand` avoids quoting too, but then
//!   PowerShell writes every error and progress record to stderr as CLIXML, and its UTF-16 encoding
//!   uses up the 32 K command-line limit almost three times as fast. The bootstrap forces UTF-8 in
//!   and out, silences progress output and removes the variable before the script runs, so child
//!   processes never see it. The script is parsed on its own first, so syntax errors point at the
//!   caller's lines; only then is a check appended that turns the last command's failure into the
//!   exit code (the real code of a native command, 1 for a failed cmdlet); PowerShell itself only
//!   reports 0 or 1.
//! - pwsh 7 colours errors with ANSI escapes whenever TERM, read once at startup, does not say
//!   "dumb" (neither `$PSStyle.OutputRendering` nor `NO_COLOR` turn that off for stderr). PowerShell
//!   therefore starts with `TERM=dumb`, and the bootstrap gives the script the TERM it would have
//!   had otherwise, so the programs it runs see no difference.
//! - **cmd**: `cmd /d /s /c "chcp 65001 >nul & (<command>)"`. `/s` strips exactly the outer quotes,
//!   so the command reaches cmd verbatim and quotes, `&`, `|` and `%VAR%` keep their usual meaning.
//!   Limits: one command line (a newline would end it, so multi-line input is refused), at most
//!   8191 characters, and `%VAR%` is expanded the way the interactive prompt does it.
//! - **sh**, **bash**, **zsh** (not Windows): `<shell> -c <command>`.
//!
//! System binaries are called by absolute path: PATH on real machines is often polluted (lesson F8).
//! Callers wait on the command's own process ([`main_process`]), not on process-wrap's wrappers.

use std::{any::Any, collections::BTreeMap, path::Path};

use anyhow::{Result, bail};
use base64::Engine;
use process_wrap::tokio::ChildWrapper;

/// Environment changes for a command: a value sets the variable, `null` removes it.
pub type EnvMap = BTreeMap<String, Option<String>>;

/// The interpreter a command string is handed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    PowerShell,
    Pwsh,
    Cmd,
    Sh,
    Bash,
    Zsh,
}

impl Shell {
    /// The shell named by a tool argument; `None` picks the platform default.
    pub fn parse(name: Option<&str>) -> Result<Self> {
        let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
            return Ok(if cfg!(windows) { Shell::PowerShell } else { Shell::Sh });
        };
        let shell = match name.to_ascii_lowercase().as_str() {
            "powershell" if cfg!(windows) => Shell::PowerShell,
            "pwsh" => Shell::Pwsh,
            "cmd" if cfg!(windows) => Shell::Cmd,
            "sh" if !cfg!(windows) => Shell::Sh,
            "bash" if !cfg!(windows) => Shell::Bash,
            "zsh" if !cfg!(windows) => Shell::Zsh,
            _ => {
                let known =
                    if cfg!(windows) { "powershell (default), pwsh or cmd" } else { "sh (default), bash, zsh or pwsh" };
                bail!("unknown shell '{name}' on this host; use {known}")
            }
        };
        Ok(shell)
    }

    pub fn name(self) -> &'static str {
        match self {
            Shell::PowerShell => "powershell",
            Shell::Pwsh => "pwsh",
            Shell::Cmd => "cmd",
            Shell::Sh => "sh",
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
        }
    }
}

/// What to run, independent of how its output is collected.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec<'a> {
    pub command: &'a str,
    pub shell: Shell,
    pub cwd: Option<&'a str>,
    pub env: Option<&'a EnvMap>,
}

/// Environment variable carrying the PowerShell script; removed by the bootstrap before the script runs.
const PS_SCRIPT_VAR: &str = "AI_REMOTE_PS_SCRIPT";
/// The TERM the PowerShell script should see (absent: none), while PowerShell itself starts with
/// `TERM=dumb`; restored and removed by the bootstrap.
const PS_TERM_VAR: &str = "AI_REMOTE_PS_TERM";
/// Names reserved for passing data to the bootstrap.
const RESERVED_ENV_PREFIX: &str = "AI_REMOTE_PS_";

/// Decodes, parses and dot-sources the script from [`PS_SCRIPT_VAR`]. Parsing with a file name
/// makes errors point at `command:<line>`; running it as the function `command` makes
/// `Write-Error` show "command : ..." instead of echoing the whole script.
///
/// The script is parsed alone first and its syntax errors are reported as they are; appending the
/// exit-code check before that made errors at the end of a script point at the check. A script that
/// parses runs with the check on a line of its own (when the combination does not parse, e.g. after
/// a trailing backtick, it runs without it).
const PS_BOOTSTRAP: &str = "$ProgressPreference='SilentlyContinue'; \
if(Test-Path Env:AI_REMOTE_PS_TERM){$env:TERM=$env:AI_REMOTE_PS_TERM; Remove-Item Env:AI_REMOTE_PS_TERM}else{Remove-Item Env:TERM -ErrorAction Ignore}; \
$__ar_u=New-Object Text.UTF8Encoding $false; [Console]::OutputEncoding=$__ar_u; [Console]::InputEncoding=$__ar_u; $OutputEncoding=$__ar_u; \
$__ar_s=$__ar_u.GetString([Convert]::FromBase64String($env:AI_REMOTE_PS_SCRIPT)); Remove-Item Env:AI_REMOTE_PS_SCRIPT; \
$__ar_e=$null; $__ar_a=[Management.Automation.Language.Parser]::ParseInput($__ar_s,'command',[ref]$null,[ref]$__ar_e); \
if($__ar_e){$__ar_e|ForEach-Object{[Console]::Error.WriteLine($_.ToString())}; exit 1}; \
$__ar_x=[Management.Automation.Language.Parser]::ParseInput($__ar_s+[char]10+'if (-not $?) { if ($LASTEXITCODE) { exit $LASTEXITCODE } else { exit 1 } }','command',[ref]$null,[ref]$__ar_e); \
if(-not $__ar_e){$__ar_a=$__ar_x}; \
${function:command}=$__ar_a.GetScriptBlock(); . command";

/// Longest environment variable value Windows accepts.
const MAX_ENV_VALUE: usize = 32_767;
/// Longest command line cmd.exe accepts.
const MAX_CMD_LINE: usize = 8_191;

/// Builds the process for `spec`: program, arguments, working directory and environment. Stdio and
/// creation flags are left to the caller.
pub fn build(spec: &CommandSpec<'_>) -> Result<tokio::process::Command> {
    if spec.command.trim().is_empty() {
        bail!("command is empty");
    }
    if let Some(cwd) = spec.cwd
        && !Path::new(cwd).is_dir()
    {
        bail!("working directory '{cwd}' does not exist or is not a directory on the host");
    }
    let mut c = match spec.shell {
        Shell::PowerShell | Shell::Pwsh => {
            let exe = if spec.shell == Shell::Pwsh {
                "pwsh".to_string()
            } else {
                system32("WindowsPowerShell\\v1.0\\powershell.exe")
            };
            let mut c = tokio::process::Command::new(exe);
            c.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                PS_BOOTSTRAP,
            ]);
            c
        }
        Shell::Cmd => {
            let mut c = tokio::process::Command::new(system32("cmd.exe"));
            raw_arg(&mut c, &cmd_args(spec.command)?);
            c
        }
        Shell::Sh => {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.args(["-c", spec.command]);
            c
        }
        Shell::Bash | Shell::Zsh => {
            let mut c = tokio::process::Command::new(spec.shell.name());
            c.args(["-c", spec.command]);
            c
        }
    };
    if let Some(cwd) = spec.cwd {
        c.current_dir(cwd);
    }
    // Python writes the ANSI code page to pipes unless told otherwise; match the forced UTF-8.
    #[cfg(windows)]
    if std::env::var_os("PYTHONIOENCODING").is_none() {
        c.env("PYTHONIOENCODING", "utf-8");
    }
    if let Some(env) = spec.env {
        for (name, value) in env {
            check_env_name(name)?;
            match value {
                Some(v) if v.contains('\0') => bail!("environment variable '{name}' contains a NUL character"),
                Some(v) => c.env(name, v),
                None => c.env_remove(name),
            };
        }
    }
    if matches!(spec.shell, Shell::PowerShell | Shell::Pwsh) {
        let term = match spec.env.and_then(|env| env.iter().find(|(name, _)| name.eq_ignore_ascii_case("TERM"))) {
            Some((_, value)) => value.clone(),
            None => std::env::var("TERM").ok(),
        };
        c.env("TERM", "dumb");
        match term {
            Some(term) => c.env(PS_TERM_VAR, term),
            None => c.env_remove(PS_TERM_VAR),
        };
        c.env(PS_SCRIPT_VAR, powershell_payload(spec.command)?);
    }
    Ok(c)
}

/// The base64 value of [`PS_SCRIPT_VAR`] for a script.
fn powershell_payload(command: &str) -> Result<String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(command.as_bytes());
    if encoded.len() > MAX_ENV_VALUE {
        bail!(
            "the PowerShell command is too long ({} bytes; the limit is about 24 KB): write the script to a file \
             on the host and run that file instead",
            command.len()
        );
    }
    Ok(encoded)
}

/// Arguments after `cmd.exe` for one command line.
fn cmd_args(command: &str) -> Result<String> {
    let command = command.trim_end_matches(['\r', '\n']);
    if command.contains(['\r', '\n']) {
        bail!(
            "cmd runs a single command line: join the commands with & (or && to stop at the first failure), \
             or use shell \"powershell\" for a multi-line script"
        );
    }
    // Keep chcp outside the caller's pipeline. Otherwise cmd runs the prefix in a pipe child,
    // leaving the parent on its original code page and replacing Unicode echo output with '?'.
    let args = format!("/d /s /c \"chcp 65001 >nul & ({command})\"");
    if args.len() + 32 > MAX_CMD_LINE {
        bail!(
            "the cmd command line is too long (cmd accepts at most {MAX_CMD_LINE} characters); use shell \"powershell\" or a script file"
        );
    }
    Ok(args)
}

fn check_env_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains(['=', '\0']) {
        bail!("invalid environment variable name '{name}': it must be non-empty and contain no '=' or NUL");
    }
    if name.get(..RESERVED_ENV_PREFIX.len()).is_some_and(|p| p.eq_ignore_ascii_case(RESERVED_ENV_PREFIX)) {
        bail!("the environment variable name '{name}' is reserved by ai-remote");
    }
    Ok(())
}

#[cfg(windows)]
fn raw_arg(c: &mut tokio::process::Command, args: &str) {
    c.raw_arg(args);
}

#[cfg(not(windows))]
fn raw_arg(_c: &mut tokio::process::Command, _args: &str) {
    unreachable!("cmd is only offered on Windows")
}

/// The command's own process inside process-wrap's wrappers. Wait on this, not on the wrapper:
/// the Job Object wrapper documents waiting for every process in the job (it returns when the main
/// process exits only because of how it polls the job's completion port), and the process group
/// wrapper also reaps the group. Callers decide themselves what happens to processes left behind.
pub fn main_process(child: &mut dyn ChildWrapper) -> &mut tokio::process::Child {
    if (&*child as &dyn Any).is::<tokio::process::Child>() {
        return (child as &mut dyn Any).downcast_mut().expect("the type was checked just above");
    }
    main_process(child.inner_mut())
}

/// Absolute path under System32: never trust PATH for system tools.
fn system32(rel: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    format!("{root}\\System32\\{rel}")
}

/// Decodes command output: UTF-8 (a leading BOM dropped), with lines that are not valid UTF-8
/// decoded in the system's ANSI code page, which is what programs that ignore the console code
/// page (Python, older Java) write. Line by line, so one such line does not garble the rest.
pub fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut out = String::with_capacity(bytes.len());
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        match std::str::from_utf8(line) {
            Ok(s) => out.push_str(s),
            Err(_) => out.push_str(&decode_legacy(line)),
        }
    }
    out
}

#[cfg(windows)]
fn decode_legacy(bytes: &[u8]) -> String {
    use windows::Win32::Globalization::{CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar};
    if bytes.len() > i32::MAX as usize {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    // SAFETY: the slices describe valid buffers; the first call only measures.
    let needed = unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None) };
    if needed <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; needed as usize];
    // SAFETY: as above, with a buffer of the measured size.
    let written = unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut wide)) };
    String::from_utf16_lossy(&wide[..written.max(0) as usize])
}

#[cfg(not(windows))]
fn decode_legacy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_unknown_shells() {
        let default = Shell::parse(None).unwrap();
        assert_eq!(default, if cfg!(windows) { Shell::PowerShell } else { Shell::Sh });
        assert_eq!(Shell::parse(Some("  ")).unwrap(), default);
        assert_eq!(Shell::parse(Some("PWSH")).unwrap(), Shell::Pwsh);
        let err = Shell::parse(Some("fish")).unwrap_err().to_string();
        assert!(err.contains("unknown shell 'fish'"), "{err}");
    }

    #[test]
    fn cmd_line_is_passed_verbatim_inside_outer_quotes() {
        let args = cmd_args(r#"echo "a & b" | findstr a & echo 100%"#).unwrap();
        assert_eq!(args, r#"/d /s /c "chcp 65001 >nul & (echo "a & b" | findstr a & echo 100%)""#);
        // A trailing newline is tolerated; an inner one is refused with advice.
        assert!(cmd_args("dir\r\n").is_ok());
        let err = cmd_args("echo a\necho b").unwrap_err().to_string();
        assert!(err.contains("single command line"), "{err}");
        assert!(cmd_args(&"x".repeat(9000)).unwrap_err().to_string().contains("too long"));
    }

    #[test]
    fn powershell_payload_round_trips_and_has_a_limit() {
        let payload = powershell_payload("Write-Output 'héllo 世界'").unwrap();
        let script = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(payload).unwrap()).unwrap();
        assert_eq!(script, "Write-Output 'héllo 世界'");
        assert!(powershell_payload(&"x".repeat(30_000)).unwrap_err().to_string().contains("too long"));
    }

    #[test]
    fn env_names_are_checked() {
        assert!(check_env_name("PATH").is_ok());
        assert!(check_env_name("").is_err());
        assert!(check_env_name("A=B").is_err());
        assert!(check_env_name("ai_remote_ps_script").is_err());
        assert!(check_env_name("AI_REMOTE_PS_TERM").is_err());
        assert!(check_env_name("AI_REMOTE").is_ok());
        assert!(check_env_name("TÉRM").is_ok());
    }

    #[test]
    fn bootstrap_needs_no_quoting_beyond_one_argument() {
        // The bootstrap is passed as a single argument; double quotes inside would need escaping.
        assert!(!PS_BOOTSTRAP.contains('"'));
        assert!(PS_BOOTSTRAP.contains(PS_SCRIPT_VAR) && PS_BOOTSTRAP.contains(PS_TERM_VAR));
    }

    #[test]
    fn decode_drops_bom_and_keeps_utf8() {
        assert_eq!(decode(b"\xEF\xBB\xBFhi \xE4\xB8\x96"), "hi 世");
        // A non-UTF-8 line does not garble the UTF-8 lines around it.
        let mixed = decode(b"ok \xE4\xB8\x96\nbad \xFF\nend\n");
        assert!(mixed.starts_with("ok 世\n"), "{mixed}");
        assert!(mixed.ends_with("\nend\n"), "{mixed}");
    }

    #[test]
    fn missing_cwd_is_reported() {
        let spec = CommandSpec {
            command: "echo hi",
            shell: Shell::parse(None).unwrap(),
            cwd: Some("Z:/no/such/dir"),
            env: None,
        };
        let err = build(&spec).unwrap_err().to_string();
        assert!(err.contains("does not exist"), "{err}");
    }
}
