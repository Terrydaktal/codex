"""Offline regression tests for ordered, source-only customization bundles."""

import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "port_local_customizations",
    Path(__file__).with_name("port-local-customizations.py"),
)
port = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = port
SPEC.loader.exec_module(port)


class PatchStackTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="codex-patch-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.git(self.source, "init", "-b", "main")
        self.write("codex-rs/Cargo.toml", '[workspace.package]\nversion = "0.159.1"\n')
        self.write("example.txt", "".join(f"line {i}\n" for i in range(80)))
        self.write("deleted.txt", "old\n")
        self.base = self.commit("build(test): establish upstream")
        manifest = {
            "schema_version": 1,
            "upstream": {"base_commit": self.base, "version": "0.159.1"},
            "generated_paths": ["generated.txt"],
            "ignored_untracked_globs": [],
            "portable_untracked_paths": [str(port.MANIFEST_PATH)],
        }
        self.write(str(port.MANIFEST_PATH), json.dumps(manifest))
        self.commit("build(test): record the customization base")
        self.bundle = self.root / "bundle"

    def git(self, repo, *args):
        return (
            subprocess.check_output(
                [
                    "git",
                    "-C",
                    str(repo),
                    "-c",
                    "user.name=Terrydaktal",
                    "-c",
                    "user.email=9lewis9@gmail.com",
                    *args,
                ],
                stderr=subprocess.PIPE,
            )
            .decode()
            .strip()
        )

    def write(self, name, content):
        path = self.source / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)

    def commit(self, subject):
        self.git(self.source, "add", ".")
        self.git(
            self.source, "commit", "-m", subject, "-m", "Problem, change and result."
        )
        return self.git(self.source, "rev-parse", "HEAD")

    def target(self):
        target = self.root / "target"
        target.mkdir()
        archive = subprocess.check_output(
            ["git", "-C", str(self.source), "archive", self.base]
        )
        with tarfile.open(fileobj=io.BytesIO(archive)) as files:
            # All members originate in the controlled fixture, not a user archive.
            options = {"filter": "data"} if hasattr(tarfile, "data_filter") else {}
            files.extractall(target, **options)
        self.git(target, "init", "-b", "main")
        self.git(target, "add", ".")
        self.git(target, "commit", "-m", "Upstream fixture")
        return target

    def change(self, before, after):
        path = self.source / "example.txt"
        path.write_text(path.read_text().replace(before, after))

    def test_dependent_commits_round_trip_without_staging_or_losing_metadata(self):
        self.change("line 30\n", "first edit\n")
        first = self.commit("fix(test): change the first stage")
        self.change("first edit\n", "second edit\n")
        (self.source / "deleted.txt").unlink()
        binary = self.source / "data.bin"
        binary.write_bytes(bytes(range(256)))
        binary.chmod(0o755)
        manifest = json.loads((self.source / port.MANIFEST_PATH).read_text())
        manifest["portable_untracked_paths"].append("data.bin")
        self.write(str(port.MANIFEST_PATH), json.dumps(manifest))
        second = self.commit("fix(test): change the second stage")
        port.export_bundle(self.source, self.bundle)
        _, metadata, paths = port.load_bundle(self.bundle)
        self.assertEqual(
            [entry["commit"] for entry in metadata["patches"]][-2:], [first, second]
        )
        self.assertIn(b"Problem, change and result.", paths[-1].read_bytes())
        self.assertEqual(
            (self.bundle / "series").read_text().splitlines(),
            [path.name for path in paths],
        )
        target = self.target()
        original_index = (target / ".git/index").read_bytes()
        self.assertFalse(port.preflight_bundle(self.bundle, target).use_three_way)
        self.assertEqual(
            (target / "example.txt").read_text().splitlines()[30], "line 30"
        )
        port.apply_bundle(self.bundle, target)
        self.assertEqual(
            (target / "example.txt").read_bytes(),
            (self.source / "example.txt").read_bytes(),
        )
        self.assertEqual((target / "data.bin").read_bytes(), binary.read_bytes())
        if sys.platform != "win32":
            self.assertEqual((target / "data.bin").stat().st_mode & 0o777, 0o755)
        self.assertFalse((target / "deleted.txt").exists())
        self.assertEqual((target / ".git/index").read_bytes(), original_index)

    def test_uncommitted_files_have_a_separate_tail_and_generated_files_are_excluded(
        self,
    ):
        self.change("line 30\n", "committed\n")
        self.commit("fix(test): committed stage")
        manifest = json.loads((self.source / port.MANIFEST_PATH).read_text())
        manifest["portable_untracked_paths"].append("new.txt")
        self.write(str(port.MANIFEST_PATH), json.dumps(manifest))
        self.write("new.txt", "new source\n")
        self.write("generated.txt", "do not export\n")
        self.git(self.source, "add", "generated.txt")
        self.change("committed\n", "uncommitted\n")
        port.export_bundle(self.source, self.bundle)
        _, metadata, paths = port.load_bundle(self.bundle)
        self.assertEqual(metadata["patches"][-1]["kind"], "worktree")
        self.assertEqual(metadata["patches"][-1]["untracked_paths"], ["new.txt"])
        self.assertNotIn(
            b"diff --git a/generated.txt", b"".join(path.read_bytes() for path in paths)
        )
        target = self.target()
        port.apply_bundle(self.bundle, target)
        self.assertEqual((target / "new.txt").read_text(), "new source\n")
        self.assertEqual(
            (target / "example.txt").read_text().splitlines()[30], "uncommitted"
        )

    def test_conflicts_leave_target_contents_and_index_untouched(self):
        self.change("line 30\n", "our edit\n")
        self.commit("fix(test): conflict")
        port.export_bundle(self.source, self.bundle)
        target = self.target()
        path = target / "example.txt"
        path.write_text(path.read_text().replace("line 30\n", "upstream edit\n"))
        self.git(target, "add", ".")
        self.git(target, "commit", "-m", "Conflicting upstream edit")
        index = (target / ".git/index").read_bytes()
        contents = path.read_bytes()
        with self.assertRaisesRegex(port.PortError, "preflight failed"):
            port.apply_bundle(self.bundle, target)
        self.assertEqual((target / ".git/index").read_bytes(), index)
        self.assertEqual(path.read_bytes(), contents)
        self.assertEqual(self.git(target, "status", "--porcelain=v1"), "")

    def test_three_way_replays_dependent_patches_on_changed_upstream_context(self):
        self.change("line 30\n", "first edit\n")
        self.commit("fix(test): first edit")
        self.change("first edit\n", "second edit\n")
        self.commit("fix(test): second edit")
        port.export_bundle(self.source, self.bundle)
        target = self.target()
        path = target / "example.txt"
        path.write_text(path.read_text().replace("line 27\n", "new upstream context\n"))
        self.git(target, "add", ".")
        self.git(target, "commit", "-m", "Adjust upstream context")
        self.assertTrue(port.preflight_bundle(self.bundle, target).use_three_way)
        port.apply_bundle(self.bundle, target)
        lines = path.read_text().splitlines()
        self.assertEqual(
            (lines[27], lines[30]), ("new upstream context", "second edit")
        )

    def test_bundle_checksums_series_order_and_old_bundle_compatibility(self):
        port.export_bundle(self.source, self.bundle)
        _, metadata, paths = port.load_bundle(self.bundle)
        original = paths[0].read_bytes()
        paths[0].write_bytes(original + b"tampered\n")
        with self.assertRaisesRegex(port.PortError, "checksum mismatch"):
            port.load_bundle(self.bundle)
        paths[0].write_bytes(original)
        (self.bundle / "series").write_text("not-the-correct-order.patch\n")
        with self.assertRaisesRegex(port.PortError, "series order"):
            port.load_bundle(self.bundle)
        metadata["schema_version"] = 1
        (self.bundle / port.BUNDLE_METADATA).write_text(json.dumps(metadata))
        target = self.target()
        port.apply_bundle(self.bundle, target)
        self.assertTrue((target / port.MANIFEST_PATH).is_file())


if __name__ == "__main__":
    unittest.main()
