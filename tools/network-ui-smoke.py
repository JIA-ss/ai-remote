#!/usr/bin/env python3
"""Settings > Network in the desktop app's browser preview (demo backend), driven through the local
Camofox server's REST API: the defaults, switching to Local network only, local discovery, changing a
paired computer's address (a bad one first), the English page, and a computer locked by quiet = true.

  python tools/network-ui-smoke.py [--url http://localhost:1437] [--camofox http://127.0.0.1:9377]

Without --url it starts the Vite preview itself on a free port and stops it afterwards. Screenshots
and a report go to --out. UI acceptance only: the demo backend stands in for the controller agent.
"""
import argparse
import json
import os
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--url", help="an already running preview (default: start one)")
parser.add_argument("--camofox", default="http://127.0.0.1:9377")
parser.add_argument("--out", type=Path, default=ROOT / "target/network-ui")
args = parser.parse_args()
# A fresh browser profile each run: Camofox keeps per-user storage between runs.
USER = f"network-ui-smoke-{os.getpid()}"
checks = []


def call(method, path, body=None, raw=False):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(args.camofox + path, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            payload = response.read()
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"camofox {method} {path}: {error.code} {error.read().decode(errors='replace')[:300]}") from None
    return payload if raw else json.loads(payload or b"{}")


def check(name, ok, detail=""):
    print(f"{'PASS' if ok else 'FAIL'} {name}" + (f"  ({detail})" if detail and not ok else ""), flush=True)
    checks.append({"name": name, "ok": bool(ok)})
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_http(url, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return
        except OSError:
            time.sleep(0.3)
    raise RuntimeError(f"{url} did not answer")


def main():
    args.out.mkdir(parents=True, exist_ok=True)
    vite = None
    base = args.url
    if not base:
        port = free_port()
        base = f"http://localhost:{port}"
        npx = "npx.cmd" if sys.platform == "win32" else "npx"
        vite = subprocess.Popen([npx, "vite", "--port", str(port), "--strictPort"], cwd=ROOT / "apps/desktop",
                                stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    tab = None
    try:
        wait_http(base)
        tab = call("POST", "/tabs", {"userId": USER, "sessionKey": "network", "url": f"{base}/?shotlang=zh&view=settings&sec=network"})["tabId"]

        def js(expression):
            r = call("POST", f"/tabs/{tab}/evaluate", {"userId": USER, "expression": expression})
            return r.get("result")

        def shot(name):
            (args.out / f"{name}.png").write_bytes(call("GET", f"/tabs/{tab}/screenshot?userId={USER}&fullPage=true", raw=True))

        def until(expression, seconds=10):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if js(expression):
                    return True
                time.sleep(0.2)
            return False

        def page(query):
            call("POST", f"/tabs/{tab}/navigate", {"userId": USER, "url": f"{base}/?{query}"})
            return until('document.querySelectorAll("[role=radio]").length === 2')

        state = """(() => ({
          radios: [...document.querySelectorAll('[role=radio]')].map(r => r.getAttribute('aria-checked')),
          switches: [...document.querySelectorAll('button[role=switch]')].map(b => [b.getAttribute('aria-label'), b.getAttribute('aria-checked'), b.disabled]),
          groups: [...document.querySelectorAll('.grp-h')].map(n => n.textContent),
          subs: [...document.querySelectorAll('.grp-sub')].map(n => n.textContent),
          notes: [...document.querySelectorAll('.gnote')].map(n => n.textContent),
          text: document.querySelector('.spage')?.innerText ?? '',
        }))()"""

        check("network page renders", page("shotlang=zh&shottheme=light&view=settings&sec=network"))
        s = js(state)
        check("automatic is chosen by default", s["radios"] == ["true", "false"], s["radios"])
        check("both extras are off and can be turned on", [x[1:] for x in s["switches"]] == [["false", False], ["false", False]], s["switches"])
        check("no address groups in automatic mode", s["groups"] == ["连接方式", "可选功能"], s["groups"])
        shot("1-automatic")

        js("[...document.querySelectorAll('[role=radio]')].find(r => r.textContent.includes('仅限局域网')).click()")
        check("local network only is chosen", until("document.querySelectorAll('[role=radio]')[1].getAttribute('aria-checked') === 'true'"))
        s = js(state)
        check("router port mapping is hidden on the local network only", [x[0] for x in s["switches"]] == ["在局域网里自动发现设备"], s["switches"])
        check("address groups appear", s["groups"] == ["连接方式", "可选功能", "这台电脑的地址", "要连接的电脑"], s["groups"])
        check("only an address another device can use is shown", "192.168.1.20:47800" in s["text"] and "fe80" not in s["text"] and "127.0.0.1" not in s["text"], s["text"][:300])
        check("saving says when the host applies it", any("下次启动时生效" in n for n in s["notes"]), s["notes"])

        js("document.querySelector('button[role=switch]').click()")
        check("local discovery turns on", until("document.querySelector('button[role=switch]').getAttribute('aria-checked') === 'true'"))
        check("computers are said to be found again", until("[...document.querySelectorAll('.grp-sub')].some(n => n.textContent.includes('自动重新找到'))"))
        shot("2-local-network-only")

        edit = """(async (value) => {
          const row = [...document.querySelectorAll('.gr')].find(r => r.textContent.startsWith('lab-pc'));
          if (!row.querySelector('input')) row.querySelector('button').click();
          await new Promise(r => setTimeout(r, 150));
          const input = row.querySelector('input');
          Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, value);
          input.dispatchEvent(new Event('input', { bubbles: true }));
          await new Promise(r => setTimeout(r, 50));
          [...row.querySelectorAll('button')].find(b => b.textContent === '保存').click();
          await new Promise(r => setTimeout(r, 600));
          return row.innerText;
        })"""
        bad = js(edit + "('office-pc')")
        check("a name instead of an IP address is refused", "请填 IP 地址" in bad, bad)
        good = js(edit + "('10.0.0.5')")
        check("an address without a port gets 47800", "10.0.0.5:47800" in good and "修改" in good, good)
        shot("3-address-changed")

        check("English page renders", page("shotlang=en&shottheme=light&view=settings&sec=network"))
        text = js(state)["text"]
        check("English labels", all(w in text for w in ["How devices connect", "Local network only", "Find devices on your local network"]), text[:200])
        shot("4-english")

        check("locked page renders", page("shotlang=zh&shottheme=dark&view=settings&sec=network&locked"))
        s = js(state)
        check("quiet = true locks both extras off", all(x[2] for x in s["switches"]) and any("quiet = true" in t for t in s["subs"]), s)
        shot("5-locked-dark")
        (args.out / "report.json").write_text(json.dumps({"status": "passed", "checks": checks}, ensure_ascii=False, indent=2))
        print(f"{len(checks)} checks passed; screenshots in {args.out}")
    except Exception as error:
        (args.out / "report.json").write_text(json.dumps({"status": "failed", "checks": checks, "error": str(error)}, ensure_ascii=False, indent=2))
        raise
    finally:
        try:
            call("DELETE", f"/sessions/{USER}")
        except OSError:
            pass
        if vite:
            vite.terminate()


if __name__ == "__main__":
    main()
