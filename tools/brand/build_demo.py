"""Animated demo: an agent operates office-pc through ai-remote while you stay in control.

The scene is the controller's view of one host. Left: the remote screen. Right: what the agent sees
(the UI tree from ui_tree) and the audit log. Top: the session bar, which says who is in control.

Story, one 18 s loop (CSS keyframes, translate/opacity/dash offset only):
  1. ui_tree: element outlines e1-e9 appear on the screen while the same rows fill the tree panel.
     Each badge flashes cobalt as it is read, then rests in a solid muted tone.
  2. The agent clicks e4 "Export PDF" by reference; a trace runs from the button, under the toolbar,
     to the tree row, and the e4 badge turns cobalt while it is the target.
  3. It stops before a shell command that deletes files (the device is at admin, and shell asks).
     A sheet drops from the session bar; your pointer approves once.
  4. The agent heads for e5 "Share". You take over first, fix one figure, and hand back.
  5. The agent resumes and shares the updated report. Every step lands in the audit log with
     consistent times; the shell row records your approval and the result.

The loop starts at 2.75 s (negative delay), so the first frame already shows the agent acting by
reference, with the e4 trace fully drawn. With prefers-reduced-motion the base attributes form one
story frame: tree read, sheet waiting for you, your pointer on "Approve once".

Set DEMO_START=0 to build without the start offset when inspecting frames by story time.
"""
import os
from common import PAL, cursor, mark, svg_doc, esc, Text

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ASSETS = os.path.join(ROOT, "assets", "brand")
T = 18.0
START_AT = float(os.environ.get("DEMO_START", "2.75"))
W, H = 1760, 1068
EASE = "cubic-bezier(.55,0,.25,1)"

COPY = {
    "en": dict(
        title="ai-remote demo. An AI agent reads the UI tree of a remote Windows PC and clicks Export PDF by its element "
              "reference. Before a shell command that deletes files it stops, and you approve it once. When it heads for "
              "Share, you take over, fix one figure, and hand back; the agent then shares the updated report. Every step is "
              "written to the audit log.",
        host="office-pc", level="admin", hostline="Windows 11, encrypted direct link",
        a_t="Agent is operating office-pc", a_s="Task: export the Q3 report, delete old drafts, share the report with Finance.",
        q_t="Waiting for your approval", q_s="The agent stopped before a shell command. Nothing runs until you decide.",
        h_t="You have control", h_s="The agent is paused and can't act until you hand back.",
        takeover="Take over", handback="Hand back",
        app="Q3 report", open="Open", save="Save", export="Export PDF", share="Share",
        doc_h="Q3 report", doc_sub="Revenue and usage, July to September",
        r1="Revenue", v1="$4.2M", r2="Churn", v2="2.4%", v2b="2.1%", para="Revenue grew 12% over Q2.",
        tree_h="What the agent sees", tree_empty="Not read yet",
        tree=[(0, "window", "Q3 report"), (1, "button", "Open"), (1, "button", "Save"), (1, "button", "Export PDF"),
              (1, "button", "Share"), (1, "heading", "Q3 report"), (1, "table", "Key figures"), (2, "cell", "$4.2M"),
              (2, "cell", "2.4%")],
        audit="Audit log", today="Today", empty="Nothing recorded yet",
        rows=[
            ("agent", "ui_tree", "Read 9 elements", "14:02:01"),
            ("agent", 'click e4 "Export PDF"', "Saved Q3 report.pdf", "14:02:03"),
            ("agent", r"shell Remove-Item .\drafts -Recurse", "Waiting for your approval", "14:02:05"),
            ("you", "take over", "Agent paused before click e5", "14:02:12"),
            ("you", "hand back", "You changed e9 to 2.1%", "14:02:19"),
            ("agent", 'click e5 "Share"', "Shared the updated report with Finance", "14:02:21"),
        ],
        row3_after="Ran after your approval, 37 files removed",
        sheet_t="Approve this shell command?",
        sheet_b="It deletes files, so it waits for you.",
        cmd=r"Remove-Item .\drafts -Recurse",
        sheet_m=r"Removes 37 files in C:\Reports\drafts",
        deny="Deny", approve="Approve once",
        toast1="Saved Q3 report.pdf", toast2="Shared the updated report with Finance",
        tag_a="agent", tag_p="paused", tag_h="you",
        btns=[("open", 84, 100), ("save", 196, 96), ("export", 304, 168), ("share", 484, 104)],
    ),
    "zh": dict(
        title="ai-remote 演示：AI Agent 读取远程 Windows 电脑的界面树，按元素引用点击“导出 PDF”；在执行会删除文件的 shell "
              "命令前停下，由你批准一次。它准备点“分享”时，你先接管、改了一个数字再交还，随后 Agent 分享更新后的报告。每一步都记入审计日志。",
        host="office-pc", level="admin", hostline="Windows 11，加密直连",
        a_t="Agent 正在操作 office-pc", a_s="任务：导出 Q3 报告，删除旧草稿，再把报告分享给财务组。",
        q_t="等待你的批准", q_s="Agent 在执行 shell 命令前停下了，你做决定之前什么都不会执行。",
        h_t="现在由你操作", h_s="Agent 已暂停，你交还之前它不能做任何操作。",
        takeover="接管", handback="交还",
        app="Q3 报告", open="打开", save="保存", export="导出 PDF", share="分享",
        doc_h="Q3 报告", doc_sub="7 月至 9 月的收入与用量",
        r1="收入", v1="$4.2M", r2="流失率", v2="2.4%", v2b="2.1%", para="收入环比增长 12%。",
        tree_h="Agent 看到的界面", tree_empty="尚未读取",
        tree=[(0, "window", "Q3 报告"), (1, "button", "打开"), (1, "button", "保存"), (1, "button", "导出 PDF"),
              (1, "button", "分享"), (1, "heading", "Q3 报告"), (1, "table", "关键指标"), (2, "cell", "$4.2M"),
              (2, "cell", "2.4%")],
        audit="审计日志", today="今天", empty="暂无记录",
        rows=[
            ("agent", "ui_tree", "读取了 9 个元素", "14:02:01"),
            ("agent", 'click e4 "导出 PDF"', "已保存 Q3 报告.pdf", "14:02:03"),
            ("agent", r"shell Remove-Item .\drafts -Recurse", "等待你批准", "14:02:05"),
            ("you", "接管", "Agent 在 click e5 之前暂停", "14:02:12"),
            ("you", "交还", "你把 e9 改成了 2.1%", "14:02:19"),
            ("agent", 'click e5 "分享"', "已把更新后的报告分享给财务组", "14:02:21"),
        ],
        row3_after="经你批准后执行，删除了 37 个文件",
        sheet_t="批准这条 shell 命令吗？",
        sheet_b="这条命令会删除文件，所以要等你批准。",
        cmd=r"Remove-Item .\drafts -Recurse",
        sheet_m=r"将删除 C:\Reports\drafts 中的 37 个文件",
        deny="拒绝", approve="仅批准这一次",
        toast1="已保存 Q3 报告.pdf", toast2="已把更新后的报告分享给财务组",
        tag_a="Agent", tag_p="已暂停", tag_h="你",
        btns=[("open", 84, 96), ("save", 192, 96), ("export", 300, 160), ("share", 472, 96)],
    ),
}


