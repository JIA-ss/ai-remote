#!/usr/bin/env python3
"""Objective quality of the host's hardware encoders: scores the streams that the ignored test
`encoded_quality` writes to target/stream-quality/ against their source with ffmpeg (PSNR, SSIM,
VMAF), and prints one row per case.

  cargo test -p ar-host --release --lib encoded_quality -- --ignored --nocapture   # AR_RC=both adds VBR
  python tools/stream-quality.py [--dir target/stream-quality] [--no-vmaf]

The reference is rebuilt from the saved source picture exactly as the test scrolled it (frame n
shows the rows from scroll_top(7 n) of a picture twice the frame's height) and converted to
4:2:0 BT.709 studio range, which is what the host's GPU converter produces; its own conversion
error (a different scaler for chroma) is in every row alike, so compare rows with each other.
"""
import argparse
import json
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--dir", type=Path, default=ROOT / "target/stream-quality")
parser.add_argument("--ffmpeg", default="ffmpeg")
parser.add_argument("--no-vmaf", action="store_true", help="PSNR and SSIM only (VMAF at 4K takes a while)")
options = parser.parse_args()

cases = json.loads((options.dir / "cases.json").read_text(encoding="utf-8"))
FORMATS = {"h264": "h264", "hevc": "hevc", "av1": "obu"}
results = []
for case in cases:
    w, h, fps, n = case["width"], case["height"], case["fps"], case["outputs"]
    rows = case["scroll_rows"]
    # The test's scroll_top: a triangle wave over the picture's spare height (ih/2 = h).
    top = f"if(lte(mod({rows}*n\\,ih)\\,ih/2)\\,mod({rows}*n\\,ih)\\,ih-mod({rows}*n\\,ih))"
    reference = (f"[1:v]crop={w}:{h}:0:'{top}',scale=out_color_matrix=bt709:out_range=tv:flags=bicubic,"
                 f"format=yuv420p,setpts=N/({fps}*TB)[ref]")
    distorted = f"[0:v]format=yuv420p,setpts=N/({fps}*TB)[dist]"
    metrics = "[d1][r1]psnr;[d2][r2]ssim"
    if not options.no_vmaf:
        metrics += ";[d3][r3]libvmaf=n_threads=16"
    split = 2 if options.no_vmaf else 3
    graph = (f"{distorted};{reference};[dist]split={split}{''.join(f'[d{i}]' for i in range(1, split + 1))};"
             f"[ref]split={split}{''.join(f'[r{i}]' for i in range(1, split + 1))};{metrics}")
    cmd = [options.ffmpeg, "-hide_banner", "-nostats",
           "-f", FORMATS[case["codec"]], "-i", str(options.dir / case["file"]),
           "-stream_loop", "-1", "-f", "rawvideo", "-pix_fmt", "bgra", "-s", f"{w}x{h * 2}", "-framerate", str(fps),
           "-i", str(options.dir / case["reference"]),
           "-filter_complex", graph, "-frames:v", str(n), "-f", "null", "-"]
    p = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    log = p.stderr
    psnr = re.search(r"PSNR y:([\d.inf]+) u:([\d.inf]+) v:([\d.inf]+) average:([\d.inf]+)", log)
    ssim = re.search(r"SSIM Y:([\d.]+).*All:([\d.]+)", log)
    vmaf = re.search(r"VMAF score: ([\d.]+)", log)
    if p.returncode != 0 or not psnr:
        print(f"{case['file']}: ffmpeg failed\n{log[-1500:]}")
        continue
    r = {**case, "psnr_y": float(psnr[1]), "psnr_avg": float(psnr[4]), "ssim_y": float(ssim[1]) if ssim else None,
         "vmaf": float(vmaf[1]) if vmaf else None}
    results.append(r)
    print(f"{w}x{h} {case['codec']:4} {case['rate_control']} {case['kbps'] / 1000:5.0f} Mbit/s requested, "
          f"{case['delta_mbps']:6.1f} produced | PSNR-Y {r['psnr_y']:6.2f} dB  avg {r['psnr_avg']:6.2f}  "
          f"SSIM-Y {r['ssim_y'] or 0:.4f}  VMAF {r['vmaf'] if r['vmaf'] is not None else '-'}"
          + ("" if case["outputs"] == case["frames"] else f"  ({case['outputs']}/{case['frames']} frames)"))
(options.dir / "scores.json").write_text(json.dumps(results, indent=2), encoding="utf-8")
print(f"scores: {options.dir / 'scores.json'}")
