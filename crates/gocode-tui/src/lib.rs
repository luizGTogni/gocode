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
use std::collections::BTreeSet;
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

#[cfg(test)]
mod preference_tests {
    use super::*;

    #[test]
    fn themes_expose_distinct_semantic_tokens_and_no_color_is_safe() {
        assert_ne!(
            theme_tokens(gocode_core::ThemeName::Light, true).background,
            theme_tokens(gocode_core::ThemeName::Dark, true).background
        );
        let fallback = theme_tokens(gocode_core::ThemeName::HighContrast, false);
        assert_eq!(fallback.primary, Color::Reset);
        assert_eq!(fallback.danger, Color::Reset);
    }

    #[test]
    fn help_includes_preference_commands() {
        let state = AppState::default();
        let help = help_tab_content(&state, HelpTab::Commands);
        assert!(help.contains("/keymap"));
        assert!(help.contains("/theme"));
        assert!(help.contains("/personality"));
    }

    #[test]
    fn parses_debug_description_and_auxiliary_commands() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..Default::default()
        };
        state.set_chat_input("/debug login fails after refresh".into());
        assert_eq!(
            handle_chat_event(
                &mut state,
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            ),
            Some(ChatSubmission::Command(SlashCommand::Debug(
                "login fails after refresh".into()
            )))
        );
        state.set_chat_input("/debug status".into());
        assert_eq!(
            handle_chat_event(
                &mut state,
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            ),
            Some(ChatSubmission::Command(SlashCommand::Debug(
                "status".into()
            )))
        );
    }

    #[test]
    fn guided_question_selects_recommended_choice_and_returns_its_label() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..Default::default()
        };
        state.apply(&AppEvent::GuidedQuestionRequested(
            gocode_core::GuidedQuestion {
                title: "How should I proceed?".into(),
                context: "Two valid approaches were found.".into(),
                choices: vec![
                    gocode_core::GuidedChoice {
                        label: "Minimal change".into(),
                        summary: "Reuse the existing module.".into(),
                        advantages: "Lower risk.".into(),
                        disadvantages: "Less flexible.".into(),
                        recommended: true,
                    },
                    gocode_core::GuidedChoice {
                        label: "New module".into(),
                        summary: "Create an isolated abstraction.".into(),
                        advantages: "More flexible.".into(),
                        disadvantages: "More code.".into(),
                        recommended: false,
                    },
                ],
            },
        ));

        assert_eq!(state.selected_guided_choice, 0);
        assert_eq!(
            handle_guided_question_event(
                &mut state,
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ),
            Some("Minimal change".into())
        );
        assert!(state.pending_guided_question.is_none());
    }
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

/// Semantic colours consumed by renderers; named themes are centralized here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeTokens {
    pub background: Color,
    pub primary: Color,
    pub secondary: Color,
    pub border: Color,
    pub highlight: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub command: Color,
    pub diff_add: Color,
    pub diff_remove: Color,
    pub approval: Color,
    pub danger: Color,
}

#[must_use]
pub fn theme_tokens(theme: gocode_core::ThemeName, colors_supported: bool) -> ThemeTokens {
    if !colors_supported {
        return ThemeTokens {
            background: Color::Reset,
            primary: Color::Reset,
            secondary: Color::Reset,
            border: Color::Reset,
            highlight: Color::Reset,
            success: Color::Reset,
            warning: Color::Reset,
            error: Color::Reset,
            command: Color::Reset,
            diff_add: Color::Reset,
            diff_remove: Color::Reset,
            approval: Color::Reset,
            danger: Color::Reset,
        };
    }
    match theme {
        gocode_core::ThemeName::Light => ThemeTokens {
            background: Color::White,
            primary: Color::Black,
            secondary: Color::DarkGray,
            border: Color::Gray,
            highlight: Color::Blue,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            command: Color::Blue,
            diff_add: Color::Green,
            diff_remove: Color::Red,
            approval: Color::Green,
            danger: Color::Red,
        },
        gocode_core::ThemeName::HighContrast => ThemeTokens {
            background: Color::Black,
            primary: Color::White,
            secondary: Color::White,
            border: Color::White,
            highlight: Color::Yellow,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            command: Color::Cyan,
            diff_add: Color::Green,
            diff_remove: Color::Red,
            approval: Color::Green,
            danger: Color::Red,
        },
        gocode_core::ThemeName::Dark | gocode_core::ThemeName::System => ThemeTokens {
            background: Color::Black,
            primary: Color::White,
            secondary: Color::DarkGray,
            border: Color::Gray,
            highlight: Color::Rgb(120, 170, 230),
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            command: Color::Cyan,
            diff_add: Color::Green,
            diff_remove: Color::Red,
            approval: Color::Green,
            danger: Color::Red,
        },
    }
}

fn active_theme(state: &AppState) -> ThemeTokens {
    let colors_supported = std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map_or(true, |term| term != "dumb");
    let theme = if state.preferences.theme == gocode_core::ThemeName::System {
        // COLORFGBG's final value is the terminal background; high ANSI values conventionally
        // mean a light background. Absence is intentionally a dark, readable fallback.
        std::env::var("COLORFGBG")
            .ok()
            .and_then(|value| value.rsplit(';').next()?.parse::<u8>().ok())
            .filter(|background| *background >= 8)
            .map_or(gocode_core::ThemeName::Dark, |_| {
                gocode_core::ThemeName::Light
            })
    } else {
        state.preferences.theme
    };
    theme_tokens(theme, colors_supported)
}

// Transitional alias used by legacy selection widgets; new rendering code resolves semantic
// roles from `ThemeTokens` instead of introducing literal colours.
const SELECTION_COLOR: Color = Color::Rgb(120, 170, 230);

/// Maximum slash-command suggestion rows shown at once, so a large command list can never grow
/// the composer without bound; the list scrolls to keep the highlighted entry in view instead.
const MAX_VISIBLE_SUGGESTIONS: usize = 6;

/// Maximum prompt history entries remembered for Up/Down recall.
const MAX_PROMPT_HISTORY: usize = 200;

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
    /// Reasoning/"thinking" text a reasoning-capable model streamed separately from its final
    /// answer this turn. Shown so a turn that ends without visible answer text isn't silently
    /// empty, but never sent back as conversation history.
    Reasoning(String),
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

/// The human-readable stage of one active agent run. This is intentionally derived from visible
/// work rather than provider internals, so the same experience is available for every provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RunPhase {
    #[default]
    Understanding,
    Changing,
    Validating,
    Recovering,
    Finalizing,
}

impl RunPhase {
    const fn label(self) -> &'static str {
        match self {
            Self::Understanding => "Entendendo",
            Self::Changing => "Alterando",
            Self::Validating => "Validando",
            Self::Recovering => "Recuperando",
            Self::Finalizing => "Finalizando",
        }
    }
}

/// Compact, live run summary displayed above the transcript while the agent is active.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag records an independently visible piece of run evidence"
)]
struct RunVisibility {
    phase: RunPhase,
    current_action: Option<String>,
    started_at: Option<Instant>,
    changed_files: BTreeSet<String>,
    explored: bool,
    changed: bool,
    validation_pending: bool,
    validation_succeeded: bool,
}

impl RunVisibility {
    fn start(&mut self) {
        *self = Self::default();
        self.started_at = Some(Instant::now());
    }

    fn observe_tool(&mut self, name: &str, status: ToolActivityStatus, detail: &str) {
        self.current_action = Some(if detail.is_empty() || detail == "running" {
            human_tool_name(name).to_string()
        } else {
            detail.to_string()
        });

        if status == ToolActivityStatus::Failed {
            self.phase = RunPhase::Recovering;
            return;
        }
        match name {
            "read_file" | "search" | "list_files" => {
                self.explored = true;
                if !self.changed {
                    self.phase = RunPhase::Understanding;
                }
            }
            "write_file" | "apply_patch" => {
                self.changed = true;
                self.validation_pending = true;
                self.phase = RunPhase::Changing;
            }
            "run_command" => {
                if is_validation_command(detail) {
                    self.phase = RunPhase::Validating;
                    if status == ToolActivityStatus::Succeeded {
                        self.validation_pending = false;
                        self.validation_succeeded = true;
                    } else {
                        self.validation_pending = true;
                    }
                } else {
                    self.phase = RunPhase::Understanding;
                }
            }
            _ => {}
        }
    }

    fn observe_file_change(&mut self, path: &str) {
        self.changed = true;
        self.validation_pending = true;
        self.changed_files.insert(path.to_string());
    }

    fn plan_line(&self) -> String {
        let understand = if self.explored {
            "✓ Entender"
        } else {
            "○ Entender"
        };
        let change = if self.changed {
            "✓ Alterar"
        } else {
            "○ Alterar"
        };
        let validate = if self.phase == RunPhase::Validating {
            "● Validar"
        } else if self.validation_succeeded {
            "✓ Validar"
        } else {
            "○ Validar"
        };
        format!("Plano: {understand} → {change} → {validate}")
    }

    fn impact_line(&self) -> String {
        let files = self.changed_files.len();
        let file_label = if files == 1 {
            "arquivo alterado"
        } else {
            "arquivos alterados"
        };
        let validation = if self.validation_pending {
            "validação pendente"
        } else if self.validation_succeeded {
            "validação concluída"
        } else {
            "sem validação necessária"
        };
        format!("Impacto: {files} {file_label} · {validation}")
    }

    fn elapsed_label(&self) -> String {
        let seconds = self
            .started_at
            .map_or(0, |started_at| started_at.elapsed().as_secs());
        format!("{seconds}s")
    }

    fn completion_summary(&mut self, partial: bool) -> String {
        self.phase = RunPhase::Finalizing;
        let outcome = if partial {
            "interrompida"
        } else {
            "concluída"
        };
        format!(
            "Entrega — execução {outcome}\n  {}\n  {}",
            self.impact_line(),
            if self.validation_pending {
                "Próximo passo: validar as alterações antes de considerar a tarefa concluída."
            } else {
                "Próximo passo: revisar o resumo e o diff, se desejar."
            }
        )
    }
}

fn human_tool_name(name: &str) -> &str {
    match name {
        "read_file" => "Lendo arquivo",
        "search" => "Buscando no projeto",
        "list_files" => "Listando arquivos",
        "write_file" => "Editando arquivo",
        "apply_patch" => "Aplicando alteração",
        "run_command" => "Executando validação",
        "ask_user" => "Aguardando sua decisão",
        _ => "Executando ação",
    }
}

fn is_validation_command(detail: &str) -> bool {
    [
        " test", " check", " clippy", " fmt", " lint", " build", "pytest", "ruff", "tsc", "mypy",
        " vet",
    ]
    .iter()
    .any(|marker| detail.contains(marker))
}

/// A pending permission confirmation shown as a modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionPrompt {
    /// Short summary of the requested action.
    pub summary: String,
    /// Working directory the action would run or write in.
    pub working_directory: String,
    /// The action category affected by “allow always”.
    pub scope_label: String,
}

/// An `/undo` or `/redo` that stopped because a transaction's files no longer match, awaiting
/// the user's choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingUndoConflict {
    /// `"undo"` or `"redo"`, echoed back on a forced retry.
    direction: String,
    /// The transaction count originally requested, echoed back on a forced retry.
    count: usize,
    /// Transactions that already applied before the conflicting one was reached.
    applied: Vec<gocode_core::UndoTransactionResult>,
    /// Every file that blocked the conflicting transaction.
    conflicting_files: Vec<gocode_core::UndoConflictFile>,
    /// Whether the diff for `conflicting_files` is currently expanded.
    show_diff: bool,
}

/// An `/agent apply <id>` or `/agent cleanup <id>` awaiting explicit Y/N confirmation before the
/// merge or removal is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingAgentConfirm {
    /// Merge the subagent's worktree branch into the main workspace. `diff` is the already
    /// computed `git diff`, shown for review before the user decides.
    Apply { id: String, diff: String },
    /// Remove the subagent's worktree and metadata. `message` names what will be discarded.
    Cleanup { id: String, message: String },
}

/// Which side of a merge conflict to keep for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictResolution {
    /// Keep the main workspace's version.
    Ours,
    /// Keep the subagent's version.
    Theirs,
}

/// One conflicting file in an in-progress `/agent apply` merge, and its resolution so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictFileState {
    /// Path relative to the repository root.
    pub path: String,
    /// `None` until the user picks a side for this file.
    pub resolution: Option<ConflictResolution>,
}

/// An in-progress `/agent apply` merge conflict, driving the guided resolver popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAgentConflict {
    /// The subagent whose merge is in progress.
    pub id: String,
    /// Every conflicting file and its resolution so far.
    pub files: Vec<ConflictFileState>,
    /// Selected row in the file list.
    pub selected: usize,
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
    /// Manage keyboard bindings.
    Keymap(String),
    /// Manage named semantic palettes.
    Theme(String),
    /// Manage response presentation style.
    Personality(String),
    /// Reopen the model picker.
    Model,
    /// Reopen the reasoning-effort picker directly, without going through the model picker.
    Effort,
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
    /// Branch this session into a new one with the same history, leaving this session untouched.
    ForkSession,
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
    /// Create, list, switch to, or remove an isolated Git worktree. The `String` is the raw text
    /// typed after `/worktree` (e.g. `list`, `switch my-task`, `remove my-task`, `my-task`, or
    /// `my-task existing-branch`); empty suggests a name for the user to edit and confirm.
    Worktree(String),
    /// Undo the last `n` agent-edit transactions in the current worktree. The `String` is the
    /// raw text typed after `/undo` (a positive integer, or empty for `1`).
    Undo(String),
    /// Redo the last `n` transactions undone with `/undo`. The `String` is the raw text typed
    /// after `/redo` (a positive integer, or empty for `1`).
    Redo(String),
    /// Start, resume, inspect, stop, or summarize a guided bug investigation.
    Debug(String),
    /// Spawn, message, stop, or apply a subagent. The `String` is the raw text typed after
    /// `/agent` (e.g. `spawn investigate flaky login test --mode research`, `status a1b2c3d4`,
    /// `message a1b2c3d4 focus on the retry path`, `apply a1b2c3d4`, `apply a1b2c3d4 confirm`).
    Agent(String),
    /// List active and recent subagents.
    Agents,
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

