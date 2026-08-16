//! Terminal user interface for Gocode.

use crossterm::event;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use gocode_core::{AgentActivityState, AppCommand, AppEvent, ToolActivityStatus};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::time::Duration;
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

/// A user-approved update prompt, delayed until it cannot interrupt work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePrompt {
    pub version: String,
    pub notes: String,
}

/// A parsed slash command handled entirely by the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommand {
    /// Reopen the model picker.
    Model,
    /// Open the settings menu to change the API key, model, or reasoning effort.
    Settings,
    /// Show the active provider.
    Provider,
    /// Show the resolved model and provider.
    Config,
    /// Clear the conversation view.
    Clear,
    /// List available commands.
    Help,
    /// Exit Gocode.
    Exit,
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
        "/config",
        "Show the resolved model and provider",
        SlashCommand::Config,
    ),
    ("/clear", "Clear the conversation view", SlashCommand::Clear),
    ("/help", "List available commands", SlashCommand::Help),
    ("/exit", "Exit Gocode", SlashCommand::Exit),
];

/// Slash-command suggestions matching the current composer prefix.
#[must_use]
pub fn slash_suggestions(input: &str) -> Vec<(&'static str, &'static str)> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    SLASH_COMMANDS
        .iter()
        .filter(|(name, _, _)| name.starts_with(input))
        .map(|(name, description, _)| (*name, *description))
        .collect()
}

