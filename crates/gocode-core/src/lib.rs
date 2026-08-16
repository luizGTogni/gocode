mod markdown_doc;
mod session;
pub use markdown_doc::{
    CustomCommand, SkillSource, SkillSummary, apply_disabled_skills, load_custom_commands,
    load_disabled_skills, load_skills, parse_frontmatter, save_disabled_skills, set_skill_enabled,
};
pub use session::{
    SessionRecord, SessionSummary, list_sessions, load_session, save_session, sessions_dir,
};

/// Conservative trigger for automatic compaction: the provider's reported input-token count for
/// the run's last turn. NVIDIA NIM's model listing does not expose each model's real context
/// window, so this errs toward compacting earlier rather than risking an oversized request
/// against a smaller-context model.
pub const AUTO_COMPACT_TOKEN_THRESHOLD: u64 = 24_000;

/// Intent emitted by an interface and handled by the application runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    /// Request an orderly application shutdown.
    Exit,
    /// Notify the runtime that the terminal viewport changed dimensions.
    Resize { columns: u16, rows: u16 },
    /// Submit an API key held only in memory for immediate provider validation.
    SubmitCredential(String),
    /// Choose one discovered provider model for the current user profile.
    SelectModel(String),
    /// Send a user message using the currently selected model.
    SubmitChat(String),
    /// Cancel the current provider request while retaining the application session.
    CancelProviderRequest,
    /// Answer the single active permission prompt: `true` approves, `false` denies.
    PermissionResponse(bool),
    /// Start the user-approved update installation.
    AcceptUpdate,
    /// Dismiss this startup's update prompt without changing the installation.
    RejectUpdate,
    /// Set the reasoning-effort level sent with future chat requests, or clear it.
    SetReasoningEffort(Option<String>),
    /// Set the permission mode applied to future agent runs.
    SetPermissionMode(PermissionMode),
    /// Summarize and replace the remembered conversation history right now.
    CompactContext,
    /// Enable or disable automatic compaction when the conversation grows large.
    SetAutoCompact(bool),
    /// Forget the current session's remembered conversation history and start it over.
    ClearConversation,
    /// Start a brand-new, empty session without discarding the current one.
    NewSession,
    /// Read the list of previously saved sessions from disk.
    RequestSessionList,
    /// Switch to a previously saved session, replacing the current one.
    ResumeSession(String),
    /// Enable or disable a discovered skill by name for this project.
    SetSkillEnabled { name: String, enabled: bool },
}

/// How permissively an agent run is allowed to act without asking the user first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    /// The MVP default: reads and low-risk commands proceed, writes and other commands follow
    /// the existing risk-based policy.
    #[default]
    Auto,
    /// Read-only: gathers information but cannot write files or run risky commands.
    Plan,
    /// Every write and command, however low-risk, asks for explicit confirmation first.
    Approve,
}

impl PermissionMode {
    /// Advances to the next mode in the Auto → Plan → Approve → Auto cycle.
    #[must_use]
    pub const fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Plan,
            Self::Plan => Self::Approve,
            Self::Approve => Self::Auto,
        }
    }

    /// Short, lowercase, user-facing name for this mode.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Plan => "plan",
            Self::Approve => "approve",
        }
    }
}

/// Fact emitted by the application runtime and rendered by an interface.
#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    /// Application bootstrap has begun.
    BootStarted,
    /// Application bootstrap completed and the primary interface is ready.
    BootCompleted,
    /// The resolved project root the runtime is operating in, sent once near boot.
    ProjectContextAvailable {
        /// Display form of the detected project root.
        working_directory: String,
    },
    /// Project-local custom slash commands discovered under `.gocode/commands/`.
    CustomCommandsAvailable(Vec<CustomCommand>),
    /// Global and project skills discovered under `.agents/skills/` (or the project's
    /// `.gocode/skills/` fallback).
    SkillsAvailable(Vec<SkillSummary>),
    /// Confirm the terminal viewport dimensions known by the runtime.
    TerminalResized { columns: u16, rows: u16 },
    /// NVIDIA requires a credential before provider work can start.
    CredentialRequired,
    /// Credential validation is running outside the rendering path.
    CredentialValidationStarted,
    /// Credential validation failed with a safe, user-actionable message.
    CredentialValidationFailed(String),
    /// Authenticated model discovery completed successfully.
    ModelsAvailable(Vec<String>),
    /// A model has been selected and chat can accept a prompt.
    ModelSelected(String),
    /// Incremental assistant text normalized from the provider stream.
    AssistantTextDelta(String),
    /// The provider request ended with a safe, user-actionable error; recoverable inline.
    ProviderFailed(String),
    /// An error severe enough to require a blocking acknowledgement before work continues.
    BlockingError(String),
    /// A newer verified release is available for user approval.
    UpdateAvailable { version: String, notes: String },
    /// A non-destructive preparation status for an accepted update.
    UpdateProgress(String),
    /// Update preparation failed; the running installation remains available.
    UpdateFailed(String),
    /// The external updater is launched; the interface must restore the terminal and exit.
    ExitForUpdate,
    /// The agent run's lifecycle state changed.
    AgentStateChanged(AgentActivityState),
    /// The model requested a tool call.
    ToolActivity {
        /// Correlation id shared by the matching start/finish pair.
        id: String,
        /// Model-facing tool name.
        name: String,
        /// Current lifecycle status of the call.
        status: ToolActivityStatus,
        /// Short human-readable summary of the action or outcome.
        detail: String,
    },
    /// An incremental chunk of tool output (typically `run_command`).
    ToolOutputChunk {
        /// Correlation id of the tool call this chunk belongs to.
        id: String,
        /// Raw chunk content.
        chunk: String,
    },
    /// A tool call affected a workspace file.
    FileChanged {
        /// Workspace-relative path.
        path: String,
        /// `created`, `modified`, or `deleted`.
        kind: String,
    },
    /// A non-fatal condition worth surfacing to the user.
    AgentWarning(String),
    /// The agent asks the user to approve or deny one action before it proceeds.
    PermissionRequested {
        /// Short summary of the requested action.
        summary: String,
        /// Working directory the action would run or write in.
        working_directory: String,
    },
    /// The agent run finished normally.
    AgentCompleted {
        /// The model's final visible response, present only when no delta was streamed.
        final_text: Option<String>,
        /// Number of inference turns consumed.
        turns: usize,
        /// Number of tool calls attempted.
        tool_calls: usize,
        /// Number of tool calls that did not succeed.
        failed_tool_calls: usize,
        /// Input tokens reported for the run's last turn, when the provider reports them. An
        /// approximate signal for context usage, since NVIDIA NIM does not expose real
        /// per-model context-window sizes.
        last_input_tokens: Option<u64>,
    },
    /// The agent run was cancelled by the user.
    AgentCancelled,
    /// The active reasoning-effort level changed.
    ReasoningEffortChanged {
        /// The newly active level, or `None` to clear it.
        effort: Option<String>,
        /// Whether to surface a confirmation in the transcript. `false` for the silent restore
        /// performed at startup from a previously saved value.
        announce: bool,
    },
    /// The remembered conversation history was summarized and replaced.
    ContextCompacted {
        /// Whether this happened automatically (context grew large) or via `/compact`.
        automatic: bool,
    },
    /// Compaction could not be completed; the previous history is unchanged.
    ContextCompactionFailed(String),
    /// The active session changed: a fresh one was started, or a saved one was resumed.
    SessionSwitched {
        /// The new current session's id.
        id: String,
        /// The new current session's display name.
        name: String,
        /// `true` for a brand-new empty session, `false` when resuming a saved one.
        is_new: bool,
        /// The resumed session's messages, empty for a new session. The interface replays these
        /// into the transcript so a resumed conversation looks the way you left it.
        history: Vec<ChatMessage>,
    },
    /// The saved-session list finished loading, newest-used first.
    SessionListAvailable(Vec<SessionSummary>),
    /// A session could not be resumed; the current session is unchanged.
    SessionResumeFailed(String),
}