/// Which screen the `/agents` popup is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum AgentsView {
    /// Every known subagent, most recently updated first.
    #[default]
    List,
    /// One subagent's full status, messages, and result.
    Detail,
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
    (
        "/keymap",
        "List or change keyboard shortcuts",
        SlashCommand::Keymap(String::new()),
    ),
    (
        "/theme",
        "List or change the visual theme",
        SlashCommand::Theme(String::new()),
    ),
    (
        "/personality",
        "List or change the response style",
        SlashCommand::Personality(String::new()),
    ),
    ("/model", "Switch the active model", SlashCommand::Model),
    (
        "/effort",
        "Change the reasoning effort level",
        SlashCommand::Effort,
    ),
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
    (
        "/fork",
        "Branch this conversation into a new session, leaving this one untouched",
        SlashCommand::ForkSession,
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
    (
        "/worktree",
        "Create, list, switch to, or remove an isolated Git worktree",
        SlashCommand::Worktree(String::new()),
    ),
    (
        "/undo",
        "Undo the agent's last edit (or `/undo n` for the last n)",
        SlashCommand::Undo(String::new()),
    ),
    (
        "/redo",
        "Redo the last undone edit (or `/redo n` for the last n)",
        SlashCommand::Redo(String::new()),
    ),
    (
        "/debug",
        "Investigate a bug safely (`status`, `stop`, or `summary`)",
        SlashCommand::Debug(String::new()),
    ),
    (
        "/agent",
        "Spawn, message, stop, or apply a subagent (`spawn`, `status`, `message`, `stop`, \
         `result`, `apply`, `cleanup`)",
        SlashCommand::Agent(String::new()),
    ),
    (
        "/agents",
        "List active and recent subagents",
        SlashCommand::Agents,
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

fn debug_status(debug: &gocode_core::DebugInvestigation) -> String {
    if !debug.is_active() {
        return "Nenhuma investigação /debug ativa.".into();
    }
    let evidence = if debug.evidence.is_empty() {
        "nenhuma"
    } else {
        "coletada"
    };
    format!(
        "Debug — Investigando\nHipótese atual: {}\nEvidências: {evidence}\nComandos executados: {}\n{}",
        debug.hypothesis.as_deref().unwrap_or("ainda não definida"),
        debug.commands.len(),
        debug.next_question().map_or_else(
            || "Triagem concluída; aguardando investigação.".into(),
            |question| format!("Informação pendente: {question}"),
        )
    )
}

fn debug_summary(debug: &gocode_core::DebugInvestigation) -> String {
    let description = debug
        .description
        .as_deref()
        .unwrap_or("Fluxo guiado /debug");
    format!(
        "Debug summary\nProblema: {description}\nHipótese: {}\nEvidências: {}\nComandos: {}\nEstado: {}",
        debug.hypothesis.as_deref().unwrap_or("não confirmada"),
        if debug.evidence.is_empty() {
            "nenhuma"
        } else {
            "coletadas"
        },
        if debug.commands.is_empty() {
            "nenhum"
        } else {
            "registrados"
        },
        if debug.stopped {
            "interrompido"
        } else if debug.is_active() {
            "em andamento"
        } else {
            "não iniciado"
        },
    )
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
    /// One answer to `/debug`'s guided intake.
    DebugAnswer(String),
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
    /// User-local, presentation-only settings loaded by the runtime.
    preferences: gocode_core::Preferences,
    active_personality: gocode_core::PersonalityName,
    chat_input: String,
    /// Char index (not byte index) of the composer's insertion point.
    cursor: usize,
    suggestion_selected: usize,
    permission_mode: PermissionMode,
    /// The active session's persisted `/debug` investigation.
    debug: gocode_core::DebugInvestigation,
    /// Inverted so the derived `Default` (false) means automatic compaction is on, matching the
    /// documented default.
    auto_compact_disabled: bool,
    sessions: Vec<SessionSummary>,
    selected_session: usize,
    selection: Option<Selection>,
    copy_notification: Option<String>,
    entries: Vec<ChatEntry>,
    /// Memoizes the word-wrapped chat history so redraws that don't change the conversation or
    /// the terminal width (every scroll key press, every idle poll tick) skip re-wrapping the
    /// entire history from scratch. See `render_history`.
    history_wrap_cache: std::cell::RefCell<HistoryWrapCache>,
    streaming_assistant: bool,
    streaming_reasoning: bool,
    file_change_buffer: Vec<String>,
    activity: Option<AgentActivityState>,
    /// Live, user-facing account of the current run's phase, impact, and validation state.
    run_visibility: RunVisibility,
    pending_permission: Option<PermissionPrompt>,
    /// A structured choice requested by the model; it pauses the active run until answered.
    pending_guided_question: Option<gocode_core::GuidedQuestion>,
    selected_guided_choice: usize,
    /// A `/worktree remove <target>` awaiting explicit Y/N confirmation before it is sent.
    pending_worktree_removal: Option<String>,
    /// An `/agent apply` or `/agent cleanup` awaiting explicit Y/N confirmation before it is sent.
    pending_agent_confirm: Option<PendingAgentConfirm>,
    /// An `/agent apply` that hit a merge conflict, awaiting the user's per-file ours/theirs
    /// choices before the merge can be finished or aborted.
    pending_agent_conflict: Option<PendingAgentConflict>,
    /// An `/undo` or `/redo` that stopped on a conflicting file, awaiting the user's choice to
    /// cancel, view a diff, or force it through.
    pending_undo_conflict: Option<PendingUndoConflict>,
    /// Number of transactions currently available to `/undo`, shown by `/status`.
    undo_count: usize,
    /// Number of transactions currently available to `/redo`, shown by `/status`.
    redo_count: usize,
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
    /// Whether the `/agents` popup is currently shown over the chat screen.
    agents_visible: bool,
    /// Which screen of the `/agents` popup is currently shown.
    agents_view: AgentsView,
    /// Selected row within the `/agents` list screen.
    agents_selected: usize,
    /// Every subagent this session knows about, refreshed each time the popup opens or `r` is
    /// pressed inside it.
    agents: Vec<gocode_core::SubagentRecord>,
    /// Lines scrolled down from the top of the `/agents` detail screen.
    agents_detail_scroll: u16,
    /// The subagent shown by the `/agents` detail screen. Set by entering detail from the list
    /// (a snapshot of `agents[agents_selected]`) or by `/agent status <id>`/`/agent result <id>`
    /// deep-linking straight to it; kept in sync with `agents` by id whenever the list refreshes.
    agent_detail: Option<gocode_core::SubagentRecord>,
    /// Which of `agent_detail`'s `result.next_steps` entries `n` prefills next; cycles, and
    /// resets to `0` each time a (possibly different) subagent's detail is opened.
    agents_next_step_cursor: usize,
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
            AppEvent::PreferencesLoaded {
                preferences,
                recovery,
            } => {
                self.preferences.clone_from(preferences);
                self.active_personality = preferences.personality;
                if let Some(recovery) = recovery {
                    self.entries.push(ChatEntry::Warning(format!(
                        "Preferences recovery: {recovery}"
                    )));
                }
            }
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
            AppEvent::AssistantReasoningDelta(delta) => self.push_reasoning_delta(delta),
            AppEvent::DebugStateUpdated(debug) => self.debug.clone_from(debug),
            AppEvent::DebugInvestigationReady(prompt) => return Some(prompt.clone()),
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
                    && self.pending_worktree_removal.is_none()
                    && self.pending_agent_confirm.is_none()
                    && self.pending_agent_conflict.is_none()
                    && self.pending_undo_conflict.is_none()
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
            } => {
                self.run_visibility.observe_tool(name, *status, detail);
                self.apply_tool_activity(id, name, *status, detail);
            }
            AppEvent::ToolOutputChunk { id, chunk } => self.append_tool_output(id, chunk),
            AppEvent::FileChanged { path, .. } => {
                self.run_visibility.observe_file_change(path);
                self.file_change_buffer.push(path.clone());
            }
            AppEvent::AgentWarning(message) => {
                self.entries.push(ChatEntry::Warning(message.clone()));
            }
            AppEvent::AgentNotice(message) => {
                self.entries.push(ChatEntry::Info(message.clone()));
            }
            AppEvent::AgentProgress { line, .. } => {
                self.entries.push(ChatEntry::Info(line.clone()));
            }
            AppEvent::AgentDiffReady { id, diff } => {
                self.pending_agent_confirm = Some(PendingAgentConfirm::Apply {
                    id: id.clone(),
                    diff: diff.clone(),
                });
            }
            AppEvent::AgentCleanupWarning { id, message } => {
                self.pending_agent_confirm = Some(PendingAgentConfirm::Cleanup {
                    id: id.clone(),
                    message: message.clone(),
                });
            }
            AppEvent::AgentListAvailable(records) => {
                self.agents.clone_from(records);
                if self.agents_selected >= self.agents.len() {
                    self.agents_selected = self.agents.len().saturating_sub(1);
                }
                if let Some(current) = &self.agent_detail
                    && let Some(refreshed) =
                        self.agents.iter().find(|record| record.id == current.id)
                {
                    self.agent_detail = Some(refreshed.clone());
                }
            }
            AppEvent::AgentDetailAvailable { id, record } => {
                self.agents_visible = true;
                self.agents_view = AgentsView::Detail;
                self.agents_detail_scroll = 0;
                self.agents_next_step_cursor = 0;
                if let Some(record) = record {
                    self.agent_detail = Some((**record).clone());
                } else {
                    self.agent_detail = None;
                    self.agents_visible = false;
                    self.entries
                        .push(ChatEntry::Warning(format!("No subagent matches '{id}'.")));
                }
            }
            AppEvent::AgentMergeConflict { id, files } => {
                self.pending_agent_conflict = Some(PendingAgentConflict {
                    id: id.clone(),
                    files: files
                        .iter()
                        .map(|path| ConflictFileState {
                            path: path.clone(),
                            resolution: None,
                        })
                        .collect(),
                    selected: 0,
                });
            }
            AppEvent::AgentConflictFileResolved { id, file, ours } => {
                if let Some(conflict) = &mut self.pending_agent_conflict
                    && conflict.id == *id
                    && let Some(entry) = conflict.files.iter_mut().find(|entry| entry.path == *file)
                {
                    entry.resolution = Some(if *ours {
                        ConflictResolution::Ours
                    } else {
                        ConflictResolution::Theirs
                    });
                }
            }
            AppEvent::AgentMergeFinished {
                id,
                applied,
                message,
            } => {
                if self
                    .pending_agent_conflict
                    .as_ref()
                    .is_some_and(|conflict| conflict.id == *id)
                {
                    self.pending_agent_conflict = None;
                }
                self.entries.push(if *applied {
                    ChatEntry::Info(message.clone())
                } else {
                    ChatEntry::Warning(message.clone())
                });
            }
            AppEvent::PermissionRequested {
                summary,
                working_directory,
                scope_label,
            } => {
                self.pending_permission = Some(PermissionPrompt {
                    summary: summary.clone(),
                    working_directory: working_directory.clone(),
                    scope_label: scope_label.clone(),
                });
            }
            AppEvent::GuidedQuestionRequested(question) => {
                self.pending_guided_question = Some(question.clone());
                self.selected_guided_choice = question
                    .choices
                    .iter()
                    .position(|choice| choice.recommended)
                    .unwrap_or(0);
            }
            AppEvent::AgentCompleted {
                final_text,
                turns,
                tool_calls,
                failed_tool_calls,
                last_input_tokens,
                partial,
            } => {
                self.last_input_tokens = *last_input_tokens;
                self.activity = None;
                self.streaming_assistant = false;
                self.streaming_reasoning = false;
                if let Some(text) = final_text.as_ref().filter(|text| !text.is_empty()) {
                    self.entries.push(ChatEntry::Assistant(text.clone()));
                }
                self.flush_file_changes();
                self.entries.push(ChatEntry::Info(
                    self.run_visibility.completion_summary(*partial),
                ));
                let status = if *partial { "Stopped early" } else { "Done" };
                self.entries.push(ChatEntry::Info(format!(
                    "{status} — {turns} turn(s), {tool_calls} tool call(s), {failed_tool_calls} failed."
                )));
                self.last_submitted_prompt = None;
                self.show_queued_update();
                return self.queued.take();
            }
            AppEvent::AgentCancelled => {
                self.activity = None;
                self.streaming_assistant = false;
                self.streaming_reasoning = false;
                self.pending_permission = None;
                self.flush_file_changes();
                self.entries.push(ChatEntry::Info(
                    self.run_visibility.completion_summary(true),
                ));
                self.entries.push(ChatEntry::Info("Cancelled.".into()));
                self.last_submitted_prompt = None;
                self.show_queued_update();
                return self.queued.take();
            }
            AppEvent::AgentStopped => {
                self.activity = None;
                self.streaming_assistant = false;
                self.streaming_reasoning = false;
                self.pending_permission = None;
                self.flush_file_changes();
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
                transition,
                history,
            } => {
                self.current_session_id.clone_from(id);
                self.current_session_name.clone_from(name);
                self.entries.clear();
                self.scroll = 0;
                self.streaming_assistant = false;
                self.streaming_reasoning = false;
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
                let verb = match transition {
                    gocode_core::SessionTransition::New => "Started",
                    gocode_core::SessionTransition::Resumed => "Resumed",
                    gocode_core::SessionTransition::Forked => "Forked",
                };
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
            AppEvent::WorktreeListAvailable(worktrees) => {
                self.entries
                    .push(ChatEntry::Info(format_worktree_list(worktrees)));
            }
            AppEvent::WorktreeCreated { path, branch } => {
                self.entries.push(ChatEntry::Info(format!(
                    "Created worktree at {path} on branch '{branch}'.\n\
                     Switched this session's working directory there.\n\
                     Return to the original workspace with `/worktree switch` \
                     (or `/worktree list` to see every worktree)."
                )));
            }
            AppEvent::WorktreeSwitched { path, branch } => {
                self.entries.push(ChatEntry::Info(format!(
                    "Switched this session's working directory to {path} (branch '{branch}')."
                )));
            }
            AppEvent::WorktreeRemoved { path, switched_to } => {
                let mut message = format!("Removed worktree {path}.");
                if let Some(switched_to) = switched_to {
                    let _ = write!(
                        message,
                        "\nThat was this session's current worktree; switched back to {switched_to}."
                    );
                }
                self.entries.push(ChatEntry::Info(message));
            }
            AppEvent::WorktreeOperationFailed(message) => {
                self.entries
                    .push(ChatEntry::Error(format!("Worktree: {message}")));
            }
            AppEvent::UndoApplied {
                direction,
                transactions,
            } => {
                let verb = if direction == "redo" {
                    "Redid"
                } else {
                    "Undid"
                };
                self.entries
                    .push(ChatEntry::Info(format_undo_summary(verb, transactions)));
            }
            AppEvent::UndoConflict {
                direction,
                requested,
                applied,
                conflicting_files,
            } => {
                if !applied.is_empty() {
                    let done_verb = if direction == "redo" {
                        "Redid"
                    } else {
                        "Undid"
                    };
                    self.entries
                        .push(ChatEntry::Info(format_undo_summary(done_verb, applied)));
                }
                let action = if direction == "redo" { "Redo" } else { "Undo" };
                let files: Vec<String> = conflicting_files
                    .iter()
                    .map(|file| format!("`{}`", file.path))
                    .collect();
                self.entries.push(ChatEntry::Error(format!(
                    "{action} stopped: {} changed since the agent's edit and would be \
                     overwritten. Press F to force it anyway, D to view the diff, or C/Esc to \
                     cancel.",
                    files.join(", ")
                )));
                self.pending_undo_conflict = Some(PendingUndoConflict {
                    direction: direction.clone(),
                    count: *requested,
                    applied: applied.clone(),
                    conflicting_files: conflicting_files.clone(),
                    show_diff: false,
                });
            }
            AppEvent::UndoUnavailable { direction } => {
                let message = if direction == "redo" {
                    "Nothing to redo."
                } else {
                    "Nothing to undo."
                };
                self.entries.push(ChatEntry::Info(message.into()));
            }
            AppEvent::UndoStackChanged {
                undo_count,
                redo_count,
            } => {
                self.undo_count = *undo_count;
                self.redo_count = *redo_count;
            }
            AppEvent::UndoOperationFailed { direction, message } => {
                self.entries.push(ChatEntry::Error(format!(
                    "{}: {message}",
                    if direction == "redo" { "Redo" } else { "Undo" }
                )));
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

    fn push_reasoning_delta(&mut self, delta: &str) {
        if self.streaming_reasoning
            && let Some(ChatEntry::Reasoning(text)) = self.entries.last_mut()
        {
            text.push_str(delta);
            return;
        }
        self.entries.push(ChatEntry::Reasoning(delta.to_string()));
        self.streaming_reasoning = true;
    }

    fn show_queued_update(&mut self) {
        if self.screen == Screen::Chat
            && self.pending_permission.is_none()
            && self.pending_worktree_removal.is_none()
            && self.pending_agent_confirm.is_none()
            && self.pending_agent_conflict.is_none()
            && self.pending_undo_conflict.is_none()
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
        self.run_visibility.start();
        self.streaming_assistant = false;
        self.streaming_reasoning = false;
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

/// How a composed history line should be painted: `User` gets the themed highlight background
/// used in place of a literal "You:" prefix (see `render_history`); everything else renders
/// plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LineKind {
    Normal,
    User,
}

/// Untagged view of [`compose_lines_tagged`], kept for tests that don't care about line styling.
#[cfg(test)]
fn compose_lines(state: &AppState) -> Vec<String> {
    compose_lines_tagged(state)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

fn compose_lines_tagged(state: &AppState) -> Vec<(String, LineKind)> {
    let mut lines: Vec<(String, LineKind)> = banner_lines(state)
        .into_iter()
        .map(|line| (line, LineKind::Normal))
        .collect();

    if state.activity.is_some() {
        let action = state
            .run_visibility
            .current_action
            .as_deref()
            .unwrap_or("Preparando a tarefa");
        lines.push((
            format!(
                "● {} · {action} · {}",
                state.run_visibility.phase.label(),
                state.run_visibility.elapsed_label()
            ),
            LineKind::Normal,
        ));
        lines.push((state.run_visibility.impact_line(), LineKind::Normal));
        lines.push((state.run_visibility.plan_line(), LineKind::Normal));
        lines.push((String::new(), LineKind::Normal));
    }

    if state.entries.is_empty() {
        lines.push(("What can I help you build?".into(), LineKind::Normal));
        return lines;
    }

    for entry in &state.entries {
        match entry {
            ChatEntry::User(text) => push_wrapped(&mut lines, "", text, LineKind::User),
            ChatEntry::Assistant(text) => {
                push_wrapped(&mut lines, "Gocode: ", text, LineKind::Normal);
            }
            ChatEntry::Reasoning(text) => {
                push_wrapped(&mut lines, "Gocode (thinking): ", text, LineKind::Normal);
            }
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
                lines.push((format!("  {marker} {name}: {detail}"), LineKind::Normal));
                if !output.is_empty() {
                    let output_lines: Vec<&str> = output.lines().collect();
                    let limit = if *expanded {
                        EXPANDED_OUTPUT_LINES
                    } else {
                        COLLAPSED_OUTPUT_LINES
                    };
                    for line in output_lines.iter().take(limit) {
                        lines.push((format!("      {line}"), LineKind::Normal));
                    }
                    if output_lines.len() > limit {
                        let hidden = output_lines.len() - limit;
                        let action = if *expanded { "collapse" } else { "expand" };
                        lines.push((
                            format!("      … {hidden} more line(s) (Ctrl+O to {action})"),
                            LineKind::Normal,
                        ));
                    }
                }
            }
            ChatEntry::FileChanges(paths) => {
                lines.push((
                    format!("  Modified files: {}", paths.join(", ")),
                    LineKind::Normal,
                ));
            }
            ChatEntry::Warning(text) => lines.push((format!("  ⚠ {text}"), LineKind::Normal)),
            ChatEntry::Error(text) => lines.push((format!("  ✗ {text}"), LineKind::Normal)),
            ChatEntry::Info(text) => lines.push((format!("  · {text}"), LineKind::Normal)),
        }
        lines.push((String::new(), LineKind::Normal));
    }

    if let Some(activity) = state.activity {
        lines.push((
            match activity {
                AgentActivityState::Thinking => "Gocode is thinking…".into(),
                AgentActivityState::RunningTools => "Gocode is running tools…".into(),
            },
            LineKind::Normal,
        ));
    }
    if let Some(queued) = &state.queued {
        lines.push((format!("Queued: {queued}"), LineKind::Normal));
    }

    lines
}

fn push_wrapped(lines: &mut Vec<(String, LineKind)>, prefix: &str, text: &str, kind: LineKind) {
    let indent = " ".repeat(prefix.chars().count());
    for (index, line) in text.lines().enumerate() {
        if index == 0 {
            lines.push((format!("{prefix}{line}"), kind));
        } else {
            lines.push((format!("{indent}{line}"), kind));
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
    let theme = active_theme(state);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background).fg(theme.primary)),
        area,
    );
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
                .style(Style::default().fg(theme.primary).bg(theme.background))
                .block(
                    Block::default()
                        .title("Gocode")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(theme.border)),
                ),
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
    let theme = active_theme(state);
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
        Paragraph::new(content)
            .style(Style::default().fg(theme.primary).bg(theme.background))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title("Gocode · NVIDIA setup")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border)),
            ),
        area,
    );
}

