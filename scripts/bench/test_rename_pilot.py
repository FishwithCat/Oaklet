import argparse
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import rename_pilot


class BehaviorBenchmarkTest(unittest.TestCase):
    def test_manifest_allows_only_local_test_registrations(self):
        original = """[package]
name = "regex-lite"
version = "0.1.0"

[[test]]
name = "existing"
path = "tests/existing.rs"
"""
        registration = '\n[[test]]\nname = "new_test"\npath = "tests/new_test.rs"\n'
        with tempfile.TemporaryDirectory() as temp:
            tree = Path(temp).resolve()
            crate = tree / "regex-lite"
            (crate / "tests").mkdir(parents=True)
            (crate / "tests/new_test.rs").touch()
            (crate / "outside.rs").touch()
            manifest = crate / "Cargo.toml"
            with patch.object(rename_pilot, "git", return_value=argparse.Namespace(stdout=original)):
                for content, allowed in [
                    (original + registration, True),
                    (original.replace('version = "0.1.0"', 'version = "0.2.0"') + registration, False),
                    (original + registration.replace("tests/new_test.rs", "tests/../outside.rs"), False),
                    (original + registration + "harness = false\n", False),
                    (original.split("[[test]]", 1)[0] + registration, False),
                ]:
                    with self.subTest(content=content):
                        manifest.write_text(content)
                        self.assertEqual(rename_pilot.only_adds_test_targets(tree, "regex-lite"), allowed)

    def test_adoption_tracks_semantic_answers_separately(self):
        args = argparse.Namespace(task="find-iter-at")
        run = {
            "changed": [],
            "seconds": 0,
            "result": {
                "steps": [
                    {"tool": "code_context", "ok": False},
                    {"tool": "code_context", "ok": True, "contextSemantic": "unavailable"},
                    {"tool": "code_context", "ok": True, "contextSemantic": "answered"},
                ],
            },
        }
        score = rename_pilot.score_run(args, run, {"rc": 0, "timed_out": False}, "")
        self.assertEqual(score["context_calls"], 3)
        self.assertEqual(score["context_semantic_calls"], 1)
        del run["result"]["steps"][-1]["contextSemantic"]
        legacy = rename_pilot.score_run(args, run, {"rc": 0, "timed_out": False}, "")
        self.assertIsNone(legacy["context_semantic_calls"])
