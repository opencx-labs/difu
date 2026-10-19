---
name: commit
description: Create git commits in the repo's commit style (root CLAUDE.md → Git). Detects mixed concerns and splits into atomic commits. Use when the user wants to commit or save work, or to split changes into atomic commits.
user-invocable: true
---

# Git Commit

Creates clean, atomic git commits in the repo's commit style. Detects mixed concerns and splits into multiple commits when needed.

## Consent

Invoking `/commit` (or any other explicit instruction to commit) is itself the consent — proceed without asking. This also grants session-wide consent for further git writes (push, follow-up commits, PRs) without re-asking. The per-session confirmation rule in `CLAUDE.md` only applies when there's no explicit user instruction to write to git.

## Workflow

### 1. Analyze Changes

```bash
git status && git diff && git diff --staged
```

### 2. Detect Mixed Concerns

Scan changed files for mixing:
- Multiple features
- Bug fixes + features
- Refactoring + new functionality
- Multiple unrelated bug fixes
- Code + unrelated documentation
- Tests for different features

**If single concern** → skip to step 4 (single commit).
**If mixed concerns** → continue to step 3.

### 3. Group Into Atomic Commits

Group files by shared purpose. Present grouping to user for confirmation.

Example:
```
Group 1: "open the model picker with Alt+M"
  → cli/src/agents/ui.rs, cli/src/agents/ui/models.rs

Group 2: "fix the Linux build of remote host discovery"
  → cli/src/agents/remote/discovery.rs
```

Wait for user confirmation before proceeding.

### 4. For Each Commit Group

1. **Branch** — run `git branch --show-current` and surface it. If it is `main`, create a branch first with the `branch` skill (`authorname/type/descriptive-name`).
2. **Stage** — `git add <file1> <file2>` (NEVER `git add -A` or `git add .`). Use `git add -p` when a file contains changes for multiple groups.
3. **Review** — `git diff --staged`
4. **Format, lint and test** the touched areas before commit (root `CLAUDE.md` → Local env):
   - `cli/` touched → from `cli/`: `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`
   - `app/` touched → build the iOS app target that changed
   - Do not commit until these pass for the changed areas.
5. **Commit** — title + body per [Format](#format)
6. **Verify** — `git log -1 --oneline`

### 5. Final Check

```bash
git log --oneline -n <N>  # where N = number of commits created
```

## Format

Follow root `CLAUDE.md` → Git: a present-imperative title of at most 70 characters that leads with the user-facing outcome, no trailing period; a bullet body for non-trivial changes, a one-liner for trivial ones.

```
Add an Alt+M model picker shared by Codex and Claude Code
Fix the Linux build of remote host discovery
```

## Issue Integration

- Check user message or branch name for issue/ticket number
- If provided (e.g. `/commit DX-523`), add `Linear: DX-523` at end of commit body
- If no issue found: ask if user wants to link one
- If user declines, leave it blank

## Atomic Commit Rules

- ONE logical change per commit
- Each commit leaves codebase in a working state (builds and tests pass)
- Can be reverted independently
- NEVER mix unrelated concerns
- NEVER commit secrets (.env, credentials)

## Closing

After all commits: "Created N atomic commit(s). Would you like to review history, push, or create PR?"