def text_w(s, size, latin=0.55):
    """Rough advance width for layout (CJK = 1 em, Latin ~0.55 em for bold labels, ~0.47 em for regular text)."""
    w = 0.0
    for ch in s:
        w += size * (1.0 if ord(ch) > 0x2E80 else latin)
    return w


class Anim:
    def __init__(self):
        self.css = []
        self.n = 0

    def track(self, prop, points, ease="linear", fmt=None):
        """points: [(t, value)]; values are held before the first and after the last point."""
        self.n += 1
        name = f"k{self.n}"
        pts = sorted(points, key=lambda x: x[0])
        if pts[0][0] > 0:
            pts.insert(0, (0.0, pts[0][1]))
        if pts[-1][0] < T:
            pts.append((T, pts[-1][1]))
        stops = []
        for t, v in pts:
            val = fmt(v) if fmt else v
            stops.append(f"{max(0.0, min(100.0, t / T * 100)):.2f}%{{{prop}:{val}}}")
        self.css.append(f"@keyframes {name}{{{''.join(stops)}}}")
        self.css.append(f".{name}{{animation:{name} {T:g}s {ease} -{START_AT:g}s infinite}}")
        return name

    def opacity(self, points):
        return self.track("opacity", points, fmt=lambda v: f"{v:g}")

    def fade(self, spans, hidden=0.0, ramp=0.25):
        pts = [(0.0, hidden)]
        for sp in spans:
            t_in, t_out, lvl = sp if len(sp) == 3 else (sp[0], sp[1], 1)
            pts += [(t_in, hidden), (t_in + ramp, lvl), (t_out, lvl), (t_out + ramp, hidden)]
        return self.opacity(pts)

    def move(self, points):
        return self.track("transform", points, ease=EASE, fmt=lambda v: f"translate({v[0]:.1f}px,{v[1]:.1f}px)")


