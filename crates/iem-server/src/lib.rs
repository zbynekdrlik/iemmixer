//! The iemmixer server (program spec §2.1; S5 design note): serves the web
//! UI, authenticates the band (PIN → JWT), and is the engine's controller —
//! an engine client over the control and media pipes, a mirror of the
//! engine's state behind the UI, Opus for Listen and Talkback, and the
//! server's own logic (permissions, Mute All, solo clean-up, presets,
//! history, backups, photos, push, SOS, tunnel health).

pub mod auth;
pub mod backup;
pub mod backup_daemon;
pub mod backup_routes;
pub mod backup_store;
pub mod band_import;
pub mod band_store;
pub mod console;
pub mod engine;
pub mod lineup_routes;
pub mod lineup_store;
pub mod lineup_switch;
pub mod login_guard;
pub mod meters;
pub mod mixer_ws;
pub mod notify;
pub mod peer_route;
pub mod people_removal;
pub mod pepper;
pub mod photo_store;
pub mod pin_hash;
pub mod pin_store;
pub mod preset_routes;
pub mod provision;
pub mod push;
pub mod push_store;
pub mod roster_state;
pub mod roster_watch;
pub mod routes;
pub mod secrets;
pub mod site_view;
pub mod snapshot_routes;
pub mod solo;
pub mod talk;
pub mod tunnel_watch;
pub mod view;
pub mod ws_alive;

// Split cfgs: cargo-mutants skips a module only under a plain `#[cfg(test)]`.
#[cfg(test)]
#[cfg(unix)]
mod engine_live_tests;
#[cfg(feature = "audio")]
pub mod listen_ws;
#[cfg(test)]
#[cfg(unix)]
mod routes_live_tests;
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
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// One connection.
    Session(u64),
}

/// What a site view is built from: the topology's hash and, in roster mode,
/// the roster's SHA-256 and the active lineup.
type SiteCacheKey = (String, Option<(String, String)>);

/// The site view built for one key.
type SiteCacheEntry = (SiteCacheKey, Arc<SiteView>);

