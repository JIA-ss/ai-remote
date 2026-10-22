//! Files on the host: list, inspect, create and delete paths, and move whole files between a
//! controller and the host (`files_push`, `files_pull`).
//!
//! Moving a file takes two steps, so that the owner approves it once with what it moves in the
//! prompt and the bytes never pass through MCP (whose results stay near 1 MiB): the tool call goes
//! through the gate like any other and returns a one-time ticket; the controller then sends or
//! receives the bytes on the files channel under that ticket (see [`crate::files_channel`]). A
//! ticket is bound to the device it was issued to and names the path and, for an upload, the size
//! and SHA-256 that were approved; it is valid once, for [`TICKET_TTL`].
//!
//! Paths must be absolute: the host's working directory is wherever its program was started, which
//! a caller cannot know. Every file tool is admin level, because file names and contents are as
//! sensitive as what `shell` reaches. Reading metadata (`files_list`, `files_stat`) runs without
//! asking; moving, creating and deleting ask the owner by default.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::Level;
use async_trait::async_trait;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolSpec, parse};
use crate::session::ToolCtx;

/// How long a transfer ticket can be redeemed after it was issued.
pub const TICKET_TTL: Duration = Duration::from_secs(120);
/// Tickets outstanding at once; older ones are dropped first.
const MAX_TICKETS: usize = 64;
/// Most entries per `files_list` page.
const PAGE: usize = 500;
/// Most characters of entries per page, so a page of long names stays below the result budget
/// (which would cut the JSON in the middle).
const PAGE_CHARS: usize = 40_000;

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(FilesList::new()),
        Arc::new(FilesStat::new()),
        Arc::new(FilesMkdir::new()),
        Arc::new(FilesDelete::new()),
        Arc::new(FilesPush::new()),
        Arc::new(FilesPull::new()),
    ]
}

/// The tools whose ticket only the files channel can use: an MCP client cannot send the bytes, so
/// the controller keeps them out of an agent's tool list (`ai-remote push` / `pull` use them).
pub const TRANSFER_TOOLS: [&str; 2] = ["files_push", "files_pull"];

/// Which way a ticket moves a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Direction {
    /// Controller to host: exactly `size` bytes whose SHA-256 (lowercase hex) is `sha256`.
    Push { size: u64, sha256: String, overwrite: bool },
    /// Host to controller.
    Pull,
}

/// One approved transfer, waiting for its bytes.
#[derive(Debug, Clone)]
pub struct Ticket {
    pub id: String,
    /// The device it was issued to (`eid:..`); only that device may redeem it.
    pub principal: Option<String>,
    pub peer: String,
    /// The MCP session whose call issued it, for the audit log.
    pub session: String,
    /// Issued to a person driving (who may transfer during a takeover), not an agent.
    pub person: bool,
    pub tool: &'static str,
    pub path: PathBuf,
    pub direction: Direction,
    issued: Instant,
}

/// Tickets issued and not yet redeemed.
#[derive(Default)]
pub struct Transfers(Mutex<HashMap<String, Ticket>>);

impl Transfers {
    pub(crate) fn issue(&self, ctx: &ToolCtx<'_>, tool: &'static str, path: PathBuf, direction: Direction) -> String {
        let id = random_id();
        let ticket = Ticket {
            id: id.clone(),
            principal: ctx.session.peer.principal.clone(),
            peer: ctx.session.peer.name.clone(),
            session: ctx.session.id.clone(),
            person: ctx.session.person(),
            tool,
            path,
            direction,
            issued: Instant::now(),
        };
        let mut tickets = self.0.lock().unwrap();
        tickets.retain(|_, t| t.issued.elapsed() < TICKET_TTL);
        while tickets.len() >= MAX_TICKETS {
            let oldest = tickets.values().min_by_key(|t| t.issued).map(|t| t.id.clone());
            match oldest {
                Some(o) => tickets.remove(&o),
                None => break,
            };
        }
        tickets.insert(id.clone(), ticket);
        id
    }

