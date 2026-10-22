# Settings

Open `/settings`, press **Ctrl+,**, or choose Settings in **Cmd+K** from Agents or
Reviews. `/theme`, `/tweaks` and `/voice` open the corresponding section. Your
active session, composer draft, selection and reading position stay intact.

## Browse, preview and save

**Tab** cycles through Search → Sections → Settings; **Shift+Tab** goes back.
**Up/Down** changes the section when Sections is focused, or selects a setting
when Settings is focused. **Ctrl+F** jumps to Search. Search text only changes
while that field is focused. **Enter** or **Down** from Search focuses the
results without changing a value. **Enter/Right** from Sections focuses its
settings; press again to edit or advance a value. **Left** selects the previous
value while Settings is focused. **PageUp/PageDown** remain section shortcuts.
Details expand with **Ctrl+I** and scroll with **Ctrl+Up/Down**, including in a
narrow terminal. The displayed keys
follow your saved custom bindings.

Search understands intent: **stop flashing** finds Animations; **change question
key** finds Open questions. Details show the saved/default values, validation,
a sample when available, and one of these effect labels:

- **Immediate on save:** applies to open views when saved.
- **New sessions:** affects sessions created afterward.
- **Next launch:** used when opening the next terminal window.
- **Applies separately:** an explicit connection or manager action with its own flow.

**Ctrl+S** saves; **Ctrl+D** discards and reloads the latest saved preferences.
**Esc** closes a clean draft, or offers to discard an edited draft. **Ctrl+M**
shows modified settings. **Ctrl+R** resets the current section in the draft;
Save is still required. Settings navigation uses committed shortcuts until Save.
Appearance and motion preview locally while editing; Discard restores saved
values. Motion samples restart when you select a control or change its value.
**Ctrl+P** replays the selected effect and brings its sample back into view.
Disabled effects explain which toggle or zero duration is preventing playback;
nonzero durations also show the effective timing after the animation cap.
**Ctrl+T** tests the selected shortcut or
notification. Shortcut tests never send messages, approve tools, interrupt work,
or start the microphone. Notification tests explicitly use the current draft.

Independent changes from another Difu window merge on Save. Conflicting edits
remain in the draft with an error; Discard loads the other window's saved state.
Invalid values and failed writes do not erase your draft. **Ctrl+E** exports a
private JSON file containing draft preferences and shortcut overrides, excluding
credentials, account details, session data and repository rules. Its path appears
in the panel; there is no preference-import flow.

## Sections

| Section | Controls |
|---|---|
| Appearance | Theme, Comfortable/Compact density, contrast, message separators |
| Motion | Full/Reduced/Off, reply/tool effects, sent-message effects, sidebar effects, screen transitions, reply speed, sent duration, input clearing duration, maximum animation delay |
| Conversation | Tool display, tool preview lines, expanded reasoning, timestamps, auto-scroll |
| Layout & sessions | Sidebar and Changes visibility, sidebar width, helper placement (Right/Bottom/Main), sorting within pinned groups, startup tab |
| Code & diffs | Unified/split layout, wrapping, line numbers, context lines, whitespace markers |
| Input & voice | Voice enablement, send-key preset, input while running, composer height, pasted-text collapse threshold |
| Keyboard shortcuts | Searchable action bindings, context conflicts, capture, reset and safe testing; question-key selection override |
| Notifications | Completion/question/approval/error events, scope, In-app/Desktop/Both delivery, sound, Quiet, presets and Test |
| Agent defaults | Provider, model, effort, repository, native starting mode, separate repository-rules manager |
| Connections | Provider status and native sign-in, MCP, Slack and remote-control managers |

Motion Off stops animation wakeups while timers and notifications continue to
update. Reduced motion keeps content readable without the full effects. Zero
reply speed or effect duration disables that effect; zero maximum delay means
uncapped reveal delay. Preview scenes use the real conversation/diff renderers.

