//! The child process behind one mount and the MCP client session to it.
//!
//! The process runs in a Job Object (Windows) or its own process group (Unix) with kill-on-drop,
//! so stopping a mount, or the host going away, takes the whole process tree with it: a UI driver
//! that launched helpers must not leave them behind (F14). On Windows the job is created with
//! KILL_ON_JOB_CLOSE, so even a hard kill of the host closes the job handle and ends the tree.
//!
//! A supervisor task owns the process and waits for the server process itself to exit. The pipe is
//! not a reliable signal: a helper the server started can hold an inherited copy of its stdout, and
//! then EOF comes when the helper exits, not when the server died. On exit the session is marked
//! dead at once, calls in flight fail, the rest of the tree is ended, and the next call starts a new
//! server. When it keeps failing, restarts back off exponentially and calls inside the backoff
//! window fail fast with the last error, rather than queueing behind a broken binary.
//!
//! A server that hangs without exiting is caught when a call times out: it gets a ping, and when
//! that goes unanswered too it is stopped and started again on the next call.

use std::{
    collections::VecDeque,
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::{VERSION, config::MountConfig};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use rmcp::{
    ServiceExt,
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResult, CancelledNotification,
        CancelledNotificationParam, ClientCapabilities, ClientConfig, ClientRequest,
        Implementation, JsonObject, RequestId, ServerResult,
    },
    service::{Peer, PeerRequestOptions, RoleClient, RunningService, ServiceError},
    transport::async_rw::AsyncRwTransport,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::ChildStderr,
    sync::{Mutex, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

/// Time a server gets from spawn to a completed MCP handshake.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling for one forwarded call; the call is cancelled on the server when it is reached.
const CALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long a server that let a call time out gets to answer a ping before it counts as hung.
const PING_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a stopping server gets to exit by itself after its stdin closed.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// Upper bound for ending what is left of a process tree.
const KILL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a response the server sent just before it exited may still take to arrive.
const EXIT_SETTLE: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const STDERR_TAIL_LINES: usize = 20;
const STDERR_LINE_MAX: usize = 500;
/// The stderr log is moved to `<name>.log.old` when it grows past this at a start.
const LOG_ROTATE_BYTES: u64 = 8 << 20;

type Client = RunningService<RoleClient, ClientConfig>;

/// One mounted server: its configuration and, while it runs, its process and MCP session.
pub struct Mount {
    config: MountConfig,
    command: PathBuf,
    /// Working directory of the server and home of its stderr log.
    dir: PathBuf,
    call_timeout: Duration,
    state: Mutex<State>,
    activity: StdMutex<Activity>,
}

#[derive(Default)]
struct State {
    running: Option<Running>,
    /// Consecutive failures (start errors and crashes); reset by the next successful call.
    failures: u32,
    last_failure: Option<Instant>,
    last_error: Option<String>,
}

struct Running {
    client: Arc<Client>,
    pid: Option<u32>,
    stderr: Arc<StderrTail>,
    process: Process,
}

impl Running {
    fn live(&self) -> Live {
        Live {
            client: self.client.clone(),
            exited: self.process.exited.clone(),
        }
    }

    fn alive(&self) -> bool {
        !self.process.exited.is_cancelled() && !self.client.is_transport_closed()
    }
}

/// What a call needs from the running server: the session, and a signal for the process's exit.
struct Live {
    client: Arc<Client>,
    exited: CancellationToken,
}

impl Live {
    /// Waits for `fut`, but not past the server's exit. A response the server sent just before it
    /// exited still gets a moment to arrive.
    async fn until_exit<T>(
        &self,
        fut: impl Future<Output = Result<T, ServiceError>>,
    ) -> Result<T, ServiceError> {
        tokio::pin!(fut);
        tokio::select! {
            biased;
            r = &mut fut => r,
            _ = self.exited.cancelled() => match tokio::time::timeout(EXIT_SETTLE, &mut fut).await {
                Ok(r) => r,
                Err(_) => Err(ServiceError::TransportClosed),
            },
        }
    }

    /// Whether the server still answers requests (after a call timed out on it).
    async fn answers_ping(&self) -> bool {
        let peer = self.client.peer().clone();
        let ping = async move {
            peer.send_request_with_option(
                ClientRequest::PingRequest(Default::default()),
                PeerRequestOptions::with_timeout(PING_TIMEOUT),
            )
            .await?
            .await_response()
            .await
        };
        self.until_exit(ping).await.is_ok()
    }
}

struct Activity {
    in_flight: usize,
    last_used: Instant,
}

impl Mount {
    /// A stopped mount. `dir` is the profile's mounts directory: relative commands resolve
    /// against it, and the server runs in it (a writable place that does not depend on how the
    /// host was launched) with its stderr logged to `<dir>/<name>.log`.
    pub fn new(config: MountConfig, dir: &Path) -> Arc<Self> {
        Self::with_call_timeout(config, dir, CALL_TIMEOUT)
    }

    /// [`Mount::new`] with another per-call ceiling (tests use a short one).
    #[doc(hidden)]
    pub fn with_call_timeout(config: MountConfig, dir: &Path, call_timeout: Duration) -> Arc<Self> {
        // Profile paths are canonical (`\\?\C:\...` on Windows); some programs mishandle that
        // form as their working directory or own path.
        let dir = dunce::simplified(dir).to_path_buf();
        let command = super::resolve_command(&config.command, &dir);
        Arc::new(Self {
            config,
            command,
            dir,
            call_timeout,
            state: Mutex::new(State::default()),
            activity: StdMutex::new(Activity {
                in_flight: 0,
                last_used: Instant::now(),
            }),
        })
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn config(&self) -> &MountConfig {
        &self.config
    }

    /// The resolved command that is started.
    pub fn command(&self) -> &Path {
        &self.command
    }

    /// The file the server's stderr goes to.
    pub fn log_path(&self) -> PathBuf {
        self.dir.join(format!("{}.log", self.config.name))
    }

    /// Where the tool list from the last successful discovery is kept.
    pub fn tools_cache_path(&self) -> PathBuf {
        self.dir.join(format!("{}.tools.json", self.config.name))
    }

    /// Every tool the server offers (allowlisted or not). Starts the server when needed.
    pub async fn list_tools(&self) -> Result<Vec<rmcp::model::Tool>> {
        let _busy = self.begin();
        let live = self.live().await?;
        // Bounded: the host waits for this at start.
        match tokio::time::timeout(START_TIMEOUT, live.until_exit(live.client.list_all_tools()))
            .await
        {
            Ok(Ok(tools)) => {
                self.state.lock().await.succeeded();
                Ok(tools)
            }
            Ok(Err(e)) => Err(self.failed_call(&live, "tools/list", e).await),
            Err(_) => {
                self.drop_session(&live, "no answer to tools/list".into(), Duration::ZERO)
                    .await;
                bail!(
                    "mount '{}' did not answer tools/list within {} s",
                    self.name(),
                    START_TIMEOUT.as_secs()
                )
            }
        }
    }

    /// Forwards one tool call. The result (text, images, structured content, `isError`) is passed
    /// through unchanged. Dropping the future cancels the call on the server.
    pub async fn call(&self, tool: &str, args: JsonObject) -> Result<CallToolResult> {
        let _busy = self.begin();
        let live = self.live().await?;
        let peer = live.client.peer().clone();
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(
            CallToolRequestParams::new(tool.to_string()).with_arguments(args),
        ));
        let options = PeerRequestOptions::with_timeout(self.call_timeout);
        let handle = match peer.send_cancellable_request(request, options).await {
            Ok(handle) => handle,
            Err(e) => return Err(self.failed_call(&live, tool, e).await),
        };
        // The gate drops this future when the session ends; tell the server so it can stop, and
        // so the pending request does not linger in the client.
        let mut on_drop = CancelOnDrop {
            peer: Some(peer),
            id: handle.id.clone(),
        };
        let response = live.until_exit(handle.await_response()).await;
        on_drop.peer = None;
        match response {
            Ok(ServerResult::CallToolResult(result)) => {
                self.state.lock().await.succeeded();
                Ok(result)
            }
            Ok(_) => bail!(
                "mount '{}' answered '{tool}' with something other than a tool result (a follow-up \
                 request or a task), which mounted servers cannot use",
                self.name()
            ),
            Err(e) => Err(self.failed_call(&live, tool, e).await),
        }
    }

    /// Stops the server (its whole process tree). The next call starts it again.
    pub async fn stop(&self) {
        let running = self.state.lock().await.running.take();
        if let Some(running) = running {
            shutdown(running, STOP_GRACE).await;
        }
    }

    /// Stops the server when no call is running and none ran for `idle`. Returns whether it stopped.
    pub async fn stop_if_idle(&self, idle: Duration) -> bool {
        // Checked under the state lock: a call counts itself busy before it takes this lock, so
        // none can slip in between the check and the stop.
        let mut state = self.state.lock().await;
        {
            let activity = self.activity.lock().unwrap();
            if activity.in_flight > 0 || activity.last_used.elapsed() < idle {
                return false;
            }
        }
        let Some(running) = state.running.take() else {
            return false;
        };
        drop(state);
        shutdown(running, STOP_GRACE).await;
        true
    }

    fn begin(&self) -> Busy<'_> {
        let mut a = self.activity.lock().unwrap();
        a.in_flight += 1;
        a.last_used = Instant::now();
        Busy(self)
    }

    /// The live session, started (or restarted after a crash) when needed.
    async fn live(&self) -> Result<Live> {
        let mut state = self.state.lock().await;
        if let Some(running) = &state.running {
            if running.alive() {
                return Ok(running.live());
            }
            let why = format!(
                "the server exited{}{}",
                running.process.exit_note(),
                running.stderr.render()
            );
            tracing::warn!(mount = %self.name(), pid = ?running.pid, "mounted server exited; restarting it");
            // The supervisor has already ended what was left of its process tree.
            state.running = None;
            state.failed(why);
        }
        if let Some(wait) = state.backoff_remaining() {
            bail!(
                "mount '{}' is not running: it failed {} times in a row; next start attempt in {} s. Last error: {}",
                self.name(),
                state.failures,
                wait.as_secs().max(1),
                state.last_error.as_deref().unwrap_or("unknown")
            );
        }
        match self.spawn().await {
            Ok(running) => {
                let live = running.live();
                state.running = Some(running);
                Ok(live)
            }
            Err(e) => {
                tracing::error!(mount = %self.name(), "mounted server did not start: {e:#}");
                state.failed(format!("{e:#}"));
                Err(e)
            }
        }
    }

    async fn spawn(&self) -> Result<Running> {
        if let Some(pinned) = &self.config.sha256 {
            self.verify_pin(pinned).await?;
        }
        let mut cmd = tokio::process::Command::new(&self.command);
        cmd.args(&self.config.args)
            .envs(&self.config.env)
            .current_dir(&self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut wrap = CommandWrap::from(cmd);
        wrap.wrap(KillOnDrop);
        #[cfg(windows)]
        {
            // No console window, and no share in the host's console: Ctrl+C in the host terminal
            // must reach the host only, which then stops its mounts. The flag has to go through
            // the wrapper: JobObject sets the creation flags itself and would drop a direct one.
            wrap.wrap(process_wrap::tokio::CreationFlags(
                windows::Win32::System::Threading::CREATE_NO_WINDOW,
            ));
            wrap.wrap(process_wrap::tokio::JobObject);
        }
        #[cfg(unix)]
        wrap.wrap(process_wrap::tokio::ProcessGroup::leader());

        let mut child = wrap.spawn().with_context(|| {
            format!("mount '{}': start {}", self.name(), self.command.display())
        })?;
        let pid = child.id();
        // Dropping `child` on the error path ends the process (kill-on-drop, job object).
        let (Some(stdin), Some(stdout)) = (child.stdin().take(), child.stdout().take()) else {
            bail!(
                "mount '{}': the server's stdin/stdout are not available",
                self.name()
            );
        };
        let tail = Arc::new(StderrTail::default());
        if let Some(stderr) = child.stderr().take() {
            tokio::spawn(drain_stderr(stderr, tail.clone(), self.log_path(), pid));
        }
        // From here on the process is owned by its supervisor; dropping `process` stops it.
        let process = Process::supervise(child, self.name().to_string(), pid);

        let handshake = tokio::time::timeout(
            START_TIMEOUT,
            client_info().serve(AsyncRwTransport::new_client(stdout, stdin)),
        );
        let outcome = tokio::select! {
            biased;
            r = handshake => r.map_err(|_| None).and_then(|r| r.map_err(|e| Some(e.to_string()))),
            _ = process.exited.cancelled() => Err(Some(format!("it exited{}", process.exit_note()))),
        };
        let client = match outcome {
            Ok(client) => client,
            Err(Some(why)) => {
                settle_stderr().await;
                bail!(
                    "mount '{}': {} did not complete the MCP handshake ({why}). Is it an MCP server speaking \
                     stdio with these arguments?{}",
                    self.name(),
                    self.command.display(),
                    tail.render()
                )
            }
            Err(None) => bail!(
                "mount '{}': {} did not complete the MCP handshake within {} s{}",
                self.name(),
                self.command.display(),
                START_TIMEOUT.as_secs(),
                tail.render()
            ),
        };
        let server = client
            .peer_info()
            .and_then(|i| {
                i.server_info
                    .as_ref()
                    .map(|s| format!("{} {}", s.name, s.version))
            })
            .unwrap_or_else(|| "unknown".into());
        tracing::info!(mount = %self.name(), ?pid, %server, "mounted server started");
        Ok(Running {
            client: Arc::new(client),
            pid,
            stderr: tail,
            process,
        })
    }

    /// Refuses to start a binary that is not the one the owner pinned.
    async fn verify_pin(&self, pinned: &str) -> Result<()> {
        if self.command.components().count() <= 1 && !self.command.is_absolute() {
            bail!(
                "mount '{}': sha256 is pinned but command '{}' is a bare program name; use the full path \
                 so the pinned file is the one that runs",
                self.name(),
                self.command.display()
            );
        }
        let path = self.command.clone();
        let actual = tokio::task::spawn_blocking(move || super::sha256_file(&path))
            .await
            .context("hash task failed")??;
        if !actual.eq_ignore_ascii_case(pinned.trim()) {
            tracing::error!(
                mount = %self.name(),
                command = %self.command.display(),
                expected = %pinned,
                %actual,
                "refusing to start a mounted server whose SHA-256 does not match the pinned value"
            );
            bail!(
                "mount '{}': refusing to start {}: its SHA-256 is {actual}, but the host config pins {}. \
                 If the file was updated on purpose, re-add the mount (ai-remote mount remove / mount add) \
                 to pin the new hash",
                self.name(),
                self.command.display(),
                pinned.trim()
            );
        }
        Ok(())
    }

    /// Forgets the session `live` belongs to (if it is still the current one), records why, and
    /// stops its process tree. The next call starts a new server.
    async fn drop_session(&self, live: &Live, why: String, grace: Duration) {
        let running = {
            let mut state = self.state.lock().await;
            if !state
                .running
                .as_ref()
                .is_some_and(|r| Arc::ptr_eq(&r.client, &live.client))
            {
                return;
            }
            state.failed(why);
            state.running.take()
        };
        if let Some(running) = running {
            shutdown(running, grace).await;
        }
    }

    /// Turns a client error into an actionable message, and forgets a session whose process died
    /// or hung.
    async fn failed_call(&self, live: &Live, what: &str, e: ServiceError) -> anyhow::Error {
        match e {
            ServiceError::McpError(err) => {
                // The server is alive and answered with a protocol error (unknown tool, bad arguments).
                anyhow!("mount '{}' rejected '{what}': {}", self.name(), err.message)
            }
            ServiceError::Timeout { timeout } => {
                if live.answers_ping().await {
                    return anyhow!(
                        "'{what}' on mount '{}' did not finish within {} s and was cancelled",
                        self.name(),
                        timeout.as_secs()
                    );
                }
                tracing::warn!(
                    mount = %self.name(),
                    "mounted server hangs: no answer to a ping after '{what}' timed out"
                );
                let why = format!(
                    "the server stopped answering ('{what}' timed out, then a ping did too)"
                );
                // It does not read its input any more, so there is no point in a graceful stop.
                self.drop_session(live, why, Duration::ZERO).await;
                anyhow!(
                    "'{what}' on mount '{}' did not finish within {} s, and the server no longer answers at all; \
                     it was stopped and is started again on the next call",
                    self.name(),
                    timeout.as_secs()
                )
            }
            ServiceError::TransportClosed | ServiceError::TransportSend(_) => {
                settle_stderr().await;
                let mut state = self.state.lock().await;
                let current = state
                    .running
                    .as_ref()
                    .filter(|r| Arc::ptr_eq(&r.client, &live.client));
                let exit = current
                    .map(|r| r.process.exit_note())
                    .filter(|n| !n.is_empty());
                let exit = exit.unwrap_or_else(|| " (crashed or was stopped)".into());
                let tail = current.map(|r| r.stderr.render()).unwrap_or_default();
                if current.is_some() {
                    // The supervisor ends the rest of the process tree once the server has exited.
                    state.running = None;
                    state.failed(format!("the server exited during '{what}'{exit}{tail}"));
                }
                tracing::warn!(mount = %self.name(), "mounted server exited during '{what}'");
                anyhow!(
                    "mount '{}' stopped while handling '{what}'{exit}; it is started again on the next call{tail}",
                    self.name()
                )
            }
            other => anyhow!("'{what}' on mount '{}' failed: {other}", self.name()),
        }
    }
}

impl State {
    fn failed(&mut self, why: String) {
        self.failures += 1;
        self.last_failure = Some(Instant::now());
        self.last_error = Some(why);
    }

    fn succeeded(&mut self) {
        self.failures = 0;
        self.last_failure = None;
        self.last_error = None;
    }

    fn backoff_remaining(&self) -> Option<Duration> {
        let due = self.last_failure? + backoff(self.failures);
        due.checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
    }
}

/// Delay before the next start after `failures` consecutive failures: a single crash restarts at
/// once, a crash loop waits 1, 2, 4, ... up to 60 seconds.
fn backoff(failures: u32) -> Duration {
    if failures <= 1 {
        return Duration::ZERO;
    }
    Duration::from_secs(1u64 << (failures - 2).min(6)).min(MAX_BACKOFF)
}

fn client_info() -> ClientConfig {
    // No elicitation or sampling capability: a mounted server has no human to ask through us.
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("ai-remote", VERSION),
    )
}

/// Ends a session: closes the server's stdin, gives it `grace` to exit, then ends its process tree.
async fn shutdown(running: Running, grace: Duration) {
    let Running {
        client, process, ..
    } = running;
    match Arc::try_unwrap(client) {
        // Waits for the session to wind down; that closes the server's stdin.
        Ok(client) => {
            let _ = tokio::time::timeout(Duration::from_secs(5), client.cancel()).await;
        }
        // A call still holds the session: cancel it; the call fails as the session ends.
        Err(shared) => shared.cancellation_token().cancel(),
    }
    process.shutdown(grace).await;
}

/// The server process, seen from outside the supervisor task that owns it.
struct Process {
    /// Cancelled once the server process itself has exited (or was ended).
    exited: CancellationToken,
    /// How it exited, once it has.
    status: Arc<StdMutex<Option<String>>>,
    /// Asks the supervisor to end the process tree after giving the server this long to exit by
    /// itself. Dropping it does the same with [`STOP_GRACE`].
    stop: Option<oneshot::Sender<Duration>>,
    supervisor: Option<JoinHandle<()>>,
}

impl Process {
    fn supervise(child: Box<dyn ChildWrapper>, mount: String, pid: Option<u32>) -> Self {
        let exited = CancellationToken::new();
        let status = Arc::new(StdMutex::new(None));
        let (stop, stop_rx) = oneshot::channel();
        let supervisor = tokio::spawn(supervise(
            child,
            stop_rx,
            exited.clone(),
            status.clone(),
            mount,
            pid,
        ));
        Self {
            exited,
            status,
            stop: Some(stop),
            supervisor: Some(supervisor),
        }
    }

    /// ` (exit code: 3)`, or nothing while the exit status is unknown.
    fn exit_note(&self) -> String {
        self.status
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default()
    }

    /// Ends the process tree, giving the server `grace` to exit by itself first, and waits for it.
    async fn shutdown(mut self, grace: Duration) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(grace);
        }
        if let Some(task) = self.supervisor.take() {
            let _ = tokio::time::timeout(grace + KILL_TIMEOUT + Duration::from_secs(1), task).await;
        }
    }
}