    /// Takes the ticket `id` for the device `principal`; it cannot be used again.
    pub fn redeem(&self, id: &str, principal: Option<&str>) -> Result<Ticket> {
        let mut tickets = self.0.lock().unwrap();
        let Some(ticket) = tickets.get(id.trim()) else {
            bail!("unknown or already used transfer ticket; ask for a new one with files_push or files_pull");
        };
        if ticket.principal.is_none() || ticket.principal.as_deref() != principal {
            bail!("this transfer ticket was issued to another device");
        }
        let ticket = tickets.remove(id.trim()).expect("present");
        if ticket.issued.elapsed() >= TICKET_TTL {
            bail!("the transfer ticket expired ({} s after it was issued); ask for a new one", TICKET_TTL.as_secs());
        }
        Ok(ticket)
    }
}

fn random_id() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    data_encoding::HEXLOWER.encode(&b)
}

/// `path` as an absolute path, or an error an agent can act on.
/// `rest` (what followed `~`) under the home directory of the user whose desktop this host serves
/// (the signed-in user's, also for the Windows service), one component at a time so the platform's
/// separator is used. `~/Desktop` is the desktop that user sees, which on Windows may live
/// elsewhere (OneDrive moves it).
fn expand_home(rest: &str) -> Result<PathBuf> {
    let mut parts = rest.split(['/', '\\']).filter(|p| !p.is_empty()).peekable();
    let desktop = parts.peek().is_some_and(|first| first.eq_ignore_ascii_case("desktop"));
    let Some((home, desktop_dir)) = crate::platform::user_folders() else {
        bail!("the host has no signed-in user whose home '~' could mean");
    };
    let mut path = match desktop_dir {
        Some(d) if desktop => {
            parts.next();
            d
        }
        _ => home,
    };
    for part in parts {
        path.push(part);
    }
    Ok(path)
}

fn absolute(path: &str) -> Result<PathBuf> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("give a path");
    }
    // `~` and `~/...` mean the host user's home (the only folder a caller can name without asking).
    let p = match trimmed.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => expand_home(rest)?,
        _ => PathBuf::from(trimmed),
    };
    if !p.is_absolute() {
        bail!(
            "'{trimmed}' is not an absolute path. File tools take absolute paths (the host's working directory \
             is not something a caller can know); shell can print one, e.g. $env:USERPROFILE or $HOME."
        );
    }
    Ok(p)
}

fn shown(p: &Path) -> String {
    dunce::simplified(p).display().to_string()
}

fn kind(ft: &fs::FileType) -> &'static str {
    if ft.is_symlink() {
        "link"
    } else if ft.is_dir() {
        "dir"
    } else if ft.is_file() {
        "file"
    } else {
        "other"
    }
}

fn millis(t: std::io::Result<SystemTime>) -> Option<u64> {
    t.ok()?.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
}

/// `*` and `?` wildcards; case-insensitive (Windows and macOS file systems usually are).
fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni, mut star, mut mark) = (0, 0, None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Runs file system work on the blocking pool.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| anyhow!("file worker failed: {e}"))?
}

/// A result with a compact JSON text block (and the same value as structured content): listings
/// would be several times larger pretty-printed.
fn compact(value: Value) -> CallToolResult {
    let mut r = CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
    r.structured_content = Some(value);
    r
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListParams {
    /// Absolute path of a directory.
    path: String,
    /// Only names matching this pattern (`*` and `?` wildcards, case-insensitive), e.g. `*.log`.
    #[serde(default)]
    pattern: Option<String>,
    /// Skip this many entries of the sorted listing (for the next page: the previous call's
    /// next_offset).
    #[serde(default)]
    offset: Option<usize>,
}

struct FilesList {
    spec: ToolSpec,
}

impl FilesList {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<ListParams>(
                "files_list",
                Level::Admin,
                "List a directory on the host: name, kind (file, dir, link, other), size in bytes and modified time \
                 (Unix ms) of each entry, folders first, then by name. At most 500 entries per call; when more \
                 remain, call again with offset = next_offset.",
            )
            .allow()
            .read_only(),
        }
    }
}

