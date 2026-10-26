#!/usr/bin/env python3
"""Glass acceptance audit of the desktop app's browser preview (demo backend), through the local
Camofox server's REST API.

  python3 tools/desktop-ui-smoke.py [--url http://127.0.0.1:1535] [--camofox http://127.0.0.1:9377]
                                    [--out target/glass-audit] [--only settings] [--langs zh] [--themes dark]

Without --url it starts the Vite preview itself on a free port and stops it afterwards. Every screen
(devices, approvals, activity, pair, settings x 6 sections, computer x 4 tabs, session, session with
the chat panel, agents, the agents workspace without computers, welcome, empty) is opened by its deep
link in zh / en and light / dark, screenshotted, and checked in the page:

  - exactly one page root is mounted, and it is the one the deep link names (plus route content);
  - one h1 (the live view is two panes: one h1 outside the embedded workspace, at most one in it);
  - at most one input[type=search] outside the agents workspace;
  - the shell sidebar on every shell route; on the live view and the workspace, their own way back
    to the computers (the workspace sidebar toggle / mode switch) instead of a second sidebar;
  - no backdrop-filter on or inside the live stage (.nt-stage and its chrome) or on the window
    surfaces (html, body, #root, .win, .page, .side, the page roots);
  - every element with a backdrop-filter (and ::before / ::after) is a leaf of glass: no ancestor
    with opacity < 1, filter, mask or will-change, and no ancestor that is itself glass; at most one
    blurred element animating;
  - no dialog (.cdlg / .smoke) inside .win, no horizontal overflow, no Vite error overlay;
  - with html[data-material=none] (the app's own setReducedTransparency, after every screen) there
    is no element with a backdrop-filter at all.

Then the interactions: Settings > General > Glass effects set to reduced through its select (and
back), Settings -> sidebar -> Devices with < and > in between, a session -> workspace sidebar ->
Computers, a session's < to its computer, the computer-less workspace -> Computers, and a sheet
opened over the computer page (it sits outside .win, its ancestors are clean).

A static lint runs first over the glass layer and the app's own sheets (styles/glass, workspace,
claude, codex, app): every non-none backdrop-filter has a -webkit-backdrop-filter with a literal value
before it in the same rule, and its selectors start with
:root:is([data-material="native"], [data-material="preview"]).

Engine notes. Camofox is Firefox (Camoufox) headless: it does not paint backdrop-filter, but
getComputedStyle reports it (checked at start, the audit stops if it does not), so the blur checks
read computed styles and the screenshots only prove layout and tints. Firefox has no
-webkit-backdrop-filter, which is why the prefixed declaration is linted from the source instead.
Camofox's REST viewport call never answers on its viewport-less contexts, yet the new size does take
effect: the audit sends it with a short timeout and then verifies 1280 x 800 with matchMedia.
Camoufox also reports a spoofed window.innerWidth (960) to the page, so the sidebar is pinned
expanded (the app reads innerWidth and would default to compact below 1100 px). UI acceptance only:
the demo backend stands in for the controller agent; no native material, video, input or transport
is exercised.

Screenshots and report.json go to --out. Exits non-zero when any check fails.
"""
import argparse
import json
import os
import re
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DESKTOP = ROOT / "apps/desktop"
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--url", help="an already running preview, e.g. http://127.0.0.1:1535 (default: start one)")
parser.add_argument("--camofox", default="http://127.0.0.1:9377")
parser.add_argument("--out", "--output", dest="out", type=Path, default=ROOT / "target/glass-audit")
parser.add_argument("--only", help="comma-separated substrings: audit only the screens whose name contains one")
parser.add_argument("--langs", default="zh,en")
parser.add_argument("--themes", default="light,dark")
parser.add_argument("--no-interactions", action="store_true", help="skip the navigation / reduced / sheet scenarios")
parser.add_argument("--verbose", action="store_true", help="print every passing check too")
args = parser.parse_args()

# A fresh browser profile each run (Camofox keeps per-user storage between runs); deleted at the end.
USER = f"glass-audit-{os.getpid()}"
W, H = 1280, 800
# Loopback only, never through a system proxy (a proxy answered 400 for 127.0.0.1 on this machine).
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))
checks = []
state = {"tab": None, "base": None, "n": 0}


# ---------------------------------------------------------------- strings (the app's own copy)

def load_strings(lang):
    text = (DESKTOP / f"src/lib/strings/{lang}.ts").read_text(encoding="utf-8")
    table = {}
    for m in re.finditer(r'^\s+(\w+):\s*"((?:[^"\\]|\\.)*)",?\s*$', text, re.M):
        table[m.group(1)] = json.loads('"' + m.group(2) + '"')
    return table


