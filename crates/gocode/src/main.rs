//! Gocode application bootstrap.

use std::{collections::HashMap, path::Path, sync::Arc};

use gocode_agent::{Agent, AgentEvent, AgentRequest};
use gocode_core::{
    AppCommand, AppError, EnvironmentPaths, Platform, PlatformPaths, RuntimeChannels,
    bootstrap_with_paths,
};
use gocode_credentials::{CredentialStore, NativeCredentialStore, SecretString};
use gocode_provider_nvidia::NvidiaProvider;
use gocode_tools::{
    ChangeKind, ToolRegistry, ToolStatus, builtin_registry,
    permissions::{
        ApproveEverythingPolicy, DefaultPermissionPolicy, PermissionContext, PermissionPolicy,
        PermissionRequest, PermissionResolver, PlanPermissionPolicy, ResolveFuture,
    },
};
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing_subscriber::prelude::*;

/// Slot for the single active permission prompt, shared between the resolver and the command
/// loop so a user response (or a run cancellation) can reach the waiting resolver future.
type PendingPermission = Arc<Mutex<Option<oneshot::Sender<bool>>>>;

/// [`PermissionResolver`] that shows the prompt in the TUI and waits for the user's answer.
struct TuiPermissionResolver {
    event_tx: mpsc::Sender<gocode_core::AppEvent>,
    pending: PendingPermission,
}

impl PermissionResolver for TuiPermissionResolver {
    fn resolve<'a>(&'a self, request: &'a PermissionRequest) -> ResolveFuture<'a> {
        Box::pin(async move {
            let (response_tx, response_rx) = oneshot::channel();
            *self.pending.lock().await = Some(response_tx);

            if self
                .event_tx
                .send(gocode_core::AppEvent::PermissionRequested {
                    summary: request.summary.clone(),
                    working_directory: request.working_directory.display().to_string(),
                })
                .await
                .is_err()
            {
                return false;
            }

            response_rx.await.unwrap_or(false)
        })
    }
}

/// Forwards one agent run's events to the interface, translating them to the provider-neutral
/// [`gocode_core::AppEvent`] contract.
async fn bridge_agent_events(
    mut agent_events: mpsc::Receiver<AgentEvent>,
    event_tx: mpsc::Sender<gocode_core::AppEvent>,
) {
    let mut streamed_any_text = false;
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut tools_with_output: std::collections::HashSet<String> = std::collections::HashSet::new();

    while let Some(event) = agent_events.recv().await {
        let mapped = match event {
            AgentEvent::Started | AgentEvent::ToolStarted(_) => None,
            AgentEvent::StateChanged(state) => match state {
                gocode_agent::AgentState::Inference => {
                    Some(gocode_core::AppEvent::AgentStateChanged(
                        gocode_core::AgentActivityState::Thinking,
                    ))
                }
                gocode_agent::AgentState::ExecutingTools
                | gocode_agent::AgentState::WaitingForPermission => {
                    Some(gocode_core::AppEvent::AgentStateChanged(
                        gocode_core::AgentActivityState::RunningTools,
                    ))
                }
                gocode_agent::AgentState::Idle
                | gocode_agent::AgentState::Preparing
                | gocode_agent::AgentState::Finalizing
                | gocode_agent::AgentState::Completed
                | gocode_agent::AgentState::Cancelled
                | gocode_agent::AgentState::Failed => None,
            },
            AgentEvent::TextDelta(delta) => {
                streamed_any_text = true;
                Some(gocode_core::AppEvent::AssistantTextDelta(delta))
            }
            AgentEvent::ToolRequested(call) => {
                let id = call.id.as_str().to_string();
                let name = call.name.as_str().to_string();
                tool_names.insert(id.clone(), name.clone());
                Some(gocode_core::AppEvent::ToolActivity {
                    id,
                    name,
                    status: gocode_core::ToolActivityStatus::Started,
                    detail: "running".into(),
                })
            }
            AgentEvent::ToolOutputChunk { id, chunk } => {
                let id = id.as_str().to_string();
                tools_with_output.insert(id.clone());
                Some(gocode_core::AppEvent::ToolOutputChunk { id, chunk })
            }
            AgentEvent::ToolFinished(result) => {
                let id = result.call_id.as_str().to_string();
                let name = tool_names.get(&id).cloned().unwrap_or_default();
                let status = match result.status {
                    ToolStatus::Success => gocode_core::ToolActivityStatus::Succeeded,
                    ToolStatus::Failed => gocode_core::ToolActivityStatus::Failed,
                    ToolStatus::Cancelled => gocode_core::ToolActivityStatus::Cancelled,
                    ToolStatus::Denied => gocode_core::ToolActivityStatus::Denied,
                };
                if !tools_with_output.contains(&id) && !result.output.content.is_empty() {
                    let _ = event_tx
                        .send(gocode_core::AppEvent::ToolOutputChunk {
                            id: id.clone(),
                            chunk: result.output.content.clone(),
                        })
                        .await;
                }
                Some(gocode_core::AppEvent::ToolActivity {
                    id,
                    name,
                    status,
                    detail: first_line(&result.output.content, 96),
                })
            }
            AgentEvent::FileChanged(change) => Some(gocode_core::AppEvent::FileChanged {
                path: change.path.display().to_string(),
                kind: match change.kind {
                    ChangeKind::Created => "created",
                    ChangeKind::Modified => "modified",
                    ChangeKind::Deleted => "deleted",
                }
                .to_string(),
            }),
            AgentEvent::Warning(warning) => Some(gocode_core::AppEvent::AgentWarning(
                describe_warning(&warning),
            )),
            AgentEvent::Completed(completion) => Some(gocode_core::AppEvent::AgentCompleted {
                final_text: (!streamed_any_text).then_some(completion.final_text),
                turns: completion.stats.turns,
                tool_calls: completion.stats.tool_calls,
                failed_tool_calls: completion.stats.failed_tool_calls,
            }),
            AgentEvent::Cancelled => Some(gocode_core::AppEvent::AgentCancelled),
        };

        if let Some(mapped) = mapped
            && event_tx.send(mapped).await.is_err()
        {
            return;
        }
    }
}

