#!/usr/bin/env bash
# The remote commands name this script's own constant paths, expanded here on purpose.
# shellcheck disable=SC2029
set -euo pipefail
# Puts a CI Gen 3 bundle on the Gen 3 box and starts the stack: the engine
# with our Dante device inside it and the server, or the standalone device
# (#72 phase C, plan Task 6; .claude/rules/gen3.md):
#
#   deploy.sh --host <user@host> --bundle <the downloaded gen3-bundle-<sha>>
#             --sha <its commit, 40 hex> --config <the private device config>
#             --iface <the box's Dante interface> [--start engine|device] [--pins]
#
# - Dev time only: refused while the "ide event" flag exists (iempc's
#   EVENT-NOW: $IEMMIXER_EVENT_NOW, else ~/.config/iemmixer/EVENT-NOW),
#   checked again right before anything is installed, right before the start
#   and once more right after it: a flag that came meanwhile ("ide event"'s
#   stop may have run before the start) stops all three units again and fails
#   the deploy.
# - The bundle (CI's gen3-bin job uploads gen3-bundle-<sha>): its BUILD must
#   be --sha and a newline, and its SHA256SUMS must list exactly the four
#   binaries and BUILD and hold for them, on the dev box before anything is
#   called and again on the box after the copy.
# - "ide event" stops the stack with `ssh <GEN3_HOST> sudo -n systemctl stop
#   iem-server iem-engine iem-dante` (plan Task 7): a --host that is not the
#   GEN3_HOST of iempc's private env ($PC_ENV, else
#   ~/.config/iemmixer/iem-pc.env) is a warning, an env without GEN3_HOST too;
#   and right after `id -un` (never root) the box's sudo must allow that stop
#   (`sudo -n -l`), else refused before anything is copied.
# - Then that stop, and none of the three units may still run (`systemctl
#   is-active`: inactive or failed): a unit no deploy has installed yet is
#   "not loaded", so the stop's own exit is not the verdict.
# - The four binaries, BUILD and SHA256SUMS, the three units (beside this
#   script), the drop-in naming the box's user (the ssh user, never root), the
#   config, the interface (written as /etc/iemmixer/iem-dante.env) and the
#   public test site and roster (this checkout's config/test-site.toml and
#   config/test-roster.toml) go by scp into ~/.cache/iem-gen3-deploy on the
#   box (overwritten, never deleted); then `sudo -n install -D` puts the
#   binaries into /opt/iemmixer/bin (0755: a downloaded artifact loses the
#   executable bit), each unit's drop-in before the units (no moment has a
#   unit without its User=), the units, the config and the env;
#   `sudo -n install -d -o <user> -m 0700 /var/lib/iemmixer/site`, and the
#   user itself copies the site and the roster there (the server writes
#   beside them).
# - --pins (the server is stopped): `iem-server pin set-engineer` and `pin
#   set-member <id>` for every [[people]] id of the roster, run as the user on
#   the box, each PIN (GEN3_ENGINEER_PIN, GEN3_MEMBER_PIN of the private env,
#   4 digits each) only on that ssh's stdin: never on a command line, never
#   printed.
# - `sudo -n systemctl daemon-reload`, `disable iem-dante iem-engine
#   iem-server` (none starts at boot; an earlier deploy's enable is undone),
#   `reset-failed iem-dante iem-engine iem-server` (the engine's start limit
#   counts manual starts too: a deploy is a deliberate new start), then
#   `start iem-engine iem-server` (--start engine, the default) or `start
#   iem-dante` (--start device; the units' Conflicts= keep it and the engine
#   apart).
# - Read back: the installed binaries' sha256 against SHA256SUMS, `iem-engine
#   --version` and `iem-dante-dev --version` naming --sha, `systemctl
#   is-active` of what was started after GEN3_SETTLE_S (3) seconds, and (the
#   engine) on the box `curl -sf http://127.0.0.1:8080/api/version` within
#   GEN3_SERVER_WAIT_S (30) tries a second apart, its git_hash a prefix of
#   --sha.
#
# Any failure stops the script: exit 1, 2 for its command line. Site values
# (host, user, interface, names, PINs) come only from the arguments, the
# private config and the private env; none is in this repository (P6), and
# none is printed.

