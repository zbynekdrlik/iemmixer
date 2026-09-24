//! IEM Mixer Server
//!
//! Axum-based API server that:
//! - Serves embedded WASM frontend assets
//! - Provides authentication (PIN → JWT)
//! - Proxies requests to REAPER HTTP API
//! - Provides real-time WebSocket updates

pub mod auth;
pub mod backup_capture;
pub mod backup_daemon;
pub mod backup_restore;
pub mod backup_routes;
pub mod backup_store;
pub mod customization_store;
pub mod login_guard;
pub mod pepper;
pub mod photo_store;
pub mod pin_hash;
pub mod pin_store;
pub mod poller;
pub mod preset_routes;
pub mod preset_store;
pub mod provision;
pub mod proxy;
pub mod push;
pub mod push_store;
pub mod routes;
pub mod secrets;
pub mod snapshot_routes;
pub mod snapshot_store;
pub mod tunnel_watch;

#[cfg(feature = "audio")]
pub mod audio_stream;
#[cfg(feature = "audio")]
pub mod talkback_buffer;

#[cfg(feature = "test-helpers")]
pub mod test_helpers;

use anyhow::Context as _;
use axum::Router;
use axum::http::{HeaderName, HeaderValue};
use iem_core::{Config, DiscoveredMember, ServerMsg};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;

/// Listen mode target tracking.
/// Tracks saved mute states during band member listen for restoration.
#[derive(Debug, Clone, Default)]
pub enum ListenTarget {
    /// Not listening — no mute overrides active
    #[default]
    Idle,
    /// Listening on band member page — mute states saved for restoration.
    /// Contains: Vec<(track_index, send_index, was_muted_before)>
    Member(Vec<(usize, usize, bool)>),
}

/// Talkback lock state — only one engineer can talk at a time (reaperiem#123)
#[derive(Debug, Clone, Default)]
pub struct TalkbackState {
    /// Engineer ID holding the talkback lock (None = idle)
    pub active_talker: Option<String>,
    /// UDP address of the OIEM Receive VST3 (learned from heartbeat packets)
    pub recv_vst_addr: Option<std::net::SocketAddr>,
}

/// Talkback runtime metrics for reaperiem#154 diagnostics API.
#[cfg(feature = "audio")]
#[derive(Debug, Default)]
pub struct TalkbackMetrics {
    /// Opus frames received from the browser WebSocket
    pub packets_in: std::sync::atomic::AtomicU64,
    /// OIEM UDP packets sent to the Receive VST (drain-loop pops)
    pub packets_out: std::sync::atomic::AtomicU64,
    /// Reserved — WS inbound sequence gaps (not yet tracked; browser does not send seq)
    pub seq_gaps: std::sync::atomic::AtomicU64,
    /// Current jitter buffer fill in milliseconds
    pub buffer_fill_ms: std::sync::atomic::AtomicU32,
    /// Total drop-oldest events in the jitter buffer
    pub buffer_overflows: std::sync::atomic::AtomicU64,
    /// Milliseconds since the most recent WS frame was received
    pub last_packet_age_ms: std::sync::atomic::AtomicU64,
    /// Drain-loop ticks that found the buffer empty (emitted keepalive only)
    pub underruns: std::sync::atomic::AtomicU64,
    /// Negotiated Opus bitrate (set on session start from client config)
    pub bitrate_kbps: std::sync::atomic::AtomicU32,
}

/// Write data to a file atomically by writing to a temp file then renaming.
/// Prevents corruption on crash/power failure.
pub fn atomic_write(path: &std::path::Path, data: &str) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, data)?;
    std::fs::rename(&tmp_path, path)
}

