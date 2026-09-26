# Engine model rework: a purpose-built monitor mixer — design note

**Ticket:** #20 (program #1). **Spec:** `2026-09-24-iemmixer-gen2-program.md` §2.4, I4–I7, §3.1–3.5, X1–X4, X13, X14, Q1, Q3, Q4, F5–F18, F29. **Plan:** `docs/superpowers/plans/2026-09-26-engine-model-rework.md`. **Replaces** the model parts of the S3 note (§3.1–3.4) and the mapping parts of the S4 note (§3.2–3.4); everything else there (RT contract, pipes, persistence mechanics, crash model, importer file rules, PIN rule, secrets) stands.

## 1. Owner rulings (verbatim on #20)

1. The REAPER architecture is over-complicated for the same mixing; Gen 2 must not be another DAW built in our code.
2. **The engine model is derived from the band members' GUI** (spec §3.2 F1–F32): it contains only what those features need, nothing "for generality". The sound stays proven by goldens against REAPER.
3. Do it now, before S5 builds the server and UI on the protocol.
4. Mandatory: `iem-migrate band` output becomes transactional, with a mid-write failure test.

REAPER survives only at the edges: the importer maps a project into the model, the exporter writes values back, and the goldens prove the laws.

## 2. What the GUI really does (evidence: the predecessor UI and server imported in S0)

| GUI feature | What it changes in REAPER | Engine element |
|---|---|---|
| Channel strip (F5): fader −60…+12 dB, pan, mute; stereo = one strip | the send of that input into the member's bus, or into the member's stems bus for a stems input (`proxy.rs` `SET/TRACK/{input}/SEND/{member}`) | **level** of an input in a mix |
| Solo (F6) | the server mutes every non-soloed channel send of that member's mix (input, stems input and mix strips); IEM VOL and the stems fader stay | **solo** set per mix |
| Stems strip (F7): fader, mute, EQ; no pan | the stems **bus** fader, mute and ReaEQ | **group** fader, mute and EQ per mix |
| IEM VOL (F7): fader ≤ +12 dB, mute, EQ, LIM; no pan | the output bus fader, mute, ReaEQ, limiter | mix **output**: EQ → limiter → volume → mute |
| EQ modal (F11) | ReaEQ of an input (shared), of the member's bus, of the stems bus | input EQ, mix EQ, group EQ |
| Limiter (F12) | ceiling, enable, active-seconds reset on the output bus | mix limiter |
| Mixes tab (F16) — engineer: members 1–9; elevated member: members 2–9 | the **bus→bus send of member N's bus into the viewer's own bus** (level, pan, mute; `resolve_send_index` → `SEND/{mix_si}`): the viewer hears member N's whole mix in their own ears | a mix may hear **other mixes** as sources |
| Engineer opens any member's mixer | nothing in audio: token check `claims.engineer` | permission (server), not audio |
| Listen (F15, F17): engineer only, the member of the page | the engineer bus tap (`VBAN IEM` after EQ and limiter); for member N the server mutes every bus→engineer send but N's (a workaround) | **monitoring taps** (X3): engineer after its limiter; one member after mute |
| Talk (F18) | `OIEM Receive` on `ENG_MIC` after TRIM and ReaEQ, before the mute | **talkback** injected into its input (A4) |
| Mute All (F15) | mutes every source send of the engineer's bus | a server batch of level mutes; no engine concept |
| F29 (new engineer surface): input mute, trim, processing, TRANSLATOR, limiter-stats reset | REAPER-GUI-only today, but the saved project holds non-default values (two trims, a muted input, two inputs with FX off) | input trim, processing, mute; the translator is a mix |
| — (no GUI, no server write) | the master: `MASTERMUTESOLO 1` (**muted** in the saved project), fed by every input fader and every stems bus | **nothing**: dropped with the input fader and pan |
| — | bus pan, stems-bus pan: no control anywhere; 0 in the saved project | **nothing** |
| — | stems bus → member bus send: vol 1, pan 0, unmuted on all 10, never written | **nothing** (the group feeds its mix directly) |

Verified in the saved project (backup branch, 2026-09-26): the master is muted, every member, engineer and stems bus has pan 0, every stems→bus send is unity, centred and unmuted, the stems-bus EQs are flat, and the 17 bus→bus sends are muted. Presets/snapshots carry per-channel levels (mix strips included) and the stems level.

## 3. The model

