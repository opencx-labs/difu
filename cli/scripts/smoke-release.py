"""Exercise extracted packages without a compiler, login, or notification prompt."""
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile


def run(args, *, cwd=None, empty_path=False, data=None):
    env = dict(os.environ)
    if empty_path:
        env["PATH"] = ""
    return subprocess.run(args, input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          cwd=cwd, env=env, timeout=12)


def helper_path(binary):
    directory = binary.resolve().parent
    for base in (directory, directory.parent / "libexec"):
        app = base / "DifuNotifications.app"
        if app.is_dir():
            return app
    raise ValueError("Packaged notification helper is missing")


def check_metadata(app):
    with (app / "Contents/Info.plist").open("rb") as source:
        metadata = plistlib.load(source)
    for key, value in {"CFBundleIdentifier": "app.difu.notifications",
                       "CFBundleExecutable": "DifuNotifications",
                       "CFBundlePackageType": "APPL",
                       "LSMinimumSystemVersion": "11.0", "LSUIElement": True}.items():
        if metadata.get(key) != value:
            raise ValueError(f"Incorrect notification bundle metadata: {key}")
    executable = app / "Contents/MacOS/DifuNotifications"
    if not executable.is_file() or not os.access(executable, os.X_OK):
        raise ValueError("Notification helper is not executable")
    return executable


def check_macho(binary, architecture):
    subprocess.run(["/usr/bin/lipo", str(binary), "-verify_arch", architecture], check=True)
    output = subprocess.check_output(["/usr/bin/otool", "-l", str(binary)], text=True)
    minimums = []
    for block in output.split("Load command"):
        if "LC_BUILD_VERSION" in block:
            minimums.extend(re.findall(r"^\s*minos (\d+\.\d+(?:\.\d+)?)", block, re.MULTILINE))
        elif "LC_VERSION_MIN_MACOSX" in block:
            minimums.extend(re.findall(r"^\s*version (\d+\.\d+(?:\.\d+)?)", block, re.MULTILINE))
    if not minimums or any(tuple(map(int, value.split(".")[:2])) > (11, 0) for value in minimums):
        raise ValueError(f"Unexpected minimum macOS version: {minimums}")
    dependencies = subprocess.check_output(["/usr/bin/otool", "-L", str(binary)], text=True)
    if "/opt/homebrew/" in dependencies or "/usr/local/" in dependencies:
        raise ValueError("Release depends on a build-machine library")


def check_helper(binary, cwd, architecture):
    app = helper_path(binary)
    executable = check_metadata(app)
    subprocess.run(["/usr/bin/codesign", "--verify", "--strict", str(app)], check=True)
    check_macho(executable, architecture)
    # None of these requests authorize notifications, deliver alerts, or play audio.
    for payload, error in ((b'{"action":"status"}', None),
                           (b'{"action":"unknown"}', "Unknown notification action"),
                           (b'{', "Invalid notification request")):
        result = run([str(executable)], cwd=cwd, empty_path=True, data=payload)
        if result.returncode != 0:
            raise ValueError("Packaged notification helper failed")
        reply = json.loads(result.stdout)
        if not isinstance(reply.get("desktop"), str) or not isinstance(reply.get("sound"), str):
            raise ValueError("Invalid notification status response")
        if error is not None and reply.get("error") != error:
            raise ValueError("Notification helper did not reject the invalid request")
        if error is None and reply.get("desktop") == "Not checked":
            raise ValueError("Notification status was not queried")


def smoke(package, target, version):
    binary = package.resolve() / "difu"
    with tempfile.TemporaryDirectory(prefix="difu-package-smoke-") as scratch:
        scratch = Path(scratch)
        version_result = run([str(binary), "--version"], cwd=scratch, empty_path=True)
        if version_result.returncode or version_result.stdout.decode().strip() != f"difu {version}":
            raise ValueError("Packaged executable has the wrong version or cannot start")
        refusal = run([str(binary)], cwd=scratch, empty_path=True, data=b"")
        if refusal.returncode != 1 or b"difu needs an interactive terminal" not in refusal.stderr:
            raise ValueError("Packaged executable does not refuse noninteractive startup safely")
        if target.endswith("apple-darwin"):
            architecture = "arm64" if target.startswith("aarch64") else "x86_64"
            check_macho(binary, architecture)
            check_helper(binary, scratch, architecture)
            # Recreate the formula's relocated Cellar layout without installing anything.
            cellar = scratch / "relocated/Cellar/difu" / version
            (cellar / "bin").mkdir(parents=True)
            (cellar / "libexec").mkdir()
            shutil.copy2(binary, cellar / "bin/difu")
            shutil.copytree(helper_path(binary), cellar / "libexec/DifuNotifications.app")
            linked = scratch / "bin/difu"
            linked.parent.mkdir()
            linked.symlink_to(cellar / "bin/difu")
            check_helper(linked, scratch, architecture)
            if run([str(linked), "--version"], cwd=scratch, empty_path=True).returncode:
                raise ValueError("Relocated Homebrew binary cannot start through its symlink")
        elif (package / "DifuNotifications.app").exists():
            raise ValueError("Linux package unexpectedly contains a macOS helper")
    print(f"Package smoke passed: {target} {version}")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("Usage: smoke-release.py <extracted-package> <target> <version>")
    smoke(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
