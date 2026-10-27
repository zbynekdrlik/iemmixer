#!/usr/bin/env bash
# The remote commands name this script's own constant paths, expanded here on purpose.
# shellcheck disable=SC2029
set -euo pipefail
# Puts a CI Gen 3 bundle on the Gen 3 box into one of two slots and starts the
# stack from it: the engine with our Dante device inside it and the server, or
# the standalone device (#72 phase C, plan Task 6; phase D, plan
# docs/superpowers/plans/2026-10-26-gen3-phase-d-the-box.md Task 7, choice 6;
# .claude/rules/gen3.md):
#
#   deploy.sh --host <user@host> --bundle <the downloaded gen3-bundle-<sha>>
#             --sha <its commit, 40 hex> --config <the private device config>
#             --iface <the box's Dante interface> [--start engine|device] [--pins]
#   deploy.sh --host <user@host> --back --sha <the build in the other slot>
#             --config <...> --iface <...> [--start engine|device]
#
# - One commit: this checkout must be exactly --sha with a clean tree (`git
#   rev-parse HEAD`, `git status --porcelain`): the units, the test site and
#   the roster come from it, the binaries from that commit's CI bundle.
# - Dev time only: refused while the "ide event" flag exists (iempc's
#   EVENT-NOW: $IEMMIXER_EVENT_NOW, else ~/.config/iemmixer/EVENT-NOW),
#   checked again before the new slot is filled, before the stack is stopped,
#   right before the start and once more right after it: a flag that came
#   meanwhile ("ide event"'s stop may have run before the start) stops all
#   three units again and fails the deploy.
# - The bundle (CI's gen3-bin job uploads gen3-bundle-<sha>, its gen3-attest
#   job attests the bundle's SHA256SUMS by digest): BUILD is --sha and a
#   newline, SHA256SUMS lists exactly the four binaries and BUILD and holds for
#   them, and `gh attestation verify` finds this repository's CI attestation
#   of that SHA256SUMS, all on the dev box before anything is sent; the sums
#   again on the box after the copy and in the slot.
# - "ide event" stops the stack with `ssh <GEN3_HOST> sudo -n systemctl stop
#   iem-server iem-engine iem-dante`: a --host that is not the GEN3_HOST of
#   iempc's private env ($PC_ENV, else ~/.config/iemmixer/iem-pc.env) is a
#   warning, an env without GEN3_HOST too; the box's sudo must allow that stop
#   (`sudo -n -l`), else refused before anything is copied.
# - Two slots: /opt/iemmixer/slot-a/bin and slot-b/bin, and /opt/iemmixer/bin
#   a link to the one the stack runs from (`readlink`). A deploy fills the
#   other one (a, while bin is no link) with the four binaries (0755: a
#   downloaded artifact loses the executable bit), BUILD and SHA256SUMS and
#   checks its sums while the stack keeps running; only then the stack is
#   stopped and the link moved in one rename (`ln -sfn` to bin.next, `mv -T`
#   over bin), so the build before stays untouched in its slot. Phase C's
#   real bin folder is moved aside once to /opt/iemmixer/bin.phase-c (never
#   deleted, never moved twice). --back moves the link back to the other slot
#   without a bundle, after checking that its BUILD is --sha and its sums hold
#   on the box; only the units and the configs are copied then.
# - The stop: none of the three units may still run after it (`systemctl
#   is-active`: inactive or failed); a unit no deploy has installed yet is
#   "not loaded", so the stop's own exit is not the verdict.
# - The four units beside this script (the engine, the server, the standalone
#   device and the alarm unit, each under the service user `iemmixer` of its
#   own), the config, the interface (written as /etc/iemmixer/iem-dante.env)
#   phase E's receiver of the band's data (band_receive.py, into
#   /opt/iemmixer/lib, root's, 0755; #72), and this checkout's public test
#   site and roster go by scp into
#   ~/.cache/iem-gen3-deploy on the box (overwritten, never deleted); `sudo -n
#   install` puts the units, the config and the env in place, makes
#   /var/lib/iemmixer/site the service user's (0700) and puts the site and the
#   roster there as the service user's (0600).
# - --pins (the server is stopped): `iem-server pin set-engineer` and `pin
#   set-member <id>` for every [[people]] id of the roster, each through
#   `systemd-run` as the service user with the PIN pepper's systemd credential,
#   each PIN (GEN3_ENGINEER_PIN, GEN3_MEMBER_PIN of the private env, 4 digits
#   each) only on that ssh's stdin: never on a command line, never printed.
# - `sudo -n systemctl daemon-reload`, `disable iem-dante iem-engine
#   iem-server` (none starts at boot; an earlier deploy's enable is undone),
#   `reset-failed` of the units systemd holds (the start limit counts manual
#   starts too: a deploy is a deliberate new start), then `start iem-engine
#   iem-server` (--start engine, the default) or `start iem-dante` (--start
#   device; the units' Conflicts= keep it and the engine apart).
# - Read back: the binaries and BUILD through /opt/iemmixer/bin against the
#   slot's SHA256SUMS, `iem-engine --version` and `iem-dante-dev --version`
#   naming --sha, then `/opt/iemmixer/bin/iem-dante-dev health` once a second
#   until it exits 0 (healthy), at most GEN3_SERVER_WAIT_S (30) tries; its last
#   line is printed, and a stack not healthy in time fails the deploy.
# - Every `sudo -n` line this script sends is a line of sudo-lines.txt (with
#   its placeholders), the template plan Task 9's provision turns into the
#   box's sudoers file (test_deploy.py checks it).
#
# Any failure stops the script: exit 1, 2 for its command line. Site values
# (host, user, interface, names, PINs) come only from the arguments, the
# private config and the private env; none is in this repository (P6), and
# none is printed.

