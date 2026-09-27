//! The iemmixer server (program spec §2.1; S5 design note): serves the web
//! UI, authenticates the band (PIN → JWT), and is the engine's controller —
//! an engine client over the control and media pipes, a mirror of the
//! engine's state behind the UI, Opus for Listen and Talkback, and the
//! server's own logic (permissions, Mute All, solo clean-up, presets,
//! history, backups, photos, push, SOS, tunnel health).

pub mod activity;
pub mod auth;
pub mod backup;
pub mod backup_daemon;
pub mod backup_routes;
pub mod backup_store;
pub mod band_import;
pub mod band_store;
pub mod console;
pub mod engine;
pub mod login_guard;
pub mod meters;
pub mod mixer_ws;
pub mod notify;
pub mod pepper;
pub mod photo_store;
pub mod pin_hash;
pub mod pin_store;
pub mod preset_routes;
pub mod provision;
pub mod push;
pub mod push_store;
pub mod routes;
pub mod secrets;
pub mod site_view;
pub mod snapshot_routes;
pub mod solo;
pub mod talk;
pub mod tunnel_watch;
pub mod view;

#[cfg(all(test, unix))]
mod engine_live_tests;
#[cfg(feature = "audio")]
pub mod listen_ws;
#[cfg(feature = "audio")]
pub mod talkback_buffer;
#[cfg(feature = "audio")]
pub mod talkback_ws;

use anyhow::Context as _;
use axum::Router;
use axum::http::{HeaderName, HeaderValue};
use iem_core::{Config, ServerMsg};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::{RwLock, broadcast};
use tower_http::set_header::SetResponseHeaderLayer;

use engine::EngineClient;
use engine::client::EngineError;
use iem_engine_proto::Cmd;
use meters::Merged;
use site_view::{Page, SiteView};

/// Write data to a file atomically by writing to a temp file then renaming.
/// Prevents corruption on crash/power failure.
pub fn atomic_write(path: &std::path::Path, data: &str) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, data)?;
    std::fs::rename(&tmp_path, path)
}

/// Where a UI event goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum To {
    /// Every mixer page.
    All,
    /// Every connection on this page.
    Page(String),
    /// Every connection with an engineer token, on any page.
    Engineers,
    /// One connection.
    Session(u64),
}

/// The guard's mode (program spec §4.1): the band-activity alarm runs in
/// `dev` only. `IEMMIXER_MODE` (S6's guard sets it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunMode {
    #[default]
    Dev,
    Live,
}

impl RunMode {
    /// `live` → Live; anything else (or nothing) → Dev.
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(str::trim) {
            Some("live") => Self::Live,
            _ => Self::Dev,
        }
    }
}

/// The site view built for one topology (keyed by its hash).
type SiteCacheEntry = (String, Arc<SiteView>);

