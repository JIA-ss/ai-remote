// First run: the product in one sentence, the two ways in, and the turn line told as a story.

import { t } from "../../lib/i18n";
import { CmdLine } from "../CmdLine";
import { Glyph, Pointer, SeatMark, Wordmark } from "../glyphs";
import { Icon } from "../Icon";
import { TurnLine, type TurnSpec } from "../TurnLine";
import { WelcomeFigure } from "../WelcomeFigure";
import { cx } from "../util";

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
}: {
  onPair: () => void;
  /** Settings › This device with the enable dialog open (the administrator confirms once). */
  onSetupHost: () => void;
}) {
  const W = 616;
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
    <div className="spread" style={{ inset: 0, overflow: "auto" }}>
      <div className="lcol">
        <div>
          <Wordmark h={24} />
        </div>
        <div className="hero">{phrases(t("fr_title"))}</div>
        <div className="lede">{t("fr_sub")}</div>
        <div className="doors">
          <div className="door">
            <div className="ic">
              <SeatMark state="you" h={22} />
            </div>
            <div>
              <div className="ttl">{t("fr_ctl")}</div>
              <div className="d">{t("fr_ctl_d")}</div>
              <div className="a">
                <button className="btn primary lg" onClick={onPair}>
                  <Icon name="add" size={14} />
                  {t("pair")}
                </button>
              </div>
              <div className="note">{t("fr_ctl_note")}</div>
            </div>
          </div>
          <div className="door">
            <div className="ic">
              <SeatMark state="agent" h={22} />
            </div>
            <div>
              <div className="ttl">{t("fr_host")}</div>
              <div className="d">{t("fr_host_d")}</div>
              <div className="a">
                <button className="btn lg" onClick={onSetupHost}>
                  {t("setup_this")}
                </button>
              </div>
              <div className="note">{t("fr_host_note")}</div>
            </div>
          </div>
        </div>
      </div>
      <div>
        <div className="fig">
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
          <div className="agentfoot" style={{ marginTop: 30 }}>
            <div style={{ paddingTop: 2, color: "var(--text-2)" }}>
              <Icon name="terminal" size={20} />
            </div>
            <div>
              <div className="strong">{t("fr_agent")}</div>
              <div className="body c2" style={{ marginTop: 2 }}>
                {t("fr_agent_d")}
              </div>
              <CmdLine cmd="claude mcp add ai-remote -- ai-remote mcp" />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