Conversation preferences retain original copied text. Timestamps use recorded
message receipt times, shown in UTC; older entries without a recorded time do
not receive invented timestamps. Reading-position mode preserves your place when
new content arrives. Sorting keeps pinned groups intact and does not rewrite
manual order. Startup tab takes effect next launch; an explicit PR argument
still opens Reviews. Changing diff context refreshes the relevant diff source
without changing Git's index or working files.

## Open questions with Shift+Left

1. Enable **Question key overrides text selection** to explicitly allow the
   question shortcut to replace the composer's normal Shift+Left selection.
2. Search **change question key**, select **Open questions**, and capture **Shift+Left**.
3. Use **Ctrl+T** to try it against a synthetic question, then **Ctrl+S** to save.

The default is **Alt+Up**. Saving a replacement removes that default for this
action. The override applies in supported question/composer contexts and does
not globally replace text selection in every editor. Conflicts are checked in
contexts where actions can actually be used together, including embedded PRs.

The Enter send preset assigns Enter to send, Shift+Enter to newline, Ctrl+Enter
to queue. The Ctrl+Enter preset assigns Ctrl+Enter to send, Enter to newline,
Alt+Enter to queue. Choosing a preset replaces all three companion bindings
atomically. Individual edits show Custom. Reset Keyboard restores known bindings,
the Enter preset and the selection override; unknown future bindings survive.

Bindings use explicit modifiers and terminal-equivalent normalization. Rare
undocumented modified variants of old list keys may need an explicit assignment.
Ctrl+C still copies an active selection before it can quit. Terminal support
matters: Shift+Left CSI and Ctrl+Enter CSI-u are supported; the existing
modifyOtherKeys encoding `ESC[27;5;13~` is not. A terminal must report Ctrl+Enter
distinctly for that preset. The embedded browser keeps raw input ownership;
its existing codec does not serialize physical BackTab, though Tab+Shift works.

## Notifications

Defaults keep delivery in-app. Native delivery requires an explicit saved switch
to Desktop/Both or an unsuppressed native Test; browsing, refreshing, Discard and
startup do not request notification permission. macOS packages contain the native
helper. Linux uses the desktop D-Bus notification service, with optional `paplay`
for independent sound. Capabilities and delivery errors appear in Settings.
Quiet keeps in-app activity and suppresses desktop alerts and sound, including
Test. Disabled events and sound Off are respected by Test too.

Scope can include all sessions, only background sessions, or only when Difu is
unfocused. A conversation visible in any focused Difu client counts as viewed;
a helper replacing that conversation does not. Completion means a successful
turn/job; cancellation is silent, and failure produces only the error event.
Native alerts contain the session title and event type, not message/tool content.

The background agent service owns notification delivery, so it can notify while
no terminal is open as long as that service is running. It does not replay old
events when settings change or the service restarts. Event markers are persisted
before delivery to prevent duplicate alerts; a crash in that short interval can
lose an alert. The in-app feed is bounded. Client focus leases expire after six
seconds, so abruptly disconnected clients stop suppressing notifications.

## Defaults and connections

Default provider/model/effort/native mode are discovered from the installed
provider CLI without running a model turn. Automatic delegates to the provider.
Known-invalid combinations are rejected; discovery failure preserves draft values
and permits structurally valid offline choices. A selected native starting mode
must be acknowledged before the first user turn; rejection is visible and sends
no turn under a different mode. Empty sessions retain lazy provider startup.
Changing defaults never changes an existing running session.

Sign in to Codex or Claude explicitly to hand the foreground terminal to that
provider's native login. **Ctrl+C** cancels and returns to the same Settings draft;
Difu restores terminal modes and refreshes sanitized connection status. Rules,
MCP, Slack and remote controls use their existing managers and apply separately
from preference Save/Discard. Rules target the draft repository. Remote browsing
and refresh only query status; **Enable/Pair** explicitly starts pairing. Slack
management retains its existing macOS-only availability.
