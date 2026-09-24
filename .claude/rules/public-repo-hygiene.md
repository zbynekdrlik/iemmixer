---
paths:
  - "scripts/**"
  - "config/**"
  - "docs/**"
  - "e2e/**"
  - ".gitleaks.toml"
  - "README.md"
  - "CLAUDE.md"
---

# Public-repo hygiene (program spec P6)

- This repository is public. Site data never enters it: band member names, host names, IPs, Dante device names and channel numbers, real track names, PINs, keys, tokens, user paths, the REAPER project name. Real values live only in the private `zbynekdrlik/iemmixer-ops`.
- Placeholders: `member1`…`member9` (MEMBER_1…MEMBER_9; `member1` = ELEVATED_MEMBER), `engineer`, TRANSLATOR; hosts `mixer.example.org`, LAN `http://10.0.0.10`; synthetic Dante numbers (member N = 69+2N/70+2N, i.e. 71–88; any other output 89/90; engineer 91/92; inputs 101–124); input names from program spec §3.1 where the code does not key on a REAPER-era name.
- **Every clone installs the pre-push hook** (S0 plan, Task 1 Step 9): gitleaks + `scripts/denylist_scan.py` with the private denylist `~/.config/iemmixer/denylist.txt` and `scripts/allowed-identities.txt`. CI repeats all three in the `secrets` job (`DENYLIST` secret). The denylist's non-credential terms are also kept in the ops repo; its credential entries only on the box and in the secret.
- **Scanner output never shows a term or an address** — only `path:line: denylist entry N` or `<sha> author|committer email is not an allowed identity`; open the local denylist to see entry N.
- **Allowlist** `scripts/denylist-allow.txt`: sha256 keys of single lines that are ordinary text equal to a denylisted term: `python3 scripts/denylist_scan.py --hash <path> <line>`, with a neutral reason ("reviewed ordinary prose, not site data") that never names or hints at the term. Prefer rewording the line. Never allowlist a real private value; never blanket-allow a term.
- **Importing more predecessor code** (S1b/S4/S5): `git -C ~/devel/reaperiem archive <pinned SHA> -- <paths>` into a temp dir, `python3 scripts/scrub_import.py --map ~/.config/iemmixer/scrub-map.tsv --root <dir> --report <tmp>`, rewrite Dante channel literals to the synthetic map and run the Dante gate (S0 plan, Task 2 Step 8), then scan the staged tree (`git write-tree` → `--tree`) before committing; add the paths to `docs/provenance/import-manifest.txt`.
- Commits use the GitHub noreply identity only (`scripts/allowed-identities.txt`); `gh pr merge` passes `--author-email` with it.