/// Coarse lifecycle phase of an active agent run, for a concise status indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentActivityState {
    /// Waiting on the model for the next turn.
    Thinking,
    /// Running one or more tool calls.
    RunningTools,
}

/// Lifecycle status of one tool call, as shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolActivityStatus {
    /// Execution began.
    Started,
    /// Execution completed successfully.
    Succeeded,
    /// Execution completed unsuccessfully.
    Failed,
    /// The user or permission policy denied the action.
    Denied,
    /// Execution was cancelled.
    Cancelled,
}

/// Severity of a normalized error, used to decide inline vs. blocking presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSeverity {
    /// Safe to show inline; the user can keep working.
    Recoverable,
    /// Serious enough to require acknowledgement before continuing.
    Blocking,
}

impl ProviderError {
    /// Classifies how urgently this error needs the user's attention.
    #[must_use]
    pub const fn severity(&self) -> ErrorSeverity {
        match self {
            Self::MissingCredential | Self::InvalidCredential => ErrorSeverity::Blocking,
            Self::Network(_)
            | Self::Timeout
            | Self::RateLimited
            | Self::ModelNotFound(_)
            | Self::UnsupportedCapability(_)
            | Self::InvalidRequest(_)
            | Self::InvalidResponse(_)
            | Self::Server { .. }
            | Self::Cancelled => ErrorSeverity::Recoverable,
        }
    }
}

/// TUI-facing halves of the bounded runtime channels.
pub struct RuntimeClient {
    /// Commands sent from the interface to the runtime.
    pub command_tx: tokio::sync::mpsc::Sender<AppCommand>,
    /// Events received by the interface from the runtime.
    pub event_rx: tokio::sync::mpsc::Receiver<AppEvent>,
}

/// Runtime-facing halves of the bounded runtime channels.
pub struct RuntimeDriver {
    /// Commands received by the runtime from the interface.
    pub command_rx: tokio::sync::mpsc::Receiver<AppCommand>,
    /// Events emitted by the runtime for the interface.
    pub event_tx: tokio::sync::mpsc::Sender<AppEvent>,
}

/// Factory for the initial application command/event bus.
pub struct RuntimeChannels;

impl RuntimeChannels {
    /// Creates bounded channels for one application runtime.
    #[must_use]
    pub fn create() -> (RuntimeClient, RuntimeDriver) {
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(32);
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(32);

        (
            RuntimeClient {
                command_tx,
                event_rx,
            },
            RuntimeDriver {
                command_rx,
                event_tx,
            },
        )
    }
}

/// Dependencies prepared during application bootstrap.
#[derive(Debug, Clone, PartialEq)]
pub struct BootstrapResult {
    /// First lifecycle event for the interface.
    pub event: AppEvent,
    /// Loaded or newly created global configuration.
    pub config: GlobalConfig,
    /// Initialized project context for the current working directory.
    pub project: ProjectContext,
    /// Configuration resolved for the current invocation.
    pub resolved_config: ResolvedConfig,
}

/// Prepares persistent platform services and reports the first lifecycle event.
///
/// # Errors
///
/// Returns [`AppError::Io`] when directories or configuration cannot be prepared, and
/// [`AppError::Configuration`] when an existing configuration is invalid.
pub fn bootstrap_with_paths(paths: &PlatformPaths) -> Result<BootstrapResult, AppError> {
    bootstrap_workspace(
        paths,
        &std::env::current_dir().map_err(|error| {
            AppError::Initialization(format!("could not determine current directory: {error}"))
        })?,
        &ConfigValues::default(),
    )
}

