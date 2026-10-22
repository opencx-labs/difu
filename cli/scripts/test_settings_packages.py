"""Verify the generated install recipe and relocatable helper contract."""
import importlib.util
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


prepare = module("prepare_release", "prepare-release.py")
smoke = module("smoke_release", "smoke-release.py")


class SettingsPackageTests(unittest.TestCase):
    def app(self, root):
        app = root / "DifuNotifications.app"
        (app / "Contents/MacOS").mkdir(parents=True)
        source = Path(__file__).resolve().parents[1] / "native/notifications/Info.plist"
        (app / "Contents/Info.plist").write_bytes(source.read_bytes())
        executable = app / "Contents/MacOS/DifuNotifications"
        executable.write_text("fixture")
        executable.chmod(0o755)
        return app

    def test_archive_and_relocated_cellar_symlink_find_executable_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app = self.app(root)
            binary = root / "difu"
            binary.touch()
            self.assertEqual(smoke.helper_path(binary), app.resolve())
            smoke.check_metadata(app)
            cellar = root / "relocated/Cellar/difu/0.26.0"
            (cellar / "bin").mkdir(parents=True)
            (cellar / "libexec").mkdir()
            binary.rename(cellar / "bin/difu")
            app.rename(cellar / "libexec/DifuNotifications.app")
            binary.symlink_to(cellar / "bin/difu")
            self.assertEqual(smoke.helper_path(binary), (cellar / "libexec/DifuNotifications.app").resolve())
            smoke.check_metadata(smoke.helper_path(binary))

    def test_missing_nonexecutable_or_wrong_identity_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(ValueError, "missing"):
                smoke.helper_path(root / "difu")
            app = self.app(root)
            executable = smoke.check_metadata(app)
            executable.chmod(0o644)
            with self.assertRaisesRegex(ValueError, "executable"):
                smoke.check_metadata(app)
            executable.chmod(0o755)
            path = app / "Contents/Info.plist"
            values = plistlib.loads(path.read_bytes())
            values["CFBundleIdentifier"] = "wrong.identity"
            path.write_bytes(plistlib.dumps(values))
            with self.assertRaisesRegex(ValueError, "CFBundleIdentifier"):
                smoke.check_metadata(app)

    def test_minimum_os_ignores_linker_version_but_rejects_newer_requirement(self):
        for command in ("LC_BUILD_VERSION\n minos {}\n sdk 26.5\n tool 3\n version 1267.0",
                        "LC_VERSION_MIN_MACOSX\n version {}\n sdk 26.5"):
            for minimum in ("11.0", "12.0"):
                with patch.object(smoke.subprocess, "run"), patch.object(
                        smoke.subprocess, "check_output", side_effect=[
                            "Load command 10\n " + command.format(minimum),
                            "/usr/lib/libSystem.B.dylib"]):
                    if minimum == "11.0":
                        smoke.check_macho(Path("difu"), "arm64")
                    else:
                        with self.assertRaisesRegex(ValueError, "minimum macOS"):
                            smoke.check_macho(Path("difu"), "arm64")

    def test_generated_formula_installs_actual_bundle_only_on_macos(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            import tomllib
            version = tomllib.loads((Path(__file__).resolve().parents[1] / "Cargo.toml").read_text())["package"]["version"]
            for target in ("aarch64-apple-darwin", "x86_64-apple-darwin",
                           "aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"):
                (root / f"difu-{version}-{target}.tar.gz").write_bytes(target.encode())
            with patch.object(prepare.sys, "argv", ["prepare-release.py", str(root)]):
                prepare.main()
            self.app(root)
            (root / "difu").write_text("fixture binary")
            # Execute the real generated install method against an isolated Formula adapter.
            adapter = r'''
require 'fileutils'
require 'pathname'
module OS
  def self.mac?; ENV.fetch('DIFU_TEST_MAC') == '1'; end
end
class Formula
  def self.test(&block); end
  def self.method_missing(*args, &block); end
  def bin; Destination.new('installed/bin'); end
  def libexec; Destination.new('installed/libexec'); end
end
class Destination
  def initialize(path); @path = path; end
  def install(source)
    FileUtils.mkdir_p(@path)
    FileUtils.cp_r(source, @path)
  end
end
load './difu.rb'
Difu.new.install
'''
            for mac in (True, False):
                import shutil
                shutil.rmtree(root / "installed", ignore_errors=True)
                env = dict(os.environ, DIFU_TEST_MAC="1" if mac else "0")
                result = subprocess.run(["ruby", "-e", adapter], cwd=root, env=env,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                self.assertTrue((root / "installed/bin/difu").exists())
                self.assertEqual((root / "installed/libexec/DifuNotifications.app").exists(), mac)


if __name__ == "__main__":
    unittest.main()