/// Owns the server process: waits for it to exit (or for a stop), then ends whatever is left of
/// its process tree.
async fn supervise(
    mut child: Box<dyn ChildWrapper>,
    mut stop: oneshot::Receiver<Duration>,
    exited: CancellationToken,
    status: Arc<StdMutex<Option<String>>>,
    mount: String,
    pid: Option<u32>,
) {
    let record = |result: std::io::Result<std::process::ExitStatus>| {
        let text = match result {
            Ok(s) => s.to_string(),
            Err(e) => format!("exit status unknown: {e}"),
        };
        *status.lock().unwrap() = Some(text);
    };
    let stopping = {
        // The job / process-group wrapper's own wait() would also wait for everything the server
        // started; the child it wraps is the server process itself.
        let server = child.inner_mut().wait();
        tokio::select! {
            result = server => {
                record(result);
                None
            }
            grace = &mut stop => Some(grace.unwrap_or(STOP_GRACE)),
        }
    };
    match stopping {
        Some(grace) => {
            // Its stdin is closed by now, so a well-behaved server exits by itself.
            if let Ok(result) = tokio::time::timeout(grace, child.inner_mut().wait()).await {
                record(result);
            }
            end_tree(&mut child, &mount).await;
            exited.cancel();
        }
        None => {
            let how = status.lock().unwrap().clone().unwrap_or_default();
            tracing::warn!(mount = %mount, ?pid, status = %how, "mounted server exited");
            // Calls waiting on it fail now, not when some helper holding its stdout exits.
            exited.cancel();
            end_tree(&mut child, &mount).await;
        }
    }
}