STAGE=.cache/iem-gen3-deploy
OPT=/opt/iemmixer
BINDIR=$OPT/bin
PHASE_C=$OPT/bin.phase-c
NEXT=$OPT/bin.next
UNITDIR=/etc/systemd/system
CONFIG=/etc/iemmixer/dante-device.toml
ENVFILE=/etc/iemmixer/iem-dante.env
SITEDIR=/var/lib/iemmixer/site
SITE=$SITEDIR/iemmixer.toml
ROSTER=$SITEDIR/roster.toml
SERVICE_USER=iemmixer
CREDENTIAL=/etc/credstore.encrypted/iemmixer-pepper
GITHUB_REPO=zbynekdrlik/iemmixer
SIGNER=$GITHUB_REPO/.github/workflows/ci.yml
BINARIES=(iem-engine iem-server iem-dante-dev iem-soakclient)
UNITS=(iem-dante.service iem-engine.service iem-server.service iem-alarm@.service)
RECEIVER=band_receive.py
LIBDIR=/opt/iemmixer/lib
# The stack's stop, exactly as "ide event" and the live guard run it (plan
# Task 7) and as the box's sudoers rule allows it.
STOP="systemctl stop iem-server iem-engine iem-dante"
# The PIN commands: the server's own binary as the service user, with the
# pepper's systemd credential and the site, the PIN on stdin (`--pipe`).
PIN_RUN="systemd-run --quiet --pipe --wait --collect --uid=$SERVICE_USER --gid=$SERVICE_USER --property=LoadCredentialEncrypted=pepper:$CREDENTIAL --setenv=IEMMIXER_CONFIG=$SITE $BINDIR/iem-server pin"
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10)

die() {
  echo "deploy.sh: $*" >&2
  exit 1
}

warn() {
  echo "deploy.sh: WARNING: $*" >&2
}

