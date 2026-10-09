"""Shared palette, type, cursor geometry, mark and wordmark for the ai-remote brand assets.

Colour rule used by every asset: cobalt = the agent, amber = you (the owner). Neutrals are the machine.
"""
import math
import xml.etree.ElementTree as ET

# Font stacks are declared once per file as CSS classes (see svg_doc); <text> elements use class="s|m|z".
SANS = "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'Helvetica Neue', Arial, sans-serif"
MONO = "ui-monospace, 'SF Mono', Menlo, Consolas, 'Liberation Mono', monospace"
CJK = "'PingFang SC', 'Microsoft YaHei', 'Noto Sans CJK SC', sans-serif"
CLASSES = {"s": SANS, "m": MONO, "z": CJK}

PAL = {
    "dark": dict(
        page="#0d1117", ink="#E6EDF3", muted="#9198A1", faint="#6E7681", line="#30363D", line2="#3D444D",
        panel="#11161D", panel2="#161C24", desk="#1A212B", win="#0E1217", winbar="#151B23", btn="#1C232D", btnpress="#2A3442",
        btnline="#38414D", doc="#2A323D", docstrong="#3A4452", rule="#262D37",
        agent="#6F9BFF", onagent="#08122B", agentsoftop="0.14", agenttext="#8DB0FF", badge="#22345C", onbadge="#C9D8FF",
        human="#F3B53F", onhuman="#1D1405", humantext="#F3B53F", humansoftop="0.16", humanline="#F3B53F", humanpress="#C98F1E",
        scrim="#010409", scrimop="0.6", outline="#0d1117", ptrline="#0d1117", ptrw="5", grey="#8B949E", greypill="#8B949E", ongrey="#0d1117",
    ),
    "light": dict(
        page="#ffffff", ink="#1F2328", muted="#59636E", faint="#818B98", line="#D1D9E0", line2="#C3CCD5",
        panel="#F6F8FA", panel2="#FFFFFF", desk="#E7ECF1", win="#FFFFFF", winbar="#F3F5F8", btn="#F6F8FA", btnpress="#DCE3EA",
        btnline="#C9D1DA", doc="#E3E8EE", docstrong="#C4CDD7", rule="#E1E6EB",
        agent="#2F5BD3", onagent="#FFFFFF", agentsoftop="0.10", agenttext="#2F5BD3", badge="#DCE5FB", onbadge="#2F5BD3",
        human="#F0A62B", onhuman="#241703", humantext="#8F5400", humansoftop="0.18", humanline="#C17C00", humanpress="#D18A12",
        scrim="#1F2328", scrimop="0.34", outline="#ffffff", ptrline="#1F2328", ptrw="3.5", grey="#8C959F", greypill="#D0D7DE", ongrey="#1F2328",
    ),
}

# Classic arrow cursor, tip at origin, unit height (stem bottom ~0.90).
CURSOR = [(0, 0), (0, 0.78), (0.20, 0.60), (0.335, 0.90), (0.49, 0.83), (0.355, 0.535), (0.60, 0.535)]
WING = math.atan2(0.535, 0.60)  # angle of the cursor's wing edge, ~41.7 degrees


def cursor_points(scale, mirror=False, ox=0.0, oy=0.0):
    pts = []
    for x, y in CURSOR:
        if mirror:
            x = -x
        pts.append(f"{ox + x * scale:.1f},{oy + y * scale:.1f}")
    return " ".join(pts)


def cursor(scale, fill, mirror=False, ox=0.0, oy=0.0, outline=None, ow=0.0, extra=""):
    """A cursor polygon. outline draws a rim behind it (paint-order stroke)."""
    if outline:
        return (f'<polygon points="{cursor_points(scale, mirror, ox, oy)}" fill="{fill}" stroke="{outline}" '
                f'stroke-width="{ow:.1f}" stroke-linejoin="round" paint-order="stroke" {extra}/>')
    return (f'<polygon points="{cursor_points(scale, mirror, ox, oy)}" fill="{fill}" stroke="{fill}" '
            f'stroke-width="{scale * 0.022:.1f}" stroke-linejoin="round" {extra}/>')


def mark(size, x, y, agent, human, gap=0.028):
    """Two mirrored cursors whose tips meet at one point: the agent's (mirrored, left) and yours (right).
    Fits a size x size square with (x, y) as its top-left corner."""
    sc = size * 0.735
    h = 0.90 * sc
    g = gap * size
    cx = x + size / 2
    top = y + (size - h) / 2
    return cursor(sc, agent, True, cx - g, top) + cursor(sc, human, False, cx + g, top)


# ---------------------------------------------------------------- wordmark
# Monoline geometric lowercase. Bowls are stroked arcs; stems are filled shapes whose tops are cut at the
# cursor's wing angle, so every stem starts in the same pointed corner as the arrow in the mark.
XH = 60.0
SW = 13.0
R = (XH - SW) / 2
CY = -XH / 2
H2 = SW / 2


