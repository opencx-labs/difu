//! Context selection is shared by Shell globals and the native Agents handlers.
use super::*;
use crate::shortcuts::{ActionId, Bindings, Context, contexts::*};

impl Ui {
    /// Shell globals follow the actual embedded editor, while panel_key retains
    /// its separately validated host helper precedence for nonmodal content.
    pub(crate) fn input_context(&self) -> crate::shortcuts::Context {
        if self.modal.is_none()
            && self.panels.focused
            && self.panels.visible()
            && let Some(super::panels::View::PullRequest { app }) = &self.panels.view
        {
            return app.shortcut_context();
        }
        self.shortcut_context()
    }

    pub(crate) fn shortcut_context(&self) -> Context {
        if self.inbox.visible && matches!(self.modal, None | Some(Modal::Approval { .. })) {
            if self.focus == Focus::List {
                return AGENT_LIST;
            }
            return if self.inline_question() {
                if self.question_note_focused {
                    AGENT_INBOX_NOTE
                } else {
                    AGENT_INBOX_QUESTION
                }
            } else if self.modal.is_some() {
                AGENT_INBOX_APPROVAL
            } else {
                AGENT_INBOX
            };
        }
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
                Modal::Mcp(state) => state.shortcut_context(),
                Modal::Slack(form) => match form.field {
                    0 | 1 => AGENT_SLACK_SECRET,
                    2 | 3 => AGENT_SLACK,
                    _ => AGENT_SLACK_BUTTON,
                },
                Modal::Transcript { .. } => AGENT_TRANSCRIPT,
                Modal::InstallBrowser { .. } => AGENT_RESOURCE_PICKER,
                Modal::Resources { .. } => AGENT_RESOURCE_LIST,
                Modal::AgentDefaults(form) => {
                    if form.field < 3 {
                        AGENT_DEFAULTS
                    } else {
                        AGENT_DEFAULTS_BUTTON
                    }
                }
                Modal::RepositoryRules(form) => match form.field {
                    0 => AGENT_RULES_REPOSITORY,
                    1 => AGENT_RULES,
                    _ => AGENT_RULES_BUTTON,
                },
                Modal::Tweaks { field, .. } => {
                    if *field < 4 {
                        AGENT_TWEAKS
                    } else {
                        AGENT_TWEAKS_BUTTON
                    }
                }
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
                Modal::Rename(_) | Modal::RenameFolder { .. } => AGENT_RENAME,
                Modal::FolderPicker { .. } => AGENT_MENU,
                Modal::DeleteFolder(_) => AGENT_DELETE,
                Modal::Model { .. } => AGENT_MODEL,
                Modal::ModelPicker(_) => AGENT_MODEL_PICKER,
                Modal::Approval { .. } => AGENT_APPROVAL,
                Modal::Delete => AGENT_DELETE,
                Modal::Clear => AGENT_CLEAR,
                Modal::Help(_) => AGENT_HELP,
            };
        }
        if self.browser_input() || self.terminal_input() {
            return if self.panels.right {
                AGENT_BROWSER
            } else {
                AGENT_BROWSER_MAIN
            };
        }
        if self.panels.focused && self.panels.visible() {
            return if self.panels.right {
                AGENT_HELPER
            } else {
                AGENT_HELPER_MAIN
            };
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
