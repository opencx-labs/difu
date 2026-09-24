# Publishing a release

The release workflow builds and tests macOS and Linux binaries for x86_64 and
ARM64. Linux binaries use musl to avoid requiring a particular glibc version.
Packages contain only difu, its license, and README. Rust runs on CI; Git, `gh`,
and Codex remain user-installed runtime prerequisites.

1. Update the version in `Cargo.toml` and `Cargo.lock` and merge the change after
   CI passes. Pull requests also validate release packaging when relevant files
   change.
2. Push a `v<version>` tag pointing to that commit. The **Release packages**
   workflow checks the tag against the executable's version, tests all four
   platforms, and creates a draft GitHub release with the archives, `SHA256SUMS`,
   and `difu.rb`.
3. Review and publish the draft release. Download its archives to verify the
   checksums and executable before updating Homebrew.
4. Copy the release's `difu.rb` into `Formula/difu.rb` in
   [opencx-labs/homebrew-tap](https://github.com/opencx-labs/homebrew-tap), test the
   formula, and merge it. The formula installs the matching prebuilt binary and
   declares no compiler or helper dependencies.

Users then install with `brew install opencx-labs/tap/difu` or upgrade with
`brew update` followed by `brew upgrade difu`. Each release needs its matching
formula update; publishing a GitHub release alone does not update the tap.

Do not add a Homebrew post-install service hook: Homebrew isolates that hook from
the user's home directory. The upgraded difu checks and refreshes an older
background service automatically on its next launch.
