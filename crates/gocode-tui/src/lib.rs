//! Terminal user interface for Gocode.

use crossterm::event;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use gocode_core::{
    AgentActivityState, AppCommand, AppEvent, ChatMessage, PermissionMode, SessionSummary,
    ToolActivityStatus,
};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Gauge, Paragraph, Tabs, Wrap},
};
use std::fmt::Write as _;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const SCROLL_STEP: usize = 5;
const MAX_TOOL_OUTPUT_CHARS: usize = 4000;
const COLLAPSED_OUTPUT_LINES: usize = 5;
const EXPANDED_OUTPUT_LINES: usize = 200;
const MIN_WIDTH: u16 = 24;
const MIN_HEIGHT: u16 = 6;

/// The active top-level interface screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Screen {
    /// Startup state before the application is ready for input.
    #[default]
    Boot,
    /// Credential onboarding before remote provider work starts.
    Onboarding,
    /// Authenticated NVIDIA models waiting for a user selection.
    ModelPicker,
    /// Menu to change the API key, model, or reasoning effort from the chat screen.
    Settings,
    /// Reasoning-effort level selection.
    EffortPicker,
    /// Saved-session picker, opened by `/resume`.
    SessionPicker,
    /// Main conversational interface.
    Chat,
}

/// Reasoning-effort choices offered by the effort picker, paired with the provider value sent
/// when selected (`None` omits the field entirely).
const EFFORT_OPTIONS: &[(&str, Option<&str>)] = &[
    ("None", None),
    ("Low", Some("low")),
    ("Medium", Some("medium")),
    ("High", Some("high")),
];

/// Menu items shown on the settings screen, in display order.
const SETTINGS_ITEMS: &[&str] = &["Change API key", "Change model", "Change reasoning effort"];

/// Highlight color for the currently selected slash-command suggestion (reddish pink).
const SUGGESTION_HIGHLIGHT_COLOR: Color = Color::Rgb(255, 92, 130);

/// Maximum slash-command suggestion rows shown at once, so a large command list can never grow
/// the composer without bound; the list scrolls to keep the highlighted entry in view instead.
const MAX_VISIBLE_SUGGESTIONS: usize = 6;

/// Maximum prompt history entries remembered for Up/Down recall.
const MAX_PROMPT_HISTORY: usize = 200;

/// Highlight color for a mouse-selected range of transcript text (a weak/soft blue).
const SELECTION_COLOR: Color = Color::Rgb(120, 170, 230);

/// How long a second Ctrl+C must arrive after the first for the app to actually exit.
const DOUBLE_CTRL_C_WINDOW: Duration = Duration::from_millis(600);

/// How long the "Copied N chars to clipboard" notification stays visible.
const COPY_NOTIFICATION_DURATION: Duration = Duration::from_secs(2);

/// One rendered fact in the chat transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEntry {
    /// A message the user sent.
    User(String),
    /// Assistant text, appended to while a response streams.
    Assistant(String),
    /// One tool call's lifecycle, updated in place as it progresses.
    Tool {
        /// Correlation id shared by the matching start/finish pair.
        id: String,
        /// Model-facing tool name.
        name: String,
        /// Current lifecycle status.
        status: ToolActivityStatus,
        /// Short human-readable status detail.
        detail: String,
        /// Accumulated output, bounded and expandable.
        output: String,
        /// Whether the full output is currently shown.
        expanded: bool,
    },
    /// Files affected by the tool calls in one run.
    FileChanges(Vec<String>),
    /// A non-fatal condition worth surfacing.
    Warning(String),
    /// A recoverable, inline error.
    Error(String),
    /// A neutral status note (command output, completion summary).
    Info(String),
}

/// A pending permission confirmation shown as a modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionPrompt {
    /// Short summary of the requested action.
    pub summary: String,
    /// Working directory the action would run or write in.
    pub working_directory: String,
}

/// Which screen the update popup is currently showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStage {
    /// "New update" with the version change and Yes/No buttons.
    Prompt,
    /// Downloading and verifying the update, with a percentage when known.
    Downloading {
        percent: Option<u8>,
        message: String,
    },
    /// The update is staged and ready; a Close button finishes the install and restarts.
    Completed { message: String },
    /// Something went wrong; a Close button dismisses the popup.
    Failed(String),
}

/// A user-approved update prompt, delayed until it cannot interrupt work, and the screen its
/// popup is currently showing (menu, download progress, completion, or failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePrompt {
    pub current_version: String,
    pub version: String,
    pub notes: String,
    pub stage: UpdateStage,
}

/// A parsed slash command handled entirely by the interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    /// Reopen the model picker.
    Model,
    /// Open the settings menu to change the API key, model, or reasoning effort.
    Settings,
    /// Show the active provider.
    Provider,
    /// Show the resolved model, provider, and reasoning effort.
    Status,
    /// Clear the conversation view and forget the current session's history.
    Clear,
    /// Summarize and shrink the remembered conversation history right now.
    Compact,
    /// Toggle automatic compaction when the conversation grows large (on by default).
    AutoCompact,
    /// Start a brand-new session without discarding the current one.
    NewSession,
    /// Open the saved-session picker.
    ResumeSession,
    /// Open the command-reference popup.
    Help,
    /// Generate an `AGENTS.md` overview of the project by asking the agent to explore it.
    Init,
    /// Open the discovered-skills popup.
    Skills,
    /// Open the MCP server manager: list servers, connect/disconnect, inspect their tools.
    Mcp,
    /// Switch permission mode to Plan (read-only).
    PlanMode,
    /// Review a diff, branch, or the working tree's uncommitted changes. The `String` is the
    /// user-supplied target (e.g. a branch name or path); empty means the working tree diff.
    Review(String),
    /// Exit Gocode.
    Exit,
}

/// Which screen the `/skills` popup is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SkillsView {
    /// The "Choose an action" menu: list skills, or enable/disable them.
    #[default]
    Menu,
    /// A read-only, spaced-out listing of every discovered skill.
    List,
    /// A checkbox list for toggling skills on or off for this project.
    EnableDisable,
}

/// Which screen the `/mcp` popup is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum McpView {
    /// The "Choose an action" menu.
    #[default]
    Menu,
    /// Every configured server and its live connection status.
    ServerList,
    /// The tools discovered from one connected server.
    ServerDetail,
    /// The guided "Add server" wizard.
    AddServer,
}

/// One step of the `/mcp` "Add server" wizard, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum McpAddStep {
    #[default]
    Name,
    /// Choosing stdio vs. streamable HTTP.
    TransportChoice,
    /// Stdio only: `command arg1 arg2 ...`, split on whitespace.
    CommandLine,
    /// HTTP only: the server's endpoint URL.
    Url,
    /// Choosing none vs. a static API key.
    AuthChoice,
    /// Only reached when the API-key auth choice was made.
    ApiKey,
}

/// One tab of the `/help` popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HelpTab {
    #[default]
    General,
    Commands,
    Custom,
}

impl HelpTab {
    const ALL: [HelpTab; 3] = [HelpTab::General, HelpTab::Commands, HelpTab::Custom];

    fn title(self) -> &'static str {
        match self {
            HelpTab::General => "General",
            HelpTab::Commands => "Commands",
            HelpTab::Custom => "Custom commands",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    fn next(self) -> HelpTab {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn previous(self) -> HelpTab {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// One recognized slash command: its typed form, description, and variant.
const SLASH_COMMANDS: &[(&str, &str, SlashCommand)] = &[
    ("/model", "Switch the active model", SlashCommand::Model),
    (
        "/settings",
        "Change API key, model, or reasoning effort",
        SlashCommand::Settings,
    ),
    (
        "/provider",
        "Show the active provider",
        SlashCommand::Provider,
    ),
    (
        "/status",
        "Show the resolved model, provider, and reasoning effort",
        SlashCommand::Status,
    ),
    (
        "/clear",
        "Clear the conversation and forget this session's history",
        SlashCommand::Clear,
    ),
    (
        "/compact",
        "Summarize and shrink the conversation history now",
        SlashCommand::Compact,
    ),
    (
        "/autocompact",
        "Toggle automatic compaction when context gets large (on by default)",
        SlashCommand::AutoCompact,
    ),
    (
        "/new",
        "Start a new session without discarding the current one",
        SlashCommand::NewSession,
    ),
    (
        "/resume",
        "Pick a previous session to continue",
        SlashCommand::ResumeSession,
    ),
    ("/help", "Show the command reference", SlashCommand::Help),
    (
        "/init",
        "Explore the project and write an AGENTS.md overview",
        SlashCommand::Init,
    ),
    (
        "/skills",
        "List discovered skills (global and project)",
        SlashCommand::Skills,
    ),
    (
        "/mcp",
        "Manage MCP servers: list, connect, disconnect, inspect tools",
        SlashCommand::Mcp,
    ),
    (
        "/plan",
        "Switch to Plan mode (read-only, no writes or risky commands)",
        SlashCommand::PlanMode,
    ),
    (
        "/review",
        "Review a diff, branch, or the working tree's changes",
        SlashCommand::Review(String::new()),
    ),
    ("/exit", "Exit Gocode", SlashCommand::Exit),
    ("/quit", "Exit Gocode", SlashCommand::Exit),
];

/// Slash-command suggestions matching the current composer prefix: every built-in command plus
/// any discovered project-local custom command, in that order.
#[must_use]
pub fn slash_suggestions(
    input: &str,
    custom_commands: &[gocode_core::CustomCommand],
) -> Vec<(String, String)> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    let builtins = SLASH_COMMANDS
        .iter()
        .filter(|(name, _, _)| name.starts_with(input))
        .map(|(name, description, _)| ((*name).to_string(), (*description).to_string()));
    let custom = custom_commands
        .iter()
        .map(|command| (format!("/{}", command.name), command.description.clone()))
        .filter(|(name, _)| name.starts_with(input));
    builtins.chain(custom).collect()
}

fn resolve_slash_command(input: &str) -> Option<SlashCommand> {
    SLASH_COMMANDS
        .iter()
        .find(|(name, _, _)| *name == input)
        .map(|(_, _, command)| command.clone())
}

/// Finds the custom command exactly named `name` (including the leading `/`), if any.
fn resolve_custom_command<'a>(
    custom_commands: &'a [gocode_core::CustomCommand],
    name: &str,
) -> Option<&'a gocode_core::CustomCommand> {
    custom_commands
        .iter()
        .find(|command| format!("/{}", command.name) == name)
}

/// Expands a custom command's body, substituting every `$ARGUMENTS` with the text typed after
/// the command name (empty when none was given).
fn expand_custom_command(body: &str, arguments: &str) -> String {
    body.replace("$ARGUMENTS", arguments)
}

/// A composer submission: either a model prompt or a client-handled command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatSubmission {
    /// Text to send to the model.
    Prompt(String),
    /// A recognized slash command.
    Command(SlashCommand),
}

/// Renderable interface state derived from application events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag independently tracks one popup/toggle's visibility; a state machine \
              would not simplify the largely orthogonal screen and modal state here"
)]
pub struct AppState {
    /// Visible top-level screen.
    pub screen: Screen,
    credential_input: String,
    models: Vec<String>,
    selected_model: usize,
    current_model: Option<String>,
    settings_selected: usize,
    selected_effort: usize,
    current_effort: Option<String>,
    chat_input: String,
    /// Char index (not byte index) of the composer's insertion point.
    cursor: usize,
    suggestion_selected: usize,
    permission_mode: PermissionMode,
    /// Inverted so the derived `Default` (false) means automatic compaction is on, matching the
    /// documented default.
    auto_compact_disabled: bool,
    sessions: Vec<SessionSummary>,
    selected_session: usize,
    selection: Option<Selection>,
    copy_notification: Option<String>,
    entries: Vec<ChatEntry>,
    streaming_assistant: bool,
    file_change_buffer: Vec<String>,
    activity: Option<AgentActivityState>,
    pending_permission: Option<PermissionPrompt>,
    pending_update: Option<UpdatePrompt>,
    /// Which button is highlighted on the update popup's Yes/No prompt screen: `false` selects
    /// Yes (the default), `true` selects No.
    update_selected_no: bool,
    /// Whether the `/help` popup is currently shown over the chat screen.
    help_visible: bool,
    /// Which tab of the `/help` popup is currently selected.
    help_tab: HelpTab,
    /// Lines scrolled down from the top of the current `/help` tab's content.
    help_scroll: u16,
    /// Whether the `/skills` popup is currently shown over the chat screen.
    skills_visible: bool,
    /// Which screen of the `/skills` popup is currently shown.
    skills_view: SkillsView,
    /// Selected row within the current `/skills` screen (menu action, or skill index).
    skills_selected: usize,
    /// Lines scrolled down from the top of the `/skills` "List skills" screen.
    skills_list_scroll: u16,
    /// Project-local slash commands discovered under `.gocode/commands/`.
    custom_commands: Vec<gocode_core::CustomCommand>,
    /// Global and project skills discovered at boot.
    skills: Vec<gocode_core::SkillSummary>,
    /// Whether the `/mcp` popup is currently shown over the chat screen.
    mcp_visible: bool,
    /// Which screen of the `/mcp` popup is currently shown.
    mcp_view: McpView,
    /// Selected row within the current `/mcp` screen (menu action, or server index).
    mcp_selected: usize,
    /// Configured MCP servers and their live connection status, refreshed after every
    /// connect/disconnect.
    mcp_servers: Vec<gocode_core::McpServerStatus>,
    /// Lines scrolled down from the top of the `/mcp` server detail screen.
    mcp_detail_scroll: u16,
    /// Current step of the `/mcp` "Add server" wizard.
    mcp_add_step: McpAddStep,
    /// Wizard draft: server name.
    mcp_add_name: String,
    /// Wizard draft: `true` for HTTP, `false` (default) for stdio.
    mcp_add_http: bool,
    /// Wizard draft: stdio command line (`command arg1 arg2 ...`).
    mcp_add_command_line: String,
    /// Wizard draft: HTTP endpoint URL.
    mcp_add_url: String,
    /// Wizard draft: `true` for a static API key, `false` (default) for no auth.
    mcp_add_api_key_auth: bool,
    /// Wizard draft: the API key itself, shown masked.
    mcp_add_api_key: String,
    /// Display form of the detected project root, shown by `/status`.
    working_directory: String,
    /// The active session's id, shown by `/status`.
    current_session_id: String,
    /// The active session's display name, shown by `/status`.
    current_session_name: String,
    /// Input tokens reported for the last completed run's last turn, shown by `/status` as an
    /// approximate context-usage figure.
    last_input_tokens: Option<u64>,
    /// Set while `/model` (not `/settings`) is driving the model picker, so a selection there
    /// chains into the effort picker instead of returning straight to chat.
    model_flow_pending_effort: bool,
    /// The model chosen by an in-progress `/model` flow, applied together with whatever effort
    /// is chosen next.
    pending_model: Option<String>,
    queued_update: Option<UpdatePrompt>,
    exit_for_update: bool,
    blocking_error: Option<String>,
    status: Option<String>,
    scroll: usize,
    last_failed_prompt: Option<String>,
    last_submitted_prompt: Option<String>,
    queued: Option<String>,
    /// Previously submitted prompts, oldest first, for Up/Down recall.
    prompt_history: Vec<String>,
    /// Index into `prompt_history` currently shown in the composer, while browsing history.
    history_cursor: Option<usize>,
    /// The composer's text right before history browsing started, restored once you cycle past
    /// the newest entry back to "now".
    draft_before_history: Option<String>,
}

