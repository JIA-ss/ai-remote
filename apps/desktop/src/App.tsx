// The app: one persistent window. The sidebar is always there (expanded, or compact on a live
// session); the content column shows one route at a time under a toolbar of glass capsules, with a
// Finder-like history (‹ ›, ⌘[ ⌘]). Settings, the live session, the first-run welcome and pairing are
// routes of this shell, not windows of their own. Routing is lib/nav.ts, keys are lib/shortcuts.ts.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { api } from "./lib/api";
import { t } from "./lib/i18n";
import {
  back as goBack,
  canBack,
  canForward,
  current,
  forward as goForward,
  fromTray,
  hidden,
  navKey,
  parseMemory,
  parseRoute,
  parseSidebarPref,
  push,
  remember,
  replace,
  routeOfNav,
  sidebarState,
  type History,
  type Memory,
  type NavGuard,
  type NavKey,
  type Route,
  type SidebarPref,
} from "./lib/nav";
import { detectOs } from "./lib/platform";
import { mouseActionOf, shortcutOf, type Action } from "./lib/shortcuts";
import { allPending, store, useStore, type Computer, type Snapshot } from "./lib/store";
import type { AgentStatus, TrayIntent } from "./lib/types";
import { DevicesPage } from "./ui/Devices";
import { Icon } from "./ui/Icon";
import { ActivityPage, activityComputers, ApprovalsPage, ComputerPage, ComputerTabStrip, MissingComputer, PairPage, WelcomePage } from "./ui/Pages";
import { SessionPage } from "./ui/Session";
import { SettingsPage, SettingsTabs, settingsTitle } from "./ui/Settings";
import { computerFacts, Facts, ShellContext, Sidebar, TitleBar, type ShellApi } from "./ui/Shell";
import { cx, useNow } from "./ui/util";

const WELCOMED = "ai-remote.welcomed";
const MEMORY = "ai-remote.nav-mem";
const SIDEBAR = "ai-remote.sidebar";
const os = detectOs();

/** A computer's status dot in the toolbar, before its name. */
function Dot({ c }: { c: Computer }) {
  return <i className={cx("gl-dot", c.online === true ? "on" : c.online === false ? "off" : "wait")} aria-hidden="true" />;
}