#[cfg(feature = "audio")]
use std::sync::Mutex;

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    /// Application configuration
    pub config: Arc<RwLock<Config>>,
    /// HTTP client for REAPER proxy
    pub http_client: reqwest::Client,
    /// Broadcast channel for WebSocket state updates (member_id, event)
    pub event_tx: broadcast::Sender<(String, ServerMsg)>,
    /// Cache of last-known state per member (for diff detection)
    pub mixer_cache: Arc<RwLock<MixerCache>>,
    /// Argon2id PIN hashes (`<config dir>/secrets/pin_hashes.json`)
    pub pin_store: Arc<RwLock<pin_store::PinStore>>,
    /// argon2id hasher keyed with the pepper (`<config dir>/secrets/`)
    pub pin_hasher: pin_hash::PinHasher,
    /// Login failure budgets (program spec §5.3)
    pub login_guard: Arc<login_guard::LoginGuard>,
    /// Bounded concurrency for argon2id work
    pub hash_gate: Arc<login_guard::HashGate>,
    /// Snapshot storage for mix history
    pub snapshot_store: Arc<snapshot_store::SnapshotStore>,
    /// Backup file store (full system backups as JSON files)
    pub backup_store: Arc<backup_store::BackupStore>,
    /// Preset storage for saved mix configurations
    pub preset_store: Arc<preset_store::PresetStore>,
    /// Channel customization storage (pin/hide preferences)
    pub customization_store: Arc<customization_store::CustomizationStore>,
    /// Profile photo storage (per-member JPEG files)
    pub photo_store: Arc<photo_store::PhotoStore>,
    /// Push subscription storage for Web Push notifications (reaperiem#133)
    pub push_store: Arc<RwLock<push_store::PushStore>>,
    /// Band members discovered from REAPER (source of truth)
    pub discovered_members: Arc<RwLock<Vec<DiscoveredMember>>>,
    /// Listen mode target (Idle / Member).
    /// Tracks saved mute states during band member listen for restoration.
    pub engineer_listen_target: Arc<RwLock<ListenTarget>>,
    /// Broadcast channel for audio Opus frames (engineer listening)
    #[cfg(feature = "audio")]
    pub audio_tx: broadcast::Sender<bytes::Bytes>,
    /// Audio pipeline health diagnostics
    #[cfg(feature = "audio")]
    pub audio_diagnostics: Arc<Mutex<audio_stream::AudioDiagnostics>>,
    /// Talkback lock state (one-at-a-time engineer talk) (reaperiem#123)
    #[cfg(feature = "audio")]
    pub talkback_state: Arc<RwLock<TalkbackState>>,
    /// UDP socket for sending talkback OIEM packets to receive VST (reaperiem#123)
    #[cfg(feature = "audio")]
    pub talkback_socket: Arc<tokio::net::UdpSocket>,
    /// Talkback runtime metrics for diagnostics (reaperiem#154)
    #[cfg(feature = "audio")]
    pub talkback_metrics: Arc<TalkbackMetrics>,
    /// Mutex to serialize EQ EXTSTATE writes (prevents race condition
    /// where concurrent tokio tasks overwrite the shared EXTSTATE key)
    pub eq_write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Mutex to serialize concurrent EQ reads against each other.
    /// The read path (set eq_read_track → trigger script → read eq_params)
    /// uses a shared EXTSTATE slot, so concurrent reads would clobber.
    /// Note: this does NOT protect reads vs writes (they use different keys).
    pub eq_read_lock: Arc<tokio::sync::Mutex<()>>,
    /// Mutex to serialize limiter EXTSTATE writes (reaperiem#72)
    pub limiter_write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Mutex to serialize limiter EXTSTATE reads (reaperiem#72)
    pub limiter_read_lock: Arc<tokio::sync::Mutex<()>>,
    /// Per-inear-track cumulative active milliseconds from the limiter
    /// (reaperiem#145). Populated by the background poller from EXTSTATE
    /// REAPERIEM_LIMITER_ACTIVITY/totals; zeroed entry-by-entry via the
    /// ClientMsg::ResetLimiterActivity handler.
    pub limiter_activity: Arc<tokio::sync::Mutex<std::collections::HashMap<usize, u64>>>,
    /// Cloudflare tunnel watchdog state (reaperiem#202). Advanced by
    /// `tunnel_watch::spawn_tunnel_watch`, read by `/api/tunnel` and on WS connect.
    pub tunnel_watch: Arc<RwLock<tunnel_watch::TunnelWatch>>,
}

/// Global IEM output volume state for a member
#[derive(Debug, Clone)]
pub struct GlobalVolState {
    pub level_db: f32,
    pub muted: bool,
}