fn resolve_slash_command(input: &str) -> Option<SlashCommand> {
    SLASH_COMMANDS
        .iter()
        .find(|(name, _, _)| *name == input)
        .map(|(_, _, command)| *command)
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
    suggestion_selected: usize,
    entries: Vec<ChatEntry>,
    streaming_assistant: bool,
    file_change_buffer: Vec<String>,
    activity: Option<AgentActivityState>,
    pending_permission: Option<PermissionPrompt>,
    pending_update: Option<UpdatePrompt>,
    queued_update: Option<UpdatePrompt>,
    exit_for_update: bool,
    blocking_error: Option<String>,
    status: Option<String>,
    scroll: usize,
    last_failed_prompt: Option<String>,
    last_submitted_prompt: Option<String>,
    queued: Option<String>,
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
            AppEvent::UpdateAvailable { version, notes } => {
                let prompt = UpdatePrompt {
                    version: version.clone(),
                    notes: notes.clone(),
                };
                if self.screen == Screen::Chat
                    && self.activity.is_none()
                    && self.pending_permission.is_none()
                {
                    self.pending_update = Some(prompt);
                } else {
                    self.queued_update = Some(prompt);
                }
            }
            AppEvent::UpdateProgress(message) => self.status = Some(message.clone()),
            AppEvent::UpdateFailed(message) => self.entries.push(ChatEntry::Error(message.clone())),
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
            } => {
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
            AppEvent::ReasoningEffortChanged(effort) => {
                self.current_effort.clone_from(effort);
                self.screen = Screen::Chat;
                let label = effort_label(effort.as_deref());
                self.entries
                    .push(ChatEntry::Info(format!("Reasoning effort set to: {label}")));
                self.status = Some(format!("Reasoning effort: {label}"));
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
        {
            self.pending_update = self.queued_update.take();
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
        let suggestions = slash_suggestions(&self.chat_input);
        if suggestions.is_empty() {
            return;
        }
        let index = self.suggestion_selected.min(suggestions.len() - 1);
        self.chat_input = suggestions[index].0.to_string();
        self.suggestion_selected = 0;
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

fn compose_lines(state: &AppState) -> Vec<String> {
    if state.entries.is_empty() {
        return vec![
            "What can I help you build?".into(),
            String::new(),
            "Prompts and the project context you share are sent to NVIDIA NIM for inference."
                .into(),
        ];
    }

    let mut lines = Vec::new();
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

fn render_chat(frame: &mut Frame, state: &AppState, area: Rect) {
    let suggestions = slash_suggestions(&state.chat_input);
    let input_lines = 1 + state.chat_input.matches('\n').count();
    let suggestion_lines = suggestions.len().min(4);
    let compose_height = u16::try_from(input_lines + suggestion_lines + 2).unwrap_or(u16::MAX);
    let compose_height = compose_height.min(area.height.saturating_sub(3)).max(3);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(compose_height)])
        .split(area);

    render_history(frame, state, layout[0]);
    render_composer(frame, state, layout[1], &suggestions);

    if let Some(prompt) = &state.pending_permission {
        render_permission_modal(frame, prompt, area);
    } else if let Some(prompt) = &state.pending_update {
        render_update_modal(frame, prompt, area);
    } else if let Some(message) = &state.blocking_error {
        render_blocking_error_modal(frame, message, area);
    }
}

fn render_update_modal(frame: &mut Frame, prompt: &UpdatePrompt, area: Rect) {
    let modal = centered(area, 64, 9);
    let notes = prompt.notes.lines().next().unwrap_or_default();
    let content = format!(
        "Gocode {} is available.\n\n{}\n\n[y] Update now   [n] Not now",
        prompt.version, notes
    );
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(content).wrap(Wrap { trim: false }).block(
            Block::default()
                .title("Update available")
                .borders(Borders::ALL),
        ),
        modal,
    );
}

fn render_history(frame: &mut Frame, state: &AppState, area: Rect) {
    let lines = compose_lines(state);
    let visible_rows = usize::from(area.height.saturating_sub(2)).max(1);
    let total = lines.len();
    let max_scroll = total.saturating_sub(visible_rows);
    let scroll = state.scroll.min(max_scroll);
    let end = total.saturating_sub(scroll);
    let start = end.saturating_sub(visible_rows);
    let visible = lines[start..end].join("\n");

    let title = if state.is_scroll_locked() {
        "Gocode · Chat"
    } else {
        "Gocode · Chat (scrolled — End to follow)"
    };
    frame.render_widget(
        Paragraph::new(visible)
            .wrap(Wrap { trim: false })
            .block(Block::default().title(title).borders(Borders::ALL)),
        area,
    );
}

fn render_composer(frame: &mut Frame, state: &AppState, area: Rect, suggestions: &[(&str, &str)]) {
    let mut lines = vec![Line::from(format!("> {}", state.chat_input))];

    let selected = state
        .suggestion_selected
        .min(suggestions.len().saturating_sub(1));
    for (index, (name, description)) in suggestions.iter().enumerate() {
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
    if let Some(status) = &state.status {
        lines.push(Line::from(status.clone()));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
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
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let _terminal_guard = TerminalGuard;
    let mut state = AppState::default();

    let send_command = |command_tx: &mpsc::Sender<AppCommand>, command: AppCommand| {
        command_tx.blocking_send(command).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                format!("runtime command channel closed: {error}"),
            )
        })
    };

    loop {
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

        if let Some(approved) = handle_permission_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::PermissionResponse(approved))?;
            continue;
        }

        if let Some(approved) = handle_update_event(&mut state, &terminal_event) {
            send_command(
                &command_tx,
                if approved {
                    AppCommand::AcceptUpdate
                } else {
                    AppCommand::RejectUpdate
                },
            )?;
            continue;
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

        if let Some(effort) = handle_effort_picker_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::SetReasoningEffort(effort))?;
            continue;
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
                }
                ChatSubmission::Command(SlashCommand::Model) => {
                    state.screen = Screen::ModelPicker;
                    state.selected_model = 0;
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
                ChatSubmission::Command(SlashCommand::Config) => {
                    let model = state
                        .current_model
                        .clone()
                        .unwrap_or_else(|| "none selected".into());
                    let effort = effort_label(state.current_effort.as_deref());
                    state.entries.push(ChatEntry::Info(format!(
                        "Provider: NVIDIA NIM · Model: {model} · Reasoning effort: {effort}"
                    )));
                }
                ChatSubmission::Command(SlashCommand::Help) => {
                    state.entries.push(ChatEntry::Info(
                        "Commands: /model /settings /provider /config /clear /help /exit".into(),
                    ));
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
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

/// Installs a panic hook that restores the terminal before Rust prints the panic report.
///
/// Call this once during application bootstrap, before entering terminal mode.
pub fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
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
        KeyCode::Enter => return state.selected_model(),
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

/// Applies Y/N confirmation keys to a pending update prompt.
#[must_use]
pub fn handle_update_event(state: &mut AppState, event: &Event) -> Option<bool> {
    state.pending_update.as_ref()?;
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
            state.pending_update = None;
            Some(true)
        }
        KeyCode::Char('n' | 'N') | KeyCode::Esc => {
            state.pending_update = None;
            Some(false)
        }
        _ => None,
    }
}