/// Prepares global and project state and resolves configuration for one workspace invocation.
///
/// # Errors
///
/// Returns an application error if any durable state cannot be prepared or parsed.
pub fn bootstrap_workspace(
    paths: &PlatformPaths,
    working_dir: &Path,
    cli_overrides: &ConfigValues,
) -> Result<BootstrapResult, AppError> {
    paths.ensure_directories()?;
    let config = load_or_create_global_config(&paths.config_dir.join("config.toml"))?;
    let project = initialize_project(&detect_project_root(working_dir))?;
    let project_config = load_project_config(&project.gocode_dir.join("project.toml"))?;
    let resolved_config = ResolvedConfig::from_layers(
        cli_overrides,
        &project_config.values,
        &config.values,
        &ConfigValues::default(),
        &ConfigValues::default(),
    );

    Ok(BootstrapResult {
        event: AppEvent::BootStarted,
        config,
        project,
        resolved_config,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        AppCommand, AppEvent, ConfigValues, EnvironmentPaths, GlobalConfig, Platform,
        PlatformPaths, ProjectConfig, ResolvedConfig, RuntimeChannels, XdgDirectories,
        atomic_write, bootstrap_workspace, detect_project_root, initialize_project,
        load_or_create_global_config,
    };

    #[test]
    fn commands_and_events_express_the_foundation_lifecycle() {
        let command = AppCommand::Exit;
        let event = AppEvent::BootStarted;

        assert_eq!(command, AppCommand::Exit);
        assert_eq!(event, AppEvent::BootStarted);
    }

    #[test]
    fn credential_and_authentication_failures_are_blocking() {
        assert_eq!(
            super::ProviderError::MissingCredential.severity(),
            super::ErrorSeverity::Blocking
        );
        assert_eq!(
            super::ProviderError::InvalidCredential.severity(),
            super::ErrorSeverity::Blocking
        );
    }

    #[test]
    fn transient_provider_failures_are_recoverable_inline() {
        assert_eq!(
            super::ProviderError::Timeout.severity(),
            super::ErrorSeverity::Recoverable
        );
        assert_eq!(
            super::ProviderError::RateLimited.severity(),
            super::ErrorSeverity::Recoverable
        );
    }

    #[test]
    fn unknown_model_capabilities_are_conservative() {
        let capabilities = super::ModelCapabilities::unknown();

        assert!(capabilities.streaming);
        assert_eq!(capabilities.tools, super::ToolCapability::Unsupported);
        assert_eq!(
            capabilities.thinking,
            super::ThinkingCapability::Unsupported
        );
    }

    #[test]
    fn stream_events_keep_provider_wire_format_out_of_consumers() {
        let event = super::ChatStreamEvent::TextDelta("Olá".into());

        assert_eq!(event, super::ChatStreamEvent::TextDelta("Olá".into()));
    }

    #[test]
    fn chat_request_keeps_model_and_user_message_provider_neutral() {
        let request = super::ChatRequest::single_user("nvidia/model", "Explique este código");

        assert_eq!(request.model.as_str(), "nvidia/model");
        assert_eq!(
            request.messages,
            vec![super::ChatMessage::User("Explique este código".into())]
        );
    }

    #[test]
    fn cancellation_token_is_shared_by_provider_work() {
        let token = super::CancellationToken::new();
        let provider_token = token.clone();

        token.cancel();

        assert!(provider_token.is_cancelled());
    }

    #[tokio::test]
    async fn runtime_channel_transports_an_in_memory_credential_submission() {
        let (client, mut driver) = RuntimeChannels::create();

        client
            .command_tx
            .send(AppCommand::SubmitCredential("temporary-key".into()))
            .await
            .expect("credential submission should send");

        assert_eq!(
            driver.command_rx.recv().await,
            Some(AppCommand::SubmitCredential("temporary-key".into()))
        );
    }

    #[tokio::test]
    async fn runtime_channels_deliver_commands_and_events() {
        let (mut client, mut driver) = RuntimeChannels::create();

        client
            .command_tx
            .send(AppCommand::Exit)
            .await
            .expect("command should send");
        driver
            .event_tx
            .send(AppEvent::BootStarted)
            .await
            .expect("event should send");

        assert_eq!(driver.command_rx.recv().await, Some(AppCommand::Exit));
        assert_eq!(client.event_rx.recv().await, Some(AppEvent::BootStarted));
    }

    #[tokio::test]
    async fn runtime_channels_preserve_resize_details() {
        let (mut client, mut driver) = RuntimeChannels::create();

        client
            .command_tx
            .send(AppCommand::Resize {
                columns: 120,
                rows: 40,
            })
            .await
            .expect("resize command should send");
        driver
            .event_tx
            .send(AppEvent::TerminalResized {
                columns: 120,
                rows: 40,
            })
            .await
            .expect("resize event should send");

        assert_eq!(
            driver.command_rx.recv().await,
            Some(AppCommand::Resize {
                columns: 120,
                rows: 40,
            })
        );
        assert_eq!(
            client.event_rx.recv().await,
            Some(AppEvent::TerminalResized {
                columns: 120,
                rows: 40,
            })
        );
    }

    #[test]
    fn bootstrap_prepares_platform_directories_and_default_config() {
        let fixture = unique_fixture_dir("bootstrap");
        let paths = PlatformPaths::resolve(
            Platform::Linux,
            &fixture,
            XdgDirectories {
                config_home: Some(fixture.join("config")),
                state_home: Some(fixture.join("state")),
                cache_home: Some(fixture.join("cache")),
            },
        );

        let project_root = fixture.join("project");
        fs::create_dir_all(&project_root).expect("project directory should be created");
        let result = bootstrap_workspace(&paths, &project_root, &ConfigValues::default())
            .expect("bootstrap should succeed");

        assert_eq!(result.event, AppEvent::BootStarted);
        assert!(paths.config_dir.is_dir());
        assert!(paths.state_dir.is_dir());
        assert!(paths.cache_dir.is_dir());
        assert_eq!(result.config.schema_version, 1);
        assert!(paths.config_dir.join("config.toml").is_file());

        fs::remove_dir_all(fixture).expect("fixture should be removed");
    }

    #[test]
    fn bootstrap_prepares_the_detected_project_and_resolves_its_config() {
        let fixture = unique_fixture_dir("bootstrap-project");
        let project_root = fixture.join("project");
        let nested = project_root.join("src");
        fs::create_dir_all(project_root.join(".git")).expect("git directory should be created");
        fs::create_dir_all(&nested).expect("nested directory should be created");
        let paths = PlatformPaths::resolve(
            Platform::Linux,
            &fixture,
            XdgDirectories {
                config_home: Some(fixture.join("config")),
                state_home: Some(fixture.join("state")),
                cache_home: Some(fixture.join("cache")),
            },
        );
        paths
            .ensure_directories()
            .expect("global paths should exist");
        atomic_write(
            &paths.config_dir.join("config.toml"),
            "schema_version = 1\ndefault_provider = \"global\"\n",
        )
        .expect("global config should be written");
        let local = initialize_project(&project_root).expect("project should initialize");
        atomic_write(
            &local.gocode_dir.join("project.toml"),
            "schema_version = 1\n[model]\nprovider = \"project\"\n",
        )
        .expect("project config should be written");

        let result = bootstrap_workspace(&paths, &nested, &ConfigValues::default())
            .expect("workspace should bootstrap");

        assert_eq!(result.project.root, project_root);
        assert_eq!(result.resolved_config.provider.as_deref(), Some("project"));

        fs::remove_dir_all(fixture).expect("fixture should be removed");
    }

    #[test]
    fn environment_paths_resolve_linux_home_and_xdg_values() {
        let paths = PlatformPaths::from_environment(
            Platform::Linux,
            EnvironmentPaths {
                home: Some("/home/alice".into()),
                user_profile: None,
                xdg: XdgDirectories {
                    config_home: Some("/config".into()),
                    state_home: None,
                    cache_home: None,
                },
            },
        )
        .expect("Linux paths should resolve");

        assert_eq!(paths.config_dir, Path::new("/config/gocode"));
        assert_eq!(
            paths.state_dir,
            Path::new("/home/alice/.local/state/gocode")
        );
    }

    #[test]
    fn linux_paths_prefer_explicit_xdg_locations() {
        let paths = PlatformPaths::resolve(
            Platform::Linux,
            Path::new("/home/alice"),
            XdgDirectories {
                config_home: Some("/work/config".into()),
                state_home: Some("/work/state".into()),
                cache_home: Some("/work/cache".into()),
            },
        );

        assert_eq!(paths.config_dir, Path::new("/work/config/gocode"));
        assert_eq!(paths.state_dir, Path::new("/work/state/gocode"));
        assert_eq!(paths.cache_dir, Path::new("/work/cache/gocode"));
    }

    #[test]
    fn linux_paths_fall_back_to_standard_home_locations() {
        let paths = PlatformPaths::resolve(
            Platform::Linux,
            Path::new("/home/alice"),
            XdgDirectories::default(),
        );

        assert_eq!(paths.config_dir, Path::new("/home/alice/.config/gocode"));
        assert_eq!(
            paths.state_dir,
            Path::new("/home/alice/.local/state/gocode")
        );
        assert_eq!(paths.cache_dir, Path::new("/home/alice/.cache/gocode"));
    }

    #[test]
    fn windows_paths_share_the_gocode_home_directory() {
        let home = Path::new(r"C:\\Users\\Alice");
        let paths = PlatformPaths::resolve(Platform::Windows, home, XdgDirectories::default());

        assert_eq!(paths.config_dir, home.join(".gocode"));
        assert_eq!(paths.state_dir, home.join(".gocode"));
        assert_eq!(paths.cache_dir, home.join(".gocode"));
    }

    #[test]
    fn project_root_uses_the_nearest_git_ancestor() {
        let fixture = unique_fixture_dir("project-root");
        let project = fixture.join("project");
        let nested = project.join("src/nested");
        fs::create_dir_all(project.join(".git")).expect("git directory should be created");
        fs::create_dir_all(&nested).expect("nested directory should be created");

        assert_eq!(detect_project_root(&nested), project);

        fs::remove_dir_all(fixture).expect("fixture should be removed");
    }

    #[test]
    fn git_ancestor_has_priority_over_a_nearer_manifest() {
        let fixture = unique_fixture_dir("project-manifest-root");
        let project = fixture.join("project");
        let nested = project.join("src/nested");
        fs::create_dir_all(fixture.join(".git")).expect("git directory should be created");
        fs::create_dir_all(&nested).expect("nested directory should be created");
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\n",
        )
        .expect("manifest should be created");

        assert_eq!(detect_project_root(&nested), fixture);

        fs::remove_dir_all(fixture).expect("fixture should be removed");
    }

    #[test]
    fn project_initialization_creates_local_configuration_instructions_and_sessions() {
        let fixture = unique_fixture_dir("project-initialization");
        let project_root = fixture.join("project");
        fs::create_dir_all(&project_root).expect("project directory should be created");

        let project = initialize_project(&project_root).expect("project should initialize");

        assert_eq!(project.root, project_root);
        assert!(project.gocode_dir.join("project.toml").is_file());
        assert!(project.gocode_dir.join("instructions.md").is_file());
        assert!(project.gocode_dir.join("sessions").is_dir());

        fs::remove_dir_all(fixture).expect("fixture should be removed");
    }

    #[test]
    fn project_config_overrides_global_values_when_resolved() {
        let project = ProjectConfig::parse(
            r#"
                schema_version = 1

                [model]
                provider = "project-provider"
                model = "project-model"
            "#,
        )
        .expect("project config should parse");
        let resolved = ResolvedConfig::from_layers(
            &ConfigValues::default(),
            &project.values,
            &ConfigValues {
                provider: Some("global-provider".into()),
                model: Some("global-model".into()),
                ..ConfigValues::default()
            },
            &ConfigValues::default(),
            &ConfigValues::default(),
        );

        assert_eq!(resolved.provider.as_deref(), Some("project-provider"));
        assert_eq!(resolved.model.as_deref(), Some("project-model"));
    }

    #[test]
    fn platform_paths_create_all_required_directories() {
        let fixture = unique_fixture_dir("platform-paths");
        let paths = PlatformPaths::resolve(
            Platform::Linux,
            &fixture,
            XdgDirectories {
                config_home: Some(fixture.join("config")),
                state_home: Some(fixture.join("state")),
                cache_home: Some(fixture.join("cache")),
            },
        );

        paths
            .ensure_directories()
            .expect("directories should be created");

        assert!(paths.config_dir.is_dir());
        assert!(paths.state_dir.is_dir());
        assert!(paths.cache_dir.is_dir());

        fs::remove_dir_all(fixture).expect("fixture should be removed");
    }

    #[cfg(unix)]
    #[test]
    fn platform_state_directories_are_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = unique_fixture_dir("private-state");
        let paths = PlatformPaths::resolve(
            Platform::Linux,
            &fixture,
            XdgDirectories {
                config_home: Some(fixture.join("config")),
                state_home: Some(fixture.join("state")),
                cache_home: Some(fixture.join("cache")),
            },
        );

        paths
            .ensure_directories()
            .expect("global paths should be created");

        assert_eq!(
            fs::metadata(&paths.state_dir)
                .expect("state directory should be readable")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        fs::remove_dir_all(fixture).expect("fixture should be removed");
    }

    #[test]
    fn configuration_resolution_prefers_the_highest_defined_layer() {
        let resolved = ResolvedConfig::from_layers(
            &ConfigValues {
                provider: Some("cli-provider".into()),
                model: None,
                ..ConfigValues::default()
            },
            &ConfigValues {
                provider: Some("project-provider".into()),
                model: Some("project-model".into()),
                ..ConfigValues::default()
            },
            &ConfigValues {
                provider: Some("global-provider".into()),
                model: Some("global-model".into()),
                ..ConfigValues::default()
            },
            &ConfigValues {
                provider: Some("provider-default".into()),
                model: Some("provider-model".into()),
                ..ConfigValues::default()
            },
            &ConfigValues {
                provider: Some("built-in-provider".into()),
                model: Some("built-in-model".into()),
                ..ConfigValues::default()
            },
        );

        assert_eq!(resolved.provider.as_deref(), Some("cli-provider"));
        assert_eq!(resolved.model.as_deref(), Some("project-model"));
    }

    #[test]
    fn atomic_write_replaces_existing_configuration_contents() {
        let fixture = unique_fixture_dir("atomic-write");
        fs::create_dir_all(&fixture).expect("fixture directory should be created");
        let config_path = fixture.join("config.toml");
        fs::write(&config_path, "schema_version = 0\n").expect("initial config should be written");

        atomic_write(&config_path, "schema_version = 1\n")
            .expect("configuration should be replaced");

        assert_eq!(
            fs::read_to_string(&config_path).expect("config should be readable"),
            "schema_version = 1\n"
        );

        fs::remove_dir_all(fixture).expect("fixture should be removed");
    }

    #[test]
    fn global_config_parses_schema_v1_and_optional_defaults() {
        let config = GlobalConfig::parse(
            r#"
                schema_version = 1
                default_provider = "nvidia"
                default_model = "nvidia/llama-3.3-nemotron-super-49b-v1"
            "#,
        )
        .expect("schema v1 configuration should parse");

        assert_eq!(config.schema_version, 1);
        assert_eq!(config.values.provider.as_deref(), Some("nvidia"));
        assert_eq!(
            config.values.model.as_deref(),
            Some("nvidia/llama-3.3-nemotron-super-49b-v1")
        );
    }

    #[test]
    fn loading_a_missing_config_creates_and_returns_schema_v1_defaults() {
        let fixture = unique_fixture_dir("config-defaults");
        fs::create_dir_all(&fixture).expect("fixture directory should be created");
        let config_path = fixture.join("config.toml");

        let config = load_or_create_global_config(&config_path)
            .expect("missing configuration should receive defaults");

        assert_eq!(config.schema_version, 1);
        assert_eq!(config.values, ConfigValues::default());
        assert_eq!(
            fs::read_to_string(&config_path).expect("default config should be persisted"),
            "schema_version = 1\n"
        );

        fs::remove_dir_all(fixture).expect("fixture should be removed");
    }

    #[test]
    fn invalid_global_config_is_preserved_for_recovery() {
        let fixture = unique_fixture_dir("invalid-global-config");
        fs::create_dir_all(&fixture).expect("fixture directory should be created");
        let config_path = fixture.join("config.toml");
        let invalid_contents = "schema_version = 2\n";
        fs::write(&config_path, invalid_contents).expect("invalid config should be written");

        let error = load_or_create_global_config(&config_path)
            .expect_err("unsupported configuration should not be replaced");

        assert!(matches!(error, super::AppError::Configuration(_)));
        assert_eq!(
            fs::read_to_string(&config_path).expect("invalid config should remain readable"),
            invalid_contents
        );

        fs::remove_dir_all(fixture).expect("fixture should be removed");
    }

    fn unique_fixture_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after the Unix epoch")
            .as_nanos();

        std::env::temp_dir().join(format!("gocode-{name}-{}-{nanos}", std::process::id()))
    }
}
use std::{
    fmt,
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    pin::Pin,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

/// Operating systems with distinct Gocode directory conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Windows stores all global Gocode data beneath the user's home directory.
    Windows,
    /// Linux separates global data according to the XDG Base Directory Specification.
    Linux,
}