STRINGS = {lang: load_strings(lang) for lang in ("zh", "en")}


def T(lang, key, **fill):
    value = STRINGS[lang][key]
    for k, v in fill.items():
        value = value.replace("{" + k + "}", v)
    return value


# ---------------------------------------------------------------- checks and reporting

def check(name, ok, detail=None):
    entry = {"name": name, "ok": bool(ok)}
    if not ok and detail is not None:
        entry["detail"] = detail
    checks.append(entry)
    if not ok:
        print(f"FAIL {name}" + (f"  ({json.dumps(detail, ensure_ascii=False)[:600]})" if detail is not None else ""), flush=True)
    elif args.verbose:
        print(f"PASS {name}", flush=True)
    return bool(ok)


# ---------------------------------------------------------------- static lint of the style sheets

LINTED = sorted([*(DESKTOP / "src/styles/glass").glob("*.css"), *[DESKTOP / "src/styles" / n for n in ("workspace.css", "claude.css", "codex.css", "app.css")]])
GATE = re.compile(r'^:root:is\(\[data-material="native"\],\s*\[data-material="preview"\]\)')


def css_rules(text):
    """(selector prelude, declarations) of every innermost block; comments removed."""
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    out, stack, start = [], [], 0
    for i, ch in enumerate(text):
        if ch == "{":
            stack.append(text[start:i].strip().split(";")[-1].strip())
            start = i + 1
        elif ch == "}":
            body = text[start:i]
            prelude = stack.pop() if stack else ""
            if "{" not in body and not prelude.startswith("@"):
                out.append((prelude, body))
            start = i + 1
    return out


def split_selectors(prelude):
    parts, depth, cur = [], 0, ""
    for ch in prelude:
        if ch in "([":
            depth += 1
        elif ch in ")]":
            depth -= 1
        if ch == "," and depth == 0:
            parts.append(cur.strip())
            cur = ""
        else:
            cur += ch
    parts.append(cur.strip())
    return [p for p in parts if p]


def lint_styles():
    for path in LINTED:
        if not path.exists():
            continue
        rel = path.relative_to(ROOT).as_posix()
        for prelude, body in css_rules(path.read_text(encoding="utf-8")):
            decls = [(m.group(1).strip().lower(), m.group(2).strip()) for m in re.finditer(r"([-\w]+)\s*:\s*([^;]+)", body)]
            for i, (prop, value) in enumerate(decls):
                if prop != "backdrop-filter" or value.split("!")[0].strip() in ("none", "inherit", "initial", "unset"):
                    continue
                where = f"{rel}: {prelude[:120]}"
                prefixed = [v for p, v in decls[:i] if p == "-webkit-backdrop-filter"]
                check(f"lint prefixed literal -webkit-backdrop-filter first: {where}", prefixed and "var(" not in prefixed[-1],
                      {"prefixed": prefixed, "unprefixed": value})
                ungated = [s for s in split_selectors(prelude) if not GATE.match(s)]
                check(f"lint gated on data-material: {where}", not ungated, {"ungated": ungated})


# ---------------------------------------------------------------- Camofox REST

def call(method, path, body=None, raw=False, timeout=60):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(args.camofox + path, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with OPENER.open(req, timeout=timeout) as response:
            payload = response.read()
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"camofox {method} {path}: {error.code} {error.read().decode(errors='replace')[:300]}") from None
    result = payload if raw else json.loads(payload or b"{}")
    if not raw and isinstance(result, dict) and result.get("ok") is False:
        raise RuntimeError(f"camofox {method} {path}: {result}")
    return result


def js(expression):
    return call("POST", f"/tabs/{state['tab']}/evaluate", {"userId": USER, "expression": expression}).get("result")


def fjs(body):
    """Run `body` (statements ending in a return) with the page's document `d` and window `w`."""
    return js(f"(() => {{ const d = document, w = window; {body} }})()")


def until(body, seconds=10.0, step=0.15):
    deadline = time.monotonic() + seconds
    while True:
        try:
            result = fjs(body)
        except RuntimeError:
            result = None
        if result:
            return result
        if time.monotonic() >= deadline:
            return False
        time.sleep(step)


def shot(name):
    path = args.out / "screens" / f"{name}.png"
    path.write_bytes(call("GET", f"/tabs/{state['tab']}/screenshot?userId={USER}&fullPage=true", raw=True))
    return path


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_http(url, seconds=40):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            OPENER.open(url, timeout=2).read()
            return
        except OSError:
            time.sleep(0.3)
    raise RuntimeError(f"{url} did not answer")


