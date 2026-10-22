//! An agent's HTML page with its scripts running, in a window of its own (the chat panel's preview
//! shows pages without scripts; "Run scripts" opens this).
//!
//! Why a separate window from a loopback server, not the app's webview or a custom protocol: in the
//! app's webview any frame reaches the native message handler behind Tauri's IPC, and Tauri treats a
//! page from a custom protocol the app registers as the app's own (every app command allowed, see
//! `Webview::is_local_url`). A page from `http://127.0.0.1:<port>` is a remote origin to Tauri: its
//! IPC calls go through the ACL, and no capability names this window (the app's capability is for
//! `main` only), so app and plugin commands are refused; channel data is kept per webview, so it
//! cannot read the main window's either.
//!
//! The server listens on 127.0.0.1 only, on a port the system picks, and serves each page at a
//! random path, nothing else. Its policy lets inline scripts and a few public script CDNs (the
//! list Orca uses for agent pages) run, and lets the page request nothing else: no `fetch` or
//! XHR, no frames, no forms, images only from itself, `data:` and `blob:`. The window is
//! incognito (no storage shared with the app), may not navigate away from its page and may not
//! open other windows. A page is dropped when its window closes.

use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use tauri::{WebviewUrl, WebviewWindowBuilder, webview::NewWindowResponse};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

/// The policy of a page with scripts: scripts inline and from a few public CDNs, nothing fetched.
pub const SCRIPT_POLICY: &str = "default-src 'none'; \
    script-src 'unsafe-inline' 'unsafe-eval' https://cdn.jsdelivr.net https://unpkg.com https://cdnjs.cloudflare.com https://esm.sh; \
    style-src 'unsafe-inline' https://cdn.jsdelivr.net https://unpkg.com https://cdnjs.cloudflare.com https://fonts.googleapis.com; \
    font-src data: https://fonts.gstatic.com; img-src 'self' data: blob:; media-src data: blob:; \
    connect-src 'none'; frame-src 'none'; worker-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'";

/// Most pages kept at once (each has its window); the oldest is dropped first.
const MAX_PAGES: usize = 16;

struct Server {
    port: u16,
    pages: Mutex<Vec<(String, Arc<Vec<u8>>)>>,
    /// The last paths requested, for the end-to-end test.
    log: Mutex<Vec<String>>,
}

static SERVER: OnceLock<Arc<Server>> = OnceLock::new();
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn server() -> Result<Arc<Server>> {
    if let Some(s) = SERVER.get() {
        return Ok(s.clone());
    }
    let _one = STARTING.lock().await;
    if let Some(s) = SERVER.get() {
        return Ok(s.clone());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.context("cannot listen on 127.0.0.1")?;
    let server = Arc::new(Server {
        port: listener.local_addr()?.port(),
        pages: Mutex::new(Vec::new()),
        log: Mutex::new(Vec::new()),
    });
    let s = server.clone();
    tauri::async_runtime::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let s = s.clone();
            tauri::async_runtime::spawn(async move {
                let _ = serve(&s, stream).await;
            });
        }
    });
    let _ = SERVER.set(server.clone());
    Ok(server)
}

/// Answers one request (and closes the connection): the page at its path, 404 for anything else.
async fn serve(server: &Server, stream: TcpStream) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(10), (&mut reader).take(8192).read_line(&mut line)).await??;
    // The headers are not needed; read them so the client is not cut off mid-request.
    loop {
        let mut h = String::new();
        let n = tokio::time::timeout(std::time::Duration::from_secs(10), (&mut reader).take(8192).read_line(&mut h))
            .await??;
        if n == 0 || h == "\r\n" || h == "\n" {
            break;
        }
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    {
        let mut log = server.log.lock().unwrap();
        if log.len() >= 64 {
            log.remove(0);
        }
        log.push(path.chars().take(200).collect());
    }
    let page = (method == "GET")
        .then(|| server.pages.lock().unwrap().iter().find(|(p, _)| p == path).map(|(_, b)| b.clone()))
        .flatten();
    let head = |status: &str, kind: &str, len: usize| {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {len}\r\nContent-Security-Policy: {SCRIPT_POLICY}\r\n\
             Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
             Cross-Origin-Opener-Policy: same-origin\r\nConnection: close\r\n\r\n"
        )
    };
    match page {
        Some(bytes) => {
            write.write_all(head("200 OK", "text/html; charset=utf-8", bytes.len()).as_bytes()).await?;
            write.write_all(&bytes).await?;
        }
        None => write.write_all(head("404 Not Found", "text/plain", 0).as_bytes()).await?,
    }
    write.shutdown().await?;
    Ok(())
}

fn random_hex() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("random bytes");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Opens `html` (an agent's page, `name` its file name) with its scripts running, in a window of
/// its own.
pub async fn open(app: &tauri::AppHandle, name: &str, html: Vec<u8>) -> Result<()> {
    let server = server().await?;
    let token = random_hex();
    let path = format!("/{token}/");
    {
        let mut pages = server.pages.lock().unwrap();
        if pages.len() >= MAX_PAGES {
            pages.remove(0);
        }
        pages.push((path.clone(), Arc::new(html)));
    }
    let url: tauri::Url = format!("http://127.0.0.1:{}{path}", server.port).parse()?;
    let label = format!("preview-{}", &token[..12]);
    let allowed = url.clone();
    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(url))
        .title(name)
        .inner_size(1100.0, 800.0)
        .incognito(true)
        // Its own page only: links and redirects go nowhere, no other windows.
        .on_navigation(move |u| u.origin() == allowed.origin() && u.path() == allowed.path())
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .build()
        .context("cannot open the preview window")?;
    // Debug smoke runs keep every window of the app off the person's screen.
    #[cfg(debug_assertions)]
    if std::env::var_os("AI_REMOTE_SMOKE").is_some() {
        let _ = window.set_focusable(false);
        let _ = window.set_position(tauri::PhysicalPosition::new(-8000, -8000));
    }
    let pages = server.clone();
    window.on_window_event(move |e| {
        if let tauri::WindowEvent::Destroyed = e {
            pages.pages.lock().unwrap().retain(|(p, _)| p != &path);
        }
    });
    Ok(())
}

/// The last paths the server was asked for (the end-to-end test checks what a page did).
pub fn requests() -> Vec<String> {
    SERVER.get().map(|s| s.log.lock().unwrap().clone()).unwrap_or_default()
}

/// Only HTML pages open with their scripts (by the file's extension).
pub fn check_kind(path: &str) -> Result<()> {
    let lower = path.to_ascii_lowercase();
    if !(lower.ends_with(".html") || lower.ends_with(".htm")) {
        bail!("only HTML pages open with their scripts");
    }
    Ok(())
}
