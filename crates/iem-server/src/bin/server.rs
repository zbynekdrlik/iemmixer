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

    // Site config (TOML). A missing or invalid file stops the server.
    let config_path =
        std::env::var("IEMMIXER_CONFIG").unwrap_or_else(|_| "iemmixer.toml".to_string());
    let config = Config::load(&config_path)
        .map_err(|e| anyhow::anyhow!("loading site config {config_path}: {e}"))?;
    tracing::info!(
        path = %config_path,
        members = config.members.len(),
        inputs = config.inputs.len(),
        "site config loaded"
    );

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
