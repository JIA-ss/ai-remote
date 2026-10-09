"""Logo and hero banner.

Banner: the mark on top, the wordmark under it, then the pitch in two lines, each led by its operator's
pointer (cobalt = the agent, amber = you), so the hero is also the legend for the demo below it.
Two quiet trails arrive at the mark: the agent's moves in straight hops between element boxes (it acts
on structure), yours is a freehand curve.
"""
import os
from common import PAL, mark, cursor, wordmark, wordmark_width, svg_doc, Text, XH

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ASSETS = os.path.join(ROOT, "assets", "brand")

COPY = {
    "en": dict(
        agent="AI agents operate your Windows and Mac remotely.",
        you="Risky steps wait for you. Take over at any time.",
        title="ai-remote: AI agents operate your Windows and Mac remotely. Risky steps wait for you. Take over at any time.",
        desc="Two pointers meet at one point: the agent's, in blue, and yours, in amber.",
        text_w=1505,   # widest line at 64 units, measured in Chrome with Segoe UI (Windows)
    ),
    "zh": dict(
        agent="让 AI Agent 远程操作你的 Windows 和 Mac。",
        you="高风险操作先问你，你随时可以接管。",
        title="ai-remote：让 AI Agent 远程操作你的 Windows 和 Mac。高风险操作先问你，你随时可以接管。",
        desc="两个指针指向同一点：蓝色是 Agent，琥珀色是你。",
        # widest line at 64 units, measured in Chrome with Microsoft YaHei (Windows): 1392 advance, minus the
        # empty right part of the closing full-width period, so the ink is what gets centred
        text_w=1350,
    ),
}


def write(name, svg):
    p = os.path.join(ASSETS, name)
    with open(p, "w", encoding="utf-8") as f:
        f.write(svg)
    return p


def logos():
    out = []
    variants = {
        "logo.svg": ("#4F80F2", "#EBA52C"),
        "logo-dark.svg": (PAL["dark"]["agent"], PAL["dark"]["human"]),
        "logo-light.svg": (PAL["light"]["agent"], "#E8A024"),
    }
    for name, (a, h) in variants.items():
        out.append(write(name, svg_doc(256, 256, "ai-remote logo", mark(256, 0, 0, a, h),
                                       desc="Two cursors meeting tip to tip: the agent's pointer and yours.", classes=())))
    return out


def trails(p, cx, top, sc, g):
    """Motion trails ending just short of each cursor's tail."""
    # tail ends of the two cursors in the mark (see CURSOR geometry)
    ax, ay = cx - g - 0.41 * sc, top + 0.87 * sc   # agent (mirrored, left)
    hx, hy = cx + g + 0.41 * sc, top + 0.87 * sc   # you (right)
    o = []
    # gradient strokes fade the trails out toward the edges
    o.append('<defs>'
             f'<linearGradient id="ta" gradientUnits="userSpaceOnUse" x1="120" y1="0" x2="{ax - 30:.0f}" y2="0">'
             f'<stop offset="0" stop-color="{p["agent"]}" stop-opacity="0"/><stop offset="1" stop-color="{p["agent"]}" stop-opacity=".85"/></linearGradient>'
             f'<linearGradient id="th" gradientUnits="userSpaceOnUse" x1="1640" y1="0" x2="{hx + 30:.0f}" y2="0">'
             f'<stop offset="0" stop-color="{p["human"]}" stop-opacity="0"/><stop offset="1" stop-color="{p["human"]}" stop-opacity=".9"/></linearGradient>'
             '</defs>')
    # agent: orthogonal hops that touch two element boxes on the way
    ex = ax - 26
    ey = ay + 22
    pts = [(150, 226), (330, 226), (330, 132), (560, 132), (560, ey), (ex, ey)]
    d = "M" + " L".join(f"{x:.0f},{y:.0f}" for x, y in pts)
    o.append(f'<path d="{d}" fill="none" stroke="url(#ta)" stroke-width="3" stroke-linejoin="round"/>')
    for (bx, by, bw, bh, op) in [(300, 196, 60, 60, 0.35), (530, 102, 60, 60, 0.6)]:
        o.append(f'<rect x="{bx}" y="{by}" width="{bw}" height="{bh}" rx="8" fill="none" stroke="{p["agent"]}" '
                 f'stroke-opacity="{op}" stroke-width="2.5" stroke-dasharray="7 6"/>')
    o.append(f'<circle cx="{ex:.0f}" cy="{ey:.0f}" r="5" fill="{p["agent"]}"/>')
    # you: one freehand curve
    sx, sy = hx + 26, hy + 22
    o.append(f'<path d="M1620,250 C1500,320 1370,90 1250,150 S1090,{sy + 40:.0f} {sx:.0f},{sy:.0f}" fill="none" '
             f'stroke="url(#th)" stroke-width="3" stroke-linecap="round"/>')
    o.append(f'<circle cx="{sx:.0f}" cy="{sy:.0f}" r="5" fill="{p["human"]}"/>')
    return "".join(o)


def banner(theme, lang, with_trails=True):
    p = PAL[theme]
    c = COPY[lang]
    T = Text(lang)
    W, H = 1760, 626
    cx = W / 2
    msize = 200
    mtop = 24
    sc = msize * 0.735
    g = 0.028 * msize
    ctop = mtop + (msize - 0.9 * sc) / 2
    o = []
    if with_trails:
        o.append(trails(p, cx, ctop, sc, g))
    o.append(mark(msize, cx - msize / 2, mtop, p["agent"], p["human"]))
    ws = 1.76
    ww = wordmark_width(scale=ws)
    base = 372
    o.append(wordmark("ai-remote", cx - ww / 2, base, ws, p["ink"])[0])
    # Tagline: 64 units (about 32 px at 880, the size that still survives a phone column). The block, pointer
    # plus text, is centred on the wordmark's axis using the measured width of its widest line.
    fs = 64
    hs = 54                      # pointer height 0.9 * hs, centred on the cap height
    icon_l, gap = 3.6, 78        # pointer's left edge and the text's x, relative to the block origin
    bx = cx - (icon_l + gap + c["text_w"]) / 2
    l1, l2 = base + 118, base + 210
    tx = bx + gap
    o.append(f'<g transform="translate({bx + 36:.1f} {l1 - 47})">{cursor(hs, p["agent"], True)}</g>')
    wt = 500 if lang == "en" else 600
    o.append(T(round(tx, 1), l1, c["agent"], fs, p["ink"], weight=wt))
    o.append(f'<g transform="translate({bx + 4:.1f} {l2 - 47})">{cursor(hs, p["human"])}</g>')
    o.append(T(round(tx, 1), l2, c["you"], fs, p["ink"], weight=wt))
    suffix = "" if lang == "en" else ".zh-CN"
    cls = ("s",) if lang == "en" else ("z",)
    return write(f"banner-{theme}{suffix}.svg", svg_doc(W, H, c["title"], "".join(o), desc=c["desc"], classes=cls))


def build():
    out = logos()
    for lang in ("en", "zh"):
        for theme in ("dark", "light"):
            out.append(banner(theme, lang))
    return out


if __name__ == "__main__":
    for f in build():
        print(f, os.path.getsize(f))
