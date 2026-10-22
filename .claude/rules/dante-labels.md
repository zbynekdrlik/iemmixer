---
paths:
  - "scripts/iem-pc/iempc_dante.py"
  - "scripts/iem-pc/test_iempc_dante.py"
  - "scripts/iem-pc/dante_card.py"
  - "scripts/iem-pc/test_dante_card.py"
---

# Dante labels of the IEM card (#58 plan Task 15, design §10)

`iempc dante-labels --roster <ops>/site/roster.toml [--site PATH] [--before PATH] [--apply]` computes the IEM card's channel labels from the roster. Without `--apply` it prints them as a sheet; with it, it writes them onto the card.

**Outcome A of the Step 1 spike (#62, 2026-10-22, the final comment; it replaces the earlier outcome B):** the dev box writes the card.
- The IEM card and the stagebox sit on a separate, routed subnet. mDNS does not cross to the dev box, but unicast does.
- So netaudio's CLI, which finds devices only through mDNS, cannot see the card. netaudio's library (0.0.10, the predecessor's tool, in the dev box's pipx venv) addresses it directly, by the card's IP from the private env, on the ARC port (4440).
- The spike read the card's name, its channel counts and every label, then set and reset one spare HIL TX label, touching nothing else.

## How the card is reached

- **`scripts/iem-pc/dante_card.py` is the only module that imports netaudio** (`test_only_the_helper_imports_netaudio`). iempc runs it with the venv's python: `<NETAUDIO_PYTHON> -I dante_card.py read <host> <expected-name>`, or `write <host> <expected-name> <EVENT-NOW path>`. iempc_dante imports the helper only for its checks (`unicast_ipv4`, `MAX_CHANNEL`); netaudio is imported only inside `open_device`.
  - `open_device` builds the `DanteDevice` by IP (`ipv4`, `services = {"arc": …}`, no mDNS). It is the one place netaudio is imported, and the place tests inject a fake device. `OpenDeviceTests` runs the real one against stub netaudio modules.
  - **A read is complete or refused** (`connect`): a name, the expected name, then as many TX and RX channels as the card reports, more than none. netaudio's `get_controls` ends with `error = None` and a timed-out reply sets nothing, so silence shows only as an empty read ("the card did not answer"), and a lost page as "incomplete read". The test fake behaves the same.
  - `read` prints one JSON line: name, counts, every TX/RX label (`friendly_name or name`).
  - `write` takes `[{type, channel, label}]` on stdin (a channel 1 to 255, `MAX_CHANNEL`). The device must give a complete read and name itself as expected before anything is written; else nothing is written (exit 1). A label equal to the channel's number is `reset_channel_name`, any other `set_channel_name`.
  - **Once writing has started it always ends with one JSON line, exit 0:** `sent` (each change whose command returned), `failed` (the one whose command raised; netaudio's commands catch only a timeout, so a refused connection raises), `error`, `read_back`, `mismatches` (sent or failed changes that did not read back) and the labels of a fresh read (`tx`, `rx`, null when not read back). The EVENT-NOW path is checked before each label: when it appears, the writes stop and the rest is read back.
  - It calls only `get_controls`, `set_channel_name` and `reset_channel_name`: never a subscription, latency, sample-rate, encoding, gain or device-name command, nor a raw command or socket. `test_the_helper_calls_no_command_but_its_reads_and_its_label_writes` greps the source for those names; do not even name them in its comments.
  - netaudio prints its own errors on stdout; the helper sends them to stderr, so stdout holds one JSON line.
  - A refusal never prints the device name the card answered with.
- **The run** (`iempc_dante.helper`): its own process on the dev box, stdout and stderr to `dante-helper.out` / `.err` in iempc's state dir, bounded by `CARD_S` (120 s) and never ended by force.
  - A run past the bound goes on by itself: `StillRunning` names its pid and where its answer goes. Its pid is in `dante-helper.json`, and no other run starts while that pid is still a run of `dante_card.py` from any checkout (`/proc/<pid>/cmdline`, matched by the file name: the event checkout shares the state dir and lock; a reused pid does not count).
  - An OSError at start is a StepError that does not name the path.
- **The private env** (`~/.config/iemmixer/iem-pc.env`; none of the values is in this repo):
  - `DANTE_IEM_HOST`: the card's IP, a unicast IPv4 address (no host name, IPv6, multicast, broadcast, loopback or unspecified address; a link-local one is allowed, Dante uses it without DHCP).
  - `DANTE_IEM_NAME`: the expected Dante device name.
  - `NETAUDIO_PYTHON`: the venv's python, the full path of an executable file.
  - `--apply` refuses unless all three are set and well-formed, naming the key, never its value.
- **No PC call, no other device, no subscription.** The owner remakes the stagebox's subscriptions to renamed TX channels in Dante Controller by hand.

## Which labels

- **Which labels.** Only labels that name a person:
  - TX: each set someone wears, `NAME inear L NN` and `NAME inear R NN`. A set is a stereo mix: a set whose site mix has one TX is refused.
  - RX: each input someone owns, `NN NAME type`. A stereo input gets two rows, `NN NAME type L` and `NN NAME type R`.
- **Who names a set or an input** (the coordinator's decision on #62): its main-lineup wearer or owner first.
  - When the main lineup leaves it empty, it is named by the one person who wears or owns it across the other lineups. The row's `lineup` says which lineup (the first that names that person); this is the case of a set worn only in the youth lineup.
  - Nobody, or different people, means no row: the card keeps its label.
  - The labels follow the roster, never the lineup that plays (design §10, decision 4), so a band switch never touches Dante.
  - No row either for the engineer's or translator's mixes, HIL's spare outputs, or an input nobody owns.
- **The convention** is the predecessor's (read from `~/devel/reaperiem`, the naming notes of 2026-02). The number goes last on TX and first on RX, so Dante Controller's matrix lines up; at least two digits (`03`); a label holds at most 31 characters (`LIMIT`).
  - `NAME` is the person's name in ASCII capitals (`plain`, then `caps`): NFKD drops the diacritics; any other non-ASCII letter, a control character and Dante's `= . @` are left out; spaces are collapsed.
  - A NAME too long for the label is cut and its row says `shortened: true`. With two-digit channels the budget is 20 characters on TX and 24 on RX.
  - **`type`** (decided here, not in the plan, which writes `mic` for every input): the last word of the naming lineup's own label for the input, folded like NAME and lower-cased (`plain`, the review of Part D), when it is then a short word (`[a-z]{1,8}`: `gtr` in `MEMBER4 gtr`, `vocals` in `MEMBER2 Vocals`). Otherwise, and for a one-word label, it is `mic`. So a person's instrument is not labelled `mic`.

## The sheet and `--apply`

- **Resubscription.** Dante subscribes an RX to a TX by the TX channel's name. A renamed TX channel therefore drops the stagebox's subscription until the owner remakes it in Dante Controller (sessions never change a subscription). An RX rename keeps its own subscription.
- **The sheet** (no `--apply`): `resubscribe` lists the TX rows whose label differs from what the card holds.
  - `compared: "card"`: the card was read now; this happens whenever the env names it.
  - `"before"`: the `--before` roster's labels, the offline fallback when the env does not name the card or its read fails (a stderr WARNING).
  - `null`: nothing known, every TX row listed.
- **`--apply`:**
  1. It reads the card. A channel above the card's count, or one its read did not list, refuses, with nothing written. A read whose counts are not its labels' is no read.
  2. It writes only the labels that differ from what the card holds. When none differs it says so and exits 0 without a write.
  3. It reads the flag again right before the write: an "ide event" since the start refuses, with nothing written. The helper then checks it before each label.
  4. It prints one `{"dante_apply": {changed, unchanged, confirmed, written, unsent, mismatches, resubscribe, error}}`. Each `written` entry and each `resubscribe` entry carries its label and what it was before.
- **The report comes from the card, not from the plan** (the safety review on #62). A rerun cannot rebuild it, since the card then holds the new labels: so a failed write run must still print it.
  - With the helper's answer and a read back (`confirmed: true`): `written` is what the card took. `unsent` is what the writes never reached (a write that raised, or the flag). `resubscribe` is the TX labels the card changed; a TX label that landed as another label is listed with that label and `mismatch: true`, and one that did not change is dropped.
  - Not read back: `written` and `resubscribe` are what was sent, `confirmed: false`.
  - A write run that fails, outlives its bound (`StillRunning`) or answers no report of its writes: the planned labels and subscriptions, `confirmed: false`, `unsent` and `mismatches` null. Then it fails.
  - Exit 0 only when every label was taken and read back, with no error.
- **Dev time and locked** (`Spec(pc=False, dev_time=True, locked=True)`): the EVENT-NOW flag refuses the sheet and `--apply` (`test_apply_refuses_while_event_now`, the plan's name, covers both the start and the read right before the write; `test_the_flag_during_the_writes_stops_them` the helper's checks). Renames happen only in dev time, with the owner ready to remake the listed subscriptions at once (design §10; plan Task 17 Step 5).
- **P6:** the sheet and `--apply`'s report hold people's names and the site's channels. They are printed on the dev box only and go to the owner from there. Never commit them, paste them into a ticket, or post them anywhere.
  - Tests use `config/test-roster.toml` and `config/test-site.toml` (TX 71–88, RX 101–132), a documentation IP (`192.0.2.10`) and an invented device name.
  - Names with letters a label cannot hold are built from code points (`chr(0x17D)`), so the test files stay ASCII.
  - No card exists in CI: `test_iempc_dante.ApplyTests` fakes the helper's subprocess (`iempc_dante.helper`), `HelperRunTests` runs the real `helper` with a stand-in program for the netaudio python, and `test_dante_card` runs the helper with a fake device.
- **What the tool does not check:** it computes labels and refuses only what stops that (no single main lineup, an unknown person, set, mix or input, channels that are not one or two card numbers from 1 to 255). Check the roster itself with `iempc roster-install --dry-run`, which runs the server's own check on the PC.