# $1 without its leading and trailing white space.
trim() {
  local s="$1"
  s="${s#"${s%%[![:space:]]*}"}"
  s="${s%"${s##*[![:space:]]}"}"
  printf '%s' "$s"
}

# Sets env_value to the value of the key $2 in the env file $1, read as
# iempc's load_env reads it (comments and blank lines skipped, KEY=VALUE split
# at the first '=', both stripped, the value's quotes stripped, the last line
# of a key winning); fails when the file has none. Never prints the value.
env_value_of() {
  local line key value found=1
  while IFS= read -r line || [ -n "$line" ]; do
    line="$(trim "$line")"
    case "$line" in '' | '#'*) continue ;; esac
    case "$line" in *=*) ;; *) continue ;; esac
    key="$(trim "${line%%=*}")"
    [ "$key" = "$2" ] || continue
    value="$(trim "${line#*=}")"
    while [[ "$value" == \"* ]]; do value="${value#\"}"; done
    while [[ "$value" == *\" ]]; do value="${value%\"}"; done
    env_value="$value"
    found=0
  done < "$1"
  return "$found"
}

usage() {
  echo "deploy.sh: $*" >&2
  echo "usage: deploy.sh --host <user@host> --bundle <dir> --sha <40 hex> --config <path> --iface <name> [--start engine|device] [--pins]" >&2
  echo "       deploy.sh --host <user@host> --back --sha <40 hex> --config <path> --iface <name> [--start engine|device]" >&2
  exit 2
}

# One command line on the box.
on_box() {
  ssh "${SSH_OPTS[@]}" "$host" "$@"
}

host="" bundle="" sha="" config="" iface="" start=engine pins=0 back=0
while [ $# -gt 0 ]; do
  case "$1" in
    --host | --bundle | --sha | --config | --iface | --start)
      [ $# -ge 2 ] || usage "$1 needs a value"
      case "$1" in
        --host) host="$2" ;;
        --bundle) bundle="$2" ;;
        --sha) sha="$2" ;;
        --config) config="$2" ;;
        --iface) iface="$2" ;;
        --start) start="$2" ;;
      esac
      shift 2
      ;;
    --pins)
      pins=1
      shift
      ;;
    --back)
      back=1
      shift
      ;;
    *) usage "unknown argument '$1'" ;;
  esac
done
if [ "$back" = 1 ]; then
  required=(host sha config iface)
else
  required=(host bundle sha config iface)
fi
for name in "${required[@]}"; do
  [ -n "${!name}" ] || usage "--$name is missing"
done
if [ "$back" = 1 ]; then
  [ -z "$bundle" ] || usage "--back takes no --bundle: it goes back to the build the other slot holds"
  [ "$pins" = 0 ] || usage "--back takes no --pins: going back leaves the PINs as they are"
fi
case "$host" in -*) usage "--host must be an ssh destination, not an option" ;; esac
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || usage "--sha is not 40 lowercase hex digits"
[[ "$iface" =~ ^[A-Za-z0-9_.-]{1,15}$ ]] || usage "--iface is not an interface name"
case "$start" in engine | device) ;; *) usage "--start is engine or device" ;; esac
if [ "$back" = 0 ]; then
  [ -d "$bundle" ] || usage "--bundle is not a folder"
  for name in "${BINARIES[@]}" BUILD SHA256SUMS; do
    [ -f "$bundle/$name" ] || usage "--bundle has no $name: not a gen3 bundle"
  done
fi
[ -f "$config" ] || usage "--config is not a file"
tries="${GEN3_SERVER_WAIT_S:-30}"
[[ "$tries" =~ ^[1-9][0-9]{0,2}$ ]] || usage "GEN3_SERVER_WAIT_S is not a number of tries"

flag="${IEMMIXER_EVENT_NOW:-$HOME/.config/iemmixer/EVENT-NOW}"
refuse_event() {
  [ ! -e "$flag" ] || die "$flag exists: an event is on; the Gen 3 stack is deployed only in dev time ($1)"
}
refuse_event "nothing was copied"

