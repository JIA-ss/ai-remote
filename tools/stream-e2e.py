#!/usr/bin/env python3
"""End-to-end live-view measurement on one machine: an isolated host and controller pair over real
QUIC, and `ai-remote view` reads the stream through the controller agent for each configuration.

Screen capture only produces frames when the screen changes, so a small animated window (a
throwaway-profile Edge/Chrome app window, `--animate`) keeps the display busy at its refresh rate
while measuring; it is closed afterwards. The stream itself stays in memory.

  python tools/stream-e2e.py [--cli target/release/ai-remote.exe] [--seconds 6] [--animate]

Reports per configuration: codec, size, fps, bitrate, first-frame time and frame-arrival gaps,
as seen by the controller (no decode/display; see tools/webcodecs-decode-check.mjs for decoding).
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXE = ".exe" if os.name == "nt" else ""
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--cli", type=Path, default=ROOT / f"target/release/ai-remote{EXE}")
parser.add_argument("--host-cli", type=Path,
                    help="a different ai-remote for the host side (e.g. an older release, for compatibility)")
parser.add_argument("--seconds", type=int, default=6)
parser.add_argument("--animate", action="store_true", help="show a small animated window while measuring")
parser.add_argument("--browser", default=r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe")
parser.add_argument("--out", type=Path, default=ROOT / "target/stream-e2e")
parser.add_argument("--desktop", type=Path, help="also measure this desktop app build (debug, for its smoke mode)")
parser.add_argument("--qualities", default="balanced,smooth,detail,ultra")
parser.add_argument("--skip-cli", action="store_true", help="only the desktop measurement")
parser.add_argument("--pattern", metavar="WxH",
                    help="the host streams a synthetic scrolling desktop of this size instead of the screen "
                         "(AI_REMOTE_TEST_PATTERN; Windows hosts): repeatable, full motion, no screen read")
parser.add_argument("--link-mbps", type=float,
                    help="the controller reads live video no faster than this (AI_REMOTE_TEST_LINK_KBPS): a slow "
                         "network, with QUIC's windows filling as on one")
parser.add_argument("--only", help="comma-separated substrings: run only the CLI configurations whose label matches")
parser.add_argument("--pointer", action="store_true",
                    help="the CLI asks for the pointer separately, as the desktop app does (counts its messages)")
parser.add_argument("--stress", action="store_true",
                    help="a larger animated window of random noise, so the stream runs at its bitrate cap")
options = parser.parse_args()
options.out.mkdir(parents=True, exist_ok=True)
if options.link_mbps:
    # Every controller process started from here (the agent, by `pair`) inherits it.
    os.environ["AI_REMOTE_TEST_LINK_KBPS"] = str(int(options.link_mbps * 1000))

# (label, max_fps, max_long_edge, bitrate_kbps, codecs)
CONFIGS = [
    ("1080p30 h264 (old default)", 30, 1920, 6000, ""),
    ("1080p60 (balanced)", 60, 1920, 12000, "hevc,av1,h264"),
    ("1440p120 (smooth)", 120, 2560, 25000, "hevc,av1,h264"),
    ("4K60 (detail)", 60, 3840, 40000, "hevc,av1,h264"),
    ("4K60 av1", 60, 3840, 40000, "av1,hevc,h264"),
    ("4K60 h264", 60, 3840, 40000, "h264"),
    ("native 240 (ultra)", 240, 8192, 150000, "hevc,av1,h264"),
    ("native 240 av1", 240, 8192, 150000, "av1,hevc,h264"),
]

ANIMATION = """<!doctype html><title>ai-remote stream test</title>
<body style="margin:0;background:#111"><canvas id=c width=300 height=200></canvas><script>
const c = document.getElementById('c').getContext('2d'); let t = 0;
(function frame() { t++; c.fillStyle = `hsl(${t * 7 % 360} 70% 50%)`; c.fillRect(0, 0, 300, 200);
  c.fillStyle = '#fff'; c.font = '28px monospace'; c.fillText('frame ' + t, 20, 110); requestAnimationFrame(frame); })();
