//! Background jobs: commands that keep running after the call that started them returns, with
//! their output in a log on the host (lesson F9).
//!
//! - The process writes straight to `<jobs_dir>/<id>.log` (the host never pipes it), so a job
//!   keeps logging while the host restarts.
//! - Metadata is persisted per job in `<id>.json`, including the pid *and* the process start time
//!   (pids are reused), so a restarted host re-adopts jobs that still run and can report on and
//!   kill them. Without this, a restart leaves jobs running but unreachable.
//! - Jobs started by this host process run in a Job Object (Windows) or their own session and
//!   process group (Unix) via process-wrap, so `jobs_kill` takes down the whole tree, including
//!   processes the job left behind after its main process exited. Nothing kills a job when the
//!   session that started it ends or the host stops: outliving the call is the point of a job. On
//!   Windows the process breaks away from a job object the host itself may run in when that is
//!   allowed, because some launchers kill their whole tree when they exit.
//! - The Job Object does not survive the host process, so a re-adopted job's tree is found by
//!   walking parent links from the recorded process, matching start times so a reused pid is never
//!   mistaken for a job's process. Children of an already exited main process are found too;
//!   deeper orphans are not.
//! - A re-adopted job's exit code is unknown (the host that started it is gone).
//! - Limits: [`MAX_RUNNING`] jobs run at a time; the last [`MAX_KEPT`] jobs are kept and older
//!   finished ones are deleted with their logs.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::{Level, store};
use async_trait::async_trait;
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use super::{
    Tool, ToolSpec,
    command::{self, CommandSpec, EnvMap, Shell},
    parse,
};
use crate::session::ToolCtx;

/// Jobs that may run at the same time; more are refused.
pub const MAX_RUNNING: usize = 16;
/// Job records kept on disk; older finished jobs are deleted with their logs.
pub const MAX_KEPT: usize = 100;
/// Longest single `jobs_wait`.
const MAX_WAIT: Duration = Duration::from_secs(120);
/// Characters of log text returned per `jobs_log` call: stays below the gate's result budget, which
/// would otherwise cut the middle out of a chunk and break the offsets.
const LOG_CHAR_BUDGET: usize = 50_000;
/// How often a re-adopted job's process is checked while waiting.
const POLL: Duration = Duration::from_millis(500);

/// Who started a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Owner {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Running,
    Exited,
    Killed,
}

/// What `<id>.json` holds. The environment is not stored: it may carry secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JobMeta {
    id: String,
    command: String,
    shell: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    intent: Option<String>,
    owner: Owner,
    pid: u32,
    /// Process start time in seconds since the epoch, as the OS reports it; with `pid` it
    /// identifies the process across host restarts.
    process_start: u64,
    /// Milliseconds since the epoch.
    started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ended_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    state: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl JobMeta {
    /// `running` until the main process is seen to end, even when a kill is pending.
    fn shown_state(&self) -> JobState {
        if self.ended_at.is_none() { JobState::Running } else { self.state }
    }
}

type KillReply = oneshot::Sender<io::Result<()>>;

struct Job {
    meta: Mutex<JobMeta>,
    /// Becomes `true` when the job's main process has ended.
    done: watch::Sender<bool>,
    control: Control,
}

enum Control {
    /// Started by this host process: a reaper task owns the child and serves kill requests.
    Own(mpsc::Sender<KillReply>),
    /// Re-adopted after a host restart: liveness is checked on demand.
    Adopted,
}

impl Job {
    fn snapshot(&self) -> JobMeta {
        self.meta.lock().unwrap().clone()
    }

    fn id(&self) -> String {
        self.meta.lock().unwrap().id.clone()
    }

    /// Whether the main process has not been seen to end yet (a requested kill may be pending).
    fn running(&self) -> bool {
        self.meta.lock().unwrap().ended_at.is_none()
    }

    /// Records the end of the main process (a kill keeps its `killed` state).
    fn finish(&self, dir: &Path, exit_code: Option<i32>, note: Option<String>) {
        {
            let mut m = self.meta.lock().unwrap();
            if m.ended_at.is_some() {
                return;
            }
            if m.state == JobState::Running {
                m.state = JobState::Exited;
            }
            m.exit_code = exit_code;
            m.ended_at = Some(now_ms());
            if note.is_some() {
                m.note = note;
            }
            persist(dir, &m);
        }
        self.done.send_replace(true);
    }
}

/// Tracks the background jobs of this host.
pub struct JobManager {
    dir: PathBuf,
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
    /// Serializes starts so concurrent calls cannot exceed [`MAX_RUNNING`].
    starting: Mutex<()>,
}

/// A `jobs_start` request.
pub struct StartRequest<'a> {
    pub spec: CommandSpec<'a>,
    pub intent: Option<String>,
    pub owner: Owner,
}

/// One `jobs_log` chunk.
#[derive(Debug, Serialize)]
pub struct LogChunk {
    /// Byte offset the text starts at.
    pub offset: u64,
    /// Offset to pass to the next call to continue.
    pub next_offset: u64,
    /// Current size of the log in bytes.
    pub size: u64,
    #[serde(skip)]
    pub text: String,
}