fn render_model_picker(frame: &mut Frame, state: &AppState, area: Rect) {
    let theme = active_theme(state);
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
        Paragraph::new(content)
            .style(Style::default().fg(theme.primary).bg(theme.background))
            .block(
                Block::default()
                    .title("Gocode · Select NVIDIA model")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border)),
            ),
        area,
    );
}

fn render_settings(frame: &mut Frame, state: &AppState, area: Rect) {
    let theme = active_theme(state);
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
        Paragraph::new(content)
            .style(Style::default().fg(theme.primary).bg(theme.background))
            .block(
                Block::default()
                    .title("Gocode · Settings")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border)),
            ),
        area,
    );
}

/// Renders `/worktree list` output: the main worktree first, then every linked one, each with
/// its branch.
fn format_worktree_list(worktrees: &[gocode_core::WorktreeSummary]) -> String {
    if worktrees.is_empty() {
        return "No worktrees found.".into();
    }
    let mut lines = Vec::with_capacity(worktrees.len());
    for worktree in worktrees {
        let branch = worktree.branch.as_deref().unwrap_or("(detached HEAD)");
        let label = if worktree.is_main { " (main)" } else { "" };
        lines.push(format!("{}{label} — {branch}", worktree.path));
    }
    lines.join("\n")
}

/// Parses the raw text typed after `/undo` or `/redo`: empty means `1`; otherwise a positive
/// integer.
fn parse_undo_redo_count(raw: &str) -> Result<usize, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(1);
    }
    match trimmed.parse::<usize>() {
        Ok(0) | Err(_) => Err(format!("Usage: /undo [n] or /redo [n] (got `{trimmed}`)")),
        Ok(n) => Ok(n),
    }
}

/// Renders one transaction's file outcomes as a comma-separated `action path` list.
fn format_action_list(files: &[(String, String)]) -> String {
    files
        .iter()
        .map(|(path, action)| format!("{action} `{path}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Renders one or more applied undo/redo transactions as a single chat entry.
fn format_undo_summary(verb: &str, transactions: &[gocode_core::UndoTransactionResult]) -> String {
    if let [transaction] = transactions {
        format!(
            "{verb} '{}' — {}.",
            transaction.description,
            format_action_list(&transaction.files)
        )
    } else {
        let mut lines = vec![format!("{verb} {} transaction(s):", transactions.len())];
        for transaction in transactions {
            lines.push(format!(
                "- '{}' — {}",
                transaction.description,
                format_action_list(&transaction.files)
            ));
        }
        lines.join("\n")
    }
}

/// What a `/worktree` invocation, once parsed, should do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorktreeInvocation {
    /// No arguments were given: prefill the composer with `/worktree <name>` for the user to
    /// edit or confirm, rather than acting immediately.
    SuggestName(String),
    List,
    Switch(String),
    /// `remove <target>`, still awaiting the user's explicit Y/N confirmation.
    ConfirmRemove(String),
    Create {
        name: String,
        branch: gocode_core::WorktreeBranchSource,
    },
    /// Malformed subcommand usage, reported inline without contacting the runtime.
    Invalid(String),
}

/// Parses the raw text typed after `/worktree` into a concrete action. Recognizes `list`,
/// `switch <target>`, and `remove <target>` as subcommands; anything else is treated as
/// `<name> [existing-branch]` for creation.
fn parse_worktree_command(raw: &str, state: &AppState) -> WorktreeInvocation {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return WorktreeInvocation::SuggestName(suggest_worktree_name(state));
    }

    let mut tokens = trimmed.split_whitespace();
    let first = tokens.next().unwrap_or_default();
    match first {
        "list" => WorktreeInvocation::List,
        "switch" => tokens.next().map_or_else(
            || WorktreeInvocation::Invalid("Usage: /worktree switch <name-or-path>".into()),
            |target| WorktreeInvocation::Switch(target.to_string()),
        ),
        "remove" => tokens.next().map_or_else(
            || WorktreeInvocation::Invalid("Usage: /worktree remove <name-or-path>".into()),
            |target| WorktreeInvocation::ConfirmRemove(target.to_string()),
        ),
        name => {
            let branch = tokens
                .next()
                .map_or(gocode_core::WorktreeBranchSource::New, |existing| {
                    gocode_core::WorktreeBranchSource::Existing(existing.to_string())
                });
            WorktreeInvocation::Create {
                name: name.to_string(),
                branch,
            }
        }
    }
}

/// One parsed `/agent <subcommand> ...` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentInvocation {
    Spawn {
        task: String,
        mode: gocode_core::SubagentMode,
        model: Option<String>,
        worktree: bool,
    },
    Status(String),
    Message {
        id: String,
        text: String,
    },
    Stop(String),
    Result(String),
    /// `apply <id>`: request the diff. `apply <id> confirm`: merge it in.
    Apply {
        id: String,
        confirm: bool,
    },
    /// `cleanup <id>`: request the warning. `cleanup <id> confirm`: remove it.
    Cleanup {
        id: String,
        confirm: bool,
    },
    /// Malformed subcommand usage, reported inline without contacting the runtime.
    Invalid(String),
}

/// Parses the raw text typed after `/agent` into a concrete action.
fn parse_agent_command(raw: &str) -> AgentInvocation {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return AgentInvocation::Invalid(
            "Usage: /agent spawn|status|message|stop|result|apply|cleanup ...".into(),
        );
    }
    let mut tokens = trimmed.split_whitespace();
    let subcommand = tokens.next().unwrap_or_default();
    let rest: Vec<&str> = tokens.collect();

    match subcommand {
        "spawn" => parse_agent_spawn(&rest),
        "status" => id_argument(&rest, "status").map_or_else(
            || AgentInvocation::Invalid("Usage: /agent status <id>".into()),
            AgentInvocation::Status,
        ),
        "stop" => id_argument(&rest, "stop").map_or_else(
            || AgentInvocation::Invalid("Usage: /agent stop <id>".into()),
            AgentInvocation::Stop,
        ),
        "result" => id_argument(&rest, "result").map_or_else(
            || AgentInvocation::Invalid("Usage: /agent result <id>".into()),
            AgentInvocation::Result,
        ),
        "message" => {
            let Some((&id, text_tokens)) = rest.split_first() else {
                return AgentInvocation::Invalid("Usage: /agent message <id> <text>".into());
            };
            let text = text_tokens.join(" ");
            if text.is_empty() {
                return AgentInvocation::Invalid("Usage: /agent message <id> <text>".into());
            }
            AgentInvocation::Message {
                id: id.to_string(),
                text,
            }
        }
        "apply" => match rest.as_slice() {
            [id] => AgentInvocation::Apply {
                id: (*id).to_string(),
                confirm: false,
            },
            [id, "confirm"] => AgentInvocation::Apply {
                id: (*id).to_string(),
                confirm: true,
            },
            _ => AgentInvocation::Invalid("Usage: /agent apply <id> [confirm]".into()),
        },
        "cleanup" => match rest.as_slice() {
            [id] => AgentInvocation::Cleanup {
                id: (*id).to_string(),
                confirm: false,
            },
            [id, "confirm"] => AgentInvocation::Cleanup {
                id: (*id).to_string(),
                confirm: true,
            },
            _ => AgentInvocation::Invalid("Usage: /agent cleanup <id> [confirm]".into()),
        },
        other => AgentInvocation::Invalid(format!(
            "Unknown /agent subcommand '{other}'. Use spawn, status, message, stop, result, \
             apply, or cleanup."
        )),
    }
}

fn id_argument(rest: &[&str], _subcommand: &str) -> Option<String> {
    match rest {
        [id] => Some((*id).to_string()),
        _ => None,
    }
}

/// Parses `/agent spawn <task...> [--mode research|plan|implement|review] [--model <id>]
/// [--worktree]`. Flags may appear anywhere after `spawn`; everything else is joined back into
/// the task description in its original order.
fn parse_agent_spawn(rest: &[&str]) -> AgentInvocation {
    let mut mode = gocode_core::SubagentMode::Research;
    let mut model = None;
    let mut worktree = false;
    let mut task_words = Vec::new();

    let mut tokens = rest.iter().copied();
    while let Some(token) = tokens.next() {
        match token {
            "--mode" => match tokens.next().and_then(gocode_core::SubagentMode::parse) {
                Some(parsed) => mode = parsed,
                None => {
                    return AgentInvocation::Invalid(
                        "Usage: --mode research|plan|implement|review".into(),
                    );
                }
            },
            "--model" => match tokens.next() {
                Some(value) => model = Some(value.to_string()),
                None => return AgentInvocation::Invalid("Usage: --model <id>".into()),
            },
            "--worktree" => worktree = true,
            "--read-only" => {}
            word => task_words.push(word),
        }
    }

    let task = task_words.join(" ");
    if task.is_empty() {
        return AgentInvocation::Invalid(
            "Usage: /agent spawn <task> [--mode M] [--model X] [--worktree]".into(),
        );
    }
    if worktree && mode != gocode_core::SubagentMode::Implement {
        return AgentInvocation::Invalid("--worktree is only valid with --mode implement.".into());
    }
    AgentInvocation::Spawn {
        task,
        mode,
        model,
        worktree,
    }
}

/// Derives a worktree/branch name candidate from the active session's name, falling back to a
/// timestamped default for a fresh session that has no name yet.
fn suggest_worktree_name(state: &AppState) -> String {
    let from_session =
        if state.current_session_name.is_empty() || state.current_session_name == "New session" {
            None
        } else {
            Some(slugify(&state.current_session_name))
        }
        .filter(|slug| !slug.is_empty());

    from_session.unwrap_or_else(|| {
        let unix_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        format!("task-{unix_seconds}")
    })
}