/// Ends every process left in the server's job / process group (F14).
async fn end_tree(child: &mut Box<dyn ChildWrapper>, mount: &str) {
    match tokio::time::timeout(KILL_TIMEOUT, Box::into_pin(child.kill())).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::debug!(mount = %mount, "ending the mounted server's process tree: {e}")
        }
        Err(_) => {
            tracing::warn!(mount = %mount, "the mounted server's process tree did not end in time")
        }
    }
}

/// Marks a mount busy for idle tracking.
struct Busy<'a>(&'a Mount);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        let mut a = self.0.activity.lock().unwrap();
        a.in_flight = a.in_flight.saturating_sub(1);
        a.last_used = Instant::now();
    }
}

/// Sends `notifications/cancelled` for a request whose caller stopped waiting.
struct CancelOnDrop {
    peer: Option<Peer<RoleClient>>,
    id: RequestId,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let Some(peer) = self.peer.take() else { return };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let note = CancelledNotification::new(CancelledNotificationParam::new(
            Some(self.id.clone()),
            Some("the caller stopped waiting".into()),
        ));
        runtime.spawn(async move {
            let _ = peer.send_notification(note.into()).await;
        });
    }
}

/// Last lines a server wrote to stderr, for error messages.
#[derive(Default)]
struct StderrTail(StdMutex<TailInner>);