/// Applies text input, navigation, and control keys to the chat composer.
///
/// Returns a submission on Enter: a prompt for the model, or a recognized slash command.
#[must_use]
pub fn handle_chat_event(state: &mut AppState, event: &Event) -> Option<ChatSubmission> {
    if state.screen != Screen::Chat
        || state.blocking_error.is_some()
        || state.pending_permission.is_some()
        || state.pending_update.is_some()
    {
        return None;
    }

    if let Event::Paste(text) = event {
        state.chat_input.push_str(text);
        state.suggestion_selected = 0;
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

    let suggestion_count = slash_suggestions(&state.chat_input).len();

    match code {
        KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.toggle_last_tool_output();
        }
        KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(prompt) = state.last_failed_prompt.take() {
                state.chat_input = prompt;
                state.suggestion_selected = 0;
            }
        }
        KeyCode::Up if suggestion_count > 0 => {
            state.suggestion_selected = state.suggestion_selected.saturating_sub(1);
        }
        KeyCode::Down if suggestion_count > 0 => {
            state.suggestion_selected =
                (state.suggestion_selected + 1).min(suggestion_count - 1);
        }
        KeyCode::PageUp => state.scroll_up(),
        KeyCode::PageDown | KeyCode::End => state.scroll_down(),
        KeyCode::Tab => state.autocomplete(),
        KeyCode::Enter if modifiers.contains(KeyModifiers::ALT) => {
            state.chat_input.push('\n');
        }
        KeyCode::Char(character)
            if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            state.chat_input.push(*character);
            state.suggestion_selected = 0;
        }
        KeyCode::Backspace => {
            state.chat_input.pop();
            state.suggestion_selected = 0;
        }
        KeyCode::Enter => {
            let trimmed = state.chat_input.trim();
            if trimmed.is_empty() {
                return None;
            }
            let suggestions = slash_suggestions(&state.chat_input);
            if !suggestions.is_empty() {
                let index = state.suggestion_selected.min(suggestions.len() - 1);
                let (name, _) = suggestions[index];
                if let Some(command) = resolve_slash_command(name) {
                    state.chat_input.clear();
                    state.suggestion_selected = 0;
                    return Some(ChatSubmission::Command(command));
                }
            }
            let text = std::mem::take(&mut state.chat_input);
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
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use gocode_core::{AgentActivityState, AppEvent, ErrorSeverity, ToolActivityStatus};
    use ratatui::{Terminal, backend::TestBackend};

    use super::{
        AppState, ChatEntry, ChatSubmission, InputAction, Screen, SlashCommand, classify_event,
        handle_chat_event, handle_onboarding_event, handle_permission_event, handle_update_event,
        render, run_with_event_source, slash_suggestions,
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
    fn update_waits_for_the_active_run_then_requires_explicit_confirmation() {
        let mut state = AppState {
            screen: Screen::Chat,
            activity: Some(AgentActivityState::Thinking),
            ..AppState::default()
        };
        state.apply(&AppEvent::UpdateAvailable {
            version: "0.0.2".into(),
            notes: "A safer updater.".into(),
        });
        assert!(state.pending_update.is_none());
        state.apply(&AppEvent::AgentCompleted {
            final_text: None,
            turns: 1,
            tool_calls: 0,
            failed_tool_calls: 0,
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
            Some(false)
        );
        assert!(state.pending_update.is_none());
    }

    #[test]
    fn chat_screen_renders_the_initial_prompt_and_privacy_disclosure() {
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
        assert!(content.contains("sent to NVIDIA NIM"));
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
            chat_input: "/c".into(),
            ..AppState::default()
        };
        assert_eq!(slash_suggestions(&state.chat_input).len(), 2);

        let _ = handle_chat_event(&mut state, &press(KeyCode::Down));
        assert_eq!(state.suggestion_selected, 1);

        let _ = handle_chat_event(&mut state, &press(KeyCode::Tab));
        assert_eq!(state.chat_input, slash_suggestions("/c")[1].0);
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
            chat_input: "/c".into(),
            ..AppState::default()
        };
        let _ = handle_chat_event(&mut state, &press(KeyCode::Down));
        assert_eq!(state.suggestion_selected, 1);

        let _ = handle_chat_event(&mut state, &press(KeyCode::Char('o')));
        assert_eq!(state.suggestion_selected, 0);
    }

    #[test]
    fn slash_suggestions_filter_by_prefix() {
        assert_eq!(slash_suggestions("/c").len(), 2);
        assert!(slash_suggestions("hello").is_empty());
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
}