impl JobManager {
    /// Loads the jobs recorded in `dir`, re-adopting those whose process still runs.
    pub fn new(dir: PathBuf) -> Self {
        let mut jobs = BTreeMap::new();
        for meta in load_all(&dir) {
            let running = meta.ended_at.is_none();
            let (done, _) = watch::channel(!running);
            let job = Arc::new(Job { meta: Mutex::new(meta), done, control: Control::Adopted });
            let id = job.snapshot().id;
            jobs.insert(id, job);
        }
        let manager = Self { dir, jobs: Mutex::new(jobs), starting: Mutex::new(()) };
        manager.refresh_adopted();
        manager
    }

    /// Starts a job. Must run inside the tokio runtime (it spawns the job's reaper task).
    pub fn start(&self, req: StartRequest<'_>) -> Result<Value> {
        let _one_at_a_time = self.starting.lock().unwrap();
        self.refresh_adopted();
        let running = self.jobs.lock().unwrap().values().filter(|j| j.running()).count();
        if running >= MAX_RUNNING {
            bail!("{running} jobs are already running (the limit is {MAX_RUNNING}); wait for one to finish or stop one with jobs_kill");
        }
        // Fail on a bad command before creating any file.
        command::build(&req.spec)?;
        let id = self.new_id();
        let log_path = self.log_path(&id);
        let log = fs::OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("create job log {}", log_path.display()))?;
        let make = || -> Result<tokio::process::Command> {
            let mut cmd = command::build(&req.spec)?;
            cmd.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log.try_clone()?);
            Ok(cmd)
        };
        let child = match spawn_job(make) {
            Ok(child) => child,
            Err(e) => {
                let _ = fs::remove_file(&log_path);
                return Err(e.context(format!("start {}", req.spec.shell.name())));
            }
        };
        let pid = child.id().ok_or_else(|| anyhow!("the job exited before its pid could be read"))?;
        let meta = JobMeta {
            id: id.clone(),
            command: req.spec.command.to_string(),
            shell: req.spec.shell.name().to_string(),
            cwd: req.spec.cwd.map(str::to_string),
            intent: req.intent,
            owner: req.owner,
            pid,
            process_start: process_start_time(pid).unwrap_or(0),
            started_at: now_ms(),
            ended_at: None,
            exit_code: None,
            state: JobState::Running,
            note: None,
        };
        persist(&self.dir, &meta);
        let (kill_tx, kill_rx) = mpsc::channel(4);
        let (done, _) = watch::channel(false);
        let job = Arc::new(Job { meta: Mutex::new(meta), done, control: Control::Own(kill_tx) });
        tokio::spawn(reap(Arc::downgrade(&job), child, kill_rx, self.dir.clone()));
        self.jobs.lock().unwrap().insert(id.clone(), job.clone());
        self.prune();
        Ok(json!({
            "id": id,
            "pid": pid,
            "log_path": display_path(&log_path),
            "started_at": job.snapshot().started_at,
        }))
    }

    /// Status of one job, with the last `tail_bytes` of its log.
    pub fn status(&self, id: &str, tail_bytes: u64) -> Result<Value> {
        let job = self.get(id)?;
        self.refresh_adopted();
        let mut v = self.describe(&job);
        if tail_bytes > 0 {
            let tail = read_log(&self.log_path(&job.id()), -(tail_bytes as i64), tail_bytes as usize, false)
                .map(|c| c.text)
                .unwrap_or_default();
            v["log_tail"] = json!(tail);
        }
        Ok(v)
    }

    /// Short status of every job, oldest first.
    pub fn list(&self) -> Value {
        self.refresh_adopted();
        let mut jobs: Vec<Arc<Job>> = self.jobs.lock().unwrap().values().cloned().collect();
        jobs.sort_by_key(|j| j.snapshot().started_at);
        let items: Vec<Value> = jobs
            .iter()
            .map(|j| {
                let m = j.snapshot();
                json!({
                    "id": m.id,
                    "state": m.shown_state(),
                    "exit_code": m.exit_code,
                    "command": ar_core::budget::head_tail(&m.command, 200, 95),
                    "started_at": m.started_at,
                    "duration_ms": m.ended_at.unwrap_or_else(now_ms).saturating_sub(m.started_at),
                    "owner": m.owner.name,
                })
            })
            .collect();
        let running = jobs.iter().filter(|j| j.running()).count();
        json!({ "running": running, "max_running": MAX_RUNNING, "jobs": items })
    }

    /// A chunk of a job's log; a negative `offset` counts from the end.
    pub fn log(&self, id: &str, offset: i64, max_bytes: usize) -> Result<(LogChunk, bool)> {
        let job = self.get(id)?;
        self.refresh_adopted();
        let running = job.running();
        let chunk = read_log(&self.log_path(&job.id()), offset, max_bytes, running)
            .with_context(|| format!("read the log of job {id}"))?;
        Ok((chunk, running))
    }

    /// Waits up to `timeout` for a job to end; returns whether it has.
    pub async fn wait(&self, id: &str, timeout: Duration) -> Result<bool> {
        let job = self.get(id)?;
        let mut done = job.done.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            self.refresh_adopted();
            if !job.running() {
                return Ok(true);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            tokio::select! {
                r = done.changed() => if r.is_err() { tokio::time::sleep(remaining.min(POLL)).await },
                _ = tokio::time::sleep(remaining.min(POLL)) => {}
            }
        }
    }

    /// Kills a job's whole process tree and waits briefly for it to end. Returns a note for the
    /// caller.
    pub async fn kill(&self, id: &str) -> Result<String> {
        let job = self.get(id)?;
        self.refresh_adopted();
        let was_running = job.running();
        if was_running {
            let mut m = job.meta.lock().unwrap();
            m.state = JobState::Killed;
            persist(&self.dir, &m);
        }
        match &job.control {
            Control::Own(requests) => {
                let (reply, answer) = oneshot::channel();
                requests.send(reply).await.map_err(|_| anyhow!("job {id} is no longer tracked"))?;
                let result = answer.await.map_err(|_| anyhow!("job {id} is no longer tracked"))?;
                if let Err(e) = result {
                    // Killing a tree that has already emptied is not an error.
                    if was_running {
                        return Err(anyhow!(e).context(format!("kill job {id}")));
                    }
                }
            }
            Control::Adopted => {
                let m = job.snapshot();
                kill_tree(m.pid, m.process_start);
            }
        }
        if !was_running {
            return Ok(match job.control {
                Control::Own(_) => "the job had already exited; processes it left running, if any, were killed".into(),
                Control::Adopted => "the job had already exited; children it left running, if any, were killed".into(),
            });
        }
        if self.wait(id, Duration::from_secs(5)).await? {
            Ok("killed the job and every process it started".into())
        } else {
            Ok("asked the OS to kill the job, but its main process has not ended within 5 s".into())
        }
    }

    fn get(&self, id: &str) -> Result<Arc<Job>> {
        self.jobs.lock().unwrap().get(id.trim()).cloned().ok_or_else(|| {
            anyhow!("no job with id '{id}' on this host; call jobs_status without an id to list the jobs")
        })
    }

    fn describe(&self, job: &Job) -> Value {
        let m = job.snapshot();
        let log_path = self.log_path(&m.id);
        let log_bytes = fs::metadata(&log_path).map(|md| md.len()).unwrap_or(0);
        json!({
            "id": m.id,
            "state": m.shown_state(),
            "running": m.ended_at.is_none(),
            "exit_code": m.exit_code,
            "command": m.command,
            "shell": m.shell,
            "cwd": m.cwd,
            "pid": m.pid,
            "started_at": m.started_at,
            "ended_at": m.ended_at,
            "duration_ms": m.ended_at.unwrap_or_else(now_ms).saturating_sub(m.started_at),
            "owner": m.owner,
            "intent": m.intent,
            "adopted": matches!(job.control, Control::Adopted),
            "note": m.note,
            "log_path": display_path(&log_path),
            "log_bytes": log_bytes,
        })
    }

    /// Marks re-adopted jobs whose process is gone as ended.
    fn refresh_adopted(&self) {
        let candidates: Vec<Arc<Job>> = self
            .jobs
            .lock()
            .unwrap()
            .values()
            .filter(|j| matches!(j.control, Control::Adopted) && j.running())
            .cloned()
            .collect();
        if candidates.is_empty() {
            return;
        }
        let pids: Vec<sysinfo::Pid> = candidates.iter().map(|j| sysinfo::Pid::from_u32(j.snapshot().pid)).collect();
        let mut sys = sysinfo::System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&pids),
            true,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        for job in candidates {
            let m = job.snapshot();
            let alive = sys
                .process(sysinfo::Pid::from_u32(m.pid))
                .is_some_and(|p| same_start(p.start_time(), m.process_start));
            if !alive {
                job.finish(
                    &self.dir,
                    None,
                    Some("the host restarted while this job ran, so its exit code is unknown".into()),
                );
            }
        }
    }

    /// Deletes the oldest finished jobs beyond [`MAX_KEPT`].
    fn prune(&self) {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.len() <= MAX_KEPT {
            return;
        }
        let mut finished: Vec<(u64, String)> =
            jobs.values().filter(|j| !j.running()).map(|j| j.snapshot()).map(|m| (m.started_at, m.id)).collect();
        finished.sort();
        let excess = jobs.len() - MAX_KEPT;
        for (_, id) in finished.into_iter().take(excess) {
            jobs.remove(&id);
            let _ = fs::remove_file(self.dir.join(format!("{id}.json")));
            let _ = fs::remove_file(self.log_path(&id));
        }
    }

    fn new_id(&self) -> String {
        let jobs = self.jobs.lock().unwrap();
        loop {
            let mut b = [0u8; 4];
            let _ = getrandom::fill(&mut b);
            let id = format!("{:08x}", u32::from_le_bytes(b));
            if !jobs.contains_key(&id) && !self.log_path(&id).exists() {
                return id;
            }
        }
    }

    fn log_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.log"))
    }
}

