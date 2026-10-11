"""Record release provenance and reuse successful PR packages for identical source."""

import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parent.parent
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
)
MANIFEST = "source.json"


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def api(path):
    return json.loads(subprocess.check_output(["gh", "api", path]))


def digest(data):
    return hashlib.sha256(data).hexdigest()


def filenames(version):
    return {f"difu-{version}-{target}.tar.gz" for target in TARGETS} | {
        "SHA256SUMS", "difu.rb"
    }


def record(dist, tree, version):
    manifest = {
        "schema": 1,
        "tree": tree,
        "commit": git("rev-parse", "HEAD"),
        "version": version,
        "run_id": int(os.environ["GITHUB_RUN_ID"]),
        "run_attempt": int(os.environ["GITHUB_RUN_ATTEMPT"]),
        "files": {name: digest((dist / name).read_bytes()) for name in sorted(filenames(version))},
    }
    (dist / MANIFEST).write_text(json.dumps(manifest, indent=2) + "\n")


def eligible(artifact, run, workflow_id):
    return (
        not artifact["expired"]
        and run["id"] == artifact["workflow_run"]["id"]
        and run["workflow_id"] == workflow_id
        and run["event"] == "pull_request"
        and run["status"] == "completed"
        and run["conclusion"] == "success"
    )


def verify(data, artifact, run, tree, version, repo):
    # Check the immutable GitHub artifact, then its exact contents. Never extract paths
    # or execute code from an artifact in the release job.
    archive_digest = artifact.get("digest")
    if archive_digest and archive_digest != f"sha256:{digest(data)}":
        raise ValueError("GitHub artifact checksum mismatch")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        expected = filenames(version) | {MANIFEST}
        if set(archive.namelist()) != expected or len(archive.namelist()) != len(expected):
            raise ValueError("Unexpected or missing release files")
        files = {name: archive.read(name) for name in expected}
    source = json.loads(files[MANIFEST])
    if (
        source["schema"] != 1
        or source["tree"] != tree
        or source["version"] != version
        or source["run_id"] != run["id"]
        or source["run_attempt"] != run["run_attempt"]
    ):
        raise ValueError("Source, version, or workflow attempt does not match")
    if source["files"] != {name: digest(files[name]) for name in filenames(version)}:
        raise ValueError("Release file checksum mismatch")
    commit = source["commit"]
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Invalid source commit")
    built = api(f"repos/{repo}/git/commits/{commit}")
    if built["tree"]["sha"] != tree or run["head_sha"] not in {
        commit, *(parent["sha"] for parent in built["parents"])
    }:
        raise ValueError("Build commit is not the matching PR source")
    return files


def reuse(dist, tree, version, repo):
    name = f"release-{tree}"
    workflow = api(f"repos/{repo}/actions/workflows/release.yml")
    page = 1
    while True:
        result = api(f"repos/{repo}/actions/artifacts?name={name}&per_page=100&page={page}")
        artifacts = result["artifacts"]
        for artifact in artifacts:
            if artifact["name"] != name or artifact["expired"]:
                continue
            try:
                run = api(f"repos/{repo}/actions/runs/{artifact['workflow_run']['id']}")
                if not eligible(artifact, run, workflow["id"]):
                    continue
                data = subprocess.check_output([
                    "gh", "api", f"repos/{repo}/actions/artifacts/{artifact['id']}/zip"
                ])
                files = verify(data, artifact, run, tree, version, repo)
            except (subprocess.CalledProcessError, ValueError, KeyError, TypeError, zipfile.BadZipFile):
                print(f"Artifact {artifact['id']} could not be validated; trying the next candidate.")
                continue
            dist.mkdir(parents=True, exist_ok=True)
            for filename, content in files.items():
                (dist / filename).write_bytes(content)
            print(f"Reusing validated release packages from PR run {run['id']} (artifact {artifact['id']}).")
            return True
        if len(artifacts) < 100:
            return False
        page += 1


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in {"record", "reuse"}:
        raise SystemExit("Usage: release-artifacts.py <record|reuse> <artifact-directory>")
    command, directory = sys.argv[1:]
    dist = Path(directory)
    tree = git("rev-parse", "HEAD^{tree}")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise SystemExit("Expected a stable major.minor.patch release version")
    if command == "record":
        record(dist, tree, version)
    else:
        if os.environ["GITHUB_REF_NAME"] != f"v{version}":
            raise SystemExit("Release tag must match the Cargo package version")
        try:
            found = reuse(dist, tree, version, os.environ["GITHUB_REPOSITORY"])
        except (subprocess.CalledProcessError, ValueError, KeyError, TypeError):
            print("Artifact lookup unavailable; rebuilding release packages.")
            found = False
        if not found:
            print("No matching validated PR packages available; rebuilding without the full test suite.")
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"reused={str(found).lower()}\n")
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"artifact_name=release-{tree}\n")


if __name__ == "__main__":
    main()