/// Talkback runtime metrics (`GET /api/talkback/diagnostics`, F28).
#[cfg(feature = "audio")]
#[derive(Debug, Default)]
pub struct TalkbackMetrics {
    /// Opus frames received from the browser WebSocket
    pub packets_in: AtomicU64,
    /// Frames sent to the engine's talkback stream
    pub packets_out: AtomicU64,
    /// Reserved — WS inbound sequence gaps (the browser sends no sequence)
    pub seq_gaps: AtomicU64,
    /// Current jitter buffer fill in milliseconds
    pub buffer_fill_ms: std::sync::atomic::AtomicU32,
    /// Frames dropped because the buffer was full
    pub buffer_overflows: AtomicU64,
    /// Milliseconds since the most recent WS frame was received
    pub last_packet_age_ms: AtomicU64,
    /// Playout ticks concealed (no frame in time)
    pub underruns: AtomicU64,
    /// Opus bitrate of the browser's encoder
    pub bitrate_kbps: std::sync::atomic::AtomicU32,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    /// Application configuration
    pub config: Arc<RwLock<Config>>,
    /// The site as loaded at start (members and inputs never change at runtime)
    pub site_config: Arc<Config>,
    /// The directory of the site file (stores, secrets, alarm subscriptions)
    pub config_dir: Arc<std::path::PathBuf>,
    /// HTTP client (Web Push, public-IP detection)
    pub http_client: reqwest::Client,
    /// The engine's control pipe and the mirror
    pub engine: EngineClient,
    site_cache: Arc<Mutex<Option<SiteCacheEntry>>>,
    /// UI events that do not come from the engine (SOS, pins, talk, tunnel)
    pub event_tx: broadcast::Sender<(To, ServerMsg)>,
    /// Merged meters every 100 ms
    pub meters_tx: broadcast::Sender<Arc<Merged>>,
    /// Latest limiter active seconds per mix (topology order)
    pub active_s: Arc<Mutex<Vec<f64>>>,
    sessions: Arc<AtomicU64>,
    pub solo: Arc<Mutex<solo::SoloJanitor>>,
    pub talk: Arc<Mutex<talk::TalkLock>>,
    /// Active SOS alerts: member id → (member id, display name)
    pub alerts: Arc<Mutex<HashMap<String, (String, String)>>>,
    /// The band-activity alarm is on
    pub activity: Arc<AtomicBool>,
    pub mode: RunMode,
    /// Members whose daily auto-snapshot is taken: member → UTC day
    auto_snapshots: Arc<Mutex<HashMap<String, String>>>,
    /// Argon2id PIN hashes (`<config dir>/secrets/pin_hashes.json`)
    pub pin_store: Arc<RwLock<pin_store::PinStore>>,
    /// argon2id hasher keyed with the pepper (`<config dir>/secrets/`)
    pub pin_hasher: pin_hash::PinHasher,
    /// Login failure budgets (program spec §5.3)
    pub login_guard: Arc<login_guard::LoginGuard>,
    /// Bounded concurrency for argon2id work
    pub hash_gate: Arc<login_guard::HashGate>,
    /// Presets, history, pins and hides (band schema 3)
    pub band: Arc<band_store::BandStore>,
    /// Backup files
    pub backup_store: Arc<backup_store::BackupStore>,
    /// Profile photo storage (per-member JPEG files)
    pub photo_store: Arc<photo_store::PhotoStore>,
    /// Push subscription storage for Web Push notifications
    pub push_store: Arc<RwLock<push_store::PushStore>>,
    /// Cloudflare tunnel watchdog state
    pub tunnel_watch: Arc<RwLock<tunnel_watch::TunnelWatch>>,
    /// The engine's media pipe: Listen taps in, talkback out
    #[cfg(feature = "audio")]
    pub media: engine::media::MediaLink,
    /// Listen connections per mix (`StopListen` when the last one leaves)
    #[cfg(feature = "audio")]
    pub listeners: Arc<Mutex<HashMap<iem_engine_proto::MixId, usize>>>,
    #[cfg(feature = "audio")]
    pub talkback_metrics: Arc<TalkbackMetrics>,
}