/// Waits for a job's main process, records its exit, and serves kill requests (also afterwards:
/// the main process may have left children running in its Job Object or process group). Returns
/// without killing anything when the manager is dropped, which is what happens when the host stops.
async fn reap(job: Weak<Job>, mut child: Box<dyn ChildWrapper>, mut kills: mpsc::Receiver<KillReply>, dir: PathBuf) {
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            request = kills.recv() => match request {
                Some(reply) => { let _ = reply.send(child.start_kill()); }
                None => return,
            },
        }
    };
    if let Some(job) = job.upgrade() {
        let (code, note) = match status {
            Ok(s) => (s.code(), None),
            Err(e) => (None, Some(format!("could not read the exit status: {e}"))),
        };
        job.finish(&dir, code, note);
    }
    while let Some(reply) = kills.recv().await {
        let _ = reply.send(child.start_kill());
    }
}

/// Spawns a job's process in its own Job Object, without a console window, breaking away from the
/// host's own job object when that job allows it.
#[cfg(windows)]
fn spawn_job(make: impl Fn() -> Result<tokio::process::Command>) -> Result<Box<dyn ChildWrapper>> {
    use process_wrap::tokio::{CreationFlags, JobObject};
    use windows::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, PROCESS_CREATION_FLAGS};
    let spawn = |flags: PROCESS_CREATION_FLAGS| -> Result<io::Result<Box<dyn ChildWrapper>>> {
        let mut wrap = CommandWrap::from(make()?);
        wrap.wrap(CreationFlags(flags));
        wrap.wrap(JobObject);
        Ok(wrap.spawn())
    };
    const ERROR_ACCESS_DENIED: i32 = 5;
    match spawn(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)? {
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => Ok(spawn(CREATE_NO_WINDOW)??),
        other => Ok(other?),
    }
}