impl AppState {
    /// Applies a normalized application event to the render state.
    ///
    /// Returns a queued prompt to auto-submit, if the run that just ended freed one up.
    #[allow(
        clippy::too_many_lines,
        reason = "the event contract is intentionally centralized in the render-state reducer"
    )]
    pub fn apply(&mut self, event: &AppEvent) -> Option<String> {
        match event {
            AppEvent::BootStarted => self.screen = Screen::Boot,
            AppEvent::BootCompleted => self.screen = Screen::Chat,
            AppEvent::ProjectContextAvailable { working_directory } => {
                self.working_directory.clone_from(working_directory);
            }
            AppEvent::CustomCommandsAvailable(commands) => {
                self.custom_commands.clone_from(commands);
            }
            AppEvent::SkillsAvailable(skills) => self.skills.clone_from(skills),
            AppEvent::McpServersAvailable(servers) => self.mcp_servers.clone_from(servers),
            AppEvent::McpAuthorizationUrlReady { server, url } => {
                self.entries.push(ChatEntry::Info(format!(
                    "Opening your browser to authorize MCP server '{server}'. If it didn't \
                     open, visit: {url}"
                )));
            }
            AppEvent::TerminalResized { .. } => {}
            AppEvent::CredentialRequired => self.screen = Screen::Onboarding,
            AppEvent::CredentialValidationStarted => {
                self.status = Some("Validating NVIDIA API key...".into());
            }
            AppEvent::CredentialValidationFailed(message) => {
                self.screen = Screen::Onboarding;
                self.status = Some(message.clone());
            }
            AppEvent::ModelsAvailable(models) => {
                self.screen = Screen::ModelPicker;
                self.models.clone_from(models);
                self.selected_model = 0;
                self.status = None;
            }
            AppEvent::ModelSelected(model) => {
                self.screen = Screen::Chat;
                self.current_model = Some(model.clone());
                self.status = Some(format!("Model: {model}"));
            }
            AppEvent::AssistantTextDelta(delta) => self.push_assistant_delta(delta),
            AppEvent::ProviderFailed(message) => {
                self.activity = None;
                self.entries.push(ChatEntry::Error(message.clone()));
                self.last_failed_prompt = self.last_submitted_prompt.take();
                return self.queued.take();
            }
            AppEvent::BlockingError(message) => {
                self.activity = None;
                self.blocking_error = Some(message.clone());
            }
            AppEvent::UpdateAvailable {
                current_version,
                version,
                notes,
            } => {
                let prompt = UpdatePrompt {
                    current_version: current_version.clone(),
                    version: version.clone(),
                    notes: notes.clone(),
                    stage: UpdateStage::Prompt,
                };
                if self.screen == Screen::Chat
                    && self.activity.is_none()
                    && self.pending_permission.is_none()
                {
                    self.pending_update = Some(prompt);
                    self.update_selected_no = false;
                } else {
                    self.queued_update = Some(prompt);
                }
            }
            AppEvent::UpdateProgress { percent, message } => {
                if let Some(prompt) = &mut self.pending_update {
                    prompt.stage = UpdateStage::Downloading {
                        percent: *percent,
                        message: message.clone(),
                    };
                }
            }
            AppEvent::UpdateReady { message } => {
                if let Some(prompt) = &mut self.pending_update {
                    prompt.stage = UpdateStage::Completed {
                        message: message.clone(),
                    };
                }
            }
            AppEvent::UpdateFailed(message) => {
                if let Some(prompt) = &mut self.pending_update {
                    prompt.stage = UpdateStage::Failed(message.clone());
                } else {
                    self.entries.push(ChatEntry::Error(message.clone()));
                }
            }
            AppEvent::ExitForUpdate => self.exit_for_update = true,
            AppEvent::AgentStateChanged(state) => self.activity = Some(*state),
            AppEvent::ToolActivity {
                id,
                name,
                status,
                detail,
            } => self.apply_tool_activity(id, name, *status, detail),
            AppEvent::ToolOutputChunk { id, chunk } => self.append_tool_output(id, chunk),
            AppEvent::FileChanged { path, .. } => self.file_change_buffer.push(path.clone()),
            AppEvent::AgentWarning(message) => {
                self.entries.push(ChatEntry::Warning(message.clone()));
            }
            AppEvent::PermissionRequested {
                summary,
                working_directory,
            } => {
                self.pending_permission = Some(PermissionPrompt {
                    summary: summary.clone(),
                    working_directory: working_directory.clone(),
                });
            }
            AppEvent::AgentCompleted {
                final_text,
                turns,
                tool_calls,
                failed_tool_calls,
                last_input_tokens,
            } => {
                self.last_input_tokens = *last_input_tokens;
                self.activity = None;
                self.streaming_assistant = false;
                if let Some(text) = final_text.as_ref().filter(|text| !text.is_empty()) {
                    self.entries.push(ChatEntry::Assistant(text.clone()));
                }
                self.flush_file_changes();
                self.entries.push(ChatEntry::Info(format!(
                    "Done — {turns} turn(s), {tool_calls} tool call(s), {failed_tool_calls} failed."
                )));
                self.last_submitted_prompt = None;
                self.show_queued_update();
                return self.queued.take();
            }
            AppEvent::AgentCancelled => {
                self.activity = None;
                self.streaming_assistant = false;
                self.pending_permission = None;
                self.flush_file_changes();
                self.entries.push(ChatEntry::Info("Cancelled.".into()));
                self.last_submitted_prompt = None;
                self.show_queued_update();
                return self.queued.take();
            }
            AppEvent::ReasoningEffortChanged { effort, announce } => {
                self.current_effort.clone_from(effort);
                if *announce {
                    self.screen = Screen::Chat;
                    let label = effort_label(effort.as_deref());
                    self.entries
                        .push(ChatEntry::Info(format!("Reasoning effort set to: {label}")));
                    self.status = Some(format!("Reasoning effort: {label}"));
                }
            }
            AppEvent::ContextCompacted { automatic } => {
                let message = if *automatic {
                    "Context compacted automatically to stay within the model's context window."
                } else {
                    "Context compacted."
                };
                self.entries.push(ChatEntry::Info(message.into()));
            }
            AppEvent::ContextCompactionFailed(message) => {
                self.entries.push(ChatEntry::Warning(format!(
                    "Could not compact context: {message}"
                )));
            }
            AppEvent::SessionSwitched {
                id,
                name,
                is_new,
                history,
            } => {
                self.current_session_id.clone_from(id);
                self.current_session_name.clone_from(name);
                self.entries.clear();
                self.scroll = 0;
                self.streaming_assistant = false;
                self.file_change_buffer.clear();
                for message in history {
                    match message {
                        ChatMessage::User(text) => {
                            self.entries.push(ChatEntry::User(text.clone()));
                        }
                        ChatMessage::Assistant {
                            text: Some(text), ..
                        } if !text.is_empty() => {
                            self.entries.push(ChatEntry::Assistant(text.clone()));
                        }
                        ChatMessage::Assistant { .. }
                        | ChatMessage::Tool { .. }
                        | ChatMessage::System(_) => {}
                    }
                }
                let verb = if *is_new { "Started" } else { "Resumed" };
                self.entries
                    .push(ChatEntry::Info(format!("{verb} session: {name}")));
                self.screen = Screen::Chat;
            }
            AppEvent::SessionListAvailable(summaries) => {
                self.sessions.clone_from(summaries);
                self.selected_session = 0;
            }
            AppEvent::SessionResumeFailed(message) => {
                self.entries.push(ChatEntry::Error(format!(
                    "Could not resume session: {message}"
                )));
                self.screen = Screen::Chat;
            }
        }
        None
    }

    fn push_assistant_delta(&mut self, delta: &str) {
        if self.streaming_assistant
            && let Some(ChatEntry::Assistant(text)) = self.entries.last_mut()
        {
            text.push_str(delta);
            return;
        }
        self.entries.push(ChatEntry::Assistant(delta.to_string()));
        self.streaming_assistant = true;
    }

    fn show_queued_update(&mut self) {
        if self.screen == Screen::Chat
            && self.pending_permission.is_none()
            && self.pending_update.is_none()
            && self.queued_update.is_some()
        {
            self.pending_update = self.queued_update.take();
            self.update_selected_no = false;
        }
    }

    /// Optimistically switches the update popup to the download-progress screen the instant
    /// the user accepts, instead of leaving the Yes/No prompt on screen until the first
    /// [`AppEvent::UpdateProgress`] arrives.
    fn begin_update_download(&mut self) {
        if let Some(prompt) = &mut self.pending_update {
            prompt.stage = UpdateStage::Downloading {
                percent: None,
                message: "Downloading update…".into(),
            };
        }
    }

    fn apply_tool_activity(
        &mut self,
        id: &str,
        name: &str,
        status: ToolActivityStatus,
        detail: &str,
    ) {
        for entry in self.entries.iter_mut().rev() {
            if let ChatEntry::Tool {
                id: entry_id,
                name: entry_name,
                status: entry_status,
                detail: entry_detail,
                ..
            } = entry
                && entry_id == id
            {
                *entry_status = status;
                entry_detail.clear();
                entry_detail.push_str(detail);
                if !name.is_empty() {
                    entry_name.clear();
                    entry_name.push_str(name);
                }
                return;
            }
        }
        self.entries.push(ChatEntry::Tool {
            id: id.to_string(),
            name: name.to_string(),
            status,
            detail: detail.to_string(),
            output: String::new(),
            expanded: false,
        });
    }

    fn append_tool_output(&mut self, id: &str, chunk: &str) {
        for entry in self.entries.iter_mut().rev() {
            if let ChatEntry::Tool {
                id: entry_id,
                output,
                ..
            } = entry
                && entry_id == id
            {
                if output.len() < MAX_TOOL_OUTPUT_CHARS {
                    output.push_str(chunk);
                    if output.len() > MAX_TOOL_OUTPUT_CHARS {
                        output.truncate(MAX_TOOL_OUTPUT_CHARS);
                        output.push_str("\n… output truncated …");
                    }
                }
                return;
            }
        }
    }

    fn flush_file_changes(&mut self) {
        if !self.file_change_buffer.is_empty() {
            self.entries.push(ChatEntry::FileChanges(std::mem::take(
                &mut self.file_change_buffer,
            )));
        }
    }

    /// Toggles full/collapsed output on the most recent tool activity entry.
    pub fn toggle_last_tool_output(&mut self) {
        for entry in self.entries.iter_mut().rev() {
            if let ChatEntry::Tool { expanded, .. } = entry {
                *expanded = !*expanded;
                return;
            }
        }
    }

    /// Records that a prompt was just sent, adding it to the transcript and marking the run
    /// active. Callers own actually sending the corresponding [`AppCommand::SubmitChat`].
    pub fn begin_run(&mut self, prompt: String) {
        self.entries.push(ChatEntry::User(prompt.clone()));
        self.last_submitted_prompt = Some(prompt);
        self.activity = Some(AgentActivityState::Thinking);
        self.streaming_assistant = false;
        self.scroll = 0;
    }

    /// Scrolls the transcript further from the bottom (toward older entries).
    pub fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_add(SCROLL_STEP);
    }

    /// Scrolls the transcript toward the bottom; reaching zero re-locks to newest output.
    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_sub(SCROLL_STEP);
    }

    /// Whether the transcript is pinned to the newest entry.
    #[must_use]
    pub fn is_scroll_locked(&self) -> bool {
        self.scroll == 0
    }

    fn autocomplete(&mut self) {
        let suggestions = slash_suggestions(&self.chat_input, &self.custom_commands);
        if suggestions.is_empty() {
            return;
        }
        let index = self.suggestion_selected.min(suggestions.len() - 1);
        self.set_chat_input(suggestions[index].0.clone());
    }

    /// Replaces the composer's text wholesale, moving the cursor to its end.
    fn set_chat_input(&mut self, text: String) {
        self.cursor = text.chars().count();
        self.chat_input = text;
        self.suggestion_selected = 0;
        self.history_cursor = None;
        self.draft_before_history = None;
    }

    /// Empties the composer and resets the cursor to the start.
    fn clear_chat_input(&mut self) {
        self.chat_input.clear();
        self.cursor = 0;
        self.suggestion_selected = 0;
    }

    /// Inserts `text` at the cursor and advances the cursor past it.
    fn insert_at_cursor(&mut self, text: &str) {
        let byte_index = char_to_byte_index(&self.chat_input, self.cursor);
        self.chat_input.insert_str(byte_index, text);
        self.cursor += text.chars().count();
        self.suggestion_selected = 0;
        self.history_cursor = None;
        self.draft_before_history = None;
    }

    /// Removes the character immediately before the cursor, if any.
    fn backspace_at_cursor(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end_byte = char_to_byte_index(&self.chat_input, self.cursor);
        let start_byte = char_to_byte_index(&self.chat_input, self.cursor - 1);
        self.chat_input.replace_range(start_byte..end_byte, "");
        self.cursor -= 1;
        self.suggestion_selected = 0;
        self.history_cursor = None;
        self.draft_before_history = None;
    }

    /// Records a submitted prompt for Up/Down recall, skipping an immediate repeat of the last
    /// entry (matching typical shell-history behavior).
    fn remember_prompt(&mut self, prompt: &str) {
        if !prompt.is_empty() && self.prompt_history.last().map(String::as_str) != Some(prompt) {
            self.prompt_history.push(prompt.to_string());
            if self.prompt_history.len() > MAX_PROMPT_HISTORY {
                self.prompt_history.remove(0);
            }
        }
        self.history_cursor = None;
        self.draft_before_history = None;
    }

    /// Walks one entry further back (older) in prompt history, starting a browse session and
    /// stashing the in-progress draft on the first press. A no-op once at the oldest entry.
    fn recall_previous_prompt(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }
        let target_index = match self.history_cursor {
            None => self.prompt_history.len() - 1,
            Some(0) => return,
            Some(index) => index - 1,
        };
        if self.history_cursor.is_none() {
            self.draft_before_history = Some(self.chat_input.clone());
        }
        let draft = self.draft_before_history.take();
        self.set_chat_input(self.prompt_history[target_index].clone());
        self.history_cursor = Some(target_index);
        self.draft_before_history = draft;
    }

    /// Walks one entry forward (newer) in prompt history, restoring the stashed draft once you
    /// cycle past the newest entry back to "now". A no-op when not currently browsing.
    fn recall_next_prompt(&mut self) {
        let Some(index) = self.history_cursor else {
            return;
        };
        if index + 1 < self.prompt_history.len() {
            let draft = self.draft_before_history.take();
            self.set_chat_input(self.prompt_history[index + 1].clone());
            self.history_cursor = Some(index + 1);
            self.draft_before_history = draft;
        } else {
            let draft = self.draft_before_history.take().unwrap_or_default();
            self.set_chat_input(draft);
        }
    }

    /// Moves the cursor one character left, clamped to the start.
    fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor one character right, clamped to the end.
    fn move_cursor_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chat_input.chars().count());
    }

    /// Moves the cursor to the same column on the line above (`delta < 0`) or below
    /// (`delta > 0`), clamped to the shorter line's length. A no-op past the first/last line.
    fn move_cursor_vertical(&mut self, delta: isize) {
        let lines: Vec<&str> = self.chat_input.split('\n').collect();
        let (line, col) = cursor_line_col(&lines, self.cursor);
        let Some(target_line) = line
            .checked_add_signed(delta)
            .filter(|line| *line < lines.len())
        else {
            return;
        };
        let target_col = col.min(lines[target_line].chars().count());
        self.cursor = char_index_from_line_col(&lines, target_line, target_col);
    }

    /// Appends one masked-on-screen credential character during onboarding.
    pub fn push_credential_character(&mut self, character: char) {
        if self.screen == Screen::Onboarding {
            self.credential_input.push(character);
        }
    }

    /// Moves a nonempty onboarding credential out of UI state for immediate runtime validation.
    pub fn take_credential_submission(&mut self) -> Option<String> {
        (self.screen == Screen::Onboarding && !self.credential_input.is_empty())
            .then(|| std::mem::take(&mut self.credential_input))
    }

    /// Returns the currently highlighted model, if discovery provided one.
    #[must_use]
    pub fn selected_model(&self) -> Option<String> {
        self.models.get(self.selected_model).cloned()
    }
}

/// Gocode's version, taken from the workspace-wide package version so it always matches the
/// running binary.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// ASCII banner: a blocky "GOCODE" wordmark. Scrolls up into history with the rest of the
/// transcript rather than staying pinned to the top of the viewport.
const GOCODE_BANNER: &[&str] = &[
    "█████ █████ █████ █████ ████  █████",
    "█     █   █ █     █   █ █   █ █    ",
    "█  ██ █   █ █     █   █ █   █ ████ ",
    "█   █ █   █ █     █   █ █   █ █    ",
    "█████ █████ █████ █████ ████  █████",
];

/// Builds the scrolling banner: ASCII art, version, active model, and working directory.
fn banner_lines(state: &AppState) -> Vec<String> {
    let mut lines: Vec<String> = GOCODE_BANNER
        .iter()
        .map(|line| (*line).to_string())
        .collect();
    lines.push(String::new());
    let model = state
        .current_model
        .as_deref()
        .unwrap_or("no model selected");
    lines.push(format!("Gocode v{VERSION} · {model} · NVIDIA NIM"));
    if let Ok(cwd) = std::env::current_dir() {
        lines.push(cwd.display().to_string());
    }
    lines.push(String::new());
    lines
}

fn compose_lines(state: &AppState) -> Vec<String> {
    let mut lines = banner_lines(state);

    if state.entries.is_empty() {
        lines.push("What can I help you build?".into());
        return lines;
    }

    for entry in &state.entries {
        match entry {
            ChatEntry::User(text) => push_wrapped(&mut lines, "You: ", text),
            ChatEntry::Assistant(text) => push_wrapped(&mut lines, "Gocode: ", text),
            ChatEntry::Tool {
                name,
                status,
                detail,
                output,
                expanded,
                ..
            } => {
                let marker = match status {
                    ToolActivityStatus::Started => "…",
                    ToolActivityStatus::Succeeded => "✓",
                    ToolActivityStatus::Failed => "✗",
                    ToolActivityStatus::Denied | ToolActivityStatus::Cancelled => "⊘",
                };
                lines.push(format!("  {marker} {name}: {detail}"));
                if !output.is_empty() {
                    let output_lines: Vec<&str> = output.lines().collect();
                    let limit = if *expanded {
                        EXPANDED_OUTPUT_LINES
                    } else {
                        COLLAPSED_OUTPUT_LINES
                    };
                    for line in output_lines.iter().take(limit) {
                        lines.push(format!("      {line}"));
                    }
                    if output_lines.len() > limit {
                        let hidden = output_lines.len() - limit;
                        let action = if *expanded { "collapse" } else { "expand" };
                        lines.push(format!(
                            "      … {hidden} more line(s) (Ctrl+O to {action})"
                        ));
                    }
                }
            }
            ChatEntry::FileChanges(paths) => {
                lines.push(format!("  Modified files: {}", paths.join(", ")));
            }
            ChatEntry::Warning(text) => lines.push(format!("  ⚠ {text}")),
            ChatEntry::Error(text) => lines.push(format!("  ✗ {text}")),
            ChatEntry::Info(text) => lines.push(format!("  · {text}")),
        }
        lines.push(String::new());
    }

    if let Some(activity) = state.activity {
        lines.push(match activity {
            AgentActivityState::Thinking => "Gocode is thinking…".into(),
            AgentActivityState::RunningTools => "Gocode is running tools…".into(),
        });
    }
    if let Some(queued) = &state.queued {
        lines.push(format!("Queued: {queued}"));
    }

    lines
}

fn push_wrapped(lines: &mut Vec<String>, prefix: &str, text: &str) {
    let indent = " ".repeat(prefix.chars().count());
    for (index, line) in text.lines().enumerate() {
        if index == 0 {
            lines.push(format!("{prefix}{line}"));
        } else {
            lines.push(format!("{indent}{line}"));
        }
    }
}

/// Converts a char index into a byte index into `text`, so `char_index` characters in never
/// lands on a multi-byte UTF-8 boundary. Char indices past the end resolve to `text.len()`.
fn char_to_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(byte_index, _)| byte_index)
}

/// Resolves a char-index cursor to its zero-based `(line, column)` within `\n`-split `lines`.
fn cursor_line_col(lines: &[&str], cursor: usize) -> (usize, usize) {
    let mut remaining = cursor;
    for (line_index, line) in lines.iter().enumerate() {
        let line_len = line.chars().count();
        if remaining <= line_len {
            return (line_index, remaining);
        }
        remaining -= line_len + 1; // the '\n' that joined this line to the next
    }
    let last = lines.len().saturating_sub(1);
    (last, lines.get(last).map_or(0, |line| line.chars().count()))
}

/// The inverse of [`cursor_line_col`]: the char-index cursor position at `(line, col)`.
fn char_index_from_line_col(lines: &[&str], line: usize, col: usize) -> usize {
    let mut index = 0;
    for line_text in lines.iter().take(line) {
        index += line_text.chars().count() + 1;
    }
    index + col.min(lines.get(line).map_or(0, |line| line.chars().count()))
}

/// Outcome of classifying one terminal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// Keep the application running; a resize or non-actionable event may trigger a redraw.
    Continue,
    /// Interrupt active work before exiting the application.
    Interrupt,
}

/// Renders the active application screen.
pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new("Terminal too small — resize to continue.").wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    match state.screen {
        Screen::Boot => frame.render_widget(
            Paragraph::new("Starting Gocode...")
                .block(Block::default().title("Gocode").borders(Borders::ALL)),
            area,
        ),
        Screen::Onboarding => render_onboarding(frame, state, area),
        Screen::ModelPicker => render_model_picker(frame, state, area),
        Screen::Settings => render_settings(frame, state, area),
        Screen::EffortPicker => render_effort_picker(frame, state, area),
        Screen::SessionPicker => render_session_picker(frame, state, area),
        Screen::Chat => render_chat(frame, state, area),
    }
}