/// Converts arbitrary text into a valid worktree/branch name segment: lowercase alphanumerics
/// separated by single hyphens, capped at a reasonable length.
fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = true; // avoids a leading '-'
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.len() > 40 {
        slug.pop();
    }
    slug.trim_end_matches('-').to_string()
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
    } else if let Some(question) = &state.pending_guided_question {
        render_guided_question_modal(frame, question, state.selected_guided_choice, area);
    } else if let Some(target) = &state.pending_worktree_removal {
        render_worktree_removal_modal(frame, target, area);
    } else if let Some(prompt) = &state.pending_agent_confirm {
        render_agent_confirm_modal(frame, prompt, area);
    } else if let Some(prompt) = &state.pending_agent_conflict {
        render_agent_conflict_modal(frame, prompt, area);
    } else if let Some(prompt) = &state.pending_undo_conflict {
        render_undo_conflict_modal(frame, prompt, area);
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
    } else if state.agents_visible {
        render_agents_modal(frame, state, area);
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

fn render_agents_modal(frame: &mut Frame, state: &AppState, area: Rect) {
    match state.agents_view {
        AgentsView::List => render_agents_list(frame, state, area),
        AgentsView::Detail => render_agents_detail(frame, state, area),
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

/// One row of the `/agents` list: cursor, id, mode, task, status, elapsed time, model, worktree.
fn agent_row(record: &gocode_core::SubagentRecord, selected: bool) -> String {
    let cursor = if selected { ">" } else { " " };
    let nesting = if record.parent_subagent_id.is_some() {
        "↳ "
    } else {
        ""
    };
    let id = record.id.get(..8).unwrap_or(&record.id);
    let worktree = record
        .worktree_path
        .as_ref()
        .map(|path| format!(", worktree {}", path.display()))
        .unwrap_or_default();
    format!(
        "{cursor} {nesting}{id} [{}] {} — {} ({}s, model {}{worktree})",
        record.mode.label(),
        record.task_summary,
        record.status.label(),
        record.elapsed_seconds(),
        record.model,
    )
}

fn render_agents_list(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal = centered(area, 88, 24);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .title("Gocode · Subagents")
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
    frame.render_widget(Paragraph::new("Active and recent subagents\n"), chunks[0]);

    if state.agents.is_empty() {
        frame.render_widget(
            Paragraph::new("None yet. Use `/agent spawn <task>` to create one.")
                .wrap(Wrap { trim: false }),
            chunks[1],
        );
    } else {
        let visible_rows = usize::from(chunks[1].height).max(1);
        let first_visible = state
            .agents_selected
            .saturating_sub(visible_rows.saturating_sub(1));
        let content = state.agents[first_visible..]
            .iter()
            .enumerate()
            .take(visible_rows)
            .map(|(offset, record)| {
                let index = first_visible + offset;
                agent_row(record, index == state.agents_selected)
            })
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(
            Paragraph::new(content).wrap(Wrap { trim: false }),
            chunks[1],
        );
    }

    frame.render_widget(
        Paragraph::new("Enter to view details · r to refresh · Esc to close")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

/// The `/agents` detail screen's footer hint, varying with whether there's anything to scroll
/// and whether `result.next_steps` has anything `n` could act on.
fn agents_detail_footer(scrollable: bool, has_next_steps: bool) -> &'static str {
    match (scrollable, has_next_steps) {
        (true, true) => {
            "Up/Down to scroll · n to spawn a next step · r to refresh · Esc to go back"
        }
        (true, false) => "Up/Down to scroll · r to refresh · Esc to go back",
        (false, true) => "n to spawn a next step · r to refresh · Esc to go back",
        (false, false) => "r to refresh · Esc to go back",
    }
}

fn render_agents_detail(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal = centered(area, 88, 24);
    frame.render_widget(Clear, modal);
    let record = state.agent_detail.as_ref();
    let title = record.map_or_else(
        || "Gocode · Subagent".to_string(),
        |record| {
            format!(
                "Gocode · Subagent · {}",
                record.id.get(..8).unwrap_or(&record.id)
            )
        },
    );
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let mut content = String::new();
    match record {
        None => content.push_str("This subagent is no longer listed."),
        Some(record) => {
            let _ = writeln!(content, "Task: {}", record.task_summary);
            let _ = writeln!(
                content,
                "Mode: {}   Status: {}   Elapsed: {}s",
                record.mode.label(),
                record.status.label(),
                record.elapsed_seconds()
            );
            let _ = writeln!(content, "Model: {}", record.model);
            if let Some(parent_id) = &record.parent_subagent_id {
                let _ = writeln!(
                    content,
                    "Depth: {} (spawned by {})",
                    record.depth,
                    parent_id.get(..8).unwrap_or(parent_id)
                );
            }
            if let Some(path) = &record.worktree_path {
                let _ = writeln!(
                    content,
                    "Worktree: {} (branch {})",
                    path.display(),
                    record.branch.as_deref().unwrap_or("?")
                );
            }
            if !record.messages.is_empty() {
                content.push_str("\nMessages:\n");
                for message in &record.messages {
                    let role = match message.role {
                        gocode_core::SubagentMessageRole::Supervisor => "you",
                        gocode_core::SubagentMessageRole::Subagent => "subagent",
                    };
                    let _ = writeln!(content, "  [{role}] {}", message.text);
                }
            }
            match &record.result {
                Some(result) => {
                    content.push_str("\nResult:\n");
                    let _ = writeln!(content, "  {}", result.summary);
                    let mut section = |title: &str, items: &[String]| {
                        if !items.is_empty() {
                            let _ = writeln!(content, "  {title}: {}", items.join("; "));
                        }
                    };
                    section("Findings", &result.findings);
                    section("Files changed", &result.files_changed);
                    section("Risks", &result.risks);
                    section("Next steps", &result.next_steps);
                    if let Some(error) = &result.error {
                        let _ = writeln!(content, "  Error: {error}");
                    }
                }
                None => content.push_str("\nNo result yet."),
            }
        }
    }

    let content_lines = content.lines().count();
    let visible_rows = usize::from(chunks[0].height).max(1);
    let max_scroll = u16::try_from(content_lines.saturating_sub(visible_rows)).unwrap_or(u16::MAX);
    let scroll = state.agents_detail_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        chunks[0],
    );

    let has_next_steps = record
        .and_then(|record| record.result.as_ref())
        .is_some_and(|result| !result.next_steps.is_empty());
    let footer = agents_detail_footer(max_scroll > 0, has_next_steps);
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::DarkGray)),
        chunks[1],
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

/// Word-wrapped chat history, cached across draws that don't change the conversation or width.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HistoryWrapCache {
    width: usize,
    source_hash: u64,
    wrapped: Vec<(String, LineKind)>,
}

fn hash_lines(lines: &[(String, LineKind)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    lines.hash(&mut hasher);
    hasher.finish()
}

/// The word-wrapped chat history at `content_width`, recomputed only when the conversation or
/// the width has actually changed since the last call. Shared by rendering, mouse hit-testing,
/// and selection-text extraction so none of them re-wrap the whole history on every draw.
fn wrapped_history_lines(
    state: &AppState,
    content_width: usize,
) -> std::cell::Ref<'_, Vec<(String, LineKind)>> {
    let composed = compose_lines_tagged(state);
    let hash = hash_lines(&composed);
    {
        let mut cache = state.history_wrap_cache.borrow_mut();
        if cache.width != content_width || cache.source_hash != hash {
            cache.wrapped = wrap_lines_tagged(&composed, content_width);
            cache.width = content_width;
            cache.source_hash = hash;
        }
    }
    std::cell::Ref::map(state.history_wrap_cache.borrow(), |cache| &cache.wrapped)
}

fn render_history(frame: &mut Frame, state: &AppState, area: Rect) {
    let theme = active_theme(state);
    let content_width = usize::from(area.width.saturating_sub(2));
    let wrapped = wrapped_history_lines(state, content_width);
    let visible_rows = usize::from(area.height.saturating_sub(2)).max(1);
    let (start, end) = compute_visible_window(wrapped.len(), visible_rows, state.scroll);

    // Themed background used in place of a literal "You:" prefix on the user's own messages —
    // a small highlighted "pill" rather than a text label.
    let user_style = Style::default().bg(theme.highlight).fg(theme.background);

    let rendered_lines: Vec<Line> = wrapped[start..end]
        .iter()
        .enumerate()
        .map(|(offset, (line, kind))| {
            let absolute_index = start + offset;
            let chars: Vec<char> = line.chars().collect();
            let is_user = *kind == LineKind::User;
            let selected_range = state
                .selection
                .as_ref()
                .and_then(|selection| selected_char_range(selection, absolute_index, chars.len()));
            let pad = || Span::styled(" ", user_style);
            // Fill the rest of the row with styled spaces so the highlight spans the full
            // content width instead of stopping at the text (a "pill" the width of the panel,
            // not just the message).
            let fill = || {
                let used = chars.len() + 2; // the leading and trailing single-space pads
                Span::styled(" ".repeat(content_width.saturating_sub(used)), user_style)
            };
            match selected_range {
                Some((from, to)) => {
                    let before: String = chars[..from].iter().collect();
                    let marked: String = chars[from..to].iter().collect();
                    let after: String = chars[to..].iter().collect();
                    let base = if is_user {
                        user_style
                    } else {
                        Style::default()
                    };
                    let mut spans = Vec::new();
                    if is_user {
                        spans.push(pad());
                    }
                    spans.push(Span::styled(before, base));
                    spans.push(Span::styled(
                        marked,
                        Style::default().bg(SELECTION_COLOR).fg(Color::Black),
                    ));
                    spans.push(Span::styled(after, base));
                    if is_user {
                        spans.push(pad());
                        spans.push(fill());
                    }
                    Line::from(spans)
                }
                None if is_user => Line::from(vec![
                    pad(),
                    Span::styled(line.clone(), user_style),
                    pad(),
                    fill(),
                ]),
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
            .style(Style::default().fg(theme.primary).bg(theme.background))
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border)),
            ),
        area,
    );
}

/// Untagged view of [`wrap_lines_tagged`], kept for tests that don't care about line styling.
#[cfg(test)]
fn wrap_lines(lines: &[String], width: usize) -> Vec<String> {
    let tagged: Vec<(String, LineKind)> = lines
        .iter()
        .cloned()
        .map(|line| (line, LineKind::Normal))
        .collect();
    wrap_lines_tagged(&tagged, width)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

/// Word-wraps every logical line to `width` columns so the resulting rows match 1:1 what gets
/// drawn to the terminal — the same rows are then used for mouse-selection hit-testing, so
/// rendering and hit-testing can never disagree about where a character lands. Each output row
/// keeps the [`LineKind`] tag of the logical line it was split from.
fn wrap_lines_tagged(lines: &[(String, LineKind)], width: usize) -> Vec<(String, LineKind)> {
    let width = width.max(1);
    let mut wrapped = Vec::new();
    for (line, kind) in lines {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            wrapped.push((String::new(), *kind));
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
            wrapped.push((segment.trim_end_matches(' ').to_string(), *kind));
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
    let theme = active_theme(state);
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
                    .fg(theme.highlight)
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
    let mode_span = Span::styled(mode_text.clone(), Style::default().fg(theme.command));
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
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    } else {
        Line::from(mode_span)
    });
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(theme.primary).bg(theme.background))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border)),
            ),
        area,
    );

    let editing = state.pending_permission.is_none()
        && state.pending_guided_question.is_none()
        && state.pending_worktree_removal.is_none()
        && state.pending_agent_confirm.is_none()
        && state.pending_agent_conflict.is_none()
        && state.pending_undo_conflict.is_none()
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
    let modal = centered(area, 72, 12);
    let content = format!(
        "O agente quer executar:\n{}\nEm: {}\n\nPermitir uma vez\n  Executa somente esta ação.\n\nPermitir sempre: {}\n  Não pergunta novamente para esta categoria nesta sessão.\n\nNão permitir\n  Cancela esta ação; o agente procura outra alternativa.\n\n[1] Uma vez   [2] Sempre   [3] Não permitir",
        prompt.summary, prompt.working_directory, prompt.scope_label
    );
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(content).wrap(Wrap { trim: false }).block(
            Block::default()
                .title("Permissão necessária")
                .borders(Borders::ALL),
        ),
        modal,
    );
}

fn render_guided_question_modal(
    frame: &mut Frame,
    question: &gocode_core::GuidedQuestion,
    selected: usize,
    area: Rect,
) {
    let mut content = question.context.clone();
    for (index, choice) in question.choices.iter().enumerate() {
        let marker = if index == selected { "›" } else { " " };
        let recommended = if choice.recommended {
            " (recomendado)"
        } else {
            ""
        };
        let _ = write!(
            content,
            "\n\n{marker} {}. {}{recommended}\n  {}\n  + {}\n  − {}",
            index + 1,
            choice.label,
            choice.summary,
            choice.advantages,
            choice.disadvantages,
        );
    }
    content.push_str("\n\n↑↓ navega  •  Enter confirma  •  Esc cancela");
    let height = (question.choices.len() as u16 * 5 + 5).min(area.height.saturating_sub(2));
    let modal = centered(area, 76, height);
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(content).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(question.title.as_str())
                .borders(Borders::ALL),
        ),
        modal,
    );
}

fn render_worktree_removal_modal(frame: &mut Frame, target: &str, area: Rect) {
    let modal = centered(area, 60, 7);
    let content = format!(
        "Remove worktree '{target}'?\n\nThis runs `git worktree remove` (no --force); \
                  git refuses if it has uncommitted changes.\n\n[y] Remove   [n] Cancel"
    );
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().title("Confirm").borders(Borders::ALL)),
        modal,
    );
}

fn render_agent_confirm_modal(frame: &mut Frame, prompt: &PendingAgentConfirm, area: Rect) {
    let (title, body, height) = match prompt {
        PendingAgentConfirm::Apply { id, diff } => (
            "Apply subagent changes",
            format!(
                "Merge subagent {id}'s branch into the current branch?\n\n{diff}\n\n[y] Apply   \
                 [n] Cancel"
            ),
            18,
        ),
        PendingAgentConfirm::Cleanup { message, .. } => (
            "Confirm",
            format!("{message}\n\n[y] Remove   [n] Cancel"),
            9,
        ),
    };
    let modal = centered(area, 76, height);
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .block(Block::default().title(title).borders(Borders::ALL)),
        modal,
    );
}

/// One row of the guided conflict resolver's file list: cursor, resolution checkbox, path.
fn conflict_file_row(file: &ConflictFileState, selected: bool) -> String {
    let cursor = if selected { ">" } else { " " };
    let status = match file.resolution {
        None => "[ ]",
        Some(ConflictResolution::Ours) => "[ours]",
        Some(ConflictResolution::Theirs) => "[theirs]",
    };
    format!("{cursor} {status} {}", file.path)
}

