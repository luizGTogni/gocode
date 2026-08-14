//! Terminal user interface for Gocode.

use crossterm::event;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use gocode_core::{AppCommand, AppEvent};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    widgets::{Block, Borders, Paragraph},
};
use std::time::Duration;
use tokio::sync::mpsc;

/// The active top-level interface screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Screen {
    /// Startup state before the application is ready for input.
    #[default]
    Boot,
    /// Main conversational interface.
    Chat,
}

/// Renderable interface state derived from application events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppState {
    /// Visible top-level screen.
    pub screen: Screen,
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
    pub fn apply(&mut self, event: AppEvent) {
        match event {
            AppEvent::BootStarted => self.screen = Screen::Boot,
            AppEvent::BootCompleted => self.screen = Screen::Chat,
            AppEvent::TerminalResized { .. } => {}
        }
    }
}

/// Renders the active application screen.
pub fn render(frame: &mut Frame, state: &AppState) {
    let content = match state.screen {
        Screen::Boot => "Starting Gocode...",
        Screen::Chat => "What can I help you build?",
    };
    let title = match state.screen {
        Screen::Boot => "Gocode",
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
    initial_event: AppEvent,
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
            state.apply(app_event);
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

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use gocode_core::AppEvent;
    use ratatui::{Terminal, backend::TestBackend};

    use super::{AppState, InputAction, Screen, classify_event, render, run_with_event_source};

    #[test]
    fn boot_lifecycle_events_render_boot_then_chat() {
        let mut state = AppState::default();

        state.apply(AppEvent::BootStarted);

        assert_eq!(state.screen, Screen::Boot);

        state.apply(AppEvent::BootCompleted);

        assert_eq!(state.screen, Screen::Chat);
    }

    #[test]
    fn chat_screen_renders_the_initial_prompt() {
        let mut terminal =
            Terminal::new(TestBackend::new(40, 4)).expect("terminal should initialize");
        let state = AppState {
            screen: Screen::Chat,
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
    fn terminal_loop_renders_then_exits_on_ctrl_c() {
        let mut terminal =
            Terminal::new(TestBackend::new(40, 4)).expect("terminal should initialize");
        let exit = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        ));

        assert_eq!(
            run_with_event_source(&mut terminal, AppEvent::BootCompleted, || Ok(exit.clone()))
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
