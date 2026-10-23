// The computer list when it is empty, after the first run.

import { t } from "../../lib/i18n";
import { CmdLine } from "../CmdLine";
import { SeatMark } from "../glyphs";
import { Icon } from "../Icon";

/**
 * The computer list when it is empty (after the first run): the one command, the one action, and the
 * stage where the first computer will appear, framed in the idle colour with a key to the frame colours.
 */
export function EmptyPage({ onPair }: { onPair: () => void }) {
  const key = (c: string, k: "em_k_agent" | "em_k_you" | "em_k_idle") => (
    <span className="em-k">
      <i style={{ boxShadow: `inset 0 0 0 2px ${c}` }} />
      {t(k)}
    </span>
  );
  return (
    <div className="ipage em-page">
      <div className="cp-hl" style={{ marginTop: 0 }}>
        <div className="dsp">{t("em_title")}</div>
      </div>
      <div className="lede">{t("em_lede")}</div>
      <div className="em-grid">
        <div className="em-steps">
          <div className="em-st">
            <span className="n tnum">1</span>
            <div className="b">
              <div className="t">{t("em_s1")}</div>
              <CmdLine cmd="ai-remote host" />
              <div className="d">{t("em_s1_d")}</div>
            </div>
          </div>
          <div className="em-st">
            <span className="n tnum">2</span>
            <div className="b">
              <div className="t">{t("em_s2")}</div>
              <div>
                <button className="btn primary lg" onClick={onPair}>
                  <Icon name="add" size={14} />
                  {t("pair")}
                </button>
              </div>
              <div className="d">{t("em_s2_d")}</div>
            </div>
          </div>
        </div>
        <div className="em-stage stagebox stage">
          <div className="em-well">
            <div className="em-in">
              <SeatMark state="idle" h={44} />
              <div className="t">{t("em_stage")}</div>
              <div className="d">{t("em_stage_d")}</div>
              <div className="em-keys">
                {key("#6F9BFF", "em_k_agent")}
                {key("#F3B53F", "em_k_you")}
                {key("#8A919C", "em_k_idle")}
              </div>
            </div>
          </div>
          <div className="em-line">
            <span className="hl" />
            <SeatMark state="idle" h={16} />
          </div>
        </div>
      </div>
      <div style={{ flex: 1 }} />
      <div className="footnote">
        <Icon name="qr" size={12} />
        <span>{t("em_foot")}</span>
      </div>
    </div>
  );
}
