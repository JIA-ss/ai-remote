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
//!   Windows the process breaks away from a job object the host itself may run in when every job
//!   up the chain allows it, because some launchers kill their whole tree when they exit. A host
//!   started at logon runs under Task Scheduler, whose job object blocks breakaway, so there a job
//!   ends when the host process does (the supervisor's job is killed on close); a host started from
//!   a terminal usually lets jobs survive its restart.
//! - A job's state follows its main process. Processes it leaves running afterwards (launchers such
//!   as `start` or `Start-Process` do that) are reported as `leftover_processes`, take a running
//!   slot and keep the job from being pruned, so they can neither get around [`MAX_RUNNING`] nor
//!   become unreachable.
//! - Without the Job Object (after a host restart, and to count leftovers) a job's processes are
//!   looked up in the process table: the main process, processes a member started (parent link and
//!   a later start time) and, on Unix, members of its session (orphans move to init but keep their
//!   session). Processes are matched by pid *and* start time, and once the main process has ended
//!   only processes started while it was known to run count ([`JobMeta::alive_until`]), so a
//!   reused pid never pulls an unrelated process into a job. Not found this way: on Windows,
//!   processes whose parent exited before the lookup (deeper orphans); on Unix, processes that
//!   left the session.
//! - A re-adopted job's exit code is unknown (the host that started it is gone).
//! - Jobs are shared: every admin-level device sees, reads and stops every job; the owner is
//!   recorded, not enforced. Policy rules can restrict `jobs_log` and `jobs_kill` per device.
//! - Process-table scans, process creation and file access run on the blocking pool.
//! - Limits: [`MAX_RUNNING`] jobs run at a time; the last [`MAX_KEPT`] jobs are kept and older
//!   finished ones are deleted with their logs.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::{Level, budget, store};
use async_trait::async_trait;
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::{mpsc, oneshot, watch};

use super::{
    Tool, ToolSpec,
    command::{self, CommandSpec, EnvMap, Shell},
    parse,
};
use crate::session::ToolCtx;

/// Jobs that may run at the same time (counting jobs whose leftovers still run); more are refused.
pub const MAX_RUNNING: usize = 16;
/// Job records kept on disk; older finished jobs are deleted with their logs.
pub const MAX_KEPT: usize = 100;
/// Longest single `jobs_wait`.
const MAX_WAIT: Duration = Duration::from_secs(120);
/// Characters of log text returned per call: stays below the gate's result budget, which would
/// otherwise cut the middle out of a chunk and break the offsets.
const LOG_CHAR_BUDGET: usize = 50_000;
/// Smallest `jobs_log` read: room for any UTF-8 character, so every call makes progress.
const MIN_LOG_BYTES: usize = 4;
/// How often a re-adopted job's process is checked while waiting.
const POLL: Duration = Duration::from_millis(500);
/// How often the time a running job's main process was last seen alive is saved.
const ALIVE_SAVE_EVERY: Duration = Duration::from_secs(30);

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
    /// The latest time (ms since the epoch) the main process is known to have been running: its
    /// end when a host saw it end, otherwise when it was last seen alive. Nothing started later can
    /// be its child, which keeps a later process with the same pid from pulling its own children
    /// into the job. Absent in records of older versions: then nothing is attributed to an ended job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alive_until: Option<u64>,
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

    /// The main process, as the process table identifies it.
    fn root(&self) -> Root {
        Root { pid: self.pid, start: self.process_start, alive_until: self.alive_until.map(|ms| ms / 1000) }
    }
}

type KillReply = oneshot::Sender<io::Result<()>>;

struct Job {
    meta: Mutex<JobMeta>,
    /// Becomes `true` when the job's main process has ended.
    done: watch::Sender<bool>,
    control: Control,
    /// `alive_until` as last saved, so a job seen alive on every poll is not rewritten each time.
    saved_alive: AtomicU64,
    /// Set once the main process has ended and none of the job's processes is left. Nothing can
    /// join the job after that, so the process table need not be searched for it again.
    settled: AtomicBool,
}

enum Control {
    /// Started by this host process: a reaper task owns the child and serves kill requests.
    Own(mpsc::Sender<KillReply>),
    /// Re-adopted after a host restart: liveness is checked on demand.
    Adopted,
}

impl Job {
    fn new(meta: JobMeta, control: Control) -> Arc<Self> {
        let (done, _) = watch::channel(meta.ended_at.is_some());
        let saved_alive = AtomicU64::new(meta.alive_until.unwrap_or(0));
        Arc::new(Self { meta: Mutex::new(meta), done, control, saved_alive, settled: AtomicBool::new(false) })
    }

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

    fn adopted(&self) -> bool {
        matches!(self.control, Control::Adopted)
    }

    /// Records that the main process runs now; saved when `save` or when the saved time is older
    /// than [`ALIVE_SAVE_EVERY`].
    fn seen_alive(&self, dir: &Path, save: bool) {
        let now = now_ms();
        let mut m = self.meta.lock().unwrap();
        if m.ended_at.is_some() {
            return;
        }
        m.alive_until = Some(now);
        let due = now.saturating_sub(self.saved_alive.load(Ordering::Relaxed)) >= ALIVE_SAVE_EVERY.as_millis() as u64;
        if save || due {
            persist(dir, &m);
            self.saved_alive.store(now, Ordering::Relaxed);
        }
    }

    /// Records the end of the main process (a kill keeps its `killed` state). `observed`: the end
    /// was seen as it happened, so the process ran until now.
    fn finish(&self, dir: &Path, exit_code: Option<i32>, note: Option<String>, observed: bool) {
        {
            let mut m = self.meta.lock().unwrap();
            if m.ended_at.is_some() {
                return;
            }
            let now = now_ms();
            if m.state == JobState::Running {
                m.state = JobState::Exited;
            }
            m.exit_code = exit_code;
            m.ended_at = Some(now);
            if observed {
                m.alive_until = Some(now);
            }
            if note.is_some() {
                m.note = note;
            }
            persist(dir, &m);
        }
        self.done.send_replace(true);
    }
}