#[derive(Default)]
struct TailInner {
    lines: VecDeque<String>,
    partial: String,
}

impl StderrTail {
    fn feed(&self, chunk: &[u8]) {
        let text = String::from_utf8_lossy(chunk);
        let mut t = self.0.lock().unwrap();
        for piece in text.split_inclusive('\n') {
            let room = STDERR_LINE_MAX.saturating_sub(t.partial.len());
            t.partial.extend(piece.chars().take(room));
            if piece.ends_with('\n') {
                let line = std::mem::take(&mut t.partial);
                let line = line.trim_end();
                if !line.is_empty() {
                    if t.lines.len() == STDERR_TAIL_LINES {
                        t.lines.pop_front();
                    }
                    t.lines.push_back(line.to_string());
                }
            }
        }
    }

    fn render(&self) -> String {
        let t = self.0.lock().unwrap();
        let mut lines: Vec<&str> = t.lines.iter().map(String::as_str).collect();
        let partial = t.partial.trim_end();
        if !partial.is_empty() {
            lines.push(partial);
        }
        if lines.is_empty() {
            return String::new();
        }
        format!(
            "\nLast lines the server wrote to stderr:\n  {}",
            lines.join("\n  ")
        )
    }
}

/// Gives the stderr reader a moment to catch a dying server's last words.
async fn settle_stderr() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

