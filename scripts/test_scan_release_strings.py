#!/usr/bin/env python3
"""名单扫描：只跳过 russh_sftp 的整句，其他 Rename 照报。"""

import importlib.util
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


if __name__ == "__main__":
    unittest.main()