impl AppState {
    /// Production constructor. Loads the PIN pepper and the PIN hashes from
    /// `<config dir>/secrets/`; an unreadable pepper or a corrupt or plaintext
    /// PIN store is an error — never regenerated, never ignored — so the server
    /// refuses to start. The engine and media links start detached;
    /// `start_server` connects them.
    pub fn try_new(
        config: Config,
        config_dir: &std::path::Path,
        mode: RunMode,
    ) -> std::io::Result<Self> {
        let secrets_dir = config_dir.join(secrets::SECRETS_DIR);
        let pepper = pepper::load_or_create(&secrets_dir)?;
        let pin_store = pin_store::PinStore::load(&secrets_dir)?;
        let (event_tx, _) = broadcast::channel(256);
        let (meters_tx, _) = broadcast::channel(16);
        Ok(Self {
            site_config: Arc::new(config.clone()),
            config_dir: Arc::new(config_dir.to_path_buf()),
            config: Arc::new(RwLock::new(config)),
            http_client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(2))
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .map_err(std::io::Error::other)?,
            engine: EngineClient::detached(),
            site_cache: Arc::new(Mutex::new(None)),
            event_tx,
            meters_tx,
            active_s: Arc::new(Mutex::new(Vec::new())),
            sessions: Arc::new(AtomicU64::new(1)),
            solo: Arc::new(Mutex::new(solo::SoloJanitor::default())),
            talk: Arc::new(Mutex::new(talk::TalkLock::default())),
            alerts: Arc::new(Mutex::new(HashMap::new())),
            activity: Arc::new(AtomicBool::new(false)),
            mode,
            auto_snapshots: Arc::new(Mutex::new(HashMap::new())),
            pin_store: Arc::new(RwLock::new(pin_store)),
            pin_hasher: pin_hash::PinHasher::new(pepper),
            login_guard: Arc::new(login_guard::LoginGuard::new()),
            hash_gate: Arc::new(login_guard::HashGate::new(
                login_guard::HASH_CONCURRENCY,
                login_guard::HASH_QUEUE,
            )),
            band: Arc::new(band_store::BandStore::new(config_dir)),
            backup_store: Arc::new(backup_store::BackupStore::new(config_dir)),
            photo_store: Arc::new(photo_store::PhotoStore::new(config_dir)),
            push_store: Arc::new(RwLock::new(push_store::PushStore::load(config_dir))),
            tunnel_watch: Arc::new(RwLock::new(tunnel_watch::TunnelWatch::new(
                std::time::Instant::now(),
            ))),
            #[cfg(feature = "audio")]
            media: engine::media::MediaLink::detached(),
            #[cfg(feature = "audio")]
            listeners: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(feature = "audio")]
            talkback_metrics: Arc::new(TalkbackMetrics::default()),
        })
    }

    /// Test constructor: panics where `try_new` returns an error.
    #[cfg(test)]
    pub fn new(config: Config, config_dir: &std::path::Path) -> Self {
        Self::try_new(config, config_dir, RunMode::Dev)
            .expect("test AppState: pepper and PIN store")
    }

    /// The site view over the engine's current topology (`None` until the
    /// engine announced one). Rebuilt when the topology hash changes.
    pub fn site(&self) -> Option<Arc<SiteView>> {
        let topo = self.engine.mirror().topology.clone()?;
        let mut cache = lock(&self.site_cache);
        if let Some((hash, view)) = cache.as_ref()
            && *hash == topo.hash
        {
            return Some(Arc::clone(view));
        }
        let (view, problems) = SiteView::build(&self.site_config, &topo);
        for p in &problems {
            tracing::error!(problem = %p, "site config and engine topology differ");
        }
        let view = Arc::new(view);
        *cache = Some((topo.hash.clone(), Arc::clone(&view)));
        Some(view)
    }

    /// The page `id`: from the site view, or (engine not seen yet) a
    /// configured member.
    pub fn page(&self, id: &str) -> Option<Page> {
        if let Some(view) = self.site() {
            return view.page(id);
        }
        self.site_config.member(id).map(|m| Page {
            id: m.id.clone(),
            name: m.name.clone(),
            mix: iem_engine_proto::MixId::new(m.mix.clone()),
            member: Some(m.id.clone()),
        })
    }

    /// A new WebSocket session id (the engine's `origin`).
    pub fn next_session(&self) -> u64 {
        self.sessions.fetch_add(1, Ordering::Relaxed)
    }

    pub fn broadcast(&self, to: To, msg: ServerMsg) {
        let _ = self.event_tx.send((to, msg));
    }

    /// The limiter active seconds of a mix from the latest meters.
    pub fn active_seconds(&self, mix: &iem_engine_proto::MixId) -> f64 {
        let Some(topo) = self.engine.mirror().topology.clone() else {
            return 0.0;
        };
        meters::active_seconds(&lock(&self.active_s), &topo, mix)
    }

    /// Takes the member's daily auto-snapshot (F14) if today has none: the
    /// mix as it is before the first change of the day.
    pub fn auto_snapshot(&self, page: &Page) {
        let Some(member) = page.member.as_deref() else {
            return;
        };
        let now = chrono::Utc::now().timestamp();
        let today = band_store::utc_day(now);
        if lock(&self.auto_snapshots).get(member) == Some(&today) {
            return;
        }
        match self.band.has_auto_snapshot_on(member, &today) {
            Ok(true) => {}
            Ok(false) => {
                let Some(view) = self.site() else {
                    return;
                };
                let c = band_store::capture(&view, &self.engine.mirror(), page);
                let snap = iem_core::band::Snapshot {
                    timestamp: now,
                    label: band_store::AUTO_LABEL.into(),
                    pinned: false,
                    sends: c.sends,
                    groups: c.groups,
                    input_eq: c.input_eq,
                    archived: false,
                    legacy_member: None,
                };
                if let Err(e) = self.band.add_snapshot(member, snap) {
                    tracing::error!(%member, error = %e, "daily auto-snapshot failed");
                    return;
                }
                tracing::info!(%member, "daily auto-snapshot taken");
            }
            Err(e) => {
                tracing::error!(%member, error = %e, "daily auto-snapshot check failed");
                return;
            }
        }
        lock(&self.auto_snapshots).insert(member.to_string(), today);
    }

    /// Sends commands that change `page`'s mix, after its daily snapshot.
    pub async fn apply(
        &self,
        page: &Page,
        cmds: Vec<Cmd>,
        origin: Option<u64>,
    ) -> Result<(), EngineError> {
        self.auto_snapshot(page);
        for cmd in cmds {
            self.engine.request_applied(cmd, origin).await?;
        }
        Ok(())
    }
}