def _stem(x, top, bottom=0.0, cut=True):
    d = SW * math.tan(WING) * 0.62 if cut else 0.0
    return f'<polygon points="{x - H2:.2f},{top:.2f} {x + H2:.2f},{top + d:.2f} {x + H2:.2f},{bottom:.2f} {x - H2:.2f},{bottom:.2f}"/>'


def _circle(cx):
    return f"M{cx - R:.2f},{CY} a{R},{R} 0 1,0 {2 * R:.2f},0 a{R},{R} 0 1,0 {-2 * R:.2f},0"


def glyph(ch):
    """Return (stroked_path_d, filled_svg, advance) with the glyph's left edge at x=0."""
    h = H2
    if ch == "a":
        cx = h + R
        stem = cx + R
        return _circle(cx), _stem(stem, -XH), stem + h
    if ch == "i":
        return "", _stem(h, -XH) + f'<rect x="0" y="{-XH - SW - 9:.2f}" width="{SW}" height="{SW}"/>', SW
    if ch == "-":
        return f"M0,{CY + 2} H24", "", 24
    if ch == "r":
        return (f"M{h},{CY + 4} A{R},{R - 2} 0 0,1 {h + R},{-XH + h} H{h + R + 9}", _stem(h, -XH), h + R + 9)
    if ch == "e":
        cx = h + R
        a = math.radians(42)
        ex, ey = cx + R * math.cos(a), CY + R * math.sin(a)
        return f"M{cx - R:.2f},{CY} H{cx + R:.2f} A{R},{R} 0 1,0 {ex:.2f},{ey:.2f}", "", cx + R + h
    if ch == "o":
        cx = h + R
        return _circle(cx), "", cx + R + h
    if ch == "m":
        rx = 19.5
        x0 = h
        x1 = x0 + 2 * rx
        x2 = x1 + 2 * rx
        return (f"M{x0},{CY} A{rx},{R} 0 0,1 {x1},{CY} V0 M{x1},{CY} A{rx},{R} 0 0,1 {x2},{CY} V0",
                _stem(x0, -XH), x2 + h)
    if ch == "t":
        x0 = h + 9
        rr = 17
        return (f"M{x0},{-h - rr} A{rr},{rr} 0 0,0 {x0 + rr},{-h} H{x0 + rr + 9} M0,{-XH + h} H{x0 + 21}",
                _stem(x0, -XH - 20, -h - rr + 0.6, cut=False), x0 + rr + 9)
    raise KeyError(ch)


KERN = {("a", "i"): 9, ("i", "-"): 9, ("-", "r"): 9, ("r", "e"): 6, ("e", "m"): 9, ("m", "o"): 9, ("o", "t"): 7, ("t", "e"): 6}


def wordmark_width(text="ai-remote", scale=1.0):
    cx, prev = 0.0, None
    for ch in text:
        if prev:
            cx += KERN.get((prev, ch), 9)
        cx += glyph(ch)[2]
        prev = ch
    return cx * scale


def wordmark(text, x, y, scale, ink):
    """Draw the wordmark with its baseline at y. Returns (svg, width)."""
    strokes, fills = [], []
    cx, prev = 0.0, None
    for ch in text:
        if prev:
            cx += KERN.get((prev, ch), 9)
        d, f, adv = glyph(ch)
        if d:
            strokes.append(f'<path transform="translate({cx:.2f} 0)" d="{d}"/>')
        if f:
            fills.append(f'<g transform="translate({cx:.2f} 0)">{f}</g>')
        cx += adv
        prev = ch
    svg = (f'<g transform="translate({x:.2f} {y:.2f}) scale({scale:.4f})">'
           f'<g fill="none" stroke="{ink}" stroke-width="{SW}" stroke-linecap="butt" stroke-linejoin="miter">{"".join(strokes)}</g>'
           f'<g fill="{ink}">{"".join(fills)}</g></g>')
    return svg, cx * scale


# ---------------------------------------------------------------- document helpers
def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


def svg_doc(w, h, title, body, css="", desc="", classes=("s", "m", "z")):
    style = "".join(f".{c}{{font-family:{CLASSES[c]}}}" for c in classes) + css
    d = f"<desc>{esc(desc)}</desc>" if desc else ""
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" role="img">'
            f"<title>{esc(title)}</title>{d}<style>{style}</style>{body}</svg>\n")


def check_xml(path):
    ET.parse(path)


class Text:
    """Small helper that emits <text> with a font class. lang='zh' switches sans text to the CJK stack."""

    def __init__(self, lang):
        self.lang = lang

    def __call__(self, x, y, s, size, fill, weight=None, anchor=None, mono=False, cls=None, extra=""):
        c = cls or ("m" if mono else ("z" if self.lang == "zh" else "s"))
        wv = f' font-weight="{weight}"' if weight else ""
        an = f' text-anchor="{anchor}"' if anchor else ""
        return (f'<text class="{c}" x="{x:g}" y="{y:g}" font-size="{size:g}"{wv}{an} fill="{fill}"{extra}>{esc(s)}</text>')