/// Cached mixer state for change detection
#[derive(Default)]
pub struct MixerCache {
    /// Last known channel states per member (member_id -> channels)
    pub member_states: HashMap<String, Vec<iem_core::Channel>>,
    /// Last known meter values (track_index -> [left, right] peak_linear)
    pub meters: HashMap<usize, [f32; 2]>,
    /// Whether REAPER is currently reachable
    pub connected: bool,
    /// Members with active WebSocket connections (member_id -> connection count)
    pub active_members: HashMap<String, usize>,
    /// Timestamps of recent commands, keyed by (member_id, track_index).
    /// Used by the poller to suppress echo broadcasts for recently-commanded channels.
    pub command_timestamps: HashMap<(String, usize), std::time::Instant>,
    /// Last known global IEM output volume per member (member_id -> state)
    pub global_volumes: HashMap<String, GlobalVolState>,
    /// Output track indices per member (member_id -> 1-based track index)
    pub output_track_indices: HashMap<String, usize>,
    /// Input track indices resolved by name from REAPER (track_name -> 1-based track index)
    pub input_track_indices: HashMap<String, usize>,
    /// Authoritative set of REAPER track indices that represent valid input
    /// tracks. Precomputed by the poller whenever `input_track_indices` or
    /// `config.inputs` changes (see `poller::recompute_valid_input_indices`),
    /// so per-command handlers can validate in O(1) without rebuilding the
    /// set. Mirrors what `build_channel_templates` sends to clients — see
    /// reaperiem#179 for why `inputs.len()` cannot substitute.
    pub valid_input_track_indices: std::collections::HashSet<usize>,
    /// Last known REAPER track count (for change detection)
    pub last_track_count: Option<usize>,
    /// Date of last auto-snapshot per member (member_id -> "YYYY-MM-DD")
    /// Used to ensure only one auto-snapshot per day per member
    pub snapshot_last_date: HashMap<String, String>,
    /// Solo state per member — transient, in-memory only (member_id -> soloed track indices)
    pub solo_states: HashMap<String, Vec<usize>>,
    /// Pre-solo mute states per member — saved when solo activates, restored on unsolo
    /// (member_id -> (track_index, send_index, was_muted))
    pub pre_solo_mutes: HashMap<String, Vec<(usize, usize, bool)>>,
    /// Stems-bus track indices per member (member_id -> 1-based track index)
    pub stems_bus_indices: HashMap<String, usize>,
    /// Last known stems-bus volume per member (member_id -> state)
    pub stems_volumes: HashMap<String, GlobalVolState>,
    /// Active SOS alerts (member_id -> (from_member, from_name)). Persists until cleared.
    pub active_alerts: HashMap<String, (String, String)>,
}

impl MixerCache {
    pub fn new() -> Self {
        Self::default()
    }
}

impl AppState {
    /// Production constructor. Loads the PIN pepper and the PIN hashes from
    /// `<config dir>/secrets/`; an unreadable pepper or a corrupt or plaintext
    /// PIN store is an error — never regenerated, never ignored — so the server
    /// refuses to start.
    pub fn try_new(config: Config, config_dir: &std::path::Path) -> std::io::Result<Self> {
        let secrets_dir = config_dir.join(secrets::SECRETS_DIR);
        let pepper = pepper::load_or_create(&secrets_dir)?;
        let pin_store = pin_store::PinStore::load(&secrets_dir)?;
        let (event_tx, _) = broadcast::channel(256);
        #[cfg(feature = "audio")]
        // Capacity 64 (was 8): a shallow channel caused stale state under
        // sustained uptime. At 50 pps, 8 slots = only 160 ms slack —
        // Windows scheduling jitter can stall the producer beyond that and
        // trap new subscribers in persistent Lagged. 64 gives 1.28 s slack.
        let (audio_tx, _) = broadcast::channel(64);
        // Seed the validator's index set with the position-fallback view
        // (i+1 for each input) so commands arriving BEFORE the first
        // successful poller tick are validated against the same set of
        // indices that build_channel_templates would send to the client.
        // The poller overwrites this with the real REAPER-resolved set on
        // its first cycle (see poller.rs `input_track_indices` update).
        let initial_valid = crate::proxy::collect_valid_input_indices(
            &config.inputs,
            &std::collections::HashMap::new(),
        );
        let mut initial_cache = MixerCache::new();
        initial_cache.valid_input_track_indices = initial_valid;
        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            http_client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(2))
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("failed to build HTTP client"),
            event_tx,
            mixer_cache: Arc::new(RwLock::new(initial_cache)),
            pin_store: Arc::new(RwLock::new(pin_store)),
            pin_hasher: pin_hash::PinHasher::new(pepper),
            login_guard: Arc::new(login_guard::LoginGuard::new()),
            hash_gate: Arc::new(login_guard::HashGate::new(
                login_guard::HASH_CONCURRENCY,
                login_guard::HASH_QUEUE,
            )),
            snapshot_store: Arc::new(snapshot_store::SnapshotStore::new(config_dir)),
            backup_store: Arc::new(backup_store::BackupStore::new(config_dir)),
            preset_store: Arc::new(preset_store::PresetStore::new(config_dir)),
            customization_store: Arc::new(customization_store::CustomizationStore::new(config_dir)),
            photo_store: Arc::new(photo_store::PhotoStore::new(config_dir)),
            push_store: Arc::new(RwLock::new(push_store::PushStore::load(config_dir))),
            discovered_members: Arc::new(RwLock::new(Vec::new())),
            engineer_listen_target: Arc::new(RwLock::new(ListenTarget::Idle)),
            #[cfg(feature = "audio")]
            audio_tx,
            #[cfg(feature = "audio")]
            audio_diagnostics: Arc::new(Mutex::new(audio_stream::AudioDiagnostics::default())),
            #[cfg(feature = "audio")]
            talkback_state: Arc::new(RwLock::new(TalkbackState::default())),
            #[cfg(feature = "audio")]
            talkback_socket: {
                let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind talkback UDP");
                sock.set_nonblocking(true).expect("set nonblocking");
                Arc::new(tokio::net::UdpSocket::from_std(sock).expect("tokio UdpSocket"))
            },
            #[cfg(feature = "audio")]
            talkback_metrics: Arc::new(TalkbackMetrics::default()),
            eq_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            eq_read_lock: Arc::new(tokio::sync::Mutex::new(())),
            limiter_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            limiter_read_lock: Arc::new(tokio::sync::Mutex::new(())),
            limiter_activity: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            tunnel_watch: Arc::new(RwLock::new(tunnel_watch::TunnelWatch::new(
                std::time::Instant::now(),
            ))),
        })
    }

    /// Test constructor: panics where `try_new` returns an error.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn new(config: Config, config_dir: &std::path::Path) -> Self {
        Self::try_new(config, config_dir).expect("test AppState: pepper and PIN store")
    }

    /// Construct a minimal `AppState` for testing purposes.
    ///
    /// Builds a fully-valid `AppState` with:
    /// - `reaper_url` pointing at the given URL (typically a closed port)
    /// - All stores backed by `data_dir` (a temp directory in tests)
    /// - No background daemons spawned
    ///
    /// Only compiled under `#[cfg(feature = "test-helpers")]`.
    #[cfg(feature = "test-helpers")]
    pub fn new_for_test(reaper_url: String, data_dir: std::path::PathBuf) -> Self {
        let config = iem_core::Config {
            reaper_url,
            ..Default::default()
        };
        Self::new(config, &data_dir)
    }
}

