//! Terminal user interface for Gocode.

use crossterm::event;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use gocode_core::{AppCommand, AppEvent};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    widgets::{Block, Borders, Paragraph},
};
use std::fmt::Write;
use std::time::Duration;
use tokio::sync::mpsc;

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
    /// Main conversational interface.
    Chat,
}

/// Renderable interface state derived from application events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppState {
    /// Visible top-level screen.
    pub screen: Screen,
    credential_input: String,
    models: Vec<String>,
    selected_model: usize,
    assistant_text: String,
    chat_input: String,
    status: Option<String>,
}

/// Outcome of classifying one terminal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// Keep the application running; a resize or non-actionable event may trigger a redraw.
    Continue,
    /// Exit the application normally.
    Exit,
    /// Interrupt active work before exiting the application.
    Interrupt,
}

impl AppState {
    /// Applies a normalized application event to the render state.
    pub fn apply(&mut self, event: &AppEvent) {
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
                self.status = Some(format!("Model: {model}"));
            }
            AppEvent::AssistantTextDelta(delta) => self.assistant_text.push_str(delta),
            AppEvent::ProviderFailed(message) => self.status = Some(message.clone()),
        }
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

/// Renders the active application screen.
pub fn render(frame: &mut Frame, state: &AppState) {
    let content = match state.screen {
        Screen::Boot => "Starting Gocode...".into(),
        Screen::Onboarding => "NVIDIA API key:\n\nEnter your key. It will be stored only in your system credential store after validation.\n\n".to_string()
            + &"•".repeat(state.credential_input.chars().count()),
        Screen::ModelPicker => {
            let visible_rows = usize::from(frame.area().height.saturating_sub(3)).max(1);
            let first_visible = state.selected_model.saturating_sub(visible_rows - 1);
            state.models[first_visible..]
            .iter()
            .enumerate()
            .take(visible_rows)
            .map(|(offset, model)| {
                let index = first_visible + offset;
                format!("{} {model}", if index == state.selected_model { ">" } else { " " })
            })
            .collect::<Vec<_>>()
            .join("\n")
        }
        Screen::Chat => {
            let mut content = if state.assistant_text.is_empty() {
                "What can I help you build?".into()
            } else {
                state.assistant_text.clone()
            };
            if let Some(status) = &state.status {
                let _ = write!(content, "\n\n{status}");
            }
            let _ = write!(content, "\n\n> {}", state.chat_input);
            content
        }
    };
    let title = match state.screen {
        Screen::Boot => "Gocode",
        Screen::Onboarding => "Gocode · NVIDIA setup",
        Screen::ModelPicker => "Gocode · Select NVIDIA model",
        Screen::Chat => "Gocode · Chat",
    };

    frame.render_widget(
        Paragraph::new(content).block(Block::default().title(title).borders(Borders::ALL)),
        frame.area(),
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
            action @ (InputAction::Exit | InputAction::Interrupt) => return Ok(action),
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
fn run_terminal(
    mut event_rx: mpsc::Receiver<AppEvent>,
    command_tx: mpsc::Sender<AppCommand>,
) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let _terminal_guard = TerminalGuard;
    let mut state = AppState::default();

    loop {
        terminal
            .draw(|frame| render(frame, &state))
            .map_err(std::io::Error::other)?;

        while let Ok(app_event) = event_rx.try_recv() {
            state.apply(&app_event);
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }

        let terminal_event = event::read()?;
        if let Event::Resize(columns, rows) = terminal_event {
            command_tx
                .blocking_send(AppCommand::Resize { columns, rows })
                .map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        format!("runtime command channel closed: {error}"),
                    )
                })?;
        }

        if let Some(credential) = handle_onboarding_event(&mut state, &terminal_event) {
            command_tx
                .blocking_send(AppCommand::SubmitCredential(credential))
                .map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        format!("runtime command channel closed: {error}"),
                    )
                })?;
            continue;
        }

        if let Some(model) = handle_model_picker_event(&mut state, &terminal_event) {
            command_tx
                .blocking_send(AppCommand::SelectModel(model))
                .map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        format!("runtime command channel closed: {error}"),
                    )
                })?;
            continue;
        }

        if let Some(message) = handle_chat_event(&mut state, &terminal_event) {
            command_tx
                .blocking_send(AppCommand::SubmitChat(message))
                .map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        format!("runtime command channel closed: {error}"),
                    )
                })?;
            continue;
        }

        if state.screen == Screen::Chat
            && matches!(
                terminal_event,
                Event::Key(KeyEvent {
                    code: KeyCode::Esc,
                    kind: KeyEventKind::Press,
                    ..
                })
            )
        {
            command_tx
                .blocking_send(AppCommand::CancelProviderRequest)
                .map_err(std::io::Error::other)?;
            continue;
        }

        match classify_event(&terminal_event) {
            InputAction::Continue => {}
            InputAction::Exit | InputAction::Interrupt => {
                command_tx
                    .blocking_send(AppCommand::Exit)
                    .map_err(|error| {
                        std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            format!("runtime command channel closed: {error}"),
                        )
                    })?;
                return Ok(());
            }
        }
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