fn describe_warning(warning: &gocode_agent::AgentWarning) -> String {
    match warning {
        gocode_agent::AgentWarning::UnknownTool(name) => {
            format!("The model requested an unavailable tool: {name}")
        }
        gocode_agent::AgentWarning::LoopDetected(name) => {
            format!("Stopped a repeating tool call: {name}")
        }
    }
}

fn first_line(text: &str, max_chars: usize) -> String {
    let first = text.lines().next().unwrap_or_default();
    if first.chars().count() > max_chars {
        format!("{}…", first.chars().take(max_chars).collect::<String>())
    } else if first.is_empty() {
        "done".into()
    } else {
        first.into()
    }
}

#[tokio::main]
async fn main() {
    gocode_tui::install_panic_hook();

    match run_application().await {
        Ok(()) => {}
        Err(error) => {
            eprintln!("Gocode could not start: {error}");
            std::process::exit(1);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the bootstrap composition root deliberately keeps runtime ownership visible"
)]
async fn run_application() -> Result<(), AppError> {
    let paths = application_paths(current_platform(), process_environment())?;
    let _log_guard = init_logging(&paths.state_dir)?;
    let bootstrap = bootstrap_with_paths(&paths)?;
    tracing::info!("application bootstrapped");

    let environment_credential = std::env::var("NVIDIA_API_KEY").ok();

    let (client, mut driver) = RuntimeChannels::create();
    driver
        .event_tx
        .send(bootstrap.event)
        .await
        .map_err(|error| AppError::Initialization(format!("could not send boot event: {error}")))?;
    start_update_check(driver.event_tx.clone());
    let tui = gocode_tui::run(client.event_rx, client.command_tx);
    let runtime = async move {
        let credential_store = NativeCredentialStore::new();
        let mut provider = None;
        let mut selected_model = None;
        let mut model_catalog: Vec<gocode_core::Model> = Vec::new();
        let mut reasoning_effort: Option<String> =
            bootstrap.resolved_config.reasoning_effort.clone();
        let mut permission_mode = gocode_core::PermissionMode::default();
        let mut active_cancellation = None;
        let config_path = paths.config_dir.join("config.toml");
        let tool_registry: Arc<ToolRegistry> = Arc::new(builtin_registry());
        let permission_pending: PendingPermission = Arc::new(Mutex::new(None));
        let instructions =
            std::fs::read_to_string(bootstrap.project.gocode_dir.join("instructions.md")).ok();

        if let Some(effort) = reasoning_effort.clone() {
            driver
                .event_tx
                .send(gocode_core::AppEvent::ReasoningEffortChanged {
                    effort: Some(effort),
                    announce: false,
                })
                .await
                .map_err(|error| {
                    AppError::Initialization(format!(
                        "could not restore the saved reasoning effort: {error}"
                    ))
                })?;
        }

        let stored_credential = environment_credential
            .map(SecretString::new)
            .or_else(|| credential_store.get_nvidia().ok().flatten());

        if let Some(secret) = stored_credential {
            let candidate = NvidiaProvider::hosted(secret);
            match candidate.list_models().await {
                Ok(models) => {
                    let model_ids: Vec<String> = models
                        .iter()
                        .map(|model| model.id.as_str().into())
                        .collect();
                    model_catalog = models;
                    provider = Some(candidate);

                    let remembered_model = bootstrap
                        .resolved_config
                        .model
                        .as_deref()
                        .filter(|model| model_ids.iter().any(|id| id == model))
                        .map(str::to_string);

                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::ModelsAvailable(model_ids))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not send discovered models: {error}"
                            ))
                        })?;

                    if let Some(model) = remembered_model {
                        selected_model = Some(model.clone());
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::ModelSelected(model))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not confirm remembered model: {error}"
                                ))
                            })?;
                    }
                }
                Err(error) => driver
                    .event_tx
                    .send(gocode_core::AppEvent::CredentialValidationFailed(
                        error.to_string(),
                    ))
                    .await
                    .map_err(|send_error| {
                        AppError::Initialization(format!(
                            "could not report provider failure: {send_error}"
                        ))
                    })?,
            }
        } else {
            driver
                .event_tx
                .send(gocode_core::AppEvent::CredentialRequired)
                .await
                .map_err(|error| {
                    AppError::Initialization(format!(
                        "could not start credential onboarding: {error}"
                    ))
                })?;
        }

        while let Some(command) = driver.command_rx.recv().await {
            match command {
                AppCommand::Exit => return Ok(AppCommand::Exit),
                AppCommand::Resize { columns, rows } => driver
                    .event_tx
                    .send(gocode_core::AppEvent::TerminalResized { columns, rows })
                    .await
                    .map_err(|error| {
                        AppError::Initialization(format!(
                            "could not send resize event to interface: {error}"
                        ))
                    })?,
                AppCommand::SubmitCredential(credential) => {
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::CredentialValidationStarted)
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not show credential validation: {error}"
                            ))
                        })?;
                    let secret = SecretString::new(credential);
                    let candidate = NvidiaProvider::hosted(secret.clone());
                    match candidate.list_models().await {
                        Ok(models) => {
                            if let Err(error) = credential_store.save_nvidia(&secret) {
                                driver.event_tx.send(gocode_core::AppEvent::ProviderFailed(format!("Credential validated but could not be saved securely: {error:?}"))).await.map_err(|send_error| AppError::Initialization(format!("could not report credential-store failure: {send_error}")))?;
                            }
                            let model_ids = models
                                .iter()
                                .map(|model| model.id.as_str().into())
                                .collect();
                            model_catalog = models;
                            provider = Some(candidate);
                            driver
                                .event_tx
                                .send(gocode_core::AppEvent::ModelsAvailable(model_ids))
                                .await
                                .map_err(|error| {
                                    AppError::Initialization(format!(
                                        "could not send discovered models: {error}"
                                    ))
                                })?;
                        }
                        Err(error) => driver
                            .event_tx
                            .send(gocode_core::AppEvent::CredentialValidationFailed(
                                error.to_string(),
                            ))
                            .await
                            .map_err(|send_error| {
                                AppError::Initialization(format!(
                                    "could not report credential failure: {send_error}"
                                ))
                            })?,
                    }
                }
                AppCommand::SelectModel(model) => {
                    gocode_core::save_global_config(
                        &config_path,
                        Some("nvidia"),
                        Some(&model),
                        reasoning_effort.as_deref(),
                    )?;
                    selected_model = Some(model.clone());
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::ModelSelected(model))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not confirm model selection: {error}"
                            ))
                        })?;
                }
                AppCommand::SubmitChat(message) => {
                    let (Some(provider), Some(model)) = (&provider, &selected_model) else {
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::ProviderFailed(
                                "Select an NVIDIA model before chatting.".into(),
                            ))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not report provider state: {error}"
                                ))
                            })?;
                        continue;
                    };
                    let tools_enabled = model_catalog
                        .iter()
                        .find(|candidate| candidate.id.as_str() == model)
                        .is_some_and(|candidate| {
                            candidate.capabilities.tools == gocode_core::ToolCapability::Supported
                        });

                    let cancellation = gocode_core::CancellationToken::new();
                    active_cancellation = Some(cancellation.clone());
                    let resolver = Arc::new(TuiPermissionResolver {
                        event_tx: driver.event_tx.clone(),
                        pending: permission_pending.clone(),
                    });
                    let policy: Arc<dyn PermissionPolicy> = match permission_mode {
                        gocode_core::PermissionMode::Auto => {
                            Arc::new(DefaultPermissionPolicy::editing())
                        }
                        gocode_core::PermissionMode::Plan => Arc::new(PlanPermissionPolicy),
                        gocode_core::PermissionMode::Approve => Arc::new(ApproveEverythingPolicy),
                    };
                    let permissions = PermissionContext::new(policy, resolver);
                    let agent = Agent::new(
                        Arc::new(provider.clone()),
                        tool_registry.clone(),
                        permissions,
                        gocode_agent::AgentLimits::default(),
                    );
                    let request = AgentRequest {
                        prompt: message,
                        model: gocode_core::ModelId::new(model.clone()),
                        project_root: bootstrap.project.root.clone(),
                        instructions: instructions.clone(),
                        tools_enabled,
                        reasoning_effort: reasoning_effort.clone(),
                    };
                    let event_tx = driver.event_tx.clone();
                    let (agent_events_tx, agent_events_rx) = mpsc::channel(64);
                    tokio::spawn(bridge_agent_events(agent_events_rx, event_tx.clone()));
                    tokio::spawn(async move {
                        if let Err(error) = agent.run(request, agent_events_tx, cancellation).await
                        {
                            match error {
                                gocode_agent::AgentError::Cancelled => {}
                                gocode_agent::AgentError::Provider(provider_error) => {
                                    let event = match provider_error.severity() {
                                        gocode_core::ErrorSeverity::Blocking => {
                                            gocode_core::AppEvent::BlockingError(
                                                provider_error.to_string(),
                                            )
                                        }
                                        gocode_core::ErrorSeverity::Recoverable => {
                                            gocode_core::AppEvent::ProviderFailed(
                                                provider_error.to_string(),
                                            )
                                        }
                                    };
                                    let _ = event_tx.send(event).await;
                                }
                                other => {
                                    let _ = event_tx
                                        .send(gocode_core::AppEvent::AgentWarning(
                                            other.to_string(),
                                        ))
                                        .await;
                                }
                            }
                        }
                    });
                }
                AppCommand::CancelProviderRequest => {
                    if let Some(cancellation) = active_cancellation.take() {
                        cancellation.cancel();
                    }
                    if let Some(pending) = permission_pending.lock().await.take() {
                        let _ = pending.send(false);
                    }
                }
                AppCommand::PermissionResponse(approved) => {
                    if let Some(pending) = permission_pending.lock().await.take() {
                        let _ = pending.send(approved);
                    }
                }
                AppCommand::RejectUpdate => {
                    tracing::info!("update declined for this startup");
                }
                AppCommand::SetReasoningEffort(effort) => {
                    reasoning_effort = effort.clone();
                    gocode_core::save_global_config(
                        &config_path,
                        Some("nvidia"),
                        selected_model.as_deref(),
                        reasoning_effort.as_deref(),
                    )?;
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::ReasoningEffortChanged {
                            effort,
                            announce: true,
                        })
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not confirm reasoning-effort selection: {error}"
                            ))
                        })?;
                }
                AppCommand::SetPermissionMode(mode) => {
                    permission_mode = mode;
                }
                AppCommand::AcceptUpdate => {
                    match prepare_windows_update(&paths.cache_dir, driver.event_tx.clone()).await {
                        Ok(()) => {
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::ExitForUpdate)
                                .await;
                        }
                        Err(error) => {
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::UpdateFailed(error))
                                .await;
                        }
                    }
                }
            }
        }

        Err(AppError::Initialization(
            "interface closed without an exit command".into(),
        ))
    };
    let (tui_result, runtime_result) = tokio::join!(tui, runtime);
    tui_result.map_err(|error| AppError::Io(format!("terminal failed: {error}")))?;
    runtime_result?;
    tracing::info!("application shutdown requested");

    Ok(())
}

