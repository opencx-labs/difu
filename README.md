# difu

*dīfu* is your diff *shīfu*... guides you through the depths of every diff... may you fathom whatever slop you're feasting your eyes on



## Run

Requires macOS or Linux, Git, [GitHub CLI](https://cli.github.com/), and an installed
[Codex CLI](https://github.com/openai/codex) with `exec` and `app-server` support.
Log in once with `gh auth login` and `codex login`.

Build with Rust 1.88 or newer:

```sh
cargo install --path . --locked
difu
```

`difu` opens your direct review requests across GitHub repositories. Select a PR
to preview its description, chronological activity, inline review comments, and
checks. Enter opens the diff and starts its guide. You can also launch directly:

```sh
difu https://github.com/owner/repo/pull/123
difu 123 # uses the GitHub repository in your current directory
```

The first time you open a repository, supply its existing local clone path.
Difu remembers it. It fetches missing PR commits using your `gh` authentication;
it never clones repositories automatically. GitHub.com is supported in v1.
The inbox contains up to GitHub search's limit of 1,000 open direct review requests.

## Reading a PR

- **Overview:** description, chronological comments/reviews/commits, then checks.
  Check states and durations refresh every ten seconds for the selected PR.
  Click a check to open its GitHub logs.
- **Guide:** chapters connect explanations to changes across files. While
  generation runs, the file navigator and diff stay usable. Completed guides
  scroll continuously, with the current explanation alongside its code.
- **Diff:** browse changed files without the guide. Side-by-side is the default;
  unified is selectable. Narrow windows temporarily use unified diffs and put
  chapter explanations above code, restoring your preference when widened.

Guide generation has no automatic timeout. Its elapsed time and activity remain
visible; cancel or retry explicitly. Every changed hunk must appear exactly once
in the guide. Binary files, renames, and permission changes have metadata review
units. Unknown, duplicate, or missing references reject the guide.

New revisions are announced without replacing the snapshot you are reading.
Refresh explicitly to load the newer diff and its matching guide.

### Controls

Buttons, tabs, files, PRs, and links are clickable; the mouse wheel scrolls the
pane under the pointer. Keyboard controls use arrows and shortcuts, with no Vim
bindings.

| Key | Action |
| --- | --- |
| Up / Down | Select a PR/file or scroll content |
| Tab / Shift+Tab | Switch navigation/content focus |
| Enter | Open selected PR |
| Page Up / Page Down / Space | Scroll a page |
| Home / End | Start / end |
| Left / Right | Scroll code horizontally |
| 1 / 2 / 3 | Overview / Guide / Diff |
| F1 | Help |
| F2 | Model and reasoning picker |
| F5 | Refresh |
| F6 | Regenerate / retry |
| F7 | Choose a local clone path |
| F8 | Cancel generation or snapshot preparation |
| Ctrl+B | Side-by-side / unified preference |
| Ctrl+O | Open the PR on GitHub |
| Esc | Close dialog / return to overview / quit |
| Ctrl+C | Quit and clean up active work |

In the clone dialog, paste a path, use Ctrl+U to clear it, then Enter. Some
terminals require Fn with function keys; the footer also offers clickable actions.

## Codex and caching

The default is **Luna High** (`gpt-5.6-luna`, `high`). F2 discovers the models and
reasoning levels available through your installed Codex. Luna High is pinned as
a recommendation when available. Choosing a model saves it for the next opening
or explicit regeneration; it does not silently restart a running generation.
An unavailable model produces an error, with no automatic model substitution.

Difu invokes `codex exec` non-interactively, using the existing login, an ephemeral
session, a read-only sandbox, and a strict JSON output schema. User config and
execution-policy rules are ignored for generation. Project instruction loading is
disabled, and the clone/worktree are marked untrusted for this invocation so their
Codex configuration is skipped. Configured MCP server names are read and each
server is explicitly disabled; the read-only guide instructions are supplied as
developer instructions. See the [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).
Apps, plugins, hooks, multi-agent
execution, memories, browser/computer use, web search, and MCP servers are disabled
for that invocation. No Codex settings or authentication files are rewritten.
The PR description, diff, and repository context Codex reads are sent through
your Codex account to generate the guide.

Guides persist locally. A cache key includes the PR identity, base/head/merge-base,
complete parsed diff, PR title/description, model, reasoning level, prompt, and
schema. Cached guides are validated again before use. F6 bypasses the cache.
Damaged cache entries produce an error and can be replaced with F6.

Settings and guides use the OS's standard per-user directories:

| OS | Settings | Guides |
| --- | --- | --- |
| macOS | `~/Library/Application Support/difu/config.json` | `~/Library/Caches/difu/` |
| Linux | `$XDG_CONFIG_HOME/difu/config.json` (default `~/.config`) | `$XDG_CACHE_HOME/difu/` (default `~/.cache`) |

Settings remember clone paths, model/effort, and diff preference. Writes are atomic
and files are created with owner-only permissions. Guide cache files contain PR
explanations and hunk references; remove the cache directory to clear them.

## Worktrees and safety

Guide generation creates a detached temporary worktree at the exact PR head.
It shares Git objects/history with your clone, while using separate disk space
for tracked files. It does not copy your untracked files, dependencies, or build
outputs. Disk use and preparation time depend on repository size; there is no
second full history clone.

Your original branch, index, and working files are preserved. Fetching adds Git
objects; worktree preparation adds temporary Git administrative records. Hooks,
checkout filters, recursive submodules, and LFS downloads are disabled. The guide
therefore sees committed LFS pointers and submodule references, not downloaded
assets or nested checkouts. Difu does not run project builds or tests.

Owned subprocess groups are terminated on cancellation. Worktrees are removed on
completion, failure, normal exit, Ctrl+C, SIGTERM, and SIGHUP. Cleanup does not use
`--force`: if a worktree unexpectedly contains changes, difu reports and preserves
its path. An uncatchable kill or power loss can leave a temporary worktree; inspect
`git worktree list` and remove that specific worktree manually after inspection.

The first-party Rust code forbids `unsafe`. Clippy denies `.unwrap()`, `.expect()`,
explicit panics, unchecked indexing/slicing, `todo!`, and `unimplemented!`, including
in tests. CI treats all warnings as errors. These rules do not establish that
third-party libraries are free of unsafe code or that an AI explanation is correct.
Coverage validation checks references and omissions; reviewers still assess the
explanation against the code.

Shallow history with no merge base and non-UTF-8 diffs produce explicit errors;
difu does not silently substitute an incomplete or lossy snapshot. The tool is a
reader: it does not submit comments, reviews, approvals, or mark review progress.

## Guide writing research

The writing instructions in [prompts/guide.md](prompts/guide.md) use the approved
reference recording's three visible Linear Guide chapters and the corresponding
PR description. The connected Linear API exposed PR metadata and descriptions,
not generated Guide chapters. This is a limited sample, not a reconstruction of
Linear's full guide-writing system. The observed style connects logical changes
in dependency order, using short causal explanations and links across files.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

The workflow test uses Python 3 to script GitHub/Codex responses, alongside real
local Git repositories. Tests do not require GitHub credentials or AI calls.
CI runs the checks on macOS and Linux.

An additional live smoke test is ignored by default. It uses your Codex login and
one Luna High generation against a tiny synthetic repository:

```sh
cargo test --locked --test codex_smoke -- --ignored --nocapture
```
