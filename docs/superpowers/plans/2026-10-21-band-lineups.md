# Band lineups and band switching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** People separate from equipment, several bands ("lineups") that assign people to sets and inputs, a click-free band switch by a leader or the engineer, leaders who hear each other's mixes, and roster changes without a maintenance session.

**Architecture:** The engine gains `peers`: a mix may hear another mix's own output regardless of declaration order, computed by a shadow chain for a mix that both hears and is heard. The server gains a roster (`roster.toml`, private, beside the site file) holding people and lineups; without it the server runs exactly as today (legacy mode). Each lineup keeps its mixer state and its people's presets, history and pins/hides under `lineups/<id>/`; a switch captures the live state, imports the target's in one `ImportState` and records the active lineup through a crash-safe intent file. The UI reads `/api/lineup` and follows `LineupChanged`.

**Tech Stack:** Rust (engine, axum server, Leptos UI), Playwright E2E, Python `iempc` on the dev box, PowerShell on the PC.

**Spec:** `docs/superpowers/specs/2026-10-21-band-lineups-design.md` (approved on #58, 2026-10-21).

## Global Constraints

- Tier 0: no local cargo compilation. Local: `cargo fmt --all --check`, `cargo metadata`, `cargo tree`, `cargo update -p`, Python unittests, `python3 scripts/denylist_scan.py …`. Every Rust step's "run" is the `dev` push's CI job named in the step.
- P6: no real names, hosts, IPs, Dante device names or channel numbers in this repo, its tests, CI logs or tickets. Tests use `config/test-site.toml`'s ids (`member1`…`member9`, `engineer`, `translator`, `mic1`…) and the new synthetic `member10`, `member11`, lineups `main`, `youth`.
- RT contract at B = 32 (`.claude/rules/engine.md`): `process` allocates, locks, logs and makes syscalls never; `tests/rt.rs`, `tests/rtsan.rs`, `examples/bench.rs` cover every new per-block path.
- Engine protocol and state are additive: no `deny_unknown_fields` in `iem-engine-proto`, every new field `#[serde(default)]`; a new `MixInfo` field is `skip_serializing_if` empty so an old site's topology hash does not change.
- Legacy mode (no `roster.toml`) behaves exactly as today: same `/api/members`, pages, Mixes tab, files. Every roster-mode test has a legacy-mode twin where behaviour differs.
- Two-step rollout (spec §12): the PC's data changes (roster, migrated files, `peers` in the site) only once both the pin and the previous pin are builds of this plan.
- UI copy for the new controls is Slovak, informal ("Prepnúť kapelu", "Zrušiť"), like `back_to_reaper.rs`.
- Versions: one workspace version; bump first after every merge to `main` (`scripts/check_version.py`).
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`; bug fixes go red before green (`[red]`/`[green]`).

## Review Focus

1. **A switch that fails half-way** (engine refuses, times out, the server restarts between import and record): nobody may keep another lineup's mix while the page says otherwise. Pinned by Task 8's crash-replay and refusal tests.
2. **A roster edit that drops or renames the active lineup, or a leader's set that cannot hear the others**: the server must refuse it and keep serving the old roster, saying why in `roster.status.json`. Pinned by Task 4 and Task 9 tests.
3. **A youth member on a leader's set** inherits that leader's heard-mix levels unless they are forced off; a non-leader must never hear another person's mix. Pinned by Task 8 `heard_mixes_go_off_on_a_non_leaders_set`.
4. **A person not in the active lineup opening their page** must see the notice, not a spinner that retries forever (today's 404 path). Pinned by Task 11 and the E2E in Task 13.
5. **Two pages switching at once, or a page sending a mix change during a switch**: one switch wins, the other gets "busy", and no change lands between capture and import. Pinned by Task 8 `a_second_switch_is_busy` and `mix_changes_are_refused_while_switching`.

---

## Part A — Engine: peers (PR 1)

### Task 1: `peers` in the site and the topology

**Files:**
- Modify: `crates/iem-engine/src/site.rs` (`SiteMix`, `SiteError`)
- Modify: `crates/iem-engine/src/topology.rs` (`MixNode`, `Topology`, `compile`, `levels`, `slot`, `source`, `info`)
- Modify: `crates/iem-engine-proto/src/msg/engine.rs` (`MixInfo`)
- Modify: `crates/iem-engine-proto/src/msg/tests.rs` (the `MixInfo` literal)
- Test: `crates/iem-engine/src/topology.rs` `mod tests`

**Interfaces:**
- Produces: `SiteMix.peers: Vec<String>` (`#[serde(default)]`); `MixNode.peers: Vec<usize>`; `MixNode.shadow: bool` (the mix hears anything and some mix has it in `peers`); `Topology.order: Vec<usize>` (render order: every mix that hears nothing, then every other, each group in declaration order); `MixInfo.peers: Vec<MixId>` (`#[serde(default, skip_serializing_if = "Vec::is_empty")]`); `SiteError::Peer { mix: String, peer: String }`.
- Slot space of mix `m`: inputs, then `mixes`, then `peers`. `levels(m) = inputs + mixes.len() + peers.len()`. `slot(m, Source::Mix(id))` looks in `mixes` first, then `peers`; `source` is its inverse.

- [ ] **Step 1: Write the failing tests** in `topology.rs` `mod tests` (use `BASE`, `err`, `replaced`):
  - `a_peer_may_be_declared_after_the_mix_that_hears_it`: `m1` with `peers = ["tr2"]` where a stereo `tr2` is declared after `m1` compiles; `topo.mixes[m1].peers == vec![index_of("tr2")]`.
  - `a_peer_must_be_another_stereo_mix_once_and_not_also_heard_as_a_mix`: each of `peers = ["m1"]` on `m1` (itself), `["ghost"]`, `["in1"]` (an input), `["tr"]` (mono), `["eng"]` (the engineer mix), `["m2","m2"]`, and `m2` in both `mixes` and `peers` gives `SiteError::Peer { mix, peer }`.
  - `peers_extend_the_slot_space_after_heard_mixes`: for a mix with one `mixes` entry and two `peers`, `levels == inputs + 3`, `slot(Source::Mix(peer2)) == Some(inputs + 2)`, `source(m, inputs + 2) == Some(Source::Mix(peer2))`.
  - `a_mix_that_hears_and_is_a_peer_has_a_shadow`: `m1.peers = ["m3"]`, `m3.peers = ["m1"]` compiles (mutual), both `shadow == true`; a peer that hears nothing has `shadow == false`.
  - `the_render_order_puts_mixes_that_hear_nothing_first`: `order` lists every mix with empty `mixes` and `peers` first in declaration order, then the rest in declaration order.
  - `a_site_without_peers_keeps_its_hash`: `serde_json::to_string(&topo.info())` of `BASE` (no `peers` anywhere) holds no `"peers"` key, so the hash of every site written before this plan is unchanged.
  - `errors_read_well` gains `SiteError::Peer`'s text: `mix "m1" can hear as a peer only another stereo set mix, each once and not also in mixes: "m1"`.
- [ ] **Step 2: Push `dev`, confirm the `test` job fails on these tests** (`gh run view <id> --log-failed`), never locally.
- [ ] **Step 3: Implement.** `compile` resolves `peers` after all mixes are declared (a second loop over `site.mixes` once `mix_index` is full); `heard` for `mixes` keeps today's declare-before-use check unchanged; `shadow` is computed after both loops; `order` is computed last. `info()` fills `MixInfo.peers`.
- [ ] **Step 4: Push; the `test` job passes** (every existing topology test unchanged).
- [ ] **Step 5: Commit** `feat(engine): peers in the site and the topology (#58)`.

### Task 2: Shadow chains and the render order

**Files:**
- Modify: `crates/iem-engine/src/rt.rs` (`MixRt`, `ShadowRt`, `Processor::with_hil`, `apply`)
- Modify: `crates/iem-engine/src/rt/render.rs` (`render_mixes`)
- Modify: `crates/iem-engine/src/core/state.rs` (`reconcile`, `to_state`: iterate `mixes` then `peers`)
- Modify: `config/test-site.toml` (sets hear each other as peers, see Step 1)
- Modify: `crates/iem-engine/tests/parity.rs` (`Model`), `tests/common/mod.rs` (`open_state`, `scenario`), `examples/bench.rs` (`open`, `worst_groups`)
- Test: `crates/iem-engine/src/rt/tests.rs`, `crates/iem-engine/tests/parity.rs`, `crates/iem-engine/src/core/tests/state.rs`

**Interfaces:**
- Consumes: Task 1's `MixNode.peers`, `MixNode.shadow`, `Topology.order`.
- Produces: `struct ShadowRt { out: Stereo, eq: Equalizer<2>, limiter: MixLimiter, fader: StereoGain, trips: Trips }` in `rt.rs`; `MixRt.own: Stereo` (the mix's inputs and group strips at their levels, before EQ) and `MixRt.shadow: Option<ShadowRt>`.
- Semantics (spec §8): a peer reads that mix's own output = `own → EQ → limiter → volume/mute`. For a mix that hears nothing, its final `sum` is that output; for a shadowed mix, `shadow.out` is.

- [ ] **Step 1: Change the test site.** In `config/test-site.toml`, `member1` gets `peers = ["member2", …, "member9"]` instead of `mixes`, and `member3` gets `peers = ["member1", "member2", "member4", …, "member9"]`; the engineer keeps `mixes = [member1 … member9]`. Update the comment above it (sets hear each other as peers; the engineer hears whole mixes). Existing tests that pin the site's shape change in their own commit before any code (`test(engine): the test site's leader sets hear each other as peers (#58)`): `test_site_has_the_program_shape` (member3 now hears 8 peers: `levels(m3) == 32`), `import_replaces_state_and_fits_one_block` (count the extra 8 peer levels: `11*24 + 17 + 8`), `test_committed_site_files_parse_and_validate` unchanged.
- [ ] **Step 2: Write the failing tests.**
  - `rt/tests.rs::a_peer_hears_the_other_mixs_own_output_without_what_that_mix_hears`: a site with `a.peers = ["b"]`, `b.peers = ["a"]`, input `x` only in `a`'s levels at 0 dB, input `y` only in `b`'s at 0 dB, `a` hears `b` at 0 dB and `b` hears `a` at 0 dB: `a`'s TX = `x + y` and `b`'s TX = `y + x`, each without the other's peer term (no doubling, no feedback). Use `Rig` and `dc`.
  - `rt/tests.rs::a_shadow_follows_the_mixs_eq_limiter_and_volume`: a `MixEq`, `Limiter` and `MixOut` command on `b` changes what `a` hears of `b` exactly as it changes `b`'s own TX (compare `a`'s peer contribution with `b`'s TX at steady state with `b` hearing nothing audible).
  - `rt/tests.rs::a_tripping_shadow_resets_and_counts`: a non-finite input into `b`'s own sum trips the shadow too; `status.trips` counts it.
  - `parity.rs`: `Model::mix` computes `own` for every mix first (inputs, groups, EQ, limiter off, volume/mute), then each mix's output in declaration order adding its `mixes` from `out` and its `peers` from `own`. `random_state` and `random_cmd` include peer levels. The existing `impulse_oracle_matches_random_states` and `outputs_do_not_depend_on_the_block_size` must pass on the new site.
- [ ] **Step 3: Push; the `test` and `engine` jobs fail on them.**
- [ ] **Step 4: Implement.**
  - `with_hil` builds `shadow: Some(ShadowRt)` for each `node.shadow` with the same EQ, limiter (`MixLimiter::new(sr, enabled, limit_db, 0)`: its active seconds are never counted) and volume as the mix.
  - `apply`: `MixOut`, `MixEq` and `Limiter` for mix `m` also set `shadow`; `ResetLimiter` only the mix's own limiter.
  - `render_mixes` walks `topo.order`. For each mix: zero and fill `own` (direct inputs, group strips) once; if `shadow` is some, copy `own` into `shadow.out` and run the shadow's EQ → limiter → `stereo_gain` → trips. Then `sum = own + Σ mixes (their sum) + Σ peers (their shadow.out, or their sum when they hear nothing)`, then the existing chain. Read other mixes through a helper `fn split_one(mixes: &mut [MixRt], m: usize) -> (&mut MixRt, Others<'_>)` built from two `split_at_mut`, where `Others::get(i) -> Option<&MixRt>` for any `i != m`.
  - All buffers are allocated in `with_hil` (I7).
- [ ] **Step 5: Extend the worst case.** `tests/common/mod.rs::open_state` opens peer levels at −12 dB like heard mixes; `scenario` adds one `Level` on a peer slot of `member3`; `examples/bench.rs::worst_groups` already emits every slot of `levels(m)`.
- [ ] **Step 6: Push; `test`, `engine` (rt, rtsan, parity, bench) and `mutation` pass.** Record the bench's worst p99.9 from the job log in the PR body next to the S3 figure (81.6 %).
- [ ] **Step 7: Commit** `feat(engine): shadow chains let sets hear each other (#58)`.

### Task 3: Peers through the server and the importer

**Files:**
- Modify: `crates/iem-server/src/site_view.rs` (`MixView.hears` = `mixes` ++ `peers`)
- Modify: `crates/iem-rpp/src/topology.rs` (`TopoMix` accepts and writes `peers`), `crates/iem-migrate/src/site.rs`
- Test: `crates/iem-server/src/site_view.rs` tests, `crates/iem-server/src/view/tests.rs`, `crates/iem-rpp` topology tests

**Interfaces:**
- Consumes: `MixInfo.peers`.
- Produces: `MixView.hears` lists `mixes` then `peers` (order = slot order).

- [ ] **Step 1: Failing tests.** `site_view::tests::a_mix_hears_its_peers_after_its_mixes` (member3 on the test site hears 8, member1 first); `view/tests.rs::member3_mutes_its_peer_member1` (a `SetMute` on channel `member1` from member3's page becomes `SetLevel{source: Mix(member1), muted}` on mix member3); `iem-rpp`: a site with `peers` round-trips through `TopoMix` unchanged.
- [ ] **Step 2: Push; fail.** **Step 3: Implement.** **Step 4: Push; pass**, the e2e job included (still legacy mode): member3 now hears peers, so a spec that asserted member3 has no Mixes tab changes in its own commit with that reason.
- [ ] **Step 5: Commit** `feat(server): peers are heard mixes on the page (#58)`. Open PR 1 (`dev` → `main`), all gates green, merge.

---

## Part B — The roster (PR 2)

### Task 4: `iem_core::roster`

**Files:**
- Create: `crates/iem-core/src/roster.rs`, `crates/iem-core/src/roster/tests.rs`
- Create: `config/test-roster.toml`
- Modify: `crates/iem-core/src/lib.rs` (module)

**Interfaces:**
- Produces:
  ```rust
  pub const ROSTER_FILE: &str = "roster.toml";
  pub struct Roster { pub people: Vec<Person>, pub lineups: Vec<Lineup> }        // deny_unknown_fields
  pub struct Person { pub id: String, pub name: String, #[serde(default)] pub takes_over: Option<String> }
  pub struct Lineup { pub id: String, pub name: String, #[serde(default)] pub main: bool,
                      #[serde(default)] pub leaders: Vec<String>,
                      pub sets: BTreeMap<String, String>,                       // mix id → person id
                      #[serde(default)] pub inputs: BTreeMap<String, LineupInput> }
  pub struct LineupInput { pub name: String, #[serde(default)] pub owner: Option<String> }
  pub struct MixFact { pub id: String, pub stereo: bool, pub hears: Vec<String> }
  pub struct TopologyFacts { pub mixes: Vec<MixFact>, pub inputs: Vec<String>, pub engineer: String }
  impl Roster {
      pub fn parse(text: &str) -> Result<Roster, RosterError>;
      pub fn problems(&self) -> Vec<String>;
      pub fn problems_against(&self, facts: &TopologyFacts) -> Vec<String>;
      pub fn person(&self, id: &str) -> Option<&Person>;
      pub fn lineup(&self, id: &str) -> Option<&Lineup>;
      pub fn main(&self) -> &Lineup;              // problems() guarantees exactly one
      pub fn set_of(&self, lineup: &str, person: &str) -> Option<&str>;
      pub fn leads_any(&self, person: &str) -> bool;
  }
  pub fn site_facts(site_text: &str) -> Result<TopologyFacts, RosterError>; // the site's [engine] only
  ```
- `problems()` (static): person ids pass `validate_member_id`, lower case, unique, never `engineer`; names not empty; lineup ids `[a-z0-9_-]{1,32}`, unique; exactly one `main`; every set's person exists; a person in at most one set per lineup; leaders and input owners are people placed in that lineup; `takes_over` is a valid id different from the person's own.
- `problems_against(facts)`: every set key is a stereo mix other than the engineer's; every input key is a topology input; for each lineup, each leader's set hears every other set of that lineup (`hears` = `mixes` ++ `peers`), else `leader {p} in {lineup}: set {s} does not hear {t}; the site needs peers (F30)`.

- [ ] **Step 1: Write `config/test-roster.toml`** (synthetic): people `member1`…`member11` (names `Member 1`…`Member 11`); lineup `main` (`main = true`, leaders `member1`, `member3`, sets `memberN → memberN` for N = 1…9, inputs as the site's owners); lineup `youth` (leaders `member3`, sets `member1 → member10`, `member3 → member3`, `member4 → member11`, inputs `mic1` "MEMBER10 mic" owner `member10`, `mic4` "MEMBER11 mic" owner `member11`).
- [ ] **Step 2: Failing tests** in `roster/tests.rs`: `the_test_roster_parses_and_fits_the_test_site` (no problems against `site_facts(include_str!(test-site.toml))`); `problems_name_every_offender` (one roster per rule above, each message asserted by substring); `a_leader_whose_set_cannot_hear_the_others_is_refused` (move `member3` out of `member1.peers` in a site copy); `exactly_one_main_lineup`; `unknown_keys_are_refused`.
- [ ] **Step 3: Push; fail. Step 4: Implement. Step 5: Push; pass.**
- [ ] **Step 6: Commit** `feat(core): the roster: people and lineups (#58)`.

### Task 5: Roster mode in the server's view of the site

**Files:**
- Create: `crates/iem-server/src/roster_state.rs`, `crates/iem-server/src/roster_state/tests.rs`
- Modify: `crates/iem-server/src/lib.rs` (`AppState.roster`, `site()` cache key, `page()`), `crates/iem-server/src/site_view.rs` (`SiteView::build_with`, `SiteView.leaders`, `SiteView.playing`)
- Modify: `crates/iem-server/src/routes.rs` (`/api/members`: `MemberInfo.playing`)
- Test: `site_view` tests, `routes/api_tests.rs`

**Interfaces:**
- Consumes: Task 4.
- Produces:
  ```rust
  pub struct RosterState { pub roster: Arc<Roster>, pub sha256: String, pub active: String }
  impl RosterState { pub fn lineup(&self) -> &Lineup; pub fn is_leader(&self, person: &str) -> bool }
  // AppState
  pub roster: Arc<RwLock<Option<RosterState>>>;       // None = legacy mode
  pub fn roster(&self) -> Option<RosterState>;         // a clone of the current one
  // SiteView
  pub fn build_with(cfg: &Config, roster: Option<&RosterState>, topo: &TopologyInfo) -> (SiteView, Vec<String>);
  pub leaders: Vec<String>;   // the active lineup's leaders; legacy: every member whose mix hears another
  pub people: Vec<PersonView>; // every person (legacy: the members), each with `playing: bool`
  ```
- Roster mode: `members` = the active lineup's sets (person → mix, the person's name); input names and owners from the lineup's `inputs`, falling back to the site's `[[inputs]]`; the engineer entry as today. `site()` caches by `(topo.hash, roster sha256, active)`.
- `/api/members`: every person (`playing` first, the engineer last as today); `MemberInfo.playing: bool` (`true` for everyone in legacy mode, so the JSON only gains a field).

- [ ] **Step 1: Failing tests.** `site_view::tests::roster_mode_maps_people_to_sets_of_the_active_lineup` (youth active: `member10` is on mix `member1`, `member2` has no page); `…input_names_follow_the_lineup` (youth: `mic1` reads "MEMBER10 mic", owner `member10`); `…legacy_mode_is_unchanged` (`build_with(cfg, None, topo)` equals `build(cfg, topo)` field by field); `api_tests::members_lists_playing_people_first` (youth: `member10`, `member3`, `member11` with `playing: true`, then the rest `false`, engineer last); `…legacy_members_are_all_playing`.
- [ ] **Step 2: Push; fail. Step 3: Implement. Step 4: Push; pass.**
- [ ] **Step 5: Commit** `feat(server): roster mode in the site view (#58)`.

### Task 6: Lineup files and the legacy migration

**Files:**
- Create: `crates/iem-server/src/lineup_store.rs`, `crates/iem-server/src/lineup_store/tests.rs`, `crates/iem-server/src/people_archive.rs`, `crates/iem-server/src/people_archive/tests.rs`
- Modify: `crates/iem-server/src/band_store.rs` (`BandStore::new` takes the data dir; unchanged otherwise), `crates/iem-server/src/lib.rs` (`AppState.band` becomes `fn band(&self) -> Arc<BandStore>` over the active lineup's dir), `crates/iem-server/src/pin_store.rs` (`archive_member`, `restore_member`: `PinFile.archived: BTreeMap<String, String>`, `#[serde(default)]`), `crates/iem-server/src/photo_store.rs` (`move_to`, never panics on a bad id: returns an error)

**Interfaces:**
- Produces:
  ```rust
  pub const LINEUPS_DIR: &str = "lineups";            // <config>/lineups/<id>/{state.json, presets/, snapshots/, customizations/}
  pub const ACTIVE_FILE: &str = "lineups/active.json"; // {"format":"iemmixer-active-lineup","lineup":id,"since":ts}
  pub const SWITCH_FILE: &str = "lineups/switch.json"; // {"from":id,"to":id}: present only during a switch
  pub struct LineupStore { dir: PathBuf }
  impl LineupStore {
      pub fn new(config_dir: &Path) -> Self;
      pub fn active(&self) -> io::Result<Option<String>>;
      pub fn set_active(&self, id: &str, since: i64) -> io::Result<()>;          // write_atomic
      pub fn state(&self, id: &str) -> io::Result<Option<MixerBackup>>;
      pub fn save_state(&self, id: &str, backup: &MixerBackup) -> io::Result<()>;
      pub fn intent(&self) -> io::Result<Option<SwitchIntent>>;
      pub fn set_intent(&self, i: &SwitchIntent) -> io::Result<()>;
      pub fn clear_intent(&self) -> io::Result<()>;
      pub fn data_dir(&self, id: &str) -> PathBuf;
  }
  pub fn migrate_legacy(config_dir: &Path, roster: &Roster, cfg: &Config) -> io::Result<MigrationReport>; // once, when lineups/ is absent
  pub fn archive_person(config_dir: &Path, id: &str) -> io::Result<()>;  // photo, every lineup's files, the PIN hash → archive/<id>/
  pub fn restore_person(config_dir: &Path, id: &str) -> io::Result<bool>; // the reverse when archive/<id> exists
  ```
- `migrate_legacy`: moves `presets/`, `snapshots/`, `customizations/<id>.json` of each legacy member who is a person of the main lineup into `lineups/<main>/…`; a person with `takes_over = X` gets X's three files (never X's PIN or photo); every legacy member that is no person is archived; writes `active.json` = main last. It stages into `lineups.migrating/` and renames it to `lineups/` as the final step, so a crash leaves either the legacy layout or the migrated one (a leftover `lineups.migrating/` is removed and the migration runs again).

- Backups (spec §6): `MixerBackup` gains `#[serde(default)] lineups: BTreeMap<String, MixerBackup>` (every inactive lineup's stored state; version stays 2, the field is additive); `backup::capture` fills it from `LineupStore`, `restore_backup` applies the live state as today and writes back the stored state of each lineup the roster still holds.

- [ ] **Step 1: Failing tests** (temp dirs): `backup::tests::a_backup_carries_every_inactive_lineup_and_a_restore_writes_them_back`; `…an_older_backup_without_lineups_restores_as_today`; `lineup_store::tests::active_state_and_intent_round_trip`; `…a_missing_state_reads_as_none`; `people_archive::tests::archive_moves_photo_files_and_pin_hash_and_restore_brings_them_back`; `…archive_of_an_unknown_id_is_a_no_op`; `lineup_store::tests::migration_moves_main_people_files_and_archives_the_rest`; `…takes_over_moves_presets_history_and_pins_hides_but_not_pin_or_photo`; `…a_crash_mid_migration_leaves_the_legacy_layout_and_runs_again` (fail the final rename through a test hook); `pin_store::tests::archived_hashes_load_from_an_older_file_as_none`.
- [ ] **Step 2: Push; fail. Step 3: Implement. Step 4: Push; pass.**
- [ ] **Step 5: Commit** `feat(server): lineup files, migration and the people archive (#58)`.

### Task 7: Loading and reloading the roster

**Files:**
- Create: `crates/iem-server/src/roster_watch.rs`, `crates/iem-server/src/roster_watch/tests.rs`
- Modify: `crates/iem-server/src/lib.rs` (`AppState::try_new` loads the roster; the watch task starts with the server), `crates/iem-server/src/bin/server.rs` (`iem-server roster check <file>`), `crates/iem-server/src/provision.rs` (`pin set-member` accepts a roster person)

**Interfaces:**
- Consumes: Tasks 4–6.
- Produces:
  ```rust
  pub const STATUS_FILE: &str = "roster.status.json"; // {"sha256":hex,"ok":bool,"at":ts,"problems":[..]}
  pub const POLL: Duration = Duration::from_secs(2);
  pub fn load(config_dir: &Path, cfg: &Config, facts: Option<&TopologyFacts>) -> Result<Option<RosterState>, String>;
  pub async fn watch(state: AppState);                 // polls (len, mtime), then sha256
  pub enum Applied { Unchanged, Applied { added: Vec<String>, removed: Vec<String> }, Refused(Vec<String>) }
  pub fn apply_text(state: &AppState, text: &str, sha256: &str) -> Applied; // pure enough to test without the poll
  ```
- Start: a roster that does not parse or has problems fails the server's start with them (exit 1, like an invalid site). On the first load with `lineups/` absent, `migrate_legacy` runs first.
- Reload: refused (old roster kept) when it has problems, does not hold the active lineup, or fails `problems_against` the current topology; applied otherwise: archive `removed` people, restore `added` ones from the archive, swap `AppState.roster`, write the status, broadcast `ServerMsg::LineupChanged` (Task 10). The status file holds ids and counts only.
- A person who moves to another set of a lineup (spec §9) takes their mix: `pub fn moves(old: &Roster, new: &Roster, lineup: &str) -> Vec<(String, String)>` (from set, to set; pure) and `pub fn apply_moves(state: &mut MixState, moves: &[(String, String)])` (the new set's mix becomes a copy of the old set's; the vacated set keeps its mix). Applied to every inactive lineup's stored state, and to the active lineup through one `ImportState` (completed with `keep_running`), before the new roster is swapped in.
- `iem-server roster check <file>`: `problems()` plus `problems_against(site_facts(<the site file named by IEMMIXER_CONFIG>))`; prints one JSON line `{"ok":bool,"problems":[..],"sha256":hex}`; exit 0 ok, 1 problems, 2 usage.

- [ ] **Step 1: Failing tests:** `roster_watch::tests::a_person_who_moves_sets_takes_their_mix` (main: `member5` moves from set `member5` to the empty set `member9`: the import gives mix `member9` `member5`'s levels, mix `member5` keeps its own); `roster_watch::tests::a_valid_change_applies_archives_the_removed_and_announces_it`; `…a_roster_without_the_active_lineup_is_refused_and_the_old_one_kept`; `…a_problem_roster_writes_a_refusing_status`; `…an_unchanged_file_is_not_applied_again`; `startup_tests::a_bad_roster_fails_the_start`; `…no_roster_is_legacy_mode`; `tests/roster_cli.rs::check_prints_one_json_line_and_exits_by_result` (integration test beside `pin_cli.rs`); `tests/pin_cli.rs::set_member_accepts_a_roster_person`.
- [ ] **Step 2: Push; fail. Step 3: Implement. Step 4: Push; pass.**
- [ ] **Step 5: Commit** `feat(server): load, check and reload the roster (#58)`.

### Task 8: The lineup switch

**Files:**
- Create: `crates/iem-server/src/lineup_switch.rs`, `crates/iem-server/src/lineup_switch/tests.rs`, `crates/iem-server/src/lineup_routes.rs`
- Modify: `crates/iem-server/src/lib.rs` (`AppState.switching: Arc<tokio::sync::Mutex<()>>`, `AppState.frozen: Arc<AtomicBool>`; startup replays an intent), `crates/iem-server/src/routes.rs` (merge `lineup_routes()`), `crates/iem-server/src/view.rs` (`ViewError::Switching` for mix changes while frozen), `crates/iem-server/src/notify.rs` (`push_engineers` payload `{"type":"LINEUP"}` with the target's name only)

**Interfaces:**
- Consumes: Tasks 5–7, `backup::keep_running`, `EngineClient::request_applied`.
- Produces:
  ```rust
  pub enum SwitchError { NotConfigured, Forbidden, Busy, Unknown(String), Same, NotSynced, Io(String), Engine(EngineError) }
  pub async fn switch_lineup(state: &AppState, to: &str, by: &AuthClaims) -> Result<(), SwitchError>;
  pub fn target_state(target: Option<MixState>, current: &MixState, lineup: &Lineup) -> MixState; // pure
  pub async fn replay_intent(state: &AppState) -> Result<(), String>; // once the mirror first syncs; mix changes stay refused (frozen) until it ends
  // routes: GET /api/lineup → LineupInfo; POST /api/lineup/switch {to} → 204 | 403 | 404 | 409 BUSY | 502
  pub struct LineupInfo { pub active: LineupRef, pub lineups: Vec<LineupRef>, pub playing: bool, pub leader: bool, pub can_switch: bool }
  pub struct LineupRef { pub id: String, pub name: String }
  ```
  (`LineupInfo` and `LineupRef` live in `iem-core` beside `ws.rs` so the UI shares them.)
- `switch_lineup` order (spec §7): right (engineer, or `roster.leads_any(sub)`) → `switching.try_lock()` else `Busy` → target exists and differs → `frozen = true` → capture the mirror (synced) with the active lineup's customizations into `lineups/<from>/state.json` → `set_intent({from, to})` → `request_applied(ImportState { state: target_state(..), baseline: false }, None)` → `set_active(to)` → `clear_intent` → swap `AppState.roster.active` → `frozen = false` → broadcast `LineupChanged`, close every member mixer socket with code `CLOSE_LINEUP = 4002` → Web Push to the engineer. Any error after the intent: import `lineups/<from>/state.json` again (best effort), `clear_intent`, `frozen = false`, return the error; the active lineup is unchanged.
- `target_state`: the target's stored state completed by `keep_running(.., current)`, or a copy of `current` when it has none; then every entry of `mixes[set].mixes` (the heard mixes and peers) on a set whose person in the target is not one of its leaders is `muted`, and a set with no person in the target gets `out.muted = true`.
- `replay_intent` at start: with an intent and `active == from`, import `to`'s stored state, `set_active(to)`, clear; with `active == to`, clear only.
- `LineupInfo.can_switch` = engineer, or a leader of any lineup. `playing` = the bearer is placed in the active lineup (the engineer: true). `leader` = a leader of the active lineup.

- [ ] **Step 1: Failing tests** (`engine::client::fake` for the engine, temp config dirs):
  - `lineup_switch::tests::a_leader_switches_and_everyone_gets_their_lineup_state` (main → youth: the import carries youth's stored state, `active.json` says youth, the intent is gone, `LineupChanged` broadcast, member sockets closed with 4002).
  - `…a_non_leader_cannot_switch` (403), `…the_engineer_can`, `…a_youth_leader_can_switch_back_to_main_without_playing_in_it`.
  - `…a_second_switch_is_busy` (two concurrent calls: one 204, one `Busy`).
  - `…mix_changes_are_refused_while_switching` (a `SetLevel` through `mixer_ws::handle` while `frozen` gets `ViewError::Switching` and reaches no engine).
  - `…an_engine_refusal_keeps_the_old_lineup` (the fake refuses `ImportState`: `active.json` unchanged, no intent, the `from` state re-imported).
  - `…a_restart_mid_switch_replays_the_intent` (intent present, `active == from`: `replay_intent` imports `to` and records it; with `active == to` it only clears).
  - `target_state` (pure): `heard_mixes_go_off_on_a_non_leaders_set` (youth: `member10` on set `member1` gets every peer level muted though main had them open); `…an_empty_set_is_muted`; `…a_lineup_without_state_starts_from_the_current_one`.
  - `lineup_routes` (`api_tests` style): `GET /api/lineup` for `member2` while youth plays → `playing: false, leader: false, can_switch: false`; for `member3` → `playing: true, leader: true, can_switch: true`; legacy mode → 404 `NOT_CONFIGURED`.
- [ ] **Step 2: Push; fail. Step 3: Implement. Step 4: Push; pass.**
- [ ] **Step 5: Commit** `feat(server): switch the active lineup (#58)`.

### Task 9: Leader rights on the page

**Files:**
- Modify: `crates/iem-server/src/view.rs` (`Viewer.leader: bool`; `channels` lists heard mixes only for the engineer or a leader; `command` refuses `SetLevel`/`SetMute`/`SetPan` on `Source::Mix` unless engineer or leader), `crates/iem-server/src/mixer_ws.rs` and `routes.rs` (build `Viewer` with `leader` from `SiteView.leaders`), `crates/iem-server/src/auth.rs` (`member_exists`: a roster person)
- Test: `crates/iem-server/src/view/tests.rs`, `auth/login_tests.rs`

**Interfaces:**
- Consumes: `SiteView.leaders`, `SiteView` people names.
- Produces: `Viewer { sub, engineer, leader }`; in roster mode heard-mix channels are named by the person on that set in the active lineup and omitted for a set nobody wears.

- [ ] **Step 1: Failing tests:** `view::tests::only_a_leader_sees_and_moves_heard_mixes` (youth: `member10` on set `member1` gets no `mixes` channels and a `SetLevel` on `member4` is `Forbidden`; `member3` gets channels named "Member 10", "Member 11"); `…legacy_keeps_todays_rule` (legacy: `Viewer.leader` is true for a member whose mix hears others, so today's tests pass unchanged); `login_tests::a_person_not_playing_may_log_in`; `…an_archived_person_may_not`.
- [ ] **Step 2: Push; fail. Step 3: Implement. Step 4: Push; pass.**
- [ ] **Step 5: Commit** `feat(server): the Mixes tab and heard mixes are the leaders' (#58)`. Open PR 2, gates green, merge.

---

## Part C — The UI (PR 3)

### Task 10: `LineupChanged` and close code 4002 in the protocol

**Files:**
- Modify: `crates/iem-core/src/ws.rs` (`ServerMsg::LineupChanged(LineupRef)`; round-trip tests), `crates/iem-server/src/mixer_ws.rs` (`CLOSE_LINEUP = 4002`)
- Modify: `crates/iem-ui/src/pages/mixer/connection.rs` (handle `LineupChanged`, a close with 4002 re-reads `/api/lineup` and reconnects or shows the notice; never `location.reload()`)
- Test: `ws.rs` round trip; `crates/iem-ui/src/pages/mixer/helpers.rs` native test `a_lineup_close_is_neither_a_failure_nor_a_reload`

- [ ] Steps: failing tests → push → implement → push → commit `feat(ui): follow a lineup change (#58)`.

### Task 11: Notice page, header badge and login tiles

**Files:**
- Create: `crates/iem-ui/src/components/lineup_notice.rs`
- Modify: `crates/iem-ui/src/api.rs` (`MemberInfo.playing` `#[serde(default = "yes")]`, `get_lineup() -> Result<LineupInfo, ApiError>`, `switch_lineup(to: &str) -> Result<(), SwitchFailure>`), `crates/iem-ui/src/pages/mixer/mod.rs` (fetch `/api/lineup` before connecting; `!playing` renders `LineupNotice`; header `.lineup-badge` with the active lineup's name), `crates/iem-ui/src/pages/landing.rs` (`MemberGrid`: playing people first, then a divider "Iná kapela" and the rest with class `member-card not-playing`), `crates/iem-ui/style.css`
- Test: native tests in `api.rs` (`lineup_info_reads_and_a_missing_playing_means_playing`), `landing.rs` (`playing_people_come_first`)

**Interfaces:**
- `LineupNotice` props: `info: LineupInfo`, `ws: …` none; text "Teraz hrá {active.name}. V tejto kapele nehráš." and, when `can_switch`, the `LineupSwitch` control (Task 12).

- [ ] Steps: failing tests → push → implement → push → commit `feat(ui): the notice for a person not playing, the lineup badge and tiles (#58)`.

### Task 12: The switch control

**Files:**
- Create: `crates/iem-ui/src/components/lineup_switch.rs`
- Modify: `crates/iem-ui/src/components/settings_modal.rs` (a "Kapela" section when `can_switch`, for leaders on their own page and the engineer on `/engineer`), `.cargo/mutants.toml` (exclude the component body per the rules, with a reason)
- Test: native tests of the pure helpers in `lineup_switch.rs`: `switch_errors_read_as_sentences`, `the_other_lineups_are_offered`

**Interfaces:**
- `LineupSwitch` props: `info: ReadSignal<LineupInfo>`. A select of the other lineups and a button "Prepnúť kapelu" opening `ConfirmDialog` with title "Prepnúť kapelu?", body "Teraz hrá {active}. Po prepnutí bude hrať {target} a každý dostane svoj mix pre túto kapelu.", `confirm_label` "Prepnúť", `cancel_label` "Zrušiť". States `Idle, Asking, Sending, Done, Failed`. Errors: 403 "Prepnúť môže len vedúci kapely alebo zvukár.", 409 "Práve sa prepína, skús to o chvíľu.", other "Prepnutie zlyhalo ({n})."
- `data-testid`: `lineup-switch`, `lineup-target`, `lineup-switch-button`, `lineup-badge`, `lineup-notice`.

- [ ] Steps: failing tests → push → implement → push → commit `feat(ui): leaders switch the band (#58)`.

### Task 13: E2E in roster mode

**Files:**
- Modify: `.github/workflows/ci.yml` e2e job: copy `config/test-roster.toml` to `$RUNNER_TEMP/site/roster.toml`; provision a PIN for every roster person (`m["id"]` over `[[people]]`) and the engineer
- Create: `e2e/tests/lineups.spec.ts`
- Modify: `.claude/rules/e2e.md` (lineups.spec owns the lineup and switches back to `main` in `afterAll`; `member10`, `member11` are its people)

- [ ] **Step 1: Write `lineups.spec.ts`** (import from `./support/fixtures`, PINs from `support/pins.ts`):
  - `a leader switches to the youth band and back` (member3 opens settings, picks `youth`, confirms; `lineup-badge` reads the youth name).
  - `a youth member hears their own mix on a borrowed set` (member10 logs in, the page connects, `global-volume-fader` visible, the header badge says youth).
  - `a person not in the band sees the notice, not a spinner` (member2 while youth plays: `lineup-notice` within 5 s, no `global-volume-fader`).
  - `an open page follows the switch` (member1's page open in main; after the switch it shows the notice without a reload).
  - `only leaders get the Mixes tab and the switch` (member4 in main: no `lineup-switch`, Mixes tab hidden; member3: both).
  - `afterAll`: `POST /api/lineup/switch {to: "main"}` as the engineer; assert 204 or already main.
- [ ] **Step 2: Push; the e2e job runs every spec in roster mode; fix what the roster mode changed** (`engineer.spec.ts:144` keeps its rule: in roster mode the elevated member is a leader).
- [ ] **Step 3: Commit** `test(e2e): lineups in the browser (#58)`. Open PR 3, gates green, merge.

---

## Part D — Tooling (PR 4)

### Task 14: `iempc roster-install`

**Files:**
- Create: `scripts/iem-pc/iempc_roster.py`, `scripts/iem-pc/test_iempc_roster.py`
- Modify: `scripts/iem-pc/iempc.py` (command table: `Spec(cmd_roster_install, pc=True, dev_time=True, locked=True)`; parser `roster-install --file PATH [--dry-run]`)

**Interfaces:**
- Consumes: `iempc_core.{load_env, ssh_ps, scp, remote, pc_mkdir, hash_check, run_module}`; new optional env key `PC_SERVER_CONFIG` (the server config's path on the PC; private).
- `run(ctx, ip) -> int`: read the file and its sha256 → scp to `PC_ROOT\incoming\roster-<sha12>.toml` → on the PC (one `ssh_ps`, the upload's `hash_check` first): `iem-server.exe roster check <upload>` with `IEMMIXER_CONFIG` = `PC_SERVER_CONFIG` from the active bundle → if ok and not `--dry-run`: copy beside the server config as `roster.toml.new`, `Move-Item -Force` onto `roster.toml` → poll `roster.status.json` (every `POLL_S`, ≤ 20 s) until its `sha256` is the file's → print one JSON object `{"roster_install": {"ok", "problems", "applied", "added", "removed"}}` with ids and counts only.
- Exit 0 applied (or dry-run ok), 1 refused or not applied in time.

- [ ] **Step 1: Failing tests** on `iempc_test_support.Base` (`FakePc.replies`): `refused_while_event_now`; `a_check_with_problems_installs_nothing`; `dry_run_checks_only`; `install_waits_for_the_status_sha`; `a_status_that_never_matches_fails_naming_the_wait`.
- [ ] **Step 2: Run** `python3 -m unittest scripts/iem-pc/test_iempc_roster.py` locally: fails. **Step 3: Implement. Step 4: Run: passes.**
- [ ] **Step 5: Commit** `feat(iempc): roster-install (#58)`.

### Task 15: Dante labels: decide the writer, then the tool

**Files:**
- Create: `scripts/iem-pc/iempc_dante.py`, `scripts/iem-pc/test_iempc_dante.py`
- Modify: `scripts/iem-pc/iempc.py` (`dante-labels --roster PATH [--apply]`), `.claude/rules/pc-tuning.md` or a new `.claude/rules/dante-labels.md` (what was learned)

- [ ] **Step 1: Spike (dev time, record on #58):** whether `netaudio` (the predecessor's tool) can run on the IEM PC or another host on the Dante network, read the IEM card's channel labels, and set one label of a channel nothing subscribes (the HIL spare TX from `[guard] hil_tx`) and set it back. Never touch another device, never a subscription. Outcome A: it works → Step 2 with `--apply`. Outcome B: it does not → Step 2 prints the sheet only, and the owner types the labels in Dante Controller.
- [ ] **Step 2: Failing tests:** `labels_follow_the_main_lineup_and_the_convention` (TX `NAME inear L NN` / `R NN`, RX `NN NAME mic`, ≤ 31 characters, the main lineup's person per set and input, from a synthetic roster and site); `renamed_tx_channels_are_listed_for_resubscription`; `apply_refuses_while_event_now`; with outcome A, `apply_touches_only_the_iem_card`.
- [ ] **Step 3: Implement. Step 4: Tests pass.** The sheet holds site values: printed on the dev box only, never committed or posted.
- [ ] **Step 5: Commit** `feat(iempc): Dante labels for the main lineup (#58)`. Open PR 4, gates green, merge.

---

## Part E — Rollout on the PC (ops, owner present for Dante)

### Task 16: Private data in `iemmixer-ops`

- [ ] **Step 1:** `iemmixer-ops/site/roster.toml`: the real people and both lineups from the owner's data (main: the current six; youth: from the owner; leaders; `takes_over` per person as the owner decides at migration). Ask the owner only for what #58 does not hold yet (the youth lineup's people and their sets).
- [ ] **Step 2:** `iemmixer-ops/site/site.toml`: the leaders' sets get `peers` (Task 2's shape); `[[members]]` stays as it is for the legacy build.
- [ ] **Step 3:** Check both with the build's `iem-engine check-site` and `iem-server roster check` (on the PC, `--dry-run` of Task 14). Ops PR, merged.

### Task 17: Two deploys, then the data

- [ ] **Step 1 (a Thursday window):** deploy the merged build X by `.claude/rules/guard.md` "A new build in prod". Legacy mode on the PC (no roster): verify `/api/members` and every page unchanged, HIL and live green, the reboot check.
- [ ] **Step 2:** deploy a second build X′ of this plan (the next `main` merge, at least a version bump), so `iemmode status` reads `pin X′, previous X`. Only then the data may change (spec §12).
- [ ] **Step 3 (maintenance):** `iemmode install-site` with the ops site's `peers` (F30); the engine restarts; `iemmode status` healthy at 32. Then a 1 h `iempc dispatch-soak` on the PC with the peers site: zero missed periods and `process` p99.9 within the S7 budget, before any roster change (spec §12, CPU).
- [ ] **Step 4:** `iempc roster-install --file <ops>/site/roster.toml`: the server migrates the legacy files into the main lineup, archives the people who left; verify the status sha, `/api/members`, and log in as a main and a youth person (PINs for new people: the engineer's PIN reset in the UI, or `iem-server pin set-member` on the PC with the PIN on stdin).
- [ ] **Step 5 (with the owner):** `iempc dante-labels --apply` (or the sheet), then the owner remakes the listed stagebox subscriptions in Dante Controller; check every in-ear output carries its mix.
- [ ] **Step 6:** the FOH note (the main lineup's renamed and removed people) goes to the owner for the fohmixer side.
- [ ] **Step 7:** on the PC, a leader switches main → youth → main from a phone; `iempc switch-test`-style evidence: no silence on the outputs, the active lineup and both states survive a graceful reboot. Report on #58.

---

## Ticket split

After the owner approves this plan: one ticket per part (A engine peers, B roster and switch, C UI and E2E, D tooling, E rollout), each linked to #58, worked in order A → B → C → D, E after D. PRs 1–4 merge to `main` as they go green; the band sees nothing until Task 17.