# iempc's private env: the host "ide event" stops and, with --pins, the PINs
# (values never printed: this output may be pasted).
pc_env="${PC_ENV:-$HOME/.config/iemmixer/iem-pc.env}"
if [ -f "$pc_env" ]; then
  env_value=""
  if ! env_value_of "$pc_env" GEN3_HOST; then
    warn "iempc's private env has no GEN3_HOST: \"ide event\" stops no Gen 3 stack; add GEN3_HOST=<this --host> to it"
  elif [ "$env_value" != "$host" ]; then
    warn "--host is not the GEN3_HOST of iempc's private env: \"ide event\" stops the stack on GEN3_HOST, not on this host; make them the same"
  fi
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
test_site="$repo/config/test-site.toml"
test_roster="$repo/config/test-roster.toml"
for unit in "${UNITS[@]}" "$RECEIVER"; do
  [ -f "$here/$unit" ] || die "$here/$unit is missing"
done
for path in "$test_site" "$test_roster"; do
  [ -f "$path" ] || die "$path is missing"
done

engineer_pin="" member_pin="" people=()
if [ "$pins" = 1 ]; then
  [ -f "$pc_env" ] || usage "--pins reads GEN3_ENGINEER_PIN and GEN3_MEMBER_PIN from iempc's private env, which is not there"
  env_value=""
  env_value_of "$pc_env" GEN3_ENGINEER_PIN || usage "--pins: iempc's private env has no GEN3_ENGINEER_PIN"
  engineer_pin="$env_value"
  env_value=""
  env_value_of "$pc_env" GEN3_MEMBER_PIN || usage "--pins: iempc's private env has no GEN3_MEMBER_PIN"
  member_pin="$env_value"
  env_value=""
  [[ "$engineer_pin" =~ ^[0-9]{4}$ ]] || usage "--pins: GEN3_ENGINEER_PIN of iempc's private env is not 4 digits"
  [[ "$member_pin" =~ ^[0-9]{4}$ ]] || usage "--pins: GEN3_MEMBER_PIN of iempc's private env is not 4 digits"
  # Every [[people]] id of the roster gets the member PIN (CI's e2e job does
  # the same); each id goes onto the box's command line, so it must be one
  # the server takes (validate_member_id).
  roster_ids="$(python3 -I -c '
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    roster = tomllib.load(f)
for person in roster.get("people", []):
    print(person["id"])
' "$test_roster")" || die "$test_roster: its [[people]] ids could not be read"
  mapfile -t people <<< "$roster_ids"
  [ -n "${people[0]:-}" ] || die "$test_roster has no [[people]]: no member PIN to set"
  for person in "${people[@]}"; do
    [[ "$person" =~ ^[A-Za-z0-9_-]{1,64}$ ]] || die "$test_roster: a [[people]] id is not a plain member id"
  done
fi

# One commit: the units, the site and the roster come from this checkout, so
# it must be --sha itself, with nothing changed or added (the paths git lists
# are not printed).
checkout_exit=0
checkout="$(git -C "$repo" rev-parse --verify HEAD 2> /dev/null)" || checkout_exit=$?
[ "$checkout_exit" -eq 0 ] || die "$repo is not a git checkout with a commit: the units and the test site come from the checkout at --sha: nothing was copied"
[ "$checkout" = "$sha" ] ||
  die "this checkout is at $checkout, not --sha $sha: the units, the configs and the test site come from the checkout, so it must be --sha (git checkout --detach $sha): nothing was copied"
changes="$(git -C "$repo" status --porcelain)" || die "git status of this checkout failed: nothing was copied"
[ -z "$changes" ] ||
  die "this checkout has $(printf '%s\n' "$changes" | wc -l) changed or untracked paths (git status --porcelain): the units, the configs and the test site must be --sha's own; commit or remove them: nothing was copied"