/// Optional XDG base directories supplied by the process environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XdgDirectories {
    /// Value of `XDG_CONFIG_HOME`, if supplied.
    pub config_home: Option<PathBuf>,
    /// Value of `XDG_STATE_HOME`, if supplied.
    pub state_home: Option<PathBuf>,
    /// Value of `XDG_CACHE_HOME`, if supplied.
    pub cache_home: Option<PathBuf>,
}

/// Environment-derived values needed to resolve platform directories.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvironmentPaths {
    /// Value of `HOME`, if supplied.
    pub home: Option<PathBuf>,
    /// Value of `USERPROFILE`, if supplied.
    pub user_profile: Option<PathBuf>,
    /// Optional Linux XDG base directories.
    pub xdg: XdgDirectories,
}

/// Resolved global directories for configuration, state, and cache data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformPaths {
    /// Directory containing user configuration.
    pub config_dir: PathBuf,
    /// Directory containing mutable state and logs.
    pub state_dir: PathBuf,
    /// Directory containing disposable cache data.
    pub cache_dir: PathBuf,
}

impl PlatformPaths {
    /// Resolves platform paths from explicit environment values.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Initialization`] when the platform's required home variable is absent.
    pub fn from_environment(
        platform: Platform,
        environment: EnvironmentPaths,
    ) -> Result<Self, AppError> {
        let home_dir = match platform {
            Platform::Windows => environment.user_profile,
            Platform::Linux => environment.home,
        }
        .ok_or_else(|| {
            AppError::Initialization(match platform {
                Platform::Windows => "USERPROFILE is not set".into(),
                Platform::Linux => "HOME is not set".into(),
            })
        })?;

        Ok(Self::resolve(platform, &home_dir, environment.xdg))
    }