#[async_trait]
impl Tool for FilesList {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: ListParams = parse(args)?;
        let dir = absolute(&p.path)?;
        let value = blocking(move || list(&dir, p.pattern.as_deref(), p.offset.unwrap_or(0))).await?;
        Ok(compact(value))
    }
}

fn list(dir: &Path, pattern: Option<&str>, offset: usize) -> Result<Value> {
    let read = fs::read_dir(dir).with_context(|| format!("cannot list {}", shown(dir)))?;
    let mut entries = Vec::new();
    for entry in read {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if pattern.is_some_and(|p| !glob_match(p, &name)) {
            continue;
        }
        let (kind, size, modified) = match (entry.file_type(), entry.metadata()) {
            (Ok(ft), Ok(md)) => (kind(&ft), ft.is_file().then_some(md.len()), millis(md.modified())),
            (Ok(ft), Err(_)) => (kind(&ft), None, None),
            _ => ("other", None, None),
        };
        entries.push((
            kind != "dir",
            name.to_lowercase(),
            json!({"name": name, "kind": kind, "size": size, "modified": modified}),
        ));
    }
    entries.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let total = entries.len();
    let (mut page, mut chars) = (Vec::new(), 0);
    for (_, _, entry) in entries.into_iter().skip(offset).take(PAGE) {
        chars += entry.to_string().chars().count() + 1;
        if chars > PAGE_CHARS && !page.is_empty() {
            break;
        }
        page.push(entry);
    }
    let next = offset + page.len();
    Ok(json!({
        "path": shown(dir),
        "total": total,
        "offset": offset,
        "next_offset": (next < total).then_some(next),
        "entries": page,
    }))
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PathParams {
    /// Absolute path.
    path: String,
}

struct FilesStat {
    spec: ToolSpec,
}

impl FilesStat {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<PathParams>(
                "files_stat",
                Level::Admin,
                "Whether a path exists on the host and what it is: kind (file, dir, link, other), size in bytes, \
                 modified time (Unix ms) and read-only flag. A missing path is not an error (exists: false).",
            )
            .allow()
            .read_only(),
        }
    }
}

#[async_trait]
impl Tool for FilesStat {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: PathParams = parse(args)?;
        let path = absolute(&p.path)?;
        Ok(compact(blocking(move || Ok(stat(&path))).await?))
    }
}

fn stat(path: &Path) -> Value {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"path": shown(path), "exists": false}),
        Err(e) => json!({"path": shown(path), "exists": null, "error": e.to_string()}),
        Ok(md) => {
            let mut v = json!({
                "path": shown(path),
                "exists": true,
                "kind": kind(&md.file_type()),
                "size": md.is_file().then_some(md.len()),
                "modified": millis(md.modified()),
                "readonly": md.permissions().readonly(),
            });
            if md.file_type().is_symlink() {
                v["target"] = json!(fs::read_link(path).ok().map(|t| shown(&t)));
                if let Ok(target) = fs::metadata(path) {
                    v["target_kind"] = json!(kind(&target.file_type()));
                    v["size"] = json!(target.is_file().then_some(target.len()));
                }
            }
            v
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MkdirParams {
    /// Absolute path of the folder; missing parent folders are created too.
    path: String,
    /// Why; shown to the host owner in approval prompts and the audit log.
    #[serde(default)]
    #[allow(dead_code)] // read by the gate from the raw arguments
    intent: Option<String>,
}

struct FilesMkdir {
    spec: ToolSpec,
}

impl FilesMkdir {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<MkdirParams>(
                "files_mkdir",
                Level::Admin,
                "Create a folder on the host, with any missing parent folders. Succeeds when it already exists. \
                 Usually needs the owner's approval.",
            )
            .non_destructive(),
        }
    }
}

