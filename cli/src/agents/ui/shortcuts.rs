//! Context selection is shared by Shell globals and the native Agents handlers.
use super::*;
use crate::shortcuts::{ActionId, Bindings, Context, contexts::*};

impl Ui {
    pub(crate) fn shortcut_context(&self) -> Context {
        if self.inline_question() {
            return if self.question_note_focused {
                AGENT_QUESTION_NOTE
            } else {
                AGENT_QUESTION
            };
        }
        if let Some(modal) = &self.modal {
            return match modal {
                Modal::Permissions(state) => state.shortcut_context(),
                Modal::Plan(state) => {
                    if state.editor().is_some() {
                        AGENT_PLAN_FEEDBACK
                    } else {
                        AGENT_PLAN
                    }
                }
                Modal::Mcp(_) => AGENT_MCP,
                Modal::Slack(_) => AGENT_SLACK,
                Modal::Transcript { .. } => AGENT_TRANSCRIPT,
                Modal::InstallBrowser { .. } => AGENT_RESOURCE_PICKER,
                Modal::Resources { .. } => AGENT_RESOURCE_LIST,
                Modal::AgentDefaults(_) => AGENT_DEFAULTS,
                Modal::RepositoryRules(_) => AGENT_RULES,
                Modal::Tweaks { .. } => AGENT_TWEAKS,
                Modal::Theme { .. } => AGENT_THEMES,
                Modal::Remote => AGENT_REMOTE,
                Modal::Repository(_) => AGENT_REPOSITORY,
                Modal::ChangeRepository(_) => AGENT_CHANGE_REPOSITORY,
                Modal::Commands { .. } => AGENT_COMMANDS,
                Modal::Status { .. } => AGENT_STATUS,
                Modal::Usage { .. } => AGENT_USAGE,
                Modal::Pending { .. } => AGENT_PENDING,
                Modal::Queue { .. } => AGENT_QUEUE,
                Modal::QueuedEdit { focus: 0, .. } => AGENT_QUEUE_EDITOR,
                Modal::QueuedEdit { .. } => AGENT_QUEUE_BUTTON,
                Modal::Menu { .. } => AGENT_MENU,
                Modal::Rename(_) => AGENT_RENAME,
                Modal::Model { .. } => AGENT_MODEL,
                Modal::ModelPicker(_) => AGENT_MODEL_PICKER,
                Modal::Approval { pending, .. } => {
                    if pending.method == "item/tool/requestUserInput" {
                        AGENT_APPROVAL_TEXT
                    } else {
                        AGENT_APPROVAL
                    }
                }
                Modal::Delete => AGENT_DELETE,
                Modal::Clear => AGENT_CLEAR,
                Modal::Help(_) => AGENT_HELP,
            };
        }
        if self.browser_input() {
            return AGENT_BROWSER;
        }
        if self.panels.focused && self.panels.visible() {
            return AGENT_HELPER;
        }
        match self.focus {
            Focus::List => AGENT_LIST,
            Focus::Conversation
                if self.drilled
                    && self
                        .selected
                        .as_ref()
                        .and_then(|id| self.sessions.get(id))
                        .is_some_and(|s| matches!(s.job, Job::Coding(_))) =>
            {
                AGENT_CONVERSATION_INPUT
            }
            Focus::Conversation => AGENT_CONVERSATION,
            Focus::Composer
                if self.changes_visible || self.panels.focused || self.question_panel() =>
            {
                AGENT_COMPOSER_PANES
            }
            Focus::Composer => AGENT_COMPOSER,
            Focus::Changes => AGENT_CHANGES,
            Focus::ChangeTree => AGENT_CHANGE_TREE,
            Focus::Resources(_) | Focus::PullRequest(_) => AGENT_RESOURCES,
        }
    }
    pub(super) fn shortcut(&self, key: KeyEvent) -> Option<ActionId> {
        Bindings::new(&self.shortcut_config).resolve(self.shortcut_context(), key)
    }
}
