//! Terminal user interface for Gocode.

use crossterm::event;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use gocode_core::{AgentActivityState, AppCommand, AppEvent, PermissionMode, ToolActivityStatus};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
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
    /// Show the resolved model, provider, and reasoning effort.
    Status,
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
        "/status",
        "Show the resolved model, provider, and reasoning effort",
        SlashCommand::Status,
    ),
    ("/clear", "Clear the conversation view", SlashCommand::Clear),
    ("/help", "List available commands", SlashCommand::Help),
    ("/exit", "Exit Gocode", SlashCommand::Exit),
    ("/quit", "Exit Gocode", SlashCommand::Exit),
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
    /// Char index (not byte index) of the composer's insertion point.
    cursor: usize,
    suggestion_selected: usize,
    permission_mode: PermissionMode,
    selection: Option<Selection>,
    copy_notification: Option<String>,
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
        self.set_chat_input(suggestions[index].0.to_string());
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
        let Some(target_line) = line.checked_add_signed(delta).filter(|line| *line < lines.len())
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
    let model = state.current_model.as_deref().unwrap_or("no model selected");
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

/// Splits the chat screen into its history and composer areas. Shared by rendering and mouse
/// hit-testing so both agree exactly on where the history viewport sits.
fn chat_layout(area: Rect, state: &AppState) -> (Rect, Rect) {
    let suggestions = slash_suggestions(&state.chat_input);
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
    let suggestions = slash_suggestions(&state.chat_input);
    let (history_area, composer_area) = chat_layout(area, state);

    render_history(frame, state, history_area);
    render_composer(frame, state, composer_area, &suggestions);

    if let Some(message) = &state.copy_notification {
        render_copy_notification(frame, message, history_area);
    }

    if let Some(prompt) = &state.pending_permission {
        render_permission_modal(frame, prompt, area);
    } else if let Some(prompt) = &state.pending_update {
        render_update_modal(frame, prompt, area);
    } else if let Some(message) = &state.blocking_error {
        render_blocking_error_modal(frame, message, area);
    }
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
    let first = selected.saturating_sub(rows.saturating_sub(1)).min(count - rows);
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
fn selected_char_range(selection: &Selection, line_index: usize, line_len: usize) -> Option<(usize, usize)> {
    let (start, end) = selection.normalized();
    if line_index < start.line || line_index > end.line {
        return None;
    }
    let from = if line_index == start.line { start.col.min(line_len) } else { 0 };
    let to = if line_index == end.line { end.col.min(line_len) } else { line_len };
    (from < to).then_some((from, to))
}

fn render_composer(frame: &mut Frame, state: &AppState, area: Rect, suggestions: &[(&str, &str)]) {
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

        if let Some(mode) = handle_permission_mode_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::SetPermissionMode(mode))?;
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
                ChatSubmission::Command(SlashCommand::Status) => {
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
                        "Commands: /model /settings /provider /status /clear /help /exit /quit"
                            .into(),
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
        KeyCode::Enter => return state.selected_model(),
        KeyCode::Esc if state.current_model.is_some() => state.screen = Screen::Chat,
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
    Some(char_index_from_line_col(&input_lines, local_row, col_in_line))
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
                    point_from_terminal_coords(state, terminal_area, event.column, event.row)
                        .map(|point| Selection {
                            anchor: point,
                            cursor: point,
                        });
            }
            true
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(point) = point_from_terminal_coords(state, terminal_area, event.column, event.row)
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
    for (line_index, line) in wrapped.iter().enumerate().take(end.line + 1).skip(start.line) {
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
fn try_copy_selection(state: &mut AppState, terminal_area: Rect, notification_deadline: &mut Option<Instant>) {
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

    let suggestion_count = slash_suggestions(&state.chat_input).len();

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
            state.suggestion_selected =
                (state.suggestion_selected + 1).min(suggestion_count - 1);
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
            let suggestions = slash_suggestions(&state.chat_input);
            if !suggestions.is_empty() {
                let index = state.suggestion_selected.min(suggestions.len() - 1);
                let (name, _) = suggestions[index];
                if let Some(command) = resolve_slash_command(name) {
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(command));
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
    use gocode_core::{AgentActivityState, AppEvent, ErrorSeverity, ToolActivityStatus};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    use super::{
        AppState, ChatEntry, ChatSubmission, InputAction, MAX_VISIBLE_SUGGESTIONS, Screen,
        SlashCommand, classify_event, handle_chat_event, handle_onboarding_event,
        handle_permission_event, handle_update_event, render, run_with_event_source,
        slash_suggestions,
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
        assert!(empty_lines.iter().any(|line| line.contains("GOCODE") || line.contains('█')));

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
        assert_eq!(slash_suggestions(&state.chat_input).len(), 2);

        let _ = handle_chat_event(&mut state, &press(KeyCode::Down));
        assert_eq!(state.suggestion_selected, 1);

        let _ = handle_chat_event(&mut state, &press(KeyCode::Tab));
        assert_eq!(state.chat_input, slash_suggestions("/s")[1].0);
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
        assert_eq!(slash_suggestions("/s").len(), 2);
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
        assert_eq!(state.cursor, super::char_index_from_line_col(&["first", "second"], 1, 3));

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
        assert_eq!(state.cursor, super::char_index_from_line_col(&["first", "second"], 0, 2));

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
        let total = slash_suggestions("/").len();
        assert!(total >= 6, "test assumes at least 6 commands share the '/' prefix");

        let (start, end, truncated) = super::visible_suggestion_window(total, 0);
        assert!(truncated);
        assert_eq!(end - start, MAX_VISIBLE_SUGGESTIONS - 1);

        state.suggestion_selected = total - 1;
        let (start, end, truncated) = super::visible_suggestion_window(total, total - 1);
        assert!(truncated);
        assert_eq!(end, total);
        assert!(end - start <= MAX_VISIBLE_SUGGESTIONS - 1);
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

        assert_eq!(
            super::selected_char_range(&selection, 0, 10),
            Some((5, 10))
        );
        assert_eq!(
            super::selected_char_range(&selection, 1, 10),
            Some((0, 10))
        );
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
        let (start, _end) = super::compute_visible_window(wrapped.len(), visible_rows, state.scroll);
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
}