#[async_trait]
impl Tool for FilesMkdir {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: MkdirParams = parse(args)?;
        let path = absolute(&p.path)?;
        let value = blocking(move || {
            let existed = path.is_dir();
            fs::create_dir_all(&path).with_context(|| format!("cannot create {}", shown(&path)))?;
            Ok(json!({"path": shown(&path), "created": !existed}))
        })
        .await?;
        Ok(compact(value))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DeleteParams {
    /// Absolute path of a file, link or folder.
    path: String,
    /// Delete a folder with everything in it. Without it only an empty folder is deleted.
    #[serde(default)]
    recursive: bool,
    /// Why; shown to the host owner in approval prompts and the audit log.
    #[serde(default)]
    #[allow(dead_code)] // read by the gate from the raw arguments
    intent: Option<String>,
}

struct FilesDelete {
    spec: ToolSpec,
}

impl FilesDelete {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<DeleteParams>(
                "files_delete",
                Level::Admin,
                "Delete a file, link or folder on the host for good (not to the recycle bin). A folder needs \
                 recursive: true unless it is empty; a link is removed, never what it points to. Drive and file \
                 system roots and the host's own profile folder are refused. Usually needs the owner's approval.",
            ),
        }
    }
}

#[async_trait]
impl Tool for FilesDelete {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: DeleteParams = parse(args)?;
        let path = absolute(&p.path)?;
        let profile = ctx.host.profile().root().to_path_buf();
        let value = blocking(move || delete(&path, p.recursive, &profile)).await?;
        Ok(compact(value))
    }
}

fn delete(path: &Path, recursive: bool, profile: &Path) -> Result<Value> {
    let md = match fs::symlink_metadata(path) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("{} does not exist", shown(path)),
        Err(e) => return Err(anyhow!("cannot read {}: {e}", shown(path))),
    };
    let resolved = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if path.parent().is_none() || resolved.parent().is_none() {
        bail!("refusing to delete {}: it is the root of a drive or file system", shown(path));
    }
    let profile = dunce::canonicalize(profile).unwrap_or_else(|_| profile.to_path_buf());
    if !md.file_type().is_symlink() && profile.starts_with(&resolved) {
        bail!("refusing to delete {}: the ai-remote host keeps its identity and logs there", shown(path));
    }
    let ft = md.file_type();
    let removed = if ft.is_dir() {
        if recursive {
            fs::remove_dir_all(path)
        } else {
            fs::remove_dir(path).map_err(|e| {
                if fs::read_dir(path).is_ok_and(|mut d| d.next().is_some()) {
                    std::io::Error::other(
                        "the folder is not empty; pass recursive: true to delete it with its contents",
                    )
                } else {
                    e
                }
            })
        }
    } else if ft.is_symlink() && fs::metadata(path).is_ok_and(|t| t.is_dir()) && cfg!(windows) {
        // A directory link on Windows is removed like a folder.
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    removed.with_context(|| format!("cannot delete {}", shown(path)))?;
    Ok(json!({"path": shown(path), "deleted": true, "kind": kind(&ft)}))
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PushParams {
    /// Absolute path of the file to write on the host.
    path: String,
    /// Exact size of the file in bytes.
    size: u64,
    /// SHA-256 of the file's content (64 hex digits); the host checks it before the file is moved into
    /// place.
    sha256: String,
    /// Replace the file when it exists.
    #[serde(default)]
    overwrite: bool,
    /// Create missing parent folders.
    #[serde(default)]
    mkdir: bool,
    /// Why; shown to the host owner in approval prompts and the audit log.
    #[serde(default)]
    #[allow(dead_code)] // read by the gate from the raw arguments
    intent: Option<String>,
}

struct FilesPush {
    spec: ToolSpec,
}

impl FilesPush {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<PushParams>(
                "files_push",
                Level::Admin,
                "Approve writing a file on the host and get a one-time ticket for its bytes, which the controller \
                 sends on the files channel within 2 minutes (`ai-remote push` does both steps). The bytes go to a \
                 temporary file next to the target; size and SHA-256 are checked before it is moved into place. \
                 Usually needs the owner's approval.",
            ),
        }
    }
}