/// Spawns a job's process as the leader of a new session (and process group), detached from the
/// host's terminal.
#[cfg(unix)]
fn spawn_job(make: impl Fn() -> Result<tokio::process::Command>) -> Result<Box<dyn ChildWrapper>> {
    let mut wrap = CommandWrap::from(make()?);
    wrap.wrap(process_wrap::tokio::ProcessSession);
    Ok(wrap.spawn()?)
}

/// Kills a re-adopted job's process and its descendants. A process belongs to the tree when its
/// parent does and it started after that parent, and before any later process that reused the
/// parent's pid; the root counts even when it has already exited, so its orphans are found.
fn kill_tree(root_pid: u32, root_start: u64) -> usize {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let procs = sys.processes();
    let nodes: Vec<(u32, Option<u32>, u64)> =
        procs.values().map(|p| (p.pid().as_u32(), p.parent().map(Pid::as_u32), p.start_time())).collect();
    let victims = tree_members(&nodes, root_pid, root_start);
    let mut killed = 0;
    for pid in victims {
        if procs.get(&Pid::from_u32(pid)).is_some_and(|p| p.kill()) {
            killed += 1;
        }
    }
    killed
}

/// The live members of the tree rooted at (`root_pid`, `root_start`), given `(pid, parent, start)`
/// of every live process. Pure, so the pid-reuse rules are testable.
fn tree_members(nodes: &[(u32, Option<u32>, u64)], root_pid: u32, root_start: u64) -> Vec<u32> {
    let live_start = |pid: u32| nodes.iter().find(|n| n.0 == pid).map(|n| n.2);
    let mut members = Vec::new();
    let mut queue = vec![(root_pid, root_start)];
    let mut seen = std::collections::HashSet::new();
    while let Some((pid, start)) = queue.pop() {
        if !seen.insert(pid) {
            continue;
        }
        // A live process with this pid but another start time is someone else.
        let reused_at = live_start(pid).filter(|s| !same_start(*s, start));
        if live_start(pid).is_some() && reused_at.is_none() {
            members.push(pid);
        }
        for &(child, parent, child_start) in nodes {
            if parent == Some(pid)
                && child != pid
                && child_start >= start
                && reused_at.is_none_or(|r| child_start < r)
            {
                queue.push((child, child_start));
            }
        }
    }
    members
}

fn process_start_time(pid: u32) -> Option<u64> {
    let mut sys = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    sys.process(pid).map(|p| p.start_time())
}

/// Start times are whole seconds; allow one second of rounding difference between readings.
fn same_start(a: u64, b: u64) -> bool {
    a.abs_diff(b) <= 1
}

fn persist(dir: &Path, meta: &JobMeta) {
    let path = dir.join(format!("{}.json", meta.id));
    if let Err(e) = store::write_json_atomic(&path, meta) {
        tracing::warn!(%e, "cannot save job metadata");
    }
}

fn load_all(dir: &Path) -> Vec<JobMeta> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match fs::read(&path).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice::<JobMeta>(&b)?)) {
            Ok(meta) => out.push(meta),
            Err(e) => tracing::warn!(path = %path.display(), %e, "ignoring unreadable job metadata"),
        }
    }
    out
}