STAGE=.cache/iem-gen3-deploy
BINDIR=/opt/iemmixer/bin
UNITDIR=/etc/systemd/system
CONFIG=/etc/iemmixer/dante-device.toml
ENVFILE=/etc/iemmixer/iem-dante.env
SITEDIR=/var/lib/iemmixer/site
SITE=$SITEDIR/iemmixer.toml
ROSTER=$SITEDIR/roster.toml
BINARIES=(iem-engine iem-server iem-dante-dev iem-soakclient)
UNITS=(iem-dante iem-engine iem-server)
# The stack's stop, exactly as "ide event" and the live guard run it (plan
# Task 7) and as the box's sudoers rule allows it.
STOP="systemctl stop iem-server iem-engine iem-dante"
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
  exit 2
}

host="" bundle="" sha="" config="" iface="" start=engine pins=0
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
    *) usage "unknown argument '$1'" ;;
  esac
done
for name in host bundle sha config iface; do
  [ -n "${!name}" ] || usage "--$name is missing"
done
case "$host" in -*) usage "--host must be an ssh destination, not an option" ;; esac
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || usage "--sha is not 40 lowercase hex digits"
[[ "$iface" =~ ^[A-Za-z0-9_.-]{1,15}$ ]] || usage "--iface is not an interface name"
case "$start" in engine | device) ;; *) usage "--start is engine or device" ;; esac
[ -d "$bundle" ] || usage "--bundle is not a folder"
for name in "${BINARIES[@]}" BUILD SHA256SUMS; do
  [ -f "$bundle/$name" ] || usage "--bundle has no $name: not a gen3 bundle"
done
[ -f "$config" ] || usage "--config is not a file"
settle="${GEN3_SETTLE_S:-3}"
tries="${GEN3_SERVER_WAIT_S:-30}"
[[ "$settle" =~ ^[0-9]{1,3}$ ]] || usage "GEN3_SETTLE_S is not a number of seconds"
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
for path in "$here/iem-dante.service" "$here/iem-engine.service" "$here/iem-server.service" "$test_site" "$test_roster"; do
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

# The bundle, on the dev box: BUILD is --sha and a newline; SHA256SUMS holds
# exactly the four binaries and BUILD, each once, and their sums.
printf '%s\n' "$sha" | cmp -s - "$bundle/BUILD" || die "the bundle's BUILD is not --sha (and a newline): nothing was copied"
declare -A sums=()
while IFS= read -r line || [ -n "$line" ]; do
  [[ "$line" =~ ^([0-9a-f]{64})\ \ ([A-Za-z0-9_.-]+)$ ]] || die "the bundle's SHA256SUMS has a line that is not '<sha256>  <name>': nothing was copied"
  name="${BASH_REMATCH[2]}"
  case " ${BINARIES[*]} BUILD " in *" $name "*) ;; *) die "the bundle's SHA256SUMS names $name, which is not in a gen3 bundle: nothing was copied" ;; esac
  [ -z "${sums[$name]:-}" ] || die "the bundle's SHA256SUMS names $name twice: nothing was copied"
  sums[$name]="${BASH_REMATCH[1]}"
done < "$bundle/SHA256SUMS"
for name in "${BINARIES[@]}" BUILD; do
  [ -n "${sums[$name]:-}" ] || die "the bundle's SHA256SUMS has no line for $name: nothing was copied"
  actual="$(sha256sum "$bundle/$name" | cut -d' ' -f1)"
  [ "$actual" = "${sums[$name]}" ] || die "$name of the bundle does not have its SHA256SUMS sum on the dev box: nothing was copied"
done

user="$(ssh "${SSH_OPTS[@]}" "$host" id -un)"
[[ "$user" =~ ^[a-z_][a-z0-9_.-]*$ ]] || die "the box's user is not a plain user name: nothing was copied"
[ "$user" != root ] || die "the services run as the box's admin user, not root: nothing was copied"
# "ide event" and the live guard stop the stack with exactly this command: a
# stack they could not stop is never installed.
ssh "${SSH_OPTS[@]}" "$host" "sudo -n -l $STOP" > /dev/null ||
  die "the box's sudo does not allow 'sudo -n $STOP', the stack's stop \"ide event\" runs: add it to the sudoers rule (.claude/rules/gen3.md, Deploy): nothing was copied"