/// Tracks the background jobs of this host. Cheap to clone; clones share the jobs.
#[derive(Clone)]
pub struct JobManager(Arc<Inner>);

struct Inner {
    dir: PathBuf,
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
    /// Serializes starts so concurrent calls cannot exceed [`MAX_RUNNING`].
    starting: Mutex<()>,
}

/// A `jobs_start` request.
pub struct StartRequest {
    pub command: String,
    pub shell: Shell,
    pub cwd: Option<String>,
    pub env: Option<EnvMap>,
    pub intent: Option<String>,
    pub owner: Owner,
}

impl StartRequest {
    fn spec(&self) -> CommandSpec<'_> {
        CommandSpec { command: &self.command, shell: self.shell, cwd: self.cwd.as_deref(), env: self.env.as_ref() }
    }
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
    /// Loads the jobs recorded in `dir`, re-adopting those whose process still runs. Blocks
    /// briefly (it reads the process table).
    pub fn new(dir: PathBuf) -> Self {
        let jobs = load_all(&dir).into_iter().map(|meta| (meta.id.clone(), Job::new(meta, Control::Adopted))).collect();
        let inner = Inner { dir, jobs: Mutex::new(jobs), starting: Mutex::new(()) };
        inner.refresh_adopted();
        Self(Arc::new(inner))
    }

    /// Starts a job; returns its id, pid and log path.
    pub async fn start(&self, req: StartRequest) -> Result<Value> {
        self.blocking(move |m| m.start(req)).await
    }

    /// Status of one job, and separately the last `tail_bytes` of its log.
    pub async fn status(&self, id: &str, tail_bytes: u64) -> Result<(Value, Option<String>)> {
        let id = id.to_string();
        self.blocking(move |m| m.status(&id, tail_bytes)).await
    }

    /// Short status of every job, oldest first.
    pub async fn list(&self) -> Result<Value> {
        self.blocking(|m| Ok(m.list())).await
    }

    /// A chunk of a job's log (a negative `offset` counts from the end), and whether it runs.
    pub async fn log(&self, id: &str, offset: i64, max_bytes: usize) -> Result<(LogChunk, bool)> {
        let id = id.to_string();
        self.blocking(move |m| m.log(&id, offset, max_bytes)).await
    }

    /// Waits up to `timeout` for a job's main process to end; returns whether it has.
    pub async fn wait(&self, id: &str, timeout: Duration) -> Result<bool> {
        let job = self.0.get(id)?;
        let mut done = job.done.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            if job.adopted() && job.running() {
                self.blocking(|m| {
                    m.refresh_adopted();
                    Ok(())
                })
                .await?;
            }
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

    /// Kills a job's whole process tree and waits briefly for its main process to end. Returns a
    /// note for the caller.
    pub async fn kill(&self, id: &str) -> Result<String> {
        let job = self.0.get(id)?;
        let marked = job.clone();
        let was_running = self.blocking(move |m| Ok(m.mark_killed(&marked))).await?;
        let root = job.snapshot().root();
        let swept = match &job.control {
            Control::Own(requests) => {
                let (reply, answer) = oneshot::channel();
                requests.send(reply).await.map_err(|_| anyhow!("job {id} is no longer tracked"))?;
                let result = answer.await.map_err(|_| anyhow!("job {id} is no longer tracked"))?;
                // Killing a tree that has already emptied is not an error.
                if let Err(e) = result
                    && was_running
                {
                    return Err(anyhow!(e).context(format!("kill job {id}")));
                }
                // The process group misses processes that moved to another group; the session does not.
                #[cfg(unix)]
                self.blocking(move |_| Ok(ProcTable::read().kill_job(&root))).await?;
                None
            }
            Control::Adopted => Some(self.blocking(move |_| Ok(ProcTable::read().kill_job(&root))).await?),
        };
        // Termination is asynchronous (on Windows a killed console process's conhost lingers for a
        // moment): wait until the tree is really gone, so a status read right after this call agrees.
        self.blocking(move |_| Ok(wait_tree_empty(&root, Duration::from_secs(3)))).await?;
        if !was_running {
            return Ok(match swept {
                Some(n) => format!("the job's main process had already ended; killed {n} process(es) it left running"),
                None => "the job's main process had already ended; killed the processes it left running, if any".into(),
            });
        }
        if self.wait(id, Duration::from_secs(5)).await? {
            Ok("killed the job and every process it started".into())
        } else {
            Ok("asked the OS to kill the job, but its main process has not ended within 5 s".into())
        }
    }

    /// Runs `f` on the blocking pool: process-table scans, process creation and file access.
    async fn blocking<T: Send + 'static>(&self, f: impl FnOnce(&Inner) -> Result<T> + Send + 'static) -> Result<T> {
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || f(&inner)).await.map_err(|e| anyhow!("the jobs task failed: {e}"))?
    }
}

