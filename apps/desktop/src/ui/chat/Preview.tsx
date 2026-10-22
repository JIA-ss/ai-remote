// Previews of the HTML, Markdown and image files a coding agent made or changed. Under each finished
// turn the files it wrote or named show in the conversation (a thumbnail, a small page, the start
// of the Markdown); a click, the eye button on a tool line or edit card, or a file named in a reply
// opens the file in a dialog over the window. The bytes come from the host (`chat_file`: only files
// the session is about, at most 16 MiB) into memory, once per conversation.
//
// Follows Orca's native chat (stablyai/orca, MIT License, Copyright (c) 2026 Lovecast Inc.): its
// visual frames render agent HTML inline in a sandboxed `srcdoc` frame with a network-blocking
// policy, images show as thumbnails that open in a dialog. Unlike Orca the frame runs no scripts at all: in this app's webview any
// frame could reach the native message handler behind Tauri's IPC, so agent code never runs here
// (the app's own policy refuses inline scripts as well). Static HTML and CSS still render.

import { Eye, FileCode2, FileText, Image as ImageIcon, Play, RotateCw, X } from "lucide-react";
import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { api } from "../../lib/api";
import { baseName } from "../../lib/chat";
import { t } from "../../lib/i18n";
import { decodeText, imageMime, previewDocument, previewKind, sizeLabel, turnFiles, type PreviewKind } from "../../lib/preview";
import type { Turn } from "../../lib/chat";
import { cx } from "../util";
import { CodeBlock, CopyButton, Markdown } from "./Markdown";
import { PreviewContext, usePreview, type PreviewApi } from "./preview-context";

export { usePreview };

/** Lets the rows of one conversation open previews of its files. */
export function PreviewProvider({ host, session, children }: { host: string; session: string; children: ReactNode }) {
  const [path, setPath] = useState<string | null>(null);
  const cache = useRef(new Map<string, Promise<Uint8Array>>());
  const value = useMemo<PreviewApi>(() => ({
    open: (p) => setPath(p),
    load: (p, fresh) => {
      let got = fresh ? undefined : cache.current.get(p);
      if (!got) {
        got = api.previewFile(host, session, p);
        cache.current.set(p, got);
        // A failure is not kept: the next look asks again.
        got.catch(() => cache.current.get(p) === got && cache.current.delete(p));
      }
      return got;
    },
    runScripts: (p) => api.openPreviewWindow(host, session, p),
  }), [host, session]);
  return (
    <PreviewContext.Provider value={value}>
      {children}
      {path && <PreviewDialog load={value.load} run={value.runScripts} path={path} onClose={() => setPath(null)} />}
    </PreviewContext.Provider>
  );
}

/** An eye button that previews `path`, when the conversation can and the file is a kind it shows. */
export function PreviewButton({ path }: { path: string }) {
  const preview = usePreview();
  if (!preview || !previewKind(path)) return null;
  return (
    <button
      type="button"
      className="nt-ib nt-pv-btn"
      aria-label={t("hc_preview_open", { name: baseName(path) })}
      title={t("hc_preview_open", { name: baseName(path) })}
      onClick={(e) => {
        e.stopPropagation();
        preview.open(path);
      }}
    >
      <Eye size={13} />
    </button>
  );
}

const KIND_ICON: Record<PreviewKind, typeof FileText> = { html: FileCode2, markdown: FileText, image: ImageIcon };

type Loaded = { state: "loading" } | { state: "error"; error: string } | { state: "ready"; bytes: Uint8Array };