fn start_update_check(event_tx: mpsc::Sender<gocode_core::AppEvent>) {
    if !cfg!(windows) || cfg!(debug_assertions) {
        return;
    }
    tokio::spawn(async move {
        let result = async {
            let source = gocode_updater::GitHubReleaseSource::new()?;
            let releases = source.stable_releases().await?;
            let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
                .map_err(|error| gocode_updater::UpdateError::InvalidRelease(error.to_string()))?;
            Ok::<_, gocode_updater::UpdateError>(gocode_updater::available_update(
                &current, releases,
            ))
        }
        .await;
        match result {
            Ok(Some(update)) => {
                let _ = event_tx
                    .send(gocode_core::AppEvent::UpdateAvailable {
                        version: update.version.to_string(),
                        notes: update.notes,
                    })
                    .await;
            }
            Ok(None) => {}
            Err(error) => tracing::info!("update check skipped: {error}"),
        }
    });
}

async fn prepare_windows_update(
    cache_dir: &Path,
    event_tx: mpsc::Sender<gocode_core::AppEvent>,
) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Automatic updates are available on Windows only. Download the latest Linux archive and replace the installation manually.".into());
    }
    let source = gocode_updater::GitHubReleaseSource::new().map_err(|e| e.to_string())?;
    let releases = source.stable_releases().await.map_err(|e| e.to_string())?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    let update = gocode_updater::available_update(&current, releases)
        .ok_or_else(|| "No newer Windows update is available.".to_string())?;
    let staging = cache_dir.join("update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let _ = event_tx
        .send(gocode_core::AppEvent::UpdateProgress(
            "Downloading update…".into(),
        ))
        .await;
    let client = reqwest::Client::builder()
        .user_agent("gocode-updater")
        .build()
        .map_err(|e| e.to_string())?;
    let archive =
        gocode_updater::download_to_staging(&client, &update.archive.download_url, &staging)
            .await
            .map_err(|e| e.to_string())?;
    let sums_url = gocode_updater::official_download_url(&update.checksums.download_url)
        .map_err(|e| e.to_string())?;
    let sums = client
        .get(sums_url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let expected =
        gocode_updater::checksum_for(&sums, &update.archive.name).map_err(|e| e.to_string())?;
    let _ = event_tx
        .send(gocode_core::AppEvent::UpdateProgress(
            "Verifying update…".into(),
        ))
        .await;
    gocode_updater::verify_sha256(&archive, &expected).map_err(|e| e.to_string())?;
    let (staged_app, _) =
        gocode_updater::extract_windows_archive(&archive, &staging.join("unpacked"))
            .map_err(|e| e.to_string())?;
    let installed = std::env::current_exe().map_err(|e| e.to_string())?;
    let updater = installed
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("gocode-updater.exe");
    if !updater.is_file() {
        return Err(
            "The installed gocode-updater.exe is missing; reinstall Gocode and try again.".into(),
        );
    }
    let _ = event_tx
        .send(gocode_core::AppEvent::UpdateProgress(
            "Restarting to install update…".into(),
        ))
        .await;
    std::process::Command::new(updater)
        .arg(std::process::id().to_string())
        .arg(staged_app)
        .arg(installed)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn init_logging(
    state_dir: &std::path::Path,
) -> Result<tracing_appender::non_blocking::WorkerGuard, AppError> {
    let logs_dir = state_dir.join("logs");
    std::fs::create_dir_all(&logs_dir).map_err(|error| {
        AppError::Io(format!("could not create {}: {error}", logs_dir.display()))
    })?;
    let appender = tracing_appender::rolling::daily(logs_dir, "gocode.jsonl");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_ansi(false)
            .with_writer(writer)
            .with_filter(
                tracing_subscriber::filter::Targets::new()
                    .with_target("gocode", tracing::Level::INFO),
            ),
    );
    tracing::subscriber::set_global_default(subscriber).map_err(|error| {
        AppError::Initialization(format!("could not initialize logging: {error}"))
    })?;

    Ok(guard)
}

fn application_paths(
    platform: Platform,
    environment: EnvironmentPaths,
) -> Result<PlatformPaths, gocode_core::AppError> {
    PlatformPaths::from_environment(platform, environment)
}

fn current_platform() -> Platform {
    if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

fn process_environment() -> EnvironmentPaths {
    EnvironmentPaths {
        home: std::env::var_os("HOME").map(Into::into),
        user_profile: std::env::var_os("USERPROFILE").map(Into::into),
        xdg: gocode_core::XdgDirectories {
            config_home: std::env::var_os("XDG_CONFIG_HOME").map(Into::into),
            state_home: std::env::var_os("XDG_STATE_HOME").map(Into::into),
            cache_home: std::env::var_os("XDG_CACHE_HOME").map(Into::into),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gocode_core::{EnvironmentPaths, Platform};

    use super::application_paths;

    #[test]
    fn application_paths_use_the_platform_environment_contract() {
        let paths = application_paths(
            Platform::Linux,
            EnvironmentPaths {
                home: Some("/home/alice".into()),
                ..EnvironmentPaths::default()
            },
        )
        .expect("paths should resolve");

        assert_eq!(paths.config_dir, Path::new("/home/alice/.config/gocode"));
    }
}
