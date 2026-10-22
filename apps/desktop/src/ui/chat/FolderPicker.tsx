// Choosing the folder an agent works in, and showing it. The folder is on the host, so browsing goes
// through the host's `files_list` tool (admin level, allowed without asking by default); typing a path
// always works, also where listing is refused. The shape follows Codex's project picker (a chip with
// the project's name that opens a searchable list of recent projects) and Orca's "folder on host"
// field (a path on the remote machine, typed or picked): recent folders first, then browsing.

import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import {
  childFolder,
  folderCrumbs,
  folderName,
  knownHome,
  listedFolders,
  looksLikePath,
  matchFolders,
  parentFolder,
  rememberHome,
  tildeFolder,
  trimFolder,
} from "../../lib/chat";
import { t } from "../../lib/i18n";
import { Icon } from "../Icon";
import { cx } from "../util";

/** One page of a `files_list` result (what the picker reads of it). */
export interface Listing {
  path: string;
  next_offset?: number | null;
  entries: { name: string; kind: string }[];
}

/** A folder outline (Lucide's `folder`, drawn here so the shared icon set stays as it is). */
export function FolderGlyph({ size = 14 }: { size?: number }) {
  return (
    <svg className="nt-folder-glyph" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
    </svg>
  );
}

/** A folder as its name and, quieter, where it is (the home shortened to `~`); the full path on hover. */
export function FolderLabel({ host, path, className }: { host: string; path: string; className?: string }) {
  const home = knownHome(host);
  const above = parentFolder(path);
  return (
    <span className={cx("nt-folder-lbl", className)} title={path}>
      <FolderGlyph size={13} />
      <span className="fn">{folderName(path)}</span>
      {above && <span className="fp mono"><bdi>{tildeFolder(above, home)}</bdi></span>}
    </span>
  );
}

type Row = { key: string; kind: "recent" | "open" | "browse" | "child" | "up"; path: string; label?: string };

/**
 * The folder field of the start form: a chip that opens recent folders, a search-or-path box and a
 * browser of the host's folders. `browse` lists a folder on the host (files_list).
 */