/// The blocking work behind [`JobManager`].
impl Inner {
    /// Must run inside the tokio runtime (it spawns the job's reaper task).
    fn start(&self, req: StartRequest) -> Result<Value> {
        let _one_at_a_time = self.starting.lock().unwrap();
        self.refresh_adopted();
        let leftovers = self.leftovers();
        let busy = self.busy(&leftovers);
        if busy >= MAX_RUNNING {
            bail!(
                "{busy} jobs are running or have left processes running (the limit is {MAX_RUNNING}); wait for \
                 one to finish or stop one with jobs_kill"
            );
        }
        let spec = req.spec();
        // Fail on a bad command before creating any file.
        command::build(&spec)?;
        let id = self.new_id();
        let log_path = self.log_path(&id);
        let log = fs::OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("create job log {}", log_path.display()))?;
        let make = || -> Result<tokio::process::Command> {
            let mut cmd = command::build(&spec)?;
            cmd.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log.try_clone()?);
            Ok(cmd)
        };
        let child = match spawn_job(make) {
            Ok(child) => child,
            Err(e) => {
                let _ = fs::remove_file(&log_path);
                return Err(e.context(format!("start {}", spec.shell.name())));
            }
        };
        let pid = child.id().ok_or_else(|| anyhow!("the job exited before its pid could be read"))?;
        let now = now_ms();
        let meta = JobMeta {
            id: id.clone(),
            command: req.command.clone(),
            shell: req.shell.name().to_string(),
            cwd: req.cwd.clone(),
            intent: req.intent.clone(),
            owner: req.owner.clone(),
            pid,
            process_start: process_start_time(pid).unwrap_or(0),
            started_at: now,
            ended_at: None,
            alive_until: Some(now),
            exit_code: None,
            state: JobState::Running,
            note: None,
        };
        persist(&self.dir, &meta);
        let (kill_tx, kill_rx) = mpsc::channel(4);
        let job = Job::new(meta, Control::Own(kill_tx));
        tokio::spawn(reap(Arc::downgrade(&job), child, kill_rx, self.dir.clone()));
        self.jobs.lock().unwrap().insert(id.clone(), job);
        self.prune(&leftovers);
        Ok(json!({ "id": id, "pid": pid, "log_path": log_path.display().to_string(), "started_at": now }))
    }

    fn status(&self, id: &str, tail_bytes: u64) -> Result<(Value, Option<String>)> {
        let job = self.get(id)?;
        self.refresh_adopted();
        let leftover = self.leftover_of(&job);
        let mut v = self.describe(&job, leftover);
        let mut tail = None;
        if tail_bytes > 0 {
            match read_tail(&self.log_path(&job.id()), tail_bytes, job.running()) {
                Ok(text) => tail = Some(text),
                Err(e) => v["log_error"] = json!(format!("cannot read the log: {e}")),
            }
        }
        Ok((v, tail))
    }

    fn list(&self) -> Value {
        self.refresh_adopted();
        let leftovers = self.leftovers();
        let mut jobs = self.all();
        jobs.sort_by_key(|j| j.snapshot().started_at);
        let items: Vec<Value> = jobs
            .iter()
            .map(|j| {
                let m = j.snapshot();
                let mut item = json!({
                    "id": m.id,
                    "state": m.shown_state(),
                    "exit_code": m.exit_code,
                    "command": budget::head_tail(&m.command, 200, 95),
                    "started_at": m.started_at,
                    "duration_ms": m.ended_at.unwrap_or_else(now_ms).saturating_sub(m.started_at),
                    "owner": m.owner.name,
                });
                if let Some(n) = leftovers.get(&m.id) {
                    item["leftover_processes"] = json!(n);
                }
                item
            })
            .collect();
        let running = jobs.iter().filter(|j| j.running()).count();
        let mut v = json!({ "running": running, "max_running": MAX_RUNNING, "jobs": items });
        if !leftovers.is_empty() {
            v["ended_with_leftovers"] = json!(leftovers.len());
            v["note"] = json!(
                "jobs whose main process ended but left processes running count toward max_running until those \
                 exit or jobs_kill stops them"
            );
        }
        v
    }

    fn log(&self, id: &str, offset: i64, max_bytes: usize) -> Result<(LogChunk, bool)> {
        let job = self.get(id)?;
        self.refresh_adopted();
        let running = job.running();
        let chunk = read_log(&self.log_path(&job.id()), offset, max_bytes, running)
            .with_context(|| format!("read the log of job {id}"))?;
        Ok((chunk, running))
    }

    /// Marks a job whose main process still runs as killed; returns whether it ran.
    fn mark_killed(&self, job: &Job) -> bool {
        self.refresh_adopted();
        let mut m = job.meta.lock().unwrap();
        if m.ended_at.is_some() {
            return false;
        }
        m.state = JobState::Killed;
        persist(&self.dir, &m);
        true
    }

    fn get(&self, id: &str) -> Result<Arc<Job>> {
        self.jobs.lock().unwrap().get(id.trim()).cloned().ok_or_else(|| {
            anyhow!("no job with id '{id}' on this host; call jobs_status without an id to list the jobs")
        })
    }

    fn all(&self) -> Vec<Arc<Job>> {
        self.jobs.lock().unwrap().values().cloned().collect()
    }

    /// Jobs that take a running slot: their main process runs, or processes they left do.
    fn busy(&self, leftovers: &HashMap<String, usize>) -> usize {
        self.all().iter().filter(|j| j.running() || leftovers.contains_key(&j.id())).count()
    }

    fn describe(&self, job: &Job, leftover: Option<usize>) -> Value {
        let m = job.snapshot();
        let log_path = self.log_path(&m.id);
        let log_bytes = fs::metadata(&log_path).map(|md| md.len()).unwrap_or(0);
        let mut v = json!({
            "id": m.id,
            "state": m.shown_state(),
            "running": m.ended_at.is_none(),
            "exit_code": m.exit_code,
            "command": budget::head_tail(&m.command, 2_000, 950),
            "shell": m.shell,
            "cwd": m.cwd,
            "pid": m.pid,
            "started_at": m.started_at,
            "ended_at": m.ended_at,
            "duration_ms": m.ended_at.unwrap_or_else(now_ms).saturating_sub(m.started_at),
            "owner": m.owner,
            "intent": m.intent.as_deref().map(|i| budget::head_tail(i, 500, 240)),
            "adopted": job.adopted(),
            "note": m.note,
            "log_path": log_path.display().to_string(),
            "log_bytes": log_bytes,
        });
        if let Some(n) = leftover {
            v["leftover_processes"] = json!(n);
            if n > 0 {
                v["hint"] = json!(format!(
                    "the main process has ended, but {n} process(es) it started still run; they count toward the \
                     running-job limit until they exit or jobs_kill stops them"
                ));
            }
        }
        v
    }

    /// How many processes a job whose main process has ended left running (`None` while it runs).
    fn leftover_of(&self, job: &Job) -> Option<usize> {
        if job.running() {
            return None;
        }
        if job.settled.load(Ordering::Relaxed) {
            return Some(0);
        }
        let n = ProcTable::read().members(&job.snapshot().root()).len();
        if n == 0 {
            job.settled.store(true, Ordering::Relaxed);
        }
        Some(n)
    }

    /// Jobs whose main process has ended but that left processes running, with their count. Reads
    /// the process table only while some ended job has not been seen to empty.
    fn leftovers(&self) -> HashMap<String, usize> {
        let pending: Vec<Arc<Job>> =
            self.all().into_iter().filter(|j| !j.running() && !j.settled.load(Ordering::Relaxed)).collect();
        if pending.is_empty() {
            return HashMap::new();
        }
        let table = ProcTable::read();
        let mut out = HashMap::new();
        for job in pending {
            match table.members(&job.snapshot().root()).len() {
                0 => job.settled.store(true, Ordering::Relaxed),
                n => {
                    out.insert(job.id(), n);
                }
            }
        }
        out
    }

    /// Marks re-adopted jobs whose main process is gone as ended, and notes the time for the others.
    fn refresh_adopted(&self) {
        let candidates: Vec<Arc<Job>> = self.all().into_iter().filter(|j| j.adopted() && j.running()).collect();
        if candidates.is_empty() {
            return;
        }
        let pids: Vec<Pid> = candidates.iter().map(|j| Pid::from_u32(j.snapshot().pid)).collect();
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, ProcessRefreshKind::nothing());
        for job in candidates {
            let m = job.snapshot();
            let alive = sys.process(Pid::from_u32(m.pid)).is_some_and(|p| same_start(p.start_time(), m.process_start));
            if alive {
                job.seen_alive(&self.dir, false);
            } else {
                job.finish(
                    &self.dir,
                    None,
                    Some(
                        "the host restarted while this job ran: its exit code is unknown, and ended_at is when the \
                         host noticed the end"
                            .into(),
                    ),
                    false,
                );
            }
        }
    }

    /// Deletes the oldest finished jobs beyond [`MAX_KEPT`]. Jobs whose leftovers still run stay:
    /// without their record jobs_kill could no longer reach those processes.
    fn prune(&self, leftovers: &HashMap<String, usize>) {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.len() <= MAX_KEPT {
            return;
        }
        let mut finished: Vec<(u64, String)> = jobs
            .values()
            .filter(|j| !j.running())
            .map(|j| j.snapshot())
            .filter(|m| !leftovers.contains_key(&m.id))
            .map(|m| (m.started_at, m.id))
            .collect();
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

impl Drop for Inner {
    /// The host is stopping and its jobs keep running: save that their main processes ran until
    /// now, so a restarted host attributes to them what they start until then. A job this process
    /// started is known to run until its reaper sees it end.
    fn drop(&mut self) {
        if let Ok(jobs) = self.jobs.get_mut() {
            for job in jobs.values().filter(|j| !j.adopted() && j.running()) {
                job.seen_alive(&self.dir, true);
            }
        }
    }
}

/// Waits for a job's main process, records its exit, and serves kill requests (also afterwards:
/// the main process may have left processes running in its Job Object or process group). Notes
/// every [`ALIVE_SAVE_EVERY`] that the main process still runs. Returns without killing anything
/// when the manager is dropped, which is what happens when the host stops.
async fn reap(job: Weak<Job>, mut child: Box<dyn ChildWrapper>, mut kills: mpsc::Receiver<KillReply>, dir: PathBuf) {
    let mut alive = tokio::time::interval_at(tokio::time::Instant::now() + ALIVE_SAVE_EVERY, ALIVE_SAVE_EVERY);
    let status = loop {
        tokio::select! {
            status = command::main_process(&mut *child).wait() => break status,
            request = kills.recv() => match request {
                Some(reply) => { let _ = reply.send(child.start_kill()); }
                None => return,
            },
            _ = alive.tick() => match job.upgrade() {
                Some(job) => job.seen_alive(&dir, true),
                None => return,
            },
        }
    };
    if let Some(job) = job.upgrade() {
        let (code, note) = match status {
            Ok(s) => (s.code(), None),
            Err(e) => (None, Some(format!("could not read the exit status: {e}"))),
        };
        job.finish(&dir, code, note, true);
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

/// One process in a snapshot of the process table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Proc {
    pid: u32,
    parent: Option<u32>,
    /// Start time in seconds since the epoch.
    start: u64,
    /// Unix session id. `None` on Windows, whose "session" is the logon session.
    session: Option<u32>,
    /// An OS helper that lives and dies with the processes it serves (Windows' console host): never
    /// counted as something the job left running, and it may outlive its console by a moment.
    os_helper: bool,
}

/// A job's main process as recorded: the pid with the start time identifies it.
#[derive(Debug, Clone, Copy)]
struct Root {
    pid: u32,
    start: u64,
    /// Seconds since the epoch until which it is known to have run.
    alive_until: Option<u64>,
}

/// A snapshot of the process table, for finding a job's processes without its Job Object.
struct ProcTable {
    sys: System,
    procs: Vec<Proc>,
}

impl ProcTable {
    fn read() -> Self {
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
        let procs = sys
            .processes()
            .values()
            .map(|p| Proc {
                pid: p.pid().as_u32(),
                parent: p.parent().map(Pid::as_u32),
                start: p.start_time(),
                session: unix_session(p),
                os_helper: cfg!(windows) && p.name().eq_ignore_ascii_case("conhost.exe"),
            })
            .collect();
        Self { sys, procs }
    }

    fn members(&self, root: &Root) -> Vec<Proc> {
        job_members(&self.procs, root)
    }

    /// Kills the job's live processes; returns how many were killed.
    fn kill_job(&self, root: &Root) -> usize {
        self.members(root).iter().filter(|p| kill_process(&self.sys, p.pid, p.start)).count()
    }
}

#[cfg(unix)]
fn unix_session(p: &sysinfo::Process) -> Option<u32> {
    p.session_id().map(Pid::as_u32)
}

#[cfg(not(unix))]
fn unix_session(_: &sysinfo::Process) -> Option<u32> {
    None
}

/// Polls the process table until the job whose main process is `root` has no live process left, or
/// `timeout` passes. Returns whether it emptied.
fn wait_tree_empty(root: &Root, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if ProcTable::read().members(root).is_empty() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The live processes of the job whose main process is `root`, given every live process. Pure, so
/// the pid-reuse rules are testable:
/// - the main process counts while a live process has its pid and start time;
/// - a process it started (its parent link, or on Unix its session, names the main process) counts
///   when it started after the main process and before any live process that reused the pid, and,
///   once the main process has ended, no later than `alive_until`; without that time nothing is
///   attributed to an ended main process;
/// - a process whose parent is a live member counts when it started after that parent.
fn job_members(procs: &[Proc], root: &Root) -> Vec<Proc> {
    let holder = procs.iter().find(|p| p.pid == root.pid);
    let root_alive = holder.is_some_and(|p| same_start(p.start, root.start));
    let latest = match (root_alive, root.alive_until) {
        (true, _) => None,
        (false, Some(t)) => Some(t),
        (false, None) => return Vec::new(),
    };
    let reused_at = holder.filter(|_| !root_alive).map(|p| p.start);
    let started_by_root = |p: &Proc| {
        p.start >= root.start && latest.is_none_or(|t| p.start <= t) && reused_at.is_none_or(|r| p.start < r)
    };
    let mut members: Vec<Proc> = holder.filter(|_| root_alive).copied().into_iter().collect();
    members.extend(
        procs
            .iter()
            .filter(|p| !p.os_helper)
            .filter(|p| p.pid != root.pid && (p.parent == Some(root.pid) || p.session == Some(root.pid)))
            .filter(|p| started_by_root(p)),
    );
    let mut seen: HashSet<u32> = members.iter().map(|p| p.pid).collect();
    let mut next = 0;
    while next < members.len() {
        let parent = members[next];
        next += 1;
        for p in procs {
            if !p.os_helper
                && p.parent == Some(parent.pid)
                && p.pid != parent.pid
                && p.start >= parent.start
                && seen.insert(p.pid)
            {
                members.push(*p);
            }
        }
    }
    members
}

/// Kills one process, unless its pid now belongs to a process with another start time. On Windows
/// the process is opened first and checked through that handle, which pins it: the pid cannot be
/// reused between the check and the kill (sysinfo would run taskkill by pid, found through PATH).
#[cfg(windows)]
fn kill_process(_sys: &System, pid: u32, start: u64) -> bool {
    use windows::Win32::{
        Foundation::{CloseHandle, FILETIME},
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
        },
    };
    // SAFETY: Win32 calls on a handle this function opens and closes; the out-parameters are
    // valid locals.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let (mut created, mut exited, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let same = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user).is_ok()
            && same_start(filetime_to_unix_secs(created), start);
        let killed = same && TerminateProcess(handle, 1).is_ok();
        let _ = CloseHandle(handle);
        killed
    }
}

/// A FILETIME (100 ns ticks since 1601) as whole seconds since the Unix epoch, as sysinfo reports
/// start times.
#[cfg(windows)]
fn filetime_to_unix_secs(ft: windows::Win32::Foundation::FILETIME) -> u64 {
    const SECS_1601_TO_1970: u64 = 11_644_473_600;
    let ticks = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    (ticks / 10_000_000).saturating_sub(SECS_1601_TO_1970)
}

#[cfg(not(windows))]
fn kill_process(sys: &System, pid: u32, start: u64) -> bool {
    sys.process(Pid::from_u32(pid)).is_some_and(|p| same_start(p.start_time(), start) && p.kill())
}

fn process_start_time(pid: u32) -> Option<u64> {
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing());
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

/// Up to `max_bytes` of a log from `offset` (negative: from the end), on UTF-8 character
/// boundaries: never starting inside a character and, while more output may follow
/// (`more_coming`, or the read stopped before the end), never ending inside one. Returns the
/// offset of the first byte returned, the bytes and the log's size.
fn read_window(path: &Path, offset: i64, max_bytes: usize, more_coming: bool) -> io::Result<(u64, Vec<u8>, u64)> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((0, Vec::new(), 0)),
        Err(e) => return Err(e),
    };
    let size = file.metadata()?.len();
    let start = if offset < 0 { size.saturating_sub(offset.unsigned_abs()) } else { (offset as u64).min(size) };
    let want = (size - start).min(max_bytes as u64) as usize;
    let mut buf = vec![0u8; want];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut buf)?;
    let (skip, len) = utf8_window(&buf, start > 0, more_coming || start + (want as u64) < size);
    buf.truncate(skip + len);
    buf.drain(..skip);
    Ok((start + skip as u64, buf, size))
}