fn render_agent_conflict_modal(frame: &mut Frame, prompt: &PendingAgentConflict, area: Rect) {
    let height = u16::try_from(prompt.files.len())
        .unwrap_or(u16::MAX)
        .saturating_add(9);
    let modal = centered(area, 76, height);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .title("Merge conflict")
        .borders(Borders::ALL);
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(format!(
            "Subagent {}'s branch conflicts with the current branch on {} file(s).\nPick a side \
             for each, then finish once every file is resolved.",
            prompt.id.get(..8).unwrap_or(&prompt.id),
            prompt.files.len(),
        ))
        .wrap(Wrap { trim: false }),
        chunks[0],
    );

    let content = prompt
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| conflict_file_row(file, index == prompt.selected))
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(Paragraph::new(content), chunks[1]);

    let all_resolved = prompt.files.iter().all(|file| file.resolution.is_some());
    let footer = if all_resolved {
        "[o] Keep ours   [t] Keep theirs   [Enter] Finish merge   [Esc] Abort merge"
    } else {
        "[o] Keep ours   [t] Keep theirs   [Esc] Abort merge (resolve every file to finish)"
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

fn render_undo_conflict_modal(frame: &mut Frame, prompt: &PendingUndoConflict, area: Rect) {
    let verb = if prompt.direction == "redo" {
        "Redo"
    } else {
        "Undo"
    };
    let files: Vec<String> = prompt
        .conflicting_files
        .iter()
        .map(|file| format!("`{}`", file.path))
        .collect();
    let mut content = format!(
        "{verb} stopped: the following file(s) changed since the agent's edit and would be \
         overwritten:\n{}\n\n[f] Force anyway   [d] {} diff   [c] Cancel",
        files.join(", "),
        if prompt.show_diff { "Hide" } else { "View" }
    );
    if prompt.show_diff {
        content.push_str("\n\n");
        for file in &prompt.conflicting_files {
            let _ = write!(
                content,
                "--- {} (expected)\n{}\n+++ {} (on disk)\n{}\n\n",
                file.path,
                file.expected.as_deref().unwrap_or("(absent)"),
                file.path,
                file.actual.as_deref().unwrap_or("(absent)"),
            );
        }
    }
    let height: u16 = 9 + if prompt.show_diff { 12 } else { 0 };
    let modal = centered(area, 76, height);
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::default().title("Conflict").borders(Borders::ALL)),
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
    // On X11, clipboard ownership ends when this handle is dropped. Keep it for the terminal
    // lifetime so clipboard managers have time to persist a copied selection.
    let mut clipboard: Option<arboard::Clipboard> = None;

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
                try_copy_selection(
                    &mut state,
                    terminal_area,
                    &mut copy_notification_deadline,
                    &mut clipboard,
                );
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
                try_copy_selection(
                    &mut state,
                    terminal_area,
                    &mut copy_notification_deadline,
                    &mut clipboard,
                );
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

        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = &terminal_event
            && state.activity.is_some()
            && key_action_matches(
                &state.preferences,
                gocode_core::KeyAction::InterruptExecution,
                *code,
                *modifiers,
            )
        {
            send_command(&command_tx, AppCommand::CancelProviderRequest)?;
            continue;
        }

        if let Some(approved) = handle_permission_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::PermissionResponse(approved))?;
            continue;
        }

        if let Some(answer) = handle_guided_question_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::GuidedAnswer(answer))?;
            continue;
        }

        if let Some((target, confirmed)) =
            handle_worktree_removal_event(&mut state, &terminal_event)
        {
            if confirmed {
                send_command(&command_tx, AppCommand::WorktreeRemove(target))?;
            } else {
                state.entries.push(ChatEntry::Info(format!(
                    "Cancelled removing worktree '{target}'."
                )));
            }
            continue;
        }

        if let Some((action, confirmed)) = handle_agent_confirm_event(&mut state, &terminal_event) {
            match (action, confirmed) {
                (AgentConfirmAction::Apply(id), true) => {
                    send_command(&command_tx, AppCommand::AgentApplyConfirm(id))?;
                }
                (AgentConfirmAction::Apply(id), false) => {
                    state.entries.push(ChatEntry::Info(format!(
                        "Cancelled applying subagent {id}."
                    )));
                }
                (AgentConfirmAction::Cleanup(id), true) => {
                    send_command(&command_tx, AppCommand::AgentCleanupConfirm(id))?;
                }
                (AgentConfirmAction::Cleanup(id), false) => {
                    state.entries.push(ChatEntry::Info(format!(
                        "Cancelled cleaning up subagent {id}."
                    )));
                }
            }
            continue;
        }

        match handle_agent_conflict_event(&mut state, &terminal_event) {
            AgentConflictEventOutcome::NotHandled => {}
            AgentConflictEventOutcome::Handled => continue,
            AgentConflictEventOutcome::Resolve { id, file, ours } => {
                send_command(
                    &command_tx,
                    AppCommand::AgentResolveConflict { id, file, ours },
                )?;
                continue;
            }
            AgentConflictEventOutcome::Finish(id) => {
                send_command(&command_tx, AppCommand::AgentFinishMerge(id))?;
                continue;
            }
            AgentConflictEventOutcome::Abort(id) => {
                send_command(&command_tx, AppCommand::AgentAbortMerge(id))?;
                continue;
            }
        }

        match handle_undo_conflict_event(&mut state, &terminal_event) {
            Some(UndoConflictOutcome::Forced { direction, count }) => {
                if direction == "redo" {
                    send_command(&command_tx, AppCommand::RedoForce(count))?;
                } else {
                    send_command(&command_tx, AppCommand::UndoForce(count))?;
                }
                continue;
            }
            Some(UndoConflictOutcome::Cancelled) => {
                state.entries.push(ChatEntry::Info("Cancelled.".into()));
                continue;
            }
            Some(UndoConflictOutcome::DiffToggled) => continue,
            None => {}
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

        let was_model_picker = state.screen == Screen::ModelPicker;
        if let Some(model) = handle_model_picker_event(&mut state, &terminal_event) {
            send_command(&command_tx, AppCommand::SelectModel(model))?;
            continue;
        }
        if was_model_picker && state.screen != Screen::ModelPicker {
            // Enter chained straight into the effort picker (see
            // `handle_model_picker_event`'s `model_flow_pending_effort` branch). Stop here so
            // the same key press isn't replayed into `handle_effort_picker_event` below, which
            // would immediately confirm whatever effort happened to be preselected.
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

        match handle_agents_event(&mut state, &terminal_event) {
            AgentsEventOutcome::NotHandled => {}
            AgentsEventOutcome::Handled => continue,
            AgentsEventOutcome::Refresh => {
                send_command(&command_tx, AppCommand::AgentList)?;
                continue;
            }
        }

        if let Some(submission) = handle_chat_event(&mut state, &terminal_event) {
            match submission {
                ChatSubmission::Command(SlashCommand::Keymap(arguments)) => {
                    let args: Vec<_> = arguments.split_whitespace().collect();
                    if args.is_empty() {
                        let defaults = gocode_core::default_keymap();
                        let mut body = String::from("Keymap (action · shortcut · origin)\n");
                        for action in gocode_core::KeyAction::all() {
                            let binding = state
                                .preferences
                                .keymap
                                .get(action)
                                .expect("normalized keymap");
                            let origin = if defaults.get(action) == Some(binding) {
                                "default"
                            } else {
                                "custom"
                            };
                            let conflicts: Vec<_> = state
                                .preferences
                                .keymap
                                .iter()
                                .filter(|(other, value)| {
                                    *other != action && value.eq_ignore_ascii_case(binding)
                                })
                                .map(|(other, _)| other.label())
                                .collect();
                            let conflict = if conflicts.is_empty() {
                                String::new()
                            } else {
                                format!(" · conflict: {}", conflicts.join(", "))
                            };
                            let _ = writeln!(
                                body,
                                "{} · {} · {origin}{conflict}",
                                action.label(),
                                binding
                            );
                        }
                        state.entries.push(ChatEntry::Info(body));
                    } else if args[0] == "reset-all" {
                        if args.contains(&"--force") {
                            state.preferences.keymap = gocode_core::default_keymap();
                            send_command(
                                &command_tx,
                                AppCommand::SetPreferences(state.preferences.clone()),
                            )?;
                            state.entries.push(ChatEntry::Info(
                                "All shortcuts restored to defaults.".into(),
                            ));
                        } else {
                            state.entries.push(ChatEntry::Warning(
                                "Confirm with `/keymap reset-all --force`.".into(),
                            ));
                        }
                    } else if args.first() == Some(&"reset") && args.len() == 2 {
                        if let Some(action) = gocode_core::KeyAction::parse(args[1]) {
                            state
                                .preferences
                                .keymap
                                .insert(action, gocode_core::default_keymap()[&action].clone());
                            send_command(
                                &command_tx,
                                AppCommand::SetPreferences(state.preferences.clone()),
                            )?;
                            state
                                .entries
                                .push(ChatEntry::Info(format!("Reset {}.", action.label())));
                        } else {
                            state
                                .entries
                                .push(ChatEntry::Error("Unknown keymap action.".into()));
                        }
                    } else if args.first() == Some(&"set") && args.len() >= 3 {
                        let Some(action) = gocode_core::KeyAction::parse(args[1]) else {
                            state
                                .entries
                                .push(ChatEntry::Error("Unknown keymap action.".into()));
                            continue;
                        };
                        let shortcut = args[2].to_ascii_lowercase();
                        if !gocode_core::valid_shortcut(&shortcut) {
                            state.entries.push(ChatEntry::Error(
                                "Invalid shortcut. Example: ctrl+enter.".into(),
                            ));
                            continue;
                        }
                        let force = args.contains(&"--force");
                        let conflicts: Vec<_> = state
                            .preferences
                            .keymap
                            .iter()
                            .filter(|(other, value)| {
                                **other != action && value.eq_ignore_ascii_case(&shortcut)
                            })
                            .map(|(other, _)| *other)
                            .collect();
                        let essential =
                            matches!(action, gocode_core::KeyAction::InterruptExecution);
                        if (essential || !conflicts.is_empty()) && !force {
                            state.entries.push(ChatEntry::Warning("This changes an essential shortcut or replaces a conflict. Repeat with `--force`.".into()));
                            continue;
                        }
                        for conflict in conflicts {
                            state
                                .preferences
                                .keymap
                                .insert(conflict, gocode_core::default_keymap()[&conflict].clone());
                        }
                        state.preferences.keymap.insert(action, shortcut.clone());
                        send_command(
                            &command_tx,
                            AppCommand::SetPreferences(state.preferences.clone()),
                        )?;
                        state.entries.push(ChatEntry::Info(format!(
                            "{} bound to {shortcut}.",
                            action.label()
                        )));
                    } else {
                        state.entries.push(ChatEntry::Info("Usage: /keymap [set <action> <shortcut> [--force] | reset <action> | reset-all --force]".into()));
                    }
                }
                ChatSubmission::Command(SlashCommand::Theme(arguments)) => {
                    let args: Vec<_> = arguments.split_whitespace().collect();
                    if args.is_empty() {
                        state.entries.push(ChatEntry::Info(format!(
                            "Themes: {}. Active: {}.",
                            gocode_core::ThemeName::all()
                                .iter()
                                .map(|theme| theme.label())
                                .collect::<Vec<_>>()
                                .join(", "),
                            state.preferences.theme.label()
                        )));
                    } else if args == ["current"] {
                        state.entries.push(ChatEntry::Info(format!(
                            "Active theme: {}.",
                            state.preferences.theme.label()
                        )));
                    } else if args == ["reset"] {
                        state.preferences.theme = gocode_core::ThemeName::System;
                        send_command(
                            &command_tx,
                            AppCommand::SetPreferences(state.preferences.clone()),
                        )?;
                    } else if args.len() == 2 && args[0] == "set" {
                        if let Some(theme) = gocode_core::ThemeName::parse(args[1]) {
                            state.preferences.theme = theme;
                            send_command(
                                &command_tx,
                                AppCommand::SetPreferences(state.preferences.clone()),
                            )?;
                            state
                                .entries
                                .push(ChatEntry::Info(format!("Theme set to {}.", theme.label())));
                        } else {
                            state
                                .entries
                                .push(ChatEntry::Error("Unknown theme.".into()));
                        }
                    } else {
                        state.entries.push(ChatEntry::Info(
                            "Usage: /theme [current | set <name> | reset]".into(),
                        ));
                    }
                }
                ChatSubmission::Command(SlashCommand::Personality(arguments)) => {
                    let args: Vec<_> = arguments.split_whitespace().collect();
                    if args.is_empty() {
                        state.entries.push(ChatEntry::Info(format!(
                            "Personalities: {}. Active: {}.",
                            gocode_core::PersonalityName::all()
                                .iter()
                                .map(|p| p.label())
                                .collect::<Vec<_>>()
                                .join(", "),
                            state.active_personality.label()
                        )));
                    } else if args == ["reset"] {
                        state.active_personality = gocode_core::PersonalityName::Default;
                        send_command(
                            &command_tx,
                            AppCommand::SetSessionPersonality(state.active_personality),
                        )?;
                    } else if args.len() == 2 && (args[0] == "set" || args[0] == "default") {
                        if let Some(personality) = gocode_core::PersonalityName::parse(args[1]) {
                            if args[0] == "default" {
                                state.preferences.personality = personality;
                                send_command(
                                    &command_tx,
                                    AppCommand::SetPreferences(state.preferences.clone()),
                                )?;
                            }
                            state.active_personality = personality;
                            send_command(
                                &command_tx,
                                AppCommand::SetSessionPersonality(personality),
                            )?;
                            state.entries.push(ChatEntry::Info(format!(
                                "Personality set to {}.",
                                personality.label()
                            )));
                        } else {
                            state
                                .entries
                                .push(ChatEntry::Error("Unknown personality.".into()));
                        }
                    } else {
                        state.entries.push(ChatEntry::Info(
                            "Usage: /personality [set <name> | default <name> | reset]".into(),
                        ));
                    }
                }
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
                ChatSubmission::Command(SlashCommand::ForkSession) => {
                    send_command(&command_tx, AppCommand::ForkSession)?;
                }
                ChatSubmission::Command(SlashCommand::Model) => {
                    state.screen = Screen::ModelPicker;
                    state.selected_model = 0;
                    state.model_flow_pending_effort = true;
                }
                ChatSubmission::Command(SlashCommand::Effort) => {
                    state.screen = Screen::EffortPicker;
                    state.model_flow_pending_effort = false;
                    state.pending_model = None;
                    state.selected_effort = EFFORT_OPTIONS
                        .iter()
                        .position(|(_, value)| *value == state.current_effort.as_deref())
                        .unwrap_or(0);
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
                         Personality: {}\n\
                         Directory: {directory}\n\
                         Session: {session}\n\
                         Context: {context}\n\
                         Undo: {} available · Redo: {} available",
                        state.active_personality.label(),
                        state.undo_count,
                        state.redo_count
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
                ChatSubmission::Command(SlashCommand::Worktree(raw)) => {
                    match parse_worktree_command(&raw, &state) {
                        WorktreeInvocation::SuggestName(suggested) => {
                            state.set_chat_input(format!("/worktree {suggested}"));
                        }
                        WorktreeInvocation::List => {
                            send_command(&command_tx, AppCommand::WorktreeList)?;
                        }
                        WorktreeInvocation::Switch(target) => {
                            send_command(&command_tx, AppCommand::WorktreeSwitch(target))?;
                        }
                        WorktreeInvocation::ConfirmRemove(target) => {
                            state.pending_worktree_removal = Some(target);
                        }
                        WorktreeInvocation::Create { name, branch } => {
                            send_command(&command_tx, AppCommand::WorktreeCreate { name, branch })?;
                        }
                        WorktreeInvocation::Invalid(message) => {
                            state.entries.push(ChatEntry::Error(message));
                        }
                    }
                }
                ChatSubmission::Command(SlashCommand::Undo(raw)) => {
                    match parse_undo_redo_count(&raw) {
                        Ok(n) => send_command(&command_tx, AppCommand::Undo(n))?,
                        Err(message) => state.entries.push(ChatEntry::Error(message)),
                    }
                }
                ChatSubmission::Command(SlashCommand::Redo(raw)) => {
                    match parse_undo_redo_count(&raw) {
                        Ok(n) => send_command(&command_tx, AppCommand::Redo(n))?,
                        Err(message) => state.entries.push(ChatEntry::Error(message)),
                    }
                }
                ChatSubmission::Command(SlashCommand::Debug(raw)) => {
                    let argument = raw.trim();
                    match argument {
                        "status" => state
                            .entries
                            .push(ChatEntry::Info(debug_status(&state.debug))),
                        "summary" => state
                            .entries
                            .push(ChatEntry::Info(debug_summary(&state.debug))),
                        "stop" => send_command(&command_tx, AppCommand::DebugStop)?,
                        "" if state.debug.is_active() => {
                            state
                                .entries
                                .push(ChatEntry::Info(state.debug.next_question().map_or_else(
                                || {
                                    "Debug em andamento. Use `/debug status` ou `/debug summary`."
                                        .into()
                                },
                                |question| format!("Retomando investigação\n\n{question}"),
                            )));
                        }
                        "" => send_command(&command_tx, AppCommand::DebugStart(None))?,
                        description => send_command(
                            &command_tx,
                            AppCommand::DebugStart(Some(description.to_string())),
                        )?,
                    }
                }
                ChatSubmission::Command(SlashCommand::Agent(raw)) => {
                    match parse_agent_command(&raw) {
                        AgentInvocation::Spawn {
                            task,
                            mode,
                            model,
                            worktree,
                        } => send_command(
                            &command_tx,
                            AppCommand::AgentSpawn {
                                task,
                                mode,
                                model,
                                worktree,
                            },
                        )?,
                        AgentInvocation::Status(id) => {
                            send_command(&command_tx, AppCommand::AgentStatus(id))?;
                        }
                        AgentInvocation::Message { id, text } => {
                            send_command(&command_tx, AppCommand::AgentMessage { id, text })?;
                        }
                        AgentInvocation::Stop(id) => {
                            send_command(&command_tx, AppCommand::AgentStop(id))?;
                        }
                        AgentInvocation::Result(id) => {
                            send_command(&command_tx, AppCommand::AgentResult(id))?;
                        }
                        AgentInvocation::Apply { id, confirm: false } => {
                            send_command(&command_tx, AppCommand::AgentApplyRequest(id))?;
                        }
                        AgentInvocation::Apply { id, confirm: true } => {
                            send_command(&command_tx, AppCommand::AgentApplyConfirm(id))?;
                        }
                        AgentInvocation::Cleanup { id, confirm: false } => {
                            send_command(&command_tx, AppCommand::AgentCleanupRequest(id))?;
                        }
                        AgentInvocation::Cleanup { id, confirm: true } => {
                            send_command(&command_tx, AppCommand::AgentCleanupConfirm(id))?;
                        }
                        AgentInvocation::Invalid(message) => {
                            state.entries.push(ChatEntry::Error(message));
                        }
                    }
                }
                ChatSubmission::Command(SlashCommand::Agents) => {
                    state.agents_visible = true;
                    state.agents_view = AgentsView::List;
                    state.agents_selected = 0;
                    send_command(&command_tx, AppCommand::AgentList)?;
                }
                ChatSubmission::DebugAnswer(answer) => {
                    send_command(&command_tx, AppCommand::DebugAnswer(answer))?;
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
            && state.pending_guided_question.is_none()
            && state.pending_worktree_removal.is_none()
            && state.pending_agent_confirm.is_none()
            && state.pending_agent_conflict.is_none()
            && state.pending_undo_conflict.is_none()
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

/// Cycles the permission mode (Auto → Plan → Approve → Manual → Auto) on Shift+Tab from the chat
/// composer, returning the newly selected mode.
///
/// Handled ahead of [`handle_chat_event`] so a Shift+Tab keypress never falls through to plain
/// Tab's autocomplete behavior.
#[must_use]
pub fn handle_permission_mode_event(state: &mut AppState, event: &Event) -> Option<PermissionMode> {
    if state.screen != Screen::Chat
        || state.blocking_error.is_some()
        || state.pending_permission.is_some()
        || state.pending_guided_question.is_some()
        || state.pending_worktree_removal.is_some()
        || state.pending_agent_confirm.is_some()
        || state.pending_agent_conflict.is_some()
        || state.pending_undo_conflict.is_some()
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
    let wrapped = wrapped_history_lines(state, content_width);
    let visible_rows = usize::from(history_area.height.saturating_sub(2)).max(1);
    let (start, _end) = compute_visible_window(wrapped.len(), visible_rows, state.scroll);

    let absolute_line = start + local_row;
    let line_len = wrapped.get(absolute_line)?.0.chars().count();
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
    let wrapped = wrapped_history_lines(state, content_width);
    let (start, end) = selection.normalized();

    let mut collected = Vec::new();
    for (line_index, (line, _)) in wrapped
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
    clipboard: &mut Option<arboard::Clipboard>,
) {
    let Some(text) = extract_selected_text(state, terminal_area) else {
        return;
    };
    let char_count = text.chars().count();
    if clipboard.is_none() {
        *clipboard = arboard::Clipboard::new().ok();
    }
    if clipboard
        .as_mut()
        .is_some_and(|clipboard| clipboard.set_text(text).is_ok())
    {
        state.copy_notification = Some(format!("Copied {char_count} chars to clipboard"));
        *notification_deadline = Some(Instant::now() + COPY_NOTIFICATION_DURATION);
    }
}

/// Applies Y/N confirmation keys to a pending permission prompt.
///
/// Returns `Some(true)` on approval, `Some(false)` on denial, clearing the prompt either way.
#[must_use]
pub fn handle_permission_event(
    state: &mut AppState,
    event: &Event,
) -> Option<gocode_core::PermissionChoice> {
    state.pending_permission.as_ref()?;
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
        KeyCode::Char('1') => {
            state.pending_permission = None;
            Some(gocode_core::PermissionChoice::AllowOnce)
        }
        _ if key_action_matches(
            &state.preferences,
            gocode_core::KeyAction::Approve,
            *code,
            *modifiers,
        ) || matches!(code, KeyCode::Enter) =>
        {
            state.pending_permission = None;
            Some(gocode_core::PermissionChoice::AllowOnce)
        }
        KeyCode::Char('2') => {
            state.pending_permission = None;
            Some(gocode_core::PermissionChoice::AllowAlways)
        }
        _ if key_action_matches(
            &state.preferences,
            gocode_core::KeyAction::Reject,
            *code,
            *modifiers,
        ) || matches!(code, KeyCode::Esc) =>
        {
            state.pending_permission = None;
            Some(gocode_core::PermissionChoice::Deny)
        }
        _ => None,
    }
}

/// Applies navigation and confirmation keys to a model-requested decision card.
#[must_use]
pub fn handle_guided_question_event(state: &mut AppState, event: &Event) -> Option<String> {
    let question = state.pending_guided_question.as_ref()?;
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return None;
    };
    let count = question.choices.len();
    match code {
        KeyCode::Up | KeyCode::Char('k') if count > 0 => {
            state.selected_guided_choice = state.selected_guided_choice.saturating_sub(1);
            None
        }
        KeyCode::Down | KeyCode::Char('j') if count > 0 => {
            state.selected_guided_choice = (state.selected_guided_choice + 1).min(count - 1);
            None
        }
        KeyCode::Enter if count > 0 => {
            let answer = question.choices[state.selected_guided_choice].label.clone();
            state.pending_guided_question = None;
            Some(answer)
        }
        KeyCode::Esc => {
            state.pending_guided_question = None;
            Some("No option selected; the user cancelled this decision.".into())
        }
        _ => None,
    }
}

/// Applies Y/N confirmation keys to a pending `/worktree remove` prompt.
///
/// Returns the removal target together with `true` on confirmation or `false` on cancellation,
/// clearing the prompt either way.
#[must_use]
pub fn handle_worktree_removal_event(
    state: &mut AppState,
    event: &Event,
) -> Option<(String, bool)> {
    let target = state.pending_worktree_removal.as_ref()?.clone();
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
            state.pending_worktree_removal = None;
            Some((target, true))
        }
        KeyCode::Char('n' | 'N') | KeyCode::Esc => {
            state.pending_worktree_removal = None;
            Some((target, false))
        }
        _ => None,
    }
}

/// Which pending `/agent` action a [`handle_agent_confirm_event`] outcome refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentConfirmAction {
    /// Merge the subagent's worktree branch.
    Apply(String),
    /// Remove the subagent's worktree and metadata.
    Cleanup(String),
}

/// Applies Y/N confirmation keys to a pending `/agent apply` or `/agent cleanup` prompt.
///
/// Returns the action together with `true` on confirmation or `false` on cancellation, clearing
/// the prompt either way.
#[must_use]
pub fn handle_agent_confirm_event(
    state: &mut AppState,
    event: &Event,
) -> Option<(AgentConfirmAction, bool)> {
    let pending = state.pending_agent_confirm.clone()?;
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return None;
    };

    let action = match &pending {
        PendingAgentConfirm::Apply { id, .. } => AgentConfirmAction::Apply(id.clone()),
        PendingAgentConfirm::Cleanup { id, .. } => AgentConfirmAction::Cleanup(id.clone()),
    };

    match code {
        KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
            state.pending_agent_confirm = None;
            Some((action, true))
        }
        KeyCode::Char('n' | 'N') | KeyCode::Esc => {
            state.pending_agent_confirm = None;
            Some((action, false))
        }
        _ => None,
    }
}