/// Server configuration
pub struct ServerConfig {
    pub port: u16,
    pub config: Config,
    /// Directory where config and runtime data live (secrets/, stores, etc.)
    pub config_dir: std::path::PathBuf,
    pub mode: RunMode,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 80,
            config: Config::default(),
            config_dir: std::path::PathBuf::from("."),
            mode: RunMode::Dev,
        }
    }
}

/// Embedded WASM assets (built by Trunk)
#[derive(rust_embed::Embed)]
#[folder = "../iem-ui/dist/"]
pub struct Assets;

/// The HTTP application: API and static routes behind the security headers.
/// No CORS layer: the UI (browser, the tray's window, the E2E suite) is always
/// loaded from this server, so its requests are same-origin; a foreign page
/// gets no `Access-Control-Allow-Origin` and cannot read API responses.
fn app_router(state: AppState) -> Router {
    let x_frame_options = SetResponseHeaderLayer::overriding(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    let x_content_type_options = SetResponseHeaderLayer::overriding(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    let referrer_policy = SetResponseHeaderLayer::overriding(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    // CSP allows WASM + inline scripts (Trunk), inline styles (Leptos), and WebSocket connections
    let csp = SetResponseHeaderLayer::overriding(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:; img-src 'self' data:; font-src 'self'",
        ),
    );

    Router::new()
        .merge(routes::api_routes(state.clone()))
        .merge(routes::static_routes())
        .layer(x_frame_options)
        .layer(x_content_type_options)
        .layer(referrer_policy)
        .layer(csp)
        .with_state(state)
}

/// Start the server, optionally signaling readiness via a oneshot channel
pub async fn start_server(
    server_config: ServerConfig,
    ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
) -> anyhow::Result<()> {
    // Install rustls crypto provider (required when tls feature brings rustls into dep tree)
    #[cfg(feature = "tls")]
    {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    let mut config = server_config.config;
    let secrets = secrets::load_or_create(&server_config.config_dir.join(secrets::SECRETS_DIR))?;
    config.jwt_secret = secrets.jwt_secret;
    config.vapid_private_key = secrets.vapid_private_key;
    let pipe = config.engine_pipe.clone();
    let mut state = AppState::try_new(config, &server_config.config_dir, server_config.mode)
        .context("loading the PIN pepper and PIN hashes")?;
    state.engine = EngineClient::spawn(pipe.clone(), iem_core::VERSION.to_string());
    #[cfg(feature = "audio")]
    {
        state.media = engine::media::MediaLink::spawn(pipe.clone());
    }
    tracing::info!(pipe = %pipe, mode = ?state.mode, "engine client started");

    // Auto-detect public IP for LAN/WAN detection (if not configured)
    {
        let needs_detection = state.config.read().await.local_public_ip.is_none();
        if needs_detection {
            match detect_public_ip(&state.http_client).await {
                Some(ip) => {
                    tracing::info!(ip = %ip, "Auto-detected public IP for LAN/WAN detection");
                    state.config.write().await.local_public_ip = Some(ip);
                }
                None => {
                    tracing::warn!(
                        "Could not auto-detect public IP; LAN/WAN detection will use private IP fallback"
                    );
                }
            }
        }
    }

    console::spawn_tasks(state.clone());
    backup_daemon::spawn(state.clone());
    tunnel_watch::spawn_tunnel_watch(state.clone());

    let app = app_router(state.clone());

    // Spawn HTTPS server on port 443 (if TLS enabled and certs exist)
    #[cfg(feature = "tls")]
    {
        let config = state.config.read().await;
        if config.tls {
            let config_dir = dirs::config_dir().unwrap_or_default().join("iemmixer");
            let cert_path = config_dir.join(&config.tls_cert);
            let key_path = config_dir.join(&config.tls_key);
            let https_port = config.https_port;
            drop(config);

            if cert_path.exists() && key_path.exists() {
                match axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path)
                    .await
                {
                    Ok(rustls_config) => {
                        let https_addr = SocketAddr::from(([0, 0, 0, 0], https_port));
                        let https_app = app.clone();
                        tokio::spawn(async move {
                            tracing::info!(port = https_port, "HTTPS server listening");
                            if let Err(e) = axum_server::bind_rustls(https_addr, rustls_config)
                                .serve(
                                    https_app.into_make_service_with_connect_info::<SocketAddr>(),
                                )
                                .await
                            {
                                tracing::error!("HTTPS server failed: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Failed to load TLS certificates: {}", e);
                    }
                }
            } else {
                tracing::warn!("TLS enabled but cert files not found at {:?}", cert_path);
            }
        }
    }

    // Wrap HTTP app with HTTPS redirect middleware when TLS + domain configured
    #[cfg(feature = "tls")]
    let app = {
        let config = state.config.read().await;
        if config.tls {
            if let Some(ref domain) = config.https_domain {
                let domain = domain.clone();
                drop(config);
                app.layer(axum::middleware::from_fn(move |req, next| {
                    let domain = domain.clone();
                    https_redirect(req, next, domain)
                }))
            } else {
                drop(config);
                app
            }
        } else {
            drop(config);
            app
        }
    };

    // HTTP server (always runs)
    let addr = SocketAddr::from(([0, 0, 0, 0], server_config.port));
    tracing::info!("Starting server on http://{}", addr);

    // Set TCP_NODELAY on every accepted connection to reduce audio streaming latency.
    use axum::serve::ListenerExt;
    let listener = tokio::net::TcpListener::bind(addr)
        .await?
        .tap_io(|tcp_stream| {
            if let Err(e) = tcp_stream.set_nodelay(true) {
                tracing::trace!("failed to set TCP_NODELAY: {e:#}");
            }
        });

    // Signal readiness AFTER successful bind
    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// Redirect HTTP requests to HTTPS when the Host header matches the configured domain.
/// Requests via IP address (e.g., from Tauri desktop app) pass through unchanged.
/// Requests proxied through Cloudflare Tunnel (X-Forwarded-Proto: https) pass through unchanged.
#[cfg(feature = "tls")]
async fn https_redirect(
    req: axum::extract::Request,
    next: axum::middleware::Next,
    domain: String,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let forwarded_proto = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if forwarded_proto == "https" {
        return next.run(req).await;
    }

    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let host_name = host.split(':').next().unwrap_or("");
    if host_name == domain {
        let path = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let location = format!("https://{domain}{path}");
        axum::response::Redirect::permanent(&location).into_response()
    } else {
        next.run(req).await
    }
}

/// Auto-detect the server's public IP by querying an external service.
/// Used for LAN/WAN detection when `local_public_ip` is not configured.
async fn detect_public_ip(client: &reqwest::Client) -> Option<String> {
    let services = [
        "https://api.ipify.org",
        "https://ifconfig.me/ip",
        "https://icanhazip.com",
    ];
    for url in services {
        match client
            .get(url)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(ip) = resp.text().await {
                    let ip = ip.trim().to_string();
                    if !ip.is_empty() && ip.len() < 46 {
                        return Some(ip);
                    }
                }
            }
            _ => continue,
        }
    }
    None
}

/// Get the local network URL for remote access
pub fn get_remote_url(port: u16) -> String {
    let ip = local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "localhost".to_string());
    format!("http://{}:{}", ip, port)
}

#[cfg(all(test, feature = "tls"))]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        middleware,
        routing::get,
    };
    use tower::util::ServiceExt;

    async fn dummy_handler() -> &'static str {
        "OK"
    }

    fn create_test_app(domain: &str) -> Router {
        let domain = domain.to_string();
        Router::new()
            .route("/api/version", get(dummy_handler))
            .layer(middleware::from_fn(move |req, next| {
                let d = domain.clone();
                async move { https_redirect(req, next, d).await }
            }))
    }

    #[tokio::test]
    async fn test_redirect_without_forwarded_proto() {
        let app = create_test_app("mixer.example.org");
        let req = Request::builder()
            .uri("/api/version")
            .header("host", "mixer.example.org")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response.headers().get("location").unwrap(),
            "https://mixer.example.org/api/version"
        );
    }

    #[tokio::test]
    async fn test_no_redirect_with_forwarded_proto_https() {
        let app = create_test_app("mixer.example.org");
        let req = Request::builder()
            .uri("/api/version")
            .header("host", "mixer.example.org")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_redirect_with_forwarded_proto_http() {
        let app = create_test_app("mixer.example.org");
        let req = Request::builder()
            .uri("/api/version")
            .header("host", "mixer.example.org")
            .header("x-forwarded-proto", "http")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    }

    #[tokio::test]
    async fn test_no_redirect_for_different_host() {
        let app = create_test_app("mixer.example.org");
        let req = Request::builder()
            .uri("/api/version")
            .header("host", "10.0.0.10")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_redirect_preserves_path_and_query() {
        let app = create_test_app("mixer.example.org");
        let req = Request::builder()
            .uri("/api/mixer/1?token=abc123")
            .header("host", "mixer.example.org")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response.headers().get("location").unwrap(),
            "https://mixer.example.org/api/mixer/1?token=abc123"
        );
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    fn server_config(dir: &std::path::Path) -> ServerConfig {
        ServerConfig {
            port: 0,
            config: Config::default(),
            config_dir: dir.to_path_buf(),
            mode: RunMode::Dev,
        }
    }

    #[tokio::test]
    async fn start_refuses_a_plaintext_pin_store() {
        let dir = tempfile::tempdir().unwrap();
        let secrets_dir = dir.path().join(secrets::SECRETS_DIR);
        std::fs::create_dir_all(&secrets_dir).unwrap();
        std::fs::write(
            secrets_dir.join(pin_store::PIN_HASHES_FILE),
            r#"{"members":{"member1":"1357"}}"#,
        )
        .unwrap();
        let err = start_server(server_config(dir.path()), None)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("argon2id"), "{err:#}");
    }

    #[tokio::test]
    async fn start_refuses_a_missing_pepper_next_to_pin_hashes() {
        // A new pepper would silently void every stored PIN (spec P9).
        let dir = tempfile::tempdir().unwrap();
        let secrets_dir = dir.path().join(secrets::SECRETS_DIR);
        std::fs::create_dir_all(&secrets_dir).unwrap();
        std::fs::write(
            secrets_dir.join(pin_store::PIN_HASHES_FILE),
            r#"{"engineer":"$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNo"}"#,
        )
        .unwrap();
        let err = start_server(server_config(dir.path()), None)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("pepper") && msg.contains("missing"), "{msg}");
        assert!(
            !secrets_dir.join(pepper::PEPPER_FILE).exists(),
            "no new pepper"
        );
    }

    // Linux only: on Windows the pepper file is DPAPI data and the error text differs.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn start_refuses_a_corrupt_pepper() {
        let dir = tempfile::tempdir().unwrap();
        let secrets_dir = dir.path().join(secrets::SECRETS_DIR);
        std::fs::create_dir_all(&secrets_dir).unwrap();
        std::fs::write(secrets_dir.join(pepper::PEPPER_FILE), b"short").unwrap();
        let err = start_server(server_config(dir.path()), None)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("pepper"), "{err:#}");
        assert_eq!(
            std::fs::read(secrets_dir.join(pepper::PEPPER_FILE)).unwrap(),
            b"short",
            "never replaced"
        );
    }

    #[test]
    fn the_mode_is_live_only_when_asked() {
        assert_eq!(RunMode::parse(Some("live")), RunMode::Live);
        assert_eq!(RunMode::parse(Some(" live\n")), RunMode::Live);
        assert_eq!(RunMode::parse(Some("dev")), RunMode::Dev);
        assert_eq!(RunMode::parse(Some("LIVE")), RunMode::Dev);
        assert_eq!(RunMode::parse(None), RunMode::Dev);
        assert_eq!(RunMode::default(), RunMode::Dev);
        assert_eq!(ServerConfig::default().mode, RunMode::Dev);
    }

    #[tokio::test]
    async fn pages_resolve_from_the_config_until_the_engine_speaks() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(crate::site_view::tests::test_config(), dir.path());
        assert!(state.site().is_none());
        let p = state.page("member3").unwrap();
        assert_eq!(
            (p.mix.0.as_str(), p.member.as_deref()),
            ("member3", Some("member3"))
        );
        assert!(state.page("translator").is_none(), "needs the topology");
        assert!(state.page("ghost").is_none());
        assert_ne!(state.next_session(), state.next_session());
        assert_eq!(
            state.active_seconds(&iem_engine_proto::MixId::new("member1")),
            0.0
        );
    }
}

#[cfg(test)]
mod app_router_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::util::ServiceExt;

    const FOREIGN: &str = "https://evil.example";

    #[tokio::test]
    async fn a_foreign_origin_gets_no_cors_grant() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_router(AppState::new(Config::default(), dir.path()));

        let simple = app
            .clone()
            .oneshot(
                Request::get("/api/version")
                    .header(header::ORIGIN, FOREIGN)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            simple
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );

        let preflight = app
            .oneshot(
                Request::options("/api/auth")
                    .header(header::ORIGIN, FOREIGN)
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            preflight
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
        assert!(
            preflight
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_same_origin_request_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_router(AppState::new(Config::default(), dir.path()));
        let resp = app
            .oneshot(
                Request::get("/api/version")
                    .header(header::HOST, "10.0.0.10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["version"], iem_core::VERSION);
    }
}