declare -A sums=()
# Reads a SHA256SUMS on stdin into sums: exactly one '<sha256>  <name>' line
# for each of the four binaries and BUILD, no other name, none twice. $1 says
# whose it is, $2 what a refusal left undone.
read_sums() {
  local line name
  sums=()
  while IFS= read -r line || [ -n "$line" ]; do
    [[ "$line" =~ ^([0-9a-f]{64})\ \ ([A-Za-z0-9_.-]+)$ ]] || die "$1 SHA256SUMS has a line that is not '<sha256>  <name>': $2"
    name="${BASH_REMATCH[2]}"
    case " ${BINARIES[*]} BUILD " in *" $name "*) ;; *) die "$1 SHA256SUMS names $name, which is not in a gen3 bundle: $2" ;; esac
    [ -z "${sums[$name]:-}" ] || die "$1 SHA256SUMS names $name twice: $2"
    sums[$name]="${BASH_REMATCH[1]}"
  done
  for name in "${BINARIES[@]}" BUILD; do
    [ -n "${sums[$name]:-}" ] || die "$1 SHA256SUMS has no line for $name: $2"
  done
}

if [ "$back" = 0 ]; then
  # The bundle, on the dev box: BUILD is --sha and a newline; SHA256SUMS
  # holds exactly the four binaries and BUILD, each once, and their sums;
  # this repository's CI attested that SHA256SUMS (gen3-attest, by digest).
  printf '%s\n' "$sha" | cmp -s - "$bundle/BUILD" || die "the bundle's BUILD is not --sha (and a newline): nothing was copied"
  read_sums "the bundle's" "nothing was copied" < "$bundle/SHA256SUMS"
  for name in "${BINARIES[@]}" BUILD; do
    actual="$(sha256sum "$bundle/$name" | cut -d' ' -f1)"
    [ "$actual" = "${sums[$name]}" ] || die "$name of the bundle does not have its SHA256SUMS sum on the dev box: nothing was copied"
  done
  gh attestation verify "$bundle/SHA256SUMS" -R "$GITHUB_REPO" --signer-workflow "$SIGNER" >&2 ||
    die "gh attestation verify found no attestation of the bundle's SHA256SUMS by $SIGNER: an unattested bundle never leaves the dev box: nothing was copied"
  echo "deploy.sh: the bundle's SHA256SUMS is attested by this repository's CI"
fi

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

# "ide event" and the live guard stop the stack with exactly this command: a
# stack they could not stop is never installed.
on_box "sudo -n -l $STOP" > /dev/null ||
  die "the box's sudo does not allow 'sudo -n $STOP', the stack's stop \"ide event\" runs: add it to the sudoers rule (scripts/gen3/sudo-lines.txt): nothing was copied"

# The slot the stack runs from, and so the one this deploy fills: the other
# one, slot a while bin is no link (a first deploy, or phase C's folder).
link_exit=0
link="$(on_box "readlink $BINDIR")" || link_exit=$?
phase_c=0
case "$link_exit" in
  0)
    case "$link" in
      slot-a/bin) target=b ;;
      slot-b/bin) target=a ;;
      *) die "$BINDIR is a link, but to neither slot-a/bin nor slot-b/bin: nothing was copied" ;;
    esac
    echo "deploy.sh: the stack's build is in $link; this deploy goes into slot-$target"
    ;;
  1)
    # No link: nothing yet, or phase C's real folder, moved aside once.
    target=a
    kinds_exit=0
    kinds="$(on_box "stat -c '%n %F' $BINDIR $PHASE_C 2> /dev/null")" || kinds_exit=$?
    [ "$kinds_exit" -le 1 ] || die "the box's $BINDIR could not be read (exit $kinds_exit): nothing was copied"
    bin_kind=none phase_c_kind=none
    while IFS= read -r line; do
      case "$line" in
        "$BINDIR "*) bin_kind="${line#"$BINDIR "}" ;;
        "$PHASE_C "*) phase_c_kind="${line#"$PHASE_C "}" ;;
      esac
    done <<< "$kinds"
    case "$bin_kind" in
      none) echo "deploy.sh: the box has no $BINDIR yet; this deploy goes into slot-a" ;;
      directory)
        [ "$phase_c_kind" = none ] ||
          die "$BINDIR is a folder while $PHASE_C exists: phase C's folder is moved aside once, never twice and never over another: nothing was copied"
        phase_c=1
        echo "deploy.sh: $BINDIR is phase C's folder: it goes aside to $PHASE_C (kept) and this deploy into slot-a"
        ;;
      *) die "$BINDIR is a $bin_kind, neither a slot's link nor phase C's folder: nothing was copied" ;;
    esac
    ;;
  *) die "readlink $BINDIR on the box failed (exit $link_exit): nothing was copied" ;;
