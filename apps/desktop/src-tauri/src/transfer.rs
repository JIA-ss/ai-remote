//! Whole-file transfers for the app: a file dropped on the session goes to the host, a file on the
//! host's clipboard comes to this computer. The same two steps as the CLI's `push` / `pull`
//! (`crates/ai-remote/src/remote.rs`): the person's `files_push` / `files_pull` call issues a
//! ticket (the host's level, policy and approval apply), then the bytes go over the agent's
//! `files` connection with their SHA-256 checked on both ends.

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::Profile;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};

/// One transfer's progress for the page.
pub struct Progress<'a> {
    report: &'a (dyn Fn(u64, u64) + Send + Sync),
    total: u64,
    done: u64,
    last: Instant,
}

impl<'a> Progress<'a> {
    fn new(total: u64, report: &'a (dyn Fn(u64, u64) + Send + Sync)) -> Self {
        (report)(0, total);
        Self { report, total, done: 0, last: Instant::now() }
    }

    fn add(&mut self, n: usize) {
        self.done += n as u64;
        if self.last.elapsed().as_millis() >= 100 || self.done == self.total {
            self.last = Instant::now();
            (self.report)(self.done, self.total);
        }
    }
}

/// Calls one tool on `host` as the person at this device.
async fn call(profile: &Profile, cli: &Path, host: &str, tool: &str, args: Value) -> Result<Value> {
    let mut conn = ar_controller::connect_agent_with(profile, cli).await?;
    let result = conn.request(&json!({"op": "call", "host": host, "tool": tool, "args": args, "person": true})).await?;
    if result["isError"] == true {
        let text = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        bail!("{}", if text.is_empty() { "the host refused".to_string() } else { text });
    }
    // Tools answer with one JSON line in their first text block.
    let text = result["content"].as_array().and_then(|c| c.first()).and_then(|c| c["text"].as_str()).unwrap_or("{}");
    serde_json::from_str(text.lines().next().unwrap_or("{}")).map_err(|e| anyhow!("bad answer from {tool}: {e}"))
}

/// Redeems a transfer ticket through the agent: the host's first reply and the stream after it.
async fn open_transfer(
    profile: &Profile,
    cli: &Path,
    host: &str,
    ticket: &str,
) -> Result<(Value, impl AsyncRead + Unpin, impl tokio::io::AsyncWrite + Unpin)> {
    let mut conn = ar_controller::connect_agent_with(profile, cli).await?;
    let reply = conn.request(&json!({"op": "files", "host": host, "request": {"ticket": ticket}})).await?;
    let (stream, buffered) = conn.into_raw();
    let (read, write) = tokio::io::split(stream);
    Ok((reply, std::io::Cursor::new(buffered).chain(read), write))
}

/// One JSON line from the host, after the bytes of a transfer.
async fn read_line<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Value> {
    let mut line = String::new();
    let mut reader = BufReader::new(reader.take(64 * 1024));
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        bail!("the host ended the transfer without an answer");
    }
    serde_json::from_str(&line).map_err(|e| anyhow!("bad answer from the host: {e}"))
}

fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn remote_join(dir: &str, name: &str) -> String {
    let sep = if dir.contains('\\') && !dir.contains('/') { '\\' } else { '/' };
    let dir = dir.trim_end_matches(['/', '\\']);
    format!("{dir}{sep}{name}")
}

