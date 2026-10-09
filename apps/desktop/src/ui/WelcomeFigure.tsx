// The first run's figure: a report window at full size, cropped to its toolbar, with the agent's
// intent marked on Export PDF before the click (dashed outline, ref chip, present-tense pill).
// Drawn at the clip's own size (568 x 336) so the marks sit exactly on the button.

import { t } from "../lib/i18n";
import { Pointer } from "./glyphs";

const W = 568;
const H = 336;
// The Export PDF button in the picture.
const btn = { x: 186, y: 64, w: 118, h: 34 };

function Toolbar({ x, label, icon }: { x: number; label: string; icon: string }) {
  return (
    <g transform={`translate(${x} 72)`}>
      <path d={icon} fill="none" stroke="#1F2328" strokeWidth={1.3} strokeLinejoin="round" />
      <text x={22} y={13} fontFamily="Segoe UI, system-ui, sans-serif" fontSize={14} fill="#1F2328">
        {label}
      </text>
    </g>
  );
}

export function WelcomeFigure() {
  const bars = [62, 68, 72, 96, 108, 126];
  return (
    <div style={{ position: "absolute", inset: 0 }}>
      <svg viewBox={`0 0 ${W} ${H}`} width={W} height={H} style={{ position: "absolute", left: 0, top: 0 }} aria-hidden="true">
        <rect width={W} height={H} fill="#9BB0BF" />
        <rect x={8} y={14} width={W + 40} height={H} rx={8} fill="#FFFFFF" />
        <rect x={8} y={14} width={W + 40} height={40} rx={8} fill="#F3F4F6" />
        <rect x={8} y={46} width={W + 40} height={8} fill="#F3F4F6" />
        <rect x={22} y={26} width={14} height={16} rx={2.5} fill="none" stroke="#2D7D52" strokeWidth={1.4} />
        <text x={46} y={39} fontFamily="Segoe UI, system-ui, sans-serif" fontSize={13} fill="#1F2328">
          Q3 report
        </text>
        <text x={112} y={39} fontFamily="Segoe UI, system-ui, sans-serif" fontSize={13} fill="#6B7280">
          Reports
        </text>
        <line x1={8} y1={106} x2={W} y2={106} stroke="#E5E7EB" />
        <Toolbar x={22} label="Open" icon="M2 4h5l2 2h6v8H2z" />
        <Toolbar x={104} label="Save" icon="M2 2h10l3 3v10H2zM5 2v4h6V2" />
        <Toolbar x={198} label="Export PDF" icon="M3 2h7l4 4v9H3zM10 2v4h4" />
        <Toolbar x={330} label="Share" icon="M10 3l5 4-5 4M15 7H7a5 5 0 0 0-5 5" />
        <text x={40} y={160} fontFamily="Segoe UI Variable Display, Segoe UI, system-ui, sans-serif" fontSize={30} fontWeight={600} fill="#1F2328">
          Q3 report
        </text>
        <text x={40} y={184} fontFamily="Segoe UI, system-ui, sans-serif" fontSize={14} fill="#4B5563">
          Revenue and usage, July to September
        </text>
        <rect x={40} y={208} width={270} height={150} rx={6} fill="none" stroke="#E5E7EB" />
        <text x={56} y={232} fontFamily="Segoe UI, system-ui, sans-serif" fontSize={13} fontWeight={600} fill="#1F2328">
          Key figures
        </text>
        {[
          ["Revenue", "$4.2M"],
          ["Churn", "2.4%"],
          ["Active teams", "1,284"],
        ].map(([k, v], i) => (
          <g key={k}>
            <line x1={56} y1={244 + i * 32} x2={294} y2={244 + i * 32} stroke="#EEF0F2" />
            <text x={56} y={264 + i * 32} fontFamily="Segoe UI, system-ui, sans-serif" fontSize={13} fill="#4B5563">
              {k}
            </text>
            <text x={294} y={264 + i * 32} textAnchor="end" fontFamily="Segoe UI, system-ui, sans-serif" fontSize={13} fill="#1F2328">
              {v}
            </text>
          </g>
        ))}
        <rect x={326} y={208} width={260} height={150} rx={6} fill="none" stroke="#E5E7EB" />
        <text x={342} y={232} fontFamily="Segoe UI, system-ui, sans-serif" fontSize={13} fontWeight={600} fill="#1F2328">
          Revenue by month
        </text>
        {bars.map((h, i) => (
          <rect key={i} x={346 + i * 34} y={340 - h} width={24} height={h} rx={2} fill={i < 3 ? "#C9D6D2" : "#4E7D74"} />
        ))}
      </svg>
      <div className="ov">
        <div className="tgt dashed" style={{ left: btn.x, top: btn.y, width: btn.w, height: btn.h }}>
          <span className="tgt-chip">
            <span className="ref solid">e4</span>
          </span>
        </div>
        <div className="ptr-wrap" style={{ left: btn.x + btn.w / 2 + 2, top: btn.y + btn.h / 2 + 2 }}>
          <Pointer who="agent" h={26} />
        </div>
        <div className="ptr-label intent" style={{ left: btn.x + btn.w / 2 + 2, top: btn.y + btn.h + 20 }}>
          {t("lb_clicking_export")}
          <span className="ref pill-ref">e4</span>
        </div>
      </div>
    </div>
  );
}