def app_call(source):
    """Run module code in the page world (Camofox evaluate is an isolated world). Imports go
    through the URL App.tsx actually used, so an HMR ?t= suffix never creates a second module."""
    state["n"] += 1
    key = f"glassAudit{state['n']}"
    imports = r"""const appSource=await(await fetch('/src/App.tsx',{cache:'no-store'})).text();
      const appImport=name=>{const path='/src/lib/'+name+'.ts';
        const exact=[...appSource.matchAll(/\bfrom\s+["']([^"']+)["']/g)].map(m=>m[1]).find(s=>s.split('?')[0]===path);
        return import(exact??path);};"""
    code = (f"try {{ {imports} const result = await (async () => {{ {source} }})(); "
            f"document.body.dataset.{key} = JSON.stringify({{ok:true,result}}); }} "
            f"catch (e) {{ document.body.dataset.{key} = JSON.stringify({{ok:false,error:String(e)}}); }}")
    fjs(f"const s=d.createElement('script'); s.type='module'; s.textContent={json.dumps(code)}; d.body.append(s); return true;")
    raw = until(f"return d.body.dataset.{key} || null;", 10)
    if not raw:
        raise RuntimeError("page-world module did not answer")
    value = json.loads(raw)
    if not value["ok"]:
        raise RuntimeError(value["error"])
    return value.get("result")


def set_reduced(on):
    return app_call(f"const {{setReducedTransparency}} = await appImport('platform'); setReducedTransparency({str(bool(on)).lower()}); return document.documentElement.dataset.material;")


# ---------------------------------------------------------------- the page model

# Every page component's root; exactly one may be mounted (the workspace root counts only when it
# owns the whole window, inside the live view it is that view's second pane).
ROOTS = [".devices-page", ".ipage", ".act", ".ppage", ".spage", ".cpage", ".spread", ".nt-session", ".win > .ws"]
STAGE = ".nt-stage, .nt-bar, .nt-hud, .nt-handle, .nt-wait, .nt-stagecard, .stream-ended, .rd-holder, .nt-picture"
SURFACES = "html, body, #root, .win, .body-row, .page, .gl-main, .nt-content, .side, .ws, .ws-main, .nt-live-workspace, .nt-session-body, " + ", ".join(ROOTS)

AUDIT = """
if (!d.body) return null;
const cs = (e, p) => w.getComputedStyle(e, p || null);
const desc = e => { let s = e.tagName.toLowerCase(); if (e.id) s += '#' + e.id;
  const c = (e.getAttribute('class') || '').trim(); if (c) s += '.' + c.split(/\\s+/).join('.'); return s.slice(0, 100); };
const blurOf = (e, p) => { const s = cs(e, p); const v = s.backdropFilter || s.webkitBackdropFilter || 'none'; return v !== 'none' ? v : null; };
const ROOTS = %(roots)s;
const blurred = [];
for (const e of d.querySelectorAll('*'))
  for (const p of [null, '::before', '::after']) { const v = blurOf(e, p); if (v) blurred.push({e, p, v}); }
const shown = b => b.e.getClientRects().length > 0;
const name = b => desc(b.e) + (b.p || '');
const problems = [];
for (const b of blurred.filter(shown)) {
  const chain = []; if (b.p) chain.push(b.e); for (let a = b.e.parentElement; a; a = a.parentElement) chain.push(a);
  const bad = [];
  for (const a of chain) { const s = cs(a);
    if (Number(s.opacity) < 1) bad.push(desc(a) + ' opacity ' + s.opacity);
    if (s.filter && s.filter !== 'none') bad.push(desc(a) + ' filter ' + s.filter);
    if (s.maskImage && s.maskImage !== 'none') bad.push(desc(a) + ' mask ' + s.maskImage.slice(0, 60));
    if (s.willChange && s.willChange !== 'auto') bad.push(desc(a) + ' will-change ' + s.willChange);
    if (a !== b.e && blurOf(a)) bad.push(desc(a) + ' is glass too (' + blurOf(a) + ')');
  }
  if (bad.length) problems.push({el: name(b), ancestors: bad});
}
const running = d.getAnimations().filter(a => a.playState === 'running');
const pane = d.querySelector('.nt-workspace-pane');
const side = d.querySelector('nav.side');
const content = d.querySelector('.nt-content');
const srH1 = d.querySelector('h1.gl-sr');
return {
  material: d.documentElement.dataset.material, theme: d.documentElement.dataset.theme,
  search: location.search, route: d.querySelector('.win')?.dataset.route ?? null,
  roots: Object.fromEntries(ROOTS.map(s => [s, d.querySelectorAll(s).length])),
  contentKids: content ? content.children.length : null,
  h1Shell: [...d.querySelectorAll('h1')].filter(h => !h.closest('.nt-workspace-pane')).map(h => h.textContent.trim()),
  h1Pane: pane ? [...pane.querySelectorAll('h1')].map(h => h.textContent.trim()) : [],
  srH1: srH1 ? srH1.textContent.trim() : null,
  searches: [...d.querySelectorAll('input[type=search]')].filter(e => !e.closest('.ws')).length,
  side: side ? {shown: side.getClientRects().length > 0 && side.getBoundingClientRect().width >= 48, count: d.querySelectorAll('nav.side').length,
    current: [...side.querySelectorAll('[aria-current=page]')].map(e => (e.querySelector('.nm,.lbl') || e).textContent.trim())} : null,
  selectedTab: [...d.querySelectorAll('.gl-toolbar [role=tab][aria-selected=true]')].map(e => e.textContent.trim()),
  wsToggle: !!d.querySelector('.nt-workspace-pane .ws-corner .ws-ib:not(:disabled)'),
  wsModes: !!d.querySelector('.ws-side .ws-modes [data-mode=computers]'),
  blurShown: blurred.filter(shown).map(name), blurAll: blurred.map(name),
  stageBlur: blurred.filter(b => b.e.closest(%(stage)s)).map(b => name(b) + ' ' + b.v),
  surfaceBlur: blurred.filter(b => !b.p && b.e.matches(%(surfaces)s)).map(b => name(b) + ' ' + b.v),
  problems,
  glassAnimating: running.filter(a => { const t = a.effect && a.effect.target; return t && blurOf(t); }).map(a => desc(a.effect.target)),
  settled: running.every(a => { const t = a.effect && a.effect.getComputedTiming(); return t && t.iterations === Infinity; }),
  dialogsInWin: [...d.querySelectorAll('.win .cdlg, .win .smoke')].map(desc),
  overflow: {doc: d.documentElement.scrollWidth, view: d.documentElement.clientWidth,
    content: content ? [content.scrollWidth, content.clientWidth] : null},
  errorOverlay: !!d.querySelector('vite-error-overlay'),
  demo: !!d.querySelector('.demo-badge'),
};""" % {"roots": json.dumps(ROOTS), "stage": json.dumps(STAGE), "surfaces": json.dumps(SURFACES)}


