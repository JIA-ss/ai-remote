#!/usr/bin/env python3
"""Run the twelve fake-agent Windows desktop checks and export only safe UI crops/typed evidence.

Raw logs, screenshots and temporary profiles stay under --private-root, never under --out.
A failed first round remains a failure even when its separately planned second round passes.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
SUITES = (
    ("chat", "desktop-chat-e2e.js", 300),
    ("render", "desktop-chat-e2e-render.js", 300),
    ("offline", "desktop-chat-offline-e2e.js", 300),
    ("preview", "desktop-chat-preview-e2e.js", 300),
    ("codex", "desktop-chat-codex-e2e.js", 360),
    ("workspace", "desktop-workspace-e2e.js", 600),
)
KIND_ALLOWLIST = {"stream_chat_rail", "preview_dialog", "workspace_ui", "devices_ui"}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def offscreen(geometry):
    if not isinstance(geometry, dict):
        return False
    window, screens = geometry.get("window"), geometry.get("screens")
    if not isinstance(window, dict) or not isinstance(screens, list) or not screens:
        return False
    for rect in [window, *screens]:
        if not isinstance(rect, dict) or any(type(rect.get(key)) is not int for key in ("x", "y", "width", "height")):
            return False
        if rect["width"] <= 0 or rect["height"] <= 0:
            return False
    return all(window["x"] + window["width"] <= screen["x"]
               or screen["x"] + screen["width"] <= window["x"]
               or window["y"] + window["height"] <= screen["y"]
               or screen["y"] + screen["height"] <= window["y"] for screen in screens)


def static_names(source):
    # Public source literals only. Dynamic names get a generic ordinal, never their raw detail.
    names = []
    for match in re.finditer(r'\bcheck\(\s*("(?:\\.|[^"\\])*")', source):
        try:
            names.append(json.loads(match[1]))
        except ValueError:
            pass
    return sorted(set(names), key=len, reverse=True)


def parse_log(raw, source):
    names = static_names(source)
    checks, exits, geometries = [], [], []
    all_passed, failed_summaries, reported_checks, storage = 0, [], [], False
    for line in raw.splitlines():
        passed = line.startswith("ok  ")
        if passed or line.startswith("FAIL"):
            body = line[4:].lstrip()
            label = next((name for name in names if body == name or body.startswith(name + "  (")), None)
            checks.append({"index": len(checks) + 1, "passed": passed,
                           "name": label if label is not None else "dynamic check"})
        if line == "all passed":
            all_passed += 1
        if re.fullmatch(r"\d+ failed", line):
            failed_summaries.append(int(line.split()[0]))
        if re.fullmatch(r"\d+ checks", line):
            reported_checks.append(int(line.split()[0]))
        match = re.fullmatch(r"\s*app exited with (-?\d+)\s*", line)
        if match:
            exits.append(int(match[1]))
        if line.strip() == "[smoke] isolated nonpersistent webview storage":
            storage = True
        if line.strip().startswith("[smoke] geometry "):
            try:
                geometries.append(json.loads(line.strip()[len("[smoke] geometry "):]))
            except ValueError:
                geometries.append(None)
    return {"checks": checks, "ok_count": sum(x["passed"] for x in checks),
            "fail_count": sum(not x["passed"] for x in checks), "all_passed_count": all_passed,
            "failed_summary_counts": failed_summaries, "reported_check_counts": reported_checks, "app_exit_codes": exits,
            "isolated_storage": storage, "isolated_placement": bool(geometries) and all(offscreen(x) for x in geometries)}


def crop_box(layout, native, size):
    """Map an observed safe DOM rectangle through the actual Win32 client origin; fail closed."""
    try:
        if layout["kind"] not in KIND_ALLOWLIST or list(size) != native["original_size"]:
            return None
        left, top, right, bottom = native["client_box"]
        if not (0 <= left < right <= size[0] and 0 <= top < bottom <= size[1]):
            return None
        viewport, rect = layout["viewport"], layout["rect"]
        ratio = viewport["dpr"]
        values = [ratio, viewport["width"], viewport["height"], *[rect[k] for k in ("x", "y", "width", "height")]]
        if not all(isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v) for v in values) or ratio <= 0:
            return None
        if abs(viewport["width"] * ratio - (right - left)) > 2 or abs(viewport["height"] * ratio - (bottom - top)) > 2:
            return None
        if not (rect["width"] > 0 and rect["height"] > 0 and rect["x"] >= 0 and rect["y"] >= 0
                and rect["x"] + rect["width"] <= viewport["width"] + 0.01
                and rect["y"] + rect["height"] <= viewport["height"] + 0.01):
            return None
        box = [left + math.ceil(rect["x"] * ratio), top + math.ceil(rect["y"] * ratio),
               left + math.floor((rect["x"] + rect["width"]) * ratio), top + math.floor((rect["y"] + rect["height"]) * ratio)]
        if not (left <= box[0] < box[2] <= right and top <= box[1] < box[3] <= bottom):
            return None
        return box
    except (KeyError, TypeError, ValueError, OverflowError):
        return None


def collect_images(raw_dir, destination):
    try:
        layouts = json.loads((raw_dir / "ci-layout.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        layouts = {}
    records = []
    for path in sorted(raw_dir.glob("*.png")):
        if not re.fullmatch(r"[A-Za-z0-9_-]+\.png", path.name) or path.is_symlink():
            records.append({"name": "invalid_name", "valid": False, "crop_status": "omitted_invalid_input"})
            continue
        record = {"name": path.name, "original_sha256": digest(path), "original_bytes": path.stat().st_size,
                  "visual_qa": "pending", "crop_status": "omitted_no_reliable_layout"}
        try:
            with Image.open(path) as image:
                if image.format != "PNG":
                    raise ValueError("not a PNG")
                image.verify()
            with Image.open(path) as image:
                record["original_size"] = list(image.size)
                image = image.convert("RGB")
                record["valid"] = image.width >= 640 and image.height >= 360 and image.getcolors(maxcolors=16) is None
                if not record["valid"]:
                    record["crop_status"] = "omitted_invalid_or_flat_image"
                    records.append(record)
                    continue
                layout = layouts.get("shots", {}).get(path.stem)
                box = crop_box(layout, layouts.get("native"), image.size)
                if box is not None:
                    cropped = image.crop(box)
                    ratio = layout["viewport"]["dpr"]
                    radius = min(math.ceil(max(0, layout.get("radius", 0)) * ratio), cropped.width // 2, cropped.height // 2)
                    # Rounded modal corners otherwise contain a few pixels of the desktop behind it.
                    if radius:
                        mask = Image.new("L", cropped.size)
                        ImageDraw.Draw(mask).rounded_rectangle((0, 0, cropped.width - 1, cropped.height - 1), radius=radius, fill=255)
                        canvas = Image.new("RGB", cropped.size, "#202020")
                        canvas.paste(cropped, mask=mask)
                        cropped = canvas
                    if cropped.getcolors(maxcolors=16) is not None:
                        record.update(valid=False, crop_status="omitted_flat_test_ui")
                        records.append(record)
                        continue
                    destination.mkdir(parents=True, exist_ok=True)
                    target = destination / path.name
                    cropped.save(target, format="PNG")
                    record.update(crop_status="saved_test_ui_only", crop_kind=layout["kind"], crop_box=box,
                                  corner_mask_radius_px=radius, crop_file=target.name,
                                  crop_size=list(cropped.size), crop_sha256=digest(target))
        except (OSError, ValueError, TypeError, KeyError, OverflowError):
            record.update(valid=False, crop_status="omitted_invalid_image_or_layout")
        records.append(record)
    return records


def accepted(record):
    return (record.get("harness_exit") == 0 and record.get("cleanup_ok") is True and not record.get("timed_out")
            and record.get("runner_error_stage") is None
            and record.get("all_passed_count") == 1 and record.get("ok_count", 0) > 0 and record.get("fail_count") == 0
            and not record.get("failed_summary_counts") and record.get("app_exit_codes") == [0]
            and bool(record.get("reported_check_counts"))
            and all(count == record["ok_count"] + record["fail_count"] for count in record["reported_check_counts"])
            and record.get("isolated_storage") and record.get("isolated_placement")
            and bool(record.get("images")) and all(x.get("valid") and x.get("crop_status") == "saved_test_ui_only" for x in record["images"]))


def run_one(suite, round_number, private_root, output, cli, app):
    from desktop_chat_ci_windows import run_scoped
    name, filename, seconds = suite
    label = f"{name}-{round_number}"
    private = private_root / label
    private.mkdir(parents=True, exist_ok=False)
    temporary, raw = private / "temp", private / "raw"
    temporary.mkdir(); raw.mkdir()
    env = {k: v for k, v in os.environ.items() if not (k.startswith("E2E_") or k.startswith("AI_REMOTE_"))}
    env.update(TEMP=str(temporary), TMP=str(temporary), TMPDIR=str(temporary), PYTHONUTF8="1", PYTHONUNBUFFERED="1",
               E2E_SCRIPT=f"tools/{filename}", AI_REMOTE_CLI=str(cli), AI_REMOTE_CI_CAPTURE_LAYOUT="1")
    if name == "workspace":
        env["E2E_QUERY"] = "view=devices&shotlang=zh&shottheme=dark"
    command = [sys.executable, str(ROOT / "tools/desktop-chat-e2e.py"), "--cli", str(cli), "--app", str(app),
               "--out", str(raw), "--secs", str(seconds)]
    result = run_scoped(command, cwd=ROOT, env=env, log=private / "harness.log", timeout_seconds=seconds + 330)
    record = {"suite": name, "round": round_number, "script": filename, "script_sha256": digest(ROOT / "tools" / filename),
              "budget_seconds": seconds, "harness_exit": result.get("returncode"), "timed_out": result.get("timed_out", False),
              "cleanup_ok": result.get("cleanup_ok", False), "runner_error_stage": result.get("error"), "runner_winerror": result.get("winerror")}
    raw_log = (private / "harness.log").read_text(encoding="utf-8", errors="replace") if (private / "harness.log").exists() else ""
    record.update(parse_log(raw_log, (ROOT / "tools" / filename).read_text(encoding="utf-8")))
    record["images"] = collect_images(raw, output / "test-ui" / label)
    record["exported_crop_count"] = sum(x["crop_status"] == "saved_test_ui_only" for x in record["images"])
    record["omitted_crop_count"] = len(record["images"]) - record["exported_crop_count"]
    record["visual_qa"] = "pending_downloaded_crops" if record["exported_crop_count"] else "unverified_no_exported_crops"
    record["passed"] = bool(accepted(record))
    return record


def execute(run, report):
    for suite in SUITES:
        for round_number in (1, 2):
            try:
                record = run(suite, round_number)
            except Exception:
                # Exception strings can contain raw paths/data; export only a static stage.
                record = {"suite": suite[0], "round": round_number, "passed": False, "runner_error_stage": "unhandled_runner_error"}
            report["runs"].append(record)
            yield record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--private-root", type=Path, required=True)
    parser.add_argument("--cli", type=Path, default=ROOT / "target/debug/ai-remote.exe")
    parser.add_argument("--app", type=Path, default=ROOT / "target/debug/ai-remote-desktop.exe")
    args = parser.parse_args()
    if os.name != "nt":
        raise SystemExit("This runner requires Windows; no native process was started.")
    output, private = args.out.resolve(), args.private_root.resolve()
    if output == private or output in private.parents or private in output.parents:
        raise SystemExit("Evidence and private raw directories must be separate.")
    if (output / "matrix-results.json").exists():
        raise SystemExit("Refusing to mix a new execution with an existing matrix.")
    output.mkdir(parents=True, exist_ok=True)
    private.mkdir(parents=True, exist_ok=False)
    cli, app = args.cli.resolve(), args.app.resolve()
    report = {"schema": 1, "platform": "windows-2025", "engine": "WebView2", "fake_only": True,
              "rounds_per_suite": 2, "automatic_retries": 0, "runs": [], "visual_qa": "pending_downloaded_crops",
              "limits": ["Only DOM-observed test UI crops are exported; streaming desktop pixels, raw images/logs, auth and homes are excluded.",
                         "Every named/final screenshot requires a reliable test-UI crop; an omitted crop is unverified and fails the run.",
                         "Pixel checks detect invalid/flat captures and do not replace per-image visual QA."]}
    if not cli.is_file() or not app.is_file():
        report.update(passed=False, error_stage="missing_built_binaries")
        write_json(output / "matrix-results.json", report)
        return 1
    revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True, check=False)
    report["source_sha"] = revision.stdout.strip() if re.fullmatch(r"[0-9a-f]{40}", revision.stdout.strip()) else None
    report["quiet_policy"] = "four isolated host/controller configs; host quiet doctor enforced by harness"
    report["binaries"] = {"cli_sha256": digest(cli), "app_sha256": digest(app)}
    report.update(status="running", passed=False, expected_run_count=len(SUITES) * 2)
    write_json(output / "matrix-results.json", report)
    for record in execute(lambda suite, index: run_one(suite, index, private, output, cli, app), report):
        write_json(output / "matrix-results.json", report)
        print(json.dumps({key: record.get(key) for key in ("suite", "round", "passed", "harness_exit", "app_exit_codes", "ok_count", "fail_count", "timed_out", "cleanup_ok")}), flush=True)
    report["passed"] = len(report["runs"]) == len(SUITES) * 2 and all(x["passed"] for x in report["runs"])
    report["status"] = "passed" if report["passed"] else "failed"
    write_json(output / "matrix-results.json", report)
    return 0 if report["passed"] else 1


def entry_point(run=main):
    try:
        return run()
    except Exception:
        # Setup exceptions may contain absolute private paths. Never print their repr/traceback.
        print(json.dumps({"passed": False, "runner_error_stage": "runner_setup_error"}), flush=True)
        return 1


if __name__ == "__main__":
    sys.exit(entry_point())
