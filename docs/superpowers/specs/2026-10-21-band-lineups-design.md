# Band lineups and band switching — design (epic #58)

Status: draft for the owner's review, 2026-10-21. Decisions are quoted from the epic's `ROZHODNUTÉ` comments; everything marked *(my call)* is the designer's and open to the owner's correction.

## 0. Zhrnutie pre vlastníka

- **Vybavenie** (mikrofón, prijímač do uší) je pevné a v Dante sa volá po hlavnej osobe. Napojenie v Dante sa nikdy nemení.
- **Človek** má meno, PIN a fotku. V každej kapele, v ktorej hrá, má vlastný mix, predvoľby a históriu.
- **Kapela** hovorí, kto nosí ktoré vybavenie a kto sú jej vedúci. Kapiel môže byť viac ako dve.
- **Prepnutie kapely** spustí vedúci alebo zvukár z mobilu. Naraz nahrá zostavu aj mixy všetkých, bez ticha v ušiach a bez reštartu.
- **Zmeny ľudí** zatiaľ robí Claude spolu s vlastníkom, bez obrazovky na to.
- **Vedúci** počúva mixy ľudí svojej kapely a prepína kapelu. Cudzie mixy nemení.

## 1. Intent and why

The band's lineup changed a lot, and the predecessor made changing it close to impossible: the REAPER project had one track per person, so new people used someone else's mix under someone else's name and juggled settings so they would not overwrite each other. iemmixer still carries that old lineup, one fixed member per in-ear output.

The church also has two bands that share the stage's equipment and some people: the main band and the youth band. Each wants its own settings, the way a sound engineer recalls a scene and its patch. The band's leaders, not only the engineer, must be able to say which band is on stage.

Success: every person logs in as themselves and hears their own mix for the band that plays; switching the band is one action on a phone; changing who is in a band is a short session with Claude.

## 2. Goals, non-goals, anti-goals

**Goals**

- G1. People are separate from equipment. A person has one identity (id, name, PIN, photo) across bands.
- G2. A band says who wears which in-ear output and whose voice or instrument is on which input, and who leads it.
- G3. A person's mix, presets, history and pins/hides belong to that person *in that band*. One person may play in several bands with a different mix in each.
- G4. Switching the active band loads its lineup and every mix at once, with no silence, no click and no engine restart, from any leader's or the engineer's phone.
- G5. People and bands can be added, removed and renamed without a maintenance session, by Claude with the owner, without a dedicated screen.
- G6. The IEM card's Dante labels name each set's main person, and change only when that person changes.

**Non-goals (this epic)**

- A screen for editing people and bands (later; the data model must allow it).
- Scheduled or automatic band switching.
- Changing any Dante subscription, or any Dante device other than the IEM card.
- Changes on the FOH machine (the band's Ableton set, fohmixer). They are another project's; this epic only produces the list of what changed for it.
- Per-band "recall safe" exceptions. A band's state is the whole mixer state.

**Anti-goals (a failure even if it works)**

- Anyone hears another person's mix in their ears after a switch, even for a moment.
- A band's settings are lost or overwritten by another band's, at a switch, a crash or a reboot.
- A switch needs maintenance, an engine restart, REAPER, or a Dante change.
- People go back to logging in under someone else's name because the system makes the right way hard.

## 3. The owner's decisions (epic #58, 2026-10-21)

1. The bands partly share people, and they share equipment. One leader plays in both bands with a different mix in each. A youth-band member uses a main-band member's mic and in-ear receiver.
2. A band leader gets today's elevated right (the Mixes tab: hearing the other members' mixes in their own ears) plus switching the band. A leader never opens or changes another member's own mix; that, talkback, Mute All, backups, PINs and the input console stay the engineer's. There are two leaders; today only one has the Mixes tab.
3. Shared equipment switches **inside iemmixer**. The output to a shared receiver and the shared mic's input stay subscribed in Dante for good; iemmixer sends the mix of whoever wears that receiver in the active band.
4. Dante keeps the **main person's** name on a shared set. Only iemmixer knows that a youth-band member borrows it.
5. Roster changes may be done with Claude, without a GUI, at first.
6. A person who leaves is removed from iemmixer, and their files (presets, history, photo, PIN hash) are kept in an archive on disk so they can be restored on return. An archived person is in no band, cannot log in, appears nowhere and costs the engine nothing.
7. **Everything ships together** (owner, 2026-10-21): people, the roster, both bands, switching and the leaders' mutual hearing reach the band as one result, not the lineup clean-up first. The technical steps below (§12, two-step rollout) may still span more than one weekly window.