esac
slot="$OPT/slot-$target/bin"

if [ "$back" = 1 ]; then
  # Back: the other slot must hold --sha, whole, before anything is touched.
  on_box "cat $slot/BUILD" > "$work/slot-BUILD" ||
    die "slot-$target has no BUILD to read: there is no build to go back to: nothing was copied"
  printf '%s\n' "$sha" | cmp -s - "$work/slot-BUILD" ||
    die "slot-$target's BUILD is not --sha (and a newline): --back goes only to the build it names: nothing was copied"
  on_box "cat $slot/SHA256SUMS" > "$work/slot-SHA256SUMS" ||
    die "slot-$target has no SHA256SUMS to read: nothing was copied"
  read_sums "slot-$target's" "nothing was copied" < "$work/slot-SHA256SUMS"
  on_box "cd $slot && sha256sum --check --strict --quiet SHA256SUMS" ||
    die "slot-$target's SHA256SUMS does not hold on the box: nothing was copied"
  echo "deploy.sh: slot-$target holds $sha, its sums hold"
fi

printf 'IEM_DANTE_IFACE=%s\n' "$iface" > "$work/iem-dante.env"
cp -- "$config" "$work/dante-device.toml"
cp -- "$test_site" "$work/iemmixer.toml"
cp -- "$test_roster" "$work/roster.toml"
for unit in "${UNITS[@]}" "$RECEIVER"; do
  cp -- "$here/$unit" "$work/$unit"
done

on_box mkdir -p "$STAGE"
sources=()
if [ "$back" = 0 ]; then
  for name in "${BINARIES[@]}" BUILD SHA256SUMS; do
    sources+=("$bundle/$name")
  done
fi
for name in "${UNITS[@]}" "$RECEIVER" dante-device.toml iem-dante.env iemmixer.toml roster.toml; do
  sources+=("$work/$name")
done
scp -q "${SSH_OPTS[@]}" "${sources[@]}" "$host:$STAGE/"
echo "deploy.sh: copied to the box's ~/$STAGE"

if [ "$back" = 0 ]; then
  on_box "cd $STAGE && sha256sum --check --strict --quiet SHA256SUMS" ||
    die "the bundle's SHA256SUMS does not hold on the box: nothing installed"
  refuse_event "copied to ~/$STAGE, nothing installed"
  # The new slot, while the stack keeps running from the other one.
  fill="set -e"
  for name in "${BINARIES[@]}"; do
    fill+=$'\n'"sudo -n install -D -m 0755 $STAGE/$name $slot/$name"
  done
  for name in BUILD SHA256SUMS; do
    fill+=$'\n'"sudo -n install -D -m 0644 $STAGE/$name $slot/$name"
  done
  on_box "$fill" || die "slot-$target could not be filled: the stack was not stopped"
  on_box "cd $slot && sha256sum --check --strict --quiet SHA256SUMS" ||
    die "slot-$target's SHA256SUMS does not hold on the box: the stack was not stopped"
  echo "deploy.sh: slot-$target holds the bundle, its sums hold; the stack was not stopped"
  refuse_event "slot-$target holds the bundle; the stack was not stopped"
else
  refuse_event "copied to ~/$STAGE; the stack was not stopped"
fi

