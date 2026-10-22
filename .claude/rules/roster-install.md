---
paths:
  - "scripts/iem-pc/iempc_roster.py"
  - "scripts/iem-pc/test_iempc_roster.py"
---

# `iempc roster-install` (#58 plan Task 14, design §9)

`iempc roster-install --file <ops>/site/roster.toml [--dry-run]` is how a roster reaches the IEM PC. The server polls `roster.toml` beside its config every 2 s and applies a valid change live (`.claude/rules/server-engine.md`, roster reload), so the tool checks, places and follows the file, and takes a roster the server does not apply back out. The module docstring of `iempc_roster.py` lists the steps. What the code does not show (the review of Part D on #62 shaped most of it):

- **Dev time, locked, a PC command** (`Spec(pc=True, dev_time=True, locked=True)`): the EVENT-NOW flag refuses it (design §9), and a flag that appears during a call runs the event path (exit 10). The flag is read again right before the install call, because `guarded` notices a flag that predates a call only at its end.
- **A server must be able to apply it.** `iemmode status` must answer ok, in dev or live, with no switch and no HIL job (`guard_refusal`; a dry run too); in event no server runs.
- **One source of the server's folder: the guard's pc.toml.** It lives at `%LOCALAPPDATA%\iemmixer\guard\pc.toml` (`site::pc_toml_path`), and its `root` is the folder `PC_ROOT` names. Its `server_config` is the config the guard starts the server with (`win/procs.rs`, `IEMMIXER_CONFIG`). It is read over ssh and parsed on the dev box (`server_config`); missing, unreadable, not TOML or not a full `X:\…\file` path refuses before the upload. There is no env key for it.
- **The check runs the active bundle's own server, admin-only.** The active bundle is the status detail's `bundle <sha>`. Its `iem-server.exe` is read once from `bundles\<sha>`, checked by this box's fetch record, and run from the admin-only stage (`iempc_bin.staged_program`; the #15 rule: the elevated ssh session runs only admin-only copies).
  - So a bundle not fetched on this box refuses (`iempc fetch-bundle --sha <active>`), and a bundle from before Part B answers exit 2 (no `roster check`).
  - The check's answer is judged in Python (`check_result`): the exit code, `ok`, `problems` and the file's sha256 must agree.
- **The install, one change call.**
  - It reads the status as it stands (`before`) and the sha256 of `lineups\applied-roster.toml`.
  - It keeps the existing `roster.toml` as `roster.toml.prev` and names its sha256 (`prev`; null when there was none).
  - It copies the upload to `roster.toml.new` and puts it in place with `[IO.File]::Replace(new, dst, $null)`, so there is never a moment without a `roster.toml` (Windows PowerShell 5.1's `Move-Item -Force` deletes, then renames). A plain `Move-Item` is used only when no `roster.toml` exists.
  - Each file is checked by its sha256 at each step.
- **Reads share the server's files.** The server replaces `roster.status.json` and `applied-roster.toml` by a tmp-then-rename (`lib.rs` `atomic_write`, `write_durable`). A handle without delete sharing would make that rename fail. So every read of those two files (and of pc.toml) opens with `[IO.File]::Open(p, 'Open', 'Read', 'ReadWrite, Delete')` (`read_shared`, `sha_shared`), never `ReadAllText`, `Get-Content` or `Get-FileHash`; `hash_check`'s `Get-FileHash` reads only the tool's own files (the upload, `.new`, `.prev`, `roster.toml`, which the server never replaces). A read that fails in the instant of a rename counts as "not now".
- **What `roster.status.json` can say** (`roster_watch::Status`, `outcome`):
  - `applied: true`: in use (exit 0). `ok: false` with `problems` names what failed after it went into use, such as an archive move.
  - `applied: false`: refused (exit 1). Its problems end with "in use instead: the roster applied last" while the server runs on its fallback.
  - The server writes **no** status for a Deferred roster (an engine not synced, a switch a restart cut short; it reads the file again every 2 s) or for the roster already in use. The wait (`WAIT_S` 20 s, a read every `POLL_S`) then ends "not applied within 20 s", naming what the status held (exit 1).
  - When the applied roster's sha256 is the upload's, the result is `unchanged` at once, whatever a later refusal wrote into the status.
  - The same refusal as `before` (these bytes refused earlier) is waited out; the same applied status as `before` is `unchanged` too.
- **A roster that was not applied goes back out** (`restore`, a second change call, finished on a flag). Without this, a refused roster would wait in `roster.toml` for the server's next start, which fails when no roster was applied before it (`roster_watch::load`; plan Task 17 Step 4 is that case).
  - The restore requires that `roster.toml` still holds the upload. With `prev` it checks the kept file, copies it to `roster.toml.new`, replaces `roster.toml` with it and checks the result. Without one it removes `roster.toml`.
  - The output says `restored: true`, or `restore_failed` with why. An install answer whose `prev` is no sha256 never removes anything: it is a named failure.
  - Known limit: a flag during the wait runs the event path at once, with no restore.
- **Not during a soak or a live run of this dev entry** (`iempc_live.refuse_while_running`, `RUNNING["roster-install"]`; a dry run is not refused): a reload announces the band, which closes every person's mixer socket with 4003, the soak client's and the live run's included.
- **P6:** the output is ids and counts only. `added` and `removed` are filtered to id characters (anything else becomes `?`), and the server's problems already name ids only. The file goes to scp by its absolute path. The file itself holds names: it lives in the private ops repo and on the PC, never here; pc.toml's paths stay on the dev box.
- **Tests:** `test_iempc_roster.py` on `iempc_test_support.Base`.
  - `FakePc.replies` answer the check.
  - `FakePc.texts` answer the four module bodies by their markers: `$iemToml` (the pc.toml read), `installed = $true` (the install), `'restored'` (the restore) and `$iemStatus` (a status read). Each marker appears only in its own body.
  - `RosterInstallGuardTests` (`test_iempc_live.RunningGuardCases`) covers the soak and live-run refusals.
  - The windows job runs the composed install, restore and status bodies against a temp folder: `Test-IemRoster.ps1`, started by `Test-IemPc.ps1`.
