// Which conversation the rows belong to, for opening file previews (ui/chat/Preview.tsx). Its own
// module so Markdown.tsx can read it without importing the dialog (which imports Markdown.tsx).

import { createContext, useContext } from "react";

/** Previews of the conversation's files (`path` as the session names it). */
export interface PreviewApi {
  /** Opens the preview dialog. */
  open: (path: string) => void;
  /** The file's bytes from the host, fetched once per conversation unless `fresh`. */
  load: (path: string, fresh?: boolean) => Promise<Uint8Array>;
  /** Opens an HTML file with its scripts running, in a window of its own (fetched again). */
  runScripts: (path: string) => Promise<void>;
}

export const PreviewContext = createContext<PreviewApi | null>(null);

/** How the panel previews files, or null when it cannot (outside a conversation). */
export function usePreview(): PreviewApi | null {
  return useContext(PreviewContext);
}