fn render_onboarding(frame: &mut Frame, state: &AppState, area: Rect) {
    let mut content = "NVIDIA API key:\n\n\
        Enter your key. It is validated immediately and stored only in your system credential \
        store afterward — never in plain configuration files.\n\n\
        Prompts and selected project context are sent to NVIDIA NIM once you start chatting.\n\n"
        .to_string();
    content.push_str(&"•".repeat(state.credential_input.chars().count()));
    if let Some(status) = &state.status {
        content.push_str("\n\n");
        content.push_str(status);
    }
    frame.render_widget(
        Paragraph::new(content).wrap(Wrap { trim: false }).block(
            Block::default()
                .title("Gocode · NVIDIA setup")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_model_picker(frame: &mut Frame, state: &AppState, area: Rect) {
    let visible_rows = usize::from(area.height.saturating_sub(2)).max(1);
    let first_visible = state.selected_model.saturating_sub(visible_rows - 1);
    let content = state.models[first_visible..]
        .iter()
        .enumerate()
        .take(visible_rows)
        .map(|(offset, model)| {
            let index = first_visible + offset;
            format!(
                "{} {model}",
                if index == state.selected_model {
                    ">"
                } else {
                    " "
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(
        Paragraph::new(content).block(
            Block::default()
                .title("Gocode · Select NVIDIA model")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_settings(frame: &mut Frame, state: &AppState, area: Rect) {
    let model_label = state.current_model.as_deref().unwrap_or("none selected");
    let effort_label = effort_label(state.current_effort.as_deref());
    let content = SETTINGS_ITEMS
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let detail = match index {
                1 => format!(" (current: {model_label})"),
                2 => format!(" (current: {effort_label})"),
                _ => String::new(),
            };
            let cursor = if index == state.settings_selected {
                ">"
            } else {
                " "
            };
            format!("{cursor} {item}{detail}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(
        Paragraph::new(content).block(
            Block::default()
                .title("Gocode · Settings")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn effort_label(effort: Option<&str>) -> &'static str {
    EFFORT_OPTIONS
        .iter()
        .find(|(_, value)| *value == effort)
        .map_or("None", |(label, _)| *label)
}

fn render_effort_picker(frame: &mut Frame, state: &AppState, area: Rect) {
    let content = EFFORT_OPTIONS
        .iter()
        .enumerate()
        .map(|(index, (label, _))| {
            let cursor = if index == state.selected_effort {
                ">"
            } else {
                " "
            };
            format!("{cursor} {label}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(
        Paragraph::new(content).block(
            Block::default()
                .title("Gocode · Reasoning effort")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_session_picker(frame: &mut Frame, state: &AppState, area: Rect) {
    if state.sessions.is_empty() {
        frame.render_widget(
            Paragraph::new("No saved sessions yet. Send a message, then /new starts another.")
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .title("Gocode · Resume a session")
                        .borders(Borders::ALL),
                ),
            area,
        );
        return;
    }

    let visible_rows = usize::from(area.height.saturating_sub(2)).max(1);
    let first_visible = state
        .selected_session
        .saturating_sub(visible_rows.saturating_sub(1));
    let content = state.sessions[first_visible..]
        .iter()
        .enumerate()
        .take(visible_rows)
        .map(|(offset, session)| {
            let index = first_visible + offset;
            let cursor = if index == state.selected_session {
                ">"
            } else {
                " "
            };
            let when = format_relative_time(session.last_used_at_unix);
            format!(
                "{cursor} {} — {when}\n    {}",
                session.name, session.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(
        Paragraph::new(content).wrap(Wrap { trim: false }).block(
            Block::default()
                .title("Gocode · Resume a session")
                .borders(Borders::ALL),
        ),
        area,
    );
}

/// Formats a Unix timestamp as "N minute(s)/hour(s) ago" within a day, or an American-format
/// date and time (`MM/DD/YYYY, h:mm AM/PM`) beyond that.
fn format_relative_time(unix_seconds: i64) -> String {
    let then = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(unix_seconds.max(0)).unwrap_or(0));
    let now = std::time::SystemTime::now();
    let elapsed = now.duration_since(then).unwrap_or_default();
    let minutes = elapsed.as_secs() / 60;

    if minutes < 1 {
        "just now".to_string()
    } else if minutes < 60 {
        if minutes == 1 {
            "1 minute ago".into()
        } else {
            format!("{minutes} minutes ago")
        }
    } else if minutes < 24 * 60 {
        let hours = minutes / 60;
        if hours == 1 {
            "1 hour ago".into()
        } else {
            format!("{hours} hours ago")
        }
    } else {
        let local: chrono::DateTime<chrono::Local> = then.into();
        local.format("%m/%d/%Y, %-I:%M %p").to_string()
    }
}

/// Splits the chat screen into its history and composer areas. Shared by rendering and mouse
/// hit-testing so both agree exactly on where the history viewport sits.
fn chat_layout(area: Rect, state: &AppState) -> (Rect, Rect) {
    let suggestions = slash_suggestions(&state.chat_input, &state.custom_commands);
    let input_lines = 1 + state.chat_input.matches('\n').count();
    let suggestion_lines = suggestions.len().min(MAX_VISIBLE_SUGGESTIONS);
    let status_lines = usize::from(state.status.is_some());
    // +1 for the permission-mode line always shown below the composer, +2 for the block borders.
    let compose_height =
        u16::try_from(input_lines + suggestion_lines + status_lines + 1 + 2).unwrap_or(u16::MAX);
    let compose_height = compose_height.min(area.height.saturating_sub(3)).max(3);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(compose_height)])
        .split(area);
    (layout[0], layout[1])
}

fn render_chat(frame: &mut Frame, state: &AppState, area: Rect) {
    let suggestions = slash_suggestions(&state.chat_input, &state.custom_commands);
    let (history_area, composer_area) = chat_layout(area, state);

    render_history(frame, state, history_area);
    render_composer(frame, state, composer_area, &suggestions);

    if let Some(message) = &state.copy_notification {
        render_copy_notification(frame, message, history_area);
    }

    if let Some(prompt) = &state.pending_permission {
        render_permission_modal(frame, prompt, area);
    } else if let Some(prompt) = &state.pending_update {
        render_update_modal(frame, state, prompt, area);
    } else if let Some(message) = &state.blocking_error {
        render_blocking_error_modal(frame, message, area);
    } else if state.help_visible {
        render_help_modal(frame, state, area);
    } else if state.skills_visible {
        render_skills_modal(frame, state, area);
    } else if state.mcp_visible {
        render_mcp_modal(frame, state, area);
    }
}

/// Builds the body text shown under the currently selected `/help` tab.
fn help_tab_content(state: &AppState, tab: HelpTab) -> String {
    match tab {
        HelpTab::General => String::from(
            "Gocode understands your codebase, makes edits with your permission, and \
             executes commands — right from your terminal.\n\
             \n\
             Shortcuts\n\
             Tab             autocomplete a suggestion\n\
             Shift+Tab / F2  cycle permission mode\n\
             Ctrl+J          insert a newline\n\
             Alt/Shift+Enter insert a newline\n\
             Up/Down         browse prompt history\n\
             PageUp/PageDown scroll the conversation\n\
             Ctrl+O          expand/collapse tool output\n\
             Ctrl+R          recall the last failed prompt\n\
             Ctrl+C, Ctrl+C  exit Gocode\n\
             Drag + Ctrl+C   copy the selection",
        ),
        HelpTab::Commands => {
            let mut content = String::from("Browse default commands\n\n");
            for (name, description, _) in SLASH_COMMANDS {
                let _ = writeln!(content, "{name:<14} {description}\n");
            }
            content.truncate(content.trim_end().len());
            content
        }
        HelpTab::Custom => {
            if state.custom_commands.is_empty() {
                String::from(
                    "No custom commands found\n\n\
                     Add Markdown files under .gocode/commands/ to define project-local \
                     slash commands.",
                )
            } else {
                let mut content = String::from("Project commands (.gocode/commands/)\n\n");
                for command in &state.custom_commands {
                    let name = format!("/{}", command.name);
                    let description = if command.description.is_empty() {
                        "(no description)"
                    } else {
                        command.description.as_str()
                    };
                    let _ = writeln!(content, "{name:<14} {description}\n");
                }
                content.truncate(content.trim_end().len());
                content
            }
        }
    }
}

fn render_help_modal(frame: &mut Frame, state: &AppState, area: Rect) {
    let content = help_tab_content(state, state.help_tab);
    let content_lines = content.lines().count();
    let height = u16::try_from((content_lines + 5).min(40)).unwrap_or(40);
    let modal = centered(area, 76, height);
    frame.render_widget(Clear, modal);

    let block = Block::default()
        .title("Gocode · Help")
        .borders(Borders::ALL);
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let titles: Vec<Line> = HelpTab::ALL
        .iter()
        .map(|tab| Line::from(tab.title()))
        .collect();
    let tabs = Tabs::new(titles)
        .select(state.help_tab.index())
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(SELECTION_COLOR)
                .add_modifier(Modifier::BOLD),
        )
        .divider(" ");
    frame.render_widget(tabs, chunks[0]);

    let visible_rows = usize::from(chunks[2].height).max(1);
    let max_scroll = u16::try_from(content_lines.saturating_sub(visible_rows)).unwrap_or(u16::MAX);
    let scroll = state.help_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        chunks[2],
    );

    let footer = if max_scroll > 0 {
        "Tab/Shift+Tab to switch tabs · Up/Down to scroll · Esc/Enter to close"
    } else {
        "Tab/Shift+Tab to switch tabs · Esc/Enter to close"
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
}

/// The `/skills` menu's two actions, in display order.
const SKILLS_MENU_ITEMS: [(&str, &str); 2] = [
    ("List skills", "Show every discovered skill."),
    (
        "Enable/Disable Skills",
        "Turn skills on or off for this project.",
    ),
];

fn render_skills_modal(frame: &mut Frame, state: &AppState, area: Rect) {
    match state.skills_view {
        SkillsView::Menu => render_skills_menu(frame, state, area),
        SkillsView::List => render_skills_list(frame, state, area),
        SkillsView::EnableDisable => render_skills_enable_disable(frame, state, area),
    }
}

/// A one-line explanation of what skills are, shown at the top of the `/skills` menu.
const SKILLS_MENU_INTRO: &str =
    "Skills are extra capabilities the model can read on demand to handle specific tasks.";

fn render_skills_menu(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal = centered(area, 76, 11);
    let mut content = format!("{SKILLS_MENU_INTRO}\n\nChoose an action\n\n");
    for (index, (label, description)) in SKILLS_MENU_ITEMS.iter().enumerate() {
        let cursor = if index == state.skills_selected {
            ">"
        } else {
            " "
        };
        let _ = writeln!(content, "{cursor} {}. {label} — {description}", index + 1);
    }
    content.push_str("\nEnter to select · Esc to close");
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(content).wrap(Wrap { trim: false }).block(
            Block::default()
                .title("Gocode · Skills")
                .borders(Borders::ALL),
        ),
        modal,
    );
}

fn render_skills_list(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal = centered(area, 76, 24);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .title("Gocode · Skills")
        .borders(Borders::ALL);
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(Paragraph::new("Discovered skills\n"), chunks[0]);

    let mut content = String::new();
    if state.skills.is_empty() {
        content.push_str("None found in ~/.agents/skills or the project's skills directory.");
    } else {
        for skill in &state.skills {
            let source = match skill.source {
                gocode_core::SkillSource::Global => "global",
                gocode_core::SkillSource::Project => "project",
            };
            let description = if skill.description.is_empty() {
                "(no description)"
            } else {
                skill.description.as_str()
            };
            let status = if skill.enabled { "" } else { " (disabled)" };
            let _ = writeln!(
                content,
                "{} — {description} ({source}){status}\n",
                skill.name
            );
        }
    }
    let content_lines = content.lines().count();
    let visible_rows = usize::from(chunks[1].height).max(1);
    let max_scroll = u16::try_from(content_lines.saturating_sub(visible_rows)).unwrap_or(u16::MAX);
    let scroll = state.skills_list_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        chunks[1],
    );

    let footer = if max_scroll > 0 {
        "Up/Down to scroll · Esc to go back"
    } else {
        "Esc to go back"
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

fn render_skills_enable_disable(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal = centered(area, 76, 24);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .title("Gocode · Enable/Disable Skills")
        .borders(Borders::ALL);
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new("Turn skills on or off. Your changes are saved automatically.\n"),
        chunks[0],
    );

    if state.skills.is_empty() {
        frame.render_widget(
            Paragraph::new("None found in ~/.agents/skills or the project's skills directory."),
            chunks[1],
        );
    } else {
        let visible_rows = usize::from(chunks[1].height).max(1);
        let first_visible = state
            .skills_selected
            .saturating_sub(visible_rows.saturating_sub(1));
        let content = state.skills[first_visible..]
            .iter()
            .enumerate()
            .take(visible_rows)
            .map(|(offset, skill)| {
                let index = first_visible + offset;
                let cursor = if index == state.skills_selected {
                    ">"
                } else {
                    " "
                };
                let checkbox = if skill.enabled { "[x]" } else { "[ ]" };
                let description = if skill.description.is_empty() {
                    "(no description)"
                } else {
                    skill.description.as_str()
                };
                format!("{cursor} {checkbox} {} — {description}", skill.name)
            })
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(Paragraph::new(content), chunks[1]);
    }

    frame.render_widget(
        Paragraph::new("Enter/Space to toggle · Esc to go back")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

/// The `/mcp` menu's actions, in display order.
const MCP_MENU_ITEMS: [(&str, &str); 2] = [
    (
        "Servers",
        "List configured MCP servers, connect/disconnect, and inspect their tools.",
    ),
    (
        "Add server",
        "Configure a new MCP server and save it to this project.",
    ),
];

fn render_mcp_modal(frame: &mut Frame, state: &AppState, area: Rect) {
    match state.mcp_view {
        McpView::Menu => render_mcp_menu(frame, state, area),
        McpView::ServerList => render_mcp_server_list(frame, state, area),
        McpView::ServerDetail => render_mcp_server_detail(frame, state, area),
        McpView::AddServer => render_mcp_add_server(frame, state, area),
    }
}

fn render_mcp_menu(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal = centered(area, 76, 10);
    let mut content = String::from("Choose an action\n\n");
    for (index, (label, description)) in MCP_MENU_ITEMS.iter().enumerate() {
        let cursor = if index == state.mcp_selected {
            ">"
        } else {
            " "
        };
        let _ = writeln!(content, "{cursor} {}. {label} — {description}", index + 1);
    }
    content.push_str("\nEnter to select · Esc to close");
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().title("Gocode · MCP").borders(Borders::ALL)),
        modal,
    );
}

/// One row of the `/mcp` server list: cursor, name, transport, and live status.
fn mcp_server_row(server: &gocode_core::McpServerStatus, selected: bool) -> String {
    let cursor = if selected { ">" } else { " " };
    let status = if let Some(error) = &server.error {
        format!("Error: {error}")
    } else if server.connected {
        format!("Connected ({} tools)", server.tool_count)
    } else {
        "Disconnected".into()
    };
    let auth_hint = if server.needs_authorization && !server.connected {
        " [press o to authorize]"
    } else {
        ""
    };
    format!(
        "{cursor} {} ({}) — {status}{auth_hint}",
        server.name, server.transport
    )
}

fn render_mcp_server_list(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal = centered(area, 76, 24);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .title("Gocode · MCP Servers")
        .borders(Borders::ALL);
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(Paragraph::new("Configured MCP servers\n"), chunks[0]);

    if state.mcp_servers.is_empty() {
        frame.render_widget(
            Paragraph::new(
                "None configured. Add one to ~/.config/gocode/mcp.toml or \
                 .gocode/mcp.toml, then reopen /mcp.",
            )
            .wrap(Wrap { trim: false }),
            chunks[1],
        );
    } else {
        let visible_rows = usize::from(chunks[1].height).max(1);
        let first_visible = state
            .mcp_selected
            .saturating_sub(visible_rows.saturating_sub(1));
        let content = state.mcp_servers[first_visible..]
            .iter()
            .enumerate()
            .take(visible_rows)
            .map(|(offset, server)| {
                let index = first_visible + offset;
                mcp_server_row(server, index == state.mcp_selected)
            })
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(Paragraph::new(content), chunks[1]);
    }

    frame.render_widget(
        Paragraph::new(
            "Enter to connect/disconnect · o to authorize · → to inspect tools · Esc to go back",
        )
        .style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

fn render_mcp_server_detail(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal = centered(area, 76, 24);
    frame.render_widget(Clear, modal);
    let server = state.mcp_servers.get(state.mcp_selected);
    let title = server.map_or_else(
        || "Gocode · MCP Server".to_string(),
        |server| format!("Gocode · MCP · {}", server.name),
    );
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let mut content = String::new();
    match server {
        None => content.push_str("This server is no longer configured."),
        Some(server) => {
            let _ = writeln!(content, "Transport: {}", server.transport);
            let status = if let Some(error) = &server.error {
                format!("Error: {error}")
            } else if server.connected {
                "Connected".to_string()
            } else {
                "Disconnected".to_string()
            };
            let _ = writeln!(content, "Status: {status}\n");
            if server.tool_names.is_empty() {
                content.push_str("No tools discovered yet. Connect this server to list them.");
            } else {
                content.push_str("Tools:\n");
                for tool in &server.tool_names {
                    let _ = writeln!(content, "  - {tool}");
                }
            }
        }
    }

    let content_lines = content.lines().count();
    let visible_rows = usize::from(chunks[0].height).max(1);
    let max_scroll = u16::try_from(content_lines.saturating_sub(visible_rows)).unwrap_or(u16::MAX);
    let scroll = state.mcp_detail_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        chunks[0],
    );

    let footer = if max_scroll > 0 {
        "Up/Down to scroll · Esc to go back"
    } else {
        "Esc to go back"
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

/// Renders a masked stand-in for a secret value, e.g. an in-progress API key.
fn masked(value: &str) -> String {
    "•".repeat(value.chars().count())
}

fn render_mcp_add_server(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal = centered(area, 76, 12);
    frame.render_widget(Clear, modal);

    let (prompt, input, footer): (&str, String, &str) = match state.mcp_add_step {
        McpAddStep::Name => (
            "Server name",
            format!("{}_", state.mcp_add_name),
            "Enter to continue · Esc to cancel",
        ),
        McpAddStep::TransportChoice => {
            let stdio_cursor = if state.mcp_add_http { " " } else { ">" };
            let http_cursor = if state.mcp_add_http { ">" } else { " " };
            (
                "Transport",
                format!("{stdio_cursor} stdio (local command)\n{http_cursor} http (remote server)"),
                "Up/Down to choose · Enter to continue · Esc to go back",
            )
        }
        McpAddStep::CommandLine => (
            "Command, with args (e.g. npx -y @modelcontextprotocol/server-filesystem /path)",
            format!("{}_", state.mcp_add_command_line),
            "Enter to continue · Esc to go back",
        ),
        McpAddStep::Url => (
            "Server URL (e.g. https://mcp.example.com/mcp)",
            format!("{}_", state.mcp_add_url),
            "Enter to continue · Esc to go back",
        ),
        McpAddStep::AuthChoice => {
            let none_cursor = if state.mcp_add_api_key_auth { " " } else { ">" };
            let key_cursor = if state.mcp_add_api_key_auth { ">" } else { " " };
            (
                "Authentication",
                format!("{none_cursor} None\n{key_cursor} API key"),
                "Up/Down to choose · Enter to continue · Esc to go back",
            )
        }
        McpAddStep::ApiKey => (
            "API key (stored in your OS keyring, never written to mcp.toml)",
            format!("{}_", masked(&state.mcp_add_api_key)),
            "Enter to save · Esc to go back",
        ),
    };

    let content = format!("Add MCP server\n\n{prompt}\n\n{input}\n\n{footer}");
    frame.render_widget(
        Paragraph::new(content).wrap(Wrap { trim: false }).block(
            Block::default()
                .title("Gocode · Add MCP Server")
                .borders(Borders::ALL),
        ),
        modal,
    );
}

/// Space kept between the copy-notification text and the history box's border, so the toast
/// never overwrites (and visually breaks) the border itself.
const COPY_NOTIFICATION_MARGIN: u16 = 2;

/// Renders the "Copied N chars to clipboard" toast inside the top-right corner of the history
/// box, on its last content row (directly above the composer) — never on the border row itself.
fn render_copy_notification(frame: &mut Frame, message: &str, history_area: Rect) {
    if history_area.height < 3 || history_area.width <= COPY_NOTIFICATION_MARGIN * 2 + 2 {
        return;
    }
    let max_text_width = history_area
        .width
        .saturating_sub(2 + COPY_NOTIFICATION_MARGIN);
    let text_width = u16::try_from(message.chars().count())
        .unwrap_or(u16::MAX)
        .min(max_text_width);
    if text_width == 0 {
        return;
    }
    let notification_area = Rect {
        x: history_area.x + history_area.width - 1 - COPY_NOTIFICATION_MARGIN - text_width,
        y: history_area.y + history_area.height - 2,
        width: text_width,
        height: 1,
    };
    frame.render_widget(Clear, notification_area);
    frame.render_widget(
        Paragraph::new(Span::styled(
            message.to_string(),
            Style::default()
                .fg(SELECTION_COLOR)
                .add_modifier(Modifier::BOLD),
        )),
        notification_area,
    );
}

/// Style for a Yes/No (or single Close) button; `selected` draws it highlighted.
fn button_span(label: &str, selected: bool) -> Span<'static> {
    let style = if selected {
        Style::default()
            .fg(SELECTION_COLOR)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Span::styled(format!(" {label} "), style)
}

fn render_update_modal(frame: &mut Frame, state: &AppState, prompt: &UpdatePrompt, area: Rect) {
    match &prompt.stage {
        UpdateStage::Prompt => render_update_prompt(frame, state, prompt, area),
        UpdateStage::Downloading { percent, message } => {
            render_update_downloading(frame, prompt, *percent, message, area);
        }
        UpdateStage::Completed { message } => {
            render_update_result(
                frame,
                "Gocode · Update completed",
                "Completed",
                message,
                area,
            );
        }
        UpdateStage::Failed(message) => {
            render_update_result(frame, "Gocode · Update failed", "Failed", message, area);
        }
    }
}

fn render_update_prompt(frame: &mut Frame, state: &AppState, prompt: &UpdatePrompt, area: Rect) {
    let modal = centered(area, 64, 10);
    frame.render_widget(Clear, modal);

    let notes = prompt.notes.lines().next().unwrap_or_default();
    let lines = vec![
        Line::from("New update"),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                prompt.current_version.clone(),
                Style::default().fg(Color::Red),
            ),
            Span::raw("  ->  "),
            Span::styled(prompt.version.clone(), Style::default().fg(Color::Green)),
        ]),
        Line::from(""),
        Line::from(notes),
        Line::from(""),
        Line::from(vec![
            button_span("Yes", !state.update_selected_no),
            Span::raw("  "),
            button_span("No", state.update_selected_no),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .title("Gocode · Update available")
                .borders(Borders::ALL),
        ),
        modal,
    );
}

fn render_update_downloading(
    frame: &mut Frame,
    prompt: &UpdatePrompt,
    percent: Option<u8>,
    message: &str,
    area: Rect,
) {
    let modal = centered(area, 64, 9);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .title(format!("Gocode · Updating to {}", prompt.version))
        .borders(Borders::ALL);
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(1)])
        .split(inner);
    frame.render_widget(Paragraph::new(message.to_string()), chunks[0]);

    let ratio = f64::from(percent.unwrap_or(0)) / 100.0;
    let label = percent.map_or_else(|| "…".to_string(), |percent| format!("{percent}%"));
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(SELECTION_COLOR))
            .label(label)
            .ratio(ratio.clamp(0.0, 1.0)),
        chunks[1],
    );
}

fn render_update_result(frame: &mut Frame, title: &str, heading: &str, message: &str, area: Rect) {
    let modal = centered(area, 64, 9);
    frame.render_widget(Clear, modal);
    let lines = vec![
        Line::from(heading),
        Line::from(""),
        Line::from(message.to_string()),
        Line::from(""),
        Line::from(button_span("Close", true)),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().title(title).borders(Borders::ALL)),
        modal,
    );
}

fn render_history(frame: &mut Frame, state: &AppState, area: Rect) {
    let content_width = usize::from(area.width.saturating_sub(2));
    let wrapped = wrap_lines(&compose_lines(state), content_width);
    let visible_rows = usize::from(area.height.saturating_sub(2)).max(1);
    let (start, end) = compute_visible_window(wrapped.len(), visible_rows, state.scroll);

    let rendered_lines: Vec<Line> = wrapped[start..end]
        .iter()
        .enumerate()
        .map(|(offset, line)| {
            let absolute_index = start + offset;
            let chars: Vec<char> = line.chars().collect();
            let selected_range = state
                .selection
                .as_ref()
                .and_then(|selection| selected_char_range(selection, absolute_index, chars.len()));
            match selected_range {
                Some((from, to)) => {
                    let before: String = chars[..from].iter().collect();
                    let marked: String = chars[from..to].iter().collect();
                    let after: String = chars[to..].iter().collect();
                    Line::from(vec![
                        Span::raw(before),
                        Span::styled(
                            marked,
                            Style::default().bg(SELECTION_COLOR).fg(Color::Black),
                        ),
                        Span::raw(after),
                    ])
                }
                None => Line::from(line.clone()),
            }
        })
        .collect();

    let title = if state.is_scroll_locked() {
        "Gocode · Chat"
    } else {
        "Gocode · Chat (scrolled — End to follow)"
    };
    frame.render_widget(
        Paragraph::new(Text::from(rendered_lines))
            .block(Block::default().title(title).borders(Borders::ALL)),
        area,
    );
}

/// Word-wraps every logical line to `width` columns so the resulting rows match 1:1 what gets
/// drawn to the terminal — the same rows are then used for mouse-selection hit-testing, so
/// rendering and hit-testing can never disagree about where a character lands.
fn wrap_lines(lines: &[String], width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut wrapped = Vec::new();
    for line in lines {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            wrapped.push(String::new());
            continue;
        }
        let mut start = 0usize;
        while start < chars.len() {
            let mut end = (start + width).min(chars.len());
            if end < chars.len()
                && let Some(break_at) = chars[start..end].iter().rposition(|c| *c == ' ')
                && break_at > 0
            {
                end = start + break_at + 1;
            }
            let segment: String = chars[start..end].iter().collect();
            wrapped.push(segment.trim_end_matches(' ').to_string());
            start = end;
            if start < chars.len() && chars[start] == ' ' {
                start += 1;
            }
        }
    }
    wrapped
}

/// Computes the `[start, end)` slice of wrapped lines currently visible, given the scroll offset
/// from the bottom. Shared by rendering and mouse hit-testing.
fn compute_visible_window(total: usize, visible_rows: usize, scroll: usize) -> (usize, usize) {
    let visible_rows = visible_rows.max(1);
    let max_scroll = total.saturating_sub(visible_rows);
    let scroll = scroll.min(max_scroll);
    let end = total.saturating_sub(scroll);
    let start = end.saturating_sub(visible_rows);
    (start, end)
}

/// Determines which suggestions are visible given the current selection, keeping the composer's
/// height bounded no matter how many commands match. Returns `(start, end, truncated)`; when
/// `truncated`, one row of the `MAX_VISIBLE_SUGGESTIONS` budget is reserved for a "N more"
/// summary instead of an entry.
fn visible_suggestion_window(count: usize, selected: usize) -> (usize, usize, bool) {
    if count <= MAX_VISIBLE_SUGGESTIONS {
        return (0, count, false);
    }
    let rows = MAX_VISIBLE_SUGGESTIONS - 1;
    let first = selected
        .saturating_sub(rows.saturating_sub(1))
        .min(count - rows);
    (first, first + rows, true)
}

/// One endpoint of a mouse selection: an absolute index into the full wrapped-lines vector, and
/// a character column within that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct SelectionPoint {
    line: usize,
    col: usize,
}

/// A mouse text selection, anchored where the drag started and extended to the current cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    anchor: SelectionPoint,
    cursor: SelectionPoint,
}

impl Selection {
    /// Returns the two endpoints in document order, regardless of drag direction.
    fn normalized(self) -> (SelectionPoint, SelectionPoint) {
        if (self.anchor.line, self.anchor.col) <= (self.cursor.line, self.cursor.col) {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }
}

/// The character range selected on one wrapped line, if any: the full line for lines strictly
/// between the two endpoints, and a partial range on the endpoint lines themselves.
fn selected_char_range(
    selection: &Selection,
    line_index: usize,
    line_len: usize,
) -> Option<(usize, usize)> {
    let (start, end) = selection.normalized();
    if line_index < start.line || line_index > end.line {
        return None;
    }
    let from = if line_index == start.line {
        start.col.min(line_len)
    } else {
        0
    };
    let to = if line_index == end.line {
        end.col.min(line_len)
    } else {
        line_len
    };
    (from < to).then_some((from, to))
}

fn render_composer(
    frame: &mut Frame,
    state: &AppState,
    area: Rect,
    suggestions: &[(String, String)],
) {
    let input_display_lines: Vec<&str> = state.chat_input.split('\n').collect();
    let input_line_count = input_display_lines.len();
    let mut lines: Vec<Line> = input_display_lines
        .iter()
        .enumerate()
        .map(|(index, line_text)| {
            let prefix = if index == 0 { "> " } else { "  " };
            Line::from(format!("{prefix}{line_text}"))
        })
        .collect();

    let selected = state
        .suggestion_selected
        .min(suggestions.len().saturating_sub(1));
    let (window_start, window_end, truncated) =
        visible_suggestion_window(suggestions.len(), selected);
    for (offset, (name, description)) in suggestions[window_start..window_end].iter().enumerate() {
        let index = window_start + offset;
        let text = format!("  {name} — {description}");
        lines.push(if index == selected {
            Line::from(Span::styled(
                text,
                Style::default()
                    .fg(SUGGESTION_HIGHLIGHT_COLOR)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(text)
        });
    }
    if truncated {
        let hidden = suggestions.len() - (window_end - window_start);
        lines.push(Line::from(format!("  … {hidden} more (↑/↓ to scroll)")));
    }
    if let Some(status) = &state.status {
        lines.push(Line::from(status.clone()));
    }
    let mode_text = format!(
        "{} mode on (shift+tab or F2 to cycle)",
        state.permission_mode.label()
    );
    let mode_span = Span::styled(
        mode_text.clone(),
        Style::default().fg(permission_mode_color(state.permission_mode)),
    );
    lines.push(if state.activity.is_some() {
        let interrupt_text = "ESC to interrupt";
        let content_width = usize::from(area.width.saturating_sub(2));
        let gap = content_width
            .saturating_sub(mode_text.chars().count())
            .saturating_sub(interrupt_text.chars().count());
        Line::from(vec![
            mode_span,
            Span::raw(" ".repeat(gap.max(1))),
            Span::styled(
                interrupt_text,
                Style::default()
                    .fg(SELECTION_COLOR)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    } else {
        Line::from(mode_span)
    });
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL)),
        area,
    );

    let editing = state.pending_permission.is_none()
        && state.pending_update.is_none()
        && state.blocking_error.is_none();
    if editing {
        let (cursor_line, cursor_col) = cursor_line_col(&input_display_lines, state.cursor);
        if cursor_line < input_line_count {
            let prefix_len = 2u16;
            let cursor_x = area.x + 1 + prefix_len + u16::try_from(cursor_col).unwrap_or(0);
            let cursor_y = area.y + 1 + u16::try_from(cursor_line).unwrap_or(0);
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn permission_mode_color(mode: PermissionMode) -> Color {
    match mode {
        PermissionMode::Auto => Color::Rgb(120, 200, 120),
        PermissionMode::Plan => Color::Rgb(120, 170, 255),
        PermissionMode::Approve => Color::Rgb(255, 170, 90),
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn render_permission_modal(frame: &mut Frame, prompt: &PermissionPrompt, area: Rect) {
    let modal = centered(area, 60, 7);
    let content = format!(
        "Permission needed\n\n{}\nin {}\n\n[y] Approve   [n] Deny",
        prompt.summary, prompt.working_directory
    );
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().title("Confirm").borders(Borders::ALL)),
        modal,
    );
}

fn render_blocking_error_modal(frame: &mut Frame, message: &str, area: Rect) {
    let modal = centered(area, 60, 7);
    let content = format!("{message}\n\nPress Enter or Esc to continue.");
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().title("Error").borders(Borders::ALL)),
        modal,
    );
}

/// Runs the application loop using the provided terminal and event source.
///
/// # Errors
///
/// Returns an I/O error when terminal drawing or event reading fails.
pub fn run_with_event_source<B, F>(
    terminal: &mut Terminal<B>,
    initial_event: &AppEvent,
    mut next_event: F,
) -> std::io::Result<InputAction>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
    F: FnMut() -> std::io::Result<Event>,
{
    let mut state = AppState::default();
    state.apply(initial_event);

    loop {
        terminal
            .draw(|frame| render(frame, &state))
            .map_err(std::io::Error::other)?;

        let event = next_event()?;
        match classify_event(&event) {
            InputAction::Continue => {}
            action @ InputAction::Interrupt => return Ok(action),
        }
    }
}

/// Initializes the terminal, runs the interface loop, and restores the terminal before returning.
///
/// # Errors
///
/// Returns an I/O error when terminal initialization, rendering, input, or restoration fails.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the sender must move into the blocking terminal task"
)]
pub async fn run(
    event_rx: mpsc::Receiver<AppEvent>,
    command_tx: mpsc::Sender<AppCommand>,
) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || run_terminal(event_rx, command_tx))
        .await
        .map_err(std::io::Error::other)?
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the sender is retained by the blocking event loop until terminal exit"
)]
#[allow(
    clippy::too_many_lines,
    reason = "the terminal loop deliberately keeps every input source visible in one place"
)]
fn run_terminal(
    mut event_rx: mpsc::Receiver<AppEvent>,
    command_tx: mpsc::Sender<AppCommand>,
) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture);
    let _terminal_guard = TerminalGuard;
    let mut state = AppState::default();
    let mut last_ctrl_c: Option<Instant> = None;
    let mut copy_notification_deadline: Option<Instant> = None;

    let send_command = |command_tx: &mpsc::Sender<AppCommand>, command: AppCommand| {
        command_tx.blocking_send(command).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                format!("runtime command channel closed: {error}"),
            )
        })
    };

    loop {
        if copy_notification_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            state.copy_notification = None;
            copy_notification_deadline = None;
        }

        terminal
            .draw(|frame| render(frame, &state))
            .map_err(std::io::Error::other)?;

        while let Ok(app_event) = event_rx.try_recv() {
            if let Some(prompt) = state.apply(&app_event) {
                state.begin_run(prompt.clone());
                send_command(&command_tx, AppCommand::SubmitChat(prompt))?;
            }
            if state.exit_for_update {
                send_command(&command_tx, AppCommand::Exit)?;
                return Ok(());
            }
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }

        let terminal_event = event::read()?;
        if let Event::Resize(columns, rows) = terminal_event {
            send_command(&command_tx, AppCommand::Resize { columns, rows })?;
        }

        if let Event::Mouse(mouse_event) = &terminal_event {
            let terminal_area: Rect = terminal.size().map_err(std::io::Error::other)?.into();
            let handled = handle_mouse_event(&mut state, mouse_event, terminal_area);
            if handled && matches!(mouse_event.kind, MouseEventKind::Up(MouseButton::Left)) {
                try_copy_selection(&mut state, terminal_area, &mut copy_notification_deadline);
            }
            if handled {
                continue;
            }
        }

        if let Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = &terminal_event
            && modifiers.contains(KeyModifiers::CONTROL)
        {
            if state.selection.is_some() {
                let terminal_area: Rect = terminal.size().map_err(std::io::Error::other)?.into();
                try_copy_selection(&mut state, terminal_area, &mut copy_notification_deadline);
                continue;
            }
            let now = Instant::now();
            let should_exit = last_ctrl_c
                .is_some_and(|previous| now.duration_since(previous) < DOUBLE_CTRL_C_WINDOW);
            if should_exit {
                send_command(&command_tx, AppCommand::Exit)?;
                return Ok(());
            }
            last_ctrl_c = Some(now);
            state.status = Some("Press Ctrl+C again to exit.".into());
            continue;
        }

        if let Some(approved) = handle_permission_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::PermissionResponse(approved))?;
            continue;
        }

        match handle_update_event(&mut state, &terminal_event) {
            UpdateEventOutcome::NotHandled => {}
            UpdateEventOutcome::Handled => continue,
            UpdateEventOutcome::Accepted => {
                send_command(&command_tx, AppCommand::AcceptUpdate)?;
                continue;
            }
            UpdateEventOutcome::Rejected => {
                send_command(&command_tx, AppCommand::RejectUpdate)?;
                continue;
            }
            UpdateEventOutcome::RestartRequested => {
                send_command(&command_tx, AppCommand::RestartForUpdate)?;
                continue;
            }
        }

        if let Some(credential) = handle_onboarding_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::SubmitCredential(credential))?;
            continue;
        }

        if let Some(model) = handle_model_picker_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::SelectModel(model))?;
            continue;
        }

        if handle_settings_event(&mut state, &terminal_event) {
            continue;
        }

        let was_effort_picker = state.screen == Screen::EffortPicker;
        if let Some(effort) = handle_effort_picker_event(&mut state, &terminal_event) {
            if let Some(model) = state.pending_model.take() {
                state.model_flow_pending_effort = false;
                send_command(&command_tx, AppCommand::SelectModel(model))?;
            }
            send_command(&command_tx, AppCommand::SetReasoningEffort(effort))?;
            continue;
        }
        if was_effort_picker
            && state.screen != Screen::EffortPicker
            && let Some(model) = state.pending_model.take()
        {
            state.model_flow_pending_effort = false;
            send_command(&command_tx, AppCommand::SelectModel(model))?;
            continue;
        }

        if let Some(session_id) = handle_session_picker_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::ResumeSession(session_id))?;
            continue;
        }

        if let Some(mode) = handle_permission_mode_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::SetPermissionMode(mode))?;
            continue;
        }

        if handle_help_event(&mut state, &terminal_event) {
            continue;
        }

        match handle_skills_event(&mut state, &terminal_event) {
            SkillsEventOutcome::NotHandled => {}
            SkillsEventOutcome::Handled => continue,
            SkillsEventOutcome::ToggleSkill { name, enabled } => {
                send_command(&command_tx, AppCommand::SetSkillEnabled { name, enabled })?;
                continue;
            }
        }

        match handle_mcp_event(&mut state, &terminal_event) {
            McpEventOutcome::NotHandled => {}
            McpEventOutcome::Handled => continue,
            McpEventOutcome::Connect(name) => {
                send_command(&command_tx, AppCommand::McpConnect(name))?;
                continue;
            }
            McpEventOutcome::Disconnect(name) => {
                send_command(&command_tx, AppCommand::McpDisconnect(name))?;
                continue;
            }
            McpEventOutcome::AddServer { entry, api_key } => {
                send_command(&command_tx, AppCommand::McpAddServer { entry, api_key })?;
                continue;
            }
            McpEventOutcome::Authorize(name) => {
                send_command(&command_tx, AppCommand::McpAuthorize(name))?;
                continue;
            }
        }

        if let Some(submission) = handle_chat_event(&mut state, &terminal_event) {
            match submission {
                ChatSubmission::Prompt(text) => {
                    state.begin_run(text.clone());
                    send_command(&command_tx, AppCommand::SubmitChat(text))?;
                }
                ChatSubmission::Command(SlashCommand::Exit) => {
                    send_command(&command_tx, AppCommand::Exit)?;
                    return Ok(());
                }
                ChatSubmission::Command(SlashCommand::Clear) => {
                    state.entries.clear();
                    state.scroll = 0;
                    send_command(&command_tx, AppCommand::ClearConversation)?;
                }
                ChatSubmission::Command(SlashCommand::Compact) => {
                    if state.activity.is_some() {
                        state.entries.push(ChatEntry::Warning(
                            "Wait for the current run to finish before compacting.".into(),
                        ));
                    } else {
                        send_command(&command_tx, AppCommand::CompactContext)?;
                    }
                }
                ChatSubmission::Command(SlashCommand::AutoCompact) => {
                    state.auto_compact_disabled = !state.auto_compact_disabled;
                    let status = if state.auto_compact_disabled {
                        "off"
                    } else {
                        "on"
                    };
                    state
                        .entries
                        .push(ChatEntry::Info(format!("Autocompact is now {status}.")));
                    send_command(
                        &command_tx,
                        AppCommand::SetAutoCompact(!state.auto_compact_disabled),
                    )?;
                }
                ChatSubmission::Command(SlashCommand::NewSession) => {
                    send_command(&command_tx, AppCommand::NewSession)?;
                }
                ChatSubmission::Command(SlashCommand::ResumeSession) => {
                    state.screen = Screen::SessionPicker;
                    state.selected_session = 0;
                    send_command(&command_tx, AppCommand::RequestSessionList)?;
                }
                ChatSubmission::Command(SlashCommand::Model) => {
                    state.screen = Screen::ModelPicker;
                    state.selected_model = 0;
                    state.model_flow_pending_effort = true;
                }
                ChatSubmission::Command(SlashCommand::Settings) => {
                    state.screen = Screen::Settings;
                    state.settings_selected = 0;
                }
                ChatSubmission::Command(SlashCommand::Provider) => {
                    state
                        .entries
                        .push(ChatEntry::Info("Provider: NVIDIA NIM (hosted).".into()));
                }
                ChatSubmission::Command(SlashCommand::Status) => {
                    let model = state
                        .current_model
                        .clone()
                        .unwrap_or_else(|| "none selected".into());
                    let effort = effort_label(state.current_effort.as_deref());
                    let directory = if state.working_directory.is_empty() {
                        "unknown".into()
                    } else {
                        state.working_directory.clone()
                    };
                    let session = if state.current_session_id.is_empty() {
                        "none".into()
                    } else {
                        format!(
                            "{} ({})",
                            state.current_session_name,
                            &state.current_session_id[..state.current_session_id.len().min(8)]
                        )
                    };
                    let context = state.last_input_tokens.map_or_else(
                        || "not yet measured".into(),
                        |tokens| {
                            let percent = tokens.saturating_mul(100)
                                / gocode_core::AUTO_COMPACT_TOKEN_THRESHOLD;
                            format!("~{percent}% ({tokens} tok, approx.)")
                        },
                    );
                    state.entries.push(ChatEntry::Info(format!(
                        "Provider: NVIDIA NIM · Model: {model} · Reasoning effort: {effort}\n\
                         Directory: {directory}\n\
                         Session: {session}\n\
                         Context: {context}"
                    )));
                }
                ChatSubmission::Command(SlashCommand::Help) => {
                    state.help_visible = true;
                    state.help_tab = HelpTab::General;
                    state.help_scroll = 0;
                }
                ChatSubmission::Command(SlashCommand::Skills) => {
                    state.skills_visible = true;
                    state.skills_view = SkillsView::Menu;
                    state.skills_selected = 0;
                }
                ChatSubmission::Command(SlashCommand::Mcp) => {
                    state.mcp_visible = true;
                    state.mcp_view = McpView::Menu;
                    state.mcp_selected = 0;
                }
                ChatSubmission::Command(SlashCommand::PlanMode) => {
                    state.permission_mode = PermissionMode::Plan;
                    send_command(
                        &command_tx,
                        AppCommand::SetPermissionMode(PermissionMode::Plan),
                    )?;
                }
                ChatSubmission::Command(SlashCommand::Review(target)) => {
                    let scope = if target.is_empty() {
                        "the current diff (uncommitted changes in the working tree; if there \
                         are none, review the changes in the most recent commit instead)"
                            .to_string()
                    } else {
                        format!(
                            "the changes on/in \"{target}\" (this may be a branch name, commit \
                             range, pull request number, or file/directory path — figure out \
                             which from context and use the appropriate git or gh command to \
                             get its diff)"
                        )
                    };
                    let prompt = format!(
                        "Review {scope}. Use git (and gh, if it's a pull request) to gather the \
                         diff yourself. Focus on correctness bugs, security issues, and \
                         significant simplification or efficiency opportunities in the changed \
                         code — do not comment on unrelated pre-existing code. For each finding, \
                         give the file and line, a one-sentence description of the defect, and \
                         a concrete suggested fix. If nothing significant is found, say so \
                         plainly instead of inventing minor nitpicks."
                    );
                    state.begin_run(prompt.clone());
                    send_command(&command_tx, AppCommand::SubmitChat(prompt))?;
                }
                ChatSubmission::Command(SlashCommand::Init) => {
                    let prompt = "Explore this repository (its structure, languages, build \
                                   system, tests, and conventions) and write a complete \
                                   AGENTS.md file at the project root. Cover: what the project \
                                   is and does, its directory structure, how to build/run/test/ \
                                   lint it, and coding conventions an AI coding agent should \
                                   follow when working in this codebase. Use the file tools to \
                                   create the file."
                        .to_string();
                    state.begin_run(prompt.clone());
                    send_command(&command_tx, AppCommand::SubmitChat(prompt))?;
                }
            }
            continue;
        }

        if state.screen == Screen::Chat
            && state.blocking_error.is_none()
            && state.pending_permission.is_none()
            && matches!(
                terminal_event,
                Event::Key(KeyEvent {
                    code: KeyCode::Esc,
                    kind: KeyEventKind::Press,
                    ..
                })
            )
        {
            send_command(&command_tx, AppCommand::CancelProviderRequest)?;
            continue;
        }

        match classify_event(&terminal_event) {
            InputAction::Continue => {}
            InputAction::Interrupt => {
                send_command(&command_tx, AppCommand::Exit)?;
                return Ok(());
            }
        }
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
        ratatui::restore();
    }
}