# Stop the stack, then read what still runs: a unit no deploy has installed
# yet makes the stop exit 5 ("not loaded") while it stops the others.
stop_exit=0
ssh "${SSH_OPTS[@]}" "$host" "sudo -n $STOP" || stop_exit=$?
read_exit=0
states="$(ssh "${SSH_OPTS[@]}" "$host" systemctl is-active iem-server iem-engine iem-dante)" || read_exit=$?
[ "$read_exit" -ne 255 ] || die "the units' states could not be read after the stop: nothing was copied"
mapfile -t state_lines <<< "$states"
[ "${#state_lines[@]}" -eq 3 ] || die "systemctl is-active did not name the three units' states: nothing was copied"
i=0
for unit in iem-server iem-engine iem-dante; do
  case "${state_lines[$i]}" in
    inactive | failed) ;;
    *) die "$unit is '${state_lines[$i]}' after the stop, not stopped (journalctl -u $unit on the box): nothing was copied" ;;
  esac
  i=$((i + 1))
done
if [ "$stop_exit" -ne 0 ]; then
  echo "deploy.sh: the stop exited $stop_exit (a unit not installed yet is not loaded); none of the three runs" >&2
fi
echo "deploy.sh: the stack is stopped"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
printf 'IEM_DANTE_IFACE=%s\n' "$iface" > "$work/iem-dante.env"
printf '[Service]\nUser=%s\n' "$user" > "$work/10-user.conf"
cp -- "$config" "$work/dante-device.toml"
cp -- "$test_site" "$work/iemmixer.toml"
cp -- "$test_roster" "$work/roster.toml"
for unit in "${UNITS[@]}"; do
  cp -- "$here/$unit.service" "$work/$unit.service"
done

ssh "${SSH_OPTS[@]}" "$host" mkdir -p "$STAGE"
sources=()
for name in "${BINARIES[@]}" BUILD SHA256SUMS; do
  sources+=("$bundle/$name")
done
for name in iem-dante.service iem-engine.service iem-server.service 10-user.conf dante-device.toml iem-dante.env \
  iemmixer.toml roster.toml; do
  sources+=("$work/$name")
done
scp -q "${SSH_OPTS[@]}" "${sources[@]}" "$host:$STAGE/"
echo "deploy.sh: copied to the box's ~/$STAGE"

ssh "${SSH_OPTS[@]}" "$host" "cd $STAGE && sha256sum --check --strict --quiet SHA256SUMS" ||
  die "the bundle's SHA256SUMS does not hold on the box: nothing installed"

refuse_event "copied to ~/$STAGE, nothing installed"
install="set -e"
for name in "${BINARIES[@]}"; do
  install+=$'\n'"sudo -n install -D -m 0755 $STAGE/$name $BINDIR/$name"
done
for unit in "${UNITS[@]}"; do
  install+=$'\n'"sudo -n install -D -m 0644 $STAGE/10-user.conf $UNITDIR/$unit.service.d/10-user.conf"
done
for unit in "${UNITS[@]}"; do
  install+=$'\n'"sudo -n install -D -m 0644 $STAGE/$unit.service $UNITDIR/$unit.service"
done
install+=$'\n'"sudo -n install -D -m 0644 $STAGE/dante-device.toml $CONFIG"
install+=$'\n'"sudo -n install -D -m 0644 $STAGE/iem-dante.env $ENVFILE"
install+=$'\n'"sudo -n install -d -o $user -m 0700 $SITEDIR"
install+=$'\n'"install -m 0600 $STAGE/iemmixer.toml $SITE"
install+=$'\n'"install -m 0600 $STAGE/roster.toml $ROSTER"
ssh "${SSH_OPTS[@]}" "$host" "$install"
echo "deploy.sh: installed the bundle, the units, the device config and the test site"

