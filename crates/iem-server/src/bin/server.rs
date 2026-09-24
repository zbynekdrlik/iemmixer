//! Standalone server binary for E2E testing and CI
//!
//! This binary runs the iem-server without Tauri desktop shell.

use iem_core::Config;
use iem_server::ServerConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize simple logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("iem_server=info".parse().unwrap())
                .add_directive("iem_server::proxy=debug".parse().unwrap()),
        )
        .init();

    tracing::info!("Starting IEM Mixer Server v{}", iem_core::VERSION);

    // Load config from file or environment variable
    let config_path =
        std::env::var("IEM_CONFIG_PATH").unwrap_or_else(|_| "config.yaml".to_string());

    let config = match Config::load(&config_path) {
        Ok(cfg) => {
            tracing::info!(
                path = %config_path,
                members = cfg.members.len(),
                inputs = cfg.inputs.len(),
                reaper_url = %cfg.reaper_url,
                "Config loaded successfully"
            );
            cfg
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %config_path,
                "Failed to load config, using defaults (controls will NOT work!)"
            );
            Config::default()
        }
    };

    // Warn if config is effectively empty
    if config.members.is_empty() {
        tracing::error!("No band members configured! Mixer controls will return 404.");
    }
    if config.inputs.is_empty() {
        tracing::error!("No input tracks configured! Mixer will show no channels.");
    }

    // Allow PORT env var to override config (useful for CI where port 80 requires root)
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(config.port);

    let config_dir = std::path::Path::new(&config_path)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    let server_config = ServerConfig {
        port,
        config,
        config_dir,
    };

    tracing::info!("Server listening on http://0.0.0.0:{}", port);
    iem_server::start_server(server_config, None).await?;

    Ok(())
}