export default function App() {
  const snap = useStore();
  const now = useNow(5000);
  const [mem, setMem] = useState<Memory>(() => parseMemory(localStorage.getItem(MEMORY)));
  const [boot] = useState(() => parseRoute(location.search, mem));
  const [hist, setHist] = useState<History>(() => ({ stack: [boot.route], i: 0 }));
  const route = current(hist);
  const [welcomed, setWelcomed] = useState(() => boot.welcomed ?? localStorage.getItem(WELCOMED) === "1");
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [statusRound, setStatusRound] = useState(0);
  // The tray menu's "Open Settings", "Show Pairing Code" and "Recent Activity" land here.
  const [intent, setIntent] = useState<TrayIntent | null>(null);
  const [query, setQuery] = useState("");
  const [sidebarPref, setSidebarPref] = useState<SidebarPref>(() => parseSidebarPref(localStorage.getItem(SIDEBAR)));
  // Read once: the narrow-window default must not flip while the window is being resized.
  const [startWidth] = useState(() => window.innerWidth);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const mainRef = useRef<HTMLElement | null>(null);
  const guard = useRef<NavGuard | null>(null);

  const context = route.page === "session" ? "session" : "shell";
  const sidebar = sidebarState(sidebarPref, context, startWidth);
  const waiting = allPending(snap).length;

  // ---- navigation ----
  const memRef = useRef(mem);
  memRef.current = mem;
  // The settings section and computer tab you were last on, for the next sidebar click.
  useEffect(() => {
    const next = remember(memRef.current, route);
    if (next === memRef.current) return;
    localStorage.setItem(MEMORY, JSON.stringify(next));
    setMem(next);
  }, [route]);
  /** Every in-app navigation passes the current page's guard (pairing asks before it is left). */
  const commit = (next: (h: History) => History) => {
    const run = () => setHist(next);
    const g = guard.current;
    if (g && g(() => {
      guard.current = null;
      run();
    }))
      return;
    run();
  };
  const go = (r: Route, how: "push" | "replace" = "push") => commit((h) => (how === "push" ? push(h, r) : replace(h, r)));
  const back = () => commit((h) => goBack(h, mem));
  const forward = () => commit(goForward);
  const nav = (n: NavKey | "home") => go(routeOfNav(n, mem));
  /** Settings › This device with an action for HostPanel, the same path the tray takes. */
  const openSettingsIntent = (to: Omit<TrayIntent, "n">) => {
    const r = fromTray(to);
    if (r) go(r);
    setIntent({ ...to, n: Date.now() });
  };

  const toggleSidebar = () =>
    setSidebarPref((p) => {
      const next = { ...p, [context]: sidebar === "expanded" ? "compact" : "expanded" } as SidebarPref;
      localStorage.setItem(SIDEBAR, JSON.stringify(next));
      return next;
    });
  const focusSearch = () => {
    if (sidebar !== "expanded") toggleSidebar();
    // The field exists in both states (hidden while compact); focus once it shows.
    setTimeout(() => searchRef.current?.focus(), 0);
  };

  // A page's guard only holds while that page is shown.
  useEffect(() => {
    if (route.page !== "pair") guard.current = null;
  }, [route.page]);

  // ---- the tray, the window going to the tray ----
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    const stops: Array<() => void> = [];
    let gone = false;
    const keep = (un: () => void) => (gone ? un() : stops.push(un));
    void import("@tauri-apps/api/event").then(({ listen }) => {
      void listen<Omit<TrayIntent, "n">>("tray-navigate", (e) => {
        const to = e.payload ?? {};
        // No view: the window was only brought back (Dock, a click on the icon); stay where we are.
        const r = fromTray(to);
        if (!r) return;
        setHist((h) => push(h, r));
        setIntent({ ...to, n: Date.now() });
      }).then(keep);
      // A live view must not go on unseen.
      void listen("window-hidden", () => setHist((h) => hidden(h, memRef.current))).then(keep);
    });
    return () => {
      gone = true;
      stops.forEach((un) => un());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ---- the store and this device's status ----
  useEffect(() => {
    void store.start();
  }, []);
  const inSettings = route.page === "settings";
  useEffect(() => {
    api.status().then(setStatus, () => setStatus(null));
  }, [statusRound, inSettings]);

  // ---- first run: the welcome page until there is a computer ----
  useEffect(() => {
    if (!snap.ready || snap.error) return;
    if (snap.computers.length > 0) {
      if (!welcomed) {
        localStorage.setItem(WELCOMED, "1");
        setWelcomed(true);
      }
      if (route.page === "welcome") go({ page: "devices" }, "replace");
      return;
    }
    if (!welcomed && !boot.explicit && hist.stack.length === 1 && route.page === "devices") go({ page: "welcome" }, "replace");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snap.ready, snap.error, snap.computers.length, welcomed, route.page]);
  const leaveWelcome = () => {
    localStorage.setItem(WELCOMED, "1");
    setWelcomed(true);
  };

  // ---- keys and the extra mouse buttons ----
  const act = (a: Action) => {
    switch (a.kind) {
      case "back":
        return back();
      case "forward":
        return forward();
      case "settings":
        return nav("settings");
      case "page":
        return nav(a.page);
      case "pair":
        return nav("pair");
      case "search":
        return focusSearch();
      case "sidebar":
        return toggleSidebar();
      case "fullscreen":
        // Windows' F11 becomes the session's own full-screen key (⌃⌘F) for its listener.
        return window.dispatchEvent(new KeyboardEvent("keydown", { key: "f", ctrlKey: true, metaKey: true }));
    }
  };
  const actRef = useRef(act);
  actRef.current = act;
  const sessionRef = useRef(false);
  sessionRef.current = route.page === "session";
  useEffect(() => {
    const where = (target: EventTarget | null) => {
      const el = target instanceof Element ? target : null;
      return {
        inPicture: !!el?.closest(".nt-picture"),
        inChat: !!el?.closest(".nt-chat"),
        modal: !!document.documentElement.dataset.modal,
        session: sessionRef.current,
      };
    };
    const onKey = (e: KeyboardEvent) => {
      const a = shortcutOf(e, os, where(e.target));
      if (!a) return;
      e.preventDefault();
      actRef.current(a);
    };
    const onMouse = (e: MouseEvent) => {
      const a = mouseActionOf(e.button, where(e.target));
      if (!a) return;
      e.preventDefault();
      actRef.current(a);
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("mouseup", onMouse);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("mouseup", onMouse);
    };
  }, []);

  // ---- the scroll edge: shown once the page scrolled under the toolbar ----
  useEffect(() => {
    const main = mainRef.current;
    if (!main) return;
    delete main.dataset.scrolled;
    const onScroll = (e: Event) => {
      const el = e.target as HTMLElement;
      if (!(el instanceof HTMLElement) || el.closest(".nt-session")) return;
      if (el.scrollTop > 2) main.dataset.scrolled = "1";
      else delete main.dataset.scrolled;
    };
    main.addEventListener("scroll", onScroll, true);
    return () => main.removeEventListener("scroll", onScroll, true);
  }, [route]);

  const shell: ShellApi = {
    sidebar,
    toggleSidebar,
    back,
    forward,
    canBack: canBack(hist, mem),
    canForward: canForward(hist),
    attention: waiting,
  };

  const computerOf = (host: string) => snap.computers.find((c) => c.host.name === host);
  const sessionComputer = route.page === "session" ? computerOf(route.host) : undefined;
  const toolbar = toolbarFor(route, snap, go);

  let content: ReactNode;
  if (sessionComputer && route.page === "session") {
    content = (
      <SessionPage
        key={sessionComputer.host.name}
        c={sessionComputer}
        device={status ? (status.device.startsWith("eid:demo") ? "home-laptop" : status.device.slice(0, 12)) : "this device"}
        waiting={waiting}
        onBack={back}
        onNav={nav}
        chat={route.chat ?? null}
        panel={route.chat || route.panel === "chat" ? "chat" : undefined}
      />
    );
  } else {
    let page: ReactNode;
    if (route.page === "settings")
      page = <SettingsPage sec={route.sec} status={status} onStatus={() => setStatusRound((n) => n + 1)} intent={intent} />;
    else if (!snap.ready) page = <div className="gl-empty gl-starting">{t("starting")}</div>;
    else if (snap.error)
      page = (
        <div className="gl-empty" role="alert">
          <Icon name="warn" size={32} />
          <div className="body c-danger">{snap.error}</div>
          <div>
            <button type="button" className="btn" onClick={() => void store.refresh()}>
              {t("try_again")}
            </button>
          </div>
        </div>
      );
    else if (route.page === "welcome")
      page = (
        <WelcomePage
          onPair={() => {
            leaveWelcome();
            nav("pair");
          }}
          onSetupHost={() => {
            leaveWelcome();
            openSettingsIntent({ view: "settings", sec: "device", action: "enable" });
          }}
        />
      );
    else if (route.page === "devices")
      page = (
        <DevicesPage
          computers={snap.computers}
          now={now}
          query={query}
          onClearQuery={() => setQuery("")}
          onPair={() => nav("pair")}
          onApprovals={() => nav("approvals")}
          onDetails={(host) => nav({ computer: host })}
          onWatch={(host) => go({ page: "session", host })}
        />
      );
    else if (route.page === "approvals") page = <ApprovalsPage snap={snap} onOpenComputer={(host) => nav({ computer: host })} />;
    else if (route.page === "activity")
      page = (
        <ActivityPage
          snap={snap}
          host={route.host}
          onHost={(host) => go(host ? { page: "activity", host } : { page: "activity" }, "replace")}
          onOpenComputer={(host) => nav({ computer: host })}
        />
      );
    else if (route.page === "pair")
      page = (
        <PairPage
          onPaired={async (h) => {
            await store.refresh();
            go({ page: "computer", host: h, tab: "overview" }, "replace");
          }}
          onShowMyCode={() => openSettingsIntent({ view: "settings", sec: "device", action: "pair_code" })}
          registerGuard={(g) => (guard.current = g)}
        />
      );
    else if (route.page === "computer") {
      const c = computerOf(route.host);
      page = c ? (
        <ComputerPage
          key={c.host.name}
          c={c}
          tab={route.tab}
          onUnpaired={() => go({ page: "devices" }, "replace")}
          onWatch={() => go({ page: "session", host: c.host.name })}
          onActivity={(host) => go({ page: "activity", host })}
          onOpenChat={(chat) => go({ page: "session", host: c.host.name, chat, panel: "chat" })}
        />
      ) : (
        <MissingComputer host={route.host} onDevices={() => nav("devices")} />
      );
    } else page = <MissingComputer host={route.host} onDevices={() => nav("devices")} />;
    content = (
      <>
        <div className="gl-edge" aria-hidden="true" />
        <TitleBar {...toolbar} />
        <div className="nt-content">{page}</div>
      </>
    );
  }

  return (
    <ShellContext.Provider value={shell}>
      <div className="win gl-win" data-sidebar={sidebar} data-route={route.page}>
        <div className="body-row">
          <Sidebar
            computers={snap.computers}
            active={navKey(route)}
            waiting={waiting}
            now={now}
            query={query}
            onQuery={setQuery}
            onNav={nav}
            state={sidebar}
            onToggle={toggleSidebar}
            onSearch={focusSearch}
            searchRef={searchRef}
          />
          <main
            className="page gl-main"
            ref={mainRef}
            data-edge={route.page === "activity" ? "hard" : "soft"}
          >
            {content}
          </main>
        </div>
      </div>
    </ShellContext.Provider>
  );
}

/** What the toolbar shows for a route: title, a second line, the centre control, actions, the one
 *  tinted action. (The session draws its own toolbar.) */
function toolbarFor(route: Route, snap: Snapshot, go: (r: Route, how?: "push" | "replace") => void): Parameters<typeof TitleBar>[0] {
  switch (route.page) {
    case "welcome":
      return { bare: true };
    case "devices": {
      const online = snap.computers.filter((c) => c.online === true).length;
      return {
        title: t("devices"),
        subtitle: snap.ready && !snap.error ? t("devices_subtitle", { online, n: snap.computers.length }) : undefined,
        primary: (
          <button type="button" className="btn primary" onClick={() => go({ page: "pair" })}>
            <Icon name="add" size={14} />
            <span className="lbl">{t("pair")}</span>
          </button>
        ),
      };
    }
    case "approvals": {
      const n = allPending(snap).length;
      return { title: t("approvals"), subtitle: n > 0 ? t("approvals_subtitle", { n }) : t("approvals_clear") };
    }
    case "activity": {
      const shown = activityComputers(snap, route.host);
      const steps = shown.reduce((sum, c) => sum + c.feed.steps.length, 0);
      return {
        title: t("activity"),
        subtitle: t("activity_subtitle", { scope: route.host ?? t("activity_all"), n: steps }),
        end: (
          <span className="nt-select-wrap">
            <select
              className="nt-select"
              aria-label={t("activity_filter")}
              value={route.host ?? ""}
              onChange={(e) => go(e.target.value ? { page: "activity", host: e.target.value } : { page: "activity" }, "replace")}
            >
              <option value="">{t("activity_all")}</option>
              {snap.computers.map((c) => (
                <option key={c.host.name} value={c.host.name}>
                  {c.host.name}
                </option>
              ))}
            </select>
            <Icon name="chevD" size={12} />
          </span>
        ),
      };
    }
    case "pair":
      return { title: t("pair") };
    case "settings":
      return {
        srTitle: settingsTitle(route.sec),
        center: <SettingsTabs sec={route.sec} onSec={(sec) => go({ page: "settings", sec }, "replace")} />,
      };
    case "computer": {
      const c = snap.computers.find((x) => x.host.name === route.host);
      if (!c) return { title: route.host };
      const canWatch = c.online === true && c.feed.level !== null;
      return {
        title: c.host.name,
        lead: <Dot c={c} />,
        subtitle: <Facts items={computerFacts(c)} />,
        center: <ComputerTabStrip tab={route.tab} onTab={(tab) => go({ page: "computer", host: c.host.name, tab }, "replace")} />,
        primary: canWatch ? (
          <button type="button" className="btn primary" onClick={() => go({ page: "session", host: c.host.name })}>
            <Icon name="eye" size={14} />
            <span className="lbl">{t("watch_live")}</span>
          </button>
        ) : undefined,
      };
    }
    case "session":
      return { title: route.host };
  }
}
