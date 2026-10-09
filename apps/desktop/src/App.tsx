// The app: the main window (pane + page) or a live session (compact rail + stage + rail).

import { useEffect, useState } from "react";
import { api } from "./lib/api";
import { t } from "./lib/i18n";
import { allPending, groupOf, store, useStore } from "./lib/store";
import { ActivityPage, ApprovalsPage, ComputerPage, EmptyPage, PairPage, SettingsPage, WelcomePage } from "./ui/Pages";
import { SessionPage } from "./ui/Session";
import { Sidebar, TitleBar, type Nav } from "./ui/Shell";
import { useNow } from "./ui/util";

type View = { kind: "main"; nav: Nav | null } | { kind: "session"; host: string };

/** `?view=` opens a page directly (screenshots of the page outside the app). */
function initialView(): View {
  const q = new URLSearchParams(location.search);
  const v = q.get("view");
  const host = q.get("host");
  if (v === "session" && host) return { kind: "session", host };
  if (v === "approvals" || v === "activity" || v === "pair" || v === "settings") return { kind: "main", nav: v };
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
    setView({ kind: "main", nav: n === "home" ? null : n });

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
        />
      );
    }
  }

  // With nothing chosen, open the computer that most needs attention.
  const order = ["needs", "you", "working", "idle", "offline"] as const;
  const first = [...snap.computers].sort((a, b) => order.indexOf(groupOf(a, now)) - order.indexOf(groupOf(b, now)))[0];
  const active: Nav | null = view.kind === "main" ? (view.nav ?? (first ? { computer: first.host.name } : null)) : null;
  const computer = active && typeof active === "object" ? snap.computers.find((c) => c.host.name === active.computer) : undefined;

  let page: React.ReactNode;
  if (!snap.ready) page = <div className="cpage body c2">{t("starting")}</div>;
  else if (snap.error) page = <div className="cpage body c-danger">{snap.error}</div>;
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
  else if (active === "settings") page = <SettingsPage />;
  else if (computer)
    page = (
      <ComputerPage
        c={computer}
        onWatch={() => setView({ kind: "session", host: computer.host.name })}
        onActivity={() => nav("activity")}
      />
    );
  else page = <EmptyPage onPair={() => nav("pair")} />;

  // First run: the welcome spread fills the window, without the pane.
  if (!welcomed && snap.ready && !snap.error && snap.computers.length === 0 && (view.kind !== "main" || view.nav === null)) {
    return (
      <div className="win" style={{ width: "100vw", height: "100vh" }}>
        <TitleBar />
        <div style={{ position: "relative", flex: 1, minHeight: 0 }}>
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
      <TitleBar />
      <div className="body-row">
        <Sidebar computers={snap.computers} active={active} waiting={waiting} now={now} onNav={(n) => setView({ kind: "main", nav: n })} />
        <main className="page">{page}</main>
      </div>
    </div>
  );
}
