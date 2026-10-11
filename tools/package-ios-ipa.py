#!/usr/bin/env python3
"""Verify an unprovisioned arm64 iphoneos .app and archive it for local signing, never installation."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import zipfile

ARM64 = 0x0100000C
IOS_DEVICE = 2
LC_BUILD_VERSION = 0x32
MACH64 = {b"\xcf\xfa\xed\xfe": "<", b"\xfe\xed\xfa\xcf": ">"}
FAT = {
    b"\xca\xfe\xba\xbe": (">", False),
    b"\xbe\xba\xfe\xca": ("<", False),
    b"\xca\xfe\xba\xbf": (">", True),
    b"\xbf\xba\xfe\xca": ("<", True),
}
MACH_MAGICS = set(MACH64) | set(FAT) | {b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xce"}


class PackageError(ValueError):
    pass


def thin_image(data: bytes, offset: int, size: int) -> dict:
    if size < 32 or offset < 0 or offset + size > len(data):
        raise PackageError("truncated Mach-O image")
    endian = MACH64.get(data[offset:offset + 4])
    if endian is None:
        raise PackageError("only 64-bit arm64 Mach-O images are supported")
    _, cpu, subtype, filetype, count, command_bytes, _, _ = struct.unpack_from(endian + "8I", data, offset)
    if cpu != ARM64:
        raise PackageError("Mach-O image contains a non-arm64 architecture")
    if subtype & 0x00FFFFFF not in {0, 1}:
        raise PackageError("Mach-O image contains arm64e or an unsupported arm64 subtype")
    cursor = offset + 32
    end = cursor + command_bytes
    if end > offset + size or count > command_bytes // 8:
        raise PackageError("invalid Mach-O load-command bounds")
    platforms = []
    for _ in range(count):
        if cursor + 8 > end:
            raise PackageError("truncated Mach-O load command")
        command, length = struct.unpack_from(endian + "II", data, cursor)
        if length < 8 or cursor + length > end:
            raise PackageError("invalid Mach-O load-command size")
        if command == LC_BUILD_VERSION:
            if length < 24:
                raise PackageError("truncated Mach-O build-version command")
            platform, _, _, tools = struct.unpack_from(endian + "4I", data, cursor + 8)
            if tools > (length - 24) // 8:
                raise PackageError("invalid Mach-O build-tool bounds")
            platforms.append(platform)
        cursor += length
    if cursor != end:
        raise PackageError("Mach-O load-command size does not match its header")
    if platforms != [IOS_DEVICE]:
        raise PackageError("Mach-O is not a verified iOS device image (simulator, Catalyst and missing platform metadata are refused)")
    return {"architecture": "arm64", "platform": "iOS-device", "filetype": filetype, "cpu_subtype": subtype & 0x00FFFFFF}


def macho_images(path: Path) -> list[dict]:
    data = path.read_bytes()
    magic = data[:4]
    if magic in MACH64:
        return [thin_image(data, 0, len(data))]
    if magic not in FAT:
        raise PackageError("main executable or embedded image is not a supported Mach-O")
    endian, wide = FAT[magic]
    if len(data) < 8:
        raise PackageError("truncated universal Mach-O header")
    count = struct.unpack_from(endian + "I", data, 4)[0]
    stride = 32 if wide else 20
    header_end = 8 + count * stride
    if not 1 <= count <= 64 or header_end > len(data):
        raise PackageError("invalid universal Mach-O architecture table")
    images, regions = [], []
    for index in range(count):
        entry = 8 + index * stride
        if wide:
            cpu, subtype, offset, size, _, _ = struct.unpack_from(endian + "IIQQII", data, entry)
        else:
            cpu, subtype, offset, size, _ = struct.unpack_from(endian + "5I", data, entry)
        if cpu != ARM64 or subtype & 0x00FFFFFF not in {0, 1} or offset < header_end or size == 0:
            raise PackageError("universal image contains an unsupported architecture or invalid slice")
        if any(offset < previous_end and previous_start < offset + size for previous_start, previous_end in regions):
            raise PackageError("universal Mach-O slices overlap")
        regions.append((offset, offset + size))
        image = thin_image(data, offset, size)
        if image["cpu_subtype"] != subtype & 0x00FFFFFF:
            raise PackageError("universal Mach-O subtype does not match its slice")
        images.append(image)
    return images


def inventory(app: Path) -> dict[str, tuple[str, bytes | None]]:
    """Reject escaping/broken links, while preserving framework link text and relative paths."""
    result = {}
    for folder, directories, files in os.walk(app, followlinks=False):
        for name in directories + files:
            path = Path(folder) / name
            relative = path.relative_to(app).as_posix()
            if "\\" in name:
                raise PackageError("bundle filenames with backslashes are refused")
            if name == "embedded.mobileprovision":
                raise PackageError("unsigned input must not contain embedded.mobileprovision; use CODE_SIGNING_ALLOWED=NO")
            if path.is_symlink():
                target = os.readlink(path)
                if os.path.isabs(target):
                    raise PackageError("absolute bundle symlinks are refused")
                try:
                    path.resolve(strict=True).relative_to(app)
                except (ValueError, FileNotFoundError, RuntimeError):
                    raise PackageError("broken or escaping bundle symlink") from None
                result[relative] = ("symlink", os.fsencode(target))
            elif path.is_file():
                result[relative] = ("file", None)
            elif not path.is_dir():
                raise PackageError("special files are not supported in an iOS app")
    return result


def validate_app(app: Path) -> tuple[dict, dict]:
    if app.suffix != ".app" or not app.is_dir():
        raise PackageError("input must be an existing .app directory")
    entries = inventory(app)
    try:
        with (app / "Info.plist").open("rb") as file:
            info = plistlib.load(file)
    except (OSError, plistlib.InvalidFileException, ValueError):
        raise PackageError("app Info.plist is missing or invalid") from None
    if not isinstance(info, dict) or info.get("CFBundleSupportedPlatforms") != ["iPhoneOS"]:
        raise PackageError("only an iphoneos device app is accepted; simulator apps cannot be installed on an iPhone")
    executable = info.get("CFBundleExecutable")
    if not isinstance(executable, str) or not executable or Path(executable).name != executable or executable in {".", ".."}:
        raise PackageError("invalid CFBundleExecutable")
    main = app / executable
    if not main.is_file() or not main.stat().st_mode & 0o111:
        raise PackageError("device app executable is missing or not executable")
    main_images = macho_images(main)
    if any(image["filetype"] != 2 for image in main_images):
        raise PackageError("CFBundleExecutable must be a Mach-O executable")
    image_count = len(main_images)
    for relative, (kind, _) in entries.items():
        path = app / relative
        if kind != "file" or path == main:
            continue
        with path.open("rb") as file:
            magic = file.read(4)
        if magic in MACH_MAGICS:
            image_count += len(macho_images(path))
    minimum = info.get("MinimumOSVersion")
    metadata = {
        "app": app.name,
        "platform": "iPhoneOS",
        "architectures": ["arm64"],
        "minimum_os": minimum if isinstance(minimum, str) and all(c.isdigit() or c == "." for c in minimum) else None,
        "mach_o_images": image_count,
        # CODE_SIGNING_ALLOWED=NO is the CI build contract. Mach-O may still carry a linker
        # ad-hoc signature; no claim about cryptographic signature absence is made here.
        "signing": "local-signing-required",
        "embedded_provisioning": False,
        "requires_local_signing": True,
    }
    return metadata, entries


def verify_archive(archive: Path, app: Path, entries: dict) -> None:
    prefix = "Payload/" + app.name + "/"
    with zipfile.ZipFile(archive) as zipped:
        if zipped.testzip() is not None:
            raise PackageError("archive failed its CRC integrity check")
        names = {item.filename: item for item in zipped.infolist()}
        if len(names) != len(zipped.infolist()):
            raise PackageError("archive contains duplicate entries")
        for name in names:
            parts = PurePosixPath(name).parts
            if name.startswith("/") or ".." in parts or not (name == "Payload/" or name.startswith(prefix) or name == prefix[:-1]):
                raise PackageError("archive has an unexpected layout")
        for relative, (kind, target) in entries.items():
            name = prefix + relative
            item = names.get(name)
            if item is None:
                raise PackageError("archive omitted app content")
            mode = item.external_attr >> 16
            if kind == "symlink":
                if not stat.S_ISLNK(mode) or zipped.read(name) != target:
                    raise PackageError("archive did not preserve a framework symlink")
            elif stat.S_ISLNK(mode):
                raise PackageError("archive changed a regular file into a symlink")
            elif item.file_size != (app / relative).stat().st_size:
                raise PackageError("archive changed an app file's size")
        executable = plistlib.loads((app / "Info.plist").read_bytes())["CFBundleExecutable"]
        item = names.get(prefix + executable)
        if item is None or not (item.external_attr >> 16) & 0o111:
            raise PackageError("archive did not preserve executable permissions")


def package(app: Path, output: Path) -> dict:
    app = app.resolve(strict=True)
    output = output.resolve()
    if output.suffix.lower() != ".ipa":
        raise PackageError("output must have an .ipa extension")
    metadata, entries = validate_app(app)
    if output.is_relative_to(app):
        raise PackageError("output must be outside the app bundle")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".iphone-ipa-", dir=output.parent) as temporary:
        staging = Path(temporary)
        payload = staging / "Payload"
        payload.mkdir()
        shutil.copytree(app, payload / app.name, symlinks=True)
        # Validate the copied bundle that will actually enter the archive, including its links.
        metadata, entries = validate_app(payload / app.name)
        archive = staging / "unsigned.ipa"
        ditto = shutil.which("ditto")
        if ditto:
            subprocess.run([ditto, "-c", "-k", "--norsrc", "--keepParent", str(payload), str(archive)], check=True, capture_output=True)
        else:
            zip_tool = shutil.which("zip")
            if not zip_tool:
                raise PackageError("packaging requires ditto or zip with symlink support")
            subprocess.run([zip_tool, "-q", "-r", "-y", str(archive), "Payload"], cwd=staging, check=True, capture_output=True)
        verify_archive(archive, payload / app.name, entries)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        size = archive.stat().st_size
        os.replace(archive, output)
    return metadata | {"ipa": output.name, "bytes": size, "sha256": digest}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        metadata = package(args.app, args.output)
    except (PackageError, OSError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
        message = str(error) if isinstance(error, PackageError) else "bundle packaging failed; check the input and local archive tools"
        print(json.dumps({"ok": False, "error": message}), file=sys.stderr)
        return 1
    print(json.dumps({"ok": True} | metadata, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