def audit():
    return fjs(AUDIT)


def screens(lang):
    """name, query, kind, root, expectations. Kinds: shell (inside the persistent shell), live (the
    live screen beside the embedded workspace), workspace (the workspace owning the window)."""
    out = [
        ("devices", "view=devices", "shell", ".devices-page", {"h1": T(lang, "devices"), "current": T(lang, "devices"),
         "js": "return d.querySelectorAll('.device-card').length === 4;"}),
        ("approvals", "view=approvals", "shell", ".ipage", {"h1": T(lang, "approvals"), "current": T(lang, "approvals"),
         "js": "return !!d.querySelector('.ipage .decide');"}),
        ("activity", "view=activity", "shell", ".act", {"h1": T(lang, "activity"), "current": T(lang, "activity")}),
        ("pair", "view=pair", "shell", ".ppage", {"h1": T(lang, "pair"), "current": T(lang, "pair")}),
    ]
    for sec in ("general", "agents", "device", "network", "approvals", "about"):
        label = T(lang, "set_" + sec)
        expect = {"sr": label, "tab": label, "current": T(lang, "settings")}
        if sec == "network":
            expect["js"] = "return d.querySelectorAll('[role=radio]').length === 2;"
        out.append((f"settings-{sec}", f"view=settings&sec={sec}", "shell", ".spage", expect))
    for tab in ("overview", "access", "tools", "agents"):
        expect = {"h1": "office-pc", "tab": T(lang, "tab_" + tab), "current": "office-pc"}
        if tab == "tools":
            expect["js"] = "return d.querySelector('.cpage').innerText.includes('ui_tree');"
        if tab == "agents":
            expect["js"] = "return !!d.querySelector('.cpage #ag-prompt');"
        out.append((f"computer-{tab}", f"view=computer&host=office-pc&tab={tab}", "shell", ".cpage", expect))
    out += [
        ("session", "view=session&host=lab-pc", "live", ".nt-session", {"h1": "lab-pc",
         "js": "return !!d.querySelector('.nt-session .nt-stage') && !!d.querySelector('.nt-workspace-pane .ws[data-embedded]');"}),
        ("session-chat", "view=session&host=lab-pc&panel=chat", "live", ".nt-session", {"h1": "lab-pc",
         "js": "return !!d.querySelector('.nt-workspace-pane textarea');"}),
        ("agents", "view=agents", "live", ".nt-session", {
         "js": "return d.querySelector('.win').dataset.route === 'agents' && !!d.querySelector('.nt-workspace-pane .ws[data-embedded]');"}),
        ("agents-empty", "empty&view=agents", "workspace", ".win > .ws", {"js": "return !!d.querySelector('.ws .ws-main');"}),
        ("welcome", "empty&view=welcome", "shell", ".spread", {"js": "return !!d.querySelector('.spread .doors');"}),
        ("empty", "empty&view=empty", "shell", ".devices-page", {"h1": T(lang, "devices"),
         "js": "return !!d.querySelector('.devices-empty') && !d.querySelector('.device-card');"}),
    ]
    return out


