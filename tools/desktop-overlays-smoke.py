#!/usr/bin/env python3
"""Glass sheets in the desktop browser preview, through headless Camofox REST.

  CAMOFOX_INTERACTIVE=off python3 tools/desktop-overlays-smoke.py --url http://localhost:1520

Uses the durable ai-remote-glass2 session and closes only this script's tab. Confirm, EnableDialog
and PairCodeDialog open through their real pages. The multiple-card/Esc checks seed two requests
into the real in-memory store and stub only their deny operation; a real Confirm component is
mounted for the modal-over-card check. No product demo fixture or source module is changed.
Screenshots prove layout only: Camofox does not render WebKit's backdrop-filter. Native material,
system install prompts and real host pairing still need native acceptance.
"""
import argparse
import json
import os
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--url", default="http://localhost:1520")
parser.add_argument("--camofox", default="http://127.0.0.1:9377")
parser.add_argument("--session", default="ai-remote-glass2")
parser.add_argument("--out", type=Path, default=ROOT / "target/desktop-overlays")
args = parser.parse_args()
USER = "camofox-" + args.session
checks = []
tab = None
counter = 0


def call(method, path, body=None, raw=False, timeout=60):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(args.camofox + path, data=data, method=method,
                                     headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            payload = response.read()
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"Camofox {path}: {error.code} {error.read().decode(errors='replace')[:500]}") from None
    result = payload if raw else json.loads(payload or b"{}")
    if not raw and result.get("ok") is False:
        raise RuntimeError(result)
    return result


def js(expression):
    return call("POST", f"/tabs/{tab}/evaluate", {"userId": USER, "expression": expression}).get("result")


