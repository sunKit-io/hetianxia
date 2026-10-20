#!/usr/bin/env python3
"""Scan a linked release executable, not a rustc rlib.

--forbid: the non-feature release must not contain the fault-injection
sentinel, UPG_FAULT, sftp-disconnect, or a stage token like backup:1.
When the path is a directory, this scans:

- ``*.app/Contents/MacOS/*``
- the executable inside each ``*.dmg``
- ``hetianxia-linkcheck`` (the linked stand-in used when the app is not built)

An rlib is never a successful scan target.

--require-sentinel: a fault-inject linked executable must contain the sentinel
outside rustc metadata. Pass the executable, or a directory that contains
``hetianxia-linkcheck``. A production ``.app`` is not required to carry it.
"""

import argparse
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

NEEDLES = (b"UPG_FAULT", b"sftp-disconnect", b"HTX_FAULT_INJECT_ENABLED", b"fsync-dir")
STAGE = re.compile(
    rb"(?:backup|write|verify-pre|verify-post|rename|rollback|log|log-probe|fsync-dir|sftp-disconnect):[0-9a-z]"
)
# Stage 变体名，外加 Item::LogProbe。按标识符边界匹配，避免撞上 core::fmt::Write 这类符号。
# `Write` 不放进裸名字：正式二进制里有 `std::io::Write`。
# `Log` 也不放：Tauri 带进来的 log::Log 会命中。阶段语法 `write:N` / `log:N` 仍在 STAGE 里。
STAGE_NAMES = (
    b"Backup",
    b"VerifyPre",
    b"VerifyPost",
    b"Rename",
    b"Rollback",
    b"LogProbe",
    b"SftpDisconnect",
    b"FsyncDir",
)
STAGE_NAME_RE = re.compile(rb"(?<![A-Za-z0-9_])(?:" + b"|".join(STAGE_NAMES) + rb")(?![A-Za-z0-9_])")
# 第三方库 russh_sftp 反序列化报错的整句。扫描名单前只剔除这一句的完整出现。
# 不做子串宽松匹配，也不放宽大小写。其他位置的 Rename、Backup、Rollback 仍要报。
EXACT_THIRD_PARTY_STRINGS = (b"struct Rename with 3 elements",)
SENTINEL = b"HTX_FAULT_INJECT_ENABLED"
LINKCHECK = "hetianxia-linkcheck"
CONTEXT_RADIUS = 64


def is_rlib(path: pathlib.Path) -> bool:
    return path.suffix == ".rlib" or path.name.startswith("libhetianxia_core-")


def app_executables(root: pathlib.Path):
    found = []
    for macos in root.rglob("Contents/MacOS"):
        # .../Name.app/Contents/MacOS — the .app is the grandparent, not Contents.
        if macos.parent.parent.suffix != ".app":
            continue
        for child in macos.iterdir():
            if child.is_file() and not is_rlib(child):
                found.append(child)
    return found


def _member(root: pathlib.Path, path: pathlib.Path) -> str:
    return path.relative_to(root).as_posix()


def dmg_executables(dmg: pathlib.Path):
    """返回 (dmg 内相对路径, 字节)。选哪些文件的规则不变。"""
    extracted = []
    with tempfile.TemporaryDirectory(prefix="htx-dmg-") as tmp:
        dest = pathlib.Path(tmp)
        if shutil.which("hdiutil"):
            mount = dest / "mnt"
            mount.mkdir()
            subprocess.run(
                ["hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", str(mount), str(dmg)],
                check=True,
            )
            try:
                extracted = [(_member(mount, path), path.read_bytes()) for path in app_executables(mount)]
            finally:
                subprocess.run(["hdiutil", "detach", str(mount)], check=False)
            return extracted
        tool = shutil.which("7z") or shutil.which("7zz")
        if not tool:
            sys.exit(f"{dmg} is present but neither hdiutil nor 7z can read it")
        subprocess.run([tool, "x", f"-o{dest}", str(dmg)], check=True, capture_output=True)
        for path in app_executables(dest):
            extracted.append((_member(dest, path), path.read_bytes()))
        if not extracted:
            for path in dest.rglob("*"):
                if path.is_file() and not is_rlib(path) and path.suffix not in {".plist", ".xml", ".txt"}:
                    blob = path.read_bytes()
                    if blob[:4] in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\x7fELF") or blob[:2] == b"MZ":
                        extracted.append((_member(dest, path), blob))
    if not extracted:
        sys.exit(f"no executable inside {dmg}")
    return extracted