def load(query, root, seconds=15):
    """Open a deep link and wait until its page root is there and nothing animates."""
    call("POST", f"/tabs/{state['tab']}/navigate", {"userId": USER, "url": f"{state['base']}/?{query}"})
    ready = until(f"""return w.location.search === '?' + {json.dumps(query)} && d.readyState === 'complete'
      && !!d.querySelector({json.dumps(root)}) && !d.querySelector('.gl-starting')
      && !(d.body.innerText.includes('正在启动') || d.body.innerText.includes('Starting up'));""", seconds)
    if not ready:
        return False
    settle()
    return True


def settle(seconds=3.0):
    until("""return (d.fonts ? d.fonts.status === 'loaded' : true) && d.getAnimations().every(a => a.playState !== 'running'
      || (a.effect && a.effect.getComputedTiming().iterations === Infinity));""", seconds, 0.1)
    time.sleep(0.15)


def same_text(a, b):
    return (a or "").replace(" ", " ").strip() == (b or "").strip()


def audit_screen(name, query, kind, root, expect, lang, theme):
    tag = f"{name} {lang}/{theme}"
    full = f"shotlang={lang}&shottheme={theme}&{query}"
    if not check(f"{tag}: page loads", load(full, root), {"query": full, "root": root}):
        try:
            shot(f"{name}-{lang}-{theme}-failed")
        except Exception:
            pass
        return
    a = audit()
    shot(f"{name}-{lang}-{theme}")
    check(f"{tag}: no Vite error overlay", not a["errorOverlay"])
    check(f"{tag}: glass material is on (preview)", a["material"] == "preview" and a["theme"] == theme, {"material": a["material"], "theme": a["theme"]})
    mounted = {k: v for k, v in a["roots"].items() if v}
    check(f"{tag}: exactly one page root, {root}", mounted == {root: 1}, mounted)
    if kind == "shell":
        check(f"{tag}: the content column holds one page", a["contentKids"] == 1, a["contentKids"])
    # headings: one per pane
    if kind == "workspace":
        check(f"{tag}: exactly one h1", len(a["h1Shell"]) + len(a["h1Pane"]) == 1, a["h1Shell"] + a["h1Pane"])
    else:
        check(f"{tag}: exactly one h1", len(a["h1Shell"]) == 1, a["h1Shell"])
        check(f"{tag}: at most one h1 in the embedded workspace", len(a["h1Pane"]) <= 1, a["h1Pane"])
    if "h1" in expect:
        check(f"{tag}: deep link lands (title {expect['h1']})", any(same_text(h, expect["h1"]) for h in a["h1Shell"]), a["h1Shell"])
    if "sr" in expect:
        check(f"{tag}: deep link lands (heading names {expect['sr']})", expect["sr"] in (a["srH1"] or ""), a["srH1"])
    if "tab" in expect:
        check(f"{tag}: deep link lands (selected tab {expect['tab']})", [same_text(x, expect["tab"]) for x in a["selectedTab"]] == [True], a["selectedTab"])
    if "js" in expect:
        check(f"{tag}: deep link lands (page content)", fjs(expect["js"]), expect["js"])
    check(f"{tag}: at most one search field outside the workspace", a["searches"] <= 1, a["searches"])
    if kind == "shell":
        side = a["side"] or {}
        check(f"{tag}: shell sidebar present", side.get("count") == 1 and side.get("shown"), side)
        if "current" in expect:
            check(f"{tag}: sidebar marks {expect['current']}", side.get("current") == [expect["current"]], side.get("current"))
    elif kind == "live":
        check(f"{tag}: no second sidebar beside the live view", a["side"] is None, a["side"])
        check(f"{tag}: workspace sidebar toggle present", a["wsToggle"])
    else:
        check(f"{tag}: workspace sidebar offers Computers", a["wsModes"])
    check(f"{tag}: nothing blurred on or inside the stage", not a["stageBlur"], a["stageBlur"])
    check(f"{tag}: no blur on window surfaces", not a["surfaceBlur"], a["surfaceBlur"])
    check(f"{tag}: glass is a leaf with clean ancestors", not a["problems"], a["problems"])
    check(f"{tag}: at most one blurred element animating", len(a["glassAnimating"]) <= 1, a["glassAnimating"])
    check(f"{tag}: no dialog inside .win", not a["dialogsInWin"], a["dialogsInWin"])
    o = a["overflow"]
    check(f"{tag}: no horizontal overflow", o["doc"] <= o["view"] + 1 and (not o["content"] or o["content"][0] <= o["content"][1] + 1), o)

    # The same screen with the material off: not a single backdrop-filter may remain.
    set_reduced(True)
    off = until("return d.documentElement.dataset.material === 'none';", 3)
    gone = until("""const all=[...d.querySelectorAll('*')];return all.every(e=>[null,'::before','::after'].every(p=>{
      const s=w.getComputedStyle(e,p);return (s.backdropFilter||s.webkitBackdropFilter||'none')==='none';}));""", 3)
    if not gone:
        settle()
    b = audit()
    check(f"{tag}: reduced transparency: data-material=none", off and b["material"] == "none", b["material"])
    check(f"{tag}: reduced transparency: no backdrop-filter at all", not b["blurAll"], b["blurAll"])
    if lang == "zh":
        settle()
        shot(f"{name}-{lang}-{theme}-reduced")
    set_reduced(False)
    check(f"{tag}: glass comes back", until("return d.documentElement.dataset.material === 'preview';", 3))


