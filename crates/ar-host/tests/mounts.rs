//! End-to-end test of mounted MCP servers against a real child process.
//!
//! This binary has its own `main` (harness = false) because it is also the mock MCP server it
//! mounts: started with `AR_MOCK_MCP=1` it serves a handful of tools on stdio. With
//! `AR_MOCK_HOST=1` it plays a host that mounts the mock and then waits to be killed, which is how
//! "the child dies with the host" is checked without a real host and network.
//!
//! Run: `cargo test -p ar-host --test mounts [-- <name filter>]`.

use std::{
    collections::BTreeMap,
    future::Future,
    io::BufRead,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use ar_core::{
    Level, Profile,
    config::{Action, HostConfig, MountConfig, MountStart, MountTool},
};
use ar_host::{mounts, tools::Registry};
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::{Value, json};

/// A 1x1 PNG.
const PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

fn main() {
    if std::env::var_os("AR_MOCK_MCP").is_some() {
        return mock::serve();
    }
    if std::env::var_os("AR_MOCK_SLEEPER").is_some() {
        // Short enough that a failed run does not leave it around (holding the test's output
        // pipe) for long.
        std::thread::sleep(Duration::from_secs(90));
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    if std::env::var_os("AR_MOCK_HOST").is_some() {
        return runtime.block_on(fake_host()).unwrap();
    }

    type Case = fn(PathBuf) -> Pin<Box<dyn Future<Output = Result<()>>>>;
    let cases: &[(&str, Case)] = &[
        ("extend_exposes_only_allowlisted_tools", |d| Box::pin(extend_exposes_only_allowlisted_tools(d))),
        ("forwards_text_structured_image_and_errors", |d| Box::pin(forwards_text_structured_image_and_errors(d))),
        ("restarts_after_a_crash", |d| Box::pin(restarts_after_a_crash(d))),
        ("backs_off_when_start_keeps_failing", |d| Box::pin(backs_off_when_start_keeps_failing(d))),
        ("dropped_call_is_cancelled_and_client_stays_usable", |d| {
            Box::pin(dropped_call_is_cancelled_and_client_stays_usable(d))
        }),
        ("sha256_pin_is_enforced", |d| Box::pin(sha256_pin_is_enforced(d))),
        ("stop_kills_the_process_tree", |d| Box::pin(stop_kills_the_process_tree(d))),
        ("lazy_mount_stops_after_discovery_and_when_idle", |d| {
            Box::pin(lazy_mount_stops_after_discovery_and_when_idle(d))
        }),
        ("killing_the_host_kills_the_mount", |d| Box::pin(killing_the_host_kills_the_mount(d))),
    ];
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let mut failed = 0;
    for (name, case) in cases {
        if filter.as_deref().is_some_and(|f| !name.contains(f)) {
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let result = runtime.block_on(case(dir.path().to_path_buf()));
        match result {
            Ok(()) => println!("test {name} ... ok ({} ms)", started.elapsed().as_millis()),
            Err(e) => {
                failed += 1;
                println!("test {name} ... FAILED\n{e:#}\n");
            }
        }
    }
    if failed > 0 {
        println!("{failed} test(s) failed");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers

fn mock_mount(name: &str, dir: &Path, tools: &[(&str, MountTool)]) -> MountConfig {
    MountConfig {
        name: name.into(),
        command: std::env::current_exe().unwrap().display().to_string(),
        args: vec![],
        env: BTreeMap::from([("AR_MOCK_MCP".into(), "1".into()), ("AR_MOCK_DIR".into(), dir.display().to_string())]),
        sha256: None,
        start: MountStart::Eager,
        needs_desktop: false,
        tools: tools.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
    }
}

fn observe() -> MountTool {
    MountTool::Level(Level::Observe)
}

fn args(v: Value) -> JsonObject {
    match v {
        Value::Object(o) => o,
        _ => panic!("arguments must be an object"),
    }
}

fn text_of(r: &CallToolResult) -> String {
    r.content.iter().filter_map(|c| c.as_text()).map(|t| t.text.clone()).collect::<Vec<_>>().join("\n")
}

async fn pid_of(mount: &mounts::Mount) -> Result<u32> {
    let r = mount.call("pid", JsonObject::new()).await?;
    text_of(&r).trim().parse().context("pid tool returned no number")
}

fn alive(pid: u32) -> bool {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    sys.process(sysinfo::Pid::from_u32(pid)).is_some()
}

async fn wait_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    check()
}

// ---------------------------------------------------------------------------------------------
// Cases

async fn extend_exposes_only_allowlisted_tools(dir: PathBuf) -> Result<()> {
    let profile = Profile::at(dir.join("profile"))?;
    let mock = mock_mount(
        "mock",
        &dir,
        &[
            ("echo", observe()),
            ("add", MountTool::Level(Level::Control)),
            (
                "noted",
                MountTool::Detailed {
                    level: Level::Control,
                    approve: Some(Action::Ask),
                    description: Some("Curated.".into()),
                },
            ),
            ("crash", MountTool::Level(Level::Admin)),
            ("not_offered", observe()),
        ],
    );
    // "system" + "info" would shadow the built-in system_info: refused.
    let shadow = mock_mount("system", &dir, &[("info", observe()), ("echo", observe())]);
    // A broken mount must not stop the others.
    let mut broken = mock_mount("broken", &dir, &[("echo", observe())]);
    broken.command = dir.join("does-not-exist.exe").display().to_string();
    let config = HostConfig { mounts: vec![mock, shadow, broken], ..Default::default() };

    let mut registry = Registry::builtin();
    let builtin_count = registry.iter().count();
    mounts::extend(&mut registry, &config, &profile).await;
    let names: Vec<String> = registry.iter().map(|t| t.spec().name.clone()).collect();

    for expected in ["mock_echo", "mock_add", "mock_noted", "mock_crash", "system_echo"] {
        ensure!(names.iter().any(|n| n == expected), "{expected} missing from {names:?}");
    }
    for hidden in ["mock_secret", "mock_not_offered", "mock_pid", "broken_echo"] {
        ensure!(!names.iter().any(|n| n == hidden), "{hidden} must not be exposed: {names:?}");
    }
    ensure!(names.len() == builtin_count + 5, "unexpected tool set {names:?}");
    let system_info = registry.get("system_info").context("system_info")?;
    ensure!(system_info.spec().level == Level::Observe && !system_info.spec().description.contains("mock"));

    let add = registry.get("mock_add").unwrap();
    let add = add.spec();
    ensure!(add.level == Level::Control && add.default_action == Action::Allow);
    ensure!(add.input_schema["properties"].get("intent").is_some(), "control tools take an intent");
    let noted = registry.get("mock_noted").unwrap();
    let noted = noted.spec();
    ensure!(noted.default_action == Action::Ask && noted.description == "Curated.");
    let crash = registry.get("mock_crash").unwrap();
    ensure!(crash.spec().level == Level::Admin && crash.spec().default_action == Action::Ask);
    let echo = registry.get("mock_echo").unwrap();
    ensure!(echo.spec().read_only, "the server's read-only hint is passed on");
    ensure!(echo.spec().to_mcp().description.unwrap_or_default().contains("[requires level: observe]"));
    Ok(())
}

async fn forwards_text_structured_image_and_errors(dir: PathBuf) -> Result<()> {
    let mount = mounts::Mount::new(mock_mount("mock", &dir, &[]), &dir);

    let r = mount.call("echo", args(json!({"text": "hello"}))).await?;
    ensure!(r.is_error != Some(true));
    let s = r.structured_content.clone().context("structured content")?;
    ensure!(s["text"] == "hello" && s["extra"] == json!([]), "echo: {s}");

    let r = mount.call("add", args(json!({"a": 2, "b": 40}))).await?;
    ensure!(r.structured_content == Some(json!({"sum": 42})), "add: {:?}", r.structured_content);

    let r = mount.call("image", JsonObject::new()).await?;
    let image = r.content.iter().find_map(|c| c.as_image()).context("no image block")?;
    ensure!(image.data == PNG_B64 && image.mime_type == "image/png", "image passed through unchanged");

    let r = mount.call("fail", JsonObject::new()).await?;
    ensure!(r.is_error == Some(true) && text_of(&r) == "nope", "isError passes through: {r:?}");

    // Bad arguments: the server's own tool error comes back as a result the agent can read.
    let r = mount.call("add", args(json!({"a": "two"}))).await?;
    ensure!(r.is_error == Some(true), "bad arguments: {r:?}");

    // A protocol error is turned into an actionable tool error.
    let e = mount.call("no_such_tool", JsonObject::new()).await.unwrap_err();
    ensure!(format!("{e:#}").contains("rejected 'no_such_tool'"), "{e:#}");

    // The server's stderr goes to the mount's log.
    let log = std::fs::read_to_string(mount.log_path())?;
    ensure!(log.contains("mock: serving") && log.contains("--- started pid"), "log: {log}");
    mount.stop().await;
    Ok(())
}

async fn restarts_after_a_crash(dir: PathBuf) -> Result<()> {
    let mount = mounts::Mount::new(mock_mount("mock", &dir, &[]), &dir);
    let first = pid_of(&mount).await?;
    let e = mount.call("crash", JsonObject::new()).await.unwrap_err().to_string();
    ensure!(e.contains("stopped while handling 'crash'"), "{e}");
    ensure!(e.contains("mock: about to crash"), "the error carries the server's last stderr lines: {e}");
    ensure!(wait_until(Duration::from_secs(5), || !alive(first)).await, "crashed process still listed");

    // Next call restarts it.
    let second = pid_of(&mount).await?;
    ensure!(second != first, "expected a new process");

    // A crash noticed between calls (not during one) is handled the same way.
    let _ = mount.call("crash", JsonObject::new()).await;
    let third = pid_of(&mount).await?;
    ensure!(third != second);
    mount.stop().await;
    Ok(())
}

async fn backs_off_when_start_keeps_failing(dir: PathBuf) -> Result<()> {
    let mut cfg = mock_mount("mock", &dir, &[]);
    cfg.env.insert("AR_MOCK_EXIT_AT_START".into(), "1".into());
    let mount = mounts::Mount::new(cfg, &dir);

    let e = format!("{:#}", mount.list_tools().await.unwrap_err());
    ensure!(e.contains("did not complete the MCP handshake"), "{e}");
    ensure!(e.contains("mock: refusing to start"), "start errors carry stderr: {e}");
    // A single failure retries at once; the second failure starts the backoff.
    let e = format!("{:#}", mount.call("echo", JsonObject::new()).await.unwrap_err());
    ensure!(e.contains("did not complete the MCP handshake"), "{e}");
    let started = Instant::now();
    let e = format!("{:#}", mount.call("echo", JsonObject::new()).await.unwrap_err());
    ensure!(e.contains("failed 2 times in a row") && e.contains("next start attempt"), "{e}");
    ensure!(started.elapsed() < Duration::from_millis(500), "calls in the backoff window fail fast");
    Ok(())
}

async fn dropped_call_is_cancelled_and_client_stays_usable(dir: PathBuf) -> Result<()> {
    let mount = mounts::Mount::new(mock_mount("mock", &dir, &[]), &dir);
    let pid = pid_of(&mount).await?;
    // The gate drops the call future when the session ends; a timeout does the same here.
    let dropped =
        tokio::time::timeout(Duration::from_millis(300), mount.call("sleep", args(json!({"ms": 30_000})))).await;
    ensure!(dropped.is_err(), "the sleep call should still have been running");
    let marker = dir.join("cancelled");
    ensure!(wait_until(Duration::from_secs(5), || marker.exists()).await, "the server never saw the cancellation");

    let started = Instant::now();
    let r = mount.call("echo", args(json!({"text": "still here"}))).await?;
    ensure!(r.structured_content.unwrap()["text"] == "still here");
    ensure!(started.elapsed() < Duration::from_secs(2), "client wedged after a dropped call");
    ensure!(pid_of(&mount).await? == pid, "no restart was needed");
    mount.stop().await;
    Ok(())
}

async fn sha256_pin_is_enforced(dir: PathBuf) -> Result<()> {
    let mut cfg = mock_mount("mock", &dir, &[]);
    cfg.sha256 = Some("0".repeat(64));
    let e = format!("{:#}", mounts::Mount::new(cfg.clone(), &dir).list_tools().await.unwrap_err());
    ensure!(e.contains("refusing to start") && e.contains("SHA-256"), "{e}");

    cfg.sha256 = Some(mounts::sha256_file(&std::env::current_exe()?)?.to_uppercase());
    let mount = mounts::Mount::new(cfg, &dir);
    let tools = mount.list_tools().await?;
    ensure!(tools.iter().any(|t| t.name == "echo"), "pinned hash matches (case-insensitive)");
    mount.stop().await;
    Ok(())
}

async fn stop_kills_the_process_tree(dir: PathBuf) -> Result<()> {
    let mount = mounts::Mount::new(mock_mount("mock", &dir, &[]), &dir);
    let server = pid_of(&mount).await?;
    let r = mount.call("spawn_child", JsonObject::new()).await?;
    let grandchild: u32 = text_of(&r).trim().parse()?;
    ensure!(alive(server) && alive(grandchild));
    mount.stop().await;
    ensure!(wait_until(Duration::from_secs(10), || !alive(server)).await, "server survived stop");
    ensure!(wait_until(Duration::from_secs(10), || !alive(grandchild)).await, "grandchild survived stop");
    Ok(())
}

async fn lazy_mount_stops_after_discovery_and_when_idle(dir: PathBuf) -> Result<()> {
    let profile = Profile::at(dir.join("profile"))?;
    let mut cfg = mock_mount("lazy", &dir, &[("echo", observe())]);
    cfg.start = MountStart::Lazy;
    let mut registry = Registry::default();
    mounts::extend(&mut registry, &HostConfig { mounts: vec![cfg.clone()], ..Default::default() }, &profile).await;
    ensure!(registry.get("lazy_echo").is_some(), "tools are known from the discovery start");
    let started: Vec<u32> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok()?.file_name().to_str()?.strip_prefix("started-")?.parse().ok())
        .collect();
    ensure!(started.len() == 1, "one discovery start expected, saw {started:?}");
    ensure!(wait_until(Duration::from_secs(10), || !alive(started[0])).await, "discovery process kept running");

    // Idle stop on a mount used directly.
    let mount = mounts::Mount::new(cfg, &dir);
    ensure!(!mount.stop_if_idle(Duration::ZERO).await, "nothing to stop before the first call");
    let pid = pid_of(&mount).await?;
    ensure!(!mount.stop_if_idle(Duration::from_secs(3600)).await, "recently used");
    ensure!(mount.stop_if_idle(Duration::ZERO).await);
    ensure!(wait_until(Duration::from_secs(10), || !alive(pid)).await, "idle server kept running");
    ensure!(pid_of(&mount).await? != pid, "the next call starts it again");
    mount.stop().await;
    Ok(())
}

async fn killing_the_host_kills_the_mount(dir: PathBuf) -> Result<()> {
    let mut host = std::process::Command::new(std::env::current_exe()?)
        .env("AR_MOCK_HOST", "1")
        .env("AR_MOCK_DIR", &dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;
    let mut line = String::new();
    std::io::BufReader::new(host.stdout.take().unwrap()).read_line(&mut line)?;
    let pids: Vec<u32> = line.split_whitespace().skip(1).filter_map(|s| s.parse().ok()).collect();
    let [server, grandchild] = pids[..] else { bail!("fake host printed {line:?}") };
    ensure!(alive(server) && alive(grandchild));

    host.kill()?; // TerminateProcess / SIGKILL: no cleanup code runs in the host.
    let _ = host.wait();
    ensure!(wait_until(Duration::from_secs(10), || !alive(server)).await, "server outlived the host");
    // On Windows the job object takes the whole tree. On Unix the server exits on stdin EOF, but
    // its own children would need a parent-death signal, which is not implemented.
    if cfg!(windows) {
        ensure!(wait_until(Duration::from_secs(10), || !alive(grandchild)).await, "grandchild outlived the host");
    }
    Ok(())
}

/// `AR_MOCK_HOST=1`: mounts the mock, starts a grandchild, prints `pids <server> <grandchild>`
/// and waits to be killed.
async fn fake_host() -> Result<()> {
    let dir = PathBuf::from(std::env::var("AR_MOCK_DIR")?);
    let mount = mounts::Mount::new(mock_mount("mock", &dir, &[]), &dir);
    let server = pid_of(&mount).await?;
    let r = mount.call("spawn_child", JsonObject::new()).await?;
    println!("pids {server} {}", text_of(&r).trim());
    let _keep: Arc<mounts::Mount> = mount;
    tokio::time::sleep(Duration::from_secs(600)).await;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The mock MCP server

mod mock {
    use std::{path::PathBuf, time::Duration};

    use rmcp::{
        ErrorData, RoleServer, ServerHandler, ServiceExt,
        handler::server::wrapper::Parameters,
        model::{CallToolResult, ContentBlock},
        service::RequestContext,
        tool, tool_handler, tool_router,
    };
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::{Map, Value, json};

    use super::PNG_B64;

    fn dir() -> Option<PathBuf> {
        std::env::var_os("AR_MOCK_DIR").map(PathBuf::from)
    }

    pub fn serve() {
        if std::env::var_os("AR_MOCK_EXIT_AT_START").is_some() {
            eprintln!("mock: refusing to start (test)");
            std::process::exit(5);
        }
        if let Some(dir) = dir() {
            let _ = std::fs::write(dir.join(format!("started-{}", std::process::id())), b"");
        }
        eprintln!("mock: serving");
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let running = Mock.serve(rmcp::transport::stdio()).await.expect("serve");
            let _ = running.waiting().await;
        });
    }

    #[derive(Debug, Clone, Default)]
    struct Mock;

    #[derive(Debug, Deserialize, JsonSchema)]
    struct EchoArgs {
        text: String,
        /// Anything else the caller sent (reported back so the test can see what was forwarded).
        #[serde(flatten)]
        rest: Map<String, Value>,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    struct AddArgs {
        a: i64,
        b: i64,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    struct SleepArgs {
        ms: u64,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    struct NotedArgs {
        /// This tool declares intent itself, so it must arrive.
        intent: Option<String>,
        x: Option<i64>,
    }

    #[tool_router]
    impl Mock {
        #[tool(description = "Echo text and list any other arguments received", annotations(read_only_hint = true))]
        async fn echo(&self, Parameters(a): Parameters<EchoArgs>) -> Result<CallToolResult, ErrorData> {
            let extra: Vec<&String> = a.rest.keys().collect();
            Ok(CallToolResult::structured(json!({ "text": a.text, "extra": extra })))
        }

        #[tool(description = "Add two numbers")]
        async fn add(&self, Parameters(a): Parameters<AddArgs>) -> Result<CallToolResult, ErrorData> {
            Ok(CallToolResult::structured(json!({ "sum": a.a + a.b })))
        }

        #[tool(description = "Return a tiny PNG")]
        async fn image(&self) -> Result<CallToolResult, ErrorData> {
            Ok(CallToolResult::success(vec![ContentBlock::text("1x1 png"), ContentBlock::image(PNG_B64, "image/png")]))
        }

        #[tool(description = "Always fails")]
        async fn fail(&self) -> Result<CallToolResult, ErrorData> {
            Ok(CallToolResult::error(vec![ContentBlock::text("nope")]))
        }

        #[tool(description = "Exit the server process without answering")]
        async fn crash(&self) -> Result<CallToolResult, ErrorData> {
            eprintln!("mock: about to crash");
            std::process::exit(3);
        }

        #[tool(description = "Sleep, or stop early when the call is cancelled")]
        async fn sleep(&self, Parameters(a): Parameters<SleepArgs>, ctx: RequestContext<RoleServer>) -> String {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(a.ms)) => "slept".into(),
                _ = ctx.ct.cancelled() => {
                    if let Some(dir) = dir() {
                        let _ = std::fs::write(dir.join("cancelled"), b"");
                    }
                    "cancelled".into()
                }
            }
        }

        #[tool(description = "Process id of this server")]
        async fn pid(&self) -> String {
            std::process::id().to_string()
        }

        #[tool(description = "Start a long-running child process and return its pid")]
        async fn spawn_child(&self) -> Result<String, ErrorData> {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .env_remove("AR_MOCK_MCP")
                .env("AR_MOCK_SLEEPER", "1")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            Ok(child.id().to_string())
        }

        #[tool(description = "Report the intent it was given")]
        async fn noted(&self, Parameters(a): Parameters<NotedArgs>) -> Result<CallToolResult, ErrorData> {
            Ok(CallToolResult::structured(json!({ "intent": a.intent, "x": a.x })))
        }

        #[tool(description = "Not on any allowlist")]
        async fn secret(&self) -> String {
            "secret".into()
        }
    }

    #[tool_handler(name = "ar-mock-mcp", version = "0.0.0")]
    impl ServerHandler for Mock {}
}
