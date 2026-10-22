---
paths:
  - "scripts/iem-pc/iempc_dante.py"
  - "scripts/iem-pc/test_iempc_dante.py"
---

# Dante labels for the main lineup (#58 plan Task 15, design §10)

`iempc dante-labels --roster <ops>/site/roster.toml [--site PATH] [--before PATH]` prints the IEM card's channel labels as a sheet the owner types into Dante Controller. This is **outcome B** of the plan's Step 1 spike: no tool writes a Dante device. `--apply` is not built. The owner runs the spike himself (`netaudio` on a host of the Dante network: read the IEM card's labels, set and restore one label of a channel nothing subscribes, never another device, never a subscription). If it shows the card can be written, the writer plugs into `write_labels(labels)`, the one seam left for it, and gets `apply_touches_only_the_iem_card` as its test.

- **Which labels.** Only labels that name a person:
  - TX: each set the main lineup places someone on, `NAME inear L NN` and `NAME inear R NN`. A set is a stereo mix: a set whose site mix has one TX is refused.
  - RX: each input the main lineup gives an owner, `NN NAME type`. A stereo input gets two rows, `NN NAME type L` and `NN NAME type R`.
  - No row for the engineer's or translator's mixes, HIL's spare outputs, an input without an owner, or a set nobody wears in main. Those keep whatever label the card has.
  - Only the main lineup names the card (design §10, decision 4). A youth member on a borrowed set or mic is iemmixer's business, so a band switch never touches Dante.
- **The convention** is the predecessor's (read from `~/devel/reaperiem`, the naming notes of 2026-02). The number goes last on TX and first on RX, so Dante Controller's matrix lines up; at least two digits (`03`); a label holds at most 31 characters (`LIMIT`).
  - `NAME` is the roster person's name in ASCII capitals. NFKD drops the diacritics; any other non-ASCII letter and Dante's `= . @` are left out; spaces are collapsed.
  - A NAME too long for the label is cut and its row says `shortened: true`. With two-digit channels the budget is 20 characters on TX and 24 on RX.
  - **`type`** (decided here, not in the plan, which writes `mic` for every input): the last word of the main lineup's own label for the input, when it is a short lower-case word (`[a-z]{1,8}`, e.g. `gtr` in `MEMBER4 gtr`). Otherwise it is `mic`, also for a one-word label. So a person's instrument is not labelled `mic`.
- **Resubscription.** Dante subscribes an RX to a TX by the TX channel's name. A renamed TX channel therefore drops the stagebox's subscription until the owner remakes it in Dante Controller (sessions never change a subscription). An RX rename keeps its own subscription.
  - `resubscribe` lists the TX rows whose label differs from the one `--before` gives. `--before` is the roster the card's labels follow now, on the same site: in practice the ops repo's previous `roster.toml`.
  - Without `--before`, the card's labels are unknown (nothing reads the card), so every TX row is listed with `before: null`. Use this for the first labelling after the migration (design §11.5), when the card still holds the predecessor's labels.
  - Renames happen only in dev time, with the owner ready to remake the listed subscriptions at once (design §10).
- **Dev time and locked** (`Spec(pc=False, dev_time=True, locked=True)`): the EVENT-NOW flag refuses even the sheet. `test_apply_refuses_while_event_now` pins that lock for the `--apply` that would sit behind it.
- **P6:** the sheet holds people's names and the site's channel numbers. It is printed on the dev box only and goes to the owner from there. Never commit it, paste it into a ticket, or post it anywhere. Tests use `config/test-roster.toml` and `config/test-site.toml` (TX 71–88, RX 101–132). Names with letters a label cannot hold are built from code points (`chr(0x17D)`), so the test file stays ASCII.
- **What the tool does not check:** it computes labels and refuses only what stops that (no single main lineup, an unknown person, set, mix or input, channels that are not one or two card numbers). Check the roster itself with `iempc roster-install --dry-run`, which runs the server's own check on the PC.
