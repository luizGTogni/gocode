/// Intent emitted by an interface and handled by the application runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    /// Request an orderly application shutdown.
    Exit,
    /// Notify the runtime that the terminal viewport changed dimensions.
    Resize { columns: u16, rows: u16 },
}

/// Fact emitted by the application runtime and rendered by an interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    /// Application bootstrap has begun.
    BootStarted,
    /// Application bootstrap completed and the primary interface is ready.
    BootCompleted,
    /// Confirm the terminal viewport dimensions known by the runtime.
    TerminalResized { columns: u16, rows: u16 },
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
            },
            &ConfigValues {
                provider: Some("project-provider".into()),
                model: Some("project-model".into()),
            },
            &ConfigValues {
                provider: Some("global-provider".into()),
                model: Some("global-model".into()),
            },
            &ConfigValues {
                provider: Some("provider-default".into()),
                model: Some("provider-model".into()),
            },
            &ConfigValues {
                provider: Some("built-in-provider".into()),
                model: Some("built-in-model".into()),
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
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;

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
    std::fs::create_dir_all(gocode_dir.join("sessions")).map_err(|error| {
        AppError::Io(format!(
            "could not create {}: {error}",
            gocode_dir.display()
        ))
    })?;

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

#[derive(Deserialize)]
struct RawGlobalConfig {
    schema_version: u32,
    default_provider: Option<String>,
    default_model: Option<String>,
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
        let mut temporary_file = std::fs::File::create(&temporary_path)?;
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
