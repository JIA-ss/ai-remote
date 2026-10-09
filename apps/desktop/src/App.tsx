// The app: the main window (pane + page) or a live session (compact rail + stage + rail).

import { useEffect, useState } from "react";
import { api } from "./lib/api";
import { t } from "./lib/i18n";
import { allPending, groupOf, store, useStore } from "./lib/store";
import { ActivityPage, ApprovalsPage, ComputerPage, EmptyHome, PairPage, SettingsPage } from "./ui/Pages";
import { SessionPage } from "./ui/Session";
import { Sidebar, TitleBar, type Nav } from "./ui/Shell";
import { useNow } from "./ui/util";

type View = { kind: "main"; nav: Nav | null } | { kind: "session"; host: string };

export default function App() {
  const snap = useStore();
  const now = useNow(5000);
  const [view, setView] = useState<View>({ kind: "main", nav: null });
  const [device, setDevice] = useState("this device");

  useEffect(() => {
    void store.start();
    api
      .status()
      .then((s) => setDevice(s.device.startsWith("eid:demo") ? "home-laptop" : s.device.slice(0, 12)))
      .catch(() => {});
  }, []);

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
  else if (active === "approvals") page = <ApprovalsPage snap={snap} onOpen={(h) => setView({ kind: "session", host: h })} />;
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
  else if (computer) page = <ComputerPage c={computer} onWatch={() => setView({ kind: "session", host: computer.host.name })} />;
  else page = <EmptyHome onPair={() => nav("pair")} />;

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