```
Input  ──RX──▶ trim → EQ (processing on/off, Q3) → + talkback (one input) → mute ──▶ P
Group  = a named set of inputs (e.g. "stems")
Mix    = one per listener (members, engineer, translator)
         Σ level·P  over every ungrouped input
       + Σ over each group:  group fader( group EQ( Σ level·P over the group's inputs ) )
       + Σ level·O  over the mixes it hears (each declared before it)
       → EQ → limiter → volume/mute ──▶ O ──▶ Q1 safety → clamp → fade → TX (2 = stereo, 1 = mono downmix)
Taps   slot 0: the engineer mix after its limiter (before volume/mute); slot 1: one other mix after mute, through a listen-path limiter (X3)
```

- **Inputs** (`trim_db`, `processing`, `muted`, `eq`): mono (one RX, L = R, A2) or stereo (two RX). One input may carry talkback (A4: `+= 0.379934·talkback` after its EQ, before its mute).
- **Groups** (site only): named, disjoint sets of inputs. A mix hears a grouped input only through the group: per mix, each group has a **strip** (`gain_db`, `muted`, `eq`; no pan).
- **Mixes** (per listener): an **output** (`volume_db` ≤ +12, `muted`, `eq`, `limiter`, no pan) and **levels** (`gain_db`, `pan`, `muted`) for every input and for each mix it hears. Every mix hears every input — a personal monitor mixer offers every channel to every listener; the site lists no per-mix sources, only the mixes a mix hears (the Mixes tab). A mix hears only mixes declared before it (define-before-use: no cycles by construction, evaluation in declaration order, no sort).
- **Output**: a mix has two TX channels (stereo) or one (mono: `(L + R)/2`, A10). Every mix has the same pipeline; the translator is simply a mono mix whose REAPER track had no plug-ins (imported with a flat EQ and a disabled limiter).
- **Solo** (per mix, transient, X2): the soloed sources (inputs and heard mixes); every other level of that mix is silenced (5 ms), the group strips and the output are untouched.
- **Monitoring**: the two X3 taps; listening needs no audio routing.
- **No master, no buses, no sends, no send modes, no bus kinds, no topological sort, no input fader or pan, no mix or group pan.**

### 3.1 Laws (unchanged; how they read now)

- A5: every level, group fader and output volume is `v·(1−m)·(gL(p), gR(p))` (the measured law; group and output at p = 0).
- A6: levels read the input's post-FX, post-mute signal P (REAPER mode 3); a heard mix is read at O, after its mute (mode 0, A9: unclipped).
- A7 (group = the stems bus): Σ → EQ → fader → mute → into its mix; the stems→bus send (unity, centred) is gone. At p = 0 the law's gain is 1 + 2.2·10⁻¹⁶, so dropping that stage moves a sample by ≤ 2.2·10⁻¹⁶ relative — below every tolerance (goldens 1e-9, oracle 1e-12).
- A8: Σ → EQ → limiter → volume → mute → Q1 safety → clamp ±1.0.
- A10: a mono mix carries `(O_L + O_R)/2` on its channel.
- A11 (master): **removed** — the master is muted in the saved project, so its TX pair is silent today and stays silent.

## 4. Site file (`[engine]` in `site.toml`)

```toml
[engine]
channels = 160                    # the card's channel map
engineer = "engineer"             # the mix with the fixed listen tap (X3 slot 0)

[[engine.inputs]]
id = "eng_mic"
rx = [114]                        # 1 = mono, 2 = stereo
talkback = true                   # at most one

[[engine.groups]]
id = "stems"
inputs = ["click", "guide", "drums", "bass", "inst", "other", "bgvs"]

[[engine.mixes]]
id = "member2"
tx = [73, 74]                     # 2 = stereo, 1 = mono

[[engine.mixes]]
id = "member1"
tx = [71, 72]
mixes = ["member2", "member3", "member4", "member5", "member6", "member7", "member8", "member9"]
```

Validation refuses: bad or duplicate ids (inputs, groups and mixes share one namespace), RX/TX counts, channels outside the map or used twice, an input in two groups or an unknown one, an empty group, a heard mix that is unknown, itself, repeated or declared later, a second talkback input, an engineer that is not a stereo mix. `config/test-site.toml`: 24 inputs on RX 101–132, the stems group, 11 mixes on TX 71–88, 91/92 (engineer), 93 (translator); member1 is declared after the mixes it hears.

## 5. Protocol and state (`iem-engine-proto`)

