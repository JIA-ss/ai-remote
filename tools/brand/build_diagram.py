"""How-it-works diagram: agent -> ai-remote mcp -> peer-to-peer link -> hosts, with you in charge."""
import os
from common import PAL, cursor, svg_doc, Text

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ASSETS = os.path.join(ROOT, "assets", "brand")
W, H = 1760, 600

COPY = {
    "en": dict(
        title="How ai-remote works: your AI agent talks MCP over stdio to ai-remote mcp on your computer. It reaches each "
              "paired host over a direct QUIC link dialed by public key, with an encrypted relay as fallback. The host needs "
              "no open ports. You set each device's level, approve risky steps, and can take over or revoke at any time.",
        h_agent="Agent", h_ctrl="Your computer", h_host="Machines you control",
        agent_t="AI agent", agent_1="Claude Code, Cursor,", agent_2="or your own",
        mcp_1="MCP", mcp_2="stdio",
        ctrl_t="ai-remote mcp", ctrl_1="Keeps your pairings", ctrl_2="and device keys",
        link_t="Direct QUIC link", link_1="dialed by public key,", link_2="punches through NAT",
        relay_t="Relay", relay_1="fallback only, sees ciphertext",
        host1="office-pc", host1_s="Windows, runs ai-remote host, no open ports", host1_fs=23,
        host2="studio-mac", host2_s="macOS, planned for M1",
        tools=["screenshot", "UI tree", "elements", "mouse, keys", "windows", "shell"],
        you="You set each device's level, approve risky steps, read the audit log, and can take over or revoke at any time.",
    ),
    "zh": dict(
        title="ai-remote 的工作方式：AI Agent 通过 stdio 上的 MCP 连接你电脑上的 ai-remote mcp，再按公钥拨号，经 QUIC 直连到达每台"
              "已配对的被控端，加密中继只作兜底，被控端不用开放任何端口。你为每台设备设定权限，审批高风险操作，随时可以接管或撤销。",
        h_agent="Agent", h_ctrl="你的电脑", h_host="你控制的机器",
        agent_t="AI Agent", agent_1="Claude Code、Cursor", agent_2="或你自己写的 Agent",
        mcp_1="MCP", mcp_2="stdio",
        ctrl_t="ai-remote mcp", ctrl_1="保存配对关系", ctrl_2="和设备密钥",
        link_t="QUIC 直连", link_1="按公钥拨号", link_2="自动穿透 NAT",
        relay_t="中继", relay_1="只作兜底，只能看到密文",
        host1="office-pc", host1_s="Windows，运行 ai-remote host，无需开放端口", host1_fs=22,
        host2="studio-mac", host2_s="macOS，计划在 M1 支持",
        tools=["截图", "界面树", "元素操作", "鼠标键盘", "窗口", "shell"],
        you="你为每台设备设定权限级别，审批高风险操作，查看审计日志，随时可以接管或撤销。",
    ),
}


