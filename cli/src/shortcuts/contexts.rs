use super::Context;
pub const AGENT_LIST: Context = Context("AgentList");
pub const AGENT_COMPOSER: Context = Context("AgentComposer");
pub const AGENT_COMPOSER_PANES: Context = Context("AgentComposerPanes");
pub const AGENT_CONVERSATION_INPUT: Context = Context("AgentConversationInput");
pub const AGENT_CONVERSATION: Context = Context("AgentConversation");
pub const AGENT_CHANGES: Context = Context("AgentChanges");
pub const AGENT_CHANGE_TREE: Context = Context("AgentChangeTree");
pub const AGENT_RESOURCES: Context = Context("AgentResources");
pub const AGENT_HELPER: Context = Context("AgentHelper");
pub const AGENT_TERMINAL: Context = Context("AgentTerminal");
pub const AGENT_BROWSER: Context = Context("AgentBrowser");
pub const AGENT_QUESTION: Context = Context("AgentQuestion");
pub const AGENT_QUESTION_NOTE: Context = Context("AgentQuestionNote");
pub const AGENT_APPROVAL: Context = Context("AgentApproval");
pub const AGENT_APPROVAL_TEXT: Context = Context("AgentApprovalText");
pub const AGENT_PENDING: Context = Context("AgentPending");
pub const AGENT_QUEUE: Context = Context("AgentQueue");
pub const AGENT_QUEUE_BUTTON: Context = Context("AgentQueueButton");
pub const AGENT_QUEUE_EDITOR: Context = Context("AgentQueueEditor");
pub const AGENT_PLAN: Context = Context("AgentPlan");
pub const AGENT_PLAN_FEEDBACK: Context = Context("AgentPlanFeedback");
pub const AGENT_COMMANDS: Context = Context("AgentCommands");
pub const AGENT_MENU: Context = Context("AgentMenu");
pub const AGENT_HELP: Context = Context("AgentHelp");
pub const AGENT_MODEL: Context = Context("AgentModel");
pub const AGENT_MODEL_PICKER: Context = Context("AgentModelPicker");
pub const AGENT_DEFAULTS: Context = Context("AgentDefaults");
pub const AGENT_PERMISSIONS: Context = Context("AgentPermissions");
pub const AGENT_PERMISSION_SEARCH: Context = Context("AgentPermissionSearch");
pub const AGENT_PERMISSION_FORM: Context = Context("AgentPermissionForm");
pub const AGENT_PERMISSION_DELETE: Context = Context("AgentPermissionDelete");
pub const AGENT_MCP: Context = Context("AgentMcp");
pub const AGENT_REMOTE: Context = Context("AgentRemote");
pub const AGENT_RULES: Context = Context("AgentRules");
pub const AGENT_SLACK: Context = Context("AgentSlack");
pub const AGENT_THEMES: Context = Context("AgentThemes");
pub const AGENT_TWEAKS: Context = Context("AgentTweaks");
pub const AGENT_USAGE: Context = Context("AgentUsage");
pub const AGENT_TRANSCRIPT: Context = Context("AgentTranscript");
pub const AGENT_STATUS: Context = Context("AgentStatus");
pub const AGENT_RESOURCE_PICKER: Context = Context("AgentResourcePicker");
pub const AGENT_RESOURCE_LIST: Context = Context("AgentResourceList");
pub const AGENT_REPOSITORY: Context = Context("AgentRepository");
pub const AGENT_CHANGE_REPOSITORY: Context = Context("AgentChangeRepository");
pub const AGENT_RENAME: Context = Context("AgentRename");
pub const AGENT_DELETE: Context = Context("AgentDelete");
pub const AGENT_CLEAR: Context = Context("AgentClear");
pub const REVIEW_HOME: Context = Context("ReviewHome");
pub const REVIEW_REPOSITORY: Context = Context("ReviewRepository");
pub const REVIEW_NAVIGATION: Context = Context("ReviewNavigation");
pub const REVIEW_CONTENT: Context = Context("ReviewContent");
pub const REVIEW_OVERVIEW: Context = Context("ReviewOverview");
pub const REVIEW_FILTER: Context = Context("ReviewFilter");
pub const REVIEW_IMAGE: Context = Context("ReviewImage");
pub const REVIEW_DEFINITION: Context = Context("ReviewDefinition");
pub const REVIEW_HELP: Context = Context("ReviewHelp");
pub const REVIEW_CLONE: Context = Context("ReviewClone");
pub const REVIEW_MODELS: Context = Context("ReviewModels");
pub const REVIEW_LOCAL_LIST: Context = Context("ReviewLocalList");
pub const REVIEW_LOCAL_ROOTS: Context = Context("ReviewLocalRoots");
pub const REVIEW_LOCAL_SELECT: Context = Context("ReviewLocalSelect");
pub const WORKFLOW_HOME: Context = Context("WorkflowHome");
pub const WORKFLOW_CONTROLS: Context = Context("WorkflowControls");
pub const WORKFLOW_REVIEWERS: Context = Context("WorkflowReviewers");
pub const WORKFLOW_COMPOSE: Context = Context("WorkflowCompose");
pub const WORKFLOW_MENTIONS: Context = Context("WorkflowMentions");
pub const WORKFLOW_CHOICE: Context = Context("WorkflowChoice");
pub const WORKFLOW_NEXT: Context = Context("WorkflowNext");
pub const WORKFLOW_CONFIRM: Context = Context("WorkflowConfirm");
pub const WORKFLOW_RESULT: Context = Context("WorkflowResult");
pub const WORKFLOW_TREES: Context = Context("WorkflowTrees");
pub const WORKFLOW_DELETE: Context = Context("WorkflowDelete");
pub const WORKFLOW_RESOLVE: Context = Context("WorkflowResolve");
pub const WORKFLOW_BUSY: Context = Context("WorkflowBusy");
pub const PALETTE: Context = Context("Palette");
pub const SETTINGS: Context = Context("Settings");
pub const SETTINGS_SEARCH: Context = Context("SettingsSearch");
pub const SETTINGS_EDIT: Context = Context("SettingsEdit");
pub const SETTINGS_CONFIRM: Context = Context("SettingsConfirm");
pub const SHORTCUT_CAPTURE: Context = Context("ShortcutCapture");
pub const SHORTCUT_TEST: Context = Context("ShortcutTest");
pub const ALL: &[Context] = &[
    AGENT_LIST,
    AGENT_COMPOSER,
    AGENT_COMPOSER_PANES,
    AGENT_CONVERSATION,
    AGENT_CONVERSATION_INPUT,
    AGENT_CHANGES,
    AGENT_CHANGE_TREE,
    AGENT_RESOURCES,
    AGENT_HELPER,
    AGENT_TERMINAL,
    AGENT_BROWSER,
    AGENT_QUESTION,
    AGENT_QUESTION_NOTE,
    AGENT_APPROVAL,
    AGENT_APPROVAL_TEXT,
    AGENT_PENDING,
    AGENT_QUEUE,
    AGENT_QUEUE_EDITOR,
    AGENT_QUEUE_BUTTON,
    AGENT_PLAN,
    AGENT_PLAN_FEEDBACK,
    AGENT_COMMANDS,
    AGENT_MENU,
    AGENT_HELP,
    AGENT_MODEL,
    AGENT_MODEL_PICKER,
    AGENT_DEFAULTS,
    AGENT_PERMISSIONS,
    AGENT_PERMISSION_SEARCH,
    AGENT_PERMISSION_FORM,
    AGENT_PERMISSION_DELETE,
    AGENT_MCP,
    AGENT_REMOTE,
    AGENT_RULES,
    AGENT_SLACK,
    AGENT_THEMES,
    AGENT_TWEAKS,
    AGENT_USAGE,
    AGENT_TRANSCRIPT,
    AGENT_STATUS,
    AGENT_RESOURCE_PICKER,
    AGENT_RESOURCE_LIST,
    AGENT_REPOSITORY,
    AGENT_CHANGE_REPOSITORY,
    AGENT_RENAME,
    AGENT_DELETE,
    AGENT_CLEAR,
    REVIEW_HOME,
    REVIEW_REPOSITORY,
    REVIEW_NAVIGATION,
    REVIEW_CONTENT,
    REVIEW_OVERVIEW,
    REVIEW_FILTER,
    REVIEW_IMAGE,
    REVIEW_DEFINITION,
    REVIEW_HELP,
    REVIEW_CLONE,
    REVIEW_MODELS,
    REVIEW_LOCAL_LIST,
    REVIEW_LOCAL_ROOTS,
    REVIEW_LOCAL_SELECT,
    WORKFLOW_HOME,
    WORKFLOW_CONTROLS,
    WORKFLOW_REVIEWERS,
    WORKFLOW_COMPOSE,
    WORKFLOW_MENTIONS,
    WORKFLOW_CHOICE,
    WORKFLOW_NEXT,
    WORKFLOW_CONFIRM,
    WORKFLOW_RESULT,
    WORKFLOW_TREES,
    WORKFLOW_DELETE,
    WORKFLOW_RESOLVE,
    WORKFLOW_BUSY,
    PALETTE,
    SETTINGS,
    SETTINGS_SEARCH,
    SETTINGS_EDIT,
    SETTINGS_CONFIRM,
    SHORTCUT_CAPTURE,
    SHORTCUT_TEST,
];