</script>"""


NOISE = """<!doctype html><title>ai-remote stream stress</title>
<body style="margin:0;background:#000;overflow:hidden"><canvas id=c></canvas><script>
const canvas = document.getElementById('c'); canvas.width = innerWidth * devicePixelRatio; canvas.height = innerHeight * devicePixelRatio;
canvas.style.width = innerWidth + 'px'; canvas.style.height = innerHeight + 'px';
const c = canvas.getContext('2d'); const img = c.createImageData(canvas.width, canvas.height); const px = new Uint32Array(img.data.buffer);
let seed = 1;
(function frame() { for (let i = 0; i < px.length; i++) { seed = (seed * 1664525 + 1013904223) >>> 0; px[i] = seed | 0xff000000; }
  c.putImageData(img, 0, 0); requestAnimationFrame(frame); })();
</script>"""


def run(*args, timeout=60):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"{args[1:4]} failed ({result.returncode}): {result.stderr[-800:]}")
    return result.stdout


def wait_for(check, what, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = check()
            if value:
                return value
        except Exception:
            pass
        time.sleep(0.2)
    raise TimeoutError(what)


report = {"platform": sys.platform, "seconds": options.seconds, "animated": options.animate, "pattern": options.pattern,
          "link_mbps": options.link_mbps, "results": []}
with tempfile.TemporaryDirectory(prefix="ar-stream-", ignore_cleanup_errors=True) as tmp:
    base = Path(tmp)
    host_home, ctl_home = base / "host", base / "controller"
    log = open(options.out / "host.log", "w")
    env = {**os.environ, "RUST_LOG": os.environ.get("RUST_LOG", "ar_host=info")}
    if options.pattern:
        env["AI_REMOTE_TEST_PATTERN"] = options.pattern
    host_cli = options.host_cli or options.cli
    host = subprocess.Popen([str(host_cli), "--home", str(host_home), "host", "--no-pair-code"],
                            stdout=log, stderr=log, env=env)
    browser = None
    try:
        ticket = wait_for(lambda: json.loads(run(host_cli, "--home", host_home, "--json", "pair-code", "--level", "observe")).get("line"),
                          "host ready")
        run(options.cli, "--home", ctl_home, "pair", ticket, "--name", "self")
        if options.animate or options.stress:
            page = base / "animate.html"
            page.write_text(NOISE if options.stress else ANIMATION, encoding="utf-8")
            size = "1200,700" if options.stress else "320,240"
            browser = subprocess.Popen([options.browser, f"--user-data-dir={base / 'browser'}", "--no-first-run",
                                        f"--app={page.as_uri()}", f"--window-size={size}", "--window-position=40,40"],
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            time.sleep(3)
            if os.name == "nt":
                # Covered, the window would stop painting and the screen would not change: keep it on top.
                title = "ai-remote stream stress" if options.stress else "ai-remote stream test"
                subprocess.run(["powershell", "-NoProfile", "-Command",
                                "Add-Type -Name W -Namespace U -MemberDefinition '[DllImport(\"user32.dll\")] public static extern bool SetWindowPos(System.IntPtr h, System.IntPtr a, int x, int y, int cx, int cy, uint f);';"
                                f"Get-Process | Where-Object {{ $_.MainWindowTitle -eq '{title}' }} | ForEach-Object {{ [U.W]::SetWindowPos($_.MainWindowHandle, [System.IntPtr](-1), 0, 0, 0, 0, 3) }}"],
                               capture_output=True, timeout=30)
        only = [o.strip() for o in options.only.split(",")] if options.only else None
        configs = [c for c in CONFIGS if not only or any(o in c[0] for o in only)]
        for label, fps, edge, kbps, codecs in [] if options.skip_cli else configs:
            args = [options.cli, "--home", ctl_home, "--json", "view", "--host", "self", "--seconds", options.seconds,
                    "--max-fps", fps, "--max-long-edge", edge, "--bitrate-kbps", kbps]
            if options.pointer:
                args += ["--pointer"]
            if codecs:
                args += ["--codecs", codecs]
            try:
                v = json.loads(run(*args, timeout=options.seconds + 60))
            except Exception as e:  # noqa: BLE001 - one failing configuration should not hide the others
                v = {"error": str(e)}
            v["label"] = label
            report["results"].append(v)
            if "error" in v:
                print(f"{label:28} ERROR {v['error'][:200]}")
                continue
            g = v["gap_ms"]
            print(f"{label:28} {v['codec']:5} {v['width']}x{v['height']:<5} {v['fps']:6.1f} fps "
                  f"{v['kbit_per_s'] / 1000:7.1f} Mbit/s  first {v['first_frame_ms'] or 0:6.0f} ms  "
                  f"gap p50 {g['p50'] or 0:5.1f} p95 {g['p95'] or 0:5.1f} max {g['max'] or 0:6.1f} ms  "
                  + (f"latency p50 {l['p50']:5.1f} p95 {l['p95']:5.1f} ms (+/-{l['error']:.1f})"
                     if (l := v.get("latency_ms") or {}).get("p50") is not None else "latency n/a")
                  + (f"  pointer {p['positions']} positions {p['shapes']} shapes {p['bytes']} B"
                     if (p := v.get("pointer") or {}).get("separate") else ""))
        if options.desktop:
            # The real desktop app (debug build: `cd apps/desktop && npx tauri build --debug --no-bundle`)
            # off screen, on the session page, once per quality; it reports its own receiver metrics.
            for quality in options.qualities.split(","):
                env = {**os.environ, "AI_REMOTE_HOME": str(ctl_home), "AI_REMOTE_CLI": str(options.cli),
                       "AI_REMOTE_SMOKE": str(base / f"{quality}.png"), "AI_REMOTE_SMOKE_SECS": str(options.seconds + 4),
                       "AI_REMOTE_SMOKE_QUERY": "view=session&host=self", "AI_REMOTE_SMOKE_QUALITY": quality,
                       "WEBVIEW2_USER_DATA_FOLDER": str(base / f"webview-{quality}"),
                       "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS": "--disable-features=CalculateNativeWinOcclusion"}
                p = subprocess.run([str(options.desktop)], capture_output=True, text=True, encoding="utf-8",
                                   errors="replace", env=env, timeout=options.seconds + 90)
                smoke = next((json.loads(l.split("] ", 1)[1]) for l in p.stderr.splitlines() if l.startswith("[page smoke]")), {})
                errors = [l for l in p.stderr.splitlines() if l.startswith("[page error]")]
                # The page's per-second metrics (debug smoke runs): a latency / bitrate timeline.
                timeline = [json.loads(l.split("Live video: stats ", 1)[1]) for l in p.stderr.splitlines()
                            if "Live video: stats " in l]
                video = [l for l in p.stderr.splitlines() if "Live video:" in l and "Live video: stats " not in l]
                (options.out / f"desktop-{quality}.log").write_text(p.stderr, encoding="utf-8")
                report.setdefault("desktop", []).append({"quality": quality, **smoke, "page_errors": errors,
                                                         "video_log": video, "timeline": timeline})
                print(f"desktop {quality:9} {smoke.get('metrics')} | {smoke.get('metricsDetail')} | canvases {smoke.get('canvases')}"
                      + (f" | {len(errors)} page errors: {errors[0][:160]}" if errors else ""))
                for line in video[:12]:
                    print(f"    {line[:200]}")
                if timeline:
                    print("    t(s) Mbit/s latency(ms) target(Mbit/s)  " + "  ".join(
                        f"{s['t'] / 1000:.0f}:{s['kbps'] / 1000:.1f}/{s['latency'] if s['latency'] is not None else '-'}"
                        f"/{s['target'] / 1000:.1f}" for s in timeline))
    finally:
        if browser:
            # The browser starts helper processes; stop the whole tree.
            if os.name == "nt":
                subprocess.run(["taskkill", "/PID", str(browser.pid), "/T", "/F"], capture_output=True)
            else:
                browser.terminate()
        # `view` and `pair` started a controller agent for the throwaway profile.
        subprocess.run([str(options.cli), "--home", str(ctl_home), "stop-agent"], capture_output=True, timeout=30)
        host.terminate()
        try:
            host.wait(timeout=10)
        except subprocess.TimeoutExpired:
            host.kill()
        log.close()
(options.out / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
print(f"report: {options.out / 'report.json'}; host log: {options.out / 'host.log'}")