- **Ids:** `InputId`, `GroupId`, `MixId` (strings, `valid_id`); `Source = input | mix` (a level's source); `EqTarget = input | mix | group{mix, group}`.
- **State** (`MixState`, persisted schema **2**): `inputs: {id → {trim_db, processing, muted, eq}}`, `mixes: {id → {out: {volume_db, muted, eq, limiter}, inputs: {id → Level}, groups: {id → {gain_db, muted, eq}}, mixes: {id → Level}}}`. `Level` defaults to off (−150 dB), a group strip and the output to 0 dB. Files written before the rework (schema 1) are refused by the load chain and reported.
- **Commands:** `set_input{trim_db, muted, processing}`, `set_mix{volume_db, muted}`, `set_level{mix, source, gain_db, pan, muted}`, `set_group{mix, group, gain_db, muted}`, `set_eq{target, eq}`, `set_limiter{mix, …}`, `reset_limiter_stats{mix}`, `set_solo{mix, sources}`, `start_listen{mix}`, `stop_listen{mix}`, and the unchanged test signal, batch, import, get, save, shutdown, fault and ping.
- **Changes:** `input`, `mix_out`, `level`, `group`, `solo`, `listen`, `test_signal`, `limiter_stats_reset` — each carries only the entity that changed.
- **Topology:** `{hash, sample_rate, engineer, inputs[{id, channels, talkback, group}], groups[{id, inputs}], mixes[{id, channels, mixes}]}`; meter frames list inputs, mixes, then every mix's group strips (mix-major), with limiter GR and X14 seconds per mix.
- `PROTO` stays 1: the protocol has no consumer yet (S5 is its first), so version 1 is redefined once, here.

## 6. Engine (`iem-engine`)

- `site.rs` parses the table above; `topology.rs` (was `graph.rs`) validates and compiles it: inputs with their group, groups with their inputs, the ungrouped inputs, mixes with TX indices and the indices of the mixes they hear, the hash of the `TopologyInfo`. No reach table: every mix hears every input, so a test signal caps every TX (X13).
- `rt.rs`: per segment, inputs as before (no fader), then the mixes in declaration order as in §3 (the group sum reuses one scratch buffer; group EQ and fader state are per mix). Sanitiser (X1) after every input, group strip and mix; the ramps, the 256-sample segments, the command budget and the sample-accurate cuts are unchanged.
- `core.rs`: the same pure `apply` with the new commands and caps; a mix's levels are one index space (inputs, then heard mixes), so a solo or a level is one RT op.
- Persistence, control loop, pipes, media, crash model: unchanged apart from the per-mix limiter counters.

## 7. Importer and exporter (`iem-rpp`, `iem-migrate`)

**Aliases** (private): `[tracks]` REAPER name → id, where a stems-bus track maps to its **group** id (all instances of a group share it); `[members]` legacy member → `{ id, mix, archived }`. The `master` key is gone.

**Project → model:**

| REAPER | Model | Rule (else a listed error) |
|---|---|---|
| record-armed track | input (RX from `REC`, talkback from `OIEM Receive`) | chain TRIM, ReaEQ (+ talkback); no receives or outputs; its fader and pan feed only the master → ignored (reported) |
| track with one `HWOUT` | mix (stereo pair or mono ≥ 1024) | stereo: ReaEQ, limiter (+ `VBAN IEM` = engineer); mono: no plug-ins (flat EQ, disabled limiter); pan 0 |
| track without `HWOUT` | a group instance of the mix it sends to | ReaEQ only; pan 0; exactly one mode-0 send, into a mix, unity/centred/unmuted |
| mode-3 receive input → mix | level of that input in that mix | the input is not grouped |
| mode-3 receive input → group instance | level of that input in the instance's mix; the input joins that group | an input sends into one group only |
| mode-0 receive mix → mix | level of the heard mix | — |
| missing receive | level off (−150 dB) | — |
| master | nothing | must be muted (`MASTERMUTESOLO` bit 0); plug-ins refused as before |

The derived topology (inputs, groups with members, mixes with the mixes they hear in dependency order, engineer) is compared with `site.toml`; any difference refuses the import (exit 3) and `--emit-topology` writes the new `[engine]` table. The REAPER-side counts (`--expect tracks=45,sends=268,eqs=44,limiters=10,trims=24`) still describe the project.

**Export:** patches the same kinds of values into the original project; a value the project cannot hold (a level with no receive, the translator's EQ or limiter, a group strip of a mix without an instance, input faders) is **not carried back and reported** (spec §4.3: iemmixer-only changes are not carried back); unchanged states export byte-identical, the self-check compares what the project holds.

**Band data** (`iem-core::band`, schema **3**): sends keyed by `Source::{input, mix}`, `groups: {id → gain_db}` instead of `stems_fader_db`; a legacy stems level maps to the site's only group (several groups: an error). A legacy key maps to an input, or to a mix the member's mix hears; a stems-bus key is unmappable (fails loudly, as before).

**Transactional `band` output (mandatory item):** every write goes into a staging directory next to the target (`.<name>.iem-migrate`), which starts as a copy of the existing band directory, so merges (PINs, push list, equal secrets) behave as before. After every file is written and synced, a `.complete` marker is written, the old directory is renamed aside (`.<name>.iem-migrate-old`), the staging directory takes its name, and the old one and the marker are removed. `recover(out)` (run at the start of every `band` run, exported for the guard and server) rolls a crashed commit forward (complete staging, no target) or back (incomplete staging, or the target intact). A failure at any write leaves the target byte-identical and removes the staging directory; tests inject a failure at every step and a crash between the two renames.

## 8. Proofs carried over

| Proof | Now |
|---|---|
| Impulse oracle (1e-12) | a new reference model written from §3 over the declarative site (not the engine's code): O per mix in declaration order, groups, heard mixes, mono downmix; random states; zero elsewhere |
| A3/A6/A9/A10 encoded cases | the same cases in model terms (mute kills every level and talkback, levels ignore nothing upstream of P, a heard mix is post-mute and unclipped, mono half-sum) |
| Goldens vs REAPER | unchanged (`goldens/s1b`: pan/send/post-fader laws, EQ, limiter, downmix); the model uses exactly those laws |
| Block-size invariance (bit-exact) | the same test over the new commands |
| `assert_no_alloc`, rtsan | the worst-case scenario rewritten with the new commands (every level, group and output moving, both taps, talkback, test signal, import) |
| Benchmark at B = 32 | the same two cases; the worst case now moves 281 levels, 11 group strips, 46 EQs |
| Fuzzing | `props.rs` and the cargo-fuzz target over the new commands |
| Import round trip | synthetic predecessor projects (`sitegen`) in the new mapping; unchanged export byte-identical; edited state within 1e-9 dB |

## 9. Old → new

| Old (REAPER-shaped) | New (purpose-built) |
|---|---|
| buses with kinds `output` / `stems` / `translator` / `master` | mixes (stereo or mono) and groups |
| 268 sends in 14 families, `tap = pre | post` | levels: every mix × every input, plus the mixes a mix hears (17) |
| 10 stems buses + 10 stems→bus sends | one group with a strip per mix |
| master bus, input fader and pan (A11) | — (the master is muted today) |
| bus pan, stems-bus pan | — |
| bus→bus sends into the engineer and member1, muted | heard mixes (the Mixes tab) |
| Kahn topological sort, cycle detection, reach table | declaration order; every TX capped by a test signal |
| `SetBus`, `SetSend`, `EqOwner`, `BusKind`, `Tap`, `SendId` | `SetMix`, `SetLevel`, `SetGroup`, `EqTarget` |
| 44 EQs, 10 limiters | 46 EQs, 11 limiters (the translator's are new, flat and disabled on import) |
| 23 TX | 21 TX (the silent master pair is gone) |

## 10. Deviations and spec amendments

- **A11 and the F29 master fader are removed**; I4 reads "validated (unique TX, channels in the map, define-before-use for heard mixes)".
- Every mix hears every input: a mix level with no REAPER receive is off after an import and is not carried back on export (reported).
- The translator gets the same EQ and limiter as every mix; the import sets them flat and disabled, so it sounds as in REAPER.
- The listen workaround of the predecessor (muting bus→engineer sends to listen to member N) is not reproduced: X3 taps listen side-effect-free (already approved in X3).
- `PROTO` stays 1 (no consumer yet); state schema 2, band schema 3 (never written in production).

## 11. Not in this rework

The server and UI on the protocol (S5: levels, strips, taps, `mix_view` = the mixes a mix hears, permissions); the ASIO backend and the guard calling `recover` (S6); real-site import rehearsals (S8).

## 12. Risks

- **Private site mapping:** the real project must import under the new rules (master muted, pans 0, unity stems sends); the private check (`tools/check_import.sh`) proves it on the CI-built binary.
- **Mutation budget:** the rework touches most of the engine; the `mutants-list` count sizes the shard matrix.
- **Worst-case CPU:** slightly more moving parts than before (281 vs 268 gain pairs, 46 vs 44 EQs), minus the master; the benchmark gate stays.