# ---------------------------------------------------------------- interactions

def click(selector_js):
    """Click the element `selector_js` (an expression over d) finds; false when it is missing."""
    return fjs(f"const e = {selector_js}; if (!e || e.disabled) return false; e.click(); return true;")


def side_row(label):
    return f"[...d.querySelectorAll('nav.side .nv')].find(b => (b.querySelector('.lbl')||b).textContent.trim() === {json.dumps(label)})"


def on_devices(lang, empty=False):
    cards = "!d.querySelector('.device-card')" if empty else "d.querySelectorAll('.device-card').length === 4"
    return until(f"""const h=[...d.querySelectorAll('h1')].map(x=>x.textContent.trim());
      return !!d.querySelector('.devices-page') && {cards} && h.length === 1 && h[0] === {json.dumps(T(lang, 'devices'))}
        && !d.querySelector('.spage, .cpage, .nt-session, .win > .ws');""")


def interactions():
    lang = "zh"
    # 1. Settings > General > Glass effects: reduced through the real select, and back.
    if load(f"shotlang={lang}&shottheme=dark&view=settings&sec=general", ".spage"):
        label = T(lang, "gn_transparency")
        pick = lambda value: fjs(f"""const s = d.querySelector('select[aria-label="{label}"]'); if (!s) return false;
          s.value = {json.dumps(value)}; s.dispatchEvent(new w.Event('change', {{bubbles: true}})); return true;""")
        check("glass select: present in Settings > General", pick("reduced"))
        check("glass select: reduced turns the material off",
              until("return d.documentElement.dataset.material === 'none' && d.documentElement.dataset.transparency === 'reduced';"))
        settle()
        a = audit()
        check("glass select: no backdrop-filter in Settings", not a["blurAll"], a["blurAll"])
        shot("x-settings-reduced-zh-dark")
        check("glass select: sidebar reaches Devices", click(side_row(T(lang, "devices"))) and on_devices(lang))
        settle()
        a = audit()
        check("glass select: still off on Devices, no backdrop-filter", a["material"] == "none" and not a["blurAll"], [a["material"], a["blurAll"]])
        shot("x-devices-reduced-zh-dark")
        check("glass select: sidebar reaches Settings", click(side_row(T(lang, "settings"))) and until("return !!d.querySelector('.spage');"))
        check("glass select: back to full", until(f"return !!d.querySelector('select[aria-label=\"{label}\"]');") and pick("full"))
        check("glass select: material and toolbar glass come back",
              until("return d.documentElement.dataset.material === 'preview' && w.getComputedStyle(d.querySelector('.gl-cap')).backdropFilter !== 'none';"))
    else:
        check("glass select: Settings > General loads", False)

    # 2. Settings -> sidebar -> Devices, then < and > through the history.
    if load(f"shotlang={lang}&shottheme=light&view=settings&sec=network", ".spage"):
        check("nav: Settings -> sidebar -> Devices", click(side_row(T(lang, "devices"))) and on_devices(lang))
        net = T(lang, "set_network")
        check("nav: < returns to Settings > Network", click("d.querySelector('.gl-toolbar .gl-nav .back')") and until(
            f"return !!d.querySelector('.spage') && [...d.querySelectorAll('.gl-toolbar [role=tab][aria-selected=true]')].map(e=>e.textContent.trim()).join() === {json.dumps(net)};"))
        check("nav: > goes forward to Devices", click("d.querySelector('.gl-toolbar .gl-nav .gl-fwd')") and on_devices(lang))
    else:
        check("nav: Settings > Network loads", False)

    # 3. A live session -> the workspace sidebar -> Computers (Devices); the stream goes away.
    if load(f"shotlang={lang}&shottheme=dark&view=session&host=lab-pc", ".nt-session"):
        opened = click("d.querySelector('.nt-workspace-pane .ws-corner .ws-ib')") and until(
            "return !!d.querySelector('.nt-workspace-pane .ws-side [data-mode=computers]');")
        check("nav: session -> workspace sidebar opens", opened)
        if opened:
            settle()
            shot("x-session-sidebar-zh-dark")
            check("nav: session -> workspace sidebar -> Computers reaches Devices",
                  click("d.querySelector('.nt-workspace-pane .ws-side [data-mode=computers]')") and on_devices(lang))
            check("nav: the live stage is gone after leaving", not fjs("return !!d.querySelector('.nt-stage');"))
    else:
        check("nav: session loads", False)

    # 4. A deep-linked session's < goes to its computer, whose sidebar reaches Devices.
    if load(f"shotlang={lang}&shottheme=light&view=session&host=lab-pc", ".nt-session"):
        check("nav: session < goes to its computer", click("d.querySelector('.nt-session .gl-nav .back')") and until(
            "return !!d.querySelector('.cpage') && [...d.querySelectorAll('h1')].map(h=>h.textContent.trim()).join() === 'lab-pc';"))
        check("nav: computer -> sidebar -> Devices", click(side_row(T(lang, "devices"))) and on_devices(lang))
    else:
        check("nav: session loads (back)", False)

    # 5. The workspace without computers -> Computers.
    if load(f"shotlang={lang}&shottheme=light&empty&view=agents", ".win > .ws"):
        check("nav: workspace -> Computers reaches Devices",
              click("d.querySelector('.ws-side [data-mode=computers]')") and on_devices(lang, empty=True))
    else:
        check("nav: workspace loads", False)

    # 6. A sheet over the computer page: outside .win, a glass leaf with clean ancestors.
    for theme in ("light", "dark"):
        if not load(f"shotlang={lang}&shottheme={theme}&view=computer&host=office-pc&tab=access", ".cpage"):
            check(f"sheet {theme}: computer access loads", False)
            continue
        unpair = T(lang, "unpair", host="office-pc")
        opened = click(f"[...d.querySelectorAll('.cpage button')].find(b => b.textContent.trim() === {json.dumps(unpair)})") and until(
            "return !!d.querySelector('.gl-modal-root [aria-modal=true]');")
        check(f"sheet {theme}: opens", opened)
        if not opened:
            continue
        settle()
        a = audit()
        where = fjs("const s=d.querySelector('.gl-modal-root [aria-modal=true]');return {inWin:!!s.closest('.win'),root:s.closest('.gl-modal-root').parentElement===d.body,cdlg:d.querySelectorAll('.cdlg').length};")
        check(f"sheet {theme}: portalled to body, outside .win", where["root"] and not where["inWin"] and where["cdlg"] >= 1, where)
        check(f"sheet {theme}: no dialog inside .win", not a["dialogsInWin"], a["dialogsInWin"])
        check(f"sheet {theme}: the sheet is glass", any("aria-modal" not in x and ("cdlg" in x or "gl-sheet" in x) for x in a["blurShown"]), a["blurShown"])
        check(f"sheet {theme}: glass is a leaf with clean ancestors", not a["problems"], a["problems"])
        check(f"sheet {theme}: at most one blurred element animating", len(a["glassAnimating"]) <= 1, a["glassAnimating"])
        shot(f"x-sheet-{lang}-{theme}")
        cancel = T(lang, "cancel")
        check(f"sheet {theme}: Cancel closes it", click(f"[...d.querySelectorAll('.gl-modal-root button')].find(b => b.textContent.trim() === {json.dumps(cancel)})")
              and until("return !d.querySelector('.gl-modal-root') && !d.documentElement.hasAttribute('data-modal');"))