/// Reads up to `max_bytes` of a log from `offset` (negative: from the end), on UTF-8 character
/// boundaries: a chunk never starts inside a character, and while more output may follow
/// (`more_coming`, or the read stopped before the end) it never ends inside one either.
fn read_log(path: &Path, offset: i64, max_bytes: usize, more_coming: bool) -> io::Result<LogChunk> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(LogChunk { offset: 0, next_offset: 0, size: 0, text: String::new() });
        }
        Err(e) => return Err(e),
    };
    let size = file.metadata()?.len();
    let start = if offset < 0 { size.saturating_sub(offset.unsigned_abs()) } else { (offset as u64).min(size) };
    let want = (size - start).min(max_bytes as u64) as usize;
    let mut buf = vec![0u8; want];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut buf)?;
    let (skip, len) = utf8_window(&buf, start > 0, more_coming || start + (want as u64) < size);
    let len = fit_chars(&buf[skip..skip + len], LOG_CHAR_BUDGET);
    let text = command::decode(&buf[skip..skip + len]);
    Ok(LogChunk { offset: start + skip as u64, next_offset: start + (skip + len) as u64, size, text })
}

/// `(skip, len)`: bytes to skip at the start (continuation bytes of a character that began
/// earlier, when `mid_file`) and the length to keep (without a trailing incomplete character,
/// when `hold_tail`).
fn utf8_window(buf: &[u8], mid_file: bool, hold_tail: bool) -> (usize, usize) {
    let is_cont = |b: u8| b & 0xC0 == 0x80;
    let skip = if mid_file { buf.iter().take(3).take_while(|b| is_cont(**b)).count() } else { 0 };
    let body = &buf[skip..];
    let mut len = body.len();
    if hold_tail {
        // Find the last lead byte within the final three bytes and check its sequence is complete.
        for back in 1..=body.len().min(4) {
            let b = body[body.len() - back];
            if is_cont(b) {
                continue;
            }
            let need = match b {
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                _ => 1,
            };
            if need > back {
                len = body.len() - back;
            }
            break;
        }
    }
    (skip, len)
}

/// Length of the longest prefix of `bytes` that decodes to at most `max_chars` characters (exact for
/// UTF-8; legacy code pages never produce more characters than bytes).
fn fit_chars(bytes: &[u8], max_chars: usize) -> usize {
    if bytes.len() <= max_chars {
        return bytes.len();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.char_indices().nth(max_chars).map_or(bytes.len(), |(i, _)| i),
        Err(_) => max_chars,
    }
}

/// A path without the `\\?\` prefix that canonicalized Windows paths carry, so it can be pasted
/// into any tool (cmd and many programs reject that form).
fn display_path(path: &Path) -> String {
    let s = path.display().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => s,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------
// Tools

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(JobsStart::new()),
        Arc::new(JobsStatus::new()),
        Arc::new(JobsLog::new()),
        Arc::new(JobsWait::new()),
        Arc::new(JobsKill::new()),
    ]
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StartParams {
    /// The command line to run. PowerShell and sh accept multi-line scripts; cmd takes one line.
    command: String,
    /// `powershell` (Windows default), `pwsh`, `cmd`, or `sh` (default elsewhere), `bash`, `zsh`.
    #[serde(default)]
    shell: Option<String>,
    /// Working directory.
    #[serde(default)]
    cwd: Option<String>,
    /// Environment changes for this job: a string sets the variable, null removes it.
    #[serde(default)]
    env: Option<EnvMap>,
    /// Why you are starting this; shown to the host owner in approval prompts and the audit log.
    #[serde(default)]
    intent: Option<String>,
}

struct JobsStart {
    spec: ToolSpec,
}

impl JobsStart {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<StartParams>(
                "jobs_start",
                Level::Admin,
                "Start a command in the background on the host and return its job id at once. Its stdout and \
                 stderr go to a log on the host (UTF-8); follow it with jobs_log, jobs_wait or jobs_status. Use \
                 this for builds, servers, GUI apps and anything that may take over ~30 s. A job keeps running \
                 after your session ends and while the host restarts, until it exits or jobs_kill stops it. At \
                 most 16 jobs run at once. Usually needs the owner's approval.",
            )
            .ask(),
        }
    }
}

#[async_trait]
impl Tool for JobsStart {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: StartParams = parse(args)?;
        let spec = CommandSpec {
            command: &p.command,
            shell: Shell::parse(p.shell.as_deref())?,
            cwd: p.cwd.as_deref(),
            env: p.env.as_ref(),
        };
        let owner = Owner { name: ctx.session.peer.name.clone(), principal: ctx.session.peer.principal.clone() };
        let started = ctx.host.jobs().start(StartRequest { spec, intent: p.intent, owner })?;
        Ok(super::json(started))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StatusParams {
    /// Job id; omit to list every job.
    #[serde(default)]
    id: Option<String>,
    /// Bytes of the log's end to include for one job (default 4096, max 65536; 0 for none).
    #[serde(default)]
    tail_bytes: Option<u64>,
}

struct JobsStatus {
    spec: ToolSpec,
}

impl JobsStatus {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<StatusParams>(
                "jobs_status",
                Level::Admin,
                "Status of a background job (state running/exited/killed, exit code, start and end time in Unix \
                 ms, duration, owner, and the end of its log), or a short list of every job when no id is given.",
            )
            .allow()
            .read_only(),
        }
    }
}

