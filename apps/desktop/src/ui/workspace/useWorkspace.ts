// The workspace's store (lib/workspace.ts) wired to the app: host tools through the controller, this
// page's storage, which computers there are and whether the workspace is in view.

import { useEffect, useState, useSyncExternalStore } from "react";
import { api } from "../../lib/api";
import type { Computer } from "../../lib/store";
import { WorkspaceStore, type WsSnapshot } from "../../lib/workspace";

/** A host tool's JSON result, or its error text (as the chat panel reads them). */
export async function call<T = unknown>(host: string, tool: string, args: Record<string, unknown> = {}): Promise<T> {
  const r = await api.callTool(host, tool, args, true);
  const text = r.content.find((c) => c.type === "text")?.text ?? "";
  if (r.isError) throw new Error(text || `${tool} failed`);
  return (r.structuredContent ?? JSON.parse(text)) as T;
}

export const workspace = new WorkspaceStore({ call, storage: typeof localStorage === "undefined" ? null : localStorage });

/** The device id of the host running on this computer, if one does (its sessions are "this computer"). */
let localDevice: Promise<string | null> | null = null;
function localHostDevice(): Promise<string | null> {
  localDevice ??= api.localHost(false).then((l) => l.host?.device ?? null, () => null);
  return localDevice;
}

/** Keeps the store told which computers there are, and polls fast while `visible`. */
export function useWorkspace(computers: Computer[], visible: boolean): WsSnapshot {
  const snap = useSyncExternalStore(workspace.subscribe, workspace.get);
  const [local, setLocal] = useState<string | null>(null);
  useEffect(() => {
    void localHostDevice().then(setLocal);
  }, []);
  useEffect(() => {
    workspace.setComputers(
      computers.map((c) => ({ name: c.host.name, online: c.online, local: !!local && !!c.host.device && c.host.device === local })),
    );
  }, [computers, local]);
  useEffect(() => {
    const sync = () => workspace.setActive(visible && document.visibilityState === "visible");
    sync();
    document.addEventListener("visibilitychange", sync);
    return () => {
      document.removeEventListener("visibilitychange", sync);
      workspace.setActive(false);
    };
  }, [visible]);
  return snap;
}
