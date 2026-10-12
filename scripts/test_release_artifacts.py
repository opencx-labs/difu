"""Exercise artifact reuse without credentials, network calls, or Rust builds."""

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("release_artifacts", Path(__file__).with_name("release-artifacts.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseArtifactsTests(unittest.TestCase):
    def setUp(self):
        self.tree = "a" * 40
        self.commit = "b" * 40
        self.head = "c" * 40
        self.version = "0.18.0"
        self.run = {"id": 10, "run_attempt": 1, "workflow_id": 20, "event": "pull_request",
                    "status": "completed", "conclusion": "success", "head_sha": self.head}
        self.artifact = {"id": 30, "name": f"release-{self.tree}", "expired": False,
                         "workflow_run": {"id": 10}}
        self.files = {name: name.encode() for name in release.filenames(self.version)}
        self.source = {"schema": 1, "tree": self.tree, "commit": self.commit,
                       "version": self.version, "run_id": 10, "run_attempt": 1,
                       "files": {name: release.digest(data) for name, data in self.files.items()}}
        self.built = {"tree": {"sha": self.tree}, "parents": [{"sha": self.head}]}

    def archive(self, extra=None):
        files = self.files | {release.MANIFEST: json.dumps(self.source).encode()} | (extra or {})
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w") as archive:
            for name, content in files.items():
                archive.writestr(name, content)
        return stream.getvalue()

    def verify(self, data=None):
        with patch.object(release, "api", return_value=self.built):
            return release.verify(data or self.archive(), self.artifact, self.run,
                                  self.tree, self.version, "owner/repo")

    def test_matching_tree_reuses_packages_even_with_different_commit_ids(self):
        files = self.verify()
        self.assertEqual(files["difu.rb"], self.files["difu.rb"])

    def test_source_version_and_attempt_must_match(self):
        for key, value in {"schema": 2, "tree": "d" * 40, "version": "0.19.0",
                           "run_id": 11, "run_attempt": 2}.items():
            with self.subTest(key=key), patch.dict(self.source, {key: value}):
                with self.assertRaises(ValueError):
                    self.verify()

    def test_changed_package_is_rejected(self):
        self.files["difu.rb"] = b"changed"
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.verify()

    def test_wrong_archive_digest_is_rejected(self):
        self.artifact["digest"] = "sha256:wrong"
        with self.assertRaisesRegex(ValueError, "artifact checksum"):
            self.verify()

    def test_matching_archive_digest_is_accepted(self):
        data = self.archive()
        self.artifact["digest"] = f"sha256:{hashlib.sha256(data).hexdigest()}"
        self.verify(data)

    def test_missing_platform_and_extra_paths_are_rejected(self):
        removed = self.files.pop(next(name for name in self.files if name.endswith("tar.gz")))
        with self.assertRaisesRegex(ValueError, "missing"):
            self.verify()
        self.assertTrue(removed)
        with self.assertRaisesRegex(ValueError, "Unexpected"):
            self.verify(self.archive({"../outside": b"bad"}))

    def test_source_commit_must_match_tree_and_pr(self):
        for built in [{"tree": {"sha": "d" * 40}, "parents": [{"sha": self.head}]},
                      {"tree": {"sha": self.tree}, "parents": []}]:
            with self.subTest(built=built):
                self.built = built
                with self.assertRaisesRegex(ValueError, "PR source"):
                    self.verify()

    def test_manifest_cannot_claim_a_different_runs_source_tree(self):
        def commits(path):
            if path.endswith(self.head):
                return {"tree": {"sha": "d" * 40}, "parents": []}
            return self.built

        with patch.object(release, "api", side_effect=commits):
            with self.assertRaisesRegex(ValueError, "PR source tree differs"):
                release.verify(self.archive(), self.artifact, self.run,
                               self.tree, self.version, "owner/repo")

    def test_failed_incomplete_other_workflow_and_tag_runs_are_ineligible(self):
        self.assertTrue(release.eligible(self.artifact, self.run, 20))
        for key, value in {"conclusion": "failure", "status": "in_progress",
                           "event": "push", "workflow_id": 21, "id": 11}.items():
            with self.subTest(key=key), patch.dict(self.run, {key: value}):
                self.assertFalse(release.eligible(self.artifact, self.run, 20))
        self.artifact["expired"] = True
        self.assertFalse(release.eligible(self.artifact, self.run, 20))

    def api(self, path):
        if path.endswith("workflows/release.yml"):
            return {"id": 20}
        if "/actions/artifacts?" in path:
            return {"artifacts": self.candidates}
        if path.endswith("/runs/10"):
            return self.run
        if "/git/commits/" in path:
            return self.built
        raise AssertionError(path)

    def test_reuse_downloads_and_verifies_before_writing(self):
        self.candidates = [self.artifact]
        with tempfile.TemporaryDirectory() as directory, patch.object(release, "api", side_effect=self.api), \
                patch.object(release.subprocess, "check_output", return_value=self.archive()):
            dist = Path(directory) / "dist"
            self.assertTrue(release.reuse(dist, self.tree, self.version, "owner/repo"))
            self.assertEqual((dist / "difu.rb").read_bytes(), self.files["difu.rb"])

    def test_missing_expired_failed_corrupt_and_unavailable_artifacts_allow_rebuild(self):
        cases = [[], [self.artifact | {"expired": True}], [self.artifact]]
        for candidates in cases:
            self.candidates = candidates
            with self.subTest(candidates=candidates), tempfile.TemporaryDirectory() as directory, \
                    patch.object(release, "api", side_effect=self.api), \
                    patch.object(release.subprocess, "check_output", return_value=b"corrupt"):
                dist = Path(directory) / "dist"
                self.assertFalse(release.reuse(dist, self.tree, self.version, "owner/repo"))
                self.assertFalse(dist.exists())
        self.candidates = [self.artifact]
        with tempfile.TemporaryDirectory() as directory, patch.object(release, "api", side_effect=self.api), \
                patch.object(release.subprocess, "check_output", side_effect=subprocess.CalledProcessError(1, "gh")):
            self.assertFalse(release.reuse(Path(directory), self.tree, self.version, "owner/repo"))

    def test_recorded_bundle_can_be_verified(self):
        with tempfile.TemporaryDirectory() as directory:
            dist = Path(directory)
            for name, content in self.files.items():
                (dist / name).write_bytes(content)
            with patch.dict(os.environ, {"GITHUB_RUN_ID": "10", "GITHUB_RUN_ATTEMPT": "1"}), \
                    patch.object(release, "git", return_value=self.commit):
                release.record(dist, self.tree, self.version)
            self.source = json.loads((dist / release.MANIFEST).read_text())
            self.verify()

    def test_lookup_failure_selects_fallback_but_wrong_tag_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[package]\nversion = "0.18.0"\n')
            output = root / "output"
            with patch.object(release, "ROOT", root), patch.object(release, "git", return_value=self.tree), \
                    patch.object(release.sys, "argv", ["script", "reuse", str(root / "dist")]), \
                    patch.dict(os.environ, {"GITHUB_REF_NAME": "v0.18.0", "GITHUB_REPOSITORY": "owner/repo",
                                            "GITHUB_OUTPUT": str(output)}), \
                    patch.object(release, "api", side_effect=subprocess.CalledProcessError(1, "gh")):
                release.main()
                self.assertIn("reused=false", output.read_text())
                with patch.dict(os.environ, {"GITHUB_REF_NAME": "v0.19.0"}):
                    with self.assertRaisesRegex(SystemExit, "tag must match"):
                        release.main()


if __name__ == "__main__":
    unittest.main()
