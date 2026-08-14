//! Gocode application bootstrap.

use gocode_core::{
    AppCommand, AppError, EnvironmentPaths, Platform, PlatformPaths, RuntimeChannels,
    bootstrap_with_paths,
};
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

async fn run_application() -> Result<(), AppError> {
    let paths = application_paths(current_platform(), process_environment())?;
    let _log_guard = init_logging(&paths.state_dir)?;
    let bootstrap = bootstrap_with_paths(&paths)?;
    tracing::info!("application bootstrapped");

    let (client, mut driver) = RuntimeChannels::create();
    driver
        .event_tx
        .send(bootstrap.event)
        .await
        .map_err(|error| AppError::Initialization(format!("could not send boot event: {error}")))?;
    driver
        .event_tx
        .send(gocode_core::AppEvent::BootCompleted)
        .await
        .map_err(|error| {
            AppError::Initialization(format!("could not complete boot event: {error}"))
        })?;
    let tui = gocode_tui::run(client.event_rx, client.command_tx);
    let runtime = async move {
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
