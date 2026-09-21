# Agent interaction research

Research date: 2026-09-17. Local versions: Codex 0.154.0, Claude Code 2.1.270,
OpenCode 1.18.1. This is an implementation inventory, not a claim of full parity.
Live documentation can describe newer behavior than an installed binary.

## Design direction

Keep difu's Agents/Reviews structure, Matrix-green accent, independent list and
Changes panes, and native Codex service. Improve the conversation itself rather
than nesting another CLI terminal inside it. Existing sandbox, publication,
worktree, and no-local-validation agreements remain in force.

### Codex

The installed app-server has separate thread, turn, item, approval, and user-input
lifecycles. Treat those as structured state rather than printing event names.
Compaction is asynchronous and reports normal turn/item events. Skills are scoped
to working directories and can be attached explicitly by name and absolute path.
The native prompt should keep the same Codex home, configuration, project guidance,
and enabled local-memory behavior. Local memories and ChatGPT web memory are
separate systems.

Sources: [app-server](https://learn.chatgpt.com/docs/app-server),
[local memories](https://learn.chatgpt.com/docs/customization/memories),
[project instructions](https://learn.chatgpt.com/docs/agent-configuration/agents-md),
[skills](https://learn.chatgpt.com/docs/build-skills).

The reference CLI separates the text composer from its pending-input preview and
question overlay. A question request can contain several questions and preserve
per-question selection and notes. Incoming questions and outgoing queued prompts
must not be represented by a single undifferentiated list. The matching source's
pending-input preview binds Alt+Up to editing queued input; difu's requested
Alt+Up behavior is specifically to inspect pending questions.

Sources: [question overlay](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/tui/src/bottom_pane/request_user_input/mod.rs),
[pending-input preview](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/tui/src/bottom_pane/pending_input_preview.rs),
[composer](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/tui/src/bottom_pane/chat_composer.rs).

### Claude Code

Useful patterns include a quiet default transcript, expandable tool detail,
visible activity while work runs, editable queued input, and clear distinctions
between interrupting a turn and exiting the application. Its agent interface
provides compact session navigation and foreground/background interaction.
These are references; difu keeps its approved send/steer/queue and exit semantics.
Rewind is a separate feature with repository effects, not just a visual change.

Sources: [interactive mode](https://code.claude.com/docs/en/interactive-mode),
[agent view](https://code.claude.com/docs/en/agent-view),
[checkpointing](https://code.claude.com/docs/en/checkpointing),
[keybindings](https://code.claude.com/docs/en/keybindings).

### OpenCode

Useful patterns include searchable commands, in-context file completion,
clear steering versus queueing, session navigation, and configurable bindings.
Its undo/redo can revert work as well as messages, so importing that behavior
requires a separate repository-history decision. Its keymap contains both
application actions and input-editor actions; difu must avoid collisions with
its existing Ctrl+B/Ctrl+D pane controls.

Sources: [TUI](https://opencode.ai/v2/docs/cli/tui/),
[keybindings](https://opencode.ai/v2/docs/cli/keybinds/).

## Interaction inventory and acceptance cases

| Area | Difu behavior / verification target | State |
| --- | --- | --- |
| Message hierarchy | Distinct user prompt; Markdown agent response; no protocol labels | Agreed |
| Tool activity | Compact action/target/status/duration; expandable arguments and output | Agreed |
| Tool errors | Failed/declined/interrupted distinguishable from success; error shown once | Agreed direction |
| Markdown | Headings, lists, emphasis, links, fenced code and diff content | Agreed |
| Live progress | Preparing, working, responding, waiting, compacting, failed; elapsed time | Agreed |
| Plans | Update step states in place instead of adding JSON snapshots | Agreed |
| Reading position | New output must not pull a reader away from earlier content | Agreed |
| Follow latest | Explicit return to latest output; no fabricated completion percentage | Implementation target |
| Tool keyboard navigation | Focus visible transcript items and expand with Enter | Implemented; approved |
| Slash completion | Empty composer opens searchable native commands; literal slash inside existing text | Agreed |
| Initial slash set | compact, model, skills, status, diff, new, rename, help, actions | Agreed |
| Resume command | Omit /resume; preserve existing session navigation/Continue | Agreed |
| Skills | Native discovery for actual cwd; explicit selection inserts into draft without sending | Agreed |
| Skill identity | Preserve exact path, including when names repeat; respect enabled state | Implementation target |
| Editor selection | Shift+arrows in text inputs; visible range, replace/delete, Cmd+C | Agreed |
| Editor wrapping | Vertical movement follows visual lines and retains intended column | Implementation target |
| Questions | Alt+Up access, unanswered count, options, free-text answers | Implemented; approved |
| Question dismissal | Preserve unanswered requests and answer drafts without inventing an answer | Implemented; approved |
| Outgoing queue | Separate from incoming questions; edit/delete before dispatch | Implemented; approved |
| Approvals | Native request identity and scope; no acceptance from defaults or dismissal | Existing agreement |
| Session actions | Interrupt, Continue, rename, archive; cleanup remains separate | Existing agreement |
| Model settings | Native defaults with per-session overrides | Existing agreement |
| Native context | Preserve Codex home/settings, project AGENTS.md, enabled memories and skills | Implemented; approved |
| Missing local guidance | Worktree may lack ignored/untracked files from original clone | Implemented; approved |
| Voice gesture | Hold Space, tap remains space, transcription at cursor, explicit send | Implemented; approved |
| Voice backend | Account-backed, API-backed, or local transcription | Implemented; approved |
| File mentions | @ completion for session workspace files and folders | Implemented; approved |
| Paste attachments | Private image/video copies; native image inputs, video paths; 500-character paste tokens | Implemented; approved |
| External editor | Temporarily edit draft through VISUAL/EDITOR, restore terminal | Research inventory; scope not yet agreed |
| Prompt history | Up/Down recalls this session’s prompts and restores the unsent draft | Implemented; approved |
| Notifications | Waiting/completed counts visible when session is not selected | Research inventory |
| Session fork/rewind | Explicit history/workspace semantics before implementation | Separate decision |
| Permissions/plugins | Mutating Codex settings, installing plugins, account changes | Separate decision |
| Sharing/publication | No automatic publication or transcript sharing | Existing agreement |

## Voice: gesture versus service

Claude's documented hold mode distinguishes a tap from repeated held-key input,
shows recording preparation and microphone level, and inserts interim/final text
at the cursor. Release normally finishes dictation without submitting the prompt.
It requires a local microphone and sends audio to Anthropic using a Claude account;
it is not a generic endpoint provided by the Anthropic API key. Its settings also
offer a separate tap mode with different submission behavior.

Difu can match the requested gesture without adopting tap mode or automatic send.
It must preserve the existing draft on cancellation, permission denial, missing
microphone, capture failure, network failure, and focus changes. The recording
owner should be the foreground UI process, not a detached coding worker.

Source: [Claude voice dictation](https://code.claude.com/docs/en/voice-dictation).

The installed stable Codex app-server schema exposes no standalone dictation
method. Its experimental schema includes realtime start/appendAudio/appendText/
appendSpeech/stop methods. Those represent a live conversation and are not proven
as a draft-only transcription service. Do not silently send audio into a coding
turn or consume account resources to emulate a missing dictation API.

Approved and implemented: macOS native CPAL capture, OpenAI Realtime
`gpt-live-transcribe`, automatic language detection, and an environment API key or
masked input saved to macOS Keychain. Audio remains in memory. Linux voice is
deferred. Synthetic capture/resampling and gesture tests cover cancellation,
selection preservation, and insertion without sending. An actual microphone/API
session still requires live verification with a configured key.

Sources: [OpenAI realtime transcription](https://developers.openai.com/api/docs/guides/realtime-transcription),
[OpenCode credential storage](https://opencode.ai/docs/providers#credentials).
OpenCode's local auth.json is not a macOS Keychain store; difu explicitly uses
Keychain following the user's choice.

## Additional approved interaction details

- Tab toggles session list/input directly; Cmd+Up/Down enters and navigates
  message blocks, with a dim focus background. Typing returns to the input.
- User bubbles use a dark Matrix-green tint and white text. The newest sent prompt
  stays pinned, with expansion for long text. Chat text supports mouse selection
  and terminal clipboard copying.
- Composer height grows from one to ten lines. Long pastes retain their full
  payload; Ctrl+V/Cmd+V at a token expands it. Pasted code is lexically highlighted.
- Luna Medium names a session after its first successful coding turn. Attempt
  state is persisted before execution, and manual renames always take precedence.

## Verification plan

- Unit tests: Unicode selection, wrapped movement, draft replacement, picker
  filtering, duplicate skill names with distinct paths, transcript rendering,
  expansion and scroll anchors, pending-question draft restoration.
- App-server fixture: exact compaction call, asynchronous completion, rejection
  during an active turn, skills scoped to cwd, explicit skill inputs retained in
  queued messages, native instructions preserved on start/resume.
- Live Ghostty: editor selection and clipboard, slash completion, tool expansion,
  concurrent pending questions, pane switching, disconnect/reconnect.
- Voice: synthetic capture tests before an explicit live
  microphone test; cancellation and terminal restoration; macOS capture packaging.
- Existing review and worktree regressions plus strict Rust lints remain required.