/// Installs a panic hook that restores the terminal before Rust prints the panic report.
///
/// Call this once during application bootstrap, before entering terminal mode.
pub fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = crossterm::execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
        ratatui::restore();
        previous_hook(panic_info);
    }));
}

/// Converts raw terminal input to one explicit application-level action.
#[must_use]
pub fn classify_event(event: &Event) -> InputAction {
    if matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            ..
        })
    ) {
        return InputAction::Interrupt;
    }

    InputAction::Continue
}

/// Applies one terminal event to the credential onboarding input.
///
/// Returns the credential only when the user explicitly submits it with Enter.
#[must_use]
pub fn handle_onboarding_event(state: &mut AppState, event: &Event) -> Option<String> {
    if state.screen != Screen::Onboarding {
        return None;
    }

    if let Event::Paste(text) = event {
        state.credential_input.push_str(text);
        return None;
    }

    let Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return None;
    };

    match code {
        KeyCode::Char(character)
            if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            state.push_credential_character(*character);
        }
        KeyCode::Backspace => {
            state.credential_input.pop();
        }
        KeyCode::Enter => return state.take_credential_submission(),
        KeyCode::Esc if state.current_model.is_some() => {
            state.credential_input.clear();
            state.status = None;
            state.screen = Screen::Chat;
        }
        _ => {}
    }
    None
}

/// Applies navigation and confirmation keys to the discovered-model picker.
#[must_use]
pub fn handle_model_picker_event(state: &mut AppState, event: &Event) -> Option<String> {
    if state.screen != Screen::ModelPicker {
        return None;
    }
    let Event::Key(KeyEvent {
        code,
        modifiers: _,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return None;
    };

    match code {
        KeyCode::Up => state.selected_model = state.selected_model.saturating_sub(1),
        KeyCode::Down if !state.models.is_empty() => {
            state.selected_model = (state.selected_model + 1).min(state.models.len() - 1);
        }
        KeyCode::Enter => {
            let model = state.selected_model()?;
            if state.model_flow_pending_effort {
                state.pending_model = Some(model);
                state.screen = Screen::EffortPicker;
                state.selected_effort = EFFORT_OPTIONS
                    .iter()
                    .position(|(_, value)| *value == state.current_effort.as_deref())
                    .unwrap_or(0);
                return None;
            }
            return Some(model);
        }
        KeyCode::Esc if state.current_model.is_some() => {
            state.model_flow_pending_effort = false;
            state.screen = Screen::Chat;
        }
        _ => {}
    }
    None
}

/// Applies navigation and selection keys to the settings menu.
///
/// Returns `true` when the event was handled (whether or not it changed anything), so the
/// caller can skip further dispatch.
pub fn handle_settings_event(state: &mut AppState, event: &Event) -> bool {
    if state.screen != Screen::Settings {
        return false;
    }
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return false;
    };

    match code {
        KeyCode::Up => state.settings_selected = state.settings_selected.saturating_sub(1),
        KeyCode::Down => {
            state.settings_selected = (state.settings_selected + 1).min(SETTINGS_ITEMS.len() - 1);
        }
        KeyCode::Enter => match state.settings_selected {
            0 => {
                state.screen = Screen::Onboarding;
                state.credential_input.clear();
                state.status = None;
            }
            1 => {
                state.screen = Screen::ModelPicker;
                state.model_flow_pending_effort = false;
                state.selected_model = state
                    .models
                    .iter()
                    .position(|model| Some(model) == state.current_model.as_ref())
                    .unwrap_or(0);
            }
            _ => {
                state.screen = Screen::EffortPicker;
                state.selected_effort = EFFORT_OPTIONS
                    .iter()
                    .position(|(_, value)| *value == state.current_effort.as_deref())
                    .unwrap_or(0);
            }
        },
        KeyCode::Esc => state.screen = Screen::Chat,
        _ => {}
    }
    true
}

