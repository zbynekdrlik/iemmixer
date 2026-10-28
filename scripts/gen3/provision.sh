#!/usr/bin/env bash
# The remote commands name this script's own constant paths, expanded here on purpose.
# shellcheck disable=SC2029
set -euo pipefail
# Provisions the Gen 3 box from the dev box, then checks it (#72 phase D, plan
# docs/superpowers/plans/2026-10-26-gen3-phase-d-the-box.md Task 9, choice 1;
# .claude/rules/gen3.md):
#
#   provision.sh --host <user@host> --ntp-from <iemmixer-ops/site/gen3-dantesync.json>
#                [--band-roster <iemmixer-ops/site/roster.toml>]
#
# - Dev time only: refused while the "ide event" flag exists
#   ($IEMMIXER_EVENT_NOW, else ~/.config/iemmixer/EVENT-NOW), at the start and
#   again right before the root script runs.
# - Reads ntp_server from the ops dantesync config (the box's dantesync must
#   be installed from it: install-dantesync.sh, run first) and the [[people]]
#   ids of this checkout's config/test-roster.toml (the sudo rule's
#   {member}), and with --band-roster the band roster's ids too, each once
#   (while 3.x serves the live gate mints a band member's token); no value is
#   ever printed.
# - On the box: `id -un` (the admin user, the sudo rule's; never root) and
#   `systemctl is-active iem-server iem-engine iem-dante`: a unit of the stack
#   that is not inactive or failed refuses the run before anything is copied
#   (provision never touches a running stack).
# - One scp into ~/.cache/iem-gen3-provision (overwritten, never deleted):
#   provision-root.sh, sudo-lines.txt, iem-napi.service, napi-priority and the
#   two values as files (ntp-server, roster-ids); then
#   `sudo -n bash .cache/iem-gen3-provision/provision-root.sh --admin <user>
#   --ntp-file ... --roster-file ...`, the steps' lines on stdout. That call
#   runs on the admin's own passwordless rule (phase D's test box; the rule
#   provision writes from sudo-lines.txt becomes the only one at phase G), so
#   it has no line in sudo-lines.txt.
# - Then verify_box.py on the box (`python3 -` on ssh's stdin, nothing left
#   there): its line is printed, and its exit is this script's (0 only when
#   every check passes; each failing check is named on stderr). Before the
#   first phase D deploy verify names `units` (phase C's units, or none, until
#   deploy.sh installs the service user's) and, on a box no deploy has
#   written the Dante interface on, `napi`.
#
# Any failure stops the script: exit 1, 2 for its command line. Site values
# (host, user, NTP server, interface) come only from the arguments and the
# private files; none is in this repository (P6), and none is printed.

STAGE=.cache/iem-gen3-provision
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10)
FILES=(provision-root.sh sudo-lines.txt iem-napi.service napi-priority)

die() {
  echo "provision.sh: $*" >&2
  exit 1
}

usage() {
  echo "provision.sh: $*" >&2
  echo "usage: provision.sh --host <user@host> --ntp-from <the ops dantesync config> [--band-roster <the ops roster>]" >&2
  exit 2
}

on_box() {
  ssh "${SSH_OPTS[@]}" "$host" "$@"
}

host="" ntp_from="" band_roster=""
while [ $# -gt 0 ]; do
  case "$1" in
    --host | --ntp-from | --band-roster)
      [ $# -ge 2 ] || usage "$1 needs a value"
      case "$1" in
        --host) host="$2" ;;
        --ntp-from) ntp_from="$2" ;;
        --band-roster) band_roster="$2" ;;
      esac
      shift 2
      ;;
    *) usage "unknown argument '$1'" ;;
  esac
done
[ -n "$host" ] || usage "--host is missing"
[ -n "$ntp_from" ] || usage "--ntp-from is missing"
case "$host" in -*) usage "--host must be an ssh destination, not an option" ;; esac
[ -f "$ntp_from" ] || usage "--ntp-from is not a file"
[ -z "$band_roster" ] || [ -f "$band_roster" ] || usage "--band-roster is not a file"

