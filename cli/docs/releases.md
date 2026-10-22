# Publishing a release

The release workflow builds and tests macOS and Linux binaries for x86_64 and
ARM64. Linux binaries use musl to avoid requiring a particular glibc version.
Packages contain difu, its license, README and third-party notices. Both macOS
archives also include `DifuNotifications.app`, compiled for their architecture
with a macOS 11 deployment target and an ad-hoc signature. This is not a
Developer ID signature or notarization. Rust and Swift run on CI; Git, `gh`, and
Codex remain user-installed runtime prerequisites.

1. Every pull request that changes difu is a release, so bump the version in
   `cli/Cargo.toml` and `cli/Cargo.lock` in that pull request (minor for
   features, patch for fixes) rather than in a separate release PR. Its CI then
   validates the release packages once; merge it after CI passes. Docs-only
   pull requests need no bump.
2. Push a `v<version>` tag pointing to that commit. The **Release packages**
   workflow checks the tag against the executable's version, tests all four
   platforms, and creates a draft GitHub release with the archives, `SHA256SUMS`,
   and `difu.rb`.
3. Review and publish the draft release. Download its archives to verify the
   checksums and executable before updating Homebrew.
4. Copy the release's `difu.rb` into `Formula/difu.rb` in
   [opencx-labs/homebrew-tap](https://github.com/opencx-labs/homebrew-tap), test the
   formula, and merge it. The formula installs the matching prebuilt binary into
   `bin` and the macOS notification helper into `libexec`. It declares no compiler
   or helper dependencies.

Users then install with `brew install opencx-labs/tap/difu` or upgrade with
`brew update` followed by `brew upgrade difu`. Each release needs its matching
formula update; publishing a GitHub release alone does not update the tap.

Do not add a Homebrew post-install service hook: Homebrew isolates that hook from
the user's home directory. The upgraded difu checks and refreshes an older
background service automatically on its next launch.

## Package verification

`python3 -m unittest discover -s cli/scripts -p 'test_*.py'` checks release
provenance, archive validation and generated Homebrew install behavior on both
platform branches. `smoke-release.py <extracted-package> <target> <version>` runs
from an unrelated directory with an empty PATH. It checks the executable version,
noninteractive refusal, macOS architecture/minimum OS/system dependencies and
helper metadata/signature. The helper's read-only status and invalid-request
responses run in both archive and relocated, symlinked Homebrew layouts. No
notification permission, alert, sound, login or actual installation is triggered.
Linux notifications use the user's desktop D-Bus session; optional standalone
sound uses `paplay`. Missing desktop/audio services are reported in Settings.