def until(expression, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = js(expression)
        if result:
            return result
        time.sleep(0.15)
    return False


def check(name, ok, detail=None):
    checks.append({"name": name, "ok": bool(ok), **({"detail": detail} if detail is not None else {})})
    print(f"{'PASS' if ok else 'FAIL'} {name}" + (f" ({detail})" if not ok else ""), flush=True)
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def press(key):
    call("POST", f"/tabs/{tab}/press", {"userId": USER, "key": key})


def shot(name):
    (args.out / f"{name}.png").write_bytes(call("GET", f"/tabs/{tab}/screenshot?userId={USER}&fullPage=true", raw=True))


def page(query, ready="document.querySelector('h1')"):
    call("POST", f"/tabs/{tab}/navigate", {"userId": USER, "url": f"{args.url.rstrip('/')}/?{query}"})
    if not until(ready):
        raise AssertionError(f"page not ready: {query}")


def main_js(source):
    """A module script runs in the app's world (Camofox evaluate is an isolated world)."""
    global counter
    counter += 1
    key = f"overlaySmoke{counter}"
    # Vite's transformed imports can have ?t= after an HMR edit. Importing the bare store URL
    # would create a second store, so resolve the actual app import on every operation.
    for name in ["api", "store", "platform"]:
        source = source.replace(f"await import('/src/lib/{name}.ts')", f"await appImport('{name}')")
    imports = r"""const appSource=await(await fetch('/src/App.tsx',{cache:'no-store'})).text();
      const appImport=name=>{const path='/src/lib/'+name+'.ts';
        const exact=[...appSource.matchAll(/\bfrom\s+["']([^"']+)["']/g)].map(m=>m[1]).find(s=>s.split('?')[0]===path);
        return import(exact??path);};"""
    code = f"try {{ {imports} const result = await (async () => {{ {source} }})(); document.body.dataset.{key} = JSON.stringify({{ok:true,result}}); }} catch (e) {{ document.body.dataset.{key} = JSON.stringify({{ok:false,error:String(e),stack:e.stack}}); }}"
    js(f"(() => {{ const s=document.createElement('script'); s.type='module'; s.textContent={json.dumps(code)}; document.body.append(s); return true; }})()")
    result = until(f"document.body.dataset.{key}")
    if not result:
        raise AssertionError("app-world module did not return")
    value = json.loads(result)
    if not value["ok"]:
        raise AssertionError(value)
    return value.get("result")


def trigger(text):
    expression = f"""(() => {{
      const b=[...document.querySelectorAll('#root button')].find(b=>b.textContent.trim()==={json.dumps(text)});
      if(!b) return false; b.id='gl-smoke-trigger'; b.focus(); b.click(); return true;
    }})()"""
    if not until(expression):
        raise AssertionError(f"trigger missing: {text}")


DIALOG = '.gl-modal-root [aria-modal="true"]'
FOCUSABLE = """[...d.querySelectorAll('button,input,select,textarea,a[href],[tabindex]')]
  .filter(e=>e.tabIndex>=0&&!e.matches(':disabled,[inert],[inert] *')&&e.getClientRects().length)"""


def inspect_modal(name):
    check(f"{name}: opens", until(f"document.querySelector({json.dumps(DIALOG)})"))
    time.sleep(0.3)  # the sheet's 240ms entrance animation has settled before geometry/screenshots
    state = js(f"""(() => {{ const d=document.querySelector({json.dumps(DIALOG)}); return {{
      bodyPortal:d.parentElement.parentElement===document.body, inWin:!!d.closest('.win'),
      flag:document.documentElement.dataset.modal, inert:document.getElementById('root').inert,
      material:document.documentElement.dataset.material,
      focus:d.contains(document.activeElement), thick:d.classList.contains('thick'),
      nested:!!d.querySelector('.gl-glass,.cdlg,.menu'),
      ancestorProblems:[...(() => {{const a=[];for(let e=d.parentElement;e;e=e.parentElement)a.push(e);return a;}})()]
        .filter(e=>{{const s=getComputedStyle(e);return Number(s.opacity)<1||s.filter!=='none'||s.maskImage!=='none'||s.willChange!=='auto';}})
        .map(e=>e.className||e.tagName),
      background:getComputedStyle(d).backgroundColor,
      blur:getComputedStyle(d).backdropFilter || getComputedStyle(d).webkitBackdropFilter || 'none'
    }}; }})()""")
    check(f"{name}: body portal outside .win", state["bodyPortal"] and not state["inWin"], state)
    check(f"{name}: modal flag and background inert", int(state["flag"] or "0") >= 1 and state["inert"], state)
    check(f"{name}: focus is inside sheet", state["focus"], state)
    check(f"{name}: thick glass leaf with safe ancestors", state["thick"] and not state["nested"] and not state["ancestorProblems"], state)
    check(f"{name}: preview glass blur is configured", state["material"] == "preview" and "blur(" in state["blur"], state)
    return state


def tab_loop(name):
    js(f"(() => {{const d=document.querySelector({json.dumps(DIALOG)});const a={FOCUSABLE};a.at(-1).focus();}})()")
    press("Tab")
    check(f"{name}: Tab wraps last to first", js(f"(() => {{const d=document.querySelector({json.dumps(DIALOG)});const a={FOCUSABLE};return document.activeElement===a[0];}})()"))
    press("Shift+Tab")
    check(f"{name}: Shift+Tab wraps first to last", js(f"(() => {{const d=document.querySelector({json.dumps(DIALOG)});const a={FOCUSABLE};return document.activeElement===a.at(-1);}})()"))


def shell_guard(name):
    before = js("({heading:document.querySelector('h1')?.textContent,current:[...document.querySelectorAll('[aria-current=page]')].map(e=>e.textContent)})")
    js(f"""(() => {{const d=document.querySelector({json.dumps(DIALOG)});
      for(const key of ['1',',','[']) d.dispatchEvent(new KeyboardEvent('keydown',{{key,metaKey:true,bubbles:true,cancelable:true}}));
      for(const key of ['1',',']) d.dispatchEvent(new KeyboardEvent('keydown',{{key,ctrlKey:true,bubbles:true,cancelable:true}}));
      d.dispatchEvent(new MouseEvent('mouseup',{{button:3,bubbles:true,cancelable:true}})); return true;}})()""")
    time.sleep(0.2)
    after = js("({heading:document.querySelector('h1')?.textContent,current:[...document.querySelectorAll('[aria-current=page]')].map(e=>e.textContent)})")
    check(f"{name}: shell page/settings/history shortcuts stay blocked", before == after and js(f"!!document.querySelector({json.dumps(DIALOG)})"), {"before": before, "after": after})


def closed(name, restore=True):
    check(f"{name}: closes", until(f"!document.querySelector({json.dumps(DIALOG)})"))
    state = js("({flag:document.documentElement.hasAttribute('data-modal'),inert:document.getElementById('root').inert,focus:document.activeElement?.id})")
    check(f"{name}: clears modal flag and inert", not state["flag"] and not state["inert"], state)
    if restore:
        check(f"{name}: restores trigger focus", state["focus"] == "gl-smoke-trigger", state)


def main():
    global tab
    os.environ["CAMOFOX_INTERACTIVE"] = "off"
    args.out.mkdir(parents=True, exist_ok=True)
    limitations = ["Browser demo acceptance; native macOS/Windows glass rendering and real host install/pairing are not exercised.",
                   "Camofox screenshots show layout; backdrop-filter rendering is not supported by this headless engine.",
                   "Multiple approvals and modal-over-card checks use in-memory store events and deny spies for fixture IDs only.",
                   "Shell shortcut guard uses dispatched DOM key/mouse events; Tab and Escape use native Camofox key presses."]
    started = time.monotonic()
    original_transparency = None
    try:
        # Camofox starts its browser on the first tab, as the other two smokes rely on.
        if not call("GET", "/health").get("ok"):
            raise RuntimeError(f"Camofox at {args.camofox} is not healthy")
        tab = call("POST", "/tabs", {"userId": USER, "sessionKey": "overlays-smoke", "url": args.url})["tabId"]
        try:
            # Camofox applies the size and then may never answer on a viewport-less context, so do not
            # wait for the reply; the size itself is verified below.
            call("POST", f"/tabs/{tab}/viewport", {"userId": USER, "width": 1280, "height": 800}, timeout=3)
        except OSError:
            pass
        if not until("matchMedia('(width: 1280px) and (height: 800px)').matches"):
            raise RuntimeError("the viewport did not become 1280x800")
        original_transparency = js("localStorage.getItem('ai-remote.reduce-transparency')")
        main_js("const {setReducedTransparency}=await import('/src/lib/platform.ts');setReducedTransparency(false);return true;")
        for theme in ["light", "dark"]:
            page(f"shotlang=zh&shottheme={theme}&view=computer&host=office-pc&tab=access",
                 "[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='解除与 office-pc 的配对')")
            trigger("解除与 office-pc 的配对")
            inspect_modal(f"Confirm {theme}")
            check(f"Confirm {theme}: Cancel gets initial focus", js("document.activeElement?.textContent.trim()==='取消'"))
            tab_loop(f"Confirm {theme}")
            shell_guard(f"Confirm {theme}")
            shot(f"1-confirm-{theme}")
            press("Escape")
            closed(f"Confirm {theme}")

            page(f"shotlang=zh&shottheme={theme}&view=settings&sec=device&shotstate=fresh",
                 "[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='开启远程控制…')")
            trigger("开启远程控制…")
            inspect_modal(f"Enable {theme}")
            check(f"Enable {theme}: install plan arrives", until("document.querySelector('.gl-sheet')?.textContent.includes('write /Library/LaunchAgents')"))
            tab_loop(f"Enable {theme}")
            shell_guard(f"Enable {theme}")
            shot(f"2-enable-{theme}")
            press("Escape")
            closed(f"Enable {theme}")

            page(f"shotlang=zh&shottheme={theme}&view=settings&sec=device",
                 "[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='显示配对码…')")
            main_js("""const {api}=await import('/src/lib/api.ts');
              const withdraw=api.localWithdrawCode.bind(api); document.body.dataset.withdrawCount='0';
              api.localWithdrawCode=async()=>{document.body.dataset.withdrawCount=String(Number(document.body.dataset.withdrawCount)+1);return withdraw();};
              return true;""")
            trigger("显示配对码…")
            inspect_modal(f"PairCode {theme}")
            check(f"PairCode {theme}: real demo code loads", until("document.querySelector('.nt-pair')?.textContent.includes('1407 1145')"))
            size = js("(()=>{const c=document.querySelector('.nt-code');return {width:c.clientWidth,scroll:c.scrollWidth};})()")
            check(f"PairCode {theme}: full code fits its column", size["scroll"] <= size["width"], size)
            pair_started = time.monotonic()
            tab_loop(f"PairCode {theme}")
            shell_guard(f"PairCode {theme}")
            shot(f"3-pair-code-{theme}")
            if theme == "dark":
                main_js("const {setReducedTransparency}=await import('/src/lib/platform.ts');setReducedTransparency(true);return true;")
                check("Reduce transparency: data-material becomes none", until("document.documentElement.dataset.material==='none'"))
                state = js("""[...document.querySelectorAll('*')].filter(e=>e.getClientRects().length).map(e=>({tag:e.tagName,cls:e.className,blur:getComputedStyle(e).backdropFilter||getComputedStyle(e).webkitBackdropFilter||'none'})).filter(s=>s.blur!=='none')""")
                check("Reduce transparency: no visible element has blur", not state, state)
                opaque = until("(()=>{const c=getComputedStyle(document.querySelector('.gl-sheet')).backgroundColor;return c.startsWith('rgb(')||c.endsWith(', 1)');})()")
                background = js("getComputedStyle(document.querySelector('.gl-sheet')).backgroundColor")
                check("Reduce transparency: sheet background is opaque after color transition", opaque, background)
                shot("4-pair-code-reduced-dark")
                main_js("const {setReducedTransparency}=await import('/src/lib/platform.ts');setReducedTransparency(false);return true;")
            press("Escape")
            closed(f"PairCode {theme}")
            check(f"PairCode {theme}: closing calls withdrawal exactly once", until("document.body.dataset.withdrawCount==='1'"))
            # The demo emits a paired audit event 12 seconds after code generation. After closing,
            # this must remain absent, independently of the spy recording the method invocation.
            remaining = 12.4 - (time.monotonic() - pair_started)
            if remaining > 0:
                time.sleep(remaining)
            recent = main_js("const {api}=await import('/src/lib/api.ts');return await api.localRecent(40);")
            check(f"PairCode {theme}: withdrawn demo code produces no paired event", not any(x["kind"] == "paired" for x in recent), recent)

        # Keep the real demo install pending, so Escape and disabled focus can be observed without
        # racing the demo's 1.2-second completion. The original install implementation runs on release.
        page("shotlang=zh&shottheme=light&view=settings&sec=device&shotstate=fresh",
             "[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='开启远程控制…')")
        main_js("""const {api}=await import('/src/lib/api.ts'); const setup=api.hostSetup.bind(api);
          api.hostSetup=async(...args)=>{document.body.dataset.installStarted='true';
            await new Promise(resolve=>document.addEventListener('overlay-install-release',resolve,{once:true}));
            return setup(...args);};return true;""")
        trigger("开启远程控制…")
        if not until("document.querySelector('.gl-sheet button.primary:not(:disabled)')"):
            raise AssertionError("install plan did not enable primary action")
        js("(()=>{const b=document.querySelector('.gl-sheet button.primary');b.focus();b.click();})()")
        focused = until("document.body.dataset.installStarted==='true' && document.querySelector('.gl-sheet')?.contains(document.activeElement) && [...document.querySelectorAll('.gl-sheet button')].every(b=>b.disabled)")
        check("Enable running: focused disabled action falls back inside sheet", focused,
              js("({started:document.body.dataset.installStarted,active:document.activeElement?.outerHTML.slice(0,200),buttons:[...document.querySelectorAll('.gl-sheet button')].map(b=>({text:b.textContent,disabled:b.disabled}))})"))
        press("Escape")
        check("Enable running: Escape preserves non-dismissible sheet", js("!!document.querySelector('.gl-sheet') && document.getElementById('root').inert"))
        js("document.querySelector('.gl-scrim').click()")
        check("Enable running: scrim preserves non-dismissible sheet", js("!!document.querySelector('.gl-sheet') && document.getElementById('root').inert"))
        shot("5-enable-running-light")
        js("document.dispatchEvent(new Event('overlay-install-release'))")
        closed("Enable completed")

        main_js("""const react=await import('/node_modules/.vite/deps/react.js');const React=react.default??react;
          const dom=await import('/node_modules/.vite/deps/react-dom_client.js');const {createRoot}=dom.default??dom;
          const {Confirm}=await import('/src/ui/ComputerTabs.tsx');
          const mount=document.createElement('div');document.getElementById('root').append(mount);
          const r=createRoot(mount);r.render(React.createElement(Confirm,{title:'Busy confirm fixture',body:'A pending action cannot be dismissed.',verb:'Run',
            onYes:()=>new Promise((resolve,reject)=>document.addEventListener('overlay-confirm-reject',()=>reject(new Error('Confirm fixture rejection')),{once:true})),
            onNo:()=>{r.unmount();mount.remove();}}));return true;""")
        check("Confirm running: fixture opens", until("document.querySelector('.gl-sheet')?.getAttribute('aria-label')==='Busy confirm fixture'"))
        js("document.querySelector('.gl-sheet .primary').focus();document.querySelector('.gl-sheet .primary').click()")
        check("Confirm running: both actions disable and focus stays inside", until("document.querySelector('.gl-sheet')?.contains(document.activeElement) && [...document.querySelectorAll('.gl-sheet button')].every(b=>b.disabled)"))
        press("Escape")
        js("document.querySelector('.gl-scrim').click()")
        check("Confirm running: Escape and scrim cannot dismiss pending action", js("document.querySelector('.gl-sheet')?.getAttribute('aria-label')==='Busy confirm fixture'"))
        js("document.dispatchEvent(new Event('overlay-confirm-reject'))")
        check("Confirm failure: error stays visible and actions recover", until("document.querySelector('.gl-sheet')?.textContent.includes('Confirm fixture rejection') && [...document.querySelectorAll('.gl-sheet button')].every(b=>!b.disabled)"))
        shot("5b-confirm-failure-light")
        press("Escape")
        closed("Confirm failure", restore=False)

        page("shotlang=zh&shottheme=light&view=approvals", "document.querySelector('.decide')")
        result = main_js("""const {store}=await import('/src/lib/store.ts'); const {api}=await import('/src/lib/api.ts');
          document.body.dataset.denials='[]'; const deny=api.deny.bind(api);
          api.deny=async(host,id,note)=>{
            document.body.dataset.denials=JSON.stringify([...JSON.parse(document.body.dataset.denials),id]);
            if(id.startsWith('overlay-fixture-')) store.onEvent(host,{kind:'approval_resolved',ts:Date.now(),id,verdict:'denied',by:'overlay-smoke'});
            else return deny(host,id,note);
          };
          for(let n=1;n<=2;n++) store.onEvent('office-pc',{kind:'approval',ts:Date.now(),remote:true,
            id:'overlay-fixture-'+n,session:'overlay-fixture',peer:'home-laptop',tool:'shell',
            intent:'Overlay keyboard acceptance fixture '+n,args_hash:'overlay-'+n,args_preview:'{"command":"echo overlay-'+n+'"}',expires_ms:Date.now()+600000});
          return store.get().computers.find(c=>c.host.name==='office-pc').feed.pending.length;""")
        check("Decision: three real cards are mounted with two in-memory fixtures", result == 3 and until("document.querySelectorAll('.decide').length===3"), result)
        press("Escape")
        check("Decision: one Esc denies only the first card", until("document.querySelectorAll('.decide').length===2 && JSON.parse(document.body.dataset.denials).length===1"))
        js("document.querySelectorAll('.decide')[1].querySelector('button:not(:disabled)').focus()")
        press("Escape")
        check("Decision: focused card owns Esc", until("document.querySelectorAll('.decide').length===1 && JSON.parse(document.body.dataset.denials).at(-1)==='overlay-fixture-2'"))
        # An Escape in a text reply first closes the reply editor and must preserve its request.
        js("document.querySelector('.decide .tellr').click()")
        check("Decision: reply editor opens", until("document.querySelector('.decide textarea')===document.activeElement"))
        press("Escape")
        check("Decision: editor Esc preserves decision", until("!document.querySelector('.decide textarea') && document.querySelectorAll('.decide').length===1 && JSON.parse(document.body.dataset.denials).length===2"))
        main_js("""const react=await import('/node_modules/.vite/deps/react.js');const React=react.default??react;
          const dom=await import('/node_modules/.vite/deps/react-dom_client.js');const {createRoot}=dom.default??dom;
          const {Confirm}=await import('/src/ui/ComputerTabs.tsx');
          const mount=document.createElement('div');mount.id='overlay-confirm-fixture';document.getElementById('root').append(mount);
          const r=createRoot(mount);r.render(React.createElement(Confirm,{title:'Modal over pending decision',body:'Esc must close this sheet and preserve the pending request.',verb:'Confirm',onYes:async()=>{},onNo:()=>r.unmount()}));return true;""")
        inspect_modal("Confirm over Decision")
        main_js("""const react=await import('/node_modules/.vite/deps/react.js');const React=react.default??react;
          const dom=await import('/node_modules/.vite/deps/react-dom_client.js');const {createRoot}=dom.default??dom;
          const {Confirm}=await import('/src/ui/ComputerTabs.tsx');
          const mount=document.createElement('div');mount.id='overlay-nested-fixture';document.getElementById('root').append(mount);
          const r=createRoot(mount);r.render(React.createElement(Confirm,{title:'Top sheet',body:'Only the top sheet owns Escape.',verb:'Confirm',onYes:async()=>{},onNo:()=>r.unmount()}));return true;""")
        check("Modal stack: count and inert belong to two layers", until("document.documentElement.dataset.modal==='2' && document.querySelectorAll('.gl-modal-root').length===2 && document.querySelectorAll('.gl-modal-root')[0].inert && !document.querySelectorAll('.gl-modal-root')[1].inert"))
        press("Escape")
        check("Modal stack: one Esc removes only top sheet", until("document.documentElement.dataset.modal==='1' && document.querySelectorAll('.gl-modal-root').length===1 && document.querySelector('.gl-sheet').contains(document.activeElement) && !document.querySelector('.gl-modal-root').inert && document.getElementById('root').inert"))
        press("Escape")
        closed("Confirm over Decision", restore=False)
        check("Decision: modal Esc does not deny request behind it", js("document.querySelectorAll('.decide').length===1 && JSON.parse(document.body.dataset.denials).length===2"))
        js("document.querySelector('.decide').insertAdjacentHTML('beforeend','<div role=menu id=overlay-menu-fixture></div>')")
        press("Escape")
        check("Decision: menu Esc does not deny request behind it", js("document.querySelectorAll('.decide').length===1 && JSON.parse(document.body.dataset.denials).length===2"))
        js("document.getElementById('overlay-menu-fixture').remove()")
        press("Escape")
        check("Decision: next Esc handles remaining single card", until("document.querySelectorAll('.decide').length===0 && JSON.parse(document.body.dataset.denials).length===3"))
        # The live Workspace shares a page with approval cards. Chat's Escape must never
        # deny that pane's approval, while focus in the pane still resolves just one card.
        main_js("""const react=await import('/node_modules/.vite/deps/react.js');const React=react.default??react;
          const dom=await import('/node_modules/.vite/deps/react-dom_client.js');const {createRoot}=dom.default??dom;
          const {DecisionCard}=await import('/src/ui/Decide.tsx');
          const scope=React.createRef();const mount=document.createElement('div');mount.id='overlay-scope-fixture';document.getElementById('root').append(mount);
          const r=createRoot(mount);let remaining=[1,2];
          const render=()=>r.render(React.createElement('div',null,
            React.createElement('button',{id:'overlay-outside-scope'},'Chat control'),
            React.createElement('div',{ref:scope,id:'overlay-approval-scope'},remaining.map(n=>React.createElement(DecisionCard,{
              key:n,host:'office-pc',agent:'Fixture',escapeScope:scope,
              p:{id:'overlay-fixture-scope-'+n,peer:'home-laptop',tool:'shell',intent:'Scoped Escape '+n,argsPreview:'{}',expiresMs:Date.now()+600000},
              onDone:()=>{remaining=remaining.filter(x=>x!==n);render();}
            })))));
          render();document.addEventListener('overlay-scope-remove',()=>{r.unmount();mount.remove();},{once:true});return true;""")
        check("Decision scope: two cards mount beside chat control", until("document.querySelectorAll('#overlay-approval-scope .decide').length===2"))
        js("document.getElementById('overlay-outside-scope').focus()")
        press("Escape")
        check("Decision scope: chat Escape preserves both approvals", js("document.querySelectorAll('#overlay-approval-scope .decide').length===2 && JSON.parse(document.body.dataset.denials).length===3"))
        js("document.querySelectorAll('#overlay-approval-scope .decide')[1].querySelector('button:not(:disabled)').focus()")
        press("Escape")
        check("Decision scope: focused second card alone is denied", until("document.querySelectorAll('#overlay-approval-scope .decide').length===1 && JSON.parse(document.body.dataset.denials).at(-1)==='overlay-fixture-scope-2'"))
        js("document.querySelector('#overlay-approval-scope button:not(:disabled)').focus(); document.activeElement.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,isComposing:true})); document.activeElement.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,repeat:true}))")
        check("Decision scope: composing and repeated Escape preserve approval", js("document.querySelectorAll('#overlay-approval-scope .decide').length===1 && JSON.parse(document.body.dataset.denials).length===4"))
        press("Escape")
        check("Decision scope: next deliberate Escape resolves last scoped card", until("!document.querySelector('#overlay-approval-scope .decide') && JSON.parse(document.body.dataset.denials).at(-1)==='overlay-fixture-scope-1'"))
        js("document.dispatchEvent(new Event('overlay-scope-remove'))")
        report = {"status": "passed", "checks": checks, "limitations": limitations,
                  "seconds": round(time.monotonic() - started, 2), "session": args.session, "url": args.url}
        (args.out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
        print(f"{len(checks)} checks passed; screenshots in {args.out}")
    except Exception as error:
        if tab:
            try:
                shot(f"failure-{int(time.time())}")
            except Exception:
                pass
        (args.out / "report.json").write_text(json.dumps({"status": "failed", "checks": checks,
            "error": str(error), "limitations": limitations}, ensure_ascii=False, indent=2))
        raise
    finally:
        if tab:
            try:
                main_js("const {setReducedTransparency}=await import('/src/lib/platform.ts');setReducedTransparency(" +
                        str(original_transparency == "1").lower() + ");return true;")
                if original_transparency is None:
                    js("localStorage.removeItem('ai-remote.reduce-transparency')")
            except Exception:
                pass
            call("DELETE", f"/tabs/{tab}?userId={USER}")


if __name__ == "__main__":
    main()