# Each PIN goes only into this ssh's stdin (printf is a shell builtin: no
# process's command line holds it), never onto the box's command line.
set_pin() {
  local pin="$1" what="$2"
  printf '%s\n' "$pin" | ssh "${SSH_OPTS[@]}" "$host" "IEMMIXER_CONFIG=$SITE $BINDIR/iem-server pin $what" ||
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
ssh "${SSH_OPTS[@]}" "$host" "set -e
sudo -n systemctl daemon-reload
sudo -n systemctl disable iem-dante iem-engine iem-server
sudo -n systemctl reset-failed iem-dante iem-engine iem-server
sudo -n systemctl start ${started[*]}"
echo "deploy.sh: started ${started[*]} (none enabled at boot)"

# "ide event" may have come between the check above and the start: its stop
# then ran first and the start brought the stack up again.
if [ -e "$flag" ]; then
  ssh "${SSH_OPTS[@]}" "$host" "sudo -n $STOP" ||
    die "$flag appeared during the deploy: an event is on, and the stop of the stack after the start failed: it may still run; stop it on the box (sudo $STOP)"
  die "$flag appeared during the deploy: an event is on; the stack stopped again, nothing read back; deploy again in dev time"
fi

paths=()
for name in "${BINARIES[@]}"; do
  paths+=("$BINDIR/$name")
done
installed="$(ssh "${SSH_OPTS[@]}" "$host" sha256sum "${paths[@]}")"
declare -A seen=()
while read -r sum path; do
  [ -n "$sum" ] || continue
  name="${path##*/}"
  [ -n "${sums[$name]:-}" ] || die "sha256sum on the box named '$path', not an installed binary"
  [ "$sum" = "${sums[$name]}" ] || die "the installed $path's sha256 is $sum, not ${sums[$name]}"
  seen[$name]=1
done <<< "$installed"
for name in "${BINARIES[@]}"; do
  [ -n "${seen[$name]:-}" ] || die "sha256sum on the box did not read $BINDIR/$name"
done

engine_version="$(ssh "${SSH_OPTS[@]}" "$host" "$BINDIR/iem-engine" --version)"
want="^iem-engine [^[:space:]]+[+]$sha\$"
[[ "$engine_version" =~ $want ]] || die "$BINDIR/iem-engine --version said '$engine_version', not 'iem-engine <version>+$sha'"
device_version="$(ssh "${SSH_OPTS[@]}" "$host" "$BINDIR/iem-dante-dev" --version)"
want="^iem-dante-dev [^[:space:]]+ $sha\$"
[[ "$device_version" =~ $want ]] || die "$BINDIR/iem-dante-dev --version said '$device_version', not 'iem-dante-dev <version> $sha'"

sleep "$settle"
read_exit=0
states="$(ssh "${SSH_OPTS[@]}" "$host" systemctl is-active "${started[@]}")" || read_exit=$?
[ "$read_exit" -ne 255 ] || die "the started units' states could not be read"
mapfile -t state_lines <<< "$states"
i=0
for unit in "${started[@]}"; do
  [ "${state_lines[$i]:-}" = active ] ||
    die "$unit is '${state_lines[$i]:-}' after $settle s, not active (journalctl -u $unit on the box)"
  i=$((i + 1))
done

if [ "$start" = device ]; then
  echo "deploy.sh: $engine_version; $device_version; sums as SHA256SUMS; iem-dante active (the standalone device; the engine and the server stopped)"
  exit 0
fi

# The server answers once it is up: a second apart, a bounded number of tries.
answer=""
for ((n = 1; n <= tries; n++)); do
  if answer="$(ssh "${SSH_OPTS[@]}" "$host" curl -sf --max-time 5 http://127.0.0.1:8080/api/version)"; then
    break
  fi
  answer=""
  if [ "$n" -lt "$tries" ]; then sleep 1; fi
done
[ -n "$answer" ] || die "the box's server did not answer /api/version on port 8080 within $tries tries (journalctl -u iem-server on the box)"
want='"git_hash"[[:space:]]*:[[:space:]]*"([0-9a-f]{7,40})"'
[[ "$answer" =~ $want ]] || die "the box's /api/version names no git_hash: '$answer'"
git_hash="${BASH_REMATCH[1]}"
[[ "$sha" == "$git_hash"* ]] || die "the box's /api/version names $git_hash, not a prefix of $sha"
echo "deploy.sh: $engine_version; $device_version; sums as SHA256SUMS; iem-engine and iem-server active; /api/version names $git_hash"
