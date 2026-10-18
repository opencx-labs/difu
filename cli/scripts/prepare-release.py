"""Generate release checksums and a binary-only Homebrew formula from CI archives."""

import hashlib
from pathlib import Path
import re
import sys
import tomllib


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python3 scripts/prepare-release.py <artifact-directory>")
    dist = Path(sys.argv[1])
    root = Path(__file__).resolve().parent.parent
    version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise SystemExit("Expected a stable major.minor.patch release version")
    targets = {
        "macos": {"arm": "aarch64-apple-darwin", "intel": "x86_64-apple-darwin"},
        "linux": {
            "arm": "aarch64-unknown-linux-musl",
            "intel": "x86_64-unknown-linux-musl",
        },
    }
    formula = [
        "class Difu < Formula",
        '  desc "Codex agents and guided pull request reviews in the terminal"',
        '  homepage "https://github.com/opencx-labs/difu"',
        f'  version "{version}"',
        '  license "MIT"',
    ]
    checksums = []
    for system, architectures in targets.items():
        formula.extend(["", f"  on_{system} do"])
        for architecture, target in architectures.items():
            name = f"difu-{version}-{target}.tar.gz"
            archive = dist / name
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            checksums.append(f"{digest}  {name}\n")
            formula.extend(
                [
                    f"    on_{architecture} do",
                    f'      url "https://github.com/opencx-labs/difu/releases/download/v{version}/{name}"',
                    f'      sha256 "{digest}"',
                    "    end",
                ]
            )
        formula.append("  end")
    formula.extend(
        [
            "",
            "  def install",
            '    bin.install "difu"',
            "  end",
            "",
            "  def caveats",
            "    <<~EOS",
            "      Git, GitHub CLI (gh), and Codex CLI must already be on your PATH.",
            "      Authenticate if needed with `gh auth login` and `codex login`.",
            "      Run `difu` to open Agents; switch to Reviews for pull requests.",
            "    EOS",
            "  end",
            "",
            "  test do",
            '    assert_match "difu #{version}", shell_output("#{bin}/difu --version")',
            '    assert_match "difu needs an interactive terminal", shell_output("#{bin}/difu 2>&1", 1)',
            "  end",
            "end",
            "",
        ]
    )
    (dist / "SHA256SUMS").write_text("".join(checksums))
    (dist / "difu.rb").write_text("\n".join(formula))


if __name__ == "__main__":
    main()