/// Sends `local` to `host`, into folder `remote_dir` under the file's own name. Returns
/// `{remote, bytes, sha256, seconds}`.
pub async fn push(
    profile: &Profile,
    cli: &Path,
    host: &str,
    local: &Path,
    remote_dir: &str,
    report: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<Value> {
    let meta = std::fs::metadata(local).with_command_context(local)?;
    if !meta.is_file() {
        bail!("{} is not a file (folders cannot be sent yet; pack one first)", local.display());
    }
    let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let remote = free_remote_name(profile, cli, host, remote_dir, &name).await?;
    let path = local.to_path_buf();
    let sha256 = tokio::task::spawn_blocking(move || sha256_file(&path)).await??;
    let mut args = Map::new();
    args.insert("path".into(), remote.clone().into());
    args.insert("size".into(), meta.len().into());
    args.insert("sha256".into(), sha256.clone().into());
    // A file of that name appearing since the check above is not replaced either: the host refuses.
    args.insert("overwrite".into(), false.into());
    args.insert("mkdir".into(), true.into());
    args.insert("intent".into(), "a file you dropped on the session in the app".into());
    let issued = call(profile, cli, host, "files_push", Value::Object(args)).await?;
    let ticket = issued["ticket"].as_str().ok_or_else(|| anyhow!("the host issued no transfer ticket"))?;
    let (reply, mut reader, mut writer) = open_transfer(profile, cli, host, ticket).await?;
    let total = reply["size"].as_u64().unwrap_or(meta.len());
    let started = Instant::now();
    let mut progress = Progress::new(total, report);
    let mut file = tokio::fs::File::open(local).await.with_command_context(local)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut sent = 0u64;
    while sent < total {
        let n = file.read(&mut buf).await.with_command_context(local)?;
        if n == 0 {
            bail!("{} shrank while it was sent", local.display());
        }
        let n = n.min((total - sent) as usize);
        if let Err(e) = writer.write_all(&buf[..n]).await {
            // The host stops reading when it fails; its answer says why.
            let answer = read_line(&mut reader).await.ok();
            let why = answer.as_ref().and_then(|v| v["error"].as_str()).map(str::to_string);
            bail!("{}", why.unwrap_or_else(|| format!("the transfer broke off: {e}")));
        }
        sent += n as u64;
        progress.add(n);
    }
    writer.flush().await?;
    let answer = read_line(&mut reader).await?;
    if answer["ok"] != true {
        bail!("{}", answer["error"].as_str().unwrap_or("the host refused the file"));
    }
    Ok(json!({
        "remote": answer["path"].as_str().or(issued["path"].as_str()).unwrap_or(&remote),
        "bytes": answer["bytes"],
        "sha256": answer["sha256"],
        "seconds": started.elapsed().as_secs_f64(),
    }))
}

/// `name` in `remote_dir` on the host, or `name-1`, `name-2`, ... when taken: a dropped file never
/// replaces one already there.
async fn free_remote_name(profile: &Profile, cli: &Path, host: &str, remote_dir: &str, name: &str) -> Result<String> {
    for n in 0..100 {
        let candidate = if n == 0 { name.to_string() } else { numbered(name, n) };
        let path = remote_join(remote_dir, &candidate);
        let stat = call(profile, cli, host, "files_stat", json!({"path": path})).await?;
        if stat["exists"] != true {
            return Ok(path);
        }
    }
    bail!("{remote_dir} on the host already has {name} and 99 numbered copies of it")
}

/// `name` with `-n` before its extension.
fn numbered(name: &str, n: usize) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{n}.{ext}"),
        _ => format!("{name}-{n}"),
    }
}