/// Applies navigation and confirmation keys to the reasoning-effort picker.
///
/// Returns the selected provider value on confirmation (`None` clears the effort level).
#[must_use]
pub fn handle_effort_picker_event(state: &mut AppState, event: &Event) -> Option<Option<String>> {
    if state.screen != Screen::EffortPicker {
        return None;
    }
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return None;
    };

    match code {
        KeyCode::Up => state.selected_effort = state.selected_effort.saturating_sub(1),
        KeyCode::Down => {
            state.selected_effort = (state.selected_effort + 1).min(EFFORT_OPTIONS.len() - 1);
        }
        KeyCode::Enter => {
            let (_, value) = EFFORT_OPTIONS[state.selected_effort];
            return Some(value.map(str::to_string));
        }
        KeyCode::Esc => state.screen = Screen::Chat,
        _ => {}
    }
    None
}

/// Applies navigation and confirmation keys to the saved-session picker.
///
/// Returns the chosen session's id on confirmation.
#[must_use]
pub fn handle_session_picker_event(state: &mut AppState, event: &Event) -> Option<String> {
    if state.screen != Screen::SessionPicker {
        return None;
    }
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return None;
    };

    match code {
        KeyCode::Up => state.selected_session = state.selected_session.saturating_sub(1),
        KeyCode::Down if !state.sessions.is_empty() => {
            state.selected_session = (state.selected_session + 1).min(state.sessions.len() - 1);
        }
        KeyCode::Enter => {
            return state
                .sessions
                .get(state.selected_session)
                .map(|session| session.id.clone());
        }
        KeyCode::Esc if state.current_model.is_some() => state.screen = Screen::Chat,
        _ => {}
    }
    None
}

/// Cycles the permission mode (Auto → Plan → Approve → Auto) on Shift+Tab from the chat
/// composer, returning the newly selected mode.
///
/// Handled ahead of [`handle_chat_event`] so a Shift+Tab keypress never falls through to plain
/// Tab's autocomplete behavior.
#[must_use]
pub fn handle_permission_mode_event(state: &mut AppState, event: &Event) -> Option<PermissionMode> {
    if state.screen != Screen::Chat
        || state.blocking_error.is_some()
        || state.pending_permission.is_some()
        || state.pending_update.is_some()
    {
        return None;
    }
    let Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return None;
    };

    // F2 is a fallback: some terminals/IDE panels intercept Shift+Tab for their own tab
    // navigation before it ever reaches this app.
    let is_cycle_key = matches!(code, KeyCode::BackTab | KeyCode::F(2))
        || (*code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT));
    if !is_cycle_key {
        return None;
    }

    state.permission_mode = state.permission_mode.cycle();
    Some(state.permission_mode)
}

/// Maps absolute terminal coordinates to a point in the wrapped-history coordinate space, or
/// `None` when the coordinates fall outside the history viewport (its border included).
fn point_from_terminal_coords(
    state: &AppState,
    terminal_area: Rect,
    column: u16,
    row: u16,
) -> Option<SelectionPoint> {
    let (history_area, _) = chat_layout(terminal_area, state);
    if column <= history_area.x
        || column >= history_area.x + history_area.width.saturating_sub(1)
        || row <= history_area.y
        || row >= history_area.y + history_area.height.saturating_sub(1)
    {
        return None;
    }

    let local_col = usize::from(column - history_area.x - 1);
    let local_row = usize::from(row - history_area.y - 1);

    let content_width = usize::from(history_area.width.saturating_sub(2));
    let wrapped = wrap_lines(&compose_lines(state), content_width);
    let visible_rows = usize::from(history_area.height.saturating_sub(2)).max(1);
    let (start, _end) = compute_visible_window(wrapped.len(), visible_rows, state.scroll);

    let absolute_line = start + local_row;
    let line_len = wrapped.get(absolute_line)?.chars().count();
    Some(SelectionPoint {
        line: absolute_line,
        col: local_col.min(line_len),
    })
}

/// Maps a click inside the composer's input rows to a char-index cursor position, or `None`
/// when the click landed elsewhere in the composer (suggestions, status, or mode line).
fn composer_input_click_position(
    state: &AppState,
    composer_area: Rect,
    column: u16,
    row: u16,
) -> Option<usize> {
    if column <= composer_area.x
        || column >= composer_area.x + composer_area.width.saturating_sub(1)
        || row <= composer_area.y
        || row >= composer_area.y + composer_area.height.saturating_sub(1)
    {
        return None;
    }

    let local_row = usize::from(row - composer_area.y - 1);
    let input_lines: Vec<&str> = state.chat_input.split('\n').collect();
    if local_row >= input_lines.len() {
        return None;
    }

    let prefix_len = 2usize;
    let local_col = usize::from(column - composer_area.x - 1);
    let col_in_line = local_col
        .saturating_sub(prefix_len)
        .min(input_lines[local_row].chars().count());
    Some(char_index_from_line_col(
        &input_lines,
        local_row,
        col_in_line,
    ))
}

/// Applies a mouse event to the transcript's text selection.
///
/// Returns `true` when the event was consumed by selection handling — a left-button press,
/// drag, or release inside the chat screen.
pub fn handle_mouse_event(state: &mut AppState, event: &MouseEvent, terminal_area: Rect) -> bool {
    if state.screen != Screen::Chat {
        return false;
    }
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let (_, composer_area) = chat_layout(terminal_area, state);
            if let Some(cursor) =
                composer_input_click_position(state, composer_area, event.column, event.row)
            {
                state.cursor = cursor;
                state.selection = None;
            } else {
                state.selection =
                    point_from_terminal_coords(state, terminal_area, event.column, event.row).map(
                        |point| Selection {
                            anchor: point,
                            cursor: point,
                        },
                    );
            }
            true
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(point) =
                point_from_terminal_coords(state, terminal_area, event.column, event.row)
                && let Some(selection) = state.selection.as_mut()
            {
                selection.cursor = point;
            }
            true
        }
        MouseEventKind::Up(MouseButton::Left) => true,
        MouseEventKind::Down(MouseButton::Right) => {
            let (_, composer_area) = chat_layout(terminal_area, state);
            if is_within(composer_area, event.column, event.row)
                && let Ok(mut clipboard) = arboard::Clipboard::new()
                && let Ok(text) = clipboard.get_text()
            {
                state.chat_input.push_str(&text);
                state.suggestion_selected = 0;
            }
            true
        }
        MouseEventKind::ScrollUp => {
            state.scroll_up();
            true
        }
        MouseEventKind::ScrollDown => {
            state.scroll_down();
            true
        }
        _ => false,
    }
}

fn is_within(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.x + area.width && row >= area.y && row < area.y + area.height
}

/// Extracts the currently selected transcript text, joined with newlines, or `None` when there
/// is no selection or it is empty.
fn extract_selected_text(state: &AppState, terminal_area: Rect) -> Option<String> {
    let selection = state.selection?;
    let (history_area, _) = chat_layout(terminal_area, state);
    let content_width = usize::from(history_area.width.saturating_sub(2));
    let wrapped = wrap_lines(&compose_lines(state), content_width);
    let (start, end) = selection.normalized();

    let mut collected = Vec::new();
    for (line_index, line) in wrapped
        .iter()
        .enumerate()
        .take(end.line + 1)
        .skip(start.line)
    {
        let chars: Vec<char> = line.chars().collect();
        if let Some((from, to)) = selected_char_range(&selection, line_index, chars.len()) {
            collected.push(chars[from..to].iter().collect::<String>());
        }
    }
    let text = collected.join("\n");
    (!text.is_empty()).then_some(text)
}

/// Copies the active selection to the system clipboard and shows a transient confirmation.
///
/// Silently does nothing when there is no selection, or the clipboard cannot be reached (a
/// missing clipboard service should not crash the interface).
fn try_copy_selection(
    state: &mut AppState,
    terminal_area: Rect,
    notification_deadline: &mut Option<Instant>,
) {
    let Some(text) = extract_selected_text(state, terminal_area) else {
        return;
    };
    let char_count = text.chars().count();
    let copied = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text));
    if copied.is_ok() {
        state.copy_notification = Some(format!("Copied {char_count} chars to clipboard"));
        *notification_deadline = Some(Instant::now() + COPY_NOTIFICATION_DURATION);
    }
}

/// Applies Y/N confirmation keys to a pending permission prompt.
///
/// Returns `Some(true)` on approval, `Some(false)` on denial, clearing the prompt either way.
#[must_use]
pub fn handle_permission_event(state: &mut AppState, event: &Event) -> Option<bool> {
    state.pending_permission.as_ref()?;
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return None;
    };

    match code {
        KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
            state.pending_permission = None;
            Some(true)
        }
        KeyCode::Char('n' | 'N') | KeyCode::Esc => {
            state.pending_permission = None;
            Some(false)
        }
        _ => None,
    }
}

/// Outcome of dispatching a terminal event to the update popup.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateEventOutcome {
    /// The popup is not shown, or the event isn't one it cares about.
    NotHandled,
    /// The event was consumed by the popup with no further side effect.
    Handled,
    /// The user chose to install the update; download and staging should begin.
    Accepted,
    /// The user declined; nothing more to do this run.
    Rejected,
    /// The user confirmed the "Completed" screen; the update should now be installed and the
    /// application restarted.
    RestartRequested,
}

/// Drives the update popup: the Yes/No prompt, the (non-interactive) download progress screen,
/// and the Completed/Failed screens' Close button.
pub fn handle_update_event(state: &mut AppState, event: &Event) -> UpdateEventOutcome {
    let Some(prompt) = state.pending_update.as_ref() else {
        return UpdateEventOutcome::NotHandled;
    };
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return UpdateEventOutcome::NotHandled;
    };

    match &prompt.stage {
        UpdateStage::Prompt => match code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                state.update_selected_no = !state.update_selected_no;
                UpdateEventOutcome::Handled
            }
            KeyCode::Char('y' | 'Y') => {
                state.begin_update_download();
                UpdateEventOutcome::Accepted
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                state.pending_update = None;
                UpdateEventOutcome::Rejected
            }
            KeyCode::Enter => {
                if state.update_selected_no {
                    state.pending_update = None;
                    UpdateEventOutcome::Rejected
                } else {
                    state.begin_update_download();
                    UpdateEventOutcome::Accepted
                }
            }
            _ => UpdateEventOutcome::Handled,
        },
        UpdateStage::Downloading { .. } => UpdateEventOutcome::Handled,
        UpdateStage::Completed { .. } => match code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') => {
                UpdateEventOutcome::RestartRequested
            }
            _ => UpdateEventOutcome::Handled,
        },
        UpdateStage::Failed(_) => match code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') => {
                state.pending_update = None;
                UpdateEventOutcome::Handled
            }
            _ => UpdateEventOutcome::Handled,
        },
    }
}

/// Switches tabs on Tab/Shift+Tab/Left/Right, scrolls the current tab's content on Up/Down and
/// PageUp/PageDown, and dismisses the `/help` popup on Esc.
///
/// Returns `true` when the event was handled, so the caller can skip further dispatch.
pub fn handle_help_event(state: &mut AppState, event: &Event) -> bool {
    if !state.help_visible {
        return false;
    }
    let Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return false;
    };
    match code {
        KeyCode::Esc | KeyCode::Enter => {
            state.help_visible = false;
        }
        KeyCode::BackTab | KeyCode::Left => {
            state.help_tab = state.help_tab.previous();
            state.help_scroll = 0;
        }
        KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => {
            state.help_tab = state.help_tab.previous();
            state.help_scroll = 0;
        }
        KeyCode::Tab | KeyCode::Right => {
            state.help_tab = state.help_tab.next();
            state.help_scroll = 0;
        }
        KeyCode::Up => state.help_scroll = state.help_scroll.saturating_sub(1),
        KeyCode::Down => state.help_scroll = state.help_scroll.saturating_add(1),
        KeyCode::PageUp => state.help_scroll = state.help_scroll.saturating_sub(5),
        KeyCode::PageDown => state.help_scroll = state.help_scroll.saturating_add(5),
        _ => {}
    }
    true
}

/// Outcome of dispatching a terminal event to the `/skills` popup.
#[derive(Debug)]
pub enum SkillsEventOutcome {
    /// The popup is not shown, or the event isn't one it cares about.
    NotHandled,
    /// The event was consumed by the popup with no further side effect.
    Handled,
    /// A skill's enabled state was toggled and should be persisted.
    ToggleSkill { name: String, enabled: bool },
}

/// Drives the `/skills` popup: its menu, the read-only skill list, and the enable/disable
/// screen.
///
/// Returns [`SkillsEventOutcome::NotHandled`] when the popup isn't shown or the event doesn't
/// apply to it, so the caller can fall through to other dispatch.
pub fn handle_skills_event(state: &mut AppState, event: &Event) -> SkillsEventOutcome {
    if !state.skills_visible {
        return SkillsEventOutcome::NotHandled;
    }
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return SkillsEventOutcome::NotHandled;
    };

    match state.skills_view {
        SkillsView::Menu => match code {
            KeyCode::Up | KeyCode::Down => {
                state.skills_selected = 1 - state.skills_selected.min(1);
                SkillsEventOutcome::Handled
            }
            KeyCode::Enter => {
                state.skills_view = if state.skills_selected == 0 {
                    SkillsView::List
                } else {
                    SkillsView::EnableDisable
                };
                state.skills_selected = 0;
                state.skills_list_scroll = 0;
                SkillsEventOutcome::Handled
            }
            KeyCode::Esc => {
                state.skills_visible = false;
                SkillsEventOutcome::Handled
            }
            _ => SkillsEventOutcome::Handled,
        },
        SkillsView::List => match code {
            KeyCode::Esc | KeyCode::Enter => {
                state.skills_view = SkillsView::Menu;
                state.skills_selected = 0;
                state.skills_list_scroll = 0;
                SkillsEventOutcome::Handled
            }
            KeyCode::Up => {
                state.skills_list_scroll = state.skills_list_scroll.saturating_sub(1);
                SkillsEventOutcome::Handled
            }
            KeyCode::Down => {
                state.skills_list_scroll = state.skills_list_scroll.saturating_add(1);
                SkillsEventOutcome::Handled
            }
            KeyCode::PageUp => {
                state.skills_list_scroll = state.skills_list_scroll.saturating_sub(5);
                SkillsEventOutcome::Handled
            }
            KeyCode::PageDown => {
                state.skills_list_scroll = state.skills_list_scroll.saturating_add(5);
                SkillsEventOutcome::Handled
            }
            _ => SkillsEventOutcome::Handled,
        },
        SkillsView::EnableDisable => match code {
            KeyCode::Up => {
                state.skills_selected = state.skills_selected.saturating_sub(1);
                SkillsEventOutcome::Handled
            }
            KeyCode::Down => {
                if !state.skills.is_empty() {
                    state.skills_selected = (state.skills_selected + 1).min(state.skills.len() - 1);
                }
                SkillsEventOutcome::Handled
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let Some(skill) = state.skills.get_mut(state.skills_selected) else {
                    return SkillsEventOutcome::Handled;
                };
                skill.enabled = !skill.enabled;
                SkillsEventOutcome::ToggleSkill {
                    name: skill.name.clone(),
                    enabled: skill.enabled,
                }
            }
            KeyCode::Esc => {
                state.skills_view = SkillsView::Menu;
                state.skills_selected = 0;
                SkillsEventOutcome::Handled
            }
            _ => SkillsEventOutcome::Handled,
        },
    }
}

/// Outcome of dispatching a terminal event to the `/mcp` popup.
#[derive(Debug)]
pub enum McpEventOutcome {
    /// The popup is not shown, or the event isn't one it cares about.
    NotHandled,
    /// The event was consumed by the popup with no further side effect.
    Handled,
    /// Connect the named configured server.
    Connect(String),
    /// Disconnect the named connected server.
    Disconnect(String),
    /// Persist and connect a newly configured server; `api_key` is set only when the wizard's
    /// auth choice was "API key".
    AddServer {
        entry: gocode_core::McpServerEntry,
        api_key: Option<String>,
    },
    /// Start the OAuth authorization flow for the named server.
    Authorize(String),
}

/// Resets every `/mcp` "Add server" wizard draft field back to its default.
fn reset_mcp_add_form(state: &mut AppState) {
    state.mcp_add_step = McpAddStep::Name;
    state.mcp_add_name.clear();
    state.mcp_add_http = false;
    state.mcp_add_command_line.clear();
    state.mcp_add_url.clear();
    state.mcp_add_api_key_auth = false;
    state.mcp_add_api_key.clear();
}

/// Builds the [`gocode_core::McpServerEntry`] (and, if chosen, the plaintext API key) from a
/// completed wizard draft.
fn build_mcp_add_server_outcome(state: &AppState) -> McpEventOutcome {
    let transport = if state.mcp_add_http {
        gocode_core::McpTransportConfig::Http {
            url: state.mcp_add_url.trim().to_string(),
            headers: std::collections::BTreeMap::new(),
        }
    } else {
        let mut parts = state.mcp_add_command_line.split_whitespace();
        let command = parts.next().unwrap_or_default().to_string();
        let args = parts.map(str::to_string).collect();
        gocode_core::McpTransportConfig::Stdio {
            command,
            args,
            env: std::collections::BTreeMap::new(),
        }
    };
    let auth = if state.mcp_add_api_key_auth {
        gocode_core::McpAuthConfig::ApiKey
    } else {
        gocode_core::McpAuthConfig::None
    };
    let entry = gocode_core::McpServerEntry {
        name: state.mcp_add_name.trim().to_string(),
        transport,
        auth,
        enabled: true,
    };
    let api_key = state
        .mcp_add_api_key_auth
        .then(|| state.mcp_add_api_key.clone());
    McpEventOutcome::AddServer { entry, api_key }
}

