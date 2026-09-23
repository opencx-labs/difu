# difu

*dīfu* is your diff *shīfu*... guides you through the depths of every diff... may you fathom whatever slop you're feasting your eyes on

A terminal workspace for Codex agents and GitHub pull request reviews, built in
Rust with Ratatui and Crossterm. Launch background coding agents, browse your
review inbox, read PR activity, and follow AI-generated
chapters that explain related changes across files. Write comments and reviews,
track chapter completion, and merge or close PRs without leaving the terminal.

## Requirements

- **macOS or Linux** with an interactive terminal.
- **Git** and an existing local clone of any repository whose diff you want to open.
- **[GitHub CLI](https://cli.github.com/)** (`gh`) for GitHub authentication and PR data.
- **[Codex CLI](https://github.com/openai/codex)** for guide generation using your
  existing login. The integration has been tested with Codex CLI 0.155.1.
- **[Rust 1.90 or newer](https://www.rust-lang.org/tools/install)** when building
  from source.

Git, `gh`, and `codex` must be available on your `PATH`. If you use Homebrew, install
these prerequisites with:

```sh
brew install git gh
brew install --cask codex
```

See the Homebrew listings for [GitHub CLI](https://formulae.brew.sh/formula/gh)
and [Codex CLI](https://formulae.brew.sh/cask/codex).

## Installation

### Homebrew

```sh
brew install opencx-labs/tap/difu
```

Homebrew adds the tap automatically and installs a prebuilt binary. **Rust is not
required.** Git, `gh`, and Codex must already be installed and available on your
`PATH`; the difu package does not install or bundle them.

Prebuilt packages are available for Intel/AMD (x86_64) and ARM64 on macOS and
Linux. You can also download them from [GitHub Releases](https://github.com/opencx-labs/difu/releases).

### Build from source

With Git and Rust 1.90+ installed:

```sh
git clone https://github.com/opencx-labs/difu.git
cd difu
cargo install --path . --locked
```

Cargo installs the executable into its binary directory, normally `~/.cargo/bin`.
Make sure that directory is on your `PATH`.

### First-time login

Authenticate GitHub CLI and Codex once, unless you are already logged in:

```sh
gh auth login
codex login
```

Difu reuses those logins for coding and reviews. Optional voice dictation uses a
separate OpenAI API key and API billing.

## Quick start

```sh
difu
```

`difu` opens **Agents** on first launch and remembers your last **Agents / Reviews**
tab thereafter. Click a tab or use **Ctrl+1 / Ctrl+2** to switch while preserving
your place. An explicit PR argument opens Reviews.

### Agents

The session list excludes Guide-generation jobs; guides remain available in Reviews.
The bottom **Shells** control lists Codex’s open background shells and opens their
available streamed output. **Artifacts** lists HTML deliverables explicitly
registered by new coding sessions. Codex is instructed to prefer self-contained
HTML for reports and visual deliverables. Existing sessions keep their original
tool catalog.

Shells and HTML artifacts can use the right pane (replacing Changes) or the main
conversation area. **Alt+P** switches placement and remembers the choice; **Esc**
returns to the conversation. Artifact previews offer **Refresh** and **Open in
browser**; edits do not automatically reload the page. Embedded HTML uses the
optional [terminal-browser](https://github.com/zenbu-labs/terminal-browser) package
and a terminal with Kitty graphics support, such as Ghostty. When the package is
missing, macOS offers an explicit Homebrew installation (about 140 MB download,
339 MB installed), external browser, or Cancel. Linux shows installation
instructions and the external-browser option. difu launches the installed engine
directly, without the CLI’s automatic skill installation or terminal setup. The
browser package is never bundled with difu. Without a working embedded browser, the local HTML can still be opened in your external browser.

**/actions → Delete chat and clean up worktree** confirms stopping the agent and
permanently deleting its difu history, questions, queue, and attachment copies.
Cleanup removes only a clean, unlocked difu-owned worktree. Modified, untracked,
and ignored files prevent deletion and retain the chat. Existing directories,
Git branches and commits, and Codex’s own history are retained. Archive remains
a separate action that keeps the chat and worktree.

Press **n** (or `/new`) to open an empty session immediately. Repository, model,
reasoning, and isolation defaults live in **/actions → Default repository, model,
reasoning and worktree**. Without a saved repository, difu uses the current Git
repository; outside one, it asks you to choose and remembers that choice.

No Codex turn runs until your first message. With isolation enabled, questions and
investigation run read-only in the original repository. When the agent requests
editing, difu stops the read-only turn, creates a branch/worktree from the pinned
starting commit, and continues the same conversation there. No clone or fetch is
needed. Existing local changes are never copied into that worktree. Missing local
guidance still requires your approval before copying. With isolation disabled,
the session works in the selected repository directly.

Model and reasoning inherit your Codex configuration unless overridden. During
read-only investigation, filesystem writes and permission escalation are disabled.
After switching to the worktree, difu restores the inherited sandbox and approval
settings. A restart never automatically replays a task or workspace transition.

The searchable session list previews conversations. **Enter** opens a session;
**Esc** returns. The conversation stays in the center, with independently toggled
side panes: **Ctrl+B** for agents and **Ctrl+D** for Changes. Visibility is remembered.
Changes compares the workspace with its session-start commit and includes later
commits, staged/unstaged edits, and non-ignored untracked files. It never stages
files or changes your index. Changes display is limited to 32 MiB; larger results
produce an explicit error.

In the composer, **Enter** sends a message and steers an active turn immediately.
**Ctrl+Enter** explicitly queues it for the next turn; **Shift+Enter** inserts a
newline. Drafts remain until delivery is acknowledged. Shift+arrows selects text;
typing replaces the selection, and Command+C copies it. **Shift+Alt+Left/Right**
extends the selection by a word; **Alt+Backspace** deletes the previous word.
**Cmd+Backspace** deletes the entire current line; **Cmd+Left/Right** moves to its
start/end. Ghostty’s translated **Ctrl+U** and **Ctrl+A/E** work too;
**Ctrl+U** still clears filter/search inputs. **Cmd+Z** undoes edits in all text inputs.
With an empty composer, **Up** recalls sent prompts from the current session.
**Down** moves forward and restores your original draft past the newest prompt.
Recalled prompts are editable and never sent automatically.

Type **/** in an empty composer for native commands: `/compact`, `/model`, `/effort`,
`/skills`, `/status`, `/diff`, `/new`, `/rename`, `/help`, `/voice`, and `/actions`.
Commands filter as you type. Backspace removes an empty `/`, `$`, or `@` trigger.
A slash inside an existing message stays literal. `/model` and `/effort` suggest
Codex’s available models and supported reasoning levels: type to filter, use
Up/Down to select, Enter to fill, and Enter again to apply.
The skills picker discovers enabled skills for the session's actual directory;
selecting one attaches it to the draft without sending. `/actions` (or Esc then /)
opens searchable session controls: continue, interrupt, rename, change model,
archive/unarchive, toggle panes, and delete a clean inactive worktree.

In Agents, `/` commands, `$` workspace skills, and `@` workspace files and folders
appear above the message input. Choose a suggestion with arrows and Enter;
selecting a skill or path inserts it without sending the message.
The session list puts active agents first and shows turn time beside the title,
with cached green additions and red deletions below. Local Git counts refresh
every ten seconds while working and after a turn finishes. Archived sessions
remain available through the actions menu.


Conversation tools show their action, target, state, and duration; click or press
Enter to expand details. Up/Down focuses transcript entries, PageUp/PageDown and
the wheel scroll, and End returns to live output. Reading earlier output keeps
your place. **Alt+Up** opens pending questions in place of the composer. Answer one question
at a time with **Enter**, or skip it without answering with **Ctrl+]**.
On a suggested choice, **n** opens a note beneath it. **Enter** submits the choice
and its note together; **Esc** returns to the choices while keeping the note.
**Alt+Up** advances; **Alt+Down** returns to the previous question, or the composer
from the first question. Question drafts survive dismissal and session switching.
Asynchronous question cards remain available after the agent finishes its turn
and across service restarts. Answers steer a running turn or start a follow-up
when idle, using the normal chat delivery path. Only the question, chosen answer,
and any additional note are sent.
Outgoing queued messages have a separate editable queue. Down from the final
transcript entry focuses the composer. Up in an empty composer recalls this
session’s previous prompts; Down walks forward and restores your draft.

Codex keeps its normal global configuration and memory settings and reads
AGENTS.md and skills from the session workspace. Before launching an isolated
session, difu asks before copying any missing untracked or ignored guidance from
the source clone. It copies only the listed guidance files after agreement.

 **?** opens
searchable shortcut help. **Tab** switches directly between the session list and
the selected session’s input (opening it if needed). **Cmd+Up/Down** focuses and navigates message blocks. The focused block has a dim
background; moving down past the final block returns to the input. Typing while
reading messages focuses the composer and inserts your text; navigation and
modified shortcuts keep their existing behavior. **f** filters sessions, and **r**
refreshes/reconnects. In Changes, arrows scroll, Shift+Up/Down selects lines, and
**c / Command+C** copies through the terminal clipboard protocol.

#### Messages, attachments, and session names

User messages have a dim green background with white text. Your newest sent prompt
stays pinned above the conversation; click it to read the full prompt. Drag across
conversation text and press **Ctrl+C / Cmd+C** to copy. Ctrl+C quits only when no
chat text is selected.

The composer starts at one line and grows up to ten lines. Pasted code receives
syntax highlighting. Pastes of **500 characters or more** appear as
`[Pasted content · N chars]`; sending and copying include the full text. Press
**Ctrl+V / Cmd+V** on or immediately after a token to expand it. Elsewhere these
keys paste normally (Command shortcuts depend on terminal forwarding).

Paste/drop local image or video paths, or use **Ctrl+V / Cmd+V** for clipboard
images. Removable `[image 1]` and `[video 1]` tokens appear above the input. Difu
keeps private session copies, including unsent attachment drafts, so original
files can move or change. Nothing is sent to Codex before you send the message.
Images use Codex’s image input; videos are supplied as local paths, without frame
extraction or a promise that every Codex model can interpret the video. Copies
remain until explicit workspace cleanup.

After the first successful coding turn, **Luna Medium** generates a short title
from the task and response. Naming runs in the background once. Failure keeps the
existing title, and a manual rename always wins.

#### Voice dictation (macOS)

Open `/voice` to enable hold-Space dictation. Use `OPENAI_API_KEY`, or enter a key
in the masked settings input to store it in **macOS Keychain**. Difu does not save
keys in its configuration or write recordings to disk. Transcription uses OpenAI's
`gpt-live-transcribe` with automatic language detection and separate API billing.
Microphone permission is required. Voice capture is not available on Linux yet.

Tap Space to type a space; hold it to dictate. The composer shows connection,
listening, audio levels, and transcription progress. Releasing inserts the final
transcript at the saved cursor without sending. Esc cancels; failures preserve
the draft. Terminals without key-release reporting use repeat cessation to detect
release. Press Enter explicitly to send the resulting message.

A private local service owns sessions and review jobs. Closing difu or its terminal
leaves them running, including pending approvals. Reopen difu to reconnect. There
is no difu concurrency limit. Only sessions launched through difu are listed;
existing external Codex conversations are not imported. Guide and conflict jobs
appear with distinct labels and keep their existing workflow restrictions.

After a service crash or machine restart, history is restored and interrupted work
requires explicit continuation. Pending queued prompts are retained as unsent text,
not replayed. Completed publication actions are never automatically retried.
Interrupting preserves edits; archiving hides the session and keeps its workspace.
Cleanup is separate and refuses active, modified, untracked, ignored, or Git-locked
worktrees. It never deletes an existing user directory. Removing a clean worktree
retains its named branch and commits.

Every isolated coding session receives a mandatory instruction at start and resume:
**do not run local tests, linting, typechecks, builds, CI scripts, or validation
suites; rely on PR CI.** Git inspection and diff checks remain allowed. This is
agent guidance, not an operating-system command block. Coding agents commit, push,
or open a PR only when explicitly requested by the task. Review conflict jobs retain
their separately authorized automatic, validated push behavior.

Session history and managed coding worktrees live in an `agents` directory beside
`config.json`, with owner-only directory/socket access. The service uses Codex's
app-server interface and existing login. No additional daemon package is required.

### Reviews

The Reviews home screen keeps its two nested tabs:

1. **My PRs** — PRs you authored or were directly requested to review, deduplicated
   and sorted by latest update.
2. **Repositories** — repositories available through your GitHub login, grouped
   into **Pinned** and **Rest**, alphabetically within each group.

Each PR row shows its opened date, changed-file count, additions in green, and
removals in red. Counts load in background batches; the list remains usable.
PR lists are cached separately for each scope and state. Cached lists appear
immediately and refresh in the background when opened and every 30 seconds while
visible. Repository names are also cached, refreshing when you enter Repositories
or press **r**. Failed refreshes keep cached data available.

Select a PR to preview its description, chronological activity, inline review
comments, and checks. **Enter** drills into that PR and selects **Guide**, starting
or retrieving its guide. Inside the PR, the tabs are **Overview / Guide / Diff**;
**Esc** returns home. Guide starts with code focused; Diff starts with files focused. The
border highlights the focused pane, the footer names it, and **Tab** switches focus. You can also launch directly:

```sh
difu https://github.com/owner/repo/pull/123
difu 123 # uses the GitHub repository in your current directory
```

The first time you open a repository, supply its existing local clone path.
Difu remembers it. It uses local commits first and automatically fetches missing
PR revisions using your `gh` login. Fetched revisions are recorded under
`refs/difu/` so later fetches can reuse their history. Your checked-out branch and
uncommitted files are preserved. Diffs are computed locally from the merge base
to the PR head (a three-dot diff). Difu does not clone repositories automatically.
GitHub.com is supported in v1.
Lists show the most recently updated PRs first. GitHub search returns up to
1,000 results for each authored/review-requested query, and up to 1,000 for
the selected repository.

### PR images

Descriptions and activity comments show inline PNG, JPEG, WebP, and GIF previews
(the first GIF frame). Click an image to enlarge it; **Esc** returns to the same
review position. The image modal also offers **o** to open it in your browser.

Images load in the background and are cached locally. Terminals without a
supported graphics protocol, inaccessible attachments, and unsupported formats
show an explanation and a browser link. Previews are limited to 10 MiB and
8192 pixels per dimension; the disk cache retains up to 256 MiB. GitHub image
requests can reuse your `gh` authentication; credentials are never forwarded
to other image hosts.

### States, pinned repositories, and filters

My PRs and repository PR lists default to **Open**. Click **Open / Merged / Closed /
All**, use **[** / **]** to cycle backward/forward, or **s** to cycle forward.
The selector wraps at either end. Closed means closed without merging.

In Repositories, press **\*** or click the pin beside a repository to pin/unpin it
locally. Pins survive restarts and do not change GitHub stars. Existing whitelist
entries migrate into Pinned, including previously disabled entries.
**Enter** opens the selected repository's PR list with a preview on the right;
**Esc** returns to Pinned / Rest. Opening a PR and returning with **Esc** keeps you
in that repository's list.

Each repository, PR, and Files sidebar has a filter input at the top. Press **f**
to focus it, then type to filter immediately by repository name, PR title or author,
or file/directory path. Matching ignores case. **Enter** or **Esc** returns focus
to the list and retains the query; **Ctrl+U** clears it. Queries last for the
session. File matches retain their parent directories; matching a directory shows
all its children. Guide chapter links are unchanged.

## Reading a PR

- **Overview:** a centered column (up to 110 terminal columns) with bordered cards
  for the description, chronological comments/reviews/commits, and checks.
  Check states and durations refresh every ten seconds for the selected PR.
  Click a check to open its GitHub logs.
- **Guide:** chapters connect explanations to changes across files. While
  generation runs, the file navigator and diff stay usable. Completed guides
  scroll continuously, with the current explanation alongside its code.
- **Diff:** browse changed files without the guide. Side-by-side is the default;
  unified is selectable. Narrow windows temporarily use unified diffs and put
  chapter explanations above code, restoring your preference when widened.

Each text hunk has **10 lines above** and **10 lines below** controls. Expansion
applies to that hunk, including its appearances in other chapters, without
expanding other hunks. Context comes from the pinned PR revisions in your local
clone, never from uncommitted edits. If expansion reaches a neighboring hunk,
a divider links to every guide chapter that explains it. File boundaries stop
expansion; read failures remain visible with a retry option.

File headers stay visible while scrolling their diffs. Long paths wrap in file
headers, chapter links, and the file tree. **Alt+Up / Alt+Down** jumps to the
previous or next guide chapter. With the chapter pane focused, plain **Up / Down**
moves between its file links and scrolls the corresponding diff into view.

### Function definitions

Hovering a symbol gives it a dotted underline; holding **Command** or **Ctrl**
makes the underline solid. Command requires the terminal to forward enhanced
keyboard events and the click; use Ctrl if it intercepts Command. The footer
shows the gesture. Ordinary clicks focus the code line.

Hold **Command** or **Ctrl** and click a function identifier in a JavaScript or
TypeScript diff (including JSX and TSX) to open its definition in a scrollable modal. Definitions can live in
unchanged tracked files. **Esc** closes the modal and preserves your chapter,
focus, selection, and scroll position. Inside the modal, use the mouse wheel or
arrow keys to scroll, **Cmd+Up/Down** to move ten lines, **PageUp/PageDown** to
page, and **Left/Right** to pan long lines. **Cmd/Ctrl+click** a symbol inside the
modal to replace its contents with the next definition at the same pinned
revision. Hover underlines work there too; **Esc** returns directly to the review.

Navigation reads local Git objects from the clicked revision: the merge base for
old lines, and the PR head for new lines. It does not use uncommitted files, fetch
objects, create a worktree, install dependencies, or run project code. The parser
is bundled with difu.

Resolution follows lexical bindings, ES module imports, namespace members,
re-exports, and `tsconfig.json`/`jsconfig.json` path aliases, including relative
configuration inheritance. It explains unsupported or ambiguous cases instead
of guessing by name. Examples include type-dependent object methods, reassigned
functions, CommonJS/dynamic imports, external configuration packages, workspace
package entry points, and custom `rootDirs`/`moduleSuffixes` resolution. Cycles and
source-size limits also produce an explanation in the modal.

Guide generation has no automatic timeout. Its elapsed time and activity remain
visible; cancel or retry explicitly. Every changed hunk must appear at least once
in the guide. Binary files, renames, and permission changes have metadata review
units. A hunk may support multiple chapters; repeated references within one
chapter are collapsed in their original order. Unknown or missing references
reject the guide.

Difu checks the open PR's head and base commit IDs through GitHub every 30 seconds.
This metadata check does not fetch Git objects. Remote updates are announced
without replacing your current diff or guide. **r** syncs missing revisions and
refreshes the review; a failed sync keeps your current review usable. If guide
generation is still running, cancel it with **x** before refreshing.

Snapshot preparation shows a five-step progress bar: checking the local clone,
checking/syncing revisions, finding the merge base, reading changed files, and
building/validating the diff. The footer includes elapsed time and Git's live
transfer percentage, size, and speed when available. **x** cancels preparation;
**g** retries a failed preparation.

### Comments, reviews, and PR actions

Press **/** inside a PR for its action wizard. At home, **/** offers **PR controls**
for the selected PR and **Memory management** for temporary worktrees.
Type in PR controls to filter commands. Use arrows and Enter to choose a match,
Backspace to edit, or **Ctrl+U** to clear the filter.

PR controls support comment/approve/request-changes reviews, merge, squash merge,
either merge method with the admin flag, and closing with an optional comment.
A final confirmation shows the PR, action, and pinned revision. Difu checks the
remote head before writing; if it changed, refresh the PR and confirm again.
Merge requests also pass GitHub's atomic expected-head guard. GitHub permissions,
branch protection, and merge-queue rules still apply; admin is used only when
explicitly selected. Failed or uncertain writes are never automatically retried.
Closing with a comment performs two operations; a comment failure after closing
is reported explicitly.

With the diff focused, **>** marks the current line. **Up / Down** moves one row;
**Command+Up / Down** moves ten. In Guide and Diff, arrow keys, line selection,
and mouse-wheel movement keep the highlighted code line near the viewport center.
Scrolling clamps at the beginning and end without adding blank padding.
**Left / Right** scrolls unwrapped code horizontally.
**Alt+Left / Right** chooses old/new code; new is selected by default. **Shift+Up / Down** selects a line range
within one file and side. **Enter** opens the comment editor: publish a standalone
comment or add it to your GitHub pending review. Existing pending reviews are
reused; pending comments stay private until the review is submitted.
GitHub validates comment locations, including expanded context, and reports any
unsupported range without posting a different kind of comment.

Text inputs show a blinking cursor. Comments and review messages support multiline
editing and paste. **Tab** moves between the message, review type, and Next button;
**Ctrl+Enter** goes to confirmation. **Esc** goes back. Unsubmitted text is retained
in the running session when closing a dialog or refreshing the PR; comments already
added to a pending review are stored by GitHub.

Type **@** for mention suggestions from visible organization members and PR
participants. **Up / Down** chooses a suggestion and **Tab** inserts it. Suggestions
are cached locally for 24 hours; expired data remains available during background
refresh. **Ctrl+R** in the editor explicitly refreshes suggestions.

### Conflicts and failed checks

Overview shows GitHub's live mergeability separately from the pinned review
snapshot, including conflicts and required checks that have not reported yet.
The preview's Open / Merged / Closed badge updates with live status even while the
diff and guide stay pinned. An empty check rollup is a valid state. If GitHub is still calculating mergeability,
difu says so. Check runs and merge status refresh every ten seconds.

A **Failed tests** section appears beneath Checks when checks fail. Difu reads
GitHub Actions job logs in the background and displays identifiable test names
with short diagnostic excerpts. Recognized formats include Jest/Vitest, pytest,
and Rust tests. Logs and parsed failures stay in memory for the session. When
logs are unavailable, too large, use an unrecognized format, or belong to another
provider, the section explains the limitation and links to the failed check.
Refresh retries unavailable details; full logs remain available on GitHub.

### Resolve conflicts

Open **/ → PR controls → Resolve conflicts**. The initial confirmation explains
that this action automatically commits and pushes after validation.

Difu uses the existing local clone, syncing missing revisions if necessary, and
creates a disposable detached worktree at the PR's head. It merges the latest
base revision there and starts Codex using **Astra High** (`gpt-6-astra`, `high`)
by default. Codex edits only conflicted text files and may read the repository
for context. Its workspace-write sandbox has network access disabled. Hooks,
connectors, project instructions and project configuration are disabled for the
model invocation.

Difu rejects edits outside the original conflict paths, Git index/HEAD changes,
unresolved entries or markers, and changed file permissions. Binary, symlink,
submodule and non-UTF-8 conflicts require manual resolution. Git's automatic
changes in non-conflicted files are preserved. Both branch revisions are checked
again before the merge commit and before a normal push to the PR's source branch,
including a fork when permissions permit. Difu never force-pushes or retries a
failed attempt automatically.

The resolver is instructed **never to run project tests, typechecks, builds,
linters or dependency installation locally**. Difu performs Git validation;
project checks are delegated to CI. A successful push does not mean CI passed.

Success, failure and cancellation discard the isolated attempt. Failures show the
error; if a network interruption makes a push result uncertain, inspect GitHub
before trying again. Cleanup failures report the remaining worktree path. The
ordinary worktree manager still protects active or modified guide worktrees.

At home, **/** offers **Default guide model** and **Default conflict resolve model**.
Each setting has its own model and reasoning level; existing guide settings are
preserved. Unavailable models report an error without substitution.

### Chapter completion and GitHub Viewed

These are separate:

- **Guide:** Enter on a diff's file heading toggles completion and collapse for
  that file **in that chapter only**. Other chapters and GitHub are unchanged.
  Chapter counters and the overall counter count chapter-file sections. Progress
  is restored for the exact same guide and diff; changed content starts fresh.
- **Diff:** Enter on a file heading toggles GitHub's **Viewed** state and collapses
  or expands the file. This does not complete any guide chapter. A separate total
  counts files Viewed on GitHub.

### Controls

Buttons, tabs, files, PRs, and links are clickable; the mouse wheel scrolls the
pane under the pointer. Keyboard controls use arrows and shortcuts, with no Vim
bindings.

| Key | Action |
| --- | --- |
| Up / Down | Select a PR, directory, file, or chapter link; move the focused code line |
| Command+Up / Command+Down | Focus content and move ten lines |
| Tab / Shift+Tab | Switch navigation/content focus |
| Enter | Open PR; comment on a code line; toggle a file heading |
| / | PR actions; home also offers worktree management |
| Shift+Up / Shift+Down | Select code lines within one file and side |
| Page Up / Page Down / Space | Scroll a page |
| Home / End | Start / end |
| Left / Right | Scroll the focused Files tree or unwrapped code horizontally |
| Alt+Left / Alt+Right | Select old/new diff side; new is selected by default |
| Alt+Up / Alt+Down | Previous / next guide chapter |
| 1 / 2 / 3 | Home: 1 My PRs / 2 Repositories; inside a PR: Overview / Guide / Diff |
| ? | Search shortcut help; type to filter, Up/Down to scroll, Esc to close |
| m | Model and reasoning picker |
| [ / ] / s | Previous / next / next PR state |
| f | Focus the repository, PR, or Files filter |
| Ctrl+U | Clear the focused filter |
| * | Pin/unpin the selected repository locally |
| r | Refresh |
| g | Regenerate / retry |
| l | Choose a local clone path |
| x | Cancel generation, snapshot preparation, or active conflict resolution |
| c / Command+C | Copy focused source line, selected range, or file heading path |
| Ctrl+R | Refresh mention suggestions in the comment/review editor |
| w | Toggle diff wrapping (saved between launches) |
| Ctrl+B | Side-by-side / unified preference |
| Ctrl+O | Open the PR on GitHub |
| Esc | Close dialog / return home / quit |
| Ctrl+C | Quit; coding agents and review jobs keep running |

The Files tree stays expanded. Select a directory with arrows or a click to see
all changed files beneath it. Tree names stay on one line and scroll horizontally;
chapter links and diff headers still wrap. Guide view opens with the code pane
focused; use Tab to focus chapter navigation. Press `w` to toggle code wrapping
in either split or unified diffs. This preference is saved between launches.

In the clone dialog, paste a path, use Ctrl+U to clear it, then Enter. Action
shortcuts use plain characters outside text inputs. Plain letters in an input
enter text; the footer also offers clickable actions.

Command shortcuts require a terminal that forwards the Command (Super) modifier.
Difu enables the enhanced keyboard protocol while running and restores it on exit.
Ghostty's default Command+arrow bindings jump between prompts and consume these
keys. To forward them to terminal applications, use these Ghostty bindings:

```ini
keybind = super+arrow_up=csi:1;9A
keybind = super+arrow_down=csi:1;9B
```

See [Ghostty keybindings](https://ghostty.org/docs/config/keybind) for configuration.
On Linux, the equivalent modifier is Super; desktop shortcuts may also intercept it.

## Codex and caching

The default is **Luna High** (`gpt-5.6-luna`, `high`). m discovers the models and
reasoning levels available through your installed Codex. Luna High is pinned as
a recommendation when available. Choosing a model saves it for the next opening
or explicit regeneration; it does not silently restart a running generation.
An unavailable model produces an error, with no automatic model substitution.

Guide generation invokes `codex exec` non-interactively, using the existing login, an ephemeral
session, a read-only sandbox, and a strict JSON output schema. User config and
execution-policy rules are ignored for generation. Project instruction loading is
disabled, and the clone/worktree are marked untrusted for this invocation so their
Codex configuration is skipped. Configured MCP server names are read and each
server is explicitly disabled; the read-only guide instructions are supplied as
developer instructions. See the
[Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).
Apps, plugins, hooks, multi-agent execution, memories, browser/computer use, web
search, and MCP servers are disabled for that invocation. No Codex settings or
authentication files are rewritten.
The PR description, diff, and repository context Codex reads are sent through
your Codex account to generate the guide.

Guides persist locally. Cache identity includes repository contents at the head
and merge base, the complete parsed diff, PR identity/title/description, model,
reasoning level, prompt, and schema. Commit-message-only rewrites can reuse a
guide; changed code or guide inputs require a matching cache entry or generation.
Existing guides from older releases are reused when their original inputs match.
Cached guides are validated again before use. g bypasses the guide cache.
Damaged cache entries produce an error and can be replaced with g.

Guide generation shows brief Codex progress preambles alongside elapsed time
when the model emits them. When commentary is absent, short headings from Codex's
reasoning summaries provide progress updates. Difu requests automatic summaries
for guide runs; the full reasoning text is never
displayed. Tool activity remains visible until a preamble or heading arrives.
These updates do not change the saved guide or invalidate existing guides.

Settings, cached PR lists, and guides use the OS's standard per-user directories:

| OS | Settings | Cache |
| --- | --- | --- |
| macOS | `~/Library/Application Support/difu/config.json` | `~/Library/Caches/difu/` |
| Linux | `$XDG_CONFIG_HOME/difu/config.json` (default `~/.config`) | `$XDG_CACHE_HOME/difu/` (default `~/.cache`) |

Settings remember clone paths, guide and conflict model/effort, diff preference, the repository
local pins. Writes are atomic
and files are created with owner-only permissions. Guide cache files contain PR
explanations and hunk references. PR-list cache files contain PR titles, authors,
opened dates, and counts. The repository directory cache contains repository names. Mention caches contain GitHub logins; progress caches
contain completed chapter-file sections. Remove the cache directory to clear
cached data. GitHub pending reviews and Viewed state are unaffected.

Expansion controls appear only when more source lines exist. File boundaries load
in the background from pinned local Git objects and are cached per revision;
reading boundaries requires no fetch or worktree. **r** retries a failed boundary
read. Full context remains loaded on demand.

### Copying code

Focus the code pane in Guide or Diff and press **c** or **Command+C** to copy the
current source line or the range selected with **Shift+Up/Down**. Copying uses the
active old/new side, preserves indentation, and excludes line numbers and diff
markers. Wrapped or horizontally clipped lines copy in full. A file heading
copies its path. Copying keeps your focus, selection, and scroll position.

Selections spanning hidden context read the pinned local Git revision and cache
that context; they do not copy uncommitted working-tree changes or fetch anything.
Clipboard writes use OSC 52, with no extra executable dependency. The terminal
(and any multiplexer) must permit clipboard writes. Ghostty permits them by
default; see its [clipboard configuration](https://ghostty.org/docs/config/reference#clipboard-write).
Difu reports “Sent to terminal clipboard” because the protocol cannot confirm
that the terminal accepted the write. Command+C must be forwarded by your terminal;
**c** works when the terminal intercepts the Command shortcut. Ctrl+C still quits.

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

Owned subprocess groups are terminated on explicit cancellation. Guide worktrees
are removed on job completion or failure. Closing the TUI leaves the background
service and its review jobs running. Cleanup does not use
`--force`: if a worktree unexpectedly contains changes, difu reports and preserves
its path. An uncatchable kill or power loss can leave a temporary worktree; inspect
the home **/ → Memory management** dialog. It lists remaining worktrees and offers
individual deletion or **Delete all stale**. Only difu-owned, inactive, clean,
unlocked worktrees can be deleted. Process leases protect active trees across
difu instances. Modified, ignored/untracked, Git-locked, and unverified legacy
trees remain protected; the dialog explains why. Ownership and eligibility are
checked again immediately before deletion; removal never uses `--force`.

The first-party Rust code forbids `unsafe`. Clippy denies `.unwrap()`, `.expect()`,
explicit panics, unchecked indexing/slicing, `todo!`, and `unimplemented!`, including
in tests. CI treats all warnings as errors. These rules do not establish that
third-party libraries are free of unsafe code or that an AI explanation is correct.
Coverage validation checks references and omissions; reviewers still assess the
explanation against the code.

Shallow history with no merge base and non-UTF-8 diffs produce explicit errors;
difu does not silently substitute an incomplete or lossy snapshot. GitHub write
actions occur only through the explicit controls described above.

## Guide writing research

The writing instructions in [prompts/guide.md](prompts/guide.md) draw on three
visible Linear Guide chapters from a reference recording and the corresponding
PR description. The Linear API exposed PR metadata and descriptions but no
generated Guide chapters, so the research is limited to that sample. The guide
style connects logical changes in dependency order, using short causal
explanations and links across files. Test changes are grouped into dedicated
chapters by concern. The guide groups chapters in this order:

1. **Manual schemas / DTOs**
2. **Database migrations**
3. **Implementation**
4. **Generated code**, including TanStack route trees, generated schemas, clients,
   and types
5. **Tests**

Each group can contain multiple chapters with a divider between groups. Empty
groups are omitted. Difu enforces the group order while preserving chapter order
within each group.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

The workflow test uses Python 3 to script GitHub/Codex responses, alongside real
local Git repositories. The default test suite requires no GitHub credentials or
AI calls.
CI runs the checks on macOS and Linux.

An additional live smoke test is ignored by default. It uses your Codex login and
one Luna High generation against a tiny synthetic repository:

```sh
cargo test --locked --test codex_smoke -- --ignored --nocapture
```

## License

[MIT](LICENSE).

### Review presentation

PR previews render Markdown tables, headings, lists, emphasis, links, and code
blocks. Inline images use a larger preview area and open in a full modal when
clicked. App and panel backgrounds inherit the terminal background, including
Ghostty transparency and blur. Selected controls retain their accent highlight.

Guide category headings remain visible while scrolling through their chapters.
In Guide and Diff, **Shift+] (`}`)** adds one context line on each side of the
focused hunk; **Shift+[ (`{`)** removes one, stopping at the original diff context.
Context comes from pinned local Git revisions, without fetching or reading dirty
working-copy contents. The selected source line stays anchored.

Guide generation supplies the complete diff directly in a compact initial Codex
prompt. Additional repository reads are reserved for specific uncertainties.
Progress records worktree preparation, setup, model time, tool calls, validation,
and cleanup. The response schema requires a nonempty chapter assignment for every
hunk, including metadata-only changes. Difu reconstructs the ordered chapters and
validates complete coverage before accepting or caching the guide.

Agent activity stays above the composer with a shimmering status, the current tool
or approval review, and pending steering messages underneath. Explicit next-turn
messages remain a separate editable queue. During a worktree guidance prompt,
messages and answers are saved until you choose whether to copy the guidance;
the existing conversation is retained. Ghostty's Option+Left/Right word shortcuts
work in text inputs, including its translated Alt+b/f key events.