# ---------------------------------------------------------------- main

def main():
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "screens").mkdir(exist_ok=True)
    for old in (args.out / "screens").glob("*.png"):
        old.unlink()
    started = time.monotonic()
    limitations = [
        "Browser demo acceptance only: native macOS / Windows material, video, input and transport are not exercised.",
        "Camofox (Firefox headless) does not paint backdrop-filter; blur checks read getComputedStyle, screenshots prove layout and tints only.",
        "-webkit-backdrop-filter does not exist in Firefox; the prefixed-first rule is linted from the CSS source.",
        "Viewport 1280x800 (set through a REST call that never answers, then verified); Camoufox spoofs innerWidth, so the sidebar is pinned expanded.",
        "Contrast (WCAG 4.5:1 on tints over the native material) is not measured here.",
    ]
    lint_styles()

    vite = None
    base = args.url.rstrip("/") if args.url else None
    if base and not re.match(r"^http://(127\.0\.0\.1|localhost):\d+$", base):
        sys.exit("--url must be a local preview (http://127.0.0.1:<port>)")
    if not base:
        port = free_port()
        base = f"http://127.0.0.1:{port}"
        npx = "npx.cmd" if sys.platform == "win32" else "npx"
        vite = subprocess.Popen([npx, "vite", "--port", str(port), "--strictPort", "--host", "127.0.0.1"], cwd=DESKTOP,
                                stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    state["base"] = base
    langs = [x for x in args.langs.split(",") if x]
    themes = [x for x in args.themes.split(",") if x]
    only = [x for x in (args.only or "").split(",") if x]
    error = None
    try:
        wait_http(base + "/")
        # First a same-origin document that does not boot the app (Vite serves the module as text):
        # the storage is set before the app reads it.
        state["tab"] = call("POST", "/tabs", {"userId": USER, "sessionKey": "glass-audit", "url": base + "/src/lib/nav.ts"})["tabId"]
        try:
            call("POST", f"/tabs/{state['tab']}/viewport", {"userId": USER, "width": W, "height": H}, timeout=3)
        except OSError:
            pass  # it applies the size and then never answers (see the docstring)
        size = f"(width: {W}px) and (height: {H}px)"
        if not until(f"return w.matchMedia({json.dumps(size)}).matches;", 10):
            raise RuntimeError(f"the viewport did not become {W}x{H}")
        js("""(() => { localStorage.clear();
          localStorage.setItem('ai-remote.sidebar', JSON.stringify({shell: 'expanded', session: 'compact'}));
          localStorage.setItem('ai-remote.reduce-transparency', '0');
          return true; })()""")
        if not load("shotlang=zh&shottheme=dark&view=devices", ".devices-page"):
            raise RuntimeError("the preview did not render the devices page")
        check("demo backend: the preview is labelled", fjs("return !!d.querySelector('.demo-badge');"))
        engine = fjs("const c=d.querySelector('.gl-toolbar .gl-cap');return c?w.getComputedStyle(c).backdropFilter:null;")
        if not check("engine: getComputedStyle reports backdrop-filter", engine and engine != "none", engine):
            raise RuntimeError("this engine does not report backdrop-filter; the blur checks would be meaningless")

        for lang in langs:
            for name, query, kind, root, expect in screens(lang):
                if only and not any(o in name for o in only):
                    continue
                for theme in themes:
                    before = sum(1 for c in checks if not c["ok"])
                    try:
                        audit_screen(name, query, kind, root, expect, lang, theme)
                    except Exception as e:  # one broken screen must not hide the others
                        check(f"{name} {lang}/{theme}: audit ran", False, str(e))
                    bad = sum(1 for c in checks if not c["ok"]) - before
                    print(f"{'ok  ' if not bad else 'FAIL'} {name} {lang}/{theme}" + (f"  ({bad} failed)" if bad else ""), flush=True)
        if not args.no_interactions and not only:
            try:
                interactions()
            except Exception as e:
                check("interactions ran", False, str(e))
    except Exception as e:
        error = str(e)
        check("audit ran", False, error)
    finally:
        try:
            call("DELETE", f"/sessions/{USER}")
        except Exception:
            pass
        if vite:
            vite.terminate()
            try:
                vite.wait(10)
            except subprocess.TimeoutExpired:
                vite.kill()

    failed = [c for c in checks if not c["ok"]]
    report = {"status": "passed" if not failed else "failed", "passed": len(checks) - len(failed), "failed": len(failed),
              "seconds": round(time.monotonic() - started, 1), "url": base, "screens": str(args.out / "screens"),
              "limitations": limitations, "failures": failed, "checks": checks}
    if error:
        report["error"] = error
    (args.out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(f"{len(checks) - len(failed)} checks passed" + (f", {len(failed)} failed" if failed else "") + f"; screenshots and report.json in {args.out}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