#[async_trait]
impl Tool for JobsStatus {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: StatusParams = parse(args)?;
        let jobs = ctx.host.jobs();
        Ok(super::json(match p.id {
            Some(id) => jobs.status(&id, p.tail_bytes.unwrap_or(4096).min(65_536))?,
            None => jobs.list(),
        }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LogParams {
    /// Job id.
    id: String,
    /// Byte offset to read from; negative counts from the end (-4096 = the last 4 KiB). Default 0.
    /// Pass the previous call's next_offset to continue.
    #[serde(default)]
    offset: Option<i64>,
    /// Most bytes to return (default 65536, max 1048576); at most about 50,000 characters are
    /// returned per call regardless.
    #[serde(default)]
    max_bytes: Option<usize>,
}

struct JobsLog {
    spec: ToolSpec,
}

impl JobsLog {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<LogParams>(
                "jobs_log",
                Level::Admin,
                "Read a background job's log (stdout and stderr together) from a byte offset. Returns a JSON line \
                 (offset, next_offset, size, running, eof) followed by the text. Loop with offset = next_offset to \
                 follow a running job.",
            )
            .allow()
            .read_only(),
        }
    }
}

#[async_trait]
impl Tool for JobsLog {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: LogParams = parse(args)?;
        let max_bytes = p.max_bytes.unwrap_or(65_536).clamp(1, 1 << 20);
        let (chunk, running) = ctx.host.jobs().log(&p.id, p.offset.unwrap_or(0), max_bytes)?;
        let header = json!({
            "id": p.id.trim(),
            "offset": chunk.offset,
            "next_offset": chunk.next_offset,
            "size": chunk.size,
            "eof": chunk.next_offset >= chunk.size,
            "running": running,
        });
        let mut content = vec![ContentBlock::text(header.to_string())];
        if !chunk.text.is_empty() {
            content.push(ContentBlock::text(chunk.text));
        }
        Ok(CallToolResult::success(content))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WaitParams {
    /// Job id.
    id: String,
    /// Seconds to wait for the job to end (default 30, max 120).
    #[serde(default)]
    timeout_secs: Option<u64>,
}

struct JobsWait {
    spec: ToolSpec,
}

impl JobsWait {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<WaitParams>(
                "jobs_wait",
                Level::Admin,
                "Wait up to timeout_secs (max 120) for a background job to end, then return its status like \
                 jobs_status. If it is still running, call jobs_wait again.",
            )
            .allow()
            .read_only(),
        }
    }
}

#[async_trait]
impl Tool for JobsWait {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: WaitParams = parse(args)?;
        let requested = Duration::from_secs(p.timeout_secs.unwrap_or(30));
        let timeout = requested.min(MAX_WAIT);
        let jobs = ctx.host.jobs();
        let ended = jobs.wait(&p.id, timeout).await?;
        let mut v = jobs.status(&p.id, 4096)?;
        if !ended {
            v["still_running"] = json!(true);
            v["hint"] = json!(if requested > MAX_WAIT {
                format!("waits are limited to {} s per call; call jobs_wait again to keep waiting", MAX_WAIT.as_secs())
            } else {
                "the job is still running; call jobs_wait again to keep waiting".to_string()
            });
            if requested > MAX_WAIT {
                v["clamped_timeout_secs"] = json!(MAX_WAIT.as_secs());
            }
        }
        Ok(super::json(v))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct KillParams {
    /// Job id.
    id: String,
    /// Why you are stopping it; shown in the audit log.
    #[serde(default)]
    #[allow(dead_code)] // read by the gate from the raw arguments
    intent: Option<String>,
}

struct JobsKill {
    spec: ToolSpec,
}

impl JobsKill {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<KillParams>(
                "jobs_kill",
                Level::Admin,
                "Stop a background job: kills its process and every process it started (also those it left \
                 running after it exited), then returns its status. Only affects jobs started with jobs_start.",
            )
            .allow(),
        }
    }
}

