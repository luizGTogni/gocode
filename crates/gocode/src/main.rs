//! Gocode application bootstrap.

use gocode_core::{
    AppCommand, AppError, EnvironmentPaths, Platform, PlatformPaths, RuntimeChannels,
    bootstrap_with_paths,
};
use gocode_credentials::{CredentialStore, NativeCredentialStore, SecretString};
use gocode_provider_nvidia::NvidiaProvider;
use tracing_subscriber::prelude::*;

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
    let tui = gocode_tui::run(client.event_rx, client.command_tx);
    let runtime = async move {
        let credential_store = NativeCredentialStore::new();
        let mut provider = None;
        let mut selected_model = None;
        let mut active_cancellation = None;
        let config_path = paths.config_dir.join("config.toml");

        if let Some(key) = environment_credential {
            let candidate = NvidiaProvider::hosted(SecretString::new(key));
            match candidate.list_models().await {
                Ok(models) => {
                    let model_ids = models
                        .into_iter()
                        .map(|model| model.id.as_str().into())
                        .collect();
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
                                .into_iter()
                                .map(|model| model.id.as_str().into())
                                .collect();
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
                    gocode_core::save_global_model_selection(&config_path, "nvidia", &model)?;
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
                    let cancellation = gocode_core::CancellationToken::new();
                    active_cancellation = Some(cancellation.clone());
                    let provider = provider.clone();
                    let model = model.clone();
                    let event_tx = driver.event_tx.clone();
                    tokio::spawn(async move {
                        let stream = provider
                            .stream_chat(
                                gocode_core::ChatRequest::single_user(model, message),
                                cancellation,
                            )
                            .await;
                        let Ok(mut stream) = stream else {
                            let message = stream
                                .expect_err("stream result is known to be an error")
                                .to_string();
                            let _ = event_tx
                                .send(gocode_core::AppEvent::ProviderFailed(message))
                                .await;
                            return;
                        };
                        while let Some(event) = stream.recv().await {
                            match event {
                                Ok(gocode_core::ChatStreamEvent::TextDelta(delta)) => {
                                    let _ = event_tx
                                        .send(gocode_core::AppEvent::AssistantTextDelta(delta))
                                        .await;
                                }
                                Ok(
                                    gocode_core::ChatStreamEvent::RequestId(_)
                                    | gocode_core::ChatStreamEvent::ToolCallDelta(_)
                                    | gocode_core::ChatStreamEvent::Usage(_)
                                    | gocode_core::ChatStreamEvent::Finished(_),
                                ) => {}
                                Err(_) => {
                                    let _ = event_tx
                                        .send(gocode_core::AppEvent::ProviderFailed(
                                            "NVIDIA stream ended unexpectedly.".into(),
                                        ))
                                        .await;
                                    return;
                                }
                            }
                        }
                    });
                }
                AppCommand::CancelProviderRequest => {
                    if let Some(cancellation) = active_cancellation.take() {
                        cancellation.cancel();
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