/// A `jobs_log` chunk: [`read_window`] cut to its first [`LOG_CHAR_BUDGET`] characters.
fn read_log(path: &Path, offset: i64, max_bytes: usize, more_coming: bool) -> io::Result<LogChunk> {
    let (start, bytes, size) = read_window(path, offset, max_bytes, more_coming)?;
    let len = fit_chars(&bytes, LOG_CHAR_BUDGET);
    Ok(LogChunk { offset: start, next_offset: start + len as u64, size, text: command::decode(&bytes[..len]) })
}

/// The end of a log: at most its last `max_bytes`, and of those at most the last
/// [`LOG_CHAR_BUDGET`] characters.
fn read_tail(path: &Path, max_bytes: u64, more_coming: bool) -> io::Result<String> {
    let max = max_bytes.min(i64::MAX as u64);
    let (_, bytes, _) = read_window(path, -(max as i64), max as usize, more_coming)?;
    let text = command::decode(&bytes);
    let chars = text.chars().count();
    Ok(if chars > LOG_CHAR_BUDGET { text.chars().skip(chars - LOG_CHAR_BUDGET).collect() } else { text })
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

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
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

/// A job's status as JSON, followed by the end of its log as raw text in a block of its own:
/// escaped inside the JSON, a long tail would push the block over the gate's result budget, which
/// cuts out its middle.
fn status_result(mut status: Value, tail: Option<String>) -> CallToolResult {
    let tail = tail.filter(|t| !t.is_empty());
    if let Some(t) = &tail {
        status["log_tail_chars"] = json!(t.chars().count());
    }
    let mut result = super::json(status);
    if let Some(t) = tail {
        result.content.push(ContentBlock::text(t));
    }
    result
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
    /// Environment changes for this job: a string sets the variable, null removes it. Values are
    /// not shown in approval prompts or the audit log (names are) and are not saved with the job.
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
                 after your session ends, until it exits or jobs_kill stops it; a restart of the host may end it \
                 (always assume so for a host started at logon), so check jobs_status after a reconnect. At \
                 most 16 jobs run at once; a job whose main process exited but left processes running still \
                 counts. Usually needs the owner's approval.",
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

    fn secret_args(&self) -> &'static [&'static str] {
        &["env"]
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: StartParams = parse(args)?;
        let req = StartRequest {
            shell: Shell::parse(p.shell.as_deref())?,
            command: p.command,
            cwd: p.cwd,
            env: p.env,
            intent: p.intent,
            owner: Owner { name: ctx.session.peer.name.clone(), principal: ctx.session.peer.principal.clone() },
        };
        Ok(super::json(ctx.host.jobs().start(req).await?))
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
                "Status of a background job as JSON (state running/exited/killed, which follows the job's main \
                 process; exit code; start and end time in Unix ms; duration; owner; and once the main process \
                 has ended, leftover_processes: how many processes it started still run), followed by the end of \
                 its log as a second text block. Without an id, a short list of every job. Jobs are shared by \
                 every admin-level device of this host.",
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
        Ok(match p.id {
            Some(id) => {
                let (status, tail) = jobs.status(&id, p.tail_bytes.unwrap_or(4096).min(65_536)).await?;
                status_result(status, tail)
            }
            None => super::json(jobs.list().await?),
        })
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
    /// Most bytes to return (default 65536, min 4, max 1048576); at most about 50,000 characters
    /// are returned per call regardless.
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
        let max_bytes = p.max_bytes.unwrap_or(65_536).clamp(MIN_LOG_BYTES, 1 << 20);
        let (chunk, running) = ctx.host.jobs().log(&p.id, p.offset.unwrap_or(0), max_bytes).await?;
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
                "Wait up to timeout_secs (max 120) for a background job's main process to end, then return its \
                 status like jobs_status. If it is still running, call jobs_wait again.",
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
        let (mut v, tail) = jobs.status(&p.id, 4096).await?;
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
        Ok(status_result(v, tail))
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
                 running after its main process exited), then returns its status. Only affects jobs started with \
                 jobs_start, but any of them, whichever device started it.",
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
        let (mut v, tail) = jobs.status(&p.id, 1024).await?;
        v["result"] = json!(note);
        Ok(status_result(v, tail))
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
            let ok = (1..=64).contains(&name.len())
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
            assert!(ok, "invalid tool name '{name}'");
        }
    }

    #[test]
    fn fit_chars_cuts_on_character_boundaries() {
        assert_eq!(fit_chars(b"abc", 5), 3);
        assert_eq!(fit_chars("世世世".as_bytes(), 2), 6);
        assert_eq!(fit_chars("ab世世".as_bytes(), 3), 5);
    }

    fn proc(pid: u32, parent: u32, start: u64) -> Proc {
        Proc { pid, parent: Some(parent), start, session: None, os_helper: false }
    }

    fn members(procs: &[Proc], root: Root) -> Vec<u32> {
        let mut pids: Vec<u32> = job_members(procs, &root).iter().map(|p| p.pid).collect();
        pids.sort();
        pids
    }

    const LIVE_ROOT: Root = Root { pid: 10, start: 100, alive_until: None };
    /// Root 10 started at 100 and is known to have run until 150.
    const ENDED_ROOT: Root = Root { pid: 10, start: 100, alive_until: Some(150) };

    #[test]
    fn os_helpers_are_not_job_members() {
        // A console job's conhost (11) may outlive the job by a moment; it never counts as a leftover.
        let conhost = Proc { os_helper: true, ..proc(11, 10, 101) };
        let procs = [conhost, proc(12, 10, 102)];
        assert_eq!(members(&procs, ENDED_ROOT), [12]);
        assert!(members(&[conhost], ENDED_ROOT).is_empty());
    }

    #[test]
    fn live_root_takes_its_descendants_only() {
        // 10 -> 11 -> 12; 13 is a child of an older process that had pid 10; 30 is unrelated.
        let procs = [proc(10, 1, 100), proc(11, 10, 101), proc(12, 11, 102), proc(13, 10, 50), proc(30, 2, 10)];
        assert_eq!(members(&procs, LIVE_ROOT), [10, 11, 12]);
    }

    #[test]
    fn ended_root_keeps_the_children_it_started() {
        // 11 was started by the root and still runs; 12 was started by 11 after the root ended.
        let procs = [proc(11, 10, 120), proc(12, 11, 300)];
        assert_eq!(members(&procs, ENDED_ROOT), [11, 12]);
    }

    #[test]
    fn reused_pid_never_pulls_in_its_children() {
        // Pid 10 was reused at 200 by a process that still runs; its child 14 is not the job's.
        let procs = [proc(11, 10, 101), proc(10, 3, 200), proc(14, 10, 201)];
        assert_eq!(members(&procs, ENDED_ROOT), [11]);
        // The reusing process has exited too, leaving its child 14 behind: still not the job's,
        // because 14 started after the root was last known to run.
        let procs = [proc(14, 10, 201), proc(99, 4, 50)];
        assert_eq!(members(&procs, ENDED_ROOT), Vec::<u32>::new());
        // A child that started while the root was known to run is the root's.
        assert_eq!(members(&[proc(14, 10, 140)], ENDED_ROOT), [14]);
    }

    #[test]
    fn ended_root_without_a_known_lifetime_has_no_members() {
        let procs = [proc(11, 10, 120)];
        assert_eq!(members(&procs, Root { alive_until: None, ..ENDED_ROOT }), Vec::<u32>::new());
    }

    #[test]
    fn unix_orphans_are_found_through_their_session() {
        // 15 was reparented to init (pid 1) but kept the job's session; 16 is its child.
        let orphan = Proc { pid: 15, parent: Some(1), start: 130, session: Some(10), os_helper: false };
        let procs = [orphan, proc(16, 15, 400)];
        assert_eq!(members(&procs, ENDED_ROOT), [15, 16]);
        // A session member that started after the root was last known alive is not attributed.
        let late = Proc { start: 160, ..orphan };
        assert_eq!(members(&[late], ENDED_ROOT), Vec::<u32>::new());
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

    #[test]
    fn smallest_reads_still_make_progress() {
        let dir = std::env::temp_dir().join(format!("ar-jobs-small-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.log");
        fs::write(&path, "世界😀!").unwrap(); // 3 + 3 + 4 + 1 bytes
        let mut offset = 0u64;
        let mut text = String::new();
        for _ in 0..10 {
            let chunk = read_log(&path, offset as i64, MIN_LOG_BYTES, true).unwrap();
            assert!(chunk.next_offset > offset, "stuck at {offset}");
            text.push_str(&chunk.text);
            offset = chunk.next_offset;
            if offset == chunk.size {
                break;
            }
        }
        assert_eq!(text, "世界😀!");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_tail_keeps_the_end() {
        let dir = std::env::temp_dir().join(format!("ar-jobs-tail-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.log");
        let lines: String = (0..20_000).map(|i| format!("l{i}\n")).collect();
        fs::write(&path, &lines).unwrap();
        let tail = read_tail(&path, 65_536, false).unwrap();
        assert_eq!(tail.chars().count(), LOG_CHAR_BUDGET);
        assert!(tail.ends_with("l19999\n"), "the last line is kept");
        // A partial character at the end of a running job's log is held back.
        fs::write(&path, &"ab世".as_bytes()[..4]).unwrap();
        assert_eq!(read_tail(&path, 100, true).unwrap(), "ab");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_tail_is_its_own_block() {
        let r = status_result(json!({ "id": "x" }), Some("tail\n".into()));
        assert_eq!(r.content.len(), 2);
        assert_eq!(r.content[1].as_text().unwrap().text, "tail\n");
        assert_eq!(r.structured_content.unwrap()["log_tail_chars"], 5);
        assert_eq!(status_result(json!({}), Some(String::new())).content.len(), 1);
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

    fn request(command: &str) -> StartRequest {
        StartRequest {
            command: command.into(),
            shell: Shell::parse(None).unwrap(),
            cwd: None,
            env: None,
            intent: Some("test".into()),
            owner: owner(),
        }
    }

    async fn start(m: &JobManager, command: &str) -> String {
        let v = m.start(request(command)).await.unwrap();
        v["id"].as_str().unwrap().to_string()
    }

    async fn status(m: &JobManager, id: &str) -> Value {
        m.status(id, 0).await.unwrap().0
    }

    fn alive(pid: u32) -> bool {
        process_start_time(pid).is_some()
    }

    #[tokio::test]
    async fn job_runs_logs_and_reports_exit() {
        let dir = test_dir("run");
        let m = JobManager::new(dir.clone());
        let cmd = if cfg!(windows) {
            "Write-Output 'line 1 世界'; [Console]::Error.WriteLine('err'); exit 7"
        } else {
            "echo 'line 1 世界'; echo err >&2; exit 7"
        };
        let id = start(&m, cmd).await;
        assert!(m.wait(&id, Duration::from_secs(60)).await.unwrap(), "the job should end");
        let (s, tail) = m.status(&id, 4096).await.unwrap();
        assert_eq!(s["state"], "exited");
        assert_eq!(s["exit_code"], 7);
        assert_eq!(s["leftover_processes"], 0);
        let tail = tail.unwrap();
        assert!(tail.contains("line 1 世界") && tail.contains("err"), "{tail}");
        // Metadata survives on disk.
        let saved: JobMeta = serde_json::from_slice(&fs::read(dir.join(format!("{id}.json"))).unwrap()).unwrap();
        assert_eq!((saved.state, saved.exit_code), (JobState::Exited, Some(7)));
        assert_eq!(saved.alive_until, saved.ended_at, "an observed end bounds the job's lifetime");
        assert_eq!(m.list().await.unwrap()["jobs"].as_array().unwrap().len(), 1);
        let err = m.status("nope", 0).await.unwrap_err().to_string();
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

    /// A job that starts a sleeping child, records both pids and exits at once (a launcher).
    fn launcher_job() -> &'static str {
        if cfg!(windows) {
            "$c = Start-Process -PassThru -NoNewWindow ping -ArgumentList '-n','300','127.0.0.1'; \
             Write-Output \"PIDS $PID $($c.Id)\""
        } else {
            "sleep 300 & echo \"PIDS $$ $!\""
        }
    }

    /// Waits up to 60 s: on a loaded CI runner PowerShell's cold start plus `Start-Process` has
    /// taken over 15 s while the rest of the suite runs alongside.
    async fn job_pids(m: &JobManager, id: &str) -> Vec<u32> {
        for _ in 0..600 {
            let (chunk, _) = m.log(id, 0, 65_536).await.unwrap();
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
        let id = start(&m, tree_job()).await;
        let pids = job_pids(&m, &id).await;
        assert!(!m.wait(&id, Duration::from_millis(300)).await.unwrap(), "still running");
        let note = m.kill(&id).await.unwrap();
        assert!(note.starts_with("killed"), "{note}");
        let s = status(&m, &id).await;
        assert_eq!(s["state"], "killed", "{s}");
        assert!(!s["ended_at"].is_null(), "the kill waits for the process to end: {s}");
        assert_gone(&pids).await;
        drop(m);
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn restarted_manager_readopts_and_kills_a_running_job() {
        let dir = test_dir("adopt");
        let first = JobManager::new(dir.clone());
        let id = start(&first, tree_job()).await;
        let pids = job_pids(&first, &id).await;
        // What a host restart looks like to the jobs: the manager goes away, the processes stay.
        drop(first);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(pids.iter().all(|p| alive(*p)), "jobs outlive the manager");

        let second = JobManager::new(dir.clone());
        let s = status(&second, &id).await;
        assert_eq!((s["state"].as_str(), s["adopted"].as_bool()), (Some("running"), Some(true)), "{s}");
        let note = second.kill(&id).await.unwrap();
        assert!(note.starts_with("killed"), "{note}");
        assert_gone(&pids).await;
        let s = status(&second, &id).await;
        assert_eq!(s["state"], "killed", "{s}");
        assert!(!s["ended_at"].is_null(), "the kill waits for the process to end: {s}");
        drop(second);

        // A third start sees the job as finished, not running.
        let third = JobManager::new(dir.clone());
        assert_eq!(status(&third, &id).await["state"], "killed");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A launcher's main process exits at once while its child keeps running: the job is reported
    /// as exited with leftovers, takes a running slot, and jobs_kill stops the leftovers, both from
    /// the host that started it and from a restarted one.
    #[tokio::test]
    async fn leftovers_are_reported_counted_and_killed() {
        let dir = test_dir("leftover");
        let first = JobManager::new(dir.clone());
        let own = start(&first, launcher_job()).await;
        let adopted = start(&first, launcher_job()).await;
        let own_pids = job_pids(&first, &own).await;
        let adopted_pids = job_pids(&first, &adopted).await;
        for id in [&own, &adopted] {
            assert!(first.wait(id, Duration::from_secs(60)).await.unwrap(), "the main process exits at once");
            let s = status(&first, id).await;
            assert_eq!(s["state"], "exited", "{s}");
            assert!(s["leftover_processes"].as_u64().unwrap() >= 1, "{s}");
        }
        assert!(alive(own_pids[1]) && alive(adopted_pids[1]), "the children still run");
        assert_eq!(first.0.busy(&first.0.leftovers()), 2, "jobs with leftovers take a running slot");
        assert_eq!(first.list().await.unwrap()["ended_with_leftovers"], 2);

        let note = first.kill(&own).await.unwrap();
        assert!(note.contains("already ended"), "{note}");
        assert_gone(&own_pids[1..]).await;
        assert_eq!(status(&first, &own).await["leftover_processes"], 0);

        drop(first);
        let second = JobManager::new(dir.clone());
        let s = status(&second, &adopted).await;
        assert!(s["leftover_processes"].as_u64().unwrap() >= 1, "found again after a restart: {s}");
        let note = second.kill(&adopted).await.unwrap();
        assert!(note.contains("already ended") && !note.contains("killed 0"), "{note}");
        assert_gone(&adopted_pids[1..]).await;
        assert_eq!(status(&second, &adopted).await["leftover_processes"], 0);
        assert_eq!(second.0.busy(&second.0.leftovers()), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn readopted_job_that_ended_is_reported_without_exit_code() {
        let dir = test_dir("gone");
        let first = JobManager::new(dir.clone());
        let id = start(&first, tree_job()).await;
        let pids = job_pids(&first, &id).await;
        drop(first);
        // The job ends while no manager is watching.
        let root = Root { pid: pids[0], start: process_start_time(pids[0]).unwrap(), alive_until: None };
        assert!(ProcTable::read().kill_job(&root) >= 2, "the main process and its child");
        assert_gone(&pids).await;
        let second = JobManager::new(dir.clone());
        let s = status(&second, &id).await;
        assert_eq!(s["state"], "exited", "{s}");
        assert!(s["exit_code"].is_null());
        assert!(s["note"].as_str().unwrap().contains("exit code is unknown"));
        assert_eq!(s["leftover_processes"], 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn running_jobs_are_limited() {
        let dir = test_dir("limit");
        let m = JobManager::new(dir.clone());
        let sleeper = if cfg!(windows) { "Start-Sleep 120" } else { "sleep 120" };
        let mut ids = Vec::new();
        for _ in 0..MAX_RUNNING {
            ids.push(start(&m, sleeper).await);
        }
        let err = m.start(request(sleeper)).await.unwrap_err().to_string();
        assert!(err.contains("the limit is 16"), "{err}");
        for id in &ids {
            m.kill(id).await.unwrap();
        }
        drop(m);
        let _ = fs::remove_dir_all(&dir);
    }
}