## 4. Facts the design rests on

From the epic's findings comments (HEAD `59b2995`):

- A member today is `{id, name, mix}` in the site file's `[[members]]`; an input's display name and owner are in `[[inputs]]`. The server loads both once at start. Mix state is keyed by mix and input ids in the engine; presets, history, pins/hides, PIN hashes and photos are keyed by member id in the server's config directory.
- The engine reads the topology once; a topology change is a controlled engine restart (program spec I4), done by F30 `iemmode install-site` in dev mode only (a maintenance session in prod).
- A mix hears only mixes declared before it, and reads their final sum (after fader and mute, A9). Two mixes cannot hear each other.
- Restore with preview (F31) already applies a whole `MixState` in one engine `ImportState`.
- No role exists beyond the engineer. The elevated member is whoever's mix hears other mixes. The only member-facing global action needs the engineer PIN again.
- In Dante, routing subscribes a receive channel to a transmit channel's name; a renamed channel is a different channel to routing, so renaming an IEM transmit channel drops the stagebox's subscription until it is remade by hand. A receive channel takes at most one source; a transmit channel feeds many.
- Nothing in iemmixer touches Dante today.

## 5. Concepts

- **Set.** One in-ear output: an engine mix with its TX channels (today `member1`…`member9`). Fixed hardware, declared in the site's `[engine]` as today. The engineer's and the translator's mixes are not sets.
- **Input.** As today (mics, instruments, stems, tech). Fixed hardware.
- **Person.** `{id, name}`; PIN and photo keyed by the id. The id never changes; a rename changes the name only. The engineer is not a person in this sense and belongs to no band.
- **Band.** `{id, name, leaders, sets, inputs}`:
  - `sets`: set → person (at most one person per set, a person in at most one set per band);
  - `inputs`: input → `{owner, name}` for the inputs whose owner or label depends on the band (a singer's mic, an instrument); inputs not listed keep their site label and have no owner;
  - `leaders`: people of this band.
- **Main band.** The band whose people name the sets in Dante (G6). One band is marked main.
- **Active band.** Exactly one at any time, persisted; it survives a reboot.

## 6. Data and where it lives

- **Roster file** *(my call)*: a private file beside the site file on the PC (its source kept in `iemmixer-ops`, never in this repo, P6), holding the people and the bands. A synthetic `config/test-roster.toml` in this repo for tests. The site's `[[members]]` and the band-dependent parts of `[[inputs]]` move into it; `[engine]` stays in the site file.
- **Per band and person**: presets, history snapshots, pins/hides. Today's per-member files become band A's.
- **Per person**: PIN hash, photo.
- **Per band**: the band's mixer state (`MixState` plus every member's pins/hides), the same shape as a backup (F19). The active band's live state stays in the engine exactly as today; an inactive band's state is a file the server keeps.
- **Global**: the active band's id; the limiter counters (they measure each output's hearing protection, whoever wears it) *(my call)*.
- **Backups** (F19) cover every band's stored state as well as the live one *(my call)*.

## 7. Switching the band

**Who:** a leader of any band, or the engineer. A leader of the youth band can switch back to the main band even though they do not play in it *(my call)*. The control sits in the page's settings and asks for confirmation naming the target band. The engineer gets a Web Push when a band is switched *(my call)*.

**What happens:**