/// Copies the server's stderr to its log file and keeps the tail. Runs until the pipe closes.
async fn drain_stderr(
    mut stderr: ChildStderr,
    tail: Arc<StderrTail>,
    log: PathBuf,
    pid: Option<u32>,
) {
    let mut file = open_log(&log).await;
    if let Some(f) = file.as_mut() {
        let header = format!(
            "--- started pid {} at unix {} ---\n",
            pid.unwrap_or(0),
            ar_core::unix_now()
        );
        let _ = f.write_all(header.as_bytes()).await;
    }
    let mut buf = vec![0u8; 8192];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Some(f) = file.as_mut() {
                    let _ = f.write_all(&buf[..n]).await;
                }
                tail.feed(&buf[..n]);
            }
        }
    }
    if let Some(f) = file.as_mut() {
        let _ = f.flush().await;
    }
}

async fn open_log(path: &Path) -> Option<tokio::fs::File> {
    if tokio::fs::metadata(path)
        .await
        .is_ok_and(|m| m.len() > LOG_ROTATE_BYTES)
    {
        let _ = tokio::fs::rename(path, path.with_extension("log.old")).await;
    }
    match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!(log = %path.display(), "cannot write the mounted server's stderr log: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(0), Duration::ZERO);
        assert_eq!(
            backoff(1),
            Duration::ZERO,
            "a single crash restarts at once"
        );
        assert_eq!(backoff(2), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(2));
        assert_eq!(backoff(6), Duration::from_secs(16));
        assert_eq!(backoff(8), MAX_BACKOFF);
        assert_eq!(backoff(1000), MAX_BACKOFF);
    }

    #[test]
    fn backoff_window_tracks_failures() {
        let mut s = State::default();
        assert!(s.backoff_remaining().is_none());
        s.failed("boom".into());
        assert!(
            s.backoff_remaining().is_none(),
            "first failure: retry immediately"
        );
        s.failed("boom".into());
        assert!(s.backoff_remaining().is_some());
        s.succeeded();
        assert!(s.backoff_remaining().is_none());
        assert_eq!(s.failures, 0);
    }

    #[test]
    fn stderr_tail_keeps_the_last_lines() {
        let t = StderrTail::default();
        assert_eq!(t.render(), "");
        t.feed(b"first\nsec");
        t.feed(b"ond\n");
        for i in 0..STDERR_TAIL_LINES {
            t.feed(format!("line {i}\n").as_bytes());
        }
        t.feed(b"partial");
        let r = t.render();
        assert!(!r.contains("first") && !r.contains("second"), "{r}");
        assert!(r.contains("line 19") && r.ends_with("partial"), "{r}");
        t.feed(&vec![b'x'; 10_000]);
        assert!(
            t.render().len() < 20 * (STDERR_LINE_MAX + 4) + 200,
            "long lines are capped"
        );
    }
}