/// Outcome of dispatching a terminal event to the guided `/agent apply` conflict resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentConflictEventOutcome {
    /// The resolver isn't shown, or the event isn't one it cares about.
    NotHandled,
    /// The event was consumed by the resolver with no further side effect (navigation, or a key
    /// that's a no-op right now, e.g. Enter before every file is resolved).
    Handled,
    /// Keep one side of the conflict for `file` and stage it.
    Resolve {
        id: String,
        file: String,
        ours: bool,
    },
    /// Every file is resolved; complete the merge.
    Finish(String),
    /// Abort the in-progress merge, discarding every resolution made so far.
    Abort(String),
}

/// Applies navigation and resolution keys to the guided `/agent apply` conflict resolver:
/// Up/Down to move the selection, `o`/`t` to keep our/their side of the selected file, Enter to
/// finish once every file is resolved, Esc/`c` to abort the merge entirely.
///
/// Never mutates `resolution` itself — the caller sends the corresponding `AppCommand` and the
/// resolution is only recorded once the backend confirms the file was actually staged (see
/// [`AppEvent::AgentConflictFileResolved`][gocode_core::AppEvent::AgentConflictFileResolved]), so
/// the popup never shows a file as resolved when the underlying `git checkout`/`add` failed.
///
/// Returns [`AgentConflictEventOutcome::NotHandled`] when the resolver isn't shown or the event
/// isn't a key press.
#[must_use]
pub fn handle_agent_conflict_event(
    state: &mut AppState,
    event: &Event,
) -> AgentConflictEventOutcome {
    let Some(pending) = state.pending_agent_conflict.clone() else {
        return AgentConflictEventOutcome::NotHandled;
    };
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return AgentConflictEventOutcome::NotHandled;
    };

    match code {
        KeyCode::Up => {
            if let Some(conflict) = &mut state.pending_agent_conflict {
                conflict.selected = conflict.selected.saturating_sub(1);
            }
            AgentConflictEventOutcome::Handled
        }
        KeyCode::Down => {
            if let Some(conflict) = &mut state.pending_agent_conflict
                && !conflict.files.is_empty()
            {
                conflict.selected = (conflict.selected + 1).min(conflict.files.len() - 1);
            }
            AgentConflictEventOutcome::Handled
        }
        KeyCode::Char('o' | 'O') => {
            pending
                .files
                .get(pending.selected)
                .map_or(AgentConflictEventOutcome::Handled, |file| {
                    AgentConflictEventOutcome::Resolve {
                        id: pending.id.clone(),
                        file: file.path.clone(),
                        ours: true,
                    }
                })
        }
        KeyCode::Char('t' | 'T') => {
            pending
                .files
                .get(pending.selected)
                .map_or(AgentConflictEventOutcome::Handled, |file| {
                    AgentConflictEventOutcome::Resolve {
                        id: pending.id.clone(),
                        file: file.path.clone(),
                        ours: false,
                    }
                })
        }
        KeyCode::Enter => {
            if pending.files.iter().all(|file| file.resolution.is_some()) {
                AgentConflictEventOutcome::Finish(pending.id)
            } else {
                AgentConflictEventOutcome::Handled
            }
        }
        KeyCode::Esc | KeyCode::Char('c' | 'C') => AgentConflictEventOutcome::Abort(pending.id),
        _ => AgentConflictEventOutcome::Handled,
    }
}

/// Outcome of dispatching a terminal event to a pending `/undo`/`/redo` conflict prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoConflictOutcome {
    /// The user cancelled; nothing further was applied.
    Cancelled,
    /// The user toggled the inline diff; the prompt stays open.
    DiffToggled,
    /// The user forced the transaction through despite the conflict.
    Forced {
        /// `"undo"` or `"redo"`.
        direction: String,
        /// The transaction count to resend, force flag on.
        count: usize,
    },
}

/// Applies force/diff/cancel keys to a pending `/undo`/`/redo` conflict prompt.
#[must_use]
pub fn handle_undo_conflict_event(
    state: &mut AppState,
    event: &Event,
) -> Option<UndoConflictOutcome> {
    state.pending_undo_conflict.as_ref()?;
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return None;
    };

    match code {
        KeyCode::Char('f' | 'F') => {
            let prompt = state.pending_undo_conflict.take()?;
            Some(UndoConflictOutcome::Forced {
                direction: prompt.direction,
                count: prompt.count,
            })
        }
        KeyCode::Char('d' | 'D') => {
            let prompt = state.pending_undo_conflict.as_mut()?;
            prompt.show_diff = !prompt.show_diff;
            Some(UndoConflictOutcome::DiffToggled)
        }
        KeyCode::Char('c' | 'C') | KeyCode::Esc => {
            state.pending_undo_conflict = None;
            Some(UndoConflictOutcome::Cancelled)
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

/// Outcome of dispatching a terminal event to the `/agents` popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsEventOutcome {
    /// The popup is not shown, or the event isn't one it cares about.
    NotHandled,
    /// The event was consumed by the popup with no further side effect.
    Handled,
    /// `r` was pressed: re-request the current list from the runtime.
    Refresh,
}

/// Applies navigation and view-switching keys to the `/agents` popup: Up/Down to move the
/// selection, Enter/Right to open a subagent's detail, Esc/Left to go back or close, `r` to
/// refresh. Never sends a command itself — the caller sends [`gocode_core::AppCommand::AgentList`]
/// on [`AgentsEventOutcome::Refresh`] (including the one implied by opening the popup).
///
/// Returns [`AgentsEventOutcome::NotHandled`] when the popup isn't shown or the event isn't a key
/// press.
#[must_use]
pub fn handle_agents_event(state: &mut AppState, event: &Event) -> AgentsEventOutcome {
    if !state.agents_visible {
        return AgentsEventOutcome::NotHandled;
    }
    let Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        ..
    }) = event
    else {
        return AgentsEventOutcome::NotHandled;
    };

    match state.agents_view {
        AgentsView::List => match code {
            KeyCode::Up => {
                state.agents_selected = state.agents_selected.saturating_sub(1);
                AgentsEventOutcome::Handled
            }
            KeyCode::Down => {
                if !state.agents.is_empty() {
                    state.agents_selected = (state.agents_selected + 1).min(state.agents.len() - 1);
                }
                AgentsEventOutcome::Handled
            }
            KeyCode::Enter | KeyCode::Right => {
                if let Some(record) = state.agents.get(state.agents_selected) {
                    state.agent_detail = Some(record.clone());
                    state.agents_view = AgentsView::Detail;
                    state.agents_detail_scroll = 0;
                    state.agents_next_step_cursor = 0;
                }
                AgentsEventOutcome::Handled
            }
            KeyCode::Char('r') => AgentsEventOutcome::Refresh,
            KeyCode::Esc => {
                state.agents_visible = false;
                AgentsEventOutcome::Handled
            }
            _ => AgentsEventOutcome::Handled,
        },
        AgentsView::Detail => match code {
            KeyCode::Esc | KeyCode::Left => {
                state.agents_view = AgentsView::List;
                state.agents_detail_scroll = 0;
                AgentsEventOutcome::Handled
            }
            KeyCode::Up => {
                state.agents_detail_scroll = state.agents_detail_scroll.saturating_sub(1);
                AgentsEventOutcome::Handled
            }
            KeyCode::Down => {
                state.agents_detail_scroll = state.agents_detail_scroll.saturating_add(1);
                AgentsEventOutcome::Handled
            }
            KeyCode::Char('r') => AgentsEventOutcome::Refresh,
            KeyCode::Char('n' | 'N') => {
                prefill_next_step_spawn(state);
                AgentsEventOutcome::Handled
            }
            _ => AgentsEventOutcome::Handled,
        },
    }
}

/// Turns the `n`th (cycling) suggestion in `agent_detail`'s `result.next_steps` into a ready-to-
/// edit `/agent spawn "<step>"` in the composer, closing the popup so the user lands on it. A
/// no-op when there is no detail open or its result has no next steps to act on.
fn prefill_next_step_spawn(state: &mut AppState) {
    let next_steps_len = state
        .agent_detail
        .as_ref()
        .and_then(|record| record.result.as_ref())
        .map_or(0, |result| result.next_steps.len());
    if next_steps_len == 0 {
        return;
    }
    let index = state.agents_next_step_cursor % next_steps_len;
    state.agents_next_step_cursor += 1;
    let Some(step) = state
        .agent_detail
        .as_ref()
        .and_then(|record| record.result.as_ref())
        .and_then(|result| result.next_steps.get(index).cloned())
    else {
        return;
    };
    state.set_chat_input(format!("/agent spawn \"{step}\""));
    state.agents_visible = false;
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
        || state.pending_guided_question.is_some()
        || state.pending_worktree_removal.is_some()
        || state.pending_agent_confirm.is_some()
        || state.pending_agent_conflict.is_some()
        || state.pending_undo_conflict.is_some()
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

    if key_action_matches(
        &state.preferences,
        gocode_core::KeyAction::OpenHelp,
        *code,
        *modifiers,
    ) {
        return Some(ChatSubmission::Command(SlashCommand::Help));
    }
    if key_action_matches(
        &state.preferences,
        gocode_core::KeyAction::OpenModelPicker,
        *code,
        *modifiers,
    ) {
        return Some(ChatSubmission::Command(SlashCommand::Model));
    }
    if key_action_matches(
        &state.preferences,
        gocode_core::KeyAction::NewConversation,
        *code,
        *modifiers,
    ) {
        return Some(ChatSubmission::Command(SlashCommand::NewSession));
    }
    if key_action_matches(
        &state.preferences,
        gocode_core::KeyAction::OpenCommandList,
        *code,
        *modifiers,
    ) {
        state.set_chat_input("/".into());
        return None;
    }

    let code = if key_action_matches(
        &state.preferences,
        gocode_core::KeyAction::SendMessage,
        *code,
        *modifiers,
    ) {
        KeyCode::Enter
    } else if key_action_matches(
        &state.preferences,
        gocode_core::KeyAction::HistoryPrevious,
        *code,
        *modifiers,
    ) {
        KeyCode::Up
    } else if key_action_matches(
        &state.preferences,
        gocode_core::KeyAction::HistoryNext,
        *code,
        *modifiers,
    ) {
        KeyCode::Down
    } else {
        *code
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
                if name == "/worktree" {
                    let target = arguments.trim().to_string();
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(SlashCommand::Worktree(target)));
                }
                if name == "/undo" {
                    let count = arguments.trim().to_string();
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(SlashCommand::Undo(count)));
                }
                if name == "/redo" {
                    let count = arguments.trim().to_string();
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(SlashCommand::Redo(count)));
                }
                if name == "/debug" {
                    let argument = arguments.trim().to_string();
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(SlashCommand::Debug(argument)));
                }
                if name == "/agent" {
                    let argument = arguments.trim().to_string();
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(SlashCommand::Agent(argument)));
                }
                if name == "/keymap" {
                    let arguments = arguments.trim().to_string();
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(SlashCommand::Keymap(arguments)));
                }
                if name == "/theme" {
                    let arguments = arguments.trim().to_string();
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(SlashCommand::Theme(arguments)));
                }
                if name == "/personality" {
                    let arguments = arguments.trim().to_string();
                    state.clear_chat_input();
                    return Some(ChatSubmission::Command(SlashCommand::Personality(
                        arguments,
                    )));
                }
                if let Some(command) = resolve_custom_command(&state.custom_commands, name) {
                    let body = expand_custom_command(&command.body, arguments.trim());
                    state.clear_chat_input();
                    return Some(ChatSubmission::Prompt(body));
                }
            }
            let text = std::mem::take(&mut state.chat_input);
            state.cursor = 0;
            if state.debug.next_question().is_some() {
                return Some(ChatSubmission::DebugAnswer(text));
            }
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