function PreviewDialog({ load, run, path, onClose }: { load: PreviewApi["load"]; run: PreviewApi["runScripts"]; path: string; onClose: () => void }) {
  const [running, setRunning] = useState<"idle" | "opening" | { error: string }>("idle");
  const kind = previewKind(path) as PreviewKind;
  const [round, setRound] = useState(0);
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  const [source, setSource] = useState(false);
  const [actual, setActual] = useState(false);
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    let live = true;
    setLoaded({ state: "loading" });
    load(path, round > 0).then(
      (bytes) => live && setLoaded({ state: "ready", bytes }),
      (e) => live && setLoaded({ state: "error", error: String((e as Error)?.message ?? e) }),
    );
    return () => {
      live = false;
    };
  }, [load, path, round]);

  // Esc closes; focus goes into the dialog and back to where it was.
  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      before?.focus?.();
    };
  }, [onClose]);

  const text = useMemo(() => (loaded.state === "ready" && kind !== "image" ? decodeText(loaded.bytes) : null), [loaded, kind]);
  const imageUrl = useMemo(() => (loaded.state === "ready" && kind === "image" ? URL.createObjectURL(new Blob([loaded.bytes as BlobPart], { type: imageMime(path) })) : null), [loaded, kind, path]);
  useEffect(() => () => {
    if (imageUrl) URL.revokeObjectURL(imageUrl);
  }, [imageUrl]);

  const Ic = KIND_ICON[kind];
  let body: ReactNode;
  if (loaded.state === "loading") body = <div className="nt-pv-msg">{t("hc_preview_loading")}</div>;
  else if (loaded.state === "error") body = <div className="nt-pv-msg err">{loaded.error}</div>;
  else if (kind === "image" && imageUrl) {
    body = (
      <div className={cx("nt-pv-img", actual && "actual")}>
        <img src={imageUrl} alt={baseName(path)} onClick={() => setActual((v) => !v)} title={actual ? t("hc_preview_fit") : t("hc_preview_actual")} />
      </div>
    );
  } else if (text && source) {
    body = <CodeBlock code={text.text} lang={kind === "html" ? "html" : "md"} className="nt-pv-src" />;
  } else if (text && kind === "markdown") {
    body = (
      <div className="nt-pv-md">
        <Markdown text={text.text} />
      </div>
    );
  } else if (text) {
    // sandbox="": no scripts, an opaque origin, no forms, popups or top navigation.
    body = <iframe className="nt-pv-frame" title={baseName(path)} sandbox="" referrerPolicy="no-referrer" srcDoc={previewDocument(text.text)} />;
  }

  return createPortal(
    <div className="nt-chat nt-pv">
      <div className="smoke nt-pv-smoke" onClick={onClose} />
      <div className="nt-pv-dlg" role="dialog" aria-modal="true" aria-label={t("hc_preview_title", { name: baseName(path) })}>
        <div className="nt-pv-hd">
          <Ic size={15} className="ic" aria-hidden />
          <div className="nm">
            <span className="b">{baseName(path)}</span>
            <span className="p mono" title={path}>
              {/* Cut from the left (the file's own name is beside it), in the path's own direction. */}
              <bdi dir="ltr">{path}</bdi>
            </span>
          </div>
          {loaded.state === "ready" && <span className="sz tnum">{sizeLabel(loaded.bytes.length)}</span>}
          {kind !== "image" && (
            <div className="seg" role="group" aria-label={t("hc_preview_view")}>
              <button type="button" className={cx(!source && "on")} aria-pressed={!source} onClick={() => setSource(false)}>
                {t("hc_preview_rendered")}
              </button>
              <button type="button" className={cx(source && "on")} aria-pressed={source} onClick={() => setSource(true)}>
                {t("hc_preview_source")}
              </button>
            </div>
          )}
          {kind === "html" && (
            <button type="button" className="btn sm nt-pv-run" disabled={running === "opening"} title={t("hc_preview_run_tip")}
              onClick={() => {
                setRunning("opening");
                run(path).then(() => setRunning("idle"), (e) => setRunning({ error: String((e as Error)?.message ?? e) }));
              }}>
              <Play size={12} aria-hidden />
              {t("hc_preview_run")}
            </button>
          )}
          <CopyButton text={path} label={t("hc_preview_copy_path")} />
          <button type="button" className="nt-ib" aria-label={t("hc_preview_reload")} title={t("hc_preview_reload")} onClick={() => setRound((n) => n + 1)}>
            <RotateCw size={13} />
          </button>
          <button ref={closeRef} type="button" className="nt-ib" aria-label={t("hc_preview_close")} title={t("hc_preview_close")} onClick={onClose}>
            <X size={14} />
          </button>
        </div>
        {text?.cut && <div className="nt-pv-note">{t("hc_preview_cut")}</div>}
        {typeof running === "object" && <div className="nt-pv-note err">{running.error}</div>}
        {kind === "html" && !source && loaded.state === "ready" && <div className="nt-pv-note">{t("hc_preview_static")}</div>}
        <div className="nt-pv-bd">{body}</div>
      </div>
    </div>,
    document.body,
  );
}