    /// Resolves Gocode directories from one platform contract and explicit inputs.
    #[must_use]
    pub fn resolve(platform: Platform, home_dir: &Path, xdg: XdgDirectories) -> Self {
        match platform {
            Platform::Windows => {
                let gocode_dir = home_dir.join(".gocode");

                Self {
                    config_dir: gocode_dir.clone(),
                    state_dir: gocode_dir.clone(),
                    cache_dir: gocode_dir,
                }
            }
            Platform::Linux => Self {
                config_dir: xdg
                    .config_home
                    .unwrap_or_else(|| home_dir.join(".config"))
                    .join("gocode"),
                state_dir: xdg
                    .state_home
                    .unwrap_or_else(|| home_dir.join(".local/state"))
                    .join("gocode"),
                cache_dir: xdg
                    .cache_home
                    .unwrap_or_else(|| home_dir.join(".cache"))
                    .join("gocode"),
            },
        }
    }

    /// Creates the configuration, state, and cache directories if they are absent.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Io`] when a required directory cannot be created.
    pub fn ensure_directories(&self) -> Result<(), AppError> {
        for directory in [&self.config_dir, &self.state_dir, &self.cache_dir] {
            std::fs::create_dir_all(directory).map_err(|error| {
                AppError::Io(format!("could not create {}: {error}", directory.display()))
            })?;
            restrict_directory_permissions(directory)?;
        }

        Ok(())
    }
}

#[cfg(unix)]
fn restrict_directory_permissions(directory: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        AppError::Io(format!(
            "could not restrict {}: {error}",
            directory.display()
        ))
    })
}

#[cfg(not(unix))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the platform-neutral caller propagates filesystem permission errors on Unix"
)]
fn restrict_directory_permissions(_directory: &Path) -> Result<(), AppError> {
    Ok(())
}

/// Finds the project root by Git first, then by the nearest supported manifest.
///
/// If neither marker is found, the supplied starting directory is the project root.
#[must_use]
pub fn detect_project_root(start_dir: &Path) -> PathBuf {
    start_dir
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .or_else(|| {
            start_dir
                .ancestors()
                .find(|candidate| has_supported_manifest(candidate))
        })
        .unwrap_or(start_dir)
        .to_path_buf()
}

fn has_supported_manifest(directory: &Path) -> bool {
    ["Cargo.toml", "package.json", "pyproject.toml", "go.mod"]
        .iter()
        .any(|manifest| directory.join(manifest).is_file())
}

/// Local project paths and metadata prepared for one Gocode workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectContext {
    /// Detected project root.
    pub root: PathBuf,
    /// Project-local Gocode directory.
    pub gocode_dir: PathBuf,
}

/// Creates required project-local data without overwriting existing user files.
///
/// # Errors
///
/// Returns [`AppError::Io`] when required files or directories cannot be created.
pub fn initialize_project(project_root: &Path) -> Result<ProjectContext, AppError> {
    let gocode_dir = project_root.join(".gocode");
    for subdirectory in ["sessions", "commands", "skills"] {
        std::fs::create_dir_all(gocode_dir.join(subdirectory)).map_err(|error| {
            AppError::Io(format!(
                "could not create {}: {error}",
                gocode_dir.display()
            ))
        })?;
    }

    create_file_if_missing(&gocode_dir.join("project.toml"), "schema_version = 1\n")?;
    create_file_if_missing(
        &gocode_dir.join("instructions.md"),
        "# Project instructions\n\n",
    )?;

    Ok(ProjectContext {
        root: project_root.to_path_buf(),
        gocode_dir,
    })
}

fn create_file_if_missing(path: &Path, contents: &str) -> Result<(), AppError> {
    if path.exists() {
        return Ok(());
    }

    atomic_write(path, contents)
}

/// Optional configuration values from one precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigValues {
    /// Selected provider identifier.
    pub provider: Option<String>,
    /// Selected model identifier.
    pub model: Option<String>,
    /// Selected reasoning-effort level.
    pub reasoning_effort: Option<String>,
}