/// Fetches `remote` from `host` into `local_dir` under its own name (a `-1`, `-2` suffix when a
/// file of that name exists). Returns `{local, bytes, sha256, seconds}`.
pub async fn pull(
    profile: &Profile,
    cli: &Path,
    host: &str,
    remote: &str,
    local_dir: &Path,
    report: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<Value> {
    let name = remote.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|n| !n.is_empty());
    let Some(name) = name.map(safe_name) else { bail!("'{remote}' names no file") };
    let name = name.as_str();
    std::fs::create_dir_all(local_dir).with_context(|| format!("create {}", local_dir.display()))?;
    let dest = free_name(local_dir, name);
    let issued = call(
        profile,
        cli,
        host,
        "files_pull",
        json!({"path": remote, "intent": "a file on the host's clipboard, fetched from the app"}),
    )
    .await?;
    let ticket = issued["ticket"].as_str().ok_or_else(|| anyhow!("the host issued no transfer ticket"))?;
    let (reply, mut reader, _writer) = open_transfer(profile, cli, host, ticket).await?;
    let total = reply["size"].as_u64().ok_or_else(|| anyhow!("the host did not say how big the file is"))?;
    let part = local_dir.join(format!(".{}.ai-remote.part", dest.file_name().unwrap_or_default().to_string_lossy()));
    let started = Instant::now();
    let mut progress = Progress::new(total, report);
    let received = receive_into(&part, &mut reader, total, &mut progress).await;
    let digest = match received {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    let trailer = read_line(&mut reader).await;
    let expected = trailer.as_ref().ok().and_then(|v| v["sha256"].as_str()).map(str::to_string);
    if expected.as_deref() != Some(digest.as_str()) {
        let _ = std::fs::remove_file(&part);
        bail!(
            "the received file's SHA-256 ({digest}) does not match the host's ({}); nothing was saved",
            expected.as_deref().unwrap_or("missing")
        );
    }
    std::fs::rename(&part, &dest).with_context(|| format!("save {}", dest.display()))?;
    Ok(json!({
        "local": dest.display().to_string(),
        "bytes": total,
        "sha256": digest,
        "seconds": started.elapsed().as_secs_f64(),
    }))
}

/// A file name from the host made safe to create here: no drive prefix (`C:x` would leave the
/// folder on Windows), no characters Windows refuses, no leading dots or trailing dots and spaces.
fn safe_name(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let cleaned = cleaned.trim_start_matches('.').trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() { "file".to_string() } else { cleaned }
}

/// `name` in `dir`, or `name-1`, `name-2`, ... when taken (the extension stays last).
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    (1..).map(|n| dir.join(numbered(name, n))).find(|p| !p.exists()).expect("an unused name")
}

/// Reads exactly `total` bytes into `part` and returns their SHA-256.
async fn receive_into<R: AsyncRead + Unpin>(
    part: &Path,
    reader: &mut R,
    total: u64,
    progress: &mut Progress<'_>,
) -> Result<String> {
    let mut file = tokio::fs::File::create(part).await.with_context(|| format!("write {}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0u64;
    while got < total {
        let want = (total - got).min(buf.len() as u64) as usize;
        let n = reader.read(&mut buf[..want]).await.map_err(|e| anyhow!("the transfer broke off: {e}"))?;
        if n == 0 {
            bail!("the host stopped sending after {got} of {total} bytes");
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await.with_context(|| format!("write {}", part.display()))?;
        got += n as u64;
        progress.add(n);
    }
    file.flush().await?;
    file.sync_all().await?;
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

trait ReadContext<T> {
    fn with_command_context(self, path: &Path) -> Result<T>;
}

impl<T, E: std::error::Error + Send + Sync + 'static> ReadContext<T> for std::result::Result<T, E> {
    fn with_command_context(self, path: &Path) -> Result<T> {
        self.with_context(|| format!("read {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_remote_paths_of_either_system() {
        assert_eq!(remote_join(r"C:\Users\me\Desktop", "a.txt"), r"C:\Users\me\Desktop\a.txt");
        assert_eq!(remote_join("~/Desktop/", "a.txt"), "~/Desktop/a.txt");
    }

    #[test]
    fn names_from_the_host_cannot_leave_the_folder() {
        assert_eq!(safe_name("C:evil.dll"), "C_evil.dll");
        assert_eq!(safe_name("..."), "file");
        assert_eq!(safe_name(".hidden. "), "hidden");
        assert_eq!(safe_name("a<b>.txt"), "a_b_.txt");
        assert_eq!(safe_name("报告.pdf"), "报告.pdf");
    }

    #[test]
    fn free_names_keep_the_extension_last() {
        let dir = std::env::temp_dir().join(format!("ai-remote-free-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.tar.gz"), b"x").unwrap();
        assert_eq!(free_name(&dir, "a.tar.gz"), dir.join("a.tar-1.gz"));
        assert_eq!(free_name(&dir, "b"), dir.join("b"));
        std::fs::write(dir.join("b"), b"x").unwrap();
        assert_eq!(free_name(&dir, "b"), dir.join("b-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