/** The file's bytes once it is near the screen (at once while the page is hidden, where nothing
 * intersects); `el` is the element whose visibility counts. */
function useNearLoad(path: string, el: React.RefObject<HTMLElement | null>) {
  const preview = usePreview();
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  useEffect(() => {
    if (!preview) return;
    let live = true;
    const go = () =>
      preview.load(path).then(
        (bytes) => live && setLoaded({ state: "ready", bytes }),
        (e) => live && setLoaded({ state: "error", error: String((e as Error)?.message ?? e) }),
      );
    if (typeof IntersectionObserver === "undefined" || document.visibilityState === "hidden" || !el.current) {
      void go();
      return () => {
        live = false;
      };
    }
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        io.disconnect();
        void go();
      }
    }, { rootMargin: "300px" });
    io.observe(el.current);
    return () => {
      live = false;
      io.disconnect();
    };
  }, [preview, path, el]);
  return loaded;
}

/** One file shown in the conversation: a thumbnail, a small page or the start of the Markdown. */
function InlineFile({ path }: { path: string }) {
  const preview = usePreview();
  const kind = previewKind(path) as PreviewKind;
  const ref = useRef<HTMLButtonElement>(null);
  const loaded = useNearLoad(path, ref);
  const text = useMemo(() => (loaded.state === "ready" && kind !== "image" ? decodeText(loaded.bytes, 256 * 1024) : null), [loaded, kind]);
  const imageUrl = useMemo(() => (loaded.state === "ready" && kind === "image" ? URL.createObjectURL(new Blob([loaded.bytes as BlobPart], { type: imageMime(path) })) : null), [loaded, kind, path]);
  useEffect(() => () => {
    if (imageUrl) URL.revokeObjectURL(imageUrl);
  }, [imageUrl]);
  const Ic = KIND_ICON[kind];
  const name = baseName(path);
  let body: ReactNode = <span className="wait">{t("hc_preview_loading")}</span>;
  if (loaded.state === "error") body = <span className="err">{loaded.error}</span>;
  else if (imageUrl) body = <img src={imageUrl} alt={name} />;
  else if (text && kind === "markdown") body = <Markdown text={text.text} />;
  // Not interactive in the conversation (a click opens the dialog); no scripts, nothing fetched.
  else if (text) body = <iframe title={name} sandbox="" referrerPolicy="no-referrer" tabIndex={-1} srcDoc={previewDocument(text.text)} />;
  return (
    <button ref={ref} type="button" className={cx("nt-pvi-f", kind, loaded.state)} title={t("hc_preview_open", { name })}
      aria-label={t("hc_preview_open", { name })} onClick={() => preview?.open(path)}>
      <span className="cap">
        <Ic size={13} aria-hidden />
        <span className="nm">{name}</span>
        <Eye size={13} className="eye" aria-hidden />
      </span>
      <span className="body">{body}</span>
    </button>
  );
}

/** Under a finished turn: the previewable files it wrote or named, shown in place (Orca's inline
 * visuals and image thumbnails). */
export const TurnFiles = memo(function TurnFiles({ turn }: { turn: Turn }) {
  const preview = usePreview();
  const files = useMemo(() => (turn.status === "running" ? [] : turnFiles(turn.blocks)), [turn]);
  if (!preview || !files.length) return null;
  const images = files.filter((f) => previewKind(f) === "image");
  const pages = files.filter((f) => previewKind(f) !== "image");
  return (
    <div className="nt-pvi">
      {pages.map((f) => <InlineFile key={f} path={f} />)}
      {images.length > 0 && <div className="imgs">{images.map((f) => <InlineFile key={f} path={f} />)}</div>}
    </div>
  );
});