1. The server checks the right again and refuses a switch while another is running.
2. It captures the live state from the mirror at one revision and stores it as the active band's state.
3. It sends the target band's state to the engine in one `ImportState`, completed with the running values for anything it lacks (as F31 does). A band with no stored state yet starts from a copy of the current state *(my call)*. A set with no person in the target band outputs silence *(my call)*.
4. Only after the engine has acknowledged it does the server record the target as active, switch its roster view, and tell every open page. A person in the new band lands on their own mix; a person not in it sees which band plays (and, if a leader, the switch control); open pages of people whose set changed are re-routed at once.
5. If any step fails, the active band does not change and the caller sees the error. A crash between steps 3 and 4 must leave a consistent pair on restart: the builder defines the order of writes so that the engine's state and the recorded active band can never belong to different bands.

**How it sounds:** no silence and no click. Levels move as a short ramp, like presets (X15, 50 ms) *(my call; the builder may choose another click-free method)*. Nothing restarts.

**Heard-mix levels follow the leader, not the set** *(my call)*: on a set whose wearer is not a leader of the band, every heard-mix level is off, forced when a band's state is copied or a set gets a new wearer. Otherwise a youth member on a leader's set would inherit that leader's Mixes-tab levels without a tab to turn them off.

**Anti-goal check:** between steps 3 and 4 a person's receiver already carries the new band's mix of that set while their page still shows the old band. The server must not accept mix changes from a page of the old band once step 3 has started.

## 8. Leaders and the Mixes tab

- The Mixes tab is shown to the active band's leaders only, listing the other people of the active band by name. The engineer keeps their own Mixes tab over every set and hears each set's whole mix, as F16 has it today; the engineer's mix is not a set, so the rule below does not apply to it.
- The server refuses heard-mix level changes from anyone but a leader of the active band or the engineer.
- **The engine must let two sets hear each other.** Leaders sit on different sets in different bands (in the main band one leader hears the other's set; in the youth band that other leader hears the first one's set, which a youth member borrows). The site's sets therefore need mutual hearing, which today's declare-before-use rule refuses, as the predecessor's REAPER did (reaperiem#121). Requirement: a set may hear any other set, and what it hears of another set is that set's own mix without the mixes *that* set hears. How to compute it is the builder's, within the RT contract at B = 32 (no allocation, rtsan), the CPU budget (p99.9 ≤ 25 % of the period), block-size independence and the parity model (`parity.rs`), which changes only from this design. One way: a second instance of a set's chain, fed only its own inputs and groups, for each set that hears others and is heard by another.
- Which people lead which band is roster data, filled when the bands are set up.

## 9. Changing people and bands (no GUI yet)

- Done by Claude with the owner, through a tool on the dev box that installs a new roster file on the PC *(my call)*. It validates first (ids unique and valid, every set and input exists in the topology, a person in at most one set per band, leaders are members of their band, exactly one main band, the active band exists) and refuses with the reason; a valid roster is applied by the server without an engine restart or maintenance.
- **Add a person:** new id, name, set per band. Their PIN is set by the engineer's PIN reset (F3) or by Claude through the secret provider; never in the roster file.
- **Rename:** name only. The id, PIN, photo and every file stay.
- **Remove:** the person's files move to an archive, never deleted (decision 6); their tokens stop working. **Restore** brings an archived person back with their files.
- **A set's person changes:** that set's mix keeps its state for the band (the new person starts from the old mix, can then make it their own) *(my call)*.
- **A person moves to another set within a band:** their mix goes with them; whoever takes the vacated set starts from its old mix *(my call)*.
- **A login someone else uses today** (a new person logging in under a former member's id): at migration the tool either hands that id's files to the new person or renames in place; the owner picks per person in the migration session.
- **During an event:** a roster install is refused while `EVENT-NOW` exists (the existing "freezes all PC work" rule). A band switch by a leader is a band feature and is never refused for an event: the owner guards live timing.
- **New equipment** (a new in-ear transmitter, a new mic channel) is a topology change: the site file through F30 in a maintenance session, with the owner subscribing the new channel in Dante. Unchanged by this epic.
- Every change the roster makes to the main band's people produces a short note for the FOH side (tracks to rename in the band's Ableton set) for the owner to pass on.

