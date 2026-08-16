//! Gocode application bootstrap.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use gocode_agent::{Agent, AgentEvent, AgentRequest};
use gocode_core::{
    AUTO_COMPACT_TOKEN_THRESHOLD, AppCommand, AppError, EnvironmentPaths, Platform, PlatformPaths,
    ProjectContext, RuntimeChannels, bootstrap_with_paths,
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
                last_input_tokens: completion.stats.last_input_tokens,
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

/// Loads the merged global+project MCP server configuration, connects every enabled server,
/// and returns a tool registry with the built-ins plus every discovered MCP tool. Best-effort:
/// a server that fails to connect is reported as an [`gocode_core::AppEvent::AgentWarning`] and
/// simply contributes no tools, rather than preventing startup.
async fn connect_configured_mcp_servers(
    paths: &PlatformPaths,
    project: &ProjectContext,
    event_tx: &mpsc::Sender<gocode_core::AppEvent>,
) -> ToolRegistry {
    let mut registry = builtin_registry();

    let load_layer = |path: &Path, layer: &str| match gocode_core::load_or_default_mcp_config(path)
    {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!("could not load {layer} mcp.toml: {error}");
            gocode_core::McpConfig::default()
        }
    };
    let global_mcp = load_layer(&paths.mcp_config_path(), "global");
    let project_mcp = load_layer(&project.mcp_config_path(), "project");
    let servers = gocode_core::merge_mcp_servers(&global_mcp, &project_mcp);

    if servers.is_empty() {
        return registry;
    }

    let outcome = gocode_mcp::connect_configured_servers(&servers).await;
    for tool in outcome.tools {
        registry.register(tool);
    }
    for (server_name, error) in outcome.failures {
        let _ = event_tx
            .send(gocode_core::AppEvent::AgentWarning(format!(
                "MCP server '{server_name}' failed to connect: {error}"
            )))
            .await;
    }

    registry
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

/// Everything a compaction (automatic or `/compact`) needs to read and update the active session.
struct CompactionContext<'a> {
    provider: &'a NvidiaProvider,
    model: &'a str,
    session: &'a Arc<Mutex<gocode_core::SessionRecord>>,
    sessions_dir: &'a Path,
}

/// Persists a completed run's history onto its session, then triggers automatic compaction when
/// the reported input-token usage crosses [`AUTO_COMPACT_TOKEN_THRESHOLD`].
async fn handle_completed_run(
    ctx: &CompactionContext<'_>,
    prompt: &str,
    completion: gocode_agent::AgentCompletion,
    auto_compact_enabled: bool,
    event_tx: &mpsc::Sender<gocode_core::AppEvent>,
) {
    let last_input_tokens = completion.stats.last_input_tokens;
    {
        let mut session = ctx.session.lock().await;
        session.history = completion.history;
        session.record_turn(prompt, &completion.final_text);
        let _ = gocode_core::save_session(ctx.sessions_dir, &session);
    }

    if auto_compact_enabled
        && last_input_tokens.is_some_and(|tokens| tokens >= AUTO_COMPACT_TOKEN_THRESHOLD)
    {
        let history_snapshot = ctx.session.lock().await.history.clone();
        compact_and_report(ctx, history_snapshot, true, event_tx).await;
    }
}

/// Summarizes `history` down to one condensed message and stores it on the session, reporting
/// the outcome either way.
async fn compact_and_report(
    ctx: &CompactionContext<'_>,
    history: Vec<gocode_core::ChatMessage>,
    automatic: bool,
    event_tx: &mpsc::Sender<gocode_core::AppEvent>,
) {
    match compact_conversation(ctx.provider, ctx.model, history).await {
        Ok(compacted) => {
            {
                let mut session = ctx.session.lock().await;
                session.history = compacted;
                let _ = gocode_core::save_session(ctx.sessions_dir, &session);
            }
            let _ = event_tx
                .send(gocode_core::AppEvent::ContextCompacted { automatic })
                .await;
        }
        Err(error) => {
            let _ = event_tx
                .send(gocode_core::AppEvent::ContextCompactionFailed(
                    error.to_string(),
                ))
                .await;
        }
    }
}