flag="${IEMMIXER_EVENT_NOW:-$HOME/.config/iemmixer/EVENT-NOW}"
refuse_event() {
  [ ! -e "$flag" ] || die "$flag exists: an event is on; the Gen 3 box is provisioned only in dev time ($1)"
}
refuse_event "nothing was sent"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
roster="$repo/config/test-roster.toml"
verify="$here/verify_box.py"
for name in "${FILES[@]}"; do
  [ -f "$here/$name" ] || die "$here/$name is missing"
done
[ -f "$verify" ] || die "$verify is missing"
[ -f "$roster" ] || die "$roster is missing"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

# The NTP server the box's dantesync must follow (never printed).
why="$(python3 -I - "$ntp_from" "$work/ntp-server" << 'PY'
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as f:
        config = json.load(f)
except (OSError, ValueError) as e:
    print(f"is not JSON ({type(e).__name__})")
    sys.exit(0)
server = config.get("ntp_server") if isinstance(config, dict) else None
if not isinstance(server, str) or not server.strip() or "\n" in server:
    print("has no ntp_server (the fleet's NTP master)")
    sys.exit(0)
with open(sys.argv[2], "w", encoding="utf-8") as f:
    f.write(server.strip() + "\n")
print("")
PY
)" || die "--ntp-from could not be read (python3 above)"
[ -z "$why" ] || usage "--ntp-from $why"

# The rosters' [[people]] ids, each a plain member id (the sudo rule's
# {member}, as deploy.sh --pins reads them), the test roster's first, each
# id once.
python3 -I - "$work/roster-ids" "$roster" ${band_roster:+"$band_roster"} << 'PY' ||
import re, sys, tomllib
ids = []
for path in sys.argv[2:]:
    with open(path, "rb") as f:
        roster = tomllib.load(f)
    people = [person.get("id") for person in roster.get("people", [])]
    if not people or not all(isinstance(i, str) and re.fullmatch(r"[A-Za-z0-9_-]{1,64}", i) for i in people):
        sys.exit(1)
    ids += [i for i in people if i not in ids]
with open(sys.argv[1], "w", encoding="utf-8") as f:
    f.write("".join(i + "\n" for i in ids))
PY
  die "the test roster or --band-roster: its [[people]] ids could not be read"

# The admin user (never printed) and the stack's state.
admin="$(on_box id -un)" || die "ssh to the box failed (id -un): nothing was sent"
[[ "$admin" =~ ^[a-z_][a-z0-9_-]{0,31}$ ]] || die "the box's ssh user is not a plain user name: nothing was sent"
[ "$admin" != root ] || die "--host logs in as root: provision runs as the box's admin user through sudo: nothing was sent"
read_exit=0
states="$(on_box systemctl is-active iem-server iem-engine iem-dante)" || read_exit=$?
[ "$read_exit" -ne 255 ] || die "the stack's states could not be read: nothing was sent"
mapfile -t state_lines <<< "$states"
[ "${#state_lines[@]}" -eq 3 ] || die "systemctl is-active did not name the three units' states: nothing was sent"
i=0
for unit in iem-server iem-engine iem-dante; do
  case "${state_lines[$i]}" in
    inactive | failed) ;;
    *) die "$unit is '${state_lines[$i]}': provision never touches a running stack; stop it first (sudo systemctl stop iem-server iem-engine iem-dante on the box): nothing was sent" ;;
  esac
  i=$((i + 1))
done

on_box mkdir -p "$STAGE"
sources=()
for name in "${FILES[@]}"; do
  sources+=("$here/$name")
done
scp -q "${SSH_OPTS[@]}" "${sources[@]}" "$work/ntp-server" "$work/roster-ids" "$host:$STAGE/"
echo "provision.sh: copied to the box's ~/$STAGE"

refuse_event "copied to ~/$STAGE, nothing changed on the box"
on_box "sudo -n bash $STAGE/provision-root.sh --admin $admin --ntp-file $STAGE/ntp-server --roster-file $STAGE/roster-ids" ||
  die "provision-root.sh failed on the box (above): the steps before it stay as they are; run provision again once the cause is fixed"

verify_exit=0
on_box python3 - < "$verify" || verify_exit=$?
[ "$verify_exit" -ne 255 ] || die "ssh to the box failed while verify ran"
if [ "$verify_exit" -ne 0 ]; then
  echo "provision.sh: verify_box.py found the box not as it should be (exit $verify_exit; the checks named above)" >&2
  exit "$verify_exit"
fi
echo "provision.sh: the box is provisioned and verified"