def build(theme, lang):
    p = PAL[theme]
    c = COPY[lang]
    t = Text(lang)
    o = []
    add = o.append

    def box(x, y, w, h, stroke, dash=None, fill=None):
        d = f' stroke-dasharray="{dash}"' if dash else ""
        add(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="14" fill="{fill or p["panel2"]}" stroke="{stroke}" stroke-width="2"{d}/>')

    add(t(36, 52, c["h_agent"], 24, p["muted"]))
    add(t(500, 52, c["h_ctrl"], 24, p["muted"]))
    add(t(1180, 52, c["h_host"], 24, p["muted"]))

    # agent
    box(30, 80, 356, 220, p["agent"])
    add(f'<g transform="translate(100 124)">{cursor(46, p["agent"], mirror=True)}</g>')
    add(t(134, 158, c["agent_t"], 30, p["ink"], weight=600))
    add(t(70, 216, c["agent_1"], 25, p["muted"]))
    add(t(70, 252, c["agent_2"], 25, p["muted"]))

    add(f'<path d="M392,190 H486" stroke="{p["muted"]}" stroke-width="2.5"/>'
        f'<path d="M478,181 L490,190 L478,199" fill="none" stroke="{p["muted"]}" stroke-width="2.5" stroke-linejoin="round"/>')
    add(t(440, 168, c["mcp_1"], 24, p["muted"], anchor="middle", cls="s"))
    add(t(440, 226, c["mcp_2"], 24, p["muted"], anchor="middle", mono=True))

    # controller
    box(496, 80, 366, 220, p["line2"])
    add(t(536, 158, c["ctrl_t"], 28, p["ink"], weight=700, mono=True))
    add(t(536, 216, c["ctrl_1"], 25, p["muted"]))
    add(t(536, 252, c["ctrl_2"], 25, p["muted"]))

    # direct link: a double rail = encrypted tunnel
    add(f'<path d="M868,184 H1174 M868,196 H1174" stroke="{p["ink"]}" stroke-width="3"/>'
        f'<circle cx="868" cy="190" r="9" fill="{p["page"]}" stroke="{p["ink"]}" stroke-width="3"/>'
        f'<circle cx="1174" cy="190" r="9" fill="{p["page"]}" stroke="{p["ink"]}" stroke-width="3"/>')
    add(t(1021, 160, c["link_t"], 26, p["ink"], weight=600, anchor="middle"))
    add(t(1021, 236, c["link_1"], 24, p["muted"], anchor="middle"))
    add(t(1021, 266, c["link_2"], 24, p["muted"], anchor="middle"))

    # hosts: a stack of paired machines; the planned macOS host peeks out behind
    hx, hw = 1180, 550
    add(f'<rect x="{hx + 26}" y="120" width="{hw - 26}" height="292" rx="14" fill="{p["panel"]}" stroke="{p["line2"]}" stroke-width="2" stroke-dasharray="10 8"/>')
    add(t(hx + 62, 394, c["host2"], 25, p["muted"], weight=600, cls="s"))
    add(t(hx + 212, 394, c["host2_s"], 23, p["muted"]))
    box(hx, 80, hw, 262, p["line2"])
    add(t(hx + 36, 134, c["host1"], 28, p["ink"], weight=600, cls="s"))
    add(t(hx + 36, 170, c["host1_s"], c["host1_fs"], p["muted"]))
    cw, ch = 154, 44
    for i, s in enumerate(c["tools"]):
        col, row = i % 3, i // 3
        x = hx + 36 + col * (cw + 12)
        y = 200 + row * (ch + 12)
        add(f'<rect x="{x}" y="{y}" width="{cw}" height="{ch}" rx="8" fill="{p["panel"]}" stroke="{p["line"]}" stroke-width="1.5"/>')
        add(t(x + cw / 2, y + 30, s, 23, p["ink"], anchor="middle"))

    # relay fallback (dashed), under the direct link: it leaves the controller's bottom edge and lands on
    # office-pc's left edge, mirroring each other at y = 300
    rx, ry, rw = 940, 316, 164
    kx = (rx + rw + hx) / 2
    add(f'<path d="M760,300 V{ry + 34} H{rx}" fill="none" stroke="{p["muted"]}" stroke-width="2.5" stroke-dasharray="9 8"/>'
        f'<path d="M{rx + rw},{ry + 34} H{kx:.0f} V300 H{hx}" fill="none" stroke="{p["muted"]}" stroke-width="2.5" '
        f'stroke-dasharray="9 8" stroke-linejoin="round"/>')
    box(rx, ry, rw, 68, p["line2"], fill=p["panel"])
    add(t(rx + rw / 2, ry + 43, c["relay_t"], 24, p["ink"], anchor="middle", weight=600))
    add(t(rx + rw / 2, ry + 110, c["relay_1"], 24, p["muted"], anchor="middle"))

    # you
    yy = 490
    add(f'<rect x="30" y="{yy}" width="{W - 60}" height="84" rx="14" fill="{p["human"]}" fill-opacity="{p["humansoftop"]}" '
        f'stroke="{p["humanline"]}" stroke-width="2"/>')
    add(f'<g transform="translate(62 {yy + 22})">{cursor(42, p["human"])}</g>')
    add(t(110, yy + 52, c["you"], 27, p["ink"]))
    # no connector: the band spans the whole diagram because it applies to every paired machine

    suffix = "" if lang == "en" else ".zh-CN"
    classes = ("s", "m") if lang == "en" else ("s", "m", "z")
    svg = svg_doc(W, H, c["title"], "".join(o), classes=classes)
    path = os.path.join(ASSETS, f"how-it-works-{theme}{suffix}.svg")
    with open(path, "w", encoding="utf-8") as f:
        f.write(svg)
    return path


def build_all():
    return [build(theme, lang) for lang in ("en", "zh") for theme in ("dark", "light")]


if __name__ == "__main__":
    for f in build_all():
        print(f, os.path.getsize(f))
