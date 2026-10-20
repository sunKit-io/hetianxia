#!/usr/bin/env python3
"""名单扫描：只跳过 russh_sftp 的整句，其他 Rename 照报。"""

import contextlib
import importlib.util
import io
import pathlib
import unittest

SPEC = importlib.util.spec_from_file_location(
    "scan_release_strings",
    pathlib.Path(__file__).with_name("scan_release_strings.py"),
)
scan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scan)

EXACT = b"struct Rename with 3 elements"


def name_hits(blob: bytes) -> list[bytes]:
    masked = scan.without_exact_third_party(blob)
    return [match.group() for match in scan.STAGE_NAME_RE.finditer(masked)]


class ExactThirdPartySentenceTest(unittest.TestCase):
    def test_only_the_exact_sentence_is_silent(self):
        self.assertEqual(name_hits(EXACT), [])
        self.assertEqual(scan.forbid_blobs([("sample", EXACT)]), 0)

    def test_rename_that_is_not_the_whole_sentence_is_reported(self):
        for sample in (b"struct Rename with 4 elements", b"Rename"):
            self.assertIn(b"Rename", name_hits(sample), sample)
            self.assertGreater(scan.forbid_blobs([("sample", sample)]), 0)

    def test_exact_sentence_plus_another_rename_is_reported(self):
        blob = EXACT + b"\nRename"
        self.assertEqual(name_hits(blob), [b"Rename"])
        self.assertGreater(scan.forbid_blobs([("sample", blob)]), 0)
        beside = EXACT + b" Backup Rollback"
        self.assertEqual(name_hits(beside), [b"Backup", b"Rollback"])

    def test_hit_output_includes_context(self):
        prefix = bytes([0x11]) + b"Z" * 69 + bytes([0x01])
        blob = prefix + b"Rename" + b"\x02" + b"Y" * 69 + bytes([0x22])
        offset = len(prefix)
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            failed = scan.forbid_blobs([("bundle/Demo.app/Contents/MacOS/hetianxia", blob)])
        text = buf.getvalue()
        self.assertGreater(failed, 0)
        self.assertIn("scan bundle/Demo.app/Contents/MacOS/hetianxia (", text)
        self.assertIn(f"{len(blob)} bytes", text)
        self.assertIn(f"contains Rename at {offset}", text)
        self.assertIn(f"offset {offset}", text)
        self.assertIn("52 65 6e 61 6d 65", text)
        self.assertIn("Rename", text)
        self.assertIn("01", text)
        self.assertIn(f"{offset - 64:08x}  ", text)
        self.assertNotIn("00000000  ", text)

    def test_every_hit_in_one_file_is_listed(self):
        blob = b"Rename\x00" + EXACT + b"\x00Backup\x00Rename"
        first = 0
        backup_at = len(b"Rename\x00") + len(EXACT) + 1
        second = backup_at + len(b"Backup\x00")
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            failed = scan.forbid_blobs([("sample", blob)])
        text = buf.getvalue()
        self.assertEqual(name_hits(blob), [b"Rename", b"Backup", b"Rename"])
        self.assertEqual(failed, 3)
        self.assertEqual(text.count("contains Rename at "), 2)
        self.assertIn(f"contains Rename at {first}", text)
        self.assertIn(f"contains Backup at {backup_at}", text)
        self.assertIn(f"contains Rename at {second}", text)
        self.assertEqual(text.count("offset "), 3)
        self.assertIn("52 65 6e 61 6d 65", text)
        self.assertIn("42 61 63 6b 75 70", text)


if __name__ == "__main__":
    unittest.main()
