import { useEffect, useState } from "react";
import { commands } from "./bindings";

type Host = { name: string; level_at_pairing?: string | null; path?: string | null };
type Status = { device: string; version: string; hosts: Host[] };

/** Bring-up screen for the bridge to the controller agent; the designed app replaces it. */
export default function App() {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    document.documentElement.dataset.theme = matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
    Promise.all([commands.agentStatus(), commands.listHosts()]).then(([status, hosts]) => {
      if (status.status === "error") return setError(status.error);
      if (hosts.status === "error") return setError(hosts.error);
      const s = status.data as Omit<Status, "hosts">;
      setStatus({ ...s, hosts: hosts.data as Host[] });
    });
  }, []);

  return (
    <main className="flex h-full flex-col gap-4 p-8">
      <h1 className="text-[length:var(--fs-title-1)] leading-[var(--lh-title-1)] font-semibold">ai-remote</h1>
      {error && <p className="text-danger">{error}</p>}
      {!status && !error && <p className="text-text-2">Starting the controller agent…</p>}
      {status && (
        <section className="flex flex-col gap-2 rounded-[var(--r-panel)] bg-card p-4 shadow-[var(--shadow-card)]">
          <p className="text-text-2">
            This device <span className="font-mono text-text-1">{status.device}</span>
          </p>
          {status.hosts.length === 0 ? (
            <p className="text-text-2">No computers yet. Pair one with a code from that computer.</p>
          ) : (
            <ul className="flex flex-col gap-1">
              {status.hosts.map((h) => (
                <li key={h.name} className="flex justify-between">
                  <span>{h.name}</span>
                  <span className="text-text-3">{[h.level_at_pairing, h.path].filter(Boolean).join(", ")}</span>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}
    </main>
  );
}
