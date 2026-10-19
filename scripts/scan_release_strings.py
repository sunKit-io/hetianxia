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
STAGE = re.compile(rb"(?:backup|write|verify-pre|verify-post|rename|rollback|log|fsync-dir|sftp-disconnect):[0-9a-z]")
STAGE_NAMES = (b"SftpDisconnect", b"FsyncDir", b"VerifyPre", b"VerifyPost")
SENTINEL = b"HTX_FAULT_INJECT_ENABLED"
LINKCHECK = "hetianxia-linkcheck"


def is_rlib(path: pathlib.Path) -> bool:
    return path.suffix == ".rlib" or path.name.startswith("libhetianxia_core-")


def app_executables(root: pathlib.Path):
    found = []
    for macos in root.rglob("Contents/MacOS"):
        if macos.parent.suffix != ".app":
            continue
        for child in macos.iterdir():
            if child.is_file() and not is_rlib(child):
                found.append(child)
    return found


def dmg_executables(dmg: pathlib.Path):
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
                extracted = [p.read_bytes() for p in app_executables(mount)]
            finally:
                subprocess.run(["hdiutil", "detach", str(mount)], check=False)
            return extracted
        tool = shutil.which("7z") or shutil.which("7zz")
        if not tool:
            sys.exit(f"{dmg} is present but neither hdiutil nor 7z can read it")
        subprocess.run([tool, "x", f"-o{dest}", str(dmg)], check=True, capture_output=True)
        for path in app_executables(dest):
            extracted.append(path.read_bytes())
        if not extracted:
            for path in dest.rglob("*"):
                if path.is_file() and not is_rlib(path) and path.suffix not in {".plist", ".xml", ".txt"}:
                    blob = path.read_bytes()
                    if blob[:4] in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\x7fELF") or blob[:2] == b"MZ":
                        extracted.append(blob)
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


def forbid_blobs(blobs):
    failed = 0
    for label, blob in blobs:
        if is_rlib(pathlib.Path(label)):
            print(f"refusing to scan rlib {label}")
            failed += 1
            continue
        for needle in NEEDLES:
            idx = blob.find(needle)
            if idx >= 0:
                print(f"{label} contains {needle.decode()} at {idx}")
                failed += 1
        for match in STAGE.finditer(blob):
            print(f"{label} contains {match.group()!r} at {match.start()}")
            failed += 1
            break
        for name in STAGE_NAMES:
            idx = blob.find(name)
            if idx >= 0:
                print(f"{label} contains {name.decode()} at {idx}")
                failed += 1
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
        for index, blob in enumerate(dmg_executables(dmg)):
            blobs.append((f"{dmg}#{index}", blob))
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
    for path in targets:
        blob = path.read_bytes()
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