#[async_trait]
impl Tool for FilesPush {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: PushParams = parse(args)?;
        let path = absolute(&p.path)?;
        let sha256 = p.sha256.trim().to_ascii_lowercase();
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("sha256 must be the file's SHA-256 as 64 hex digits");
        }
        let (target, overwrite, mkdir) = (path.clone(), p.overwrite, p.mkdir);
        blocking(move || check_push_target(&target, overwrite, mkdir)).await?;
        let direction = Direction::Push { size: p.size, sha256: sha256.clone(), overwrite: p.overwrite };
        let ticket = ctx.host.transfers().issue(ctx, "files_push", path.clone(), direction);
        Ok(compact(json!({
            "ticket": ticket,
            "path": shown(&path),
            "size": p.size,
            "sha256": sha256,
            "expires_in_secs": TICKET_TTL.as_secs(),
        })))
    }
}

/// Refuses a push that could not land: no file name, a folder in the way, an existing file without
/// `overwrite`, or a missing parent folder without `mkdir` (which creates it).
fn check_push_target(path: &Path, overwrite: bool, mkdir: bool) -> Result<()> {
    if path.file_name().is_none() {
        bail!("{} names no file", shown(path));
    }
    match fs::metadata(path) {
        Ok(md) if md.is_dir() => bail!("{} is a folder; give the path of the file to write in it", shown(path)),
        Ok(_) if !overwrite => bail!("{} already exists; pass overwrite: true to replace it", shown(path)),
        _ => {}
    }
    let parent = path.parent().ok_or_else(|| anyhow!("{} has no parent folder", shown(path)))?;
    if !parent.is_dir() {
        if !mkdir {
            bail!("the folder {} does not exist; pass mkdir: true to create it", shown(parent));
        }
        fs::create_dir_all(parent).with_context(|| format!("cannot create {}", shown(parent)))?;
    }
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PullParams {
    /// Absolute path of the file to read on the host.
    path: String,
    /// Why; shown to the host owner in approval prompts and the audit log.
    #[serde(default)]
    #[allow(dead_code)] // read by the gate from the raw arguments
    intent: Option<String>,
}

struct FilesPull {
    spec: ToolSpec,
}

impl FilesPull {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<PullParams>(
                "files_pull",
                Level::Admin,
                "Approve reading a whole file from the host and get a one-time ticket, which the controller \
                 redeems on the files channel within 2 minutes to receive the bytes and their SHA-256 \
                 (`ai-remote pull` does both steps). Usually needs the owner's approval.",
            )
            .non_destructive(),
        }
    }
}