/// Global configuration persisted in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfig {
    /// Version of the persisted configuration schema.
    pub schema_version: u32,
    /// Values contributed by the global configuration layer.
    pub values: ConfigValues,
}

/// Project configuration persisted in `.gocode/project.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Version of the persisted configuration schema.
    pub schema_version: u32,
    /// Values contributed by the project configuration layer.
    pub values: ConfigValues,
}

impl ProjectConfig {
    /// Parses and validates a project configuration document.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Configuration`] when TOML is invalid or the schema is unsupported.
    pub fn parse(contents: &str) -> Result<Self, AppError> {
        let raw: RawProjectConfig = toml::from_str(contents).map_err(|error| {
            AppError::Configuration(format!("could not parse project.toml: {error}"))
        })?;

        if raw.schema_version != 1 {
            return Err(AppError::Configuration(format!(
                "project.toml schema_version {} is unsupported; expected 1",
                raw.schema_version
            )));
        }

        let model = raw.model.unwrap_or_default();
        Ok(Self {
            schema_version: raw.schema_version,
            values: ConfigValues {
                provider: model.provider,
                model: model.model,
                reasoning_effort: None,
            },
        })
    }
}

/// Loads project configuration that has already been created during project initialization.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the file cannot be read, or [`AppError::Configuration`] when it
/// contains invalid or unsupported TOML.
pub fn load_project_config(path: &Path) -> Result<ProjectConfig, AppError> {
    std::fs::read_to_string(path)
        .map_err(|error| AppError::Io(format!("could not read {}: {error}", path.display())))
        .and_then(|contents| ProjectConfig::parse(&contents))
}

impl GlobalConfig {
    /// Parses and validates a global configuration document.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Configuration`] when TOML is invalid or the schema is unsupported.
    pub fn parse(contents: &str) -> Result<Self, AppError> {
        let raw: RawGlobalConfig = toml::from_str(contents).map_err(|error| {
            AppError::Configuration(format!("could not parse config.toml: {error}"))
        })?;

        if raw.schema_version != 1 {
            return Err(AppError::Configuration(format!(
                "config.toml schema_version {} is unsupported; expected 1",
                raw.schema_version
            )));
        }

        Ok(Self {
            schema_version: raw.schema_version,
            values: ConfigValues {
                provider: raw.default_provider,
                model: raw.default_model,
                reasoning_effort: raw.default_reasoning_effort,
            },
        })
    }
}

/// Loads a persisted global configuration or creates schema-v1 defaults when it is absent.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the configuration cannot be read or written, and
/// [`AppError::Configuration`] when an existing document is invalid or unsupported.
pub fn load_or_create_global_config(path: &Path) -> Result<GlobalConfig, AppError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => GlobalConfig::parse(&contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let config = GlobalConfig {
                schema_version: 1,
                values: ConfigValues::default(),
            };
            atomic_write(path, "schema_version = 1\n")?;
            Ok(config)
        }
        Err(error) => Err(AppError::Io(format!(
            "could not read {}: {error}",
            path.display()
        ))),
    }
}

/// Persists the selected provider, model, and reasoning-effort level, without ever writing a
/// credential. Fields left `None` are simply omitted, not cleared from a future read — callers
/// should pass every field they currently know so an update to one does not appear to erase the
/// others.
///
/// # Errors
///
/// Returns an error when the configuration cannot be replaced atomically.
pub fn save_global_config(
    path: &Path,
    provider: Option<&str>,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Result<(), AppError> {
    use std::fmt::Write as _;

    let mut contents = String::from("schema_version = 1\n");
    if let Some(provider) = provider {
        let _ = writeln!(contents, "default_provider = \"{provider}\"");
    }
    if let Some(model) = model {
        let _ = writeln!(contents, "default_model = \"{model}\"");
    }
    if let Some(reasoning_effort) = reasoning_effort {
        let _ = writeln!(
            contents,
            "default_reasoning_effort = \"{reasoning_effort}\""
        );
    }
    atomic_write(path, &contents)
}

#[derive(Deserialize)]
struct RawGlobalConfig {
    schema_version: u32,
    default_provider: Option<String>,
    default_model: Option<String>,
    default_reasoning_effort: Option<String>,
}

#[derive(Deserialize)]
struct RawProjectConfig {
    schema_version: u32,
    model: Option<RawProjectModel>,
}

#[derive(Default, Deserialize)]
struct RawProjectModel {
    provider: Option<String>,
    model: Option<String>,
}

/// Configuration after applying every precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedConfig {
    /// Effective provider identifier, if configured.
    pub provider: Option<String>,
    /// Effective model identifier, if configured.
    pub model: Option<String>,
    /// Effective reasoning-effort level, if configured.
    pub reasoning_effort: Option<String>,
}

impl ResolvedConfig {
    /// Resolves values in CLI, project, global, provider-default, built-in order.
    #[must_use]
    pub fn from_layers(
        cli: &ConfigValues,
        project: &ConfigValues,
        global: &ConfigValues,
        provider_defaults: &ConfigValues,
        built_in_defaults: &ConfigValues,
    ) -> Self {
        Self {
            provider: first_defined(&[
                &cli.provider,
                &project.provider,
                &global.provider,
                &provider_defaults.provider,
                &built_in_defaults.provider,
            ]),
            model: first_defined(&[
                &cli.model,
                &project.model,
                &global.model,
                &provider_defaults.model,
                &built_in_defaults.model,
            ]),
            reasoning_effort: first_defined(&[
                &cli.reasoning_effort,
                &project.reasoning_effort,
                &global.reasoning_effort,
                &provider_defaults.reasoning_effort,
                &built_in_defaults.reasoning_effort,
            ]),
        }
    }
}

fn first_defined(values: &[&Option<String>]) -> Option<String> {
    values.iter().find_map(|value| (*value).clone())
}

/// Atomically replaces a file with fully flushed content written beside it.
///
/// # Errors
///
/// Returns [`AppError::Io`] when the target has no parent directory, the temporary write fails, or
/// the replacement cannot be completed.
pub fn atomic_write(path: &Path, contents: &str) -> Result<(), AppError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| AppError::Io(format!("{} has no parent directory", path.display())))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| AppError::Io(format!("{} has no file name", path.display())))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AppError::Io(format!("system clock is invalid: {error}")))?
        .as_nanos();
    let temporary_path = parent.join(format!(
        ".{}.{}-{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        nonce
    ));

    let write_result = (|| -> std::io::Result<()> {
        // Never follow or overwrite a pre-created temporary path. If another process happens
        // to collide with this name, fail safely and leave the existing configuration intact.
        let mut temporary_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)?;
        temporary_file.write_all(contents.as_bytes())?;
        temporary_file.sync_all()
    })();

    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temporary_path);
        return Err(AppError::Io(format!(
            "could not write {}: {error}",
            path.display()
        )));
    }

    // `std::fs::rename` replaces an existing destination on the supported Windows and Linux
    // baselines, keeping this replacement safe under the workspace's `unsafe_code = forbid` lint.
    std::fs::rename(&temporary_path, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary_path);
        AppError::Io(format!("could not replace {}: {error}", path.display()))
    })
}