/// Server configuration
pub struct ServerConfig {
    pub port: u16,
    pub config: Config,
    /// Directory where config and runtime data live (secrets/, stores, etc.)
    pub config_dir: std::path::PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 80,
            config: Config::default(),
            config_dir: std::path::PathBuf::from("."),
        }
    }
}

/// Embedded WASM assets (built by Trunk)
#[derive(rust_embed::Embed)]
#[folder = "../iem-ui/dist/"]
pub struct Assets;

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
    let state = AppState::try_new(config, &server_config.config_dir)
        .context("loading the PIN pepper and PIN hashes")?;

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

    // Discover members from REAPER (source of truth)
    let members = poller::discover_members(&state).await;
    {
        let mut discovered = state.discovered_members.write().await;
        *discovered = members;
    }
    let discovered_count = state.discovered_members.read().await.len();
    tracing::info!(count = discovered_count, "Members discovered from REAPER");

    // Spawn background REAPER poller
    poller::spawn_poller(state.clone());

    // Spawn backup daemon (scheduled captures)
    backup_daemon::spawn(state.clone());

    // Spawn Cloudflare tunnel watchdog (reaperiem#202): polls cloudflared /ready,
    // restarts the service when the tunnel is down, broadcasts status.
    tunnel_watch::spawn_tunnel_watch(state.clone());

    // Spawn audio listener (receives OIEM Opus packets from VST plugin)
    #[cfg(feature = "audio")]
    audio_stream::spawn_audio_listener(
        state.audio_tx.clone(),
        state.audio_diagnostics.clone(),
        state.talkback_state.clone(),
    );

    let cors = CorsLayer::permissive();

    // Security headers to prevent common attacks
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

    let app = Router::new()
        .merge(routes::api_routes(state.clone()))
        .merge(routes::static_routes())
        .layer(cors)
        .layer(x_frame_options)
        .layer(x_content_type_options)
        .layer(referrer_policy)
        .layer(csp)
        .with_state(state.clone());

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
    // Disables Nagle's algorithm so Opus frames are sent immediately without waiting
    // to coalesce with subsequent data. Critical for real-time audio over WebSocket.
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

    // Skip redirect if already coming through HTTPS (via Cloudflare Tunnel)
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
    // Strip port from host header for comparison
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
    // Try multiple services for reliability
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
        // Direct HTTP request to mixer.example.org should redirect to HTTPS
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
        // Request via Cloudflare Tunnel (X-Forwarded-Proto: https) should NOT redirect
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
        // Request with X-Forwarded-Proto: http should still redirect
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
        // Request via IP address should pass through unchanged
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

    // cargo-mutants 27.1 does not apply `exclude_re` to struct-field deletions,
    // so the `new_for_test` exclude does not cover them: pin the field here.
    #[cfg(feature = "test-helpers")]
    #[tokio::test]
    async fn new_for_test_targets_the_given_reaper_url() {
        let dir = tempfile::tempdir().unwrap();
        let state =
            AppState::new_for_test("http://127.0.0.1:9".to_string(), dir.path().to_path_buf());
        assert_eq!(state.config.read().await.reaper_url, "http://127.0.0.1:9");
    }
}