fn key_action_matches(
    preferences: &gocode_core::Preferences,
    action: gocode_core::KeyAction,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    let Some(binding) = preferences.keymap.get(&action) else {
        return false;
    };
    let normalized = binding.to_ascii_lowercase();
    let mut parts = normalized.split('+').collect::<Vec<_>>();
    let Some(key) = parts.pop() else {
        return false;
    };
    let expected = parts
        .into_iter()
        .fold(KeyModifiers::empty(), |value, modifier| {
            value
                | match modifier {
                    "ctrl" => KeyModifiers::CONTROL,
                    "alt" => KeyModifiers::ALT,
                    "shift" => KeyModifiers::SHIFT,
                    _ => KeyModifiers::empty(),
                }
        });
    if (modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)) != expected {
        return false;
    }
    match key {
        "enter" => code == KeyCode::Enter,
        "esc" => code == KeyCode::Esc,
        "tab" => code == KeyCode::Tab,
        "up" => code == KeyCode::Up,
        "down" => code == KeyCode::Down,
        "left" => code == KeyCode::Left,
        "right" => code == KeyCode::Right,
        "f1" => code == KeyCode::F(1),
        "f2" => code == KeyCode::F(2),
        "f3" => code == KeyCode::F(3),
        "f4" => code == KeyCode::F(4),
        one if one.chars().count() == 1 => {
            matches!(code, KeyCode::Char(character) if character.to_string() == one)
        }
        _ => false,
    }
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
        AgentConfirmAction, AgentConflictEventOutcome, AgentInvocation, AgentsEventOutcome,
        AgentsView, AppState, ChatEntry, ChatSubmission, ConflictFileState, ConflictResolution,
        HelpTab, InputAction, MAX_VISIBLE_SUGGESTIONS, McpAddStep, McpEventOutcome, McpView,
        PendingAgentConfirm, PendingAgentConflict, RunPhase, Screen, SkillsEventOutcome,
        SkillsView, SlashCommand, UpdateEventOutcome, UpdateStage, WorktreeInvocation,
        classify_event, handle_agent_confirm_event, handle_agent_conflict_event,
        handle_agents_event, handle_chat_event, handle_effort_picker_event, handle_help_event,
        handle_mcp_event, handle_model_picker_event, handle_onboarding_event,
        handle_permission_event, handle_session_picker_event, handle_skills_event,
        handle_update_event, handle_worktree_removal_event, parse_agent_command,
        parse_worktree_command, render, run_with_event_source, slash_suggestions,
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
            partial: false,
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

    #[test]
    fn active_run_shows_phase_current_action_and_validation_impact() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.begin_run("add a health endpoint".into());
        state.apply(&AppEvent::ToolActivity {
            id: "edit-1".into(),
            name: "apply_patch".into(),
            status: ToolActivityStatus::Succeeded,
            detail: "updated src/health.rs".into(),
        });
        state.apply(&AppEvent::FileChanged {
            path: "src/health.rs".into(),
            kind: "modified".into(),
        });
        state.apply(&AppEvent::ToolActivity {
            id: "test-1".into(),
            name: "run_command".into(),
            status: ToolActivityStatus::Started,
            detail: "cargo test".into(),
        });

        let mut terminal = Terminal::new(TestBackend::new(100, 24)).expect("test terminal");
        terminal
            .draw(|frame| render(frame, &state))
            .expect("progress view should render");
        let output = buffer_text(&terminal);

        assert!(output.contains("Validando"));
        assert!(output.contains("cargo test"));
        assert!(output.contains("1 arquivo alterado"));
        assert!(output.contains("validação pendente"));
        assert!(output.contains("0s"));
    }

    #[test]
    fn non_validation_command_is_not_presented_as_validation() {
        let mut state = AppState::default();
        state.begin_run("inspect repository state".into());
        state.apply(&AppEvent::ToolActivity {
            id: "command-1".into(),
            name: "run_command".into(),
            status: ToolActivityStatus::Started,
            detail: "git status".into(),
        });

        assert_eq!(state.run_visibility.phase, RunPhase::Understanding);
        assert!(!state.run_visibility.validation_pending);
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
        assert!(busy_lines.iter().any(|line| line.trim() == "hello"));
    }

    #[test]
    fn user_messages_are_tagged_without_a_literal_you_prefix() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.begin_run("hello there".into());

        let tagged = super::compose_lines_tagged(&state);
        assert!(
            tagged
                .iter()
                .any(|(text, kind)| *kind == super::LineKind::User && text.trim() == "hello there")
        );
        assert!(tagged.iter().all(|(text, _)| !text.starts_with("You:")));
    }

    #[test]
    fn wrapped_history_is_cached_until_content_or_width_changes() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.begin_run("hi".into());

        let _ = super::wrapped_history_lines(&state, 40);
        let hash_after_first_draw = state.history_wrap_cache.borrow().source_hash;

        // Redrawing at the same width without touching the conversation must hit the cache.
        let _ = super::wrapped_history_lines(&state, 40);
        assert_eq!(
            state.history_wrap_cache.borrow().source_hash,
            hash_after_first_draw
        );

        // New content invalidates the cache.
        state.entries.push(ChatEntry::Info("new".into()));
        let _ = super::wrapped_history_lines(&state, 40);
        assert_ne!(
            state.history_wrap_cache.borrow().source_hash,
            hash_after_first_draw
        );

        // A width change invalidates it too, even with unchanged content.
        let hash_after_second_draw = state.history_wrap_cache.borrow().source_hash;
        let _ = super::wrapped_history_lines(&state, 20);
        assert_eq!(
            state.history_wrap_cache.borrow().source_hash,
            hash_after_second_draw
        );
        assert_eq!(state.history_wrap_cache.borrow().width, 20);
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
    fn shift_tab_cycles_the_permission_mode_through_auto_plan_approve_manual() {
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
            Some(PermissionMode::Manual)
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
    fn streaming_reasoning_deltas_append_to_one_entry_and_a_turn_with_only_reasoning_is_not_empty()
    {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.begin_run("hi".into());

        state.apply(&AppEvent::AssistantReasoningDelta("hmm".into()));
        state.apply(&AppEvent::AssistantReasoningDelta(", let me think".into()));

        assert_eq!(
            state.entries.last(),
            Some(&ChatEntry::Reasoning("hmm, let me think".into()))
        );

        state.apply(&AppEvent::AgentCompleted {
            final_text: None,
            turns: 1,
            tool_calls: 0,
            failed_tool_calls: 0,
            last_input_tokens: None,
            partial: false,
        });

        assert!(
            super::compose_lines(&state)
                .iter()
                .any(|line| line.contains("hmm, let me think"))
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
            scope_label: "comandos de risco médio".into(),
        });

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Char('x'))),
            None
        );
        assert_eq!(
            handle_permission_event(&mut state, &press(KeyCode::Char('y'))),
            Some(gocode_core::PermissionChoice::AllowOnce)
        );
        assert!(state.pending_permission.is_none());
    }

    #[test]
    fn permission_denial_via_n_clears_the_prompt() {
        let mut state = AppState::default();
        state.apply(&AppEvent::PermissionRequested {
            summary: "run: rm".into(),
            working_directory: ".".into(),
            scope_label: "comandos de alto risco".into(),
        });

        assert_eq!(
            handle_permission_event(&mut state, &press(KeyCode::Char('n'))),
            Some(gocode_core::PermissionChoice::Deny)
        );
        assert!(state.pending_permission.is_none());
    }

    #[test]
    fn permission_prompt_can_allow_a_command_risk_for_the_session() {
        let mut state = AppState::default();
        state.apply(&AppEvent::PermissionRequested {
            summary: "run: npm install".into(),
            working_directory: ".".into(),
            scope_label: "comandos de risco médio".into(),
        });

        assert_eq!(
            handle_permission_event(&mut state, &press(KeyCode::Char('2'))),
            Some(gocode_core::PermissionChoice::AllowAlways)
        );
        assert!(state.pending_permission.is_none());
    }

    #[test]
    fn cancellation_clears_a_stale_permission_prompt() {
        let mut state = AppState::default();
        state.apply(&AppEvent::PermissionRequested {
            summary: "run: rm".into(),
            working_directory: ".".into(),
            scope_label: "comandos de alto risco".into(),
        });

        state.apply(&AppEvent::AgentCancelled);

        assert!(state.pending_permission.is_none());
    }

    #[test]
    fn agent_stopped_releases_the_running_tools_indicator_left_by_a_limit_warning() {
        // A run that hits a safety limit (max turns, a detected loop, ...) sends an AgentWarning
        // with the explanation, then AgentStopped as the terminal signal — mirroring what a
        // successful or cancelled run would otherwise do to release the busy indicator.
        let mut state = AppState::default();
        state.apply(&AppEvent::AgentStateChanged(
            AgentActivityState::RunningTools,
        ));
        assert!(state.activity.is_some());

        state.apply(&AppEvent::AgentWarning(
            "run stopped: the maximum number of turns was reached".into(),
        ));
        state.apply(&AppEvent::AgentStopped);

        assert!(state.activity.is_none());
        assert!(
            state
                .entries
                .iter()
                .any(|entry| matches!(entry, ChatEntry::Warning(message) if message.contains("maximum number of turns")))
        );
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
    fn worktree_with_no_argument_is_recognized_and_carries_empty_text() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/worktree".into(),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::Worktree(
                String::new()
            )))
        );
        assert!(state.chat_input.is_empty());
    }

    #[test]
    fn worktree_with_arguments_carries_them_through_as_raw_text() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/worktree list".into(),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::Worktree(
                "list".into()
            )))
        );
    }

    #[test]
    fn agent_spawn_parses_task_and_flags_in_any_order() {
        assert_eq!(
            parse_agent_command("spawn investigate flaky login test --mode research"),
            AgentInvocation::Spawn {
                task: "investigate flaky login test".into(),
                mode: gocode_core::SubagentMode::Research,
                model: None,
                worktree: false,
            }
        );
        assert_eq!(
            parse_agent_command("spawn --mode implement --worktree add a doc comment"),
            AgentInvocation::Spawn {
                task: "add a doc comment".into(),
                mode: gocode_core::SubagentMode::Implement,
                model: None,
                worktree: true,
            }
        );
    }

    #[test]
    fn agent_worktree_flag_is_rejected_outside_implement_mode() {
        assert_eq!(
            parse_agent_command("spawn --worktree --mode research look around"),
            AgentInvocation::Invalid("--worktree is only valid with --mode implement.".into())
        );
    }

    #[test]
    fn agent_apply_without_confirm_requests_the_diff_and_with_confirm_merges() {
        assert_eq!(
            parse_agent_command("apply a1b2c3d4"),
            AgentInvocation::Apply {
                id: "a1b2c3d4".into(),
                confirm: false,
            }
        );
        assert_eq!(
            parse_agent_command("apply a1b2c3d4 confirm"),
            AgentInvocation::Apply {
                id: "a1b2c3d4".into(),
                confirm: true,
            }
        );
    }

    #[test]
    fn agent_message_joins_the_remaining_words_into_the_text() {
        assert_eq!(
            parse_agent_command("message a1b2c3d4 focus on the retry path"),
            AgentInvocation::Message {
                id: "a1b2c3d4".into(),
                text: "focus on the retry path".into(),
            }
        );
    }

    #[test]
    fn agent_slash_command_dispatches_to_the_matching_app_command() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/agent stop a1b2c3d4".into(),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::Agent(
                "stop a1b2c3d4".into()
            )))
        );
    }

    #[test]
    fn bare_agents_command_is_recognized() {
        let mut state = AppState {
            screen: Screen::Chat,
            chat_input: "/agents".into(),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Enter)),
            Some(ChatSubmission::Command(SlashCommand::Agents))
        );
    }

    fn sample_agent_records() -> Vec<gocode_core::SubagentRecord> {
        vec![
            gocode_core::SubagentRecord::new(
                "session-1".into(),
                "investigate flaky login test".into(),
                gocode_core::SubagentMode::Research,
                "test-model".into(),
                true,
                gocode_core::PermissionMode::Auto,
            ),
            gocode_core::SubagentRecord::new(
                "session-1".into(),
                "add a doc comment".into(),
                gocode_core::SubagentMode::Implement,
                "test-model".into(),
                false,
                gocode_core::PermissionMode::Auto,
            ),
        ]
    }

    #[test]
    fn agent_list_available_event_populates_the_popup_and_clamps_selection() {
        let mut state = AppState {
            agents_selected: 5,
            ..AppState::default()
        };
        state.apply(&AppEvent::AgentListAvailable(sample_agent_records()));
        assert_eq!(state.agents.len(), 2);
        assert_eq!(state.agents_selected, 1);
    }

    #[test]
    fn agent_detail_available_opens_the_popup_directly_to_detail() {
        let mut state = AppState::default();
        let record = sample_agent_records().remove(0);
        state.apply(&AppEvent::AgentDetailAvailable {
            id: record.id.clone(),
            record: Some(Box::new(record.clone())),
        });
        assert!(state.agents_visible);
        assert_eq!(state.agents_view, AgentsView::Detail);
        assert_eq!(state.agent_detail, Some(record));
    }

    #[test]
    fn agent_detail_available_with_no_match_warns_and_does_not_open_the_popup() {
        let mut state = AppState::default();
        state.apply(&AppEvent::AgentDetailAvailable {
            id: "deadbeef".into(),
            record: None,
        });
        assert!(!state.agents_visible);
        assert!(state.agent_detail.is_none());
        assert!(
            matches!(state.entries.last(), Some(ChatEntry::Warning(message)) if message.contains("deadbeef"))
        );
    }

    #[test]
    fn agent_list_refresh_keeps_the_open_detail_in_sync_by_id() {
        let mut records = sample_agent_records();
        let target = records[0].clone();
        let mut state = AppState {
            agent_detail: Some(target.clone()),
            ..AppState::default()
        };

        records[0].status = gocode_core::SubagentStatus::Completed;
        state.apply(&AppEvent::AgentListAvailable(records.clone()));

        assert_eq!(
            state.agent_detail.as_ref().map(|record| record.status),
            Some(gocode_core::SubagentStatus::Completed)
        );
    }

    #[test]
    fn entering_detail_from_the_list_snapshots_the_selected_record() {
        let mut state = AppState {
            agents_visible: true,
            agents_view: AgentsView::List,
            agents: sample_agent_records(),
            agents_selected: 1,
            ..AppState::default()
        };

        assert_eq!(
            handle_agents_event(&mut state, &press(KeyCode::Enter)),
            AgentsEventOutcome::Handled
        );
        assert_eq!(
            state
                .agent_detail
                .as_ref()
                .map(|record| record.task_summary.clone()),
            Some("add a doc comment".to_string())
        );
    }

    fn agent_with_next_steps(steps: &[&str]) -> gocode_core::SubagentRecord {
        let mut record = sample_agent_records().remove(0);
        record.status = gocode_core::SubagentStatus::Completed;
        record.result = Some(gocode_core::SubagentResult {
            summary: "investigated the login flow".into(),
            next_steps: steps.iter().map(|step| (*step).to_string()).collect(),
            ..gocode_core::SubagentResult::default()
        });
        record
    }

    #[test]
    fn pressing_n_prefills_agent_spawn_with_the_first_next_step_and_closes_the_popup() {
        let mut state = AppState {
            agents_visible: true,
            agents_view: AgentsView::Detail,
            agent_detail: Some(agent_with_next_steps(&["investigate the signup flow too"])),
            ..AppState::default()
        };

        assert_eq!(
            handle_agents_event(&mut state, &press(KeyCode::Char('n'))),
            AgentsEventOutcome::Handled
        );

        assert_eq!(
            state.chat_input,
            "/agent spawn \"investigate the signup flow too\""
        );
        assert!(!state.agents_visible);
    }

    #[test]
    fn pressing_n_repeatedly_cycles_through_every_next_step() {
        let mut state = AppState {
            agents_visible: true,
            agents_view: AgentsView::Detail,
            agent_detail: Some(agent_with_next_steps(&["step one", "step two"])),
            ..AppState::default()
        };

        let _ = handle_agents_event(&mut state, &press(KeyCode::Char('n')));
        assert_eq!(state.chat_input, "/agent spawn \"step one\"");

        // Re-open the popup (as if the user reconsidered) and press n again.
        state.agents_visible = true;
        let _ = handle_agents_event(&mut state, &press(KeyCode::Char('n')));
        assert_eq!(state.chat_input, "/agent spawn \"step two\"");

        state.agents_visible = true;
        let _ = handle_agents_event(&mut state, &press(KeyCode::Char('n')));
        assert_eq!(state.chat_input, "/agent spawn \"step one\"");
    }

    #[test]
    fn pressing_n_with_no_next_steps_is_a_no_op() {
        let mut state = AppState {
            agents_visible: true,
            agents_view: AgentsView::Detail,
            agent_detail: Some(agent_with_next_steps(&[])),
            ..AppState::default()
        };

        assert_eq!(
            handle_agents_event(&mut state, &press(KeyCode::Char('n'))),
            AgentsEventOutcome::Handled
        );
        assert!(state.chat_input.is_empty());
        assert!(state.agents_visible);
    }

    #[test]
    fn agent_detail_available_resets_the_next_step_cursor() {
        let mut state = AppState {
            agents_next_step_cursor: 7,
            ..AppState::default()
        };
        let record = agent_with_next_steps(&["a", "b"]);
        state.apply(&AppEvent::AgentDetailAvailable {
            id: record.id.clone(),
            record: Some(Box::new(record)),
        });
        assert_eq!(state.agents_next_step_cursor, 0);
    }

    #[test]
    fn agents_list_navigation_stays_within_bounds() {
        let mut state = AppState {
            agents_visible: true,
            agents_view: AgentsView::List,
            agents: sample_agent_records(),
            agents_selected: 0,
            ..AppState::default()
        };

        assert_eq!(
            handle_agents_event(&mut state, &press(KeyCode::Up)),
            AgentsEventOutcome::Handled
        );
        assert_eq!(state.agents_selected, 0);

        let _ = handle_agents_event(&mut state, &press(KeyCode::Down));
        let _ = handle_agents_event(&mut state, &press(KeyCode::Down));
        assert_eq!(state.agents_selected, 1);
    }

    #[test]
    fn enter_opens_agent_detail_then_esc_returns_to_the_list() {
        let mut state = AppState {
            agents_visible: true,
            agents_view: AgentsView::List,
            agents: sample_agent_records(),
            agents_selected: 0,
            ..AppState::default()
        };

        assert_eq!(
            handle_agents_event(&mut state, &press(KeyCode::Enter)),
            AgentsEventOutcome::Handled
        );
        assert_eq!(state.agents_view, AgentsView::Detail);

        assert_eq!(
            handle_agents_event(&mut state, &press(KeyCode::Esc)),
            AgentsEventOutcome::Handled
        );
        assert_eq!(state.agents_view, AgentsView::List);
    }

    #[test]
    fn esc_on_the_agents_list_closes_the_whole_popup() {
        let mut state = AppState {
            agents_visible: true,
            agents_view: AgentsView::List,
            agents: sample_agent_records(),
            ..AppState::default()
        };

        assert_eq!(
            handle_agents_event(&mut state, &press(KeyCode::Esc)),
            AgentsEventOutcome::Handled
        );
        assert!(!state.agents_visible);
    }

    #[test]
    fn pressing_r_requests_a_refresh_from_either_view() {
        let mut state = AppState {
            agents_visible: true,
            agents_view: AgentsView::List,
            agents: sample_agent_records(),
            ..AppState::default()
        };
        assert_eq!(
            handle_agents_event(&mut state, &press(KeyCode::Char('r'))),
            AgentsEventOutcome::Refresh
        );

        state.agents_view = AgentsView::Detail;
        assert_eq!(
            handle_agents_event(&mut state, &press(KeyCode::Char('r'))),
            AgentsEventOutcome::Refresh
        );
    }

    #[test]
    fn agents_popup_is_not_handled_when_closed() {
        let mut state = AppState::default();
        assert_eq!(
            handle_agents_event(&mut state, &press(KeyCode::Down)),
            AgentsEventOutcome::NotHandled
        );
    }

    #[test]
    fn worktree_with_no_argument_suggests_a_name_from_the_session() {
        let state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        // A fresh session has no name yet, so the suggestion falls back to a timestamp.
        match parse_worktree_command("", &state) {
            WorktreeInvocation::SuggestName(name) => assert!(name.starts_with("task-")),
            other => panic!("expected a name suggestion, got {other:?}"),
        }
    }

    #[test]
    fn worktree_suggests_a_name_derived_from_a_named_session() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };
        state.apply(&AppEvent::SessionSwitched {
            id: "abc".into(),
            name: "Fix the flaky login test!".into(),
            transition: gocode_core::SessionTransition::New,
            history: Vec::new(),
        });

        match parse_worktree_command("", &state) {
            WorktreeInvocation::SuggestName(name) => {
                assert_eq!(name, "fix-the-flaky-login-test");
            }
            other => panic!("expected a name suggestion, got {other:?}"),
        }
    }

    #[test]
    fn worktree_list_is_parsed_as_the_list_subcommand() {
        let state = AppState::default();
        assert_eq!(
            parse_worktree_command("list", &state),
            WorktreeInvocation::List
        );
    }

    #[test]
    fn worktree_switch_carries_the_target_through() {
        let state = AppState::default();
        assert_eq!(
            parse_worktree_command("switch my-task", &state),
            WorktreeInvocation::Switch("my-task".into())
        );
        assert!(matches!(
            parse_worktree_command("switch", &state),
            WorktreeInvocation::Invalid(_)
        ));
    }

    #[test]
    fn worktree_remove_requires_confirmation_before_it_is_sent() {
        let state = AppState::default();
        assert_eq!(
            parse_worktree_command("remove my-task", &state),
            WorktreeInvocation::ConfirmRemove("my-task".into())
        );
        assert!(matches!(
            parse_worktree_command("remove", &state),
            WorktreeInvocation::Invalid(_)
        ));
    }

    #[test]
    fn worktree_create_defaults_to_a_new_branch_from_the_current_one() {
        let state = AppState::default();
        assert_eq!(
            parse_worktree_command("my-task", &state),
            WorktreeInvocation::Create {
                name: "my-task".into(),
                branch: gocode_core::WorktreeBranchSource::New,
            }
        );
    }

    #[test]
    fn worktree_create_uses_an_existing_branch_when_given_one() {
        let state = AppState::default();
        assert_eq!(
            parse_worktree_command("my-task some-existing-branch", &state),
            WorktreeInvocation::Create {
                name: "my-task".into(),
                branch: gocode_core::WorktreeBranchSource::Existing("some-existing-branch".into()),
            }
        );
    }

    #[test]
    fn worktree_removal_confirmation_resolves_on_yes_and_cancels_on_no() {
        let mut state = AppState {
            pending_worktree_removal: Some("my-task".into()),
            ..AppState::default()
        };

        assert_eq!(
            handle_worktree_removal_event(&mut state, &press(KeyCode::Char('n'))),
            Some(("my-task".into(), false))
        );
        assert!(state.pending_worktree_removal.is_none());

        state.pending_worktree_removal = Some("my-task".into());
        assert_eq!(
            handle_worktree_removal_event(&mut state, &press(KeyCode::Char('y'))),
            Some(("my-task".into(), true))
        );
        assert!(state.pending_worktree_removal.is_none());
    }

    #[test]
    fn worktree_removal_confirmation_blocks_chat_input_while_pending() {
        let mut state = AppState {
            screen: Screen::Chat,
            pending_worktree_removal: Some("my-task".into()),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Char('x'))),
            None
        );
    }

    #[test]
    fn agent_diff_ready_opens_a_confirm_prompt_instead_of_logging_immediately() {
        let mut state = AppState::default();
        state.apply(&AppEvent::AgentDiffReady {
            id: "a1b2c3d4".into(),
            diff: "+added line".into(),
        });
        assert_eq!(
            state.pending_agent_confirm,
            Some(PendingAgentConfirm::Apply {
                id: "a1b2c3d4".into(),
                diff: "+added line".into(),
            })
        );
    }

    #[test]
    fn agent_cleanup_warning_opens_a_confirm_prompt() {
        let mut state = AppState::default();
        state.apply(&AppEvent::AgentCleanupWarning {
            id: "a1b2c3d4".into(),
            message: "This removes the worktree...".into(),
        });
        assert_eq!(
            state.pending_agent_confirm,
            Some(PendingAgentConfirm::Cleanup {
                id: "a1b2c3d4".into(),
                message: "This removes the worktree...".into(),
            })
        );
    }

    #[test]
    fn agent_apply_confirmation_resolves_on_yes_and_cancels_on_no() {
        let mut state = AppState {
            pending_agent_confirm: Some(PendingAgentConfirm::Apply {
                id: "a1b2c3d4".into(),
                diff: "+added line".into(),
            }),
            ..AppState::default()
        };

        assert_eq!(
            handle_agent_confirm_event(&mut state, &press(KeyCode::Char('n'))),
            Some((AgentConfirmAction::Apply("a1b2c3d4".into()), false))
        );
        assert!(state.pending_agent_confirm.is_none());

        state.pending_agent_confirm = Some(PendingAgentConfirm::Apply {
            id: "a1b2c3d4".into(),
            diff: "+added line".into(),
        });
        assert_eq!(
            handle_agent_confirm_event(&mut state, &press(KeyCode::Char('y'))),
            Some((AgentConfirmAction::Apply("a1b2c3d4".into()), true))
        );
        assert!(state.pending_agent_confirm.is_none());
    }

    #[test]
    fn agent_cleanup_confirmation_blocks_chat_input_while_pending() {
        let mut state = AppState {
            screen: Screen::Chat,
            pending_agent_confirm: Some(PendingAgentConfirm::Cleanup {
                id: "a1b2c3d4".into(),
                message: "This removes the worktree...".into(),
            }),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Char('x'))),
            None
        );
    }

    fn sample_conflict() -> PendingAgentConflict {
        PendingAgentConflict {
            id: "a1b2c3d4".into(),
            files: vec![
                ConflictFileState {
                    path: "src/foo.rs".into(),
                    resolution: None,
                },
                ConflictFileState {
                    path: "src/bar.rs".into(),
                    resolution: None,
                },
            ],
            selected: 0,
        }
    }

    #[test]
    fn agent_merge_conflict_event_opens_the_guided_resolver() {
        let mut state = AppState::default();
        state.apply(&AppEvent::AgentMergeConflict {
            id: "a1b2c3d4".into(),
            files: vec!["src/foo.rs".into(), "src/bar.rs".into()],
        });
        let conflict = state
            .pending_agent_conflict
            .expect("conflict resolver should be open");
        assert_eq!(conflict.id, "a1b2c3d4");
        assert_eq!(conflict.files.len(), 2);
        assert!(conflict.files.iter().all(|file| file.resolution.is_none()));
    }

    #[test]
    fn conflict_file_resolved_event_records_the_chosen_side() {
        let mut state = AppState {
            pending_agent_conflict: Some(sample_conflict()),
            ..AppState::default()
        };
        state.apply(&AppEvent::AgentConflictFileResolved {
            id: "a1b2c3d4".into(),
            file: "src/bar.rs".into(),
            ours: false,
        });
        let conflict = state.pending_agent_conflict.expect("still open");
        assert_eq!(conflict.files[0].resolution, None);
        assert_eq!(
            conflict.files[1].resolution,
            Some(ConflictResolution::Theirs)
        );
    }

    #[test]
    fn merge_finished_event_closes_the_resolver_and_logs_the_outcome() {
        let mut state = AppState {
            pending_agent_conflict: Some(sample_conflict()),
            ..AppState::default()
        };
        state.apply(&AppEvent::AgentMergeFinished {
            id: "a1b2c3d4".into(),
            applied: true,
            message: "Applied subagent a1b2c3d4's changes.".into(),
        });
        assert!(state.pending_agent_conflict.is_none());
        assert!(matches!(state.entries.last(), Some(ChatEntry::Info(_))));
    }

    #[test]
    fn conflict_navigation_stays_within_bounds() {
        let mut state = AppState {
            pending_agent_conflict: Some(sample_conflict()),
            ..AppState::default()
        };

        assert_eq!(
            handle_agent_conflict_event(&mut state, &press(KeyCode::Up)),
            AgentConflictEventOutcome::Handled
        );
        assert_eq!(state.pending_agent_conflict.as_ref().unwrap().selected, 0);

        assert_eq!(
            handle_agent_conflict_event(&mut state, &press(KeyCode::Down)),
            AgentConflictEventOutcome::Handled
        );
        assert_eq!(state.pending_agent_conflict.as_ref().unwrap().selected, 1);

        assert_eq!(
            handle_agent_conflict_event(&mut state, &press(KeyCode::Down)),
            AgentConflictEventOutcome::Handled
        );
        assert_eq!(state.pending_agent_conflict.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn pressing_o_and_t_request_resolving_the_selected_file() {
        let mut state = AppState {
            pending_agent_conflict: Some(sample_conflict()),
            ..AppState::default()
        };

        assert_eq!(
            handle_agent_conflict_event(&mut state, &press(KeyCode::Char('o'))),
            AgentConflictEventOutcome::Resolve {
                id: "a1b2c3d4".into(),
                file: "src/foo.rs".into(),
                ours: true,
            }
        );

        let _ = handle_agent_conflict_event(&mut state, &press(KeyCode::Down));
        assert_eq!(
            handle_agent_conflict_event(&mut state, &press(KeyCode::Char('t'))),
            AgentConflictEventOutcome::Resolve {
                id: "a1b2c3d4".into(),
                file: "src/bar.rs".into(),
                ours: false,
            }
        );
    }

    #[test]
    fn enter_is_a_no_op_until_every_file_is_resolved_then_finishes() {
        let mut conflict = sample_conflict();
        let mut state = AppState {
            pending_agent_conflict: Some(conflict.clone()),
            ..AppState::default()
        };

        assert_eq!(
            handle_agent_conflict_event(&mut state, &press(KeyCode::Enter)),
            AgentConflictEventOutcome::Handled
        );

        conflict.files[0].resolution = Some(ConflictResolution::Ours);
        conflict.files[1].resolution = Some(ConflictResolution::Theirs);
        state.pending_agent_conflict = Some(conflict);

        assert_eq!(
            handle_agent_conflict_event(&mut state, &press(KeyCode::Enter)),
            AgentConflictEventOutcome::Finish("a1b2c3d4".into())
        );
    }

    #[test]
    fn esc_aborts_the_merge_even_with_unresolved_files() {
        let mut state = AppState {
            pending_agent_conflict: Some(sample_conflict()),
            ..AppState::default()
        };

        assert_eq!(
            handle_agent_conflict_event(&mut state, &press(KeyCode::Esc)),
            AgentConflictEventOutcome::Abort("a1b2c3d4".into())
        );
    }

    #[test]
    fn conflict_resolver_blocks_chat_input_while_pending() {
        let mut state = AppState {
            screen: Screen::Chat,
            pending_agent_conflict: Some(sample_conflict()),
            ..AppState::default()
        };

        assert_eq!(
            handle_chat_event(&mut state, &press(KeyCode::Char('x'))),
            None
        );
    }

    #[test]
    fn worktree_list_is_rendered_as_an_info_entry() {
        let mut state = AppState::default();
        state.apply(&AppEvent::WorktreeListAvailable(vec![
            gocode_core::WorktreeSummary {
                path: "/code/myapp".into(),
                branch: Some("main".into()),
                is_main: true,
            },
            gocode_core::WorktreeSummary {
                path: "/code/myapp-worktrees/my-task".into(),
                branch: Some("my-task".into()),
                is_main: false,
            },
        ]));

        let ChatEntry::Info(text) = state.entries.last().expect("an entry should be pushed") else {
            panic!("expected an Info entry");
        };
        assert!(text.contains("/code/myapp"));
        assert!(text.contains("(main)"));
        assert!(text.contains("my-task"));
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
            transition: gocode_core::SessionTransition::Resumed,
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
            transition: gocode_core::SessionTransition::New,
            history: Vec::new(),
        });
        assert_eq!(state.entries.len(), 1);
        assert_eq!(
            state.entries[0],
            ChatEntry::Info("Started session: New session".into())
        );
    }

    #[test]
    fn forking_a_session_replays_its_history_and_announces_the_fork() {
        let mut state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };

        state.apply(&AppEvent::SessionSwitched {
            id: "session-1-fork".into(),
            name: "implement JWT auth (fork)".into(),
            transition: gocode_core::SessionTransition::Forked,
            history: vec![ChatMessage::User("implement JWT auth".into())],
        });

        assert_eq!(state.entries.len(), 2);
        assert_eq!(
            state.entries[1],
            ChatEntry::Info("Forked session: implement JWT auth (fork)".into())
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
            partial: false,
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
    fn the_effort_slash_command_resolves_and_skips_the_model_picker() {
        use super::{EFFORT_OPTIONS, handle_effort_picker_event, resolve_slash_command};

        assert_eq!(resolve_slash_command("/effort"), Some(SlashCommand::Effort));

        let mut state = AppState {
            screen: Screen::EffortPicker,
            model_flow_pending_effort: false,
            pending_model: None,
            current_effort: Some("high".into()),
            selected_effort: EFFORT_OPTIONS
                .iter()
                .position(|(_, value)| *value == Some("high"))
                .unwrap_or(0),
            ..AppState::default()
        };

        assert_eq!(state.selected_effort, 3);
        assert_eq!(
            handle_effort_picker_event(&mut state, &press(KeyCode::Enter)),
            Some(Some("high".to_string()))
        );
        assert!(state.pending_model.is_none());
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