/// Installs a panic hook that restores the terminal before Rust prints the panic report.
///
/// Call this once during application bootstrap, before entering terminal mode.
pub fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |panic_info| {
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

    if matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            ..
        })
    ) {
        return InputAction::Exit;
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

/// Applies text input to the chat composer and returns a prompt on Enter.
#[must_use]
pub fn handle_chat_event(state: &mut AppState, event: &Event) -> Option<String> {
    if state.screen != Screen::Chat {
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
            state.chat_input.push(*character);
        }
        KeyCode::Backspace => {
            state.chat_input.pop();
        }
        KeyCode::Enter if !state.chat_input.is_empty() => {
            return Some(std::mem::take(&mut state.chat_input));
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use gocode_core::AppEvent;
    use ratatui::{Terminal, backend::TestBackend};

    use super::{AppState, InputAction, Screen, classify_event, render, run_with_event_source};

    #[test]
    fn boot_lifecycle_events_render_boot_then_chat() {
        let mut state = AppState::default();

        state.apply(&AppEvent::BootStarted);

        assert_eq!(state.screen, Screen::Boot);

        state.apply(&AppEvent::BootCompleted);

        assert_eq!(state.screen, Screen::Chat);
    }

    #[test]
    fn chat_screen_renders_the_initial_prompt() {
        let mut terminal =
            Terminal::new(TestBackend::new(80, 10)).expect("terminal should initialize");
        let state = AppState {
            screen: Screen::Chat,
            ..AppState::default()
        };

        terminal
            .draw(|frame| render(frame, &state))
            .expect("screen should render");

        let buffer = terminal.backend().buffer();
        let content = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();

        assert!(content.contains("What can I help you build?"));
    }

    #[test]
    fn credential_onboarding_masks_the_api_key() {
        let mut state = AppState::default();
        state.apply(&AppEvent::CredentialRequired);
        state.push_credential_character('a');
        state.push_credential_character('b');
        let mut terminal =
            Terminal::new(TestBackend::new(80, 10)).expect("terminal should initialize");

        terminal
            .draw(|frame| render(frame, &state))
            .expect("screen should render");

        let content = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
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
    fn onboarding_key_events_capture_and_submit_a_masked_credential() {
        let mut state = AppState::default();
        state.apply(&AppEvent::CredentialRequired);
        let input = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('k'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));
        let submit = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));

        assert_eq!(super::handle_onboarding_event(&mut state, &input), None);
        assert_eq!(
            super::handle_onboarding_event(&mut state, &submit),
            Some("k".into())
        );
    }

    #[test]
    fn onboarding_accepts_shift_modified_printable_characters() {
        let mut state = AppState::default();
        state.apply(&AppEvent::CredentialRequired);
        let input = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('A'),
            KeyModifiers::SHIFT,
            KeyEventKind::Press,
        ));

        assert_eq!(super::handle_onboarding_event(&mut state, &input), None);
        assert_eq!(state.take_credential_submission(), Some("A".into()));
    }

    #[test]
    fn model_picker_confirms_the_highlighted_model() {
        let mut state = AppState::default();
        state.apply(&AppEvent::ModelsAvailable(vec!["nvidia/model".into()]));
        let submit = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));

        assert_eq!(
            super::handle_model_picker_event(&mut state, &submit),
            Some("nvidia/model".into())
        );
    }

    #[test]
    fn terminal_loop_renders_then_exits_on_ctrl_c() {
        let mut terminal =
            Terminal::new(TestBackend::new(40, 4)).expect("terminal should initialize");
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

        let content = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(content.contains("What can I help you build?"));
    }

    #[test]
    fn resize_and_key_release_do_not_exit_the_tui() {
        assert_eq!(
            classify_event(&Event::Resize(120, 40)),
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
    fn q_and_ctrl_c_are_distinct_exit_actions() {
        assert_eq!(
            classify_event(&Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                KeyEventKind::Press,
            ))),
            InputAction::Exit
        );
        assert_eq!(
            classify_event(&Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ))),
            InputAction::Interrupt
        );
    }
}