#[async_trait]
impl Tool for FilesPull {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: PullParams = parse(args)?;
        let path = absolute(&p.path)?;
        let target = path.clone();
        let (size, modified) = blocking(move || {
            // Opened now so a file that cannot be read is refused before anything moves.
            let file = fs::File::open(&target).with_context(|| format!("cannot read {}", shown(&target)))?;
            let md = file.metadata()?;
            if !md.is_file() {
                bail!("{} is not a file", shown(&target));
            }
            Ok((md.len(), millis(md.modified())))
        })
        .await?;
        let ticket = ctx.host.transfers().issue(ctx, "files_pull", path.clone(), Direction::Pull);
        Ok(compact(json!({
            "ticket": ticket,
            "path": shown(&path),
            "size": size,
            "modified": modified,
            "expires_in_secs": TICKET_TTL.as_secs(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(glob_match("*.log", "Host.LOG"));
        assert!(glob_match("a?c*", "abcdef"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("*.log", "host.log.1"));
        assert!(glob_match("*.log*", "host.log.1"));
        assert!(!glob_match("a?c", "ac"));
    }

    #[test]
    fn relative_paths_are_refused() {
        assert!(absolute("logs/host.log").is_err());
        assert!(absolute("  ").is_err());
        let abs = std::env::temp_dir();
        assert_eq!(absolute(&abs.display().to_string()).unwrap(), abs);
        let home = directories::BaseDirs::new().expect("a home directory").home_dir().to_path_buf();
        assert_eq!(absolute("~").unwrap(), home);
        assert_eq!(absolute("~/docs\\a.txt").unwrap(), home.join("docs").join("a.txt"));
        let desktop = directories::UserDirs::new().and_then(|d| d.desktop_dir().map(|p| p.to_path_buf()));
        assert_eq!(absolute("~/Desktop/a.txt").unwrap(), desktop.unwrap_or_else(|| home.join("Desktop")).join("a.txt"));
        assert!(absolute("~user/x").is_err());
    }

    #[test]
    fn listing_sorts_folders_first_and_pages() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b.txt", "A.log", "c.log"] {
            fs::write(dir.path().join(name), name).unwrap();
        }
        fs::create_dir(dir.path().join("zdir")).unwrap();
        let v = list(dir.path(), None, 0).unwrap();
        let names: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["zdir", "A.log", "b.txt", "c.log"]);
        assert_eq!(v["entries"][1]["size"], 5);
        assert_eq!(v["entries"][0]["kind"], "dir");
        assert!(v["next_offset"].is_null());
        let logs = list(dir.path(), Some("*.LOG"), 0).unwrap();
        assert_eq!(logs["total"], 2);
        let rest = list(dir.path(), None, 3).unwrap();
        assert_eq!(rest["entries"].as_array().unwrap().len(), 1);
        assert!(list(&dir.path().join("missing"), None, 0).is_err());
    }

    #[test]
    fn long_names_page_by_size() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..300 {
            fs::write(dir.path().join(format!("{i:03}-{}", "n".repeat(150))), "").unwrap();
        }
        let first = list(dir.path(), None, 0).unwrap();
        let n = first["entries"].as_array().unwrap().len();
        assert!(n < 300 && first.to_string().len() < budget_limit(), "{n} entries");
        assert_eq!(first["next_offset"], n);
        let second = list(dir.path(), None, n).unwrap();
        assert_eq!(second["offset"], n);
    }

    fn budget_limit() -> usize {
        ar_core::budget::TEXT_LIMIT
    }

    #[test]
    fn stat_reports_missing_paths_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(stat(&dir.path().join("nope"))["exists"], false);
        fs::write(dir.path().join("f"), "1234").unwrap();
        let v = stat(&dir.path().join("f"));
        assert_eq!((v["kind"].as_str(), v["size"].as_u64()), (Some("file"), Some(4)));
        assert_eq!(stat(dir.path())["kind"], "dir");
    }

    #[test]
    fn delete_guards_roots_profiles_and_full_folders() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        fs::create_dir_all(profile.join("host")).unwrap();
        let root = if cfg!(windows) { PathBuf::from("C:\\") } else { PathBuf::from("/") };
        assert!(delete(&root, true, &profile).unwrap_err().to_string().contains("root"));
        assert!(delete(&profile, true, &profile).unwrap_err().to_string().contains("ai-remote"));
        assert!(delete(dir.path(), true, &profile).unwrap_err().to_string().contains("ai-remote"));

        let full = dir.path().join("full");
        fs::create_dir(&full).unwrap();
        fs::write(full.join("x"), "x").unwrap();
        assert!(format!("{:#}", delete(&full, false, &profile).unwrap_err()).contains("recursive"));
        assert_eq!(delete(&full, true, &profile).unwrap()["kind"], "dir");
        assert!(!full.exists());
        assert!(delete(&full, true, &profile).unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn push_targets_are_checked_before_approval_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        check_push_target(&file, false, false).unwrap();
        fs::write(&file, "x").unwrap();
        assert!(check_push_target(&file, false, false).unwrap_err().to_string().contains("overwrite"));
        check_push_target(&file, true, false).unwrap();
        assert!(check_push_target(dir.path(), true, false).unwrap_err().to_string().contains("folder"));
        let deep = dir.path().join("new/sub/b.bin");
        assert!(check_push_target(&deep, false, false).unwrap_err().to_string().contains("mkdir"));
        check_push_target(&deep, false, true).unwrap();
        assert!(deep.parent().unwrap().is_dir());
    }
}