#[async_trait]
impl Tool for JobsKill {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: KillParams = parse(args)?;
        let jobs = ctx.host.jobs();
        let note = jobs.kill(&p.id).await?;
        let mut v = jobs.status(&p.id, 1024)?;
        v["result"] = json!(note);
        Ok(super::json(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_window_skips_and_holds_partial_characters() {
        let s = "a世b".as_bytes(); // 61 e4 b8 96 62
        assert_eq!(utf8_window(s, false, true), (0, 5));
        // Starting inside 世: its two continuation bytes are skipped.
        assert_eq!(utf8_window(&s[2..], true, true), (2, 1));
        // Ending inside 世: held back while more may follow, kept otherwise.
        assert_eq!(utf8_window(&s[..3], false, true), (0, 1));
        assert_eq!(utf8_window(&s[..3], false, false), (0, 3));
        assert_eq!(utf8_window(&s[..4], false, true), (0, 4));
        assert_eq!(utf8_window(b"", true, true), (0, 0));
    }

    #[test]
    fn only_starting_a_job_asks_by_default() {
        use ar_core::config::Action;
        for tool in tools() {
            let spec = tool.spec();
            let expected = if spec.name == "jobs_start" { Action::Ask } else { Action::Allow };
            assert_eq!((spec.level, spec.default_action), (Level::Admin, expected), "{}", spec.name);
        }
    }

    #[test]
    fn builtin_tool_names_are_valid_for_model_apis() {
        // ^[a-zA-Z0-9_-]{1,64}$, the limit model APIs put on tool names.
        for tool in super::super::Registry::builtin().iter() {
            let name = &tool.spec().name;
            let ok = (1..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
            assert!(ok, "invalid tool name '{name}'");
        }
    }

    #[test]
    fn display_path_drops_the_verbatim_prefix() {
        assert_eq!(display_path(Path::new(r"\\?\C:\x\a.log")), r"C:\x\a.log");
        assert_eq!(display_path(Path::new(r"\\?\UNC\srv\share\a.log")), r"\\?\UNC\srv\share\a.log");
        assert_eq!(display_path(Path::new("/tmp/a.log")), "/tmp/a.log");
    }

    #[test]
    fn fit_chars_cuts_on_character_boundaries() {
        assert_eq!(fit_chars(b"abc", 5), 3);
        assert_eq!(fit_chars("世世世".as_bytes(), 2), 6);
        assert_eq!(fit_chars("ab世世".as_bytes(), 3), 5);
    }

    #[test]
    fn tree_members_respect_pid_reuse() {
        // root 10 (start 100) -> 11 (start 101) -> 12 (start 102); 13 is a child of an *older*
        // process that had pid 10 (started before the root); 20 reuses pid 21's parent slot.
        let nodes = vec![
            (10, Some(1), 100),
            (11, Some(10), 101),
            (12, Some(11), 102),
            (13, Some(10), 50),
            (30, Some(2), 10),
        ];
        let mut m = tree_members(&nodes, 10, 100);
        m.sort();
        assert_eq!(m, [10, 11, 12]);
    }

    #[test]
    fn tree_members_find_orphans_of_an_exited_root() {
        // Root 10 exited; its child 11 still runs; pid 10 was reused at t=200 by an unrelated
        // process whose child 14 must not be killed.
        let nodes = vec![(11, Some(10), 101), (10, Some(3), 200), (14, Some(10), 201)];
        assert_eq!(tree_members(&nodes, 10, 100), [11]);
        // Without reuse, the orphan is still found.
        assert_eq!(tree_members(&[(11, Some(10), 101)], 10, 100), [11]);
    }

    #[test]
    fn read_log_offsets() {
        let dir = std::env::temp_dir().join(format!("ar-jobs-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.log");
        fs::write(&path, "hello 世界\n").unwrap(); // 13 bytes
        let all = read_log(&path, 0, 1000, false).unwrap();
        assert_eq!((all.offset, all.next_offset, all.size, all.text.as_str()), (0, 13, 13, "hello 世界\n"));
        let tail = read_log(&path, -4, 1000, false).unwrap();
        assert_eq!((tail.offset, tail.text.as_str()), (9, "界\n"), "starts on a character boundary");
        let part = read_log(&path, 0, 8, true).unwrap();
        assert_eq!((part.next_offset, part.text.as_str()), (6, "hello "), "never ends inside 世");
        let next = read_log(&path, part.next_offset as i64, 1000, false).unwrap();
        assert_eq!(next.text, "世界\n");
        let past = read_log(&path, 999, 10, false).unwrap();
        assert_eq!((past.offset, past.next_offset, past.text.as_str()), (13, 13, ""));
        assert_eq!(read_log(&dir.join("missing.log"), 0, 10, false).unwrap().size, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    fn owner() -> Owner {
        Owner { name: "test".into(), principal: Some("eid:test".into()) }
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ar-jobs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn start(m: &JobManager, command: &str) -> String {
        let spec = CommandSpec { command, shell: Shell::parse(None).unwrap(), cwd: None, env: None };
        let v = m.start(StartRequest { spec, intent: Some("test".into()), owner: owner() }).unwrap();
        v["id"].as_str().unwrap().to_string()
    }

    fn alive(pid: u32) -> bool {
        process_start_time(pid).is_some()
    }

    #[tokio::test]
    async fn job_runs_logs_and_reports_exit() {
        let dir = test_dir("run");
        let m = JobManager::new(dir.clone());
        let cmd = if cfg!(windows) { "Write-Output 'line 1 世界'; [Console]::Error.WriteLine('err'); exit 7" } else { "echo 'line 1 世界'; echo err >&2; exit 7" };
        let id = start(&m, cmd);
        assert!(m.wait(&id, Duration::from_secs(60)).await.unwrap(), "the job should end");
        let s = m.status(&id, 4096).unwrap();
        assert_eq!(s["state"], "exited");
        assert_eq!(s["exit_code"], 7);
        let tail = s["log_tail"].as_str().unwrap();
        assert!(tail.contains("line 1 世界") && tail.contains("err"), "{tail}");
        // Metadata survives on disk.
        let saved: JobMeta = serde_json::from_slice(&fs::read(dir.join(format!("{id}.json"))).unwrap()).unwrap();
        assert_eq!((saved.state, saved.exit_code), (JobState::Exited, Some(7)));
        assert_eq!(m.list()["jobs"].as_array().unwrap().len(), 1);
        let err = m.status("nope", 0).unwrap_err().to_string();
        assert!(err.contains("no job with id 'nope'"), "{err}");
        drop(m);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A job that records its own pid and a sleeping child's pid, then sleeps.
    fn tree_job() -> &'static str {
        if cfg!(windows) {
            "$c = Start-Process -PassThru -NoNewWindow ping -ArgumentList '-n','300','127.0.0.1'; \
             Write-Output \"PIDS $PID $($c.Id)\"; Start-Sleep 300"
        } else {
            "sleep 300 & echo \"PIDS $$ $!\"; sleep 300"
        }
    }

    async fn job_pids(m: &JobManager, id: &str) -> Vec<u32> {
        for _ in 0..150 {
            let (chunk, _) = m.log(id, 0, 65_536).unwrap();
            if let Some(line) = chunk.text.lines().find(|l| l.starts_with("PIDS ")) {
                return line.split_whitespace().skip(1).filter_map(|p| p.parse().ok()).collect();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the job did not print its pids");
    }

    async fn assert_gone(pids: &[u32]) {
        for _ in 0..50 {
            if pids.iter().all(|p| !alive(*p)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("still alive: {:?}", pids.iter().filter(|p| alive(**p)).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn kill_takes_down_the_tree() {
        let dir = test_dir("kill");
        let m = JobManager::new(dir.clone());
        let id = start(&m, tree_job());
        let pids = job_pids(&m, &id).await;
        assert!(!m.wait(&id, Duration::from_millis(300)).await.unwrap(), "still running");
        let note = m.kill(&id).await.unwrap();
        assert!(note.starts_with("killed"), "{note}");
        let s = m.status(&id, 0).unwrap();
        assert_eq!(s["state"], "killed", "{s}");
        assert!(!s["ended_at"].is_null(), "the kill waits for the process to end: {s}");
        assert_gone(&pids).await;
        drop(m);
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn restarted_manager_readopts_and_kills_leftovers() {
        let dir = test_dir("adopt");
        let first = JobManager::new(dir.clone());
        let id = start(&first, tree_job());
        let pids = job_pids(&first, &id).await;
        // What a host restart looks like to the jobs: the manager goes away, the processes stay.
        drop(first);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(pids.iter().all(|p| alive(*p)), "jobs outlive the manager");

        let second = JobManager::new(dir.clone());
        let s = second.status(&id, 0).unwrap();
        assert_eq!((s["state"].as_str(), s["adopted"].as_bool()), (Some("running"), Some(true)), "{s}");
        let note = second.kill(&id).await.unwrap();
        assert!(note.starts_with("killed"), "{note}");
        assert_gone(&pids).await;
        let s = second.status(&id, 0).unwrap();
        assert_eq!(s["state"], "killed", "{s}");
        assert!(!s["ended_at"].is_null(), "the kill waits for the process to end: {s}");
        drop(second);

        // A third start sees the job as finished, not running.
        let third = JobManager::new(dir.clone());
        assert_eq!(third.status(&id, 0).unwrap()["state"], "killed");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn readopted_job_that_ended_is_reported_without_exit_code() {
        let dir = test_dir("gone");
        let first = JobManager::new(dir.clone());
        let id = start(&first, tree_job());
        let pids = job_pids(&first, &id).await;
        drop(first);
        // The job ends while no manager is watching.
        kill_tree(pids[0], process_start_time(pids[0]).unwrap());
        assert_gone(&pids).await;
        let second = JobManager::new(dir.clone());
        let s = second.status(&id, 0).unwrap();
        assert_eq!(s["state"], "exited", "{s}");
        assert!(s["exit_code"].is_null());
        assert!(s["note"].as_str().unwrap().contains("exit code is unknown"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn running_jobs_are_limited() {
        let dir = test_dir("limit");
        let m = JobManager::new(dir.clone());
        let sleeper = if cfg!(windows) { "Start-Sleep 120" } else { "sleep 120" };
        let ids: Vec<String> = (0..MAX_RUNNING).map(|_| start(&m, sleeper)).collect();
        let spec = CommandSpec { command: sleeper, shell: Shell::parse(None).unwrap(), cwd: None, env: None };
        let err = m.start(StartRequest { spec, intent: None, owner: owner() }).unwrap_err().to_string();
        assert!(err.contains("already running"), "{err}");
        for id in &ids {
            m.kill(id).await.unwrap();
        }
        drop(m);
        let _ = fs::remove_dir_all(&dir);
    }
}