def build(theme, lang):
    p = PAL[theme]
    c = COPY[lang]
    t = Text(lang)
    A = Anim()
    o = []
    add = o.append

    # ------------------------------------------------------------------ geometry
    BAR = (20, 20, 1740, 124)
    SCR = (20, 140, 1068, 1048)
    WIN = (52, 172, 1036, 1016)
    TRE = (1088, 140, 1740, 512)
    AUD = (1088, 528, 1740, 1048)
    WX0, WY0, WX1, WY1 = WIN
    TB = WY0 + 48            # toolbar top
    BY, BH = TB + 42, 44     # buttons
    LEAD_Y = TB + 120        # leader lane: under the toolbar divider (TB + 104), above the document (352)
    B = {k: (x, BY, w, BH) for k, x, w in c["btns"]}
    TARGET = {"e4": [(2.2, 4.3)], "e5": [(8.0, 9.2), (13.1, 14.2)]}   # when the agent acts on each element

    # ------------------------------------------------------------------ frame
    add(f'<rect x="1" y="1" width="{W - 2}" height="{H - 2}" rx="20" fill="{p["panel"]}" stroke="{p["line"]}" stroke-width="2"/>')

    # ------------------------------------------------------------------ tree panel (what the agent sees)
    X0, Y0, X1, Y1 = TRE
    add(f'<rect x="{X0}" y="{Y0}" width="{X1 - X0}" height="{Y1 - Y0}" rx="12" fill="{p["panel2"]}" stroke="{p["line"]}" stroke-width="1.5"/>')
    add(t(X0 + 24, Y0 + 44, c["tree_h"], 26, p["ink"], weight=600))
    add(f'<path d="M{X0},{Y0 + 64} H{X1}" stroke="{p["line"]}" stroke-width="1.5"/>')
    k = A.opacity([(0, 1), (0.35, 1), (0.5, 0), (17.6, 0), (17.8, 1)])
    add(f'<g class="{k}" opacity="0">{t(X0 + 24, Y0 + 104, c["tree_empty"], 22, p["muted"])}</g>')
    ROW0, RSTEP = Y0 + 96, 32
    rowy = [ROW0 + RSTEP * i for i in range(len(c["tree"]))]

    def row_hl(i, spans, color, base=0.0):
        k = A.fade(spans, ramp=0.2)
        y = rowy[i] - 23
        return (f'<g class="{k}" opacity="{base}"><rect x="{X0 + 10}" y="{y}" width="{X1 - X0 - 20}" height="32" rx="6" '
                f'fill="{color}" fill-opacity="{0.16 if color == p["agent"] else 0.22}"/>'
                f'<rect x="{X0 + 10}" y="{y}" width="4" height="32" rx="2" fill="{color}"/></g>')

    # row highlights sit under the rows
    add(row_hl(3, [(a, b, 1) for a, b in TARGET["e4"]], p["agent"]))
    add(row_hl(4, [(a, b, 1) for a, b in TARGET["e5"]], p["agent"]))
    add(row_hl(8, [(11.0, 11.9, 1)], p["human"]))

    t_rows = [0.45 + 0.15 * i for i in range(9)]
    for i, (ind, role, name) in enumerate(c["tree"]):
        y = rowy[i]
        k = A.opacity([(0, 0), (t_rows[i], 0), (t_rows[i] + 0.25, 1), (17.3, 1), (17.55, 0)])
        g = [f'<g class="{k}" opacity="1">']
        g.append(f'<rect x="{X0 + 20}" y="{y - 19}" width="44" height="25" rx="6" fill="{p["agent"]}"/>')
        g.append(t(X0 + 42, y, f"e{i + 1}", 20, p["onagent"], weight=700, anchor="middle", mono=True))
        tx = X0 + 84 + 28 * ind
        if ind:
            gx = X0 + 84 + 28 * (ind - 1) + 8
            # a later sibling at this depth (before the branch closes) continues the spine: draw a tee, else an elbow
            later = False
            for ind2, _, _ in c["tree"][i + 1:]:
                if ind2 < ind:
                    break
                if ind2 == ind:
                    later = True
                    break
            y_end = y + 9 if later else y - 7
            g.append(f'<path d="M{gx},{y - 23} V{y_end} M{gx},{y - 7} H{gx + 13}" fill="none" stroke="{p["line2"]}" stroke-width="2"/>')
        if i == 8:
            # this cell's value changes when you edit it; the agent sees your change
            ka = A.opacity([(0, 1), (11.0, 1), (11.12, 0), (17.3, 0), (17.31, 1)])
            kb = A.opacity([(0, 0), (11.05, 0), (11.2, 1), (17.3, 1), (17.31, 0)])
            g.append(f'<text class="m" x="{tx}" y="{y}" font-size="22" fill="{p["muted"]}">{role} </text>')
            nx = tx + 13.2 * (len(role) + 1)
            g.append(f'<g class="{ka}" opacity="1"><text class="m" x="{nx:.1f}" y="{y}" font-size="22" fill="{p["ink"]}">"{esc(name)}"</text></g>')
            g.append(f'<g class="{kb}" opacity="0"><text class="m" x="{nx:.1f}" y="{y}" font-size="22" fill="{p["ink"]}">"{esc(c["v2b"])}"</text></g>')
        else:
            g.append(f'<text class="m" x="{tx}" y="{y}" font-size="22" fill="{p["muted"]}">{role} '
                     f'<tspan fill="{p["ink"]}">"{esc(name)}"</tspan></text>')
        g.append('</g>')
        add("".join(g))

    # ------------------------------------------------------------------ audit log
    X0, Y0, X1, Y1 = AUD
    add(f'<rect x="{X0}" y="{Y0}" width="{X1 - X0}" height="{Y1 - Y0}" rx="12" fill="{p["panel2"]}" stroke="{p["line"]}" stroke-width="1.5"/>')
    add(t(X0 + 24, Y0 + 42, c["audit"], 26, p["ink"], weight=600))
    add(t(X1 - 24, Y0 + 42, c["today"], 21, p["muted"], anchor="end"))
    add(f'<path d="M{X0},{Y0 + 62} H{X1}" stroke="{p["line"]}" stroke-width="1.5"/>')
    k = A.opacity([(0, 1), (0.2, 1), (0.35, 0), (17.6, 0), (17.8, 1)])
    add(f'<g class="{k}" opacity="0">{t(X0 + 24, Y0 + 104, c["empty"], 22, p["muted"])}</g>')
    appear = [0.3, 3.45, 4.15, 9.4, 12.6, 14.15]
    RH = 64
    ry0 = Y0 + 68
    for i, ((who, act, status, tm), t_in) in enumerate(zip(c["rows"], appear)):
        y = ry0 + i * RH
        col = p["agent"] if who == "agent" else p["human"]
        base = 1 if i < 3 else 0
        k1 = A.opacity([(0, 0), (t_in, 0), (t_in + 0.3, 1), (17.2, 1), (17.5, 0)])
        k2 = A.move([(0, (0, 10)), (t_in, (0, 10)), (t_in + 0.35, (0, 0)), (17.5, (0, 0)), (17.51, (0, 10))])
        g = [f'<g class="{k1}" opacity="{base}"><g class="{k2}">']
        if i > 0:
            g.append(f'<path d="M{X0 + 24},{y} H{X1 - 24}" stroke="{p["rule"]}" stroke-width="1.5"/>')
        ix = X0 + 44 if who == "agent" else X0 + 26
        g.append(f'<g transform="translate({ix} {y + 12})">{cursor(24, col, mirror=(who == "agent"))}</g>')
        # tool calls stay in monospace; in Chinese your own events (接管 / 交还) read as words, in the CJK face
        g.append(t(X0 + 62, y + 30, act, 22, p["ink"], mono=not (who == "you" and lang == "zh")))
        if i == 2:
            ka = A.opacity([(0, 1), (6.9, 1), (7.02, 0), (17.5, 0), (17.51, 1)])
            kb = A.opacity([(0, 0), (6.95, 0), (7.1, 1), (17.5, 1), (17.51, 0)])
            g.append(f'<g class="{ka}" opacity="1">{t(X0 + 62, y + 56, status, 21, p["humantext"], weight=600)}</g>')
            g.append(f'<g class="{kb}" opacity="0">{t(X0 + 62, y + 56, c["row3_after"], 21, p["muted"])}</g>')
        else:
            g.append(t(X0 + 62, y + 56, status, 21, p["muted"]))
        g.append(t(X1 - 24, y + 56, tm, 20, p["muted"], anchor="end", extra=' style="font-variant-numeric:tabular-nums"'))
        g.append('</g></g>')
        add("".join(g))

    # ------------------------------------------------------------------ remote screen
    X0, Y0, X1, Y1 = SCR
    add(f'<rect x="{X0}" y="{Y0}" width="{X1 - X0}" height="{Y1 - Y0}" rx="12" fill="{p["desk"]}"/>')
    add(f'<rect x="{WX0}" y="{WY0}" width="{WX1 - WX0}" height="{WY1 - WY0}" rx="8" fill="{p["win"]}" stroke="{p["btnline"]}" stroke-width="2"/>')
    add(f'<path d="M{WX0 + 1},{TB} V{WY0 + 8} a7,7 0 0,1 7,-7 H{WX1 - 8} a7,7 0 0,1 7,7 V{TB} Z" fill="{p["winbar"]}"/>')
    add(t(WX0 + 24, WY0 + 32, c["app"], 22, p["ink"]))
    cy = WY0 + 24
    add(f'<g stroke="{p["muted"]}" stroke-width="2" fill="none">'
        f'<path d="M{WX1 - 132},{cy} h16"/><rect x="{WX1 - 88}" y="{cy - 8}" width="16" height="16"/>'
        f'<path d="M{WX1 - 44},{cy - 8} l16,16 M{WX1 - 28},{cy - 8} l-16,16"/></g>')
    add(f'<rect x="{WX0 + 1}" y="{TB}" width="{WX1 - WX0 - 2}" height="104" fill="{p["winbar"]}" opacity="0.5"/>')
    add(f'<path d="M{WX0},{TB} H{WX1} M{WX0},{TB + 104} H{WX1}" stroke="{p["btnline"]}" stroke-width="1.5"/>')
    press = {"export": [(3.05, 3.3)], "share": [(13.75, 14.0)]}
    for key in ("open", "save", "export", "share"):
        bx, by, bw, bh = B[key]
        add(f'<rect x="{bx}" y="{by}" width="{bw}" height="{bh}" rx="7" fill="{p["btn"]}" stroke="{p["btnline"]}" stroke-width="1.5"/>')
        if key in press:
            k = A.fade([(a, b, 1) for a, b in press[key]], ramp=0.08)
            add(f'<rect class="{k}" x="{bx}" y="{by}" width="{bw}" height="{bh}" rx="7" fill="{p["agent"]}" fill-opacity="0.35" opacity="0"/>')
        add(t(bx + bw / 2, by + 30, c[key], 22, p["ink"], anchor="middle"))
    # document page
    PX0, PY0, PX1, PY1 = 100, 352, 988, 990
    add(f'<rect x="{PX0}" y="{PY0}" width="{PX1 - PX0}" height="{PY1 - PY0}" rx="6" fill="{p["panel2"]}" stroke="{p["btnline"]}" stroke-width="1.5"/>')
    add(t(140, 420, c["doc_h"], 40, p["ink"], weight=600))
    add(t(140, 458, c["doc_sub"], 23, p["muted"]))
    TX0, TY0, TX1 = 140, 488, 600
    add(f'<rect x="{TX0}" y="{TY0}" width="{TX1 - TX0}" height="124" rx="8" fill="none" stroke="{p["btnline"]}" stroke-width="1.5"/>'
        f'<path d="M{TX0},{TY0 + 62} H{TX1}" stroke="{p["btnline"]}" stroke-width="1.5"/>')
    add(t(164, TY0 + 40, c["r1"], 23, p["muted"]))
    add(t(576, TY0 + 40, c["v1"], 23, p["ink"], weight=600, anchor="end"))
    add(t(164, TY0 + 102, c["r2"], 23, p["muted"]))
    # your selection on the churn value, then the new value
    k = A.fade([(10.65, 11.35, 1)], ramp=0.12)
    add(f'<rect class="{k}" x="512" y="{TY0 + 76}" width="72" height="34" rx="4" fill="{p["human"]}" fill-opacity="0.38" opacity="0"/>')
    ka = A.opacity([(0, 1), (11.0, 1), (11.1, 0), (17.3, 0), (17.31, 1)])
    kb = A.opacity([(0, 0), (11.02, 0), (11.12, 1), (17.3, 1), (17.31, 0)])
    add(f'<g class="{ka}" opacity="1">{t(576, TY0 + 102, c["v2"], 23, p["ink"], weight=600, anchor="end")}</g>')
    add(f'<g class="{kb}" opacity="0">{t(576, TY0 + 102, c["v2b"], 23, p["ink"], weight=600, anchor="end")}</g>')
    add(t(140, 676, c["para"], 22, p["muted"]))
    for i, hv in enumerate([92, 128, 112, 166, 190, 150]):
        add(f'<rect x="{140 + i * 64}" y="{940 - hv}" width="40" height="{hv}" rx="3" fill="{p["docstrong"]}"/>')
    add(f'<path d="M140,942 H{140 + 6 * 64 - 24}" stroke="{p["docstrong"]}" stroke-width="2"/>')

    # ------------------------------------------------------------------ element outlines (refs)
    hw = text_w(c["doc_h"], 40) * 0.92
    els = [("e1", WX0 + 5, WY0 + 5, WX1 - WX0 - 10, WY1 - WY0 - 10)]
    for i, key in enumerate(["open", "save", "export", "share"]):
        bx, by, bw, bh = B[key]
        els.append((f"e{2 + i}", bx - 4, by - 4, bw + 8, bh + 8))
    els += [("e6", 130, 380, hw + 24, 54), ("e7", 132, TY0 - 8, TX1 - TX0 + 16, 140),
            ("e8", 440, TY0 + 8, 148, 46), ("e9", 440, TY0 + 70, 148, 46)]
    badge_at = {"e1": lambda x, y, w, h: (x + 10, y + h - 36), "e7": lambda x, y, w, h: (x + w + 8, y + h / 2 - 13),
                "e8": lambda x, y, w, h: (x - 52, y + 10), "e9": lambda x, y, w, h: (x - 52, y + 10)}
    for i, (ref, x, y, w, h) in enumerate(els):
        t0 = t_rows[i]
        # the outline dims once the tree is read; the badge stays solid so the reference stays legible
        k = A.opacity([(0, 0), (t0, 0), (t0 + 0.25, 0.85), (3.4, 0.85), (3.8, 0.45), (17.3, 0.45), (17.55, 0)])
        bx, by = badge_at.get(ref, lambda x, y, w, h: (x, y - 30))(x, y, w, h)
        add(f'<g class="{k}" opacity="0.45">'
            f'<rect x="{x}" y="{y}" width="{w:.0f}" height="{h}" rx="7" fill="{p["agent"]}" fill-opacity="{p["agentsoftop"]}" '
            f'stroke="{p["agent"]}" stroke-width="2"/></g>')
        kb = A.opacity([(0, 0), (t0, 0), (t0 + 0.25, 1), (17.3, 1), (17.55, 0)])
        badge = (f'<g class="{kb}" opacity="1">'
                 f'<rect x="{bx:.0f}" y="{by:.0f}" width="44" height="26" rx="6" fill="{p["badge"]}" stroke="{p["agent"]}" '
                 f'stroke-opacity="0.35" stroke-width="1.5"/>'
                 f'{t(bx + 22, by + 20, ref, 20, p["onbadge"], weight=700, anchor="middle", mono=True)}')
        # cobalt while being read, and again whenever the agent acts on this element
        ks = A.fade([(t0, t0 + 0.9)] + TARGET.get(ref, []), ramp=0.15)
        badge += (f'<g class="{ks}" opacity="0"><rect x="{bx:.0f}" y="{by:.0f}" width="44" height="26" rx="6" fill="{p["agent"]}"/>'
                  f'{t(bx + 22, by + 20, ref, 20, p["onagent"], weight=700, anchor="middle", mono=True)}</g></g>')
        add(badge)

    # traces from the tree row to the element the agent acts on, plus a marching-ants selection
    A.css.append("@keyframes ants{to{stroke-dashoffset:-24}}.ants{animation:ants .9s linear infinite}")

    def act_on(key, row, spans):
        """Leader from the element the agent acts on to its row in the tree. It leaves the button from the
        bottom of the selection ring, runs in its own lane under the toolbar divider (so it never grazes a
        neighbouring button), and is drawn from the button toward the tree."""
        bx, by, bw, bh = B[key]
        ty = rowy[row] - 7
        tx = TRE[0] + 20
        gx = (SCR[2] + TRE[0]) / 2
        ex = bx + bw / 2
        y0 = by + bh + 11          # selection ring bottom is at by + bh + 9
        d = f"M{ex},{y0} V{LEAD_Y} H{gx} V{ty} H{tx}"
        L = (LEAD_Y - y0) + (gx - ex) + abs(ty - LEAD_Y) + (tx - gx)
        dash_pts = [(0, L)]
        for a, b in spans:
            dash_pts += [(a, L), (a + 0.4, 0), (b, 0), (b + 0.3, 0), (b + 0.31, L)]
        kd = A.track("stroke-dashoffset", dash_pts, fmt=lambda v: f"{v:.0f}")
        ko = A.fade([(a, b, 1) for a, b in spans], ramp=0.15)
        s = (f'<g class="{ko}" opacity="0"><path class="{kd}" d="{d}" fill="none" stroke="{p["agent"]}" stroke-width="3" '
             f'stroke-dasharray="{L:.0f} {L:.0f}" stroke-dashoffset="{L:.0f}" stroke-linejoin="round"/>'
             f'<circle cx="{ex}" cy="{y0}" r="5" fill="{p["agent"]}"/>'
             f'<circle cx="{tx}" cy="{ty}" r="5" fill="{p["agent"]}"/></g>')
        ka = A.fade([(a + 0.15, b, 1) for a, b in spans], ramp=0.2)
        s += (f'<g class="{ka}" opacity="0"><rect class="ants" x="{bx - 9}" y="{by - 9}" width="{bw + 18}" height="{bh + 18}" rx="10" '
              f'fill="none" stroke="{p["agent"]}" stroke-width="3.5" stroke-dasharray="14 10"/></g>')
        return s

    add(act_on("export", 3, TARGET["e4"]))
    add(act_on("share", 4, TARGET["e5"]))

    # toasts (cobalt: the agent did it)
    def toast(text, span):
        tw = text_w(text, 22, latin=0.47) + 88
        x, y = WX1 - 28 - tw, WY1 - 92
        k = A.fade([span], ramp=0.25)
        return (f'<g class="{k}" opacity="0"><rect x="{x:.0f}" y="{y}" width="{tw:.0f}" height="56" rx="10" fill="{p["panel2"]}" '
                f'stroke="{p["agent"]}" stroke-width="2"/>'
                f'<g transform="translate({x + 46:.0f} {y + 14})">{cursor(28, p["agent"], mirror=True)}</g>'
                f'{t(x + 62, y + 36, text, 22, p["ink"])}</g>')

    add(toast(c["toast1"], (3.25, 4.9)))
    add(toast(c["toast2"], (13.95, 15.8)))

    # scrim over the remote screen while you decide
    k = A.fade([(4.45, 6.85, 1)], ramp=0.3)
    add(f'<rect class="{k}" x="{SCR[0]}" y="{SCR[1]}" width="{SCR[2] - SCR[0]}" height="{SCR[3] - SCR[1]}" rx="12" '
        f'fill="{p["scrim"]}" fill-opacity="{p["scrimop"]}" opacity="1"/>')

    # ------------------------------------------------------------------ pointers
    def pointer(color, oncolor, mirror, tags, path, clicks, vis, base_pos, base_op, dim=None, grey=None):
        kpos = A.move(path)
        kvis = A.opacity(vis)
        pulse, ring_r, ring_o = [], [], []
        for tc in clicks:
            pulse += [(tc - 0.01, 1.0), (tc + 0.07, 0.84), (tc + 0.2, 1.0)]
            ring_r += [(tc - 0.01, 0.2), (tc, 0.2), (tc + 0.45, 1.0), (tc + 0.46, 0.2)]
            ring_o += [(tc - 0.01, 0.0), (tc, 0.75), (tc + 0.45, 0.0)]
        kp = A.track("transform", [(0, 1.0)] + pulse + [(T, 1.0)], fmt=lambda v: f"scale({v:g})")
        kr = A.track("transform", [(0, 0.2)] + ring_r + [(T, 0.2)], fmt=lambda v: f"scale({v:g})")
        ko = A.opacity([(0, 0.0)] + ring_o + [(T, 0.0)])
        s = [f'<g class="{kvis}" opacity="{base_op}"><g class="{kpos}" transform="translate({base_pos[0]} {base_pos[1]})">',
             f'<g class="{ko}" opacity="0"><circle class="{kr}" cx="0" cy="0" r="30" fill="none" stroke="{color}" stroke-width="4"/></g>',
             f'<g class="{kp}">']
        if dim:
            s.append(f'<g class="{dim[0]}" opacity="{dim[1]}">')
        s.append(cursor(48, color, mirror=mirror, outline=p["ptrline"], ow=float(p["ptrw"])))
        if grey:
            s.append(f'<g class="{grey}" opacity="0">{cursor(48, p["grey"], mirror=mirror)}</g>')
        for text, tk, fill, tcol in tags:
            w = text_w(text, 21) + 28
            # the tag hangs beside the cursor's tail: left of it for the agent's mirrored arrow, right for yours
            tx = -26 - w if mirror else 26
            cls = f' class="{tk[0]}"' if tk else ""
            base = f' opacity="{tk[1]}"' if tk else ""
            s.append(f'<g{cls}{base}><rect x="{tx}" y="44" width="{w:.0f}" height="32" rx="8" fill="{fill}" stroke="{p["outline"]}" '
                     f'stroke-width="3" paint-order="stroke"/>{t(tx + w / 2, 67, text, 21, tcol, weight=700, anchor="middle")}</g>')
        if dim:
            s.append('</g>')
        s.append('</g></g></g>')
        return "".join(s)

    ex, ey, ew, eh = B["export"]
    sx_, sy_, sw_, sh_ = B["share"]
    # the agent clicks left of centre so its arrow and tag stay clear of the leader dropping from the centre
    EXP = (ex + 38, ey + 34)
    SHR = (sx_ + 26, sy_ + 34)
    START = (760, 724)
    PARK = (890, 610)
    apath = [(0, START), (2.0, START), (2.85, EXP), (4.2, EXP), (4.9, PARK), (7.4, PARK), (8.2, SHR), (17.35, SHR), (17.95, START)]
    adim = A.opacity([(0, 1), (4.35, 1), (4.6, 0.45), (7.05, 0.45), (7.3, 1)])
    agrey = A.opacity([(0, 0), (9.2, 0), (9.4, 1), (12.6, 1), (12.85, 0)])
    ktag_a = A.opacity([(0, 1), (9.2, 1), (9.3, 0), (12.75, 0), (12.85, 1)])
    ktag_p = A.opacity([(0, 0), (9.25, 0), (9.35, 1), (12.65, 1), (12.75, 0)])
    agent_ptr = pointer(p["agent"], p["onagent"], True,
                        [(c["tag_a"], (ktag_a, 1), p["agent"], p["onagent"]), (c["tag_p"], (ktag_p, 0), p["greypill"], p["ongrey"])],
                        apath, [3.05, 13.75], [(0, 1)], PARK, 1, dim=(adim, 0.45), grey=agrey)
    add(agent_ptr)

    # ------------------------------------------------------------------ approval sheet (drops from the session bar)
    QX0, QY0, QX1, QY1 = 80, 100, 1008, 518   # room under the buttons so your pointer's tag stays inside
    add(f'<clipPath id="below-bar"><rect x="0" y="{BAR[3]}" width="{SCR[2] + 10}" height="{H}"/></clipPath>')
    k1 = A.opacity([(0, 0), (4.5, 0), (4.51, 1), (7.1, 1), (7.11, 0)])
    k2 = A.move([(0, (0, -400)), (4.5, (0, -400)), (4.95, (0, 0)), (6.75, (0, 0)), (7.1, (0, -400))])
    add(f'<g clip-path="url(#below-bar)"><g class="{k1}" opacity="1"><g class="{k2}" transform="translate(0 0)">')
    add(f'<rect x="{QX0}" y="{QY0}" width="{QX1 - QX0}" height="{QY1 - QY0}" rx="16" fill="{p["panel2"]}" stroke="{p["humanline"]}" stroke-width="3"/>')
    add(f'<g transform="translate({QX0 + 40} {QY0 + 58})">{cursor(34, p["human"])}</g>')
    add(t(QX0 + 86, QY0 + 88, c["sheet_t"], 30, p["ink"], weight=600))
    add(t(QX0 + 86, QY0 + 128, c["sheet_b"], 23, p["muted"]))
    add(f'<rect x="{QX0 + 86}" y="{QY0 + 154}" width="{QX1 - QX0 - 126}" height="62" rx="8" fill="{p["win"]}" stroke="{p["line"]}" stroke-width="1.5"/>')
    add(t(QX0 + 110, QY0 + 194, c["cmd"], 25, p["ink"], mono=True))
    add(t(QX0 + 86, QY0 + 256, c["sheet_m"], 22, p["muted"]))
    by_ = QY1 - 120
    dx0 = QX1 - 40 - 212 - 16 - 150
    add(f'<rect x="{dx0}" y="{by_}" width="150" height="60" rx="10" fill="none" stroke="{p["line2"]}" stroke-width="2"/>')
    add(t(dx0 + 75, by_ + 39, c["deny"], 24, p["ink"], anchor="middle"))
    ax0 = QX1 - 40 - 212
    add(f'<rect x="{ax0}" y="{by_}" width="212" height="60" rx="10" fill="{p["human"]}"/>')
    k = A.fade([(6.45, 6.65, 1)], ramp=0.08)
    add(f'<rect class="{k}" x="{ax0}" y="{by_}" width="212" height="60" rx="10" fill="{p["humanpress"]}" opacity="0"/>')
    add(t(ax0 + 106, by_ + 39, c["approve"], 24, p["onhuman"], weight=600, anchor="middle"))
    add('</g></g></g>')
    APPROVE = (ax0 + 144, by_ + 34)   # on the right half of the label, so the click reads as deliberate

    # ------------------------------------------------------------------ session bar (who is in control)
    X0, Y0, X1, Y1 = BAR
    BTN = (1480, 42, 200, 60)
    add(f'<rect x="{X0}" y="{Y0}" width="{X1 - X0}" height="{Y1 - Y0}" rx="14" fill="{p["panel2"]}" stroke="{p["line"]}" stroke-width="2"/>')
    fa = A.opacity([(0, 1), (4.35, 1), (4.6, 0), (6.8, 0), (7.05, 1), (9.15, 1), (9.4, 0), (12.5, 0), (12.75, 1)])
    fh = A.opacity([(0, 0), (4.35, 0), (4.6, 1), (6.8, 1), (7.05, 0), (9.15, 0), (9.4, 1), (12.5, 1), (12.75, 0)])
    add(f'<rect class="{fa}" x="{X0}" y="{Y0}" width="{X1 - X0}" height="{Y1 - Y0}" rx="14" fill="{p["agent"]}" fill-opacity="0.06" '
        f'stroke="{p["agent"]}" stroke-width="2" opacity="0"/>')
    add(f'<rect class="{fh}" x="{X0}" y="{Y0}" width="{X1 - X0}" height="{Y1 - Y0}" rx="14" fill="{p["human"]}" '
        f'fill-opacity="{p["humansoftop"]}" stroke="{p["humanline"]}" stroke-width="2" opacity="1"/>')
    # host identity (constant)
    add(mark(48, 40, 48, p["agent"], p["human"]))
    add(t(104, 66, c["host"], 28, p["ink"], weight=600, cls="s"))
    chip_x = 104 + text_w(c["host"], 28) * 0.9 + 16
    add(f'<rect x="{chip_x:.0f}" y="41" width="100" height="32" rx="16" fill="none" stroke="{p["line2"]}" stroke-width="2"/>')
    add(t(chip_x + 50, 64, c["level"], 20, p["muted"], anchor="middle", cls="s"))
    add(f'<g transform="translate(106 92)" stroke="{p["muted"]}" stroke-width="2.5" fill="none">'
        f'<circle cx="0" cy="0" r="5.5"/><circle cx="30" cy="0" r="5.5"/><path d="M5.5,0 H24.5"/></g>')
    add(t(150, 100, c["hostline"], 21, p["muted"]))
    add(f'<path d="M536,42 V102" stroke="{p["line"]}" stroke-width="2"/>')
    MX = 616

    def state(title, sub, icon):
        s = f'<g transform="translate({MX - 22 if icon[1] else MX - 46} 50)">{cursor(42, icon[0], mirror=icon[1])}</g>'
        s += t(MX + 6, 66, title, 27, p["ink"], weight=600)
        s += t(MX + 6, 100, sub, 22, p["muted"])
        return s

    def bar_button(button, button_stroke, pressed_spans):
        s = ""
        if button:
            s += (f'<rect x="{BTN[0]}" y="{BTN[1]}" width="{BTN[2]}" height="{BTN[3]}" rx="10" fill="{p["panel2"]}" '
                  f'stroke="{button_stroke}" stroke-width="2.5"/>')
            if pressed_spans:
                k = A.fade(pressed_spans, ramp=0.08)
                s += f'<rect class="{k}" x="{BTN[0]}" y="{BTN[1]}" width="{BTN[2]}" height="{BTN[3]}" rx="10" fill="{p["btnpress"]}" opacity="0"/>'
            s += t(BTN[0] + BTN[2] / 2, BTN[1] + 39, button, 25, p["ink"], weight=600, anchor="middle")
        return s

    ka = A.opacity([(0, 1), (4.35, 1), (4.47, 0), (6.95, 0), (7.1, 1), (9.15, 1), (9.27, 0), (12.7, 0), (12.85, 1)])
    kq = A.opacity([(0, 0), (4.5, 0), (4.65, 1), (6.75, 1), (6.87, 0)])
    kh = A.opacity([(0, 0), (9.3, 0), (9.45, 1), (12.5, 1), (12.62, 0)])
    add(f'<g class="{ka}" opacity="0">{state(c["a_t"], c["a_s"], (p["agent"], True))}</g>')
    add(f'<g class="{kq}" opacity="1">{state(c["q_t"], c["q_s"], (p["human"], False))}</g>')
    add(f'<g class="{kh}" opacity="0">{state(c["h_t"], c["h_s"], (p["human"], False))}</g>')
    kb1 = A.opacity([(0, 1), (4.35, 1), (4.47, 0), (6.95, 0), (7.1, 1), (9.15, 1), (9.27, 0), (13.05, 0), (13.25, 1)])
    kb2 = A.opacity([(0, 0), (9.3, 0), (9.45, 1), (12.5, 1), (12.62, 0)])
    add(f'<g class="{kb1}" opacity="0">{bar_button(c["takeover"], p["humanline"], [(9.1, 9.25)])}</g>')
    add(f'<g class="{kb2}" opacity="0">{bar_button(c["handback"], p["agent"], [(12.45, 12.6)])}</g>')
    TAKE = (BTN[0] + BTN[2] - 28, BTN[1] + 46)

    # ------------------------------------------------------------------ your pointer
    OFF = (1330, 1150)
    IDLE = (990, 760)
    CELL = (588, TY0 + 112)
    hpath = [(0, OFF), (5.1, OFF), (6.3, APPROVE), (7.2, APPROVE), (7.9, IDLE), (8.3, IDLE), (9.05, TAKE), (9.6, TAKE),
             (10.5, CELL), (11.5, CELL), (12.4, TAKE), (12.6, TAKE), (13.2, (TAKE[0] - 140, TAKE[1] + 240)), (13.21, OFF)]
    hvis = [(0, 0), (5.05, 0), (5.3, 1), (12.75, 1), (13.0, 0)]
    add(pointer(p["human"], p["onhuman"], False, [(c["tag_h"], None, p["human"], p["onhuman"])],
                hpath, [6.45, 9.1, 10.6, 10.76, 12.45], hvis, APPROVE, 1))

    css = "".join(A.css) + "@media (prefers-reduced-motion: reduce){*{animation:none!important}}"
    suffix = "" if lang == "en" else ".zh-CN"
    classes = ("s", "m") if lang == "en" else ("s", "m", "z")
    svg = svg_doc(W, H, c["title"], "".join(o), css=css, classes=classes)
    path = os.path.join(ASSETS, f"demo-{theme}{suffix}.svg")
    with open(path, "w", encoding="utf-8") as f:
        f.write(svg)
    return path


def build_all():
    out = []
    for lang in ("en", "zh"):
        for theme in ("dark", "light"):
            out.append(build(theme, lang))
    return out


if __name__ == "__main__":
    for f in build_all():
        print(f, os.path.getsize(f))
