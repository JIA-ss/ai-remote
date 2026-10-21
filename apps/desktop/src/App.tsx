// The app: the main window (pane + page) or a live session (compact rail + stage + rail).

import { useEffect, useState } from "react";
import { api } from "./lib/api";
import { t } from "./lib/i18n";
import { allPending, store, useStore } from "./lib/store";
import { DevicesPage } from "./ui/Devices";
import { ActivityPage, ApprovalsPage, ComputerPage, EmptyPage, PairPage, WelcomePage } from "./ui/Pages";
import type { ChatTarget } from "./ui/AgentChat";
import { SessionPage } from "./ui/Session";
import { SettingsWindow } from "./ui/Settings";
import { Sidebar, TitleBar, type Nav } from "./ui/Shell";
import { useNow } from "./ui/util";
import type { TrayIntent } from "./lib/types";

type View = { kind: "main"; nav: Nav | null } | { kind: "session"; host: string; chat?: ChatTarget | null; panel?: "chat" };

/** `?view=` opens a page directly (screenshots of the page outside the app). */
function initialView(): View {
  const q = new URLSearchParams(location.search);
  const v = q.get("view");
  const host = q.get("host");
  if (v === "session" && host) return { kind: "session", host, panel: q.get("panel") === "chat" ? "chat" : undefined };
  if (v === "devices" || v === "approvals" || v === "activity" || v === "pair" || v === "settings") return { kind: "main", nav: v };
  if (v === "computer" && host) return { kind: "main", nav: { computer: host } };
  return { kind: "main", nav: null };
}

const WELCOMED = "ai-remote.welcomed";

/** Whether the first-run welcome was seen; after it an empty list shows the empty computer page instead. */
function initialWelcomed(): boolean {
  const v = new URLSearchParams(location.search).get("view");
  if (v === "welcome" || v === "empty") return v === "empty";
  return localStorage.getItem(WELCOMED) === "1";
}

export default function App() {
  const snap = useStore();
  const now = useNow(5000);
  const [view, setView] = useState<View>(initialView);
  const [device, setDevice] = useState("this device");
  const [welcomed, setWelcomed] = useState(initialWelcomed);
  const [deviceQuery, setDeviceQuery] = useState("");
  // The tray menu's "Open Settings", "Show Pairing Code" and "Recent Activity" land here.
  const [intent, setIntent] = useState<TrayIntent | null>(null);
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let stop: (() => void) | null = null;
    let stopHidden: (() => void) | null = null;
    let gone = false;
    void import("@tauri-apps/api/event").then(({ listen }) =>
      listen<Omit<TrayIntent, "n">>("tray-navigate", (e) => {
        const to = e.payload ?? {};
        // No view: the window was only brought back (Dock, a click on the icon); stay where we are.
        if (to.view === "settings") {
          setView({ kind: "main", nav: "settings" });
          setIntent({ ...to, n: Date.now() });
        }
      }).then((un) => {
        if (gone) un();
        else stop = un;
      }),
    );
    // The window went to the tray: a live view must not go on unseen.
    void import("@tauri-apps/api/event").then(({ listen }) =>
      listen("window-hidden", () =>
        setView((v) => (v.kind === "session" ? { kind: "main", nav: { computer: v.host } } : v)),
      ).then((un) => {
        if (gone) un();
        else stopHidden = un;
      }),
    );
    return () => {
      gone = true;
      stop?.();
      stopHidden?.();
    };
  }, []);
  const welcome = () => {
    localStorage.setItem(WELCOMED, "1");
    setWelcomed(true);
  };

  useEffect(() => {
    void store.start();
    api
      .status()
      .then((s) => setDevice(s.device.startsWith("eid:demo") ? "home-laptop" : s.device.slice(0, 12)))
      .catch(() => {});
  }, []);

  // Once there was a computer, the first run is over.
  useEffect(() => {
    if (snap.computers.length > 0 && !welcomed) welcome();
  }, [snap.computers.length, welcomed]);

  const waiting = allPending(snap).length;
  const nav = (n: Nav | "home") =>
    setView({ kind: "main", nav: n === "home" ? "devices" : n });

  if (view.kind === "session") {
    const c = snap.computers.find((x) => x.host.name === view.host);
    if (c) {
      return (
        <SessionPage
          c={c}
          device={device}
          waiting={waiting}
          onBack={() => setView({ kind: "main", nav: { computer: c.host.name } })}
          onNav={nav}
          chat={view.chat ?? null}
          panel={view.chat || view.panel === "chat" ? "chat" : undefined}
        />
      );
    }
  }

  if (view.kind === "main" && view.nav === "settings") return <SettingsWindow onBack={() => nav("home")} intent={intent} />;

  // Start with the whole workspace, so attention and connection state are visible at a glance.
  const active: Nav = view.kind === "main" ? (view.nav ?? "devices") : "devices";
  const computer = active && typeof active === "object" ? snap.computers.find((c) => c.host.name === active.computer) : undefined;

  const pageTitle =
    active === "devices" ? t("devices") : active === "approvals" ? t("approvals") : active === "activity" ? t("activity") : active === "pair" ? t("pair") : computer ? computer.host.name : t("devices");
  let page: React.ReactNode;
  if (!snap.ready) page = <div className="cpage body c2">{t("starting")}</div>;
  else if (snap.error) page = <div className="cpage body c-danger">{snap.error}</div>;
  else if (active === "devices")
    page = (
      <DevicesPage
        computers={snap.computers}
        now={now}
        query={deviceQuery}
        onQuery={setDeviceQuery}
        onPair={() => nav("pair")}
        onApprovals={() => nav("approvals")}
        onDetails={(host) => nav({ computer: host })}
        onWatch={(host) => setView({ kind: "session", host })}
      />
    );
  else if (active === "approvals") page = <ApprovalsPage snap={snap} />;
  else if (active === "activity") page = <ActivityPage snap={snap} />;
  else if (active === "pair")
    page = (
      <PairPage
        onPaired={async (h) => {
          await store.refresh();
          setView({ kind: "main", nav: { computer: h } });
        }}
      />
    );
  else if (computer)
    page = (
      <ComputerPage
        key={computer.host.name}
        c={computer}
        onUnpaired={() => nav("home")}
        onWatch={() => setView({ kind: "session", host: computer.host.name })}
        onActivity={() => nav("activity")}
        onOpenChat={(chat) => setView({ kind: "session", host: computer.host.name, chat, panel: "chat" })}
      />
    );
  else page = <EmptyPage onPair={() => nav("pair")} />;

  // First run: the welcome spread fills the window, without the pane.
  if (!welcomed && snap.ready && !snap.error && snap.computers.length === 0 && (view.kind !== "main" || view.nav === null)) {
    return (
      <div className="win nt-alone" style={{ width: "100vw", height: "100vh" }}>
        <TitleBar />
        <div className="nt-content">
          <WelcomePage
            onPair={() => {
              welcome();
              nav("pair");
            }}
          />
        </div>
      </div>
    );
  }

  return (
    <div className="win" style={{ width: "100vw", height: "100vh" }}>
      <div className="body-row">
        <Sidebar
          computers={snap.computers}
          active={active}
          waiting={waiting}
          now={now}
          query={deviceQuery}
          onQuery={setDeviceQuery}
          onNav={(n) => setView({ kind: "main", nav: n })}
        />
        <main className="page">
          <TitleBar title={pageTitle} />
          <div className="nt-content">{page}</div>
        </main>
      </div>
    </div>
  );
}