# Stop the stack, then read what still runs: a unit no deploy has installed
# yet makes the stop exit 5 ("not loaded") while it stops the others.
stop_exit=0
on_box "sudo -n $STOP" || stop_exit=$?
read_exit=0
states="$(on_box systemctl is-active iem-server iem-engine iem-dante)" || read_exit=$?
[ "$read_exit" -ne 255 ] || die "the units' states could not be read after the stop: nothing was switched"
mapfile -t state_lines <<< "$states"
[ "${#state_lines[@]}" -eq 3 ] || die "systemctl is-active did not name the three units' states: nothing was switched"
i=0
for unit in iem-server iem-engine iem-dante; do
  case "${state_lines[$i]}" in
    inactive | failed) ;;
    *) die "$unit is '${state_lines[$i]}' after the stop, not stopped (journalctl -u $unit on the box): nothing was switched" ;;
  esac
  i=$((i + 1))
done
if [ "$stop_exit" -ne 0 ]; then
  echo "deploy.sh: the stop exited $stop_exit (a unit not installed yet is not loaded); none of the three runs" >&2
fi
echo "deploy.sh: the stack is stopped"

# One rename moves the link: bin is the old slot's or the new one's, never
# neither (but on the first switch over phase C's folder, which goes aside
# first: a link cannot replace a folder).
switch="set -e"
if [ "$phase_c" = 1 ]; then
  switch+=$'\n'"sudo -n mv -T $BINDIR $PHASE_C"
fi
switch+=$'\n'"sudo -n ln -sfn slot-$target/bin $NEXT"
switch+=$'\n'"sudo -n mv -T $NEXT $BINDIR"
on_box "$switch" || die "the switch to slot-$target failed on the box: the stack is stopped; look at $BINDIR there and deploy again"
if [ "$phase_c" = 1 ]; then
  echo "deploy.sh: phase C's $BINDIR moved aside to $PHASE_C (kept)"
fi
echo "deploy.sh: $BINDIR is slot-$target/bin"

install="set -e"
for unit in "${UNITS[@]}"; do
  install+=$'\n'"sudo -n install -D -m 0644 $STAGE/$unit $UNITDIR/$unit"
done
install+=$'\n'"sudo -n install -D -m 0644 $STAGE/dante-device.toml $CONFIG"
install+=$'\n'"sudo -n install -D -m 0644 $STAGE/iem-dante.env $ENVFILE"
install+=$'\n'"sudo -n install -D -m 0755 $STAGE/$RECEIVER $LIBDIR/$RECEIVER"
install+=$'\n'"sudo -n install -d -o $SERVICE_USER -g $SERVICE_USER -m 0700 $SITEDIR"
install+=$'\n'"sudo -n install -D -o $SERVICE_USER -g $SERVICE_USER -m 0600 $STAGE/iemmixer.toml $SITE"
install+=$'\n'"sudo -n install -D -o $SERVICE_USER -g $SERVICE_USER -m 0600 $STAGE/roster.toml $ROSTER"
on_box "$install" || die "the units, the device config or the test site could not be installed: the stack is stopped, nothing started"
echo "deploy.sh: installed the units, the device config and the test site"

# Each PIN goes only into this ssh's stdin (printf is a shell builtin: no
# process's command line holds it), never onto the box's command line.
set_pin() {
  local pin="$1" what="$2"
  printf '%s\n' "$pin" | on_box "sudo -n $PIN_RUN $what" ||
    die "the box's iem-server did not store the PIN (pin $what): nothing started"
}
if [ "$pins" = 1 ]; then
  set_pin "$engineer_pin" set-engineer
  for person in "${people[@]}"; do
    set_pin "$member_pin" "set-member $person"
  done
  echo "deploy.sh: PINs set: the engineer and ${#people[@]} roster people"
fi
engineer_pin="" member_pin=""

refuse_event "installed, nothing started"
if [ "$start" = engine ]; then
  started=(iem-engine iem-server)
else
  started=(iem-dante)
fi
# reset-failed only what systemd holds: a unit installed and never started
# (or long inactive) is not in its memory, and reset-failed of one fails
# ("not loaded"; Live 1 of phase C); such a unit has no start count to reset.
# The held ones go in one order, as the sudoers rule lists them.
listed="$(on_box "systemctl list-units --all --plain --no-legend --full iem-dante.service iem-engine.service iem-server.service")" ||
  die "systemctl list-units on the box failed: nothing started"