## 10. Dante labels

- Only the IEM card's own labels: TX `NAME inear L 03` / `NAME inear R 04`, RX `03 NAME mic`, the predecessor's convention (number at the end for TX, at the start for RX, 31-character limit). `NAME` is the main band's person on that set or input.
- Labels change only when the main band's person of a set or input changes, never at a band switch.
- A TX rename drops the stagebox's subscription to it. Renames therefore happen only in dev time, with the owner ready to remake those subscriptions in Dante Controller right away; the tool prints the exact list. Sessions never change a subscription.
- The tool that writes labels is dev-time tooling, not a band feature. The builder investigates existing Dante control libraries first (the predecessor used `netaudio`) and where it can run (the IEM PC or another host on the Dante network).

## 11. Migration from today

1. Today's members become people with their ids unchanged, so PINs, photos and tokens survive.
2. Band A (main) = today's sets, adjusted to the current lineup: people who no longer play are archived; new people are added. Its state = the engine's current state; today's presets, history and pins/hides become band A's.
3. Band B (youth) is created from the owner's data; its first state is a copy of band A's.
4. The leaders, the main band and the active band (A) are set.
5. Dante labels are rewritten for the main band in one dev-time session with the owner.
6. The FOH note goes to the owner.

## 12. Constraints

- RT contract at B = 32 / 96 kHz; the engine's parity model and block-size independence.
- P6: names live only in the private roster and ops repo, never in this repo, its CI logs or tickets.
- Prod: a new build reaches the PC by the documented procedure (`.claude/rules/guard.md`), before a Thursday event; before a Sunday service only fixes. Nothing goes to REAPER on its own.
- Dante: only the IEM card; never a subscription.
- Login, PIN hashing, tokens and the login guard as today (`.claude/rules/security-baseline.md`).
- **Two-step rollout.** A build that reads both today's format (`[[members]]` in the site, declare-before-use mixes) and the new one (the roster, mutual hearing) ships and becomes the pin before any data on the PC changes. The previous pin must always be able to run the PC's data (`.claude/rules/engine.md`, persistence: never run a bundle older than the data's format).
- **CPU.** The engine's worst case already reaches 81.6 % of the period at p99.9 on the hosted runner (S3 bench, run 36246425178, `docs/superpowers/plans/2026-09-26-s3-engine-core.md`), and a whole-band switch is that kind of load; mutual hearing adds work, so it is measured on the PC (bench and soak) before it reaches prod.

## 13. Tests

- **Unit:** roster validation; the switch's state machine, including every failure point and a crash between steps; rights (a non-leader cannot switch or move heard-mix levels, a leader cannot open another member's page); per-band presets, history and pins/hides; migration of today's files.
- **Engine:** mutual hearing in `parity.rs` (from this design), RT and rtsan proofs, the bench's worst case with every set hearing every other.
- **E2E (mock, Playwright, zero console errors):** a leader switches the band; a youth member logs in and hears their own mix on the borrowed set; a person not in the active band sees the notice; a non-leader has no switch and no Mixes tab; open pages re-route at a switch.
- **On the PC (live/HIL):** a switch is click-free and silent-free at the outputs; the active band and both bands' states survive a reboot.

## 14. Left to the builder

- The roster file's exact format and the install tool's shape (CLI over ssh, an engineer API, or both).
- The engine technique for mutual hearing (§8), within the stated constraints.
- The write order that keeps the active band and the engine's state consistent across a crash (§7).
- The click-free method of the switch.
- The Dante label library and host (§10).
- UI layout of the switch control, the notice page and the login tiles (the active band's people first is the obvious start).
- How the work splits into tickets and PRs.
