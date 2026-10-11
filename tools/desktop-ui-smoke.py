#!/usr/bin/env python3
"""Exercise the local browser preview through the durable, headless Camofox session.

Requires the desktop Vite server and Camofox CLI. Uses only the labelled demo
backend; this is UI acceptance, not native transport/video/device acceptance.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:1420")
    parser.add_argument("--session", default="general")
    parser.add_argument("--port", type=int, default=9378)
    parser.add_argument("--output", type=Path, default=Path("target/ui-modernization/browser"))
    args = parser.parse_args()
    assert re.match(r"^http://(127\.0\.0\.1|localhost):\d+$", args.url), "Local preview only"
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CAMOFOX_INTERACTIVE="off")
    checks = []
    current_tab = None

    def cli(*command):
        return subprocess.check_output(["camofox", "--session", args.session, "--port", str(args.port), *command], env=env, text=True, timeout=45)

    def rpc(command, **data):
        req = urllib.request.Request(f"http://localhost:{args.port}/tabs/{current_tab}/{command}", data=json.dumps({"userId": f"camofox-{args.session}", **data}).encode(), headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=45) as response:
            return json.load(response)

    def evaluate(expression):
        return json.loads(cli("evaluate", expression))["result"]

    def check(name, condition):
        assert condition, name
        checks.append(name)
        print(f"PASS {name}", flush=True)

    def wait_for(predicate, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.2)
        raise AssertionError("UI condition did not become true")

    def snap(name=None):
        value = cli("snapshot")
        if name:
            (out / f"{name}.txt").write_text(value)
        return value

    def click(label, kind="button", occurrence=0):
        refs = re.findall(r'^\s*- ' + kind + r' "' + re.escape(label) + r'" \[(e\d+)\]', snap(), re.M)
        assert len(refs) > occurrence, f"Missing {kind}: {label}"
        cli("click", "@" + refs[occurrence])

    def navigate(query):
        cli("navigate", f"{args.url}/?{query}")
        wait_for(lambda: evaluate('document.querySelector(".demo-badge") !== null && !document.querySelector("vite-error-overlay") && !document.body.innerText.includes("Starting up") && !document.body.innerText.includes("正在启动")'))
        check("preview labelled: " + query, "Preview" in snap() or "界面预览" in snap())

    def screenshot(name):
        cli("screenshot", str(out / f"{name}.png"))
        snap(name)

    try:
        opened = cli("open", f"{args.url}/?shotlang=zh&view=devices")
        current_tab = re.search(r"Tab: (\S+)", opened).group(1)
        rpc("viewport", width=1280, height=800)
        wait_for(lambda: evaluate('document.querySelectorAll(".device-card").length === 4'))
        check("demo guard", "界面预览" in snap())
        original = evaluate('Object.fromEntries(["ai-remote.theme","ai-remote.reduce-transparency","ai-remote.stream-quality"].map(k=>[k,localStorage.getItem(k)]))')
        evaluate('(()=>{localStorage.setItem("ai-remote.theme","light");localStorage.setItem("ai-remote.reduce-transparency","0");return true})()')
        navigate("shotlang=zh&view=devices")
        wait_for(lambda: evaluate('document.querySelectorAll(".device-card").length === 4 && document.querySelector(".devices-summary").innerText.includes("直连 1")'))
        check("online and route counts", evaluate('document.querySelector(".devices-summary").innerText.includes("3 / 4") && document.querySelector(".devices-summary").innerText.includes("直连 1 · 中继 1 · 未知 1")'))
        offline_selector = json.dumps('button[aria-label="实时查看 render-box"]')
        check("offline watch disabled", evaluate(f"document.querySelector({offline_selector}).disabled"))
        screenshot("devices-light-1280")
        evaluate('(()=>{document.querySelectorAll(".side > .nv")[2].focus();return true})()')
        rpc("press", key="Enter")
        wait_for(lambda: evaluate('document.querySelector(".act") !== null'))
        check("native Enter navigation", True)
        click("设备 4")

        ref = re.search(r'searchbox "搜索电脑" \[(e\d+)\]', snap()).group(1)
        cli("type", "@" + ref, "office")
        wait_for(lambda: evaluate('document.querySelectorAll(".device-card").length === 1'))
        check("shared search", evaluate('[...document.querySelectorAll("input[type=search]")].every(x=>x.value === "office")'))
        click("清除搜索")
        wait_for(lambda: evaluate('document.querySelectorAll(".device-card").length === 4'))
        ref = re.search(r'searchbox "搜索电脑" \[(e\d+)\]', snap()).group(1)
        cli("type", "@" + ref, "no-such-host-acceptance")
        wait_for(lambda: evaluate('document.querySelectorAll(".device-card").length === 0'))
        check("search empty state", evaluate('document.querySelector(".devices-empty") !== null'))
        click("清除搜索")

        click("查看 studio-mac 的详情")
        wait_for(lambda: evaluate('document.querySelector(".cp-top .nm")?.textContent === "studio-mac"'))
        click("访问权限", kind="tab")
        check("device access tab", "studio-mac" in snap())
        click("工具", kind="tab")
        wait_for(lambda: evaluate('document.body.innerText.includes("ui_tree")'))
        check("device tools tab", "ui_tree" in snap())
        click("概览", kind="tab")
        screenshot("computer-light")
        click("设备 4")
        click("实时查看 studio-mac")
        wait_for(lambda: evaluate('document.querySelector(".capsule") !== null'))
        check("stream quality controls", evaluate('document.querySelector("#stream-quality")?.options.length === 4'))
        evaluate('(()=>{const s=document.querySelector("#stream-quality");s.value="smooth";s.dispatchEvent(new Event("change",{bubbles:true}));return true})()')
        wait_for(lambda: evaluate('document.querySelector(".stream-request").innerText.includes("60")'))
        check("quality selection requested limits", "无实时测量" in snap())
        click("接管")
        wait_for(lambda: evaluate('document.querySelector(".capsule").innerText.includes("你已接管")'))
        check("takeover changes driver", "交还" in snap())
        # Observe the actual DOM mouse/keyboard path, while staying guarded in preview.
        # Camofox evaluates in an isolated world; install the guarded observer in
        # the page's module world so it observes the real React event handlers.
        evaluate('(()=>{const url=performance.getEntriesByType("resource").filter(x=>new URL(x.name).origin===location.origin && new URL(x.name).pathname==="/src/lib/api.ts").at(-1)?.name ?? "/src/lib/api.ts";const s=document.createElement("script");s.type="module";s.textContent=`import {api} from ${JSON.stringify(url)};if(api.kind!=="demo")throw new Error("preview guard");window.__manualCalls=[];const send=api.callTool;api.callTool=async(...args)=>{window.__manualCalls.push(args);return send(...args)};window.__manualSpyReady=true;`;document.head.append(s);return true})()')
        wait_for(lambda: evaluate('(window.wrappedJSObject??window).__manualSpyReady === true'))
        input_selector = json.dumps('[aria-label="Remote screen"]')
        wait_for(lambda: evaluate(f'document.querySelector({input_selector}) !== null'))
        cli("click", "css:[aria-label='Remote screen']")
        wait_for(lambda: any(x[1] == "mouse" and x[2].get("action") == "click" and x[3] is True for x in (evaluate('(window.wrappedJSObject??window).__manualCalls') or [])))
        check("real DOM zero-detail manual click", True)
        rpc("press", key="a")
        wait_for(lambda: any(x[1] == "keyboard" and x[2].get("text") == "a" for x in (evaluate('(window.wrappedJSObject??window).__manualCalls') or [])))
        check("real DOM manual keyboard", True)
        screenshot("session-takeover")
        # The handback label includes the visible keyboard shortcut.
        handback = re.search(r'button "(交还[^"\n]*)" \[(e\d+)\]', snap())
        assert handback
        cli("click", "@" + handback.group(2))
        wait_for(lambda: evaluate('!document.querySelector(".capsule").innerText.includes("你已接管")'))
        check("handback changes driver", True)
        click("设备")
        check("session returns to devices", 'heading "设备" [level=1]' in snap())

        click("设置")
        # Camofox has no CLI select command; native select's change event is supported.
        evaluate('(()=>{const s=document.querySelector("select[aria-label=外观]");s.value="dark";s.dispatchEvent(new Event("change",{bubbles:true}));return true})()')
        wait_for(lambda: evaluate('document.documentElement.dataset.theme === "dark"'))
        check("theme selection applied", True)
        evaluate('(()=>{const s=document.querySelector("select[aria-label=玻璃效果]");s.value="reduced";s.dispatchEvent(new Event("change",{bubbles:true}));return true})()')
        check("opaque surfaces", evaluate('document.documentElement.dataset.transparency === "reduced" && getComputedStyle(document.querySelector(".snav")).backdropFilter === "none"'))
        cli("refresh")
        wait_for(lambda: evaluate('document.querySelectorAll(".device-card").length === 4'))
        check("appearance persists after reload", evaluate('document.documentElement.dataset.theme === "dark" && document.documentElement.dataset.transparency === "reduced"'))
        screenshot("devices-dark-opaque")
        click("设置")
        evaluate('(()=>{const s=document.querySelector("select[aria-label=玻璃效果]");s.value="full";s.dispatchEvent(new Event("change",{bubbles:true}));return true})()')
        navigate("shotlang=zh&view=devices")
        check("dark glass restored", evaluate('document.documentElement.dataset.theme === "dark" && getComputedStyle(document.querySelector(".side")).backdropFilter !== "none"'))

        for width, height in [(960, 600), (1280, 800), (1728, 1031)]:
            rpc("viewport", width=width, height=height)
            check(f"no horizontal overflow at {width}x{height}", evaluate('(()=>{const e=document.querySelector(".devices-page");return e.scrollWidth <= e.clientWidth + 1})()'))
            screenshot(f"devices-dark-{width}")

        navigate("shotlang=en&shottheme=light&view=devices")
        check("English devices", 'heading "Devices" [level=1]' in snap())
        for query, selector, name in [
            ("shotlang=zh&shottheme=light&view=pair", ".ppage", "pair"),
            ("shotlang=zh&shottheme=light&view=approvals", ".ipage", "approvals"),
            ("shotlang=zh&shottheme=dark&view=activity", ".act", "activity"),
            ("shotlang=zh&shottheme=light&view=settings", ".spage", "settings"),
            ("shotlang=zh&shottheme=light&empty&view=welcome", ".spread", "welcome"),
            ("shotlang=zh&shottheme=light&empty&view=devices", ".devices-empty", "empty"),
        ]:
            navigate(query)
            wait_for(lambda: evaluate(f'document.querySelector({json.dumps(selector)}) !== null'))
            screenshot(name)
            check("page renders: " + name, True)
        (out / "report.json").write_text(json.dumps({"status": "passed", "scope": "Camofox demo-backend UI only; no native video or input proof", "checks": checks}, ensure_ascii=False, indent=2))
    except Exception as error:
        try:
            screenshot("failure")
        except Exception:
            pass
        (out / "report.json").write_text(json.dumps({"status": "failed", "checks": checks, "error": str(error)}, ensure_ascii=False, indent=2))
        raise
    finally:
        if current_tab:
            if "original" in locals():
                evaluate('(()=>{const v=' + json.dumps(original) + ';for(const [k,x] of Object.entries(v)){if(x===null)localStorage.removeItem(k);else localStorage.setItem(k,x)}return true})()')
            cli("close")


if __name__ == "__main__":
    main()
