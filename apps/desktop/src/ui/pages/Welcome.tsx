// First run: the product in one sentence, the two ways in (one grouped card, no command lines: the
// app itself pairs and sets up), and the turn line told as a story beside it. The figure is drawn at
// its own width and scaled down to the column, so it never runs past the window.

import { t } from "../../lib/i18n";
import { Glyph, Pointer, SeatMark, Wordmark } from "../glyphs";
import { Icon } from "../Icon";
import { TurnLine, type TurnSpec } from "../TurnLine";
import { WelcomeFigure } from "../WelcomeFigure";
import { cx, useSize } from "../util";

/** Display text with phrase marks: `|` allows a break there (Chinese is set keep-all), a newline forces one. */
function phrases(s: string): React.ReactNode {
  return s.split("\n").map((line, i) => (
    <span key={i}>
      {i > 0 && <br />}
      {line.split("|").map((p, j) => (
        <span key={j}>
          {j > 0 && <wbr />}
          {p}
        </span>
      ))}
    </span>
  ));
}

/** First run: the product in one sentence, the two ways in, and the turn line told as a story. */
export function WelcomePage({
  onPair,
  onSetupHost,
  onAgents,
}: {
  onPair: () => void;
  /** Settings › This device with the enable dialog open (the administrator confirms once). */
  onSetupHost: () => void;
  /** Settings › Agent access (how to connect Claude Code, Codex, ...); a text hint without it. */
  onAgents?: () => void;
}) {
  const W = 616;
  const [pageRef, page] = useSize<HTMLDivElement>();
  const [figRef, fig] = useSize<HTMLDivElement>();
  // The figure keeps its drawn proportions and is scaled to its column. Side by side while that
  // column keeps it at 72 % or more; below that it goes under the two doors at up to full size.
  const GAP = 48;
  const total = Math.min(page.w || 1180, 1180);
  const sideW = ((total - GAP) * 1.15) / 2.15;
  const side = sideW / W >= 0.72;
  const scale = Math.min(1, (side ? sideW : total) / W);
  const spec: TurnSpec = {
    // A finished story: the wait was answered, so it is the thin stroke with its decision dot, not dashes.
    segs: [
      ["agent", 0, 26],
      ["waited", 26, 46],
      ["agent", 46, 68],
      ["you", 68, 88],
      ["agent", 88, 100],
    ],
    ticks: [
      ["", 6],
      ["", 13],
      ["", 20],
      ["dec", 46],
      ["", 53],
      ["", 61],
      ["", 95],
    ],
    head: "agent",
  };
  const L = W - 18 * 0.62;
  const X = (p: number) => (p / 100) * L;
  const stops: Array<[number, "agent" | "you", string, "up" | "dn"]> = [
    [0, "agent", "fr_stop1", "up"],
    [26, "you", "fr_stop2", "dn"],
    [46, "you", "fr_stop3", "up"],
    [68, "you", "fr_stop4", "dn"],
    [88, "agent", "fr_stop5", "up"],
  ];
  return (
    <div
      className={cx("spread", "gl-welcome", !side && "stacked")}
      ref={pageRef}
      style={{ gridTemplateColumns: side ? `minmax(0, ${total - GAP - sideW}px) ${sideW}px` : "minmax(0, 1fr)" }}
    >
      <div className="lcol">
        <div>
          <Wordmark h={24} />
        </div>
        <h1 className="hero">{phrases(t("fr_title"))}</h1>
        <div className="lede">{t("fr_sub")}</div>
        <div className="doors">
          <div className="door">
            <div className="ic">
              <SeatMark state="you" h={22} />
            </div>
            <div>
              <div className="ttl">{t("fr_ctl")}</div>
              <div className="d">{t("fr_ctl_d")}</div>
              <div className="note">{t("pg_ctl_note")}</div>
              <div className="a">
                <button className="btn primary" onClick={onPair}>
                  <Icon name="add" size={14} />
                  {t("pair")}
                </button>
              </div>
            </div>
          </div>
          <div className="door">
            <div className="ic">
              <SeatMark state="agent" h={22} />
            </div>
            <div>
              <div className="ttl">{t("fr_host")}</div>
              <div className="d">{t("pg_host_d")}</div>
              <div className="note">{t("fr_host_note")}</div>
              <div className="a">
                <button className="btn" onClick={onSetupHost}>
                  {t("setup_this")}…
                </button>
              </div>
            </div>
          </div>
        </div>
        <div className="gl-wl-agents">
          <Icon name="puzzle" size={14} />
          {onAgents ? (
            <p>
              {t("pg_agents_lead")}{" "}
              <button className="nt-link" onClick={onAgents}>
                {t("pg_agents_go")}
              </button>
            </p>
          ) : (
            <p>{t("pg_agents_note")}</p>
          )}
        </div>
      </div>
      <div className="rcol">
        <div className="gl-fig-box" style={{ height: fig.h ? Math.ceil(fig.h * scale) : undefined }}>
          <div className="fig" ref={figRef} style={{ width: W, transform: scale < 1 ? `scale(${scale})` : undefined }}>
            <div className="stage-wrap">
              <div className="stagebox stage" style={{ height: 452 }}>
                <div className="capsule compact" style={{ top: 20, left: "50%", transform: "translateX(-50%)" }}>
                  <span className="disc">
                    <SeatMark state="agent" h={20} />
                  </span>
                  <div className="who">
                    <span className="l1">{t("cap_agent_1", { agent: "Claude Code" })}</span>
                    <span className="l2">{t("cap_agent_2", { device: "home-laptop" })}</span>
                  </div>
                  <span className="pill take">{t("take_over")}</span>
                  <span className="pill">
                    <Glyph name="paused" size={16} />
                    {t("pause_agent")}
                  </span>
                </div>
                <div className="clip">
                  <WelcomeFigure />
                </div>
                <div className="clipframe" />
              </div>
            </div>
            <div className="stops">
              {stops.map(([p, who, key, pos], i) => {
                const last = i === stops.length - 1;
                return (
                  <div key={key} className={cx("lbl", pos)} style={last ? { right: 0, textAlign: "right" } : { left: Math.max(0, X(p) - 1) }}>
                    <b style={last ? { justifyContent: "flex-end" } : undefined}>
                      <Pointer who={who} h={11} ui />
                      {t(key as Parameters<typeof t>[0])}
                    </b>
                    {t(`${key}_d` as Parameters<typeof t>[0])}
                  </div>
                );
              })}
              <div style={{ position: "absolute", left: 0, top: 63 }}>
                <TurnLine spec={spec} w={W} />
              </div>
            </div>
            <div className="figcap">{t("fr_fig")}</div>
          </div>
        </div>
      </div>
    </div>
  );
}
