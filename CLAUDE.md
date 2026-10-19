# difu

Your diff shifu: the `difu` terminal app and background agent service live in `cli/` (Rust); the iOS remote-control app lives in `app/`.

## Local env

- Run the Rust checks from `cli/` before every commit that touches it, in this order: `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`.
- Lints deny `indexing_slicing` and `panic` even in tests. Use `.get()`, slice patterns and `anyhow::bail!`, and write tests as `-> anyhow::Result<()>`.

## Git

- Branches follow `authorname/type/descriptive-name`, all kebab-case, with the author name taken from `git config user.name`: `ziad-altawil/feat/model-picker`, `ziad-altawil/fix/email-recipient-lookup-latency`. Types: `feature` (default) or its alias `feat`, `fix`, `hotfix`, `refactor`, `perf`, `chore`, `docs`. Use the `branch` skill (`/branch [type] description`) to create one.
- Run `git branch --show-current` before every commit/push and surface it. Never commit or push to `main` without an unambiguous "yes, push to main"; "commit and push" means the current feature branch. Applies in auto mode.
- Commits: present imperative, ≤70-char title leading with the user-facing outcome (`Add an Alt+M model picker shared by Codex and Claude Code`), no trailing period, bullet body for non-trivial changes, one-liner for trivial ones. Use the `commit` skill (`/commit`) to split mixed changes into atomic commits.
- Pull requests take their title from the commit title and go to `main`.