/// Drives the `/mcp` popup: its menu, the server list, one server's detail view, and the "Add
/// server" wizard.
///
/// Returns [`McpEventOutcome::NotHandled`] when the popup isn't shown or the event doesn't
/// belong to it, so callers can fall through to other handlers.
#[allow(
    clippy::too_many_lines,
    reason = "the wizard's per-step key dispatch is clearer as one flat match than split further"
)]
pub fn handle_mcp_event(state: &mut AppState, event: &Event) -> McpEventOutcome {
    if !state.mcp_visible {
        return McpEventOutcome::NotHandled;
    }
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return McpEventOutcome::NotHandled;
    };

    match state.mcp_view {
        McpView::Menu => match code {
            KeyCode::Up | KeyCode::Down => {
                state.mcp_selected = 1 - state.mcp_selected.min(1);
                McpEventOutcome::Handled
            }
            KeyCode::Enter if state.mcp_selected == 0 => {
                state.mcp_view = McpView::ServerList;
                state.mcp_selected = 0;
                McpEventOutcome::Handled
            }
            KeyCode::Enter => {
                reset_mcp_add_form(state);
                state.mcp_view = McpView::AddServer;
                McpEventOutcome::Handled
            }
            KeyCode::Esc => {
                state.mcp_visible = false;
                McpEventOutcome::Handled
            }
            _ => McpEventOutcome::Handled,
        },
        McpView::ServerList => match code {
            KeyCode::Up => {
                state.mcp_selected = state.mcp_selected.saturating_sub(1);
                McpEventOutcome::Handled
            }
            KeyCode::Down => {
                if !state.mcp_servers.is_empty() {
                    state.mcp_selected = (state.mcp_selected + 1).min(state.mcp_servers.len() - 1);
                }
                McpEventOutcome::Handled
            }
            KeyCode::Right => {
                if state.mcp_servers.get(state.mcp_selected).is_some() {
                    state.mcp_view = McpView::ServerDetail;
                    state.mcp_detail_scroll = 0;
                }
                McpEventOutcome::Handled
            }
            KeyCode::Enter => {
                let Some(server) = state.mcp_servers.get(state.mcp_selected) else {
                    return McpEventOutcome::Handled;
                };
                if server.connected {
                    McpEventOutcome::Disconnect(server.name.clone())
                } else {
                    McpEventOutcome::Connect(server.name.clone())
                }
            }
            KeyCode::Char('o') => {
                let Some(server) = state.mcp_servers.get(state.mcp_selected) else {
                    return McpEventOutcome::Handled;
                };
                if server.needs_authorization {
                    McpEventOutcome::Authorize(server.name.clone())
                } else {
                    McpEventOutcome::Handled
                }
            }
            KeyCode::Esc => {
                state.mcp_view = McpView::Menu;
                state.mcp_selected = 0;
                McpEventOutcome::Handled
            }
            _ => McpEventOutcome::Handled,
        },
        McpView::ServerDetail => match code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Left => {
                state.mcp_view = McpView::ServerList;
                state.mcp_detail_scroll = 0;
                McpEventOutcome::Handled
            }
            KeyCode::Up => {
                state.mcp_detail_scroll = state.mcp_detail_scroll.saturating_sub(1);
                McpEventOutcome::Handled
            }
            KeyCode::Down => {
                state.mcp_detail_scroll = state.mcp_detail_scroll.saturating_add(1);
                McpEventOutcome::Handled
            }
            KeyCode::PageUp => {
                state.mcp_detail_scroll = state.mcp_detail_scroll.saturating_sub(5);
                McpEventOutcome::Handled
            }
            KeyCode::PageDown => {
                state.mcp_detail_scroll = state.mcp_detail_scroll.saturating_add(5);
                McpEventOutcome::Handled
            }
            _ => McpEventOutcome::Handled,
        },
        McpView::AddServer => match state.mcp_add_step {
            McpAddStep::Name => match code {
                KeyCode::Enter if !state.mcp_add_name.trim().is_empty() => {
                    state.mcp_add_step = McpAddStep::TransportChoice;
                    McpEventOutcome::Handled
                }
                KeyCode::Backspace => {
                    state.mcp_add_name.pop();
                    McpEventOutcome::Handled
                }
                KeyCode::Char(character) => {
                    state.mcp_add_name.push(*character);
                    McpEventOutcome::Handled
                }
                KeyCode::Esc => {
                    state.mcp_view = McpView::Menu;
                    McpEventOutcome::Handled
                }
                _ => McpEventOutcome::Handled,
            },
            McpAddStep::TransportChoice => match code {
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                    state.mcp_add_http = !state.mcp_add_http;
                    McpEventOutcome::Handled
                }
                KeyCode::Enter => {
                    state.mcp_add_step = if state.mcp_add_http {
                        McpAddStep::Url
                    } else {
                        McpAddStep::CommandLine
                    };
                    McpEventOutcome::Handled
                }
                KeyCode::Esc => {
                    state.mcp_add_step = McpAddStep::Name;
                    McpEventOutcome::Handled
                }
                _ => McpEventOutcome::Handled,
            },
            McpAddStep::CommandLine => match code {
                KeyCode::Enter if !state.mcp_add_command_line.trim().is_empty() => {
                    state.mcp_add_step = McpAddStep::AuthChoice;
                    McpEventOutcome::Handled
                }
                KeyCode::Backspace => {
                    state.mcp_add_command_line.pop();
                    McpEventOutcome::Handled
                }
                KeyCode::Char(character) => {
                    state.mcp_add_command_line.push(*character);
                    McpEventOutcome::Handled
                }
                KeyCode::Esc => {
                    state.mcp_add_step = McpAddStep::TransportChoice;
                    McpEventOutcome::Handled
                }
                _ => McpEventOutcome::Handled,
            },
            McpAddStep::Url => match code {
                KeyCode::Enter if !state.mcp_add_url.trim().is_empty() => {
                    state.mcp_add_step = McpAddStep::AuthChoice;
                    McpEventOutcome::Handled
                }
                KeyCode::Backspace => {
                    state.mcp_add_url.pop();
                    McpEventOutcome::Handled
                }
                KeyCode::Char(character) => {
                    state.mcp_add_url.push(*character);
                    McpEventOutcome::Handled
                }
                KeyCode::Esc => {
                    state.mcp_add_step = McpAddStep::TransportChoice;
                    McpEventOutcome::Handled
                }
                _ => McpEventOutcome::Handled,
            },
            McpAddStep::AuthChoice => match code {
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                    state.mcp_add_api_key_auth = !state.mcp_add_api_key_auth;
                    McpEventOutcome::Handled
                }
                KeyCode::Enter if state.mcp_add_api_key_auth => {
                    state.mcp_add_step = McpAddStep::ApiKey;
                    McpEventOutcome::Handled
                }
                KeyCode::Enter => {
                    let outcome = build_mcp_add_server_outcome(state);
                    state.mcp_view = McpView::Menu;
                    state.mcp_selected = 0;
                    outcome
                }
                KeyCode::Esc => {
                    state.mcp_add_step = if state.mcp_add_http {
                        McpAddStep::Url
                    } else {
                        McpAddStep::CommandLine
                    };
                    McpEventOutcome::Handled
                }
                _ => McpEventOutcome::Handled,
            },
            McpAddStep::ApiKey => match code {
                KeyCode::Enter if !state.mcp_add_api_key.is_empty() => {
                    let outcome = build_mcp_add_server_outcome(state);
                    state.mcp_view = McpView::Menu;
                    state.mcp_selected = 0;
                    outcome
                }
                KeyCode::Backspace => {
                    state.mcp_add_api_key.pop();
                    McpEventOutcome::Handled
                }
                KeyCode::Char(character) => {
                    state.mcp_add_api_key.push(*character);
                    McpEventOutcome::Handled
                }
                KeyCode::Esc => {
                    state.mcp_add_step = McpAddStep::AuthChoice;
                    McpEventOutcome::Handled
                }
                _ => McpEventOutcome::Handled,
            },
        },
    }
}

