//! The Windows Task Scheduler definition that starts the background host at logon.
//!
//! Plain text generation (no Windows APIs) so the exact XML is unit-tested on every platform and
//! shown verbatim by `ai-remote host install --dry-run`.

use std::path::Path;

use quick_xml::escape::{escape, unescape};

/// What the task runs and for whom.
pub struct TaskSpec<'a> {
    /// The account whose logon triggers the task and whose token runs it (a SID is safest:
    /// it survives renames and needs no domain prefix).
    pub user_id: &'a str,
    pub description: &'a str,
    pub command: &'a Path,
    /// Already quoted for the Windows command line (see [`quote_arg`]).
    pub arguments: &'a str,
    pub working_dir: &'a Path,
}

/// The task definition. Every setting the defaults get wrong for a long-running host is explicit:
/// no execution time limit (the default stops a task after 3 days), never stopped or blocked on
/// battery, started late if the logon was missed, and a second start while one runs is ignored.
/// The interactive token puts the host in the user's desktop session (capture, input and approval
/// prompts need it); highest privileges let it drive elevated windows.
pub fn task_xml(spec: &TaskSpec<'_>) -> String {
    let user = escape(spec.user_id);
    let description = escape(spec.description);
    let command = escape(path_str(spec.command));
    let arguments = escape(spec.arguments);
    let working_dir = escape(path_str(spec.working_dir));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>ai-remote</Author>
    <Description>{description}</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
      <WorkingDirectory>{working_dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// The file schtasks reads: UTF-16LE with a byte-order mark, matching the declaration, so
/// non-ASCII user or folder names survive.
pub fn utf16_file(xml: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in xml.replace('\n', "\r\n").encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// The program and arguments of the first `Exec` action in a task's XML (as `schtasks /Query
/// /XML` prints it).
pub fn read_exec(xml: &str) -> Option<(String, String)> {
    let exec = between(xml, "<Exec>", "</Exec>")?;
    let command = between(exec, "<Command>", "</Command>")?;
    let arguments = between(exec, "<Arguments>", "</Arguments>").unwrap_or("");
    Some((unescape(command.trim()).ok()?.into_owned(), unescape(arguments.trim()).ok()?.into_owned()))
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

/// Quotes one argument for the Windows command line (the rules `CommandLineToArgvW` and the C
/// runtime parse by): backslashes are literal except before a quote, so the ones before an
/// embedded or closing quote are doubled. Always quotes, which keeps paths readable.
pub fn quote_arg(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use quick_xml::{Reader, events::Event};

    use super::*;

    fn spec_xml(command: &str, arguments: &str) -> String {
        let command = PathBuf::from(command);
        let dir = PathBuf::from(r"C:\Tools & Co\ai-remote");
        task_xml(&TaskSpec {
            user_id: "S-1-5-21-1-2-3-1001",
            description: "Starts <the> host",
            command: &command,
            arguments,
            working_dir: &dir,
        })
    }

    #[test]
    fn task_xml_is_well_formed() {
        let xml = spec_xml(r"C:\Tools & Co\ai-remote\ai-remote-hostw.exe", r#"--home "C:\Users\me\AI <remote>""#);
        let mut reader = Reader::from_str(&xml);
        let mut elements = 0;
        loop {
            match reader.read_event() {
                Ok(Event::Eof) => break,
                Ok(Event::Start(_)) => elements += 1,
                Ok(_) => {}
                Err(e) => panic!("malformed task XML at {}: {e}\n{xml}", reader.buffer_position()),
            }
        }
        assert!(elements > 20);
    }

    #[test]
    fn task_xml_has_the_settings_a_long_running_host_needs() {
        let xml = spec_xml(r"C:\x\ai-remote-hostw.exe", "");
        for needle in [
            "<LogonType>InteractiveToken</LogonType>",
            "<RunLevel>HighestAvailable</RunLevel>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StartWhenAvailable>true</StartWhenAvailable>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<UserId>S-1-5-21-1-2-3-1001</UserId>",
        ] {
            assert!(xml.contains(needle), "missing {needle}");
        }
        // Logon trigger and principal name the same user.
        assert_eq!(xml.matches("<UserId>S-1-5-21-1-2-3-1001</UserId>").count(), 2);
    }

    #[test]
    fn exec_round_trips_through_escaping() {
        let command = r"C:\Tools & Co\ai-remote\ai-remote-hostw.exe";
        let arguments = r#"--home "C:\Users\me\AI <remote>""#;
        let (c, a) = read_exec(&spec_xml(command, arguments)).unwrap();
        assert_eq!(c, command);
        assert_eq!(a, arguments);
    }

    #[test]
    fn reads_exec_from_a_queried_task() {
        let queried = "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\r\n<Task><Actions Context=\"Author\">\r\r\n\
                       <Exec>\r\r\n<Command>C:\\a\\ai-remote-hostw.exe</Command>\r\r\n</Exec></Actions></Task>";
        assert_eq!(read_exec(queried), Some((r"C:\a\ai-remote-hostw.exe".into(), String::new())));
        assert_eq!(read_exec("<Task/>"), None);
    }

    #[test]
    fn quotes_like_the_c_runtime() {
        assert_eq!(quote_arg(r"C:\Program Files\x"), r#""C:\Program Files\x""#);
        assert_eq!(quote_arg(r"C:\dir\"), r#""C:\dir\\""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_arg(""), r#""""#);
    }

    #[test]
    fn utf16_file_has_bom_and_crlf() {
        let bytes = utf16_file("a\nb");
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        let units: Vec<u16> = bytes[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "a\r\nb");
    }
}