declare -A loaded=()
while read -r unit _; do
  case "$unit" in iem-dante.service | iem-engine.service | iem-server.service) loaded[$unit]=1 ;; esac
done <<< "$listed"
held=()
for unit in iem-dante iem-engine iem-server; do
  if [ -n "${loaded[$unit.service]:-}" ]; then held+=("$unit"); fi
done
reset=""
if [ "${#held[@]}" -gt 0 ]; then
  reset="sudo -n systemctl reset-failed ${held[*]}"
fi
on_box "set -e
sudo -n systemctl daemon-reload
sudo -n systemctl disable iem-dante iem-engine iem-server${reset:+
$reset}
sudo -n systemctl start ${started[*]}" ||
  die "the start on the box failed (journalctl -u iem-engine -u iem-server -u iem-dante there): the stack may be partly up; deploy again, or stop it (sudo $STOP)"
echo "deploy.sh: started ${started[*]} (none enabled at boot)"

# "ide event" may have come between the check above and the start: its stop
# then ran first and the start brought the stack up again.
if [ -e "$flag" ]; then
  on_box "sudo -n $STOP" ||
    die "$flag appeared during the deploy: an event is on, and the stop of the stack after the start failed: it may still run; stop it on the box (sudo $STOP)"
  die "$flag appeared during the deploy: an event is on; the stack stopped again, nothing read back; deploy again in dev time"
fi

# Read back through the link: the binaries and BUILD are the slot's sums.
paths=()
for name in "${BINARIES[@]}" BUILD; do
  paths+=("$BINDIR/$name")
done
installed="$(on_box sha256sum "${paths[@]}")"
declare -A seen=()
while read -r sum path; do
  [ -n "$sum" ] || continue
  name="${path##*/}"
  [ -n "${sums[$name]:-}" ] || die "sha256sum on the box named '$path', not a file of the slot"
  [ "$sum" = "${sums[$name]}" ] || die "the installed $path's sha256 is $sum, not ${sums[$name]}"
  seen[$name]=1
done <<< "$installed"
for name in "${BINARIES[@]}" BUILD; do
  [ -n "${seen[$name]:-}" ] || die "sha256sum on the box did not read $BINDIR/$name"
done

engine_version="$(on_box "$BINDIR/iem-engine" --version)"
want="^iem-engine [^[:space:]]+[+]$sha\$"
[[ "$engine_version" =~ $want ]] || die "$BINDIR/iem-engine --version said '$engine_version', not 'iem-engine <version>+$sha'"
device_version="$(on_box "$BINDIR/iem-dante-dev" --version)"
want="^iem-dante-dev [^[:space:]]+ $sha\$"
[[ "$device_version" =~ $want ]] || die "$BINDIR/iem-dante-dev --version said '$device_version', not 'iem-dante-dev <version> $sha'"

# Health (iem-dante-dev health: one line of numbers and fixed words; exit 0
# healthy, 1 degraded or failed, 3 stopped, 2 its command line), once a
# second until healthy, a bounded number of tries.
health="" health_exit=0
for ((n = 1; n <= tries; n++)); do
  health_exit=0
  health="$(on_box "$BINDIR/iem-dante-dev health")" || health_exit=$?
  health="${health##*$'\n'}"
  [ "$health_exit" -ne 0 ] || break
  [ "$health_exit" -ne 2 ] || die "$BINDIR/iem-dante-dev health refused its command line (exit 2): ${health:-no line}"
  if [ "$n" -lt "$tries" ]; then sleep 1; fi
done
[ "$health_exit" -eq 0 ] ||
  die "the stack is not healthy after $tries tries a second apart (iem-dante-dev health exit $health_exit; journalctl -u iem-engine -u iem-server -u iem-dante on the box): ${health:-no line}"
echo "deploy.sh: slot-$target: $engine_version; $device_version; sums as SHA256SUMS; ${started[*]} started; health $health"