/// Applies text input, navigation, and control keys to the chat composer.
///
/// Returns a submission on Enter: a prompt for the model, or a recognized slash command.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one flat key-dispatch match is clearer here than splitting it across helpers"
)]
pub fn handle_chat_event(state: &mut AppState, event: &Event) -> Option<ChatSubmission> {
    if state.screen != Screen::Chat
        || state.blocking_error.is_some()
        || state.pending_permission.is_some()
        || state.pending_update.is_some()
    {
        return None;
    }

    if let Event::Paste(text) = event {
        state.insert_at_cursor(text);
        return None;
    }

    let Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return None;
    };

    let suggestion_count = slash_suggestions(&state.chat_input, &state.custom_commands).len();

    match code {
        KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.toggle_last_tool_output();
        }
        KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(prompt) = state.last_failed_prompt.take() {
                state.set_chat_input(prompt);
            }
        }
        KeyCode::Up if suggestion_count > 0 => {
            state.suggestion_selected = state.suggestion_selected.saturating_sub(1);
        }
        KeyCode::Down if suggestion_count > 0 => {
            state.suggestion_selected = (state.suggestion_selected + 1).min(suggestion_count - 1);
        }
        KeyCode::Up => {
            let lines: Vec<&str> = state.chat_input.split('\n').collect();
            let (cursor_line, _) = cursor_line_col(&lines, state.cursor);
            if cursor_line == 0 {
                state.recall_previous_prompt();
            } else {
                state.move_cursor_vertical(-1);
            }
        }
        KeyCode::Down => {
            let lines: Vec<&str> = state.chat_input.split('\n').collect();
            let (cursor_line, _) = cursor_line_col(&lines, state.cursor);
            if cursor_line == lines.len() - 1 {
                state.recall_next_prompt();
            } else {
                state.move_cursor_vertical(1);
            }
        }
        KeyCode::Left => state.move_cursor_left(),
        KeyCode::Right => state.move_cursor_right(),
        KeyCode::PageUp => state.scroll_up(),
        KeyCode::PageDown | KeyCode::End => state.scroll_down(),
        KeyCode::Tab => state.autocomplete(),
        KeyCode::Enter if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) => {
            state.insert_at_cursor("\n");
        }
        KeyCode::Char('j' | 'J') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.insert_at_cursor("\n");
        }
        KeyCode::Char(character)
            if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            let mut buffer = [0u8; 4];
            state.insert_at_cursor(character.encode_utf8(&mut buffer));
        }
        KeyCode::Backspace => state.backspace_at_cursor(),
        KeyCode::Enter => {
            let trimmed = state.chat_input.trim();
            if trimmed.is_empty() {
                return None;
            }
            let suggestions = slash_suggestions(&state.chat_input, &state.custom_commands);
            if !suggestions.is_empty() {
                let index = state.suggestion_selected.min(suggestions.len() - 1);
                let name = suggestions[index].0.clone();
                if let Some(command) = resolve_slash_command(&name) {
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(command));
                }
                if let Some(command) = resolve_custom_command(&state.custom_commands, &name) {
                    let body = expand_custom_command(&command.body, "");
                    state.clear_chat_input();
                    return Some(ChatSubmission::Prompt(body));
                }
            } else if let Some((name, arguments)) = trimmed.split_once(char::is_whitespace) {
                if name == "/review" {
                    let target = arguments.trim().to_string();
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(SlashCommand::Review(target)));
                }
                if let Some(command) = resolve_custom_command(&state.custom_commands, name) {
                    let body = expand_custom_command(&command.body, arguments.trim());
                    state.clear_chat_input();
                    return Some(ChatSubmission::Prompt(body));
                }
            }
            let text = std::mem::take(&mut state.chat_input);
            state.cursor = 0;
            state.remember_prompt(&text);
            if state.activity.is_some() {
                state.queued = Some(text);
                return None;
            }
            return Some(ChatSubmission::Prompt(text));
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    };
    use gocode_core::{
        AgentActivityState, AppEvent, ChatMessage, ErrorSeverity, McpServerStatus, SessionSummary,
        SkillSource, SkillSummary, ToolActivityStatus,
    };
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    use super::{
        AppState, ChatEntry, ChatSubmission, HelpTab, InputAction, MAX_VISIBLE_SUGGESTIONS,
        McpAddStep, McpEventOutcome, McpView, Screen, SkillsEventOutcome, SkillsView, SlashCommand,
        UpdateEventOutcome, UpdateStage, classify_event, handle_chat_event,
        handle_effort_picker_event, handle_help_event, handle_mcp_event, handle_model_picker_event,
        handle_onboarding_event, handle_permission_event, handle_session_picker_event,
        handle_skills_event, handle_update_event, render, run_with_event_source, slash_suggestions,
    };

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ))
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn error_severity_maps_to_distinct_provider_events() {
        assert_eq!(
            gocode_core::ProviderError::MissingCredential.severity(),
            ErrorSeverity::Blocking
        );
    }

    #[test]
    fn boot_lifecycle_events_render_boot_then_chat() {
        let mut state = AppState::default();

        state.apply(&AppEvent::BootStarted);
        assert_eq!(state.screen, Screen::Boot);

        state.apply(&AppEvent::BootCompleted);
        assert_eq!(state.screen, Screen::Chat);
    }

    #[test]
    fn restoring_the_saved_reasoning_effort_at_boot_does_not_spam_the_transcript() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };

        state.apply(&AppEvent::ReasoningEffortChanged {
            effort: Some("high".into()),
            announce: false,
        });
        assert!(state.entries.is_empty());

        state.apply(&AppEvent::ReasoningEffortChanged {
            effort: Some("low".into()),
            announce: true,
        });
        assert_eq!(state.entries.len(), 1);
    }

    #[test]
    fn update_waits_for_the_active_run_then_requires_explicit_confirmation() {
        let mut state = AppState {
            screen: Screen::Chat,
            activity: Some(AgentActivityState::Thinking),
            ..AppState::default()
        };
        state.apply(&AppEvent::UpdateAvailable {
            current_version: "0.0.1".into(),
            version: "0.0.2".into(),
            notes: "A safer updater.".into(),
        });
        assert!(state.pending_update.is_none());
        state.apply(&AppEvent::AgentCompleted {
            final_text: None,
            turns: 1,
            tool_calls: 0,
            failed_tool_calls: 0,
            last_input_tokens: None,
        });
        assert_eq!(
            state
                .pending_update
                .as_ref()
                .map(|prompt| prompt.version.as_str()),
            Some("0.0.2")
        );
        assert_eq!(
            handle_update_event(&mut state, &press(KeyCode::Char('n'))),
            UpdateEventOutcome::Rejected
        );
        assert!(state.pending_update.is_none());
    }

    fn update_prompt_state() -> AppState {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.apply(&AppEvent::UpdateAvailable {
            current_version: "0.1.0".into(),
            version: "0.2.0".into(),
            notes: "Release notes.".into(),
        });
        state
    }

    #[test]
    fn accepting_the_update_moves_straight_to_the_downloading_stage() {
        let mut state = update_prompt_state();
        assert_eq!(
            handle_update_event(&mut state, &press(KeyCode::Char('y'))),
            UpdateEventOutcome::Accepted
        );
        assert!(matches!(
            state.pending_update.as_ref().map(|prompt| &prompt.stage),
            Some(UpdateStage::Downloading { percent: None, .. })
        ));
    }

    #[test]
    fn progress_and_ready_events_drive_the_popup_through_to_completion() {
        let mut state = update_prompt_state();
        handle_update_event(&mut state, &press(KeyCode::Char('y')));

        state.apply(&AppEvent::UpdateProgress {
            percent: Some(42),
            message: "Downloading update…".into(),
        });
        assert!(matches!(
            state.pending_update.as_ref().map(|prompt| &prompt.stage),
            Some(UpdateStage::Downloading {
                percent: Some(42),
                ..
            })
        ));

        // While downloading, no key does anything but the popup keeps consuming input.
        assert_eq!(
            handle_update_event(&mut state, &press(KeyCode::Esc)),
            UpdateEventOutcome::Handled
        );

        state.apply(&AppEvent::UpdateReady {
            message: "Gocode will restart to finish the update.".into(),
        });
        assert!(matches!(
            state.pending_update.as_ref().map(|prompt| &prompt.stage),
            Some(UpdateStage::Completed { .. })
        ));

        assert_eq!(
            handle_update_event(&mut state, &press(KeyCode::Enter)),
            UpdateEventOutcome::RestartRequested
        );
        // The popup stays up until the driver confirms the exit; Close doesn't dismiss it.
        assert!(state.pending_update.is_some());
    }

    #[test]
    fn a_failed_update_shows_the_failure_and_close_dismisses_it() {
        let mut state = update_prompt_state();
        handle_update_event(&mut state, &press(KeyCode::Char('y')));

        state.apply(&AppEvent::UpdateFailed(
            "The download could not be verified.".into(),
        ));
        assert!(matches!(
            state.pending_update.as_ref().map(|prompt| &prompt.stage),
            Some(UpdateStage::Failed(message)) if message == "The download could not be verified."
        ));

        assert_eq!(
            handle_update_event(&mut state, &press(KeyCode::Enter)),
            UpdateEventOutcome::Handled
        );
        assert!(state.pending_update.is_none());
    }

    #[test]
    fn every_update_stage_renders_without_panicking() {
        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("terminal should initialize");

        let mut prompt_state = update_prompt_state();
        terminal
            .draw(|frame| render(frame, &prompt_state))
            .expect("prompt stage should render");
        assert!(buffer_text(&terminal).contains("New update"));

        handle_update_event(&mut prompt_state, &press(KeyCode::Char('y')));
        terminal
            .draw(|frame| render(frame, &prompt_state))
            .expect("downloading stage should render");

        prompt_state.apply(&AppEvent::UpdateProgress {
            percent: Some(55),
            message: "Downloading update…".into(),
        });
        terminal
            .draw(|frame| render(frame, &prompt_state))
            .expect("downloading stage with a percent should render");
        assert!(buffer_text(&terminal).contains("55%"));

        prompt_state.apply(&AppEvent::UpdateReady {
            message: "Gocode will restart to finish the update.".into(),
        });
        terminal
            .draw(|frame| render(frame, &prompt_state))
            .expect("completed stage should render");
        assert!(buffer_text(&terminal).contains("Completed"));

        let mut failed_state = update_prompt_state();
        handle_update_event(&mut failed_state, &press(KeyCode::Char('y')));
        failed_state.apply(&AppEvent::UpdateFailed("Network unreachable.".into()));
        terminal
            .draw(|frame| render(frame, &failed_state))
            .expect("failed stage should render");
        assert!(buffer_text(&terminal).contains("Network unreachable."));
    }

    #[test]
    fn chat_screen_renders_the_initial_prompt() {
        let mut terminal =
            Terminal::new(TestBackend::new(80, 20)).expect("terminal should initialize");
        let state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };

        terminal
            .draw(|frame| render(frame, &state))
            .expect("screen should render");

        let content = buffer_text(&terminal);
        assert!(content.contains("What can I help you build?"));
    }

    #[test]
    fn the_ascii_banner_scrolls_with_history_instead_of_staying_pinned() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        let empty_lines = super::compose_lines(&state);
        assert!(
            empty_lines
                .iter()
                .any(|line| line.contains("GOCODE") || line.contains('█'))
        );

        state.begin_run("hello".into());
        let busy_lines = super::compose_lines(&state);
        assert!(busy_lines.iter().any(|line| line.contains('█')));
        assert!(busy_lines.iter().any(|line| line.contains("You: hello")));
    }

    #[test]
    fn the_permission_mode_line_is_visible_even_with_a_status_line_shown() {
        let mut terminal =
            Terminal::new(TestBackend::new(80, 20)).expect("terminal should initialize");
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.apply(&AppEvent::ModelSelected("nvidia/model".into()));

        terminal
            .draw(|frame| render(frame, &state))
            .expect("screen should render");

        assert!(buffer_text(&terminal).contains("auto mode on"));
    }

    #[test]
    fn esc_to_interrupt_hint_shows_only_while_the_agent_is_active() {
        let mut terminal =
            Terminal::new(TestBackend::new(80, 20)).expect("terminal should initialize");
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };

        terminal
            .draw(|frame| render(frame, &state))
            .expect("screen should render");
        assert!(!buffer_text(&terminal).contains("ESC to interrupt"));

        state.begin_run("hi".into());
        terminal
            .draw(|frame| render(frame, &state))
            .expect("screen should render");
        assert!(buffer_text(&terminal).contains("ESC to interrupt"));
    }

    #[test]
    fn the_copy_notification_does_not_overwrite_the_history_border() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.copy_notification = Some("Copied 78 chars to clipboard".into());
        let terminal_area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        };
        let (history_area, _) = super::chat_layout(terminal_area, &state);

        let mut terminal =
            Terminal::new(TestBackend::new(80, 20)).expect("terminal should initialize");
        terminal
            .draw(|frame| render(frame, &state))
            .expect("screen should render");

        let buffer = terminal.backend().buffer();
        let border_row = history_area.y + history_area.height - 1;
        let border_char = buffer
            .cell((history_area.x + history_area.width - 1, border_row))
            .expect("border cell should exist")
            .symbol();
        assert_ne!(border_char, " ");
        assert_ne!(border_char, "d");

        assert!(buffer_text(&terminal).contains("Copied 78 chars to clipboard"));
    }

    #[test]
    fn shift_tab_cycles_the_permission_mode_through_auto_plan_approve() {
        use gocode_core::PermissionMode;

        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        assert_eq!(state.permission_mode, PermissionMode::Auto);

        let back_tab = Event::Key(KeyEvent::new_with_kind(
            KeyCode::BackTab,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));

        assert_eq!(
            super::handle_permission_mode_event(&mut state, &back_tab),
            Some(PermissionMode::Plan)
        );
        assert_eq!(
            super::handle_permission_mode_event(&mut state, &back_tab),
            Some(PermissionMode::Approve)
        );
        assert_eq!(
            super::handle_permission_mode_event(&mut state, &back_tab),
            Some(PermissionMode::Auto)
        );
    }

    #[test]
    fn credential_onboarding_masks_the_api_key() {
        let mut state = AppState::default();
        state.apply(&AppEvent::CredentialRequired);
        state.push_credential_character('a');
        state.push_credential_character('b');
        let mut terminal =
            Terminal::new(TestBackend::new(80, 20)).expect("terminal should initialize");

        terminal
            .draw(|frame| render(frame, &state))
            .expect("screen should render");

        let content = buffer_text(&terminal);
        assert!(content.contains("NVIDIA API key"));
        assert!(content.contains("••"));
        assert!(!content.contains("ab"));
    }

    #[test]
    fn onboarding_submission_moves_the_key_out_of_tui_state() {
        let mut state = AppState::default();
        state.apply(&AppEvent::CredentialRequired);
        state.push_credential_character('a');

        assert_eq!(state.take_credential_submission(), Some("a".into()));
        assert_eq!(state.take_credential_submission(), None);
    }

    #[test]
    fn onboarding_paste_appends_the_api_key_without_submitting_it() {
        let mut state = AppState::default();
        state.apply(&AppEvent::CredentialRequired);

        assert_eq!(
            handle_onboarding_event(&mut state, &Event::Paste("nvapi-secret".into())),
            None
        );
        assert_eq!(
            state.take_credential_submission(),
            Some("nvapi-secret".into())
        );
    }

    #[test]
    fn model_picker_confirms_the_highlighted_model() {
        let mut state = AppState::default();
        state.apply(&AppEvent::ModelsAvailable(vec!["nvidia/model".into()]));
        let submit = press(KeyCode::Enter);

        assert_eq!(
            super::handle_model_picker_event(&mut state, &submit),
            Some("nvidia/model".into())
        );
    }

    #[test]
    fn esc_cancels_the_model_picker_only_when_a_model_was_already_selected() {
        let mut state = AppState::default();
        state.apply(&AppEvent::ModelsAvailable(vec!["nvidia/model".into()]));

        assert_eq!(
            super::handle_model_picker_event(&mut state, &press(KeyCode::Esc)),
            None
        );
        assert_eq!(state.screen, Screen::ModelPicker);

        state.apply(&AppEvent::ModelSelected("nvidia/model".into()));
        state.screen = Screen::ModelPicker;

        assert_eq!(
            super::handle_model_picker_event(&mut state, &press(KeyCode::Esc)),
            None
        );
        assert_eq!(state.screen, Screen::Chat);
    }

    #[test]
    fn esc_cancels_credential_onboarding_only_when_a_model_was_already_selected() {
        let mut state = AppState::default();
        state.apply(&AppEvent::CredentialRequired);
        state.push_credential_character('a');

        assert_eq!(
            handle_onboarding_event(&mut state, &press(KeyCode::Esc)),
            None
        );
        assert_eq!(state.screen, Screen::Onboarding);

        state.apply(&AppEvent::ModelSelected("nvidia/model".into()));
        state.screen = Screen::Onboarding;
        state.push_credential_character('a');

        assert_eq!(
            handle_onboarding_event(&mut state, &press(KeyCode::Esc)),
            None
        );
        assert_eq!(state.screen, Screen::Chat);
    }

    #[test]
    fn terminal_loop_renders_then_exits_on_ctrl_c() {
        let mut terminal =
            Terminal::new(TestBackend::new(40, 10)).expect("terminal should initialize");
        let exit = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        ));

        assert_eq!(
            run_with_event_source(&mut terminal, &AppEvent::BootCompleted, || Ok(exit.clone()))
                .expect("terminal loop should exit cleanly"),
            InputAction::Interrupt
        );
    }

    #[test]
    fn resize_key_release_and_plain_q_do_not_exit_the_tui() {
        assert_eq!(
            classify_event(&Event::Resize(120, 40)),
            InputAction::Continue
        );
        assert_eq!(
            classify_event(&Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                KeyEventKind::Press,
            ))),
            InputAction::Continue
        );
        assert_eq!(
            classify_event(&Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ))),
            InputAction::Continue
        );
    }

    #[test]
    fn ctrl_c_is_the_global_exit_action() {
        assert_eq!(
            classify_event(&Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ))),
            InputAction::Interrupt
        );
    }

    #[test]
    fn streaming_deltas_append_to_one_assistant_entry_per_run() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.begin_run("hi".into());

        state.apply(&AppEvent::AssistantTextDelta("Hel".into()));
        state.apply(&AppEvent::AssistantTextDelta("lo".into()));

        assert_eq!(
            state.entries.last(),
            Some(&ChatEntry::Assistant("Hello".into()))
        );
    }

    #[test]
    fn tool_activity_updates_the_same_entry_by_id() {
        let mut state = AppState::default();

        state.apply(&AppEvent::ToolActivity {
            id: "call-1".into(),
            name: "read_file".into(),
            status: ToolActivityStatus::Started,
            detail: "running".into(),
        });
        state.apply(&AppEvent::ToolActivity {
            id: "call-1".into(),
            name: String::new(),
            status: ToolActivityStatus::Succeeded,
            detail: "done".into(),
        });

        assert_eq!(state.entries.len(), 1);
        let ChatEntry::Tool { name, status, .. } = &state.entries[0] else {
            panic!("expected a tool entry");
        };
        assert_eq!(name, "read_file");
        assert_eq!(*status, ToolActivityStatus::Succeeded);
    }

    #[test]
    fn tool_output_is_collapsed_until_toggled() {
        let mut state = AppState::default();
        state.apply(&AppEvent::ToolActivity {
            id: "call-1".into(),
            name: "run_command".into(),
            status: ToolActivityStatus::Started,
            detail: "running".into(),
        });
        for line in 0..10 {
            state.apply(&AppEvent::ToolOutputChunk {
                id: "call-1".into(),
                chunk: format!("line {line}\n"),
            });
        }

        let mut terminal =
            Terminal::new(TestBackend::new(80, 20)).expect("terminal should initialize");
        let mut chat_state = state.clone();
        chat_state.screen = Screen::Chat;
        terminal
            .draw(|frame| render(frame, &chat_state))
            .expect("screen should render");
        assert!(!buffer_text(&terminal).contains("line 9"));

        chat_state.toggle_last_tool_output();
        terminal
            .draw(|frame| render(frame, &chat_state))
            .expect("screen should render");
        assert!(buffer_text(&terminal).contains("line 9"));
    }

    #[test]
    fn permission_prompt_blocks_chat_input_and_resolves_on_yes() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.apply(&AppEvent::PermissionRequested {
            summary: "run: npm install".into(),
            working_directory: ".".into(),
        });

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Char('x'))),
            None
        );
        assert_eq!(
            handle_permission_event(&mut state, &press(KeyCode::Char('y'))),
            Some(true)
        );
        assert!(state.pending_permission.is_none());
    }

    #[test]
    fn permission_denial_via_n_clears_the_prompt() {
        let mut state = AppState::default();
        state.apply(&AppEvent::PermissionRequested {
            summary: "run: rm".into(),
            working_directory: ".".into(),
        });

        assert_eq!(
            handle_permission_event(&mut state, &press(KeyCode::Char('n'))),
            Some(false)
        );
        assert!(state.pending_permission.is_none());
    }

    #[test]
    fn cancellation_clears_a_stale_permission_prompt() {
        let mut state = AppState::default();
        state.apply(&AppEvent::PermissionRequested {
            summary: "run: rm".into(),
            working_directory: ".".into(),
        });

        state.apply(&AppEvent::AgentCancelled);

        assert!(state.pending_permission.is_none());
    }

    #[test]
    fn blocking_error_suppresses_chat_input_until_dismissed() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.apply(&AppEvent::BlockingError("credential rejected".into()));

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Char('x'))),
            None
        );
        assert!(state.chat_input.is_empty());
    }

    #[test]
    fn slash_command_is_recognized_and_cleared_from_the_composer() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/clear".into(),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::Clear))
        );
        assert!(state.chat_input.is_empty());
    }

    #[test]
    fn review_with_no_target_reviews_the_working_tree_diff() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/review".into(),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::Review(String::new())))
        );
        assert!(state.chat_input.is_empty());
    }

    #[test]
    fn review_with_a_target_carries_it_through_as_an_argument() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/review main".into(),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::Review("main".into())))
        );
        assert!(state.chat_input.is_empty());
    }

    #[test]
    fn resuming_a_session_replays_its_history_and_starting_new_clears_the_transcript() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.begin_run("leftover prompt".into());

        state.apply(&AppEvent::SessionSwitched {
            id: "session-1".into(),
            name: "fix the login bug".into(),
            is_new: false,
            history: vec![
                ChatMessage::User("what broke login?".into()),
                ChatMessage::assistant_text("A race condition in the token refresh."),
            ],
        });

        assert_eq!(state.screen, Screen::Chat);
        assert_eq!(state.entries.len(), 3);
        assert_eq!(
            state.entries[0],
            ChatEntry::User("what broke login?".into())
        );
        assert_eq!(
            state.entries[1],
            ChatEntry::Assistant("A race condition in the token refresh.".into())
        );

        state.apply(&AppEvent::SessionSwitched {
            id: "session-2".into(),
            name: "New session".into(),
            is_new: true,
            history: Vec::new(),
        });
        assert_eq!(state.entries.len(), 1);
        assert_eq!(
            state.entries[0],
            ChatEntry::Info("Started session: New session".into())
        );
    }

    #[test]
    fn the_session_list_populates_the_picker_and_enter_returns_the_selected_id() {
        let mut state = AppState {
            screen: Screen::SessionPicker,
            current_model: Some("nvidia/model".into()),
            ..AppState::default()
        };
        state.apply(&AppEvent::SessionListAvailable(vec![
            SessionSummary {
                id: "one".into(),
                name: "first".into(),
                summary: "did stuff".into(),
                last_used_at_unix: 100,
            },
            SessionSummary {
                id: "two".into(),
                name: "second".into(),
                summary: "did other stuff".into(),
                last_used_at_unix: 200,
            },
        ]));
        assert_eq!(state.sessions.len(), 2);

        let _ = handle_session_picker_event(&mut state, &press(KeyCode::Down));
        assert_eq!(
            handle_session_picker_event(&mut state, &press(KeyCode::Enter)),
            Some("two".into())
        );

        assert_eq!(
            handle_session_picker_event(&mut state, &press(KeyCode::Esc)),
            None
        );
        assert_eq!(state.screen, Screen::Chat);
    }

    #[test]
    fn autocompact_toggles_and_reports_the_command_it_sent() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/autocompact".into(),
            ..AppState::default()
        };
        assert!(!state.auto_compact_disabled);

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::AutoCompact))
        );
    }

    #[test]
    fn compact_is_blocked_while_the_agent_is_busy_but_allowed_when_idle() {
        let mut idle = AppState {
            screen: Screen::Chat,
            chat_input: "/compact".into(),
            ..AppState::default()
        };
        assert_eq!(
            handle_chat_event(&mut idle, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::Compact))
        );
    }

    #[test]
    fn context_compacted_and_failed_events_surface_as_transcript_entries() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.apply(&AppEvent::ContextCompacted { automatic: true });
        assert!(matches!(state.entries.last(), Some(ChatEntry::Info(_))));

        state.apply(&AppEvent::ContextCompactionFailed("network error".into()));
        assert!(matches!(state.entries.last(), Some(ChatEntry::Warning(_))));
    }

    #[test]
    fn relative_time_reports_minutes_and_hours_within_a_day() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_secs();

        assert_eq!(
            super::format_relative_time(i64::try_from(now).unwrap() - 60),
            "1 minute ago"
        );
        assert_eq!(
            super::format_relative_time(i64::try_from(now).unwrap() - 2 * 3600),
            "2 hours ago"
        );
        // Beyond 24h it falls back to an absolute date, not a relative phrase.
        let over_a_day_ago = i64::try_from(now).unwrap() - 25 * 3600;
        assert!(!super::format_relative_time(over_a_day_ago).contains("ago"));
    }

    #[test]
    fn unrecognized_slash_text_is_still_sent_as_a_prompt() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/not-a-command please help".into(),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Prompt("/not-a-command please help".into()))
        );
    }

    #[test]
    fn tab_completes_the_first_matching_slash_suggestion() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/mo".into(),
            ..AppState::default()
        };

        let _ = handle_chat_event(&mut state, &press(KeyCode::Tab));

        assert_eq!(state.chat_input, "/model");
    }

    #[test]
    fn arrow_keys_move_the_highlighted_suggestion_and_tab_completes_it() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/s".into(),
            ..AppState::default()
        };
        assert_eq!(
            slash_suggestions(&state.chat_input, &state.custom_commands).len(),
            3
        );

        let _ = handle_chat_event(&mut state, &press(KeyCode::Down));
        assert_eq!(state.suggestion_selected, 1);

        let _ = handle_chat_event(&mut state, &press(KeyCode::Tab));
        assert_eq!(state.chat_input, slash_suggestions("/s", &[])[1].0);
    }

    #[test]
    fn enter_runs_the_highlighted_suggestion_even_when_the_command_is_incomplete() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/set".into(),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::Settings))
        );
        assert!(state.chat_input.is_empty());
    }

    #[test]
    fn typing_resets_the_highlighted_suggestion_back_to_the_first_match() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/s".into(),
            ..AppState::default()
        };
        let _ = handle_chat_event(&mut state, &press(KeyCode::Down));
        assert_eq!(state.suggestion_selected, 1);

        let _ = handle_chat_event(&mut state, &press(KeyCode::Char('o')));
        assert_eq!(state.suggestion_selected, 0);
    }

    #[test]
    fn slash_suggestions_filter_by_prefix() {
        assert_eq!(slash_suggestions("/s", &[]).len(), 3);
        assert!(slash_suggestions("hello", &[]).is_empty());
    }

    #[test]
    fn slash_suggestions_include_custom_commands_and_are_matched_before_a_builtin_lookup() {
        let custom = vec![gocode_core::CustomCommand {
            name: "deploy".into(),
            description: "Ship the app".into(),
            body: "Deploy to $ARGUMENTS.".into(),
        }];

        let suggestions = slash_suggestions("/dep", &custom);
        assert_eq!(
            suggestions,
            vec![("/deploy".to_string(), "Ship the app".to_string())]
        );
    }

    #[test]
    fn enter_expands_a_custom_command_with_its_arguments() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/deploy staging".into(),
            custom_commands: vec![gocode_core::CustomCommand {
                name: "deploy".into(),
                description: "Ship the app".into(),
                body: "Deploy to $ARGUMENTS now.".into(),
            }],
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Prompt("Deploy to staging now.".into()))
        );
        assert!(state.chat_input.is_empty());
    }

    #[test]
    fn enter_expands_a_custom_command_with_no_arguments() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/deploy".into(),
            custom_commands: vec![gocode_core::CustomCommand {
                name: "deploy".into(),
                description: "Ship the app".into(),
                body: "Deploy to $ARGUMENTS now.".into(),
            }],
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Prompt("Deploy to  now.".into()))
        );
    }

    #[test]
    fn help_popup_toggles_visible_and_dismisses_on_escape() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/help".into(),
            ..AppState::default()
        };
        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::Help))
        );

        state.help_visible = true;
        assert!(handle_help_event(&mut state, &press(KeyCode::Esc)));
        assert!(!state.help_visible);
    }

    #[test]
    fn help_popup_cycles_tabs_with_tab_and_shift_tab() {
        let mut state = AppState {
            screen: Screen::Chat,
            help_visible: true,
            ..AppState::default()
        };
        assert_eq!(state.help_tab, HelpTab::General);

        assert!(handle_help_event(&mut state, &press(KeyCode::Tab)));
        assert_eq!(state.help_tab, HelpTab::Commands);

        assert!(handle_help_event(&mut state, &press(KeyCode::Tab)));
        assert_eq!(state.help_tab, HelpTab::Custom);

        assert!(handle_help_event(&mut state, &press(KeyCode::Tab)));
        assert_eq!(state.help_tab, HelpTab::General);

        assert!(handle_help_event(&mut state, &press(KeyCode::BackTab)));
        assert_eq!(state.help_tab, HelpTab::Custom);
    }

    #[test]
    fn help_popup_scrolls_with_up_down_and_resets_on_tab_switch() {
        let mut state = AppState {
            screen: Screen::Chat,
            help_visible: true,
            ..AppState::default()
        };

        assert!(handle_help_event(&mut state, &press(KeyCode::Down)));
        assert_eq!(state.help_scroll, 1);
        assert!(handle_help_event(&mut state, &press(KeyCode::PageDown)));
        assert_eq!(state.help_scroll, 6);
        assert!(handle_help_event(&mut state, &press(KeyCode::Up)));
        assert_eq!(state.help_scroll, 5);

        assert!(handle_help_event(&mut state, &press(KeyCode::Tab)));
        assert_eq!(state.help_scroll, 0);
    }

    #[test]
    fn skills_list_scrolls_with_up_down_and_resets_when_leaving_the_view() {
        let mut state = AppState {
            skills_visible: true,
            skills: sample_skills(),
            skills_view: SkillsView::List,
            ..AppState::default()
        };

        assert!(matches!(
            handle_skills_event(&mut state, &press(KeyCode::Down)),
            SkillsEventOutcome::Handled
        ));
        assert_eq!(state.skills_list_scroll, 1);

        assert!(matches!(
            handle_skills_event(&mut state, &press(KeyCode::Esc)),
            SkillsEventOutcome::Handled
        ));
        assert_eq!(state.skills_view, SkillsView::Menu);
        assert_eq!(state.skills_list_scroll, 0);
    }

    #[test]
    fn model_flow_chains_into_the_effort_picker_before_returning_to_chat() {
        let mut state = AppState {
            screen: Screen::ModelPicker,
            models: vec!["nvidia/model-a".into()],
            selected_model: 0,
            model_flow_pending_effort: true,
            ..AppState::default()
        };

        assert_eq!(
            handle_model_picker_event(&mut state, &press(KeyCode::Enter)),
            None
        );
        assert_eq!(state.screen, Screen::EffortPicker);
        assert_eq!(state.pending_model.as_deref(), Some("nvidia/model-a"));

        assert_eq!(
            handle_effort_picker_event(&mut state, &press(KeyCode::Enter)),
            Some(None)
        );
    }

    #[test]
    fn a_prompt_submitted_while_busy_is_queued_not_sent() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "second question".into(),
            ..AppState::default()
        };
        state.begin_run("first question".into());

        assert_eq!(handle_chat_event(&mut state, &press(KeyCode::Enter)), None);
        assert_eq!(state.queued.as_deref(), Some("second question"));
    }

    #[test]
    fn completion_dispatches_the_queued_prompt() {
        let mut state = AppState::default();
        state.begin_run("first".into());
        state.queued = Some("second".into());

        let dispatched = state.apply(&AppEvent::AgentCompleted {
            final_text: None,
            turns: 1,
            tool_calls: 0,
            failed_tool_calls: 0,
            last_input_tokens: None,
        });

        assert_eq!(dispatched, Some("second".into()));
        assert!(state.queued.is_none());
    }

    #[test]
    fn a_failed_provider_call_remembers_the_prompt_for_retry() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.begin_run("flaky prompt".into());

        state.apply(&AppEvent::ProviderFailed("network error".into()));

        assert_eq!(state.last_failed_prompt.as_deref(), Some("flaky prompt"));
        let _ = handle_chat_event(
            &mut state,
            &Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('r'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            )),
        );
        assert_eq!(state.chat_input, "flaky prompt");
    }

    #[test]
    fn scroll_locks_to_the_bottom_until_the_user_scrolls_up() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        assert!(state.is_scroll_locked());

        state.scroll_up();
        assert!(!state.is_scroll_locked());

        state.scroll_down();
        assert!(state.is_scroll_locked());
    }

    #[test]
    fn a_narrow_terminal_shows_a_resize_notice_instead_of_the_screen() {
        let mut terminal =
            Terminal::new(TestBackend::new(20, 5)).expect("terminal should initialize");
        let state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };

        terminal
            .draw(|frame| render(frame, &state))
            .expect("screen should render");

        assert!(buffer_text(&terminal).contains("too small"));
    }

    #[test]
    fn agent_thinking_state_renders_as_a_status_line() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.begin_run("hi".into());
        state.apply(&AppEvent::AgentStateChanged(AgentActivityState::Thinking));

        let mut terminal =
            Terminal::new(TestBackend::new(80, 20)).expect("terminal should initialize");
        terminal
            .draw(|frame| render(frame, &state))
            .expect("screen should render");

        assert!(buffer_text(&terminal).contains("thinking"));
    }

    #[test]
    fn settings_menu_navigates_to_the_effort_picker_and_confirms_a_selection() {
        use super::{handle_effort_picker_event, handle_settings_event};

        let mut state = AppState {
            screen: Screen::Settings,
            ..AppState::default()
        };

        assert!(handle_settings_event(&mut state, &press(KeyCode::Down)));
        assert_eq!(state.settings_selected, 1);
        assert!(handle_settings_event(&mut state, &press(KeyCode::Down)));
        assert_eq!(state.settings_selected, 2);
        assert!(handle_settings_event(&mut state, &press(KeyCode::Enter)));
        assert_eq!(state.screen, Screen::EffortPicker);

        assert_eq!(
            handle_effort_picker_event(&mut state, &press(KeyCode::Down)),
            None
        );
        assert_eq!(state.selected_effort, 1);
        assert_eq!(
            handle_effort_picker_event(&mut state, &press(KeyCode::Enter)),
            Some(Some("low".into()))
        );
    }

    #[test]
    fn pasted_text_is_inserted_into_the_composer_without_submitting() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &Event::Paste("multi\nline".into())),
            None
        );
        assert_eq!(state.chat_input, "multi\nline");
    }

    #[test]
    fn shift_enter_and_ctrl_j_insert_a_newline_without_submitting() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };

        let shift_enter = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::SHIFT,
            KeyEventKind::Press,
        ));
        assert_eq!(handle_chat_event(&mut state, &shift_enter), None);
        assert_eq!(state.chat_input, "\n");

        let ctrl_j = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        ));
        assert_eq!(handle_chat_event(&mut state, &ctrl_j), None);
        assert_eq!(state.chat_input, "\n\n");
    }

    #[test]
    fn left_and_right_arrows_move_the_cursor_and_typing_inserts_at_it() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };

        for character in "helo".chars() {
            let _ = handle_chat_event(&mut state, &press(KeyCode::Char(character)));
        }
        assert_eq!(state.chat_input, "helo");
        assert_eq!(state.cursor, 4);

        // Move left twice to land between 'l' and 'o', then insert 'l' to fix "helo" -> "hello".
        let _ = handle_chat_event(&mut state, &press(KeyCode::Left));
        let _ = handle_chat_event(&mut state, &press(KeyCode::Left));
        assert_eq!(state.cursor, 2);
        let _ = handle_chat_event(&mut state, &press(KeyCode::Char('l')));
        assert_eq!(state.chat_input, "hello");
        assert_eq!(state.cursor, 3);

        let _ = handle_chat_event(&mut state, &press(KeyCode::Right));
        assert_eq!(state.cursor, 4);
    }

    #[test]
    fn backspace_removes_the_character_before_the_cursor_not_the_last_one() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "abc".into(),
            cursor: 3,
            ..AppState::default()
        };

        let _ = handle_chat_event(&mut state, &press(KeyCode::Left));
        assert_eq!(state.cursor, 2);
        let _ = handle_chat_event(&mut state, &press(KeyCode::Backspace));
        assert_eq!(state.chat_input, "ac");
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn editing_at_the_cursor_is_utf8_safe_with_accented_characters() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };

        for character in "café".chars() {
            let _ = handle_chat_event(&mut state, &press(KeyCode::Char(character)));
        }
        assert_eq!(state.chat_input, "café");
        assert_eq!(state.cursor, 4);

        // Cursor sits between 'f' and 'é'; backspace removes 'f', proving byte offsets never
        // land inside 'é's multi-byte UTF-8 encoding.
        let _ = handle_chat_event(&mut state, &press(KeyCode::Left));
        let _ = handle_chat_event(&mut state, &press(KeyCode::Backspace));
        assert_eq!(state.chat_input, "caé");
    }

    #[test]
    fn up_and_down_arrows_move_the_cursor_between_lines_when_no_suggestions_are_open() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "first\nsecond".into(),
            cursor: 3,
            ..AppState::default()
        };

        let _ = handle_chat_event(&mut state, &press(KeyCode::Down));
        assert_eq!(
            state.cursor,
            super::char_index_from_line_col(&["first", "second"], 1, 3)
        );

        let _ = handle_chat_event(&mut state, &press(KeyCode::Up));
        assert_eq!(state.cursor, 3);
    }

    #[test]
    fn up_arrow_recalls_prompt_history_and_down_arrow_walks_back_to_the_draft() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.remember_prompt("first prompt");
        state.remember_prompt("second prompt");
        state.chat_input = "unsent draft".into();
        state.cursor = state.chat_input.chars().count();

        let _ = handle_chat_event(&mut state, &press(KeyCode::Up));
        assert_eq!(state.chat_input, "second prompt");

        let _ = handle_chat_event(&mut state, &press(KeyCode::Up));
        assert_eq!(state.chat_input, "first prompt");

        // Already at the oldest entry: another Up is a no-op.
        let _ = handle_chat_event(&mut state, &press(KeyCode::Up));
        assert_eq!(state.chat_input, "first prompt");

        let _ = handle_chat_event(&mut state, &press(KeyCode::Down));
        assert_eq!(state.chat_input, "second prompt");

        // Past the newest entry, the original in-progress draft comes back.
        let _ = handle_chat_event(&mut state, &press(KeyCode::Down));
        assert_eq!(state.chat_input, "unsent draft");
    }

    #[test]
    fn submitting_a_prompt_makes_it_recallable_and_typing_exits_history_browsing() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "hello agent".into(),
            cursor: "hello agent".chars().count(),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Prompt("hello agent".into()))
        );
        assert!(state.chat_input.is_empty());

        let _ = handle_chat_event(&mut state, &press(KeyCode::Up));
        assert_eq!(state.chat_input, "hello agent");

        // Typing while browsing history detaches from it instead of mutating the saved entry.
        let _ = handle_chat_event(&mut state, &press(KeyCode::Char('!')));
        assert_eq!(state.chat_input, "hello agent!");
        let _ = handle_chat_event(&mut state, &press(KeyCode::Up));
        assert_eq!(state.chat_input, "hello agent");
    }

    #[test]
    fn multiline_up_only_recalls_history_from_the_first_line() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "first\nsecond".into(),
            cursor: super::char_index_from_line_col(&["first", "second"], 1, 2),
            ..AppState::default()
        };
        state.remember_prompt("older prompt");

        // On the last line: Up moves the cursor up a line, not history.
        let _ = handle_chat_event(&mut state, &press(KeyCode::Up));
        assert_eq!(state.chat_input, "first\nsecond");
        assert_eq!(
            state.cursor,
            super::char_index_from_line_col(&["first", "second"], 0, 2)
        );

        // Now on the first line: Up recalls history instead of moving further up.
        let _ = handle_chat_event(&mut state, &press(KeyCode::Up));
        assert_eq!(state.chat_input, "older prompt");
    }

    #[test]
    fn the_suggestion_list_scrolls_instead_of_growing_without_bound() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/".into(),
            cursor: 1,
            ..AppState::default()
        };
        let total = slash_suggestions("/", &[]).len();
        assert!(
            total >= 6,
            "test assumes at least 6 commands share the '/' prefix"
        );

        let (start, end, truncated) = super::visible_suggestion_window(total, 0);
        assert!(truncated);
        assert_eq!(end - start, MAX_VISIBLE_SUGGESTIONS - 1);

        state.suggestion_selected = total - 1;
        let (start, end, truncated) = super::visible_suggestion_window(total, total - 1);
        assert!(truncated);
        assert_eq!(end, total);
        assert!(end - start < MAX_VISIBLE_SUGGESTIONS);
    }

    #[test]
    fn clicking_the_composer_input_moves_the_cursor_there() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "hello world".into(),
            cursor: 0,
            ..AppState::default()
        };
        let terminal_area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        };
        let (_, composer_area) = super::chat_layout(terminal_area, &state);

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: composer_area.x + 1 + 2 + 6, // border + "> " prefix + 6 chars into "hello world"
            row: composer_area.y + 1,
            modifiers: KeyModifiers::NONE,
        };
        assert!(super::handle_mouse_event(&mut state, &click, terminal_area));
        assert_eq!(state.cursor, 6);
        assert!(state.selection.is_none());
    }

    #[test]
    fn wrap_lines_breaks_long_lines_at_a_word_boundary() {
        let lines = vec!["the quick brown fox jumps".to_string()];
        let wrapped = super::wrap_lines(&lines, 10);

        assert_eq!(wrapped, vec!["the quick", "brown fox", "jumps"]);
    }

    #[test]
    fn wrap_lines_preserves_blank_lines() {
        let lines = vec![String::new(), "hi".to_string()];
        assert_eq!(super::wrap_lines(&lines, 10), vec!["", "hi"]);
    }

    #[test]
    fn selected_char_range_spans_a_partial_first_line_full_middle_and_partial_last_line() {
        let selection = super::Selection {
            anchor: super::SelectionPoint { line: 0, col: 5 },
            cursor: super::SelectionPoint { line: 2, col: 3 },
        };

        assert_eq!(super::selected_char_range(&selection, 0, 10), Some((5, 10)));
        assert_eq!(super::selected_char_range(&selection, 1, 10), Some((0, 10)));
        assert_eq!(super::selected_char_range(&selection, 2, 10), Some((0, 3)));
        assert_eq!(super::selected_char_range(&selection, 3, 10), None);
    }

    #[test]
    fn dragging_the_mouse_extends_the_selection_and_extracts_the_selected_text() {
        let mut state = AppState {
            screen: Screen::Chat,
            entries: vec![ChatEntry::Info("hello world".into())],
            ..AppState::default()
        };
        let terminal_area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        };

        let (history_area, _) = super::chat_layout(terminal_area, &state);
        let content_width = usize::from(history_area.width.saturating_sub(2));
        let wrapped = super::wrap_lines(&super::compose_lines(&state), content_width);
        let line_index = wrapped
            .iter()
            .position(|line| line.contains("hello world"))
            .expect("the info entry should appear in the wrapped transcript");
        let line_chars: Vec<char> = wrapped[line_index].chars().collect();
        let needle: Vec<char> = "hello".chars().collect();
        let hello_col = line_chars
            .windows(needle.len())
            .position(|window| window == needle.as_slice())
            .expect("hello is on the line");
        let visible_rows = usize::from(history_area.height.saturating_sub(2)).max(1);
        let (start, _end) =
            super::compute_visible_window(wrapped.len(), visible_rows, state.scroll);
        let row = history_area.y + 1 + u16::try_from(line_index - start).unwrap();

        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: history_area.x + 1 + u16::try_from(hello_col).unwrap(),
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert!(super::handle_mouse_event(&mut state, &down, terminal_area));
        assert!(state.selection.is_some());

        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: down.column + 5,
            row: down.row,
            modifiers: KeyModifiers::NONE,
        };
        assert!(super::handle_mouse_event(&mut state, &drag, terminal_area));

        let selected = super::extract_selected_text(&state, terminal_area);
        assert_eq!(selected.as_deref(), Some("hello"));
    }

    #[test]
    fn clicking_outside_the_history_area_clears_the_selection() {
        let mut state = AppState {
            screen: Screen::Chat,
            selection: Some(super::Selection {
                anchor: super::SelectionPoint { line: 0, col: 0 },
                cursor: super::SelectionPoint { line: 0, col: 3 },
            }),
            ..AppState::default()
        };
        let terminal_area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        };

        let click_below_history = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: terminal_area.height - 1,
            modifiers: KeyModifiers::NONE,
        };
        assert!(super::handle_mouse_event(
            &mut state,
            &click_below_history,
            terminal_area
        ));
        assert!(state.selection.is_none());
    }

    fn sample_skills() -> Vec<SkillSummary> {
        vec![
            SkillSummary {
                name: "brainstorming".into(),
                description: "Use before creative work".into(),
                source: SkillSource::Global,
                path: "brainstorming/SKILL.md".into(),
                enabled: true,
            },
            SkillSummary {
                name: "brandkit".into(),
                description: "Premium brand-kit skill".into(),
                source: SkillSource::Project,
                path: "brandkit/SKILL.md".into(),
                enabled: true,
            },
        ]
    }

    #[test]
    fn skills_popup_opens_on_the_choose_an_action_menu() {
        let mut state = AppState {
            skills_visible: true,
            skills: sample_skills(),
            ..AppState::default()
        };
        assert_eq!(state.skills_view, SkillsView::Menu);
        assert_eq!(state.skills_selected, 0);

        assert!(matches!(
            handle_skills_event(&mut state, &press(KeyCode::Down)),
            SkillsEventOutcome::Handled
        ));
        assert_eq!(state.skills_selected, 1);
    }

    #[test]
    fn selecting_list_skills_shows_every_skill_then_esc_returns_to_the_menu() {
        let mut state = AppState {
            skills_visible: true,
            skills: sample_skills(),
            ..AppState::default()
        };

        assert!(matches!(
            handle_skills_event(&mut state, &press(KeyCode::Enter)),
            SkillsEventOutcome::Handled
        ));
        assert_eq!(state.skills_view, SkillsView::List);

        assert!(matches!(
            handle_skills_event(&mut state, &press(KeyCode::Esc)),
            SkillsEventOutcome::Handled
        ));
        assert_eq!(state.skills_view, SkillsView::Menu);
        assert!(state.skills_visible);
    }

    #[test]
    fn esc_on_the_menu_closes_the_whole_popup() {
        let mut state = AppState {
            skills_visible: true,
            skills: sample_skills(),
            ..AppState::default()
        };

        assert!(matches!(
            handle_skills_event(&mut state, &press(KeyCode::Esc)),
            SkillsEventOutcome::Handled
        ));
        assert!(!state.skills_visible);
    }

    #[test]
    fn enable_disable_screen_toggles_a_skill_and_reports_it_for_persistence() {
        let mut state = AppState {
            skills_visible: true,
            skills: sample_skills(),
            skills_view: SkillsView::EnableDisable,
            skills_selected: 0,
            ..AppState::default()
        };
        assert!(state.skills[0].enabled);

        match handle_skills_event(&mut state, &press(KeyCode::Enter)) {
            SkillsEventOutcome::ToggleSkill { name, enabled } => {
                assert_eq!(name, "brainstorming");
                assert!(!enabled);
            }
            other => panic!("expected a toggle outcome, got a different result: {other:?}"),
        }
        assert!(!state.skills[0].enabled);

        match handle_skills_event(&mut state, &press(KeyCode::Char(' '))) {
            SkillsEventOutcome::ToggleSkill { name, enabled } => {
                assert_eq!(name, "brainstorming");
                assert!(enabled);
            }
            other => panic!("expected a toggle outcome, got a different result: {other:?}"),
        }
        assert!(state.skills[0].enabled);
    }

    fn sample_mcp_servers() -> Vec<McpServerStatus> {
        vec![
            McpServerStatus {
                name: "filesystem".into(),
                transport: "stdio",
                connected: true,
                tool_count: 2,
                tool_names: vec![
                    "mcp__filesystem__read".into(),
                    "mcp__filesystem__write".into(),
                ],
                error: None,
                needs_authorization: false,
            },
            McpServerStatus {
                name: "broken".into(),
                transport: "http",
                connected: false,
                tool_count: 0,
                tool_names: Vec::new(),
                error: Some("connection refused".into()),
                needs_authorization: false,
            },
        ]
    }

    #[test]
    fn mcp_servers_available_event_updates_the_server_list() {
        let mut state = AppState::default();
        state.apply(&AppEvent::McpServersAvailable(sample_mcp_servers()));
        assert_eq!(state.mcp_servers.len(), 2);
        assert_eq!(state.mcp_servers[0].name, "filesystem");
    }

    #[test]
    fn mcp_popup_opens_on_the_choose_an_action_menu_then_enter_shows_the_server_list() {
        let mut state = AppState {
            mcp_visible: true,
            mcp_servers: sample_mcp_servers(),
            ..AppState::default()
        };
        assert_eq!(state.mcp_view, McpView::Menu);

        assert!(matches!(
            handle_mcp_event(&mut state, &press(KeyCode::Enter)),
            McpEventOutcome::Handled
        ));
        assert_eq!(state.mcp_view, McpView::ServerList);
    }

    #[test]
    fn esc_on_the_mcp_menu_closes_the_whole_popup() {
        let mut state = AppState {
            mcp_visible: true,
            ..AppState::default()
        };
        assert!(matches!(
            handle_mcp_event(&mut state, &press(KeyCode::Esc)),
            McpEventOutcome::Handled
        ));
        assert!(!state.mcp_visible);
    }

    #[test]
    fn server_list_enter_connects_a_disconnected_server_and_disconnects_a_connected_one() {
        let mut state = AppState {
            mcp_visible: true,
            mcp_view: McpView::ServerList,
            mcp_servers: sample_mcp_servers(),
            mcp_selected: 0,
            ..AppState::default()
        };

        match handle_mcp_event(&mut state, &press(KeyCode::Enter)) {
            McpEventOutcome::Disconnect(name) => assert_eq!(name, "filesystem"),
            other => panic!("expected a disconnect outcome, got {other:?}"),
        }

        state.mcp_selected = 1;
        match handle_mcp_event(&mut state, &press(KeyCode::Enter)) {
            McpEventOutcome::Connect(name) => assert_eq!(name, "broken"),
            other => panic!("expected a connect outcome, got {other:?}"),
        }
    }

    #[test]
    fn server_list_navigation_stays_within_bounds() {
        let mut state = AppState {
            mcp_visible: true,
            mcp_view: McpView::ServerList,
            mcp_servers: sample_mcp_servers(),
            mcp_selected: 0,
            ..AppState::default()
        };

        assert!(matches!(
            handle_mcp_event(&mut state, &press(KeyCode::Up)),
            McpEventOutcome::Handled
        ));
        assert_eq!(state.mcp_selected, 0);

        handle_mcp_event(&mut state, &press(KeyCode::Down));
        handle_mcp_event(&mut state, &press(KeyCode::Down));
        assert_eq!(state.mcp_selected, 1);
    }

    #[test]
    fn right_arrow_opens_server_detail_then_esc_returns_to_the_list() {
        let mut state = AppState {
            mcp_visible: true,
            mcp_view: McpView::ServerList,
            mcp_servers: sample_mcp_servers(),
            mcp_selected: 0,
            ..AppState::default()
        };

        assert!(matches!(
            handle_mcp_event(&mut state, &press(KeyCode::Right)),
            McpEventOutcome::Handled
        ));
        assert_eq!(state.mcp_view, McpView::ServerDetail);

        assert!(matches!(
            handle_mcp_event(&mut state, &press(KeyCode::Esc)),
            McpEventOutcome::Handled
        ));
        assert_eq!(state.mcp_view, McpView::ServerList);
    }

    fn type_str(state: &mut AppState, text: &str) {
        for character in text.chars() {
            handle_mcp_event(state, &press(KeyCode::Char(character)));
        }
    }

    #[test]
    fn menu_down_selects_add_server_and_enter_opens_the_wizard_on_the_name_step() {
        let mut state = AppState {
            mcp_visible: true,
            ..AppState::default()
        };
        handle_mcp_event(&mut state, &press(KeyCode::Down));
        assert_eq!(state.mcp_selected, 1);

        assert!(matches!(
            handle_mcp_event(&mut state, &press(KeyCode::Enter)),
            McpEventOutcome::Handled
        ));
        assert_eq!(state.mcp_view, McpView::AddServer);
        assert_eq!(state.mcp_add_step, McpAddStep::Name);
    }

    #[test]
    fn add_server_wizard_builds_a_stdio_entry_with_no_auth() {
        let mut state = AppState {
            mcp_visible: true,
            mcp_view: McpView::AddServer,
            ..AppState::default()
        };

        type_str(&mut state, "filesystem");
        handle_mcp_event(&mut state, &press(KeyCode::Enter));
        assert_eq!(state.mcp_add_step, McpAddStep::TransportChoice);

        // Default transport is stdio; Enter accepts it without toggling.
        handle_mcp_event(&mut state, &press(KeyCode::Enter));
        assert_eq!(state.mcp_add_step, McpAddStep::CommandLine);

        type_str(&mut state, "npx -y server-filesystem /tmp");
        handle_mcp_event(&mut state, &press(KeyCode::Enter));
        assert_eq!(state.mcp_add_step, McpAddStep::AuthChoice);

        // Default auth is none; Enter submits immediately.
        match handle_mcp_event(&mut state, &press(KeyCode::Enter)) {
            McpEventOutcome::AddServer { entry, api_key } => {
                assert_eq!(entry.name, "filesystem");
                assert_eq!(
                    entry.transport,
                    gocode_core::McpTransportConfig::Stdio {
                        command: "npx".into(),
                        args: vec!["-y".into(), "server-filesystem".into(), "/tmp".into()],
                        env: std::collections::BTreeMap::new(),
                    }
                );
                assert_eq!(entry.auth, gocode_core::McpAuthConfig::None);
                assert!(api_key.is_none());
            }
            other => panic!("expected an AddServer outcome, got {other:?}"),
        }
        assert_eq!(state.mcp_view, McpView::Menu);
    }

    #[test]
    fn add_server_wizard_builds_an_http_entry_with_an_api_key() {
        let mut state = AppState {
            mcp_visible: true,
            mcp_view: McpView::AddServer,
            ..AppState::default()
        };

        type_str(&mut state, "remote");
        handle_mcp_event(&mut state, &press(KeyCode::Enter));

        handle_mcp_event(&mut state, &press(KeyCode::Right)); // toggle to http
        handle_mcp_event(&mut state, &press(KeyCode::Enter));
        assert_eq!(state.mcp_add_step, McpAddStep::Url);

        type_str(&mut state, "https://mcp.example.com/mcp");
        handle_mcp_event(&mut state, &press(KeyCode::Enter));
        assert_eq!(state.mcp_add_step, McpAddStep::AuthChoice);

        handle_mcp_event(&mut state, &press(KeyCode::Down)); // toggle to API key
        handle_mcp_event(&mut state, &press(KeyCode::Enter));
        assert_eq!(state.mcp_add_step, McpAddStep::ApiKey);

        type_str(&mut state, "secret-token");
        match handle_mcp_event(&mut state, &press(KeyCode::Enter)) {
            McpEventOutcome::AddServer { entry, api_key } => {
                assert_eq!(entry.name, "remote");
                assert_eq!(
                    entry.transport,
                    gocode_core::McpTransportConfig::Http {
                        url: "https://mcp.example.com/mcp".into(),
                        headers: std::collections::BTreeMap::new(),
                    }
                );
                assert_eq!(entry.auth, gocode_core::McpAuthConfig::ApiKey);
                assert_eq!(api_key.as_deref(), Some("secret-token"));
            }
            other => panic!("expected an AddServer outcome, got {other:?}"),
        }
        assert_eq!(state.mcp_view, McpView::Menu);
    }

    #[test]
    fn add_server_wizard_rejects_an_empty_name() {
        let mut state = AppState {
            mcp_visible: true,
            mcp_view: McpView::AddServer,
            ..AppState::default()
        };

        assert!(matches!(
            handle_mcp_event(&mut state, &press(KeyCode::Enter)),
            McpEventOutcome::Handled
        ));
        assert_eq!(state.mcp_add_step, McpAddStep::Name);
    }

    #[test]
    fn esc_on_the_name_step_returns_to_the_menu_without_side_effects() {
        let mut state = AppState {
            mcp_visible: true,
            mcp_view: McpView::AddServer,
            ..AppState::default()
        };
        type_str(&mut state, "abandoned");

        assert!(matches!(
            handle_mcp_event(&mut state, &press(KeyCode::Esc)),
            McpEventOutcome::Handled
        ));
        assert_eq!(state.mcp_view, McpView::Menu);
    }

    #[test]
    fn pressing_o_authorizes_a_server_that_needs_it_and_is_a_no_op_otherwise() {
        let mut servers = sample_mcp_servers();
        servers[1].needs_authorization = true; // "broken", currently disconnected
        let mut state = AppState {
            mcp_visible: true,
            mcp_view: McpView::ServerList,
            mcp_servers: servers,
            mcp_selected: 0,
            ..AppState::default()
        };

        // "filesystem" (index 0) doesn't need authorization.
        assert!(matches!(
            handle_mcp_event(&mut state, &press(KeyCode::Char('o'))),
            McpEventOutcome::Handled
        ));

        state.mcp_selected = 1;
        match handle_mcp_event(&mut state, &press(KeyCode::Char('o'))) {
            McpEventOutcome::Authorize(name) => assert_eq!(name, "broken"),
            other => panic!("expected an authorize outcome, got {other:?}"),
        }
    }

    #[test]
    fn authorization_url_ready_event_shows_the_url_in_the_transcript() {
        let mut state = AppState::default();
        state.apply(&AppEvent::McpAuthorizationUrlReady {
            server: "remote".into(),
            url: "https://example.com/authorize?state=abc".into(),
        });
        assert!(
            matches!(state.entries.last(), Some(ChatEntry::Info(text)) if text.contains("https://example.com/authorize"))
        );
    }
}