/// The band store of each lineup (`None`: legacy mode, the config dir).
type BandStores = HashMap<Option<String>, Arc<band_store::BandStore>>;

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
    /// The roster and its active lineup (#58); `None` is legacy mode, the
    /// site's `[[members]]` as before. A std lock: read without awaiting.
    pub roster: Arc<std::sync::RwLock<Option<roster_state::RosterState>>>,
    /// The directory of the site file (stores, secrets, push subscriptions)
    pub config_dir: Arc<std::path::PathBuf>,
    /// HTTP client (Web Push, public-IP detection)
    pub http_client: reqwest::Client,
    /// The engine's control pipe and the mirror
    pub engine: EngineClient,
    site_cache: Arc<Mutex<Option<SiteCacheEntry>>>,
    /// UI events that do not come from the engine (SOS, pins, talk, tunnel)
    pub event_tx: broadcast::Sender<(To, ServerMsg)>,
    /// The active lineup after a switch or a roster change (#58):
    /// [`AppState::announce_lineup`]
    pub lineup_tx: broadcast::Sender<iem_core::LineupRef>,
    /// One lineup switch or roster reload at a time (#58 design §7).
    pub switching: Arc<tokio::sync::Mutex<()>>,
    /// Set while a switch runs (and, after a restart that cut one short,
    /// until it is finished): no page's change reaches the engine
    /// ([`AppState::may_change`]).
    pub frozen: Arc<std::sync::atomic::AtomicBool>,
    /// How long the replay of a switch and the settle of a roster wait
    /// between two tries (`lineup_switch::REPLAY_RETRY`; tests shorten it).
    pub retry: std::time::Duration,
    /// How long a roster's import whose answer was lost waits for an absent
    /// engine before the engineer's devices hear of it, once
    /// (`roster_watch::ABSENT_ALARM`; tests shorten it).
    pub absent_alarm: std::time::Duration,
    /// Reloads in a row whose pending record could not be written
    /// (`roster_watch::NOT_RECORDED`): the engineer's devices hear of it once
    /// the count reaches `roster_watch::MAX_SETTLES`; a reload that applies
    /// starts it again.
    pub(crate) unrecorded: Arc<std::sync::atomic::AtomicU32>,
    /// The server started on the roster applied last because `roster.toml`
    /// could not be used (`roster_watch::Loaded::fallback`), until a roster
    /// file is in use again: the status says so.
    pub fallback: Arc<std::sync::atomic::AtomicBool>,
    /// A page's change is checked and queued under the read side, `frozen`
    /// is set under the write side ([`AppState::apply`],
    /// [`AppState::freeze`]). A std lock: never held across an `.await`.
    change_gate: Arc<std::sync::RwLock<()>>,
    /// A person's page dropped an engine event while frozen
    /// ([`AppState::frozen_for_page`]): the next thaw announces the band
    /// even when nothing changed ([`AppState::thaw_unchanged`]).
    missed: Arc<std::sync::atomic::AtomicBool>,
    /// The roster in use has not reached the engine's state yet
    /// (`roster_watch::settle`): no page's change, switch or restore runs.
    unsettled: Arc<Mutex<Option<roster_watch::Unsettled>>>,
    /// Merged meters every 100 ms
    pub meters_tx: broadcast::Sender<Arc<Merged>>,
    /// Latest limiter active seconds per mix (topology order)
    pub active_s: Arc<Mutex<Vec<f64>>>,
    sessions: Arc<AtomicU64>,
    pub solo: Arc<Mutex<solo::SoloJanitor>>,
    pub talk: Arc<Mutex<talk::TalkLock>>,
    /// Active SOS alerts: member id → (member id, display name)
    pub alerts: Arc<Mutex<HashMap<String, (String, String)>>>,
    /// Members whose daily auto-snapshot is taken: [`snapshot_key`] → UTC day
    auto_snapshots: Arc<Mutex<HashMap<String, String>>>,
    /// Argon2id PIN hashes (`<config dir>/secrets/pin_hashes.json`)
    pub pin_store: Arc<RwLock<pin_store::PinStore>>,
    /// argon2id hasher keyed with the pepper (`<config dir>/secrets/`)
    pub pin_hasher: pin_hash::PinHasher,
    /// Login failure budgets (program spec §5.3)
    pub login_guard: Arc<login_guard::LoginGuard>,
    /// Bounded concurrency for argon2id work
    pub hash_gate: Arc<login_guard::HashGate>,
    /// Presets, history, pins and hides (band schema 3), one store per
    /// lineup: [`AppState::band`], [`AppState::band_of`]
    bands: Arc<Mutex<BandStores>>,
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
    /// refuses to start. So is a roster (`roster.toml`) that cannot be used
    /// (`roster_watch::load`, which migrates the legacy layout first); without
    /// one the server runs in legacy mode. The engine and media links start
    /// detached; `start_server` connects them.
    pub fn try_new(config: Config, config_dir: &std::path::Path) -> std::io::Result<Self> {
        let secrets_dir = config_dir.join(secrets::SECRETS_DIR);
        let pepper = pepper::load_or_create(&secrets_dir)?;
        let mut pin_store = pin_store::PinStore::load(&secrets_dir)?;
        // A roster is checked against the site's own `[engine]` table, as
        // `iem-server roster check` does (a site config built in code has
        // none, and one the engine could not read either is only logged:
        // no topology check until the engine speaks).
        let facts = config
            .engine
            .as_ref()
            .and_then(|t| match iem_core::roster::engine_facts(t) {
                Ok(facts) => Some(facts),
                Err(e) => {
                    tracing::error!(error = %e, "the site's [engine] table: a roster is checked once the engine speaks");
                    None
                }
            });
        let loaded = roster_watch::load(config_dir, &config, &mut pin_store, facts.as_ref())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let fallback = loaded.as_ref().is_some_and(|l| l.fallback);
        let roster = loaded.map(|l| l.roster);
        // A switch a restart cut short: no change until it is finished
        // (`lineup_switch::replay_when_synced`).
        let cut_short = roster.is_some()
            && lineup_store::LineupStore::new(config_dir)
                .intent()?
                .is_some();
        // The roster reaches the states once the engine has synced
        // (`roster_watch::settle_when_synced`): each state from the roster
        // its marker names (a kept one), one without a marker from the roster
        // applied last, also after a switch a restart cut short.
        let unsettled = roster
            .as_ref()
            .map(|_| roster_watch::Unsettled::new(roster_watch::applied_base(config_dir)));
        // The tunnel connector runs on this PC: CF-Connecting-IP counts only
        // from loopback and this host's own addresses (design note §6).
        let host = login_guard::HostAddrs::read();
        tracing::info!(
            ?host,
            "CF-Connecting-IP is trusted from loopback and these host addresses"
        );
        if config.activity.is_some() {
            tracing::info!(
                "site: the [activity] table is no longer read (the band-activity alarm was removed, #38); it can go"
            );
        }
        let (event_tx, _) = broadcast::channel(256);
        let (meters_tx, _) = broadcast::channel(16);
        let (lineup_tx, _) = broadcast::channel(16);
        Ok(Self {
            site_config: Arc::new(config.clone()),
            roster: Arc::new(std::sync::RwLock::new(roster)),
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
            lineup_tx,
            switching: Arc::new(tokio::sync::Mutex::new(())),
            frozen: Arc::new(std::sync::atomic::AtomicBool::new(cut_short)),
            retry: lineup_switch::REPLAY_RETRY,
            absent_alarm: roster_watch::ABSENT_ALARM,
            unrecorded: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            fallback: Arc::new(std::sync::atomic::AtomicBool::new(fallback)),
            change_gate: Arc::new(std::sync::RwLock::new(())),
            missed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            unsettled: Arc::new(Mutex::new(unsettled)),
            meters_tx,
            active_s: Arc::new(Mutex::new(Vec::new())),
            sessions: Arc::new(AtomicU64::new(1)),
            solo: Arc::new(Mutex::new(solo::SoloJanitor::default())),
            talk: Arc::new(Mutex::new(talk::TalkLock::default())),
            alerts: Arc::new(Mutex::new(HashMap::new())),
            auto_snapshots: Arc::new(Mutex::new(HashMap::new())),
            pin_store: Arc::new(RwLock::new(pin_store)),
            pin_hasher: pin_hash::PinHasher::new(pepper),
            login_guard: Arc::new(login_guard::LoginGuard::for_host(host)),
            hash_gate: Arc::new(login_guard::HashGate::new(
                login_guard::HASH_CONCURRENCY,
                login_guard::HASH_QUEUE,
            )),
            bands: Arc::new(Mutex::new(HashMap::new())),
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
        Self::try_new(config, config_dir).expect("test AppState: pepper and PIN store")
    }

    /// The presets, history and pins/hides store of the active lineup (the
    /// one the site view shows: `RosterState::lineup`), or in legacy mode
    /// the config directory's.
    pub fn band(&self) -> Arc<band_store::BandStore> {
        let roster = self.roster();
        self.band_of(roster.as_ref().map(|r| r.lineup().id.as_str()))
    }

    /// The store of `lineup`'s files (`lineups/<id>/`; `None`: the config
    /// directory, legacy mode). One store per lineup for the server's life,
    /// so every writer of a lineup's files shares its lock.
    pub fn band_of(&self, lineup: Option<&str>) -> Arc<band_store::BandStore> {
        let mut bands = lock(&self.bands);
        let store = bands.entry(lineup.map(str::to_string)).or_insert_with(|| {
            let dir = match lineup {
                Some(id) => lineup_store::LineupStore::new(&self.config_dir).data_dir(id),
                None => self.config_dir.to_path_buf(),
            };
            Arc::new(band_store::BandStore::new(&dir))
        });
        Arc::clone(store)
    }

    /// The roster in use (a copy; its roster is shared), `None` in legacy
    /// mode.
    pub fn roster(&self) -> Option<roster_state::RosterState> {
        self.roster
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Puts `roster` into use (`None`: legacy mode).
    pub fn set_roster(&self, roster: Option<roster_state::RosterState>) {
        *self.roster.write().unwrap_or_else(PoisonError::into_inner) = roster;
    }

    /// Tells every mixer page that the lineup or the roster changed (#58):
    /// the active lineup on [`AppState::lineup_tx`]. Nothing in legacy mode.
    pub fn announce_lineup(&self) {
        if let Some(r) = self.roster() {
            let l = r.lineup();
            let _ = self.lineup_tx.send(iem_core::LineupRef {
                id: l.id.clone(),
                name: l.name.clone(),
            });
        }
    }

    /// Whether a switch runs (or one a restart cut short is not finished).
    pub fn frozen_now(&self) -> bool {
        self.frozen.load(Ordering::SeqCst)
    }

    /// What the states follow while the roster in use has not reached them
    /// (`None`: it has, or legacy mode).
    pub fn unsettled(&self) -> Option<roster_watch::Unsettled> {
        lock(&self.unsettled).clone()
    }

    pub fn set_unsettled(&self, unsettled: Option<roster_watch::Unsettled>) {
        *lock(&self.unsettled) = unsettled;
    }

    /// Whether the pages wait for the roster in use to reach the engine's
    /// state (`roster_watch::Unsettled::refusing`).
    pub fn settling(&self) -> bool {
        lock(&self.unsettled).as_ref().is_some_and(|u| u.refusing)
    }

    /// Whether a change from `page` may reach the engine (or the page's
    /// pins and hides) now (#58 design §7): not while a switch runs or the
    /// roster in use has not reached the engine's state yet, and not from a
    /// page the active lineup no longer gives as it is (another band plays:
    /// the page's socket is closing, its change would land on another
    /// person's mix).
    pub fn may_change(&self, page: &Page) -> Result<(), view::ViewError> {
        if self.frozen_now() || self.settling() || self.page(&page.id).as_ref() != Some(page) {
            return Err(view::ViewError::Switching);
        }
        Ok(())
    }

    /// The site view over the engine's current topology (`None` until the
    /// engine announced one). Rebuilt when the topology hash, the roster or
    /// its active lineup changes.
    pub fn site(&self) -> Option<Arc<SiteView>> {
        let topo = self.engine.mirror().topology.clone()?;
        let roster = self.roster();
        let key: SiteCacheKey = (
            topo.hash.clone(),
            roster
                .as_ref()
                .map(|r| (r.sha256.clone(), r.active.clone())),
        );
        let mut cache = lock(&self.site_cache);
        if let Some((built_for, view)) = cache.as_ref()
            && *built_for == key
        {
            return Some(Arc::clone(view));
        }
        let (view, problems) = SiteView::build_with(&self.site_config, roster.as_ref(), &topo);
        for p in &problems {
            tracing::error!(problem = %p, "site config and engine topology differ");
        }
        let view = Arc::new(view);
        *cache = Some((key, Arc::clone(&view)));
        Some(view)
    }

    /// The page `id`: from the site view, or (engine not seen yet) a member
    /// as the site, or the roster's active lineup, places them.
    pub fn page(&self, id: &str) -> Option<Page> {
        if let Some(view) = self.site() {
            return view.page(id);
        }
        roster_state::site_members(&self.site_config, self.roster().as_ref())
            .into_iter()
            .find(|m| m.id == id)
            .map(|m| Page {
                id: m.id.clone(),
                name: m.name,
                mix: iem_engine_proto::MixId::new(m.mix),
                member: Some(m.id),
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
    /// mix as it is before the first change of the day. In roster mode
    /// each lineup's history is its own, so a person who plays in two
    /// bands the same day gets one in each (#58).
    pub fn auto_snapshot(&self, page: &Page) {
        let Some(member) = page.member.as_deref() else {
            return;
        };
        let lineup = self.roster().map(|r| r.lineup().id.clone());
        let band = self.band_of(lineup.as_deref());
        let key = snapshot_key(lineup.as_deref(), member);
        let now = chrono::Utc::now().timestamp();
        let today = band_store::utc_day(now);
        if lock(&self.auto_snapshots).get(&key) == Some(&today) {
            return;
        }
        match band.has_auto_snapshot_on(member, &today) {
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
                if let Err(e) = band.add_snapshot(member, snap) {
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
        lock(&self.auto_snapshots).insert(key, today);
    }

    /// Sends a page's commands to the engine and waits until the mirror
    /// holds each: refused while a switch runs or from a page whose band no
    /// longer plays ([`AppState::may_change`]); when they change the page's
    /// mix, its daily snapshot is taken first. The check, the snapshot and
    /// the queueing happen under the read side of the change gate, which a
    /// switch or a reload holds to freeze ([`AppState::freeze`]): no change
    /// slips in between its check and the freeze (#58).
    pub async fn apply(
        &self,
        page: &Page,
        cmds: Vec<Cmd>,
        origin: Option<u64>,
    ) -> Result<(), ChangeError> {
        let sent = {
            let _gate = self
                .change_gate
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            self.may_change(page)?;
            if cmds.iter().any(|c| mixer_ws::changes_mix(c, &page.mix)) {
                self.auto_snapshot(page);
            }
            cmds.into_iter()
                .map(|cmd| self.engine.send(cmd, origin))
                .collect::<Result<Vec<_>, _>>()?
        };
        for s in sent {
            s.applied().await?;
        }
        Ok(())
    }

    /// Freezes every page's changes for a switch or a reload: `frozen` set
    /// under the change gate's write side (no page is between its check and
    /// its queueing), then a `Ping` that comes back only after every change
    /// queued before it reached the mirror. Returns whether `frozen` was set
    /// already (a switch a restart cut short); a failed barrier puts it back
    /// as it was.
    pub async fn freeze(&self) -> Result<bool, EngineError> {
        let was = {
            let _gate = self
                .change_gate
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            self.frozen.swap(true, Ordering::SeqCst)
        };
        if let Err(e) = self.engine.request_applied(Cmd::Ping, None).await {
            if !was {
                self.thaw_unchanged();
            }
            return Err(e);
        }
        Ok(was)
    }

    /// Frees the pages' changes again after a change for them (an import,
    /// another band or roster; #58 re-review): a person's page heard nothing
    /// of the engine while they were frozen (`mixer_ws::closing`), so every
    /// page hears which band plays ([`AppState::announce_lineup`]).
    pub fn thaw(&self) {
        self.thaw_after(true);
    }

    /// Frees the pages' changes again after a try that changed nothing for
    /// them (no import went in): the band is announced, closing the people's
    /// sockets, only when a person's page dropped an engine event while
    /// frozen (#58 fifth review), so a try repeated every poll does not
    /// reconnect every phone.
    pub fn thaw_unchanged(&self) {
        self.thaw_after(false);
    }

    fn thaw_after(&self, changed: bool) {
        let (was, missed) = {
            let _gate = self
                .change_gate
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            (
                self.frozen.swap(false, Ordering::SeqCst),
                self.missed.swap(false, Ordering::SeqCst),
            )
        };
        if was && (changed || missed) {
            self.announce_lineup();
        }
    }

    /// Whether the pages are frozen, for a person's page about to drop an
    /// engine event (`mixer_ws::closing`): one dropped while frozen is noted
    /// for the thaw, under the change gate's read side, so no thaw falls
    /// between the look and the note.
    pub fn frozen_for_page(&self) -> bool {
        let _gate = self
            .change_gate
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let frozen = self.frozen_now();
        if frozen {
            self.missed.store(true, Ordering::SeqCst);
        }
        frozen
    }
}

/// Why a page's commands did not reach the engine ([`AppState::apply`]).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ChangeError {
    /// [`AppState::may_change`] refused: a switch runs, or the page's band
    /// no longer plays.
    #[error(transparent)]
    Refused(#[from] view::ViewError),
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// What [`AppState::auto_snapshot`] records a day's snapshot under: the
/// member in legacy mode, `<lineup>/<member>` in roster mode.
fn snapshot_key(lineup: Option<&str>, member: &str) -> String {
    match lineup {
        Some(l) => format!("{l}/{member}"),
        None => member.to_string(),
    }
}

/// Server configuration
pub struct ServerConfig {
    pub port: u16,
    pub config: Config,
    /// Directory where config and runtime data live (secrets/, stores, etc.)
    pub config_dir: std::path::PathBuf,
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
    // Framing (fohmixer's surface, #91): CSP `frame-ancestors` from the site
    // list; with no list it is `'none'` and the legacy X-Frame-Options DENY
    // stays for browsers without CSP 2. A listed parent must not also get
    // DENY, which would refuse it in browsers that read only that header.
    let frame_ancestors = state
        .config
        .try_read()
        .map(|c| c.frame_ancestors_source())
        .unwrap_or_else(|_| "'none'".to_string());
    let x_frame_options = (frame_ancestors == "'none'").then(|| {
        SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        )
    });
    let x_content_type_options = SetResponseHeaderLayer::overriding(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    let referrer_policy = SetResponseHeaderLayer::overriding(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    // CSP allows WASM + inline scripts (Trunk), inline styles (Leptos), and WebSocket connections
    let csp_value = format!(
        "default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:; img-src 'self' data:; font-src 'self'; frame-ancestors {frame_ancestors}"
    );
    let csp = SetResponseHeaderLayer::overriding(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_str(&csp_value).expect("site validation keeps the CSP a valid header"),
    );

    let mut router = Router::new()
        .merge(routes::api_routes(state.clone()))
        .merge(routes::static_routes());
    if let Some(deny) = x_frame_options {
        router = router.layer(deny);
    }
    router
        .layer(x_content_type_options)
        .layer(referrer_policy)
        .layer(csp)
        .with_state(state)
}

/// How long a stop waits for open requests before the server returns anyway.
pub const STOP_DRAIN: std::time::Duration = std::time::Duration::from_secs(5);

/// Start the server, optionally signaling readiness via a oneshot channel;
/// it runs until the process ends.
pub async fn start_server(
    server_config: ServerConfig,
    ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
) -> anyhow::Result<()> {
    start_server_until(server_config, ready_tx, std::future::pending()).await
}

/// [`start_server`] until `stop` resolves (S6 graceful stop): then the HTTP
/// and (with `tls`) the HTTPS listener close at once (the ports are free),
/// idle connections close, open requests on either get up to [`STOP_DRAIN`]
/// from the stop to finish, and it returns `Ok`. The caller's runtime still
/// runs the background tasks (engine client, backup daemon, tunnel
/// watchdog); shutting the runtime down ends them.
pub async fn start_server_until<F>(
    server_config: ServerConfig,
    ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
    stop: F,
) -> anyhow::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
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
    let mut state = AppState::try_new(config, &server_config.config_dir)
        .context("loading the PIN pepper, the PIN hashes and the roster")?;
    state.engine = EngineClient::spawn(pipe.clone(), iem_core::VERSION.to_string());
    #[cfg(feature = "audio")]
    {
        state.media = engine::media::MediaLink::spawn(pipe.clone());
    }
    tracing::info!(pipe = %pipe, "engine client started");

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
    tokio::spawn(roster_watch::watch(state.clone()));
    {
        // A switch a restart cut short first, then the roster into the
        // states (each once the engine has synced).
        let s = state.clone();
        tokio::spawn(async move {
            let replayed = lineup_switch::replay_when_synced(s.clone()).await;
            tracing::info!(
                ?replayed,
                "the start's replay of an unfinished switch is done"
            );
            roster_watch::settle_when_synced(s).await;
        });
        // A start on the roster applied last: the engineer hears of it.
        let s = state.clone();
        tokio::spawn(async move {
            roster_watch::fallback_alarm(&s).await;
        });
    }

    let app = app_router(state.clone());

    // Spawn HTTPS server on port 443 (if TLS enabled and certs exist). Its
    // handle takes the same graceful stop as the HTTP server below.
    #[cfg(feature = "tls")]
    let mut https: Option<(axum_server::Handle<SocketAddr>, tokio::task::JoinHandle<()>)> = None;
    #[cfg(feature = "tls")]
    {
        let config = state.config.read().await;
        if config.tls {
            // Next to the site file: `iem-migrate band` writes them into the
            // server's config directory (S6 design note §6).
            let cert_path = server_config.config_dir.join(&config.tls_cert);
            let key_path = server_config.config_dir.join(&config.tls_key);
            let https_port = config.https_port;
            drop(config);

            if cert_path.exists() && key_path.exists() {
                match axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path)
                    .await
                {
                    Ok(rustls_config) => {
                        let https_addr = SocketAddr::from(([0, 0, 0, 0], https_port));
                        let https_app = app.clone();
                        let handle = axum_server::Handle::new();
                        let server_handle = handle.clone();
                        let task = tokio::spawn(async move {
                            tracing::info!(port = https_port, "HTTPS server listening");
                            if let Err(e) = axum_server::bind_rustls(https_addr, rustls_config)
                                .handle(server_handle)
                                .serve(
                                    https_app.into_make_service_with_connect_info::<SocketAddr>(),
                                )
                                .await
                            {
                                tracing::error!("HTTPS server failed: {}", e);
                            }
                        });
                        https = Some((handle, task));
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

    // The stop closes both listeners at once; open requests on either get
    // STOP_DRAIN.
    let stopping = Arc::new(tokio::sync::Notify::new());
    let stop_seen = Arc::clone(&stopping);
    #[cfg(feature = "tls")]
    let https_handle = https.as_ref().map(|(handle, _)| handle.clone());
    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        stop.await;
        tracing::info!("stop requested: the listeners close, open requests get up to 5 s");
        #[cfg(feature = "tls")]
        {
            if let Some(handle) = https_handle {
                handle.graceful_shutdown(Some(STOP_DRAIN));
            }
        }
        stop_seen.notify_one();
    });
    tokio::select! {
        result = serve => result?,
        () = async {
            stopping.notified().await;
            tokio::time::sleep(STOP_DRAIN).await;
        } => tracing::warn!("HTTP requests still open 5 s after the stop: stopping without them"),
    }
    tracing::info!("HTTP server stopped");
    #[cfg(feature = "tls")]
    {
        if let Some((_, task)) = https {
            // It took the stop with the HTTP server, and axum-server ends
            // its connections STOP_DRAIN after it; this bound is only the
            // backstop.
            match tokio::time::timeout(STOP_DRAIN, task).await {
                Ok(Ok(())) => tracing::info!("HTTPS server stopped"),
                Ok(Err(e)) => tracing::error!(error = %e, "HTTPS server task failed"),
                Err(_) => tracing::warn!(
                    "HTTPS requests still open 5 s after the stop: stopping without them"
                ),
            }
        }
    }
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
mod startup_tests;

#[cfg(test)]
mod app_router_tests;

#[cfg(test)]
mod auto_snapshot_tests;