/// Errors that cross the application boundary with an actionable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// Application startup could not be completed.
    Initialization(String),
    /// A local filesystem operation failed.
    Io(String),
    /// A configuration document is malformed or unsupported.
    Configuration(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Initialization(message) => write!(formatter, "initialization failed: {message}"),
            Self::Io(message) => write!(formatter, "filesystem operation failed: {message}"),
            Self::Configuration(message) => write!(formatter, "configuration error: {message}"),
        }
    }
}

impl std::error::Error for AppError {}

/// Provider-neutral capabilities resolved for a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    /// Whether the provider can return incremental text for this model.
    pub streaming: bool,
    /// Whether function tools may be sent to the model.
    pub tools: ToolCapability,
    /// Whether reasoning controls are available for the model.
    pub thinking: ThinkingCapability,
}

impl ModelCapabilities {
    /// Uses only the behavior that can be safely assumed for an unrecognized model.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            streaming: true,
            tools: ToolCapability::Unsupported,
            thinking: ThinkingCapability::Unsupported,
        }
    }
}

/// A provider-neutral incremental event emitted while a chat request is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatStreamEvent {
    /// Additional visible assistant text.
    TextDelta(String),
    /// An incremental fragment of one tool call, correlated by `index` until assembled.
    ToolCallDelta(ToolCallDelta),
    /// A provider request identifier safe to retain for diagnostics.
    RequestId(String),
    /// Token accounting for the turn, when the provider reports it.
    Usage(Usage),
    /// The provider ended the response, with a normalized reason.
    Finished(FinishReason),
}

/// One incremental fragment of a streamed tool call.
///
/// Providers that stream tool calls (rather than delivering them whole) split the call's `id`,
/// function name, and JSON arguments across multiple deltas correlated by `index`. A
/// provider-specific assembler accumulates these into a complete call before the Agent executes
/// it; no partial call is ever executed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolCallDelta {
    /// Position of this call among the tool calls requested in the current turn.
    pub index: usize,
    /// The call's correlation identifier, present on the first fragment.
    pub id: Option<String>,
    /// Additional characters of the tool name, if the provider streams it incrementally.
    pub name_delta: Option<String>,
    /// Additional characters of the JSON-encoded arguments.
    pub arguments_delta: Option<String>,
}

/// Normalized token accounting for one provider turn. Fields remain optional because not every
/// provider reports every count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    /// Tokens consumed by the request (system, history, and tool definitions).
    pub input_tokens: Option<u64>,
    /// Tokens produced in the response.
    pub output_tokens: Option<u64>,
    /// Tokens attributed to internal reasoning, when the provider separates them.
    pub reasoning_tokens: Option<u64>,
}

/// Why the provider stopped generating for the current turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    /// The model produced a complete response with no pending tool calls.
    Stop,
    /// The model is requesting one or more tool calls before it can continue.
    ToolCalls,
    /// Generation stopped because an output or context limit was reached.
    Length,
    /// The request was cancelled before the provider finished.
    Cancelled,
    /// The provider's content filter stopped generation.
    ContentFilter,
    /// A provider-specific reason with no normalized equivalent.
    Other(String),
}

/// A provider-independent identifier for a catalog model.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelId(String);

impl ModelId {
    /// Creates a model identifier from the provider's canonical name.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the canonical provider model name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One provider catalog model with normalized, safely resolved capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// The provider's canonical identifier.
    pub id: ModelId,
    /// A UI-safe label, initially equal to the canonical identifier when no metadata exists.
    pub display_name: String,
    /// Capabilities resolved without guessing unsupported behavior.
    pub capabilities: ModelCapabilities,
}

/// A role-tagged normalized conversation message.
///
/// `Assistant` and `Tool` carry the fields needed to keep a multi-turn tool-calling
/// conversation coherent: an assistant turn may request tool calls alongside (or instead of)
/// visible text, and a tool turn must be correlated back to the call it answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatMessage {
    /// A system instruction.
    System(String),
    /// A user-authored request.
    User(String),
    /// An assistant response retained for a subsequent request.
    Assistant {
        /// Visible assistant text produced this turn, if any.
        text: Option<String>,
        /// Tool calls requested this turn, in requested order.
        tool_calls: Vec<ProviderToolCall>,
    },
    /// The result of one tool call, returned to the model.
    Tool {
        /// Correlation identifier copied from the originating [`ProviderToolCall`].
        tool_call_id: String,
        /// Model-facing result content.
        content: String,
    },
}

impl ChatMessage {
    /// Creates a text-only assistant message with no tool calls.
    #[must_use]
    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::Assistant {
            text: Some(text.into()),
            tool_calls: Vec::new(),
        }
    }
}

/// A normalized tool call requested by the model during a chat turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderToolCall {
    /// Correlation identifier assigned by the provider.
    pub id: String,
    /// Name of the requested tool.
    pub name: String,
    /// Parsed JSON arguments, not yet validated against the tool's schema.
    pub arguments: serde_json::Value,
}

/// Generic tool metadata sent to a capable model, independent of any concrete tool
/// implementation.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    /// Stable, model-facing tool name.
    pub name: String,
    /// Concise description that teaches the model when to use, and not use, the tool.
    pub description: String,
    /// JSON schema describing accepted arguments.
    pub input_schema: serde_json::Value,
}

/// A normalized request for streamed chat inference.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    /// The selected catalog model.
    pub model: ModelId,
    /// Conversation history in role order.
    pub messages: Vec<ChatMessage>,
    /// Tool definitions offered to the model, empty when tools are not applicable.
    pub tools: Vec<ToolDefinition>,
    /// User-selected reasoning-effort level, sent as-is to the provider when present.
    pub reasoning_effort: Option<String>,
}

/// Cooperative cancellation signal shared by one provider operation and its caller.
#[derive(Clone)]
pub struct CancellationToken {
    cancelled: tokio::sync::watch::Sender<bool>,
    receiver: tokio::sync::watch::Receiver<bool>,
}

impl CancellationToken {
    /// Creates a token that is initially active.
    #[must_use]
    pub fn new() -> Self {
        let (cancelled, receiver) = tokio::sync::watch::channel(false);
        Self {
            cancelled,
            receiver,
        }
    }