export function FolderPicker({
  host,
  value,
  folders,
  browse,
  onChange,
}: {
  host: string;
  value: string;
  /** Recent folders and those of the host's open sessions, most useful first. */
  folders: string[];
  browse: (path: string) => Promise<Listing>;
  onChange: (path: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  /** The folder being browsed (null: the recent list). */
  const [dir, setDir] = useState<string | null>(null);
  const [listing, setListing] = useState<Listing | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [active, setActive] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const round = useRef(0);

  const close = () => {
    setOpen(false);
    setQuery("");
    setDir(null);
    setError(null);
  };
  const choose = (path: string) => {
    onChange(trimFolder(path));
    close();
  };
  const go = (path: string) => {
    const target = trimFolder(path);
    const n = ++round.current;
    setDir(target);
    setQuery("");
    setActive(0);
    setError(null);
    setLoading(true);
    browse(target).then(
      (l) => {
        if (n !== round.current) return;
        if (target === "~") rememberHome(host, l.path);
        setListing(l);
        setDir(l.path);
        setLoading(false);
      },
      (e) => {
        if (n !== round.current) return;
        setListing(null);
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      },
    );
  };
  const show = () => {
    setOpen(true);
    setQuery("");
    setActive(0);
    if (!folders.length) go(value || "~");
    else setDir(null);
  };

  // A click anywhere else closes the list.
  useEffect(() => {
    if (!open) return;
    const down = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) close();
    };
    document.addEventListener("mousedown", down);
    input.current?.focus();
    return () => document.removeEventListener("mousedown", down);
  }, [open]);

  const typedPath = looksLikePath(query) ? trimFolder(query) : null;
  const rows: Row[] = useMemo(() => {
    if (dir === null) {
      const out: Row[] = [];
      if (typedPath) out.push({ key: "open", kind: "open", path: typedPath });
      for (const f of matchFolders(folders, typedPath ? "" : query)) out.push({ key: `r:${f}`, kind: "recent", path: f });
      out.push({ key: "browse", kind: "browse", path: value || "~" });
      return out;
    }
    if (typedPath) return [{ key: "open", kind: "open", path: typedPath }];
    const out: Row[] = [];
    const up = parentFolder(dir);
    if (up && !query) out.push({ key: "up", kind: "up", path: up });
    for (const name of listedFolders(listing?.entries ?? [], query)) out.push({ key: `c:${name}`, kind: "child", path: childFolder(dir, name), label: name });
    return out;
  }, [dir, query, typedPath, folders, listing, value]);
  const at = Math.min(active, Math.max(rows.length - 1, 0));

  const act = (row: Row) => {
    if (row.kind === "recent") choose(row.path);
    else go(row.path);
  };
  const key = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => (rows.length ? (Math.min(a, rows.length - 1) + (e.key === "ArrowDown" ? 1 : rows.length - 1)) % rows.length : 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      // ⌘/Ctrl+Enter takes the folder being browsed; Enter opens the highlighted row.
      if ((e.metaKey || e.ctrlKey) && dir && !error) choose(dir);
      else if (rows[at]) act(rows[at]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      if (dir !== null && folders.length) {
        setDir(null);
        setQuery("");
        setError(null);
      } else close();
    } else if (e.key === "Backspace" && !query && dir) {
      const up = parentFolder(dir);
      if (up) {
        e.preventDefault();
        go(up);
      }
    }
  };
  const more = !!listing?.next_offset && listing.entries.at(-1)?.kind === "dir";
  const home = knownHome(host);

  return (
    <div className={cx("nt-folder", open && "open")} ref={box}>
      <button type="button" className="nt-folder-btn" aria-haspopup="listbox" aria-expanded={open} onClick={() => (open ? close() : show())}>
        {value ? <FolderLabel host={host} path={value} /> : (
          <span className="nt-folder-lbl none">
            <FolderGlyph size={13} />
            <span className="fn">{t("hc_folder_choose")}</span>
          </span>
        )}
        <Icon name="chevD" size={12} />
      </button>
      {open && (
        <div className="nt-folder-pop" role="dialog" aria-label={t("hc_folder_choose")}>
          <div className="nt-folder-q">
            <Icon name="search" size={13} />
            <input
              ref={input}
              id="ag-folder-q"
              className="mono"
              value={query}
              placeholder={dir === null ? t("hc_folder_search_ph") : t("hc_folder_filter_ph")}
              onChange={(e) => {
                setQuery(e.target.value);
                setActive(0);
              }}
              onKeyDown={key}
              spellCheck={false}
              autoComplete="off"
            />
          </div>
          {dir !== null && (
            <div className="nt-folder-crumbs">
              {folders.length > 0 && (
                <button type="button" className="back" aria-label={t("hc_folder_recent")} title={t("hc_folder_recent")} onClick={() => { setDir(null); setQuery(""); setError(null); }}>
                  <Icon name="back" size={12} />
                </button>
              )}
              {folderCrumbs(dir).map((c, i, all) => (
                <span key={c.path} className="crumb">
                  <button type="button" className={cx(i === all.length - 1 && "cur")} onClick={() => go(c.path)} title={c.path}>
                    {c.name}
                  </button>
                  {i < all.length - 1 && <Icon name="chevR" size={10} />}
                </span>
              ))}
            </div>
          )}
          <div className="nt-folder-list" role="listbox">
            {dir === null && folders.length > 0 && !typedPath && <div className="sec">{t("hc_folder_recent")}</div>}
            {loading && <div className="nt-folder-note">{t("hc_folder_loading")}</div>}
            {!loading && error && <div className="nt-folder-note err">{t("hc_folder_cant_list", { error })}</div>}
            {!loading && !error && dir !== null && !typedPath && rows.every((r) => r.kind === "up") && (
              <div className="nt-folder-note">{query ? t("hc_folder_no_match") : t("hc_folder_empty")}</div>
            )}
            {!loading &&
              rows.map((r, i) => (
                <button
                  key={r.key}
                  type="button"
                  role="option"
                  aria-selected={i === at}
                  className={cx("row", r.kind, i === at && "on", r.kind === "recent" && value && r.path === trimFolder(value) && "sel")}
                  onMouseEnter={() => setActive(i)}
                  onClick={() => act(r)}
                >
                  {r.kind === "up" ? <Icon name="back" size={13} /> : <FolderGlyph size={13} />}
                  {r.kind === "recent" ? (
                    <>
                      <span className="fn">{folderName(r.path)}</span>
                      <span className="fp mono"><bdi>{tildeFolder(parentFolder(r.path) ?? "", home)}</bdi></span>
                    </>
                  ) : r.kind === "open" ? (
                    <span className="fn mono">{t("hc_folder_open", { path: r.path })}</span>
                  ) : r.kind === "browse" ? (
                    <span className="fn">{t("hc_folder_browse")}</span>
                  ) : r.kind === "up" ? (
                    <span className="fn">..</span>
                  ) : (
                    <span className="fn">{r.label}</span>
                  )}
                  {r.kind === "child" && <Icon name="chevR" size={12} />}
                  {r.kind === "recent" && value && r.path === trimFolder(value) && <Icon name="check" size={12} />}
                </button>
              ))}
            {!loading && more && <div className="nt-folder-note">{t("hc_folder_more")}</div>}
          </div>
          {dir !== null && (
            <div className="nt-folder-ft">
              <span className="fp mono" title={dir}><bdi>{tildeFolder(dir, home)}</bdi></span>
              {error ? (
                <button type="button" className="btn" onClick={() => choose(dir)}>
                  {t("hc_folder_use_anyway")}
                </button>
              ) : (
                <button type="button" className="btn primary" disabled={loading} onClick={() => choose(dir)}>
                  {t("hc_folder_use", { name: folderName(dir) })}
                </button>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