def linkcheck_bins(root: pathlib.Path):
    names = [LINKCHECK, f"{LINKCHECK}.exe"]
    found = []
    for name in names:
        path = root / name
        if path.is_file():
            found.append(path)
    return found


def hex_ascii_context(blob: bytes, offset: int, length: int) -> str:
    """命中点前 64 字节到命中结束后 64 字节。不可见字节的 ASCII 列用 `.`。"""
    start = max(0, offset - CONTEXT_RADIUS)
    end = min(len(blob), offset + length + CONTEXT_RADIUS)
    window = blob[start:end]
    lines = []
    for row in range(0, len(window), 16):
        chunk = window[row : row + 16]
        hexpart = " ".join(f"{byte:02x}" for byte in chunk)
        text = "".join(chr(byte) if 32 <= byte <= 126 else "." for byte in chunk)
        lines.append(f"{start + row:08x}  {hexpart:<47}  {text}")
    return "\n".join(lines)


def report_hit(label: str, blob: bytes, offset: int, length: int, detail: str) -> None:
    print(f"{label} contains {detail} at {offset}")
    print(f"{label} offset {offset}")
    print(hex_ascii_context(blob, offset, length))


def without_exact_third_party(blob: bytes) -> bytes:
    """用等长空字节盖住白名单整句，后面的偏移仍对得上原文件。"""
    skipped = blob
    for phrase in EXACT_THIRD_PARTY_STRINGS:
        skipped = skipped.replace(phrase, b"\x00" * len(phrase))
    return skipped


def forbid_blobs(blobs):
    print(f"scanning {len(blobs)} target(s)")
    for label, blob in blobs:
        print(f"scan {label} ({len(blob)} bytes)")
    failed = 0
    for label, blob in blobs:
        if is_rlib(pathlib.Path(label)):
            print(f"refusing to scan rlib {label}")
            failed += 1
            continue
        for needle in NEEDLES:
            idx = blob.find(needle)
            if idx >= 0:
                report_hit(label, blob, idx, len(needle), needle.decode())
                failed += 1
        for match in STAGE.finditer(blob):
            report_hit(label, blob, match.start(), len(match.group()), repr(match.group()))
            failed += 1
            break
        for match in STAGE_NAME_RE.finditer(without_exact_third_party(blob)):
            report_hit(label, blob, match.start(), len(match.group()), match.group().decode())
            failed += 1
            break
    return failed


def collect_forbid(root: pathlib.Path):
    if root.is_file():
        if is_rlib(root):
            sys.exit(f"refusing to scan rlib {root}")
        return [(str(root), root.read_bytes())]
    blobs = []
    for path in app_executables(root):
        blobs.append((str(path), path.read_bytes()))
    for dmg in sorted(root.rglob("*.dmg")):
        for index, (member, blob) in enumerate(dmg_executables(dmg)):
            blobs.append((f"{dmg}#{index} {member}", blob))
    for path in linkcheck_bins(root):
        blobs.append((str(path), path.read_bytes()))
    if not blobs:
        sys.exit(f"no linked executable under {root}; refusing to scan an rlib")
    return blobs


def require_sentinel(root: pathlib.Path) -> int:
    targets = []
    if root.is_file():
        if is_rlib(root):
            sys.exit(f"refusing to scan rlib {root}")
        targets = [root]
    else:
        targets = linkcheck_bins(root)
        if not targets:
            sys.exit(f"no {LINKCHECK} under {root}; refusing to scan an rlib")
    print(f"scanning {len(targets)} target(s)")
    for path in targets:
        blob = path.read_bytes()
        print(f"scan {path} ({len(blob)} bytes)")
        if SENTINEL in blob:
            print(f"sentinel present in {path}")
            return 0
    print("fault-inject build is missing HTX_FAULT_INJECT_ENABLED in the linked executable")
    return 1


def main() -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--forbid", action="store_true")
    mode.add_argument("--require-sentinel", action="store_true")
    parser.add_argument("root", type=pathlib.Path)
    args = parser.parse_args()
    if args.require_sentinel:
        return require_sentinel(args.root)
    failed = forbid_blobs(collect_forbid(args.root))
    if failed:
        print(f"release strings check failed ({failed})")
        return 1
    print("release strings check passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