    /// Requests cancellation of all work holding a clone of this token.
    pub fn cancel(&self) {
        let _ = self.cancelled.send(true);
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Waits until cancellation is requested.
    pub async fn cancelled(&self) {
        let mut receiver = self.receiver.clone();
        if !*receiver.borrow() {
            let _ = receiver.changed().await;
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatRequest {
    /// Creates the smallest chat request: one selected model and one user message.
    #[must_use]
    pub fn single_user(model: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            model: ModelId::new(model),
            messages: vec![ChatMessage::User(message.into())],
            tools: Vec::new(),
            reasoning_effort: None,
        }
    }
}

/// The normalized event channel returned by [`Provider::stream_chat`].
pub type ChatStream = tokio::sync::mpsc::Receiver<Result<ChatStreamEvent, ProviderError>>;

/// Future type returned by [`Provider::stream_chat`].
///
/// Hand-written instead of an `async-trait`-style macro so `dyn Provider` stays a
/// dependency-free trait object, matching [`gocode_tools`]'s `ToolFuture` convention.
pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ChatStream, ProviderError>> + Send + 'a>>;

/// The capability boundary between the Agent and a concrete model-inference backend.
///
/// Implementations own every provider-specific detail: authentication, endpoints, wire request
/// and response shapes, and streaming protocol. Callers only ever see normalized
/// [`ChatRequest`]/[`ChatStreamEvent`]/[`ProviderError`] values.
pub trait Provider: Send + Sync {
    /// Starts a streamed chat request and returns its normalized event channel.
    ///
    /// Implementations must stop producing events promptly once `cancellation` fires.
    fn stream_chat(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_>;
}

/// Stable, safe-to-display provider failure categories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// No credential was available for this provider.
    MissingCredential,
    /// The provider rejected the configured credential.
    InvalidCredential,
    /// The request could not reach the provider or the connection failed mid-flight.
    Network(String),
    /// The provider did not respond before the client's timeout.
    Timeout,
    /// The provider asked the client to slow down.
    RateLimited,
    /// The requested model is not available from this provider.
    ModelNotFound(String),
    /// The request needed a capability the model or provider does not support.
    UnsupportedCapability(String),
    /// The normalized request could not be translated into a valid provider request.
    InvalidRequest(String),
    /// The provider returned a response that could not be parsed or normalized safely.
    InvalidResponse(String),
    /// A provider-side failure occurred.
    Server {
        /// HTTP status code, when the transport is HTTP.
        status: Option<u16>,
        /// Safe, non-sensitive diagnostic message.
        message: String,
    },
    /// The request was cancelled before the provider finished.
    Cancelled,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCredential => formatter.write_str("no credential is configured"),
            Self::InvalidCredential => formatter.write_str("the credential was rejected"),
            Self::Network(message) => write!(formatter, "network error: {message}"),
            Self::Timeout => formatter.write_str("the request timed out"),
            Self::RateLimited => formatter.write_str("the provider rate-limited this request"),
            Self::ModelNotFound(model) => write!(formatter, "model {model} was not found"),
            Self::UnsupportedCapability(capability) => {
                write!(formatter, "unsupported capability: {capability}")
            }
            Self::InvalidRequest(message) => write!(formatter, "invalid request: {message}"),
            Self::InvalidResponse(message) => write!(formatter, "invalid response: {message}"),
            Self::Server { status, message } => match status {
                Some(status) => write!(formatter, "provider error ({status}): {message}"),
                None => write!(formatter, "provider error: {message}"),
            },
            Self::Cancelled => formatter.write_str("the request was cancelled"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// Test doubles reusable by any crate that consumes [`Provider`], without depending on a real
/// provider implementation.
pub mod testing {
    use std::{collections::VecDeque, sync::Mutex};

    use super::{
        CancellationToken, ChatRequest, ChatStreamEvent, Provider, ProviderError, ProviderFuture,
    };

    /// A [`Provider`] that replays one scripted turn per call to
    /// [`Provider::stream_chat`], in call order.
    ///
    /// Each turn is a fixed sequence of events delivered on the returned channel. Once the
    /// script is exhausted, further calls fail loudly with [`ProviderError::InvalidRequest`]
    /// instead of hanging, so a test with an unexpectedly long agent loop fails fast.
    pub struct FakeProvider {
        turns: Mutex<VecDeque<Vec<Result<ChatStreamEvent, ProviderError>>>>,
    }

    impl FakeProvider {
        /// Creates a provider that replays `turns` in order, one per `stream_chat` call.
        #[must_use]
        pub fn script(turns: Vec<Vec<Result<ChatStreamEvent, ProviderError>>>) -> Self {
            Self {
                turns: Mutex::new(turns.into_iter().collect()),
            }
        }
    }

    impl Provider for FakeProvider {
        fn stream_chat(
            &self,
            _request: ChatRequest,
            _cancellation: CancellationToken,
        ) -> ProviderFuture<'_> {
            Box::pin(async move {
                let events = self
                    .turns
                    .lock()
                    .expect("fake provider script lock should not be poisoned")
                    .pop_front()
                    .ok_or_else(|| {
                        ProviderError::InvalidRequest("FakeProvider script exhausted".into())
                    })?;
                let (sender, receiver) = tokio::sync::mpsc::channel(events.len().max(1));
                for event in events {
                    let _ = sender.send(event).await;
                }
                Ok(receiver)
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::FakeProvider;
        use crate::{
            CancellationToken, ChatRequest, ChatStreamEvent, FinishReason, Provider, ProviderError,
        };

        #[tokio::test]
        async fn replays_scripted_turns_in_order() {
            let provider = FakeProvider::script(vec![
                vec![
                    Ok(ChatStreamEvent::TextDelta("Hi".into())),
                    Ok(ChatStreamEvent::Finished(FinishReason::Stop)),
                ],
                vec![Ok(ChatStreamEvent::Finished(FinishReason::Stop))],
            ]);
            let request = ChatRequest::single_user("model", "hello");

            let mut first = provider
                .stream_chat(request.clone(), CancellationToken::new())
                .await
                .expect("first scripted turn should stream");
            assert_eq!(
                first.recv().await,
                Some(Ok(ChatStreamEvent::TextDelta("Hi".into())))
            );

            let mut second = provider
                .stream_chat(request, CancellationToken::new())
                .await
                .expect("second scripted turn should stream");
            assert_eq!(
                second.recv().await,
                Some(Ok(ChatStreamEvent::Finished(FinishReason::Stop)))
            );
        }

        #[tokio::test]
        async fn exhausted_script_fails_loudly_instead_of_hanging() {
            let provider = FakeProvider::script(vec![]);

            let outcome = provider
                .stream_chat(
                    ChatRequest::single_user("model", "hello"),
                    CancellationToken::new(),
                )
                .await;

            assert!(matches!(outcome, Err(ProviderError::InvalidRequest(_))));
        }
    }
}

/// Whether a model can use function tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCapability {
    /// Tool support is absent or cannot be established safely.
    Unsupported,
    /// Tool support is known for the selected model.
    Supported,
}

/// Supported normalized reasoning controls for a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThinkingCapability {
    /// No reasoning-specific request fields may be sent.
    Unsupported,
    /// The model accepts one of the listed provider-defined effort names.
    Effort {
        /// Accepted provider-defined effort names.
        levels: Vec<String>,
        /// Provider-recommended default, when known.
        default: Option<String>,
    },
}