/// Asks the model to summarize `history`, replacing it with a single condensed message.
///
/// # Errors
///
/// Returns the provider error when the summarization request fails.
async fn compact_conversation(
    provider: &NvidiaProvider,
    model: &str,
    history: Vec<gocode_core::ChatMessage>,
) -> Result<Vec<gocode_core::ChatMessage>, gocode_core::ProviderError> {
    let mut messages = history;
    messages.push(gocode_core::ChatMessage::User(
        "Summarize this conversation so far in a concise paragraph, preserving important facts, \
         decisions, and any unfinished tasks. Respond with only the summary, no preamble."
            .into(),
    ));
    let request = gocode_core::ChatRequest {
        model: gocode_core::ModelId::new(model),
        messages,
        tools: Vec::new(),
        reasoning_effort: None,
    };
    let cancellation = gocode_core::CancellationToken::new();
    let mut stream = provider.stream_chat(request, cancellation).await?;

    let mut summary = String::new();
    while let Some(event) = stream.recv().await {
        if let gocode_core::ChatStreamEvent::TextDelta(delta) = event? {
            summary.push_str(&delta);
        }
    }
    let summary = summary.trim();
    let summary = if summary.is_empty() {
        "(no summary was produced)"
    } else {
        summary
    };

    Ok(vec![gocode_core::ChatMessage::User(format!(
        "(Earlier conversation was compacted to save context. Summary of what happened so far:)\n\
         {summary}"
    ))])
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
        let mut auto_compact_enabled = true;
        let mut staged_update: Option<StagedUpdate> = None;
        let sessions_dir = gocode_core::sessions_dir(&paths.state_dir);
        let current_session: Arc<Mutex<gocode_core::SessionRecord>> =
            Arc::new(Mutex::new(gocode_core::SessionRecord::new()));
        let mut active_cancellation = None;
        let config_path = paths.config_dir.join("config.toml");
        let tool_registry: Arc<ToolRegistry> = Arc::new(
            connect_configured_mcp_servers(&paths, &bootstrap.project, &driver.event_tx).await,
        );
        let permission_pending: PendingPermission = Arc::new(Mutex::new(None));
        let instructions =
            std::fs::read_to_string(bootstrap.project.gocode_dir.join("instructions.md")).ok();
        let project_overview =
            std::fs::read_to_string(bootstrap.project.root.join("AGENTS.md")).ok();

        let custom_commands =
            gocode_core::load_custom_commands(&bootstrap.project.gocode_dir.join("commands"));
        driver
            .event_tx
            .send(gocode_core::AppEvent::CustomCommandsAvailable(
                custom_commands,
            ))
            .await
            .map_err(|error| {
                AppError::Initialization(format!(
                    "could not send discovered custom commands: {error}"
                ))
            })?;

        let global_skills_dir = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| Path::new(&home).join(".agents").join("skills"));
        let project_agents_skills_dir = bootstrap.project.root.join(".agents").join("skills");
        let project_skills_dir = if project_agents_skills_dir.is_dir() {
            project_agents_skills_dir
        } else {
            bootstrap.project.gocode_dir.join("skills")
        };
        let mut skills =
            gocode_core::load_skills(global_skills_dir.as_deref(), &project_skills_dir);
        let disabled_skills = gocode_core::load_disabled_skills(&bootstrap.project.gocode_dir);
        gocode_core::apply_disabled_skills(&mut skills, &disabled_skills);
        let skills_summary = skills.iter().any(|skill| skill.enabled).then(|| {
            skills
                .iter()
                .filter(|skill| skill.enabled)
                .map(|skill| {
                    format!(
                        "- {}: {} (read {} to use)",
                        skill.name,
                        skill.description,
                        skill.path.display()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        driver
            .event_tx
            .send(gocode_core::AppEvent::SkillsAvailable(skills))
            .await
            .map_err(|error| {
                AppError::Initialization(format!("could not send discovered skills: {error}"))
            })?;

        driver
            .event_tx
            .send(gocode_core::AppEvent::ProjectContextAvailable {
                working_directory: bootstrap.project.root.display().to_string(),
            })
            .await
            .map_err(|error| {
                AppError::Initialization(format!(
                    "could not send the project working directory: {error}"
                ))
            })?;
        driver
            .event_tx
            .send(gocode_core::AppEvent::SessionSwitched {
                id: current_session.lock().await.id.clone(),
                name: "New session".into(),
                is_new: true,
                history: Vec::new(),
            })
            .await
            .map_err(|error| {
                AppError::Initialization(format!("could not confirm the initial session: {error}"))
            })?;

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
                    let history_snapshot = current_session.lock().await.history.clone();
                    let prompt = message.clone();
                    let request = AgentRequest {
                        prompt: message,
                        model: gocode_core::ModelId::new(model.clone()),
                        project_root: bootstrap.project.root.clone(),
                        instructions: instructions.clone(),
                        project_overview: project_overview.clone(),
                        skills_summary: skills_summary.clone(),
                        tools_enabled,
                        reasoning_effort: reasoning_effort.clone(),
                        history: history_snapshot,
                    };
                    let event_tx = driver.event_tx.clone();
                    let (agent_events_tx, agent_events_rx) = mpsc::channel(64);
                    tokio::spawn(bridge_agent_events(agent_events_rx, event_tx.clone()));
                    let session_for_run = current_session.clone();
                    let sessions_dir_for_run = sessions_dir.clone();
                    let provider_for_run = provider.clone();
                    let model_for_run = model.clone();
                    tokio::spawn(async move {
                        match agent.run(request, agent_events_tx, cancellation).await {
                            Ok(completion) => {
                                let ctx = CompactionContext {
                                    provider: &provider_for_run,
                                    model: &model_for_run,
                                    session: &session_for_run,
                                    sessions_dir: &sessions_dir_for_run,
                                };
                                handle_completed_run(
                                    &ctx,
                                    &prompt,
                                    completion,
                                    auto_compact_enabled,
                                    &event_tx,
                                )
                                .await;
                            }
                            Err(error) => match error {
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
                            },
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
                AppCommand::SetAutoCompact(enabled) => {
                    auto_compact_enabled = enabled;
                }
                AppCommand::SetSkillEnabled { name, enabled } => {
                    if let Err(error) = gocode_core::set_skill_enabled(
                        &bootstrap.project.gocode_dir,
                        &name,
                        enabled,
                    ) {
                        tracing::warn!("could not persist skill enable state: {error}");
                    }
                }
                AppCommand::CompactContext => {
                    let history_snapshot = current_session.lock().await.history.clone();
                    if history_snapshot.is_empty() {
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::AgentWarning(
                                "Nothing to compact yet.".into(),
                            ))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not report an empty compaction request: {error}"
                                ))
                            })?;
                    } else if let (Some(provider), Some(model)) = (&provider, &selected_model) {
                        let provider = provider.clone();
                        let model = model.clone();
                        let session_for_compact = current_session.clone();
                        let sessions_dir_for_compact = sessions_dir.clone();
                        let event_tx = driver.event_tx.clone();
                        tokio::spawn(async move {
                            let ctx = CompactionContext {
                                provider: &provider,
                                model: &model,
                                session: &session_for_compact,
                                sessions_dir: &sessions_dir_for_compact,
                            };
                            compact_and_report(&ctx, history_snapshot, false, &event_tx).await;
                        });
                    } else {
                        driver
                            .event_tx
                            .send(gocode_core::AppEvent::ContextCompactionFailed(
                                "select a model before compacting".into(),
                            ))
                            .await
                            .map_err(|error| {
                                AppError::Initialization(format!(
                                    "could not report a failed compaction: {error}"
                                ))
                            })?;
                    }
                }
                AppCommand::ClearConversation => {
                    let stale_id = {
                        let mut session = current_session.lock().await;
                        let stale_id = session.id.clone();
                        *session = gocode_core::SessionRecord::new();
                        stale_id
                    };
                    let _ = std::fs::remove_file(sessions_dir.join(format!("{stale_id}.json")));
                }
                AppCommand::NewSession => {
                    let id = {
                        let mut session = current_session.lock().await;
                        *session = gocode_core::SessionRecord::new();
                        session.id.clone()
                    };
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::SessionSwitched {
                            id,
                            name: "New session".into(),
                            is_new: true,
                            history: Vec::new(),
                        })
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not confirm the new session: {error}"
                            ))
                        })?;
                }
                AppCommand::RequestSessionList => {
                    let summaries = gocode_core::list_sessions(&sessions_dir)
                        .unwrap_or_default()
                        .iter()
                        .map(gocode_core::SessionSummary::from)
                        .collect();
                    driver
                        .event_tx
                        .send(gocode_core::AppEvent::SessionListAvailable(summaries))
                        .await
                        .map_err(|error| {
                            AppError::Initialization(format!(
                                "could not send the session list: {error}"
                            ))
                        })?;
                }
                AppCommand::ResumeSession(id) => {
                    match gocode_core::load_session(&sessions_dir, &id) {
                        Ok(mut session) => {
                            session.last_used_at_unix = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |duration| {
                                    i64::try_from(duration.as_secs()).unwrap_or(0)
                                });
                            let _ = gocode_core::save_session(&sessions_dir, &session);
                            let id = session.id.clone();
                            let name = session.name.clone();
                            let history = session.history.clone();
                            *current_session.lock().await = session;
                            driver
                                .event_tx
                                .send(gocode_core::AppEvent::SessionSwitched {
                                    id,
                                    name,
                                    is_new: false,
                                    history,
                                })
                                .await
                                .map_err(|error| {
                                    AppError::Initialization(format!(
                                        "could not confirm the resumed session: {error}"
                                    ))
                                })?;
                        }
                        Err(error) => {
                            driver
                                .event_tx
                                .send(gocode_core::AppEvent::SessionResumeFailed(
                                    error.to_string(),
                                ))
                                .await
                                .map_err(|send_error| {
                                    AppError::Initialization(format!(
                                        "could not report a failed session resume: {send_error}"
                                    ))
                                })?;
                        }
                    }
                }
                AppCommand::AcceptUpdate => {
                    match prepare_update(&paths.cache_dir, driver.event_tx.clone()).await {
                        Ok(staged) => {
                            staged_update = Some(staged);
                            let _ = driver
                                .event_tx
                                .send(gocode_core::AppEvent::UpdateReady {
                                    message: "The update is ready. Gocode will restart to \
                                              finish installing it."
                                        .into(),
                                })
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
                AppCommand::RestartForUpdate => {
                    let Some(staged) = staged_update.take() else {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::UpdateFailed(
                                "No update is staged to install.".into(),
                            ))
                            .await;
                        continue;
                    };
                    if let Err(error) = install_and_restart(&staged) {
                        let _ = driver
                            .event_tx
                            .send(gocode_core::AppEvent::UpdateFailed(error))
                            .await;
                        continue;
                    }
                    let _ = driver
                        .event_tx
                        .send(gocode_core::AppEvent::ExitForUpdate)
                        .await;
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

/// A downloaded, verified, and extracted update sitting in the cache directory, ready to be
/// installed over `installed` once the user confirms on the "Completed" screen.
struct StagedUpdate {
    staged_binary: PathBuf,
    installed: PathBuf,
}

/// Whether this build checks for and can install updates: Windows and Linux release builds
/// only (macOS isn't packaged, and debug builds would otherwise nag during development).
fn updates_supported() -> bool {
    (cfg!(windows) || cfg!(target_os = "linux")) && !cfg!(debug_assertions)
}

fn start_update_check(event_tx: mpsc::Sender<gocode_core::AppEvent>) {
    if !updates_supported() {
        return;
    }
    tokio::spawn(async move {
        let result = async {
            let source = gocode_updater::GitHubReleaseSource::new()?;
            let releases = source.stable_releases().await?;
            let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
                .map_err(|error| gocode_updater::UpdateError::InvalidRelease(error.to_string()))?;
            let suffix = gocode_updater::current_platform_archive_suffix()?;
            Ok::<_, gocode_updater::UpdateError>((
                current.clone(),
                gocode_updater::available_update(&current, releases, suffix),
            ))
        }
        .await;
        match result {
            Ok((current, Some(update))) => {
                let _ = event_tx
                    .send(gocode_core::AppEvent::UpdateAvailable {
                        current_version: current.to_string(),
                        version: update.version.to_string(),
                        notes: update.notes,
                    })
                    .await;
            }
            Ok((_, None)) => {}
            Err(error) => tracing::info!("update check skipped: {error}"),
        }
    });
}

/// Downloads, verifies, and extracts the newest available update for this platform into the
/// cache directory, without touching the installed executable. Reports download progress via
/// `event_tx` as it goes.
async fn prepare_update(
    cache_dir: &Path,
    event_tx: mpsc::Sender<gocode_core::AppEvent>,
) -> Result<StagedUpdate, String> {
    let archive_suffix =
        gocode_updater::current_platform_archive_suffix().map_err(|e| e.to_string())?;
    let source = gocode_updater::GitHubReleaseSource::new().map_err(|e| e.to_string())?;
    let releases = source.stable_releases().await.map_err(|e| e.to_string())?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    let update = gocode_updater::available_update(&current, releases, archive_suffix)
        .ok_or_else(|| "No newer update is available for this platform.".to_string())?;
    let staging = cache_dir.join("update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    let client = reqwest::Client::builder()
        .user_agent("gocode-updater")
        .build()
        .map_err(|e| e.to_string())?;

    let progress_tx = event_tx.clone();
    let mut last_percent = None;
    let archive = gocode_updater::download_to_staging(
        &client,
        &update.archive.download_url,
        &staging,
        move |downloaded, total| {
            let percent = total.map(|total| {
                downloaded
                    .saturating_mul(100)
                    .checked_div(total)
                    .map_or(100, |value| u8::try_from(value.min(100)).unwrap_or(100))
            });
            if percent != last_percent {
                last_percent = percent;
                let _ = progress_tx.try_send(gocode_core::AppEvent::UpdateProgress {
                    percent,
                    message: "Downloading update…".into(),
                });
            }
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    let _ = event_tx
        .send(gocode_core::AppEvent::UpdateProgress {
            percent: Some(100),
            message: "Verifying update…".into(),
        })
        .await;
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
    gocode_updater::verify_sha256(&archive, &expected).map_err(|e| e.to_string())?;

    let _ = event_tx
        .send(gocode_core::AppEvent::UpdateProgress {
            percent: Some(100),
            message: "Extracting update…".into(),
        })
        .await;
    let unpacked = staging.join("unpacked");
    let staged_binary = if cfg!(windows) {
        let (staged_app, _staged_updater) =
            gocode_updater::extract_windows_archive(&archive, &unpacked)
                .map_err(|e| e.to_string())?;
        staged_app
    } else {
        gocode_updater::extract_linux_archive(&archive, &unpacked).map_err(|e| e.to_string())?
    };
    let installed = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(StagedUpdate {
        staged_binary,
        installed,
    })
}

/// Installs a staged update over the running installation and starts the replacement process.
///
/// On Windows the running executable is locked, so this hands off to the separately installed
/// `gocode-updater.exe` helper, which waits for this process to exit before swapping files and
/// restarting. On Linux the executable can be replaced in place (an atomic rename doesn't
/// require the file to be closed), so this does it directly and restarts immediately.
fn install_and_restart(staged: &StagedUpdate) -> Result<(), String> {
    if cfg!(windows) {
        let updater = staged
            .installed
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("gocode-updater.exe");
        if !updater.is_file() {
            return Err(
                "The installed gocode-updater.exe is missing; reinstall Gocode and try again."
                    .into(),
            );
        }
        std::process::Command::new(updater)
            .arg(std::process::id().to_string())
            .arg(&staged.staged_binary)
            .arg(&staged.installed)
            .spawn()
            .map_err(|error| {
                format!("Gocode could not restart automatically ({error}). Please reopen Gocode manually.")
            })?;
        return Ok(());
    }
    gocode_updater::replace_with_rollback(&staged.staged_binary, &staged.installed)
        .map_err(|error| error.to_string())?;
    gocode_updater::restart(&staged.installed, &[]).map_err(|error| {
        format!(
            "Gocode was updated but could not restart automatically ({error}). Please reopen \
             Gocode manually."
        )
    })
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
