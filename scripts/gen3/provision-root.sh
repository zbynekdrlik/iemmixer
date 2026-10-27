#!/usr/bin/env bash
# sudoers and dpkg-query formats are literal text here, never shell expansions.
# shellcheck disable=SC2016
set -euo pipefail
# The Gen 3 box's provisioning, run as root on the box (#72 phase D, plan
# docs/superpowers/plans/2026-10-26-gen3-phase-d-the-box.md Task 9, choices 1,
# 4 and 5; rules in .claude/rules/gen3.md). provision.sh copies it with
# sudo-lines.txt, iem-napi.service, napi-priority and the two values into the
# admin's ~/.cache/iem-gen3-provision and runs it there:
#
#   sudo -n bash .cache/iem-gen3-provision/provision-root.sh --admin <user>
#        --ntp-file <file: the ops dantesync config's ntp_server>
#        --roster-file <file: config/test-roster.toml's [[people]] ids>
#
# Idempotent: write and enable only, each step a line `<step> changed` or
# `<step> unchanged`; a second run changes nothing. It never starts, stops or
# restarts the stack; it restarts journald when its configuration changed,
# starts the box's time owner (dantesync) when it is not running, runs
# iem-napi (the Dante interface's packet priority) when it does not hold, and
# turns the display output polling off when it is on.
# Refused with nothing changed: not root, dantesync not installed (the box's
# one time owner comes from install-dantesync.sh, never from here) or its
# config not the ops follower config --ntp-file's NTP server names, a PTP
# daemon besides it, a unit of the stack running, a sudoers rule
# sudo-lines.txt renders that visudo refuses.
#
# Steps, in order:
#   user      the system group and user iemmixer (no login, no home), the
#             admin in its group (the device's control socket, health, the
#             live guard).
#   folders   /etc/iemmixer 0755, /etc/credstore.encrypted 0700 root,
#             /var/lib/iemmixer/mint 0700 iemmixer (gen3-mix's tokens).
#   state     the stack's state the service user's: /var/lib/iemmixer and
#             /var/lib/iem-dante 0750 iemmixer:iemmixer (made when missing;
#             one that is a link refused), and when anything under them is
#             another user's or group's (phase C ran the stack as the admin),
#             `chown -R -h` of both, never through a link, read back; modes
#             kept, nothing deleted. systemd's StateDirectory= takes a tree
#             over only at a unit's start (and not when its top folder is
#             right), while deploy.sh --pins writes site/secrets as the
#             service user before any start.
#   pepper    only when absent: 32 random bytes straight into `systemd-creds
#             encrypt --name=pepper --with-key=auto` (the TPM2 with the host
#             key when the box has one, else the host key) under umask 077 as
#             /etc/credstore.encrypted/iemmixer-pepper; never written in
#             clear, never overwritten, never printed.
#   phase-c   phase C's 10-user.conf drop-ins of the three units go (the
#             units name their user since phase D), and the synthetic band's
#             plain test pepper with the PIN hashes made with it
#             (deploy.sh --pins gives every PIN again).
#   journal   journald persistent on the SSD (1 GiB, 30 days), restarted
#             when that changed; rsyslog purged.
#   apt       the installed kernel meta packages held, no unattended
#             upgrades (20auto-upgrades, the apt timers and the
#             unattended-upgrades service masked): an appliance never gains a
#             kernel or a package by itself (camera-box #295).
#   sleep     sleep, suspend, hibernate and hybrid-sleep masked.
#   swap      a swap file sized to the memory, 2 to 8 GiB, in /etc/fstab,
#             when the box has no active swap (the owner's rule for every
#             Linux box: an out-of-memory kill must never take a service).
#   clock     NTP off in timedatectl, systemd-timesyncd, chrony, ntp and
#             time-sync-coordinator disabled and stopped, dantesync enabled
#             and running: the box's one time owner, nothing beside it.
#   napi      napi-priority at /usr/local/lib/iemmixer/napi-priority and
#             iem-napi.service, enabled (at boot before dantesync and the
#             stack); irqbalance masked (it would move the interrupt off its
#             core); run now and read back once deploy.sh has written the
#             Dante interface (/etc/iemmixer/iem-dante.env).
#   display   /etc/tmpfiles.d/iemmixer-display.conf: the display drivers'
#             output polling off (drm_kms_helper's poll N) at every boot,
#             applied with systemd-tmpfiles and read back when the box has
#             the parameter. The box has no screen; the i915 driver's poll
#             (every ~10 s, waking the GPU) held a CPU ~2 ms and the engine's
#             pump, woken onto it, missed its blocks (Live 4).
#   stack     none of the stack's units enabled (nothing of it starts at
#             boot).
#   sudoers   /etc/sudoers.d/iemmixer-gen3 (0440): sudo-lines.txt for the
#             admin, NOPASSWD, each program by the absolute path sudo's
#             secure_path finds it at, placeholders expanded; written to a
#             temporary file visudo -cf must pass before it is moved into
#             place. The admin keeps its own passwordless rule until phase G.
#
# IEM_PROVISION_ROOT prefixes every path it reads or writes, and every
# command is called by name: the tests' seams. Exit 1 for a refusal or a
# failure, 2 for its command line. It prints no site value (P6): the admin,
# the NTP server and the interface are never in its output.

R="${IEM_PROVISION_ROOT:-}"
SERVICE_USER=iemmixer
STACK_UNITS=(iem-engine.service iem-server.service iem-dante.service)
TIME_UNITS=(systemd-timesyncd.service chrony.service ntp.service time-sync-coordinator.service)
PTP_UNITS=(ptp4l.service phc2sys.service statime.service)
APT_UNITS=(unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer)
SLEEP_UNITS=(sleep.target suspend.target hibernate.target hybrid-sleep.target)
KERNEL_PACKAGES=('linux-generic*' 'linux-image-generic*' 'linux-headers-generic*')
CREDENTIAL=/etc/credstore.encrypted/iemmixer-pepper
# The stack's state folders (the units' StateDirectory=), and their paths
# under this run's root.
STATE_DIRS=(/var/lib/iemmixer /var/lib/iem-dante)
STATE_PATHS=("$R${STATE_DIRS[0]}" "$R${STATE_DIRS[1]}")
SECRETS=/var/lib/iemmixer/site/secrets
JOURNAL_CONF=/etc/systemd/journald.conf.d/50-iemmixer.conf
AUTO_UPGRADES=/etc/apt/apt.conf.d/20auto-upgrades
SWAPFILE=/swapfile
NAPI_HELPER=/usr/local/lib/iemmixer/napi-priority
NAPI_UNIT=/etc/systemd/system/iem-napi.service
DISPLAY_CONF=/etc/tmpfiles.d/iemmixer-display.conf
POLL_PARAM=/sys/module/drm_kms_helper/parameters/poll
ENVFILE=/etc/iemmixer/iem-dante.env
SUDOERS=/etc/sudoers.d/iemmixer-gen3
# sudo's secure_path on Ubuntu, in its order: where sudo finds a program the
# admin names without a path, so where the sudoers rule names it.
SECURE_PATH=(/usr/local/sbin /usr/local/bin /usr/sbin /usr/bin /sbin /bin)
SLOTS=(a b)
UNIT_SETS=("iem-dante" "iem-engine" "iem-server" "iem-dante iem-engine" "iem-dante iem-server"
  "iem-engine iem-server" "iem-dante iem-engine iem-server")

die() {
  echo "provision-root.sh: $*" >&2
  exit 1
}

usage() {
  echo "provision-root.sh: $*" >&2
  echo "usage: provision-root.sh --admin <user> --ntp-file <file> --roster-file <file>" >&2
  exit 2
}

# $1 without its leading and trailing white space.
trim() {
  local s="$1"
  s="${s#"${s%%[![:space:]]*}"}"
  s="${s%"${s##*[![:space:]]}"}"
  printf '%s' "$s"
}

admin="" ntp_file="" roster_file=""
while [ $# -gt 0 ]; do
  case "$1" in
    --admin | --ntp-file | --roster-file)
      [ $# -ge 2 ] || usage "$1 needs a value"
      case "$1" in
        --admin) admin="$2" ;;
        --ntp-file) ntp_file="$2" ;;
        --roster-file) roster_file="$2" ;;
      esac
      shift 2
      ;;
    *) usage "unknown argument '$1'" ;;
  esac
done
for name in admin ntp_file roster_file; do
  [ -n "${!name}" ] || usage "--${name//_/-} is missing"
done
[[ "$admin" =~ ^[a-z_][a-z0-9_-]{0,31}$ ]] || usage "--admin is not a user name"
case "$admin" in root | "$SERVICE_USER") usage "--admin is the box's admin user, neither root nor $SERVICE_USER" ;; esac
[ -f "$ntp_file" ] || usage "--ntp-file is not a file"
[ -f "$roster_file" ] || usage "--roster-file is not a file"
members=()
while IFS= read -r line || [ -n "$line" ]; do
  line="$(trim "$line")"
  [ -n "$line" ] || continue
  [[ "$line" =~ ^[A-Za-z0-9_-]{1,64}$ ]] || usage "--roster-file holds a line that is not a plain member id"
  members+=("$line")
done < "$roster_file"
[ "${#members[@]}" -gt 0 ] || usage "--roster-file holds no member id"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
for name in sudo-lines.txt iem-napi.service napi-priority; do
  [ -f "$here/$name" ] || die "$here/$name is missing: provision.sh copies it beside this script"
done

# --- What systemd and dpkg say (read only). ---

# Sets load_state, file_state and active_state of the unit $1.
unit_states() {
  local out line
  out="$(systemctl show --property=LoadState,UnitFileState,ActiveState "$1")"
  load_state="" file_state="" active_state=""
  while IFS= read -r line; do
    case "$line" in
      LoadState=*) load_state="${line#*=}" ;;
      UnitFileState=*) file_state="${line#*=}" ;;
      ActiveState=*) active_state="${line#*=}" ;;
    esac
  done <<< "$out"
}

# Whether the unit unit_states read is enabled (at boot, or pulled in by one
# that is) or running.
is_enabled() {
  case "$file_state" in enabled | enabled-runtime | linked | linked-runtime | alias | indirect) return 0 ;; esac
  return 1
}
is_running() {
  case "$active_state" in active | activating | deactivating | reloading | refreshing) return 0 ;; esac
  return 1
}

# The status word of a package (installed, config-files, ...), absent when
# dpkg knows none.
package_status() {
  local out rc=0
  out="$(dpkg-query -W -f '${Status}' "$1" 2> /dev/null)" || rc=$?
  case "$rc" in
    0) echo "${out##* }" ;;
    1) echo absent ;;
    *) die "dpkg-query of $1 failed (exit $rc)" ;;
  esac
}

# The box's active swap areas: /proc/swaps has more than its header.
swap_active() {
  [ "$(wc -l < "$R/proc/swaps")" -gt 1 ]
}

# --- Writing (each sets wrote and, when it wrote, changed). ---

# $1 mode, $2 owner, $3 group, $4 the box path, $5 the source: installed
# unless the box's file has those bytes, that mode and owner already.
put_file() {
  local dest="$R$4"
  wrote=0
  if [ -f "$dest" ] && [ ! -L "$dest" ] && cmp -s -- "$5" "$dest" &&
    [ "$(stat -c '%a %U %G' -- "$dest")" = "${1#0} $2 $3" ]; then
    return 0
  fi
  install -D -m "$1" -o "$2" -g "$3" -- "$5" "$dest"
  wrote=1
  changed=1
}

# $1 mode, $2 owner, $3 group, $4 the box path of a folder.
put_dir() {
  local dest="$R$4"
  wrote=0
  if [ -d "$dest" ] && [ ! -L "$dest" ] && [ "$(stat -c '%a %U %G' -- "$dest")" = "${1#0} $2 $3" ]; then
    return 0
  fi
  install -d -m "$1" -o "$2" -g "$3" -- "$dest"
  wrote=1
  changed=1
}

# Masks those of the units $@ not masked yet, then reads them back.
mask_units() {
  local unit
  local -a todo=()
  for unit in "$@"; do
    unit_states "$unit"
    [ "$file_state" = masked ] || todo+=("$unit")
  done
  [ "${#todo[@]}" -gt 0 ] || return 0
  systemctl mask "${todo[@]}"
  changed=1
  for unit in "${todo[@]}"; do
    unit_states "$unit"
    [ "$file_state" = masked ] || die "$unit is '$file_state' after its mask, not masked"
  done
}

# --- The sudoers rule. ---

# Sets expanded to the template command $1 with its placeholders expanded:
# {slot} a and b, {member} each roster id, {units} each ordered non-empty
# subset of the stack (as test_deploy.py's expand).
expand_command() {
  local key text choice
  local -a choices next
  expanded=("$1")
  for key in '{slot}' '{member}' '{units}'; do
    [[ "$1" == *"$key"* ]] || continue
    case "$key" in
      '{slot}') choices=("${SLOTS[@]}") ;;
      '{member}') choices=("${members[@]}") ;;
      '{units}') choices=("${UNIT_SETS[@]}") ;;
    esac
    next=()
    for text in "${expanded[@]}"; do
      for choice in "${choices[@]}"; do
        next+=("${text//"$key"/"$choice"}")
      done
    done
    expanded=("${next[@]}")
  done
}

# The absolute path of the program $1: as given, or the first of sudo's
# secure_path folders on the box that holds it.
absolute() {
  local dir
  case "$1" in /*)
    echo "$1"
    return 0
    ;;
  esac
  for dir in "${SECURE_PATH[@]}"; do
    if [ -f "$R$dir/$1" ] && [ -x "$R$dir/$1" ]; then
      echo "$dir/$1"
      return 0
    fi
  done
  die "sudo-lines.txt names $1, which is in none of sudo's secure_path folders on the box: nothing changed"
}

# One argument as sudoers reads it literally: its special characters (and
# fnmatch's) behind a backslash.
escape() {
  local word="$1" out="" c i
  for ((i = 0; i < ${#word}; i++)); do
    c="${word:i:1}"
    case "$c" in
      \\ | ',' | ':' | '=' | '#' | '*' | '?' | '[' | ']' | '!' | '^' | ' ') out+="\\$c" ;;
      *) out+="$c" ;;
    esac
  done
  printf '%s' "$out"
}

# The sudoers file, from sudo-lines.txt: one rule per expanded command, in the
# template's order, none twice.
render_sudoers() {
  local raw line runas command text path word args rule
  local -a words
  local -A seen=()
  cat << 'HEAD'
# The Gen 3 box's sudo rule for its admin user (#72 phase D): written by
# scripts/gen3/provision-root.sh from scripts/gen3/sudo-lines.txt at every
# provision run (never edit it here): each command exactly as a Gen 3 tool
# sends it through `sudo -n`, its program by the absolute path sudo's
# secure_path finds, NOPASSWD. Until phase G the admin also keeps its own
# passwordless rule (plan Task 9).
HEAD
  while IFS= read -r raw || [ -n "$raw" ]; do
    line="$(trim "$raw")"
    case "$line" in '' | '#'*) continue ;; esac
    runas="${line%% | *}"
    command="${line#* | }"
    if [ "$runas" = "$line" ] || [ -z "$command" ]; then
      die "sudo-lines.txt has a line that is not '<runas> | <command>': nothing changed"
    fi
    case "$runas" in root | "$SERVICE_USER") ;; *) die "sudo-lines.txt runs a command as neither root nor $SERVICE_USER: nothing changed" ;; esac
    case "$command" in *\'* | *\"* | *\\*) die "sudo-lines.txt has a command with a quote or a backslash: nothing changed" ;; esac
    expand_command "$command"
    for text in "${expanded[@]}"; do
      case "$text" in *'{'* | *'}'*) die "sudo-lines.txt has a placeholder that is none of {slot}, {member}, {units}: nothing changed" ;; esac
      read -r -a words <<< "$text"
      path="$(absolute "${words[0]}")"
      args=""
      for word in "${words[@]:1}"; do
        args+=" $(escape "$word")"
      done
      # A rule without arguments would allow any: "" allows none.
      [ -n "$args" ] || args=' ""'
      rule="$admin ALL=($runas) NOPASSWD: $path$args"
      [ -z "${seen[$rule]:-}" ] || continue
      seen[$rule]=1
      echo "$rule"
    done
  done < "$here/sudo-lines.txt"
}

# --- Before any step: refusals with nothing changed. ---

[ "$(id -u)" = 0 ] || die "not root: it runs through sudo (provision.sh: sudo -n bash ...): nothing changed"
getent passwd "$admin" > /dev/null || die "--admin names no user of the box: nothing changed"

# dantesync, the box's one time owner, installed from the ops follower config
# (install-dantesync.sh): its config's ntp_server is --ntp-file's.
if [ ! -f "$R/usr/local/bin/dantesync" ] || [ ! -x "$R/usr/local/bin/dantesync" ] ||
  [ ! -f "$R/etc/systemd/system/dantesync.service" ] || [ ! -f "$R/etc/dantesync/config.json" ]; then
  die "dantesync is not installed: the box's one time owner comes from scripts/gen3/install-dantesync.sh (the fleet's pin, the ops follower config); run it first: nothing changed"
fi
why="$(python3 -I - "$R/etc/dantesync/config.json" "$ntp_file" << 'PY'
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as f:
        config = json.load(f)
except (OSError, ValueError) as e:
    print(f"cannot be read ({type(e).__name__})")
    sys.exit(0)
with open(sys.argv[2], encoding="utf-8") as f:
    want = f.read().strip()
if not want or "\n" in want:
    print("compared with no NTP server: --ntp-file holds none on one line")
    sys.exit(0)
server = config.get("ntp_server") if isinstance(config, dict) else None
print("" if isinstance(server, str) and server.strip() == want else "a follower of another NTP server")
PY
)" || die "dantesync's config could not be compared (python3 above): nothing changed"
[ -z "$why" ] ||
  die "dantesync's config (/etc/dantesync/config.json) $why: install it from the ops follower config --ntp-from names (install-dantesync.sh --config <it>): nothing changed"
for unit in "${PTP_UNITS[@]}"; do
  unit_states "$unit"
  if is_enabled || is_running; then
    die "$unit is enabled or running: dantesync must be the box's one PTP owner; remove it first: nothing changed"
  fi
done
for unit in "${STACK_UNITS[@]}"; do
  unit_states "$unit"
  if is_running; then
    die "$unit is $active_state: provision never touches a running stack; stop it first (sudo systemctl stop iem-server iem-engine iem-dante): nothing changed"
  fi
done

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
render_sudoers > "$work/sudoers"
visudo -cf "$work/sudoers" > /dev/null ||
  die "the sudoers rule sudo-lines.txt renders does not pass visudo -cf: nothing changed"
cat > "$work/50-iemmixer.conf" << 'EOF'
# The Gen 3 box's journal (#72 phase D, choice 4): kept on the SSD across
# reboots (journalctl -b -1), at most 1 GiB and 30 days, every unit's output
# in it. Written by scripts/gen3/provision-root.sh.
[Journal]
Storage=persistent
SystemMaxUse=1G
MaxRetentionSec=30day
EOF
cat > "$work/20auto-upgrades" << 'EOF'
// The Gen 3 box (#72 phase D, choice 1): no unattended upgrades; OS packages
// by hand in dev time. Written by scripts/gen3/provision-root.sh.
APT::Periodic::Update-Package-Lists "0";
APT::Periodic::Unattended-Upgrade "0";
EOF
cat > "$work/iemmixer-display.conf" << 'EOF'
# The Gen 3 box (#72 phase D, Live 4): no display output polling. The box has
# no screen; the i915 driver's poll (every ~10 s, waking the GPU) held a CPU
# for ~2 ms, and the engine's pump, woken onto that CPU, missed its blocks.
# Applied at every boot by systemd-tmpfiles. Written by
# scripts/gen3/provision-root.sh.
w /sys/module/drm_kms_helper/parameters/poll - - - - N
EOF

# --- The steps. ---

step_user() {
  local entry uid gid home shell ggid members
  if ! getent group "$SERVICE_USER" > /dev/null; then
    groupadd --system "$SERVICE_USER"
    changed=1
  fi
  if ! getent passwd "$SERVICE_USER" > /dev/null; then
    useradd --system --gid "$SERVICE_USER" --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin "$SERVICE_USER"
    changed=1
  fi
  entry="$(getent passwd "$SERVICE_USER")"
  IFS=: read -r _ _ uid gid _ home shell <<< "$entry"
  entry="$(getent group "$SERVICE_USER")"
  IFS=: read -r _ _ ggid members <<< "$entry"
  if [[ ! "$uid" =~ ^[0-9]+$ ]] || [ "$uid" -ge 1000 ]; then
    die "the user $SERVICE_USER is no system user: provision makes the service user and changes no other; look at it"
  fi
  [ "$gid" = "$ggid" ] || die "the user $SERVICE_USER's group is not the group $SERVICE_USER: look at it"
  case "$shell" in */nologin | /bin/false | /usr/bin/false) ;; *) die "the user $SERVICE_USER can log in: look at it" ;; esac
  [ ! -e "$R$home" ] || die "the user $SERVICE_USER has a home folder: look at it"
  case ",$members," in
    *",$admin,"*) ;;
    *)
      usermod --append --groups "$SERVICE_USER" "$admin"
      changed=1
      entry="$(getent group "$SERVICE_USER")"
      IFS=: read -r _ _ _ members <<< "$entry"
      case ",$members," in *",$admin,"*) ;; *) die "the admin is not in the group $SERVICE_USER after usermod" ;; esac
      ;;
  esac
}

step_folders() {
  put_dir 0755 root root /etc/iemmixer
  put_dir 0700 root root /etc/credstore.encrypted
  put_dir 0700 "$SERVICE_USER" "$SERVICE_USER" /var/lib/iemmixer/mint
}

# The first path under the stack's state folders (the folders included) that
# is not owned by the service user and its group, empty when none: the
# question verify_box.py's `state` check asks. -P: never through a link.
foreign_state() {
  find -P "${STATE_PATHS[@]}" -not -user "$SERVICE_USER" -print -quit \
    -o -not -group "$SERVICE_USER" -print -quit
}

step_state() {
  local dir found
  for dir in "${STATE_DIRS[@]}"; do
    if [ -L "$R$dir" ] || { [ -e "$R$dir" ] && [ ! -d "$R$dir" ]; }; then
      die "$dir is no folder: the stack's state is never taken over through it; look at it"
    fi
  done
  for dir in "${STATE_DIRS[@]}"; do
    put_dir 0750 "$SERVICE_USER" "$SERVICE_USER" "$dir"
  done
  found="$(foreign_state)" || die "the stack's state folders could not be searched (find)"
  [ -n "$found" ] || return 0
  chown -R -h "$SERVICE_USER:$SERVICE_USER" -- "${STATE_PATHS[@]}"
  changed=1
  found="$(foreign_state)" || die "the stack's state folders could not be searched (find) after chown"
  [ -z "$found" ] || die "the stack's state is not all the service user's after chown -R -h"
}

step_pepper() {
  local credential="$R$CREDENTIAL" mode
  if [ -e "$credential" ] || [ -L "$credential" ]; then
    if [ ! -f "$credential" ] || [ -L "$credential" ] || [ ! -s "$credential" ]; then
      die "$CREDENTIAL is there but no file or empty: it is never overwritten here; look at it"
    fi
  else
    (
      umask 077
      head -c 32 /dev/urandom | systemd-creds encrypt --name=pepper --with-key=auto - "$credential"
    )
    changed=1
    if [ ! -f "$credential" ] || [ ! -s "$credential" ]; then
      die "systemd-creds made no credential at $CREDENTIAL"
    fi
    echo "pepper: sealed by systemd-creds --with-key=auto (with the TPM2 when the box has one)"
  fi
  mode="$(stat -c '%a %U' -- "$credential")"
  [ "$mode" = "600 root" ] || die "$CREDENTIAL is $mode, not 600 root: look at it (never rewritten here)"
}

step_phase_c() {
  local unit dir reload=0
  for unit in "${STACK_UNITS[@]}"; do
    dir="$R/etc/systemd/system/$unit.d"
    if [ -e "$dir/10-user.conf" ] || [ -L "$dir/10-user.conf" ]; then
      rm -f -- "$dir/10-user.conf"
      rmdir --ignore-fail-on-non-empty -- "$dir"
      reload=1
      changed=1
    fi
  done
  if [ "$reload" = 1 ]; then
    systemctl daemon-reload
  fi
  if [ -e "$R$SECRETS/pepper.test" ] || [ -L "$R$SECRETS/pepper.test" ]; then
    rm -f -- "$R$SECRETS/pepper.test" "$R$SECRETS/pin_hashes.json"
    changed=1
    echo "phase-c: the plain test pepper and the PIN hashes made with it are gone: deploy.sh --pins gives every PIN again"
  fi
}

step_journal() {
  local restart state
  put_file 0644 root root "$JOURNAL_CONF" "$work/50-iemmixer.conf"
  restart=$wrote
  if [ ! -d "$R/var/log/journal" ]; then
    install -d -m 2755 -o root -g systemd-journal -- "$R/var/log/journal"
    restart=1
    changed=1
  fi
  if [ "$restart" = 1 ]; then
    systemctl restart systemd-journald.service
  fi
  state="$(package_status rsyslog)"
  case "$state" in
    absent | not-installed) ;;
    *)
      DEBIAN_FRONTEND=noninteractive apt-get purge -y rsyslog
      changed=1
      state="$(package_status rsyslog)"
      case "$state" in absent | not-installed) ;; *) die "rsyslog is '$state' after its purge" ;; esac
      ;;
  esac
}

step_apt() {
  local pattern out rc package want status
  local -a hold=()
  put_file 0644 root root "$AUTO_UPGRADES" "$work/20auto-upgrades"
  for pattern in "${KERNEL_PACKAGES[@]}"; do
    rc=0
    out="$(dpkg-query -W -f '${Package} ${Status}\n' "$pattern" 2> /dev/null)" || rc=$?
    [ "$rc" -le 1 ] || die "dpkg-query of the kernel's packages failed (exit $rc)"
    while read -r package want _ status; do
      if [ -z "$package" ] || [ "$status" != installed ]; then continue; fi
      [ "$want" = hold ] || hold+=("$package")
    done <<< "$out"
  done
  if [ "${#hold[@]}" -gt 0 ]; then
    apt-mark hold "${hold[@]}" > /dev/null
    changed=1
    for package in "${hold[@]}"; do
      out="$(dpkg-query -W -f '${Status}' "$package")"
      [ "${out%% *}" = hold ] || die "$package is not held after apt-mark hold"
    done
  fi
  mask_units "${APT_UNITS[@]}"
}

step_sleep() {
  mask_units "${SLEEP_UNITS[@]}"
}

step_swap() {
  local mem_kb size_g mode
  if swap_active; then
    return 0
  fi
  if [ ! -e "$R$SWAPFILE" ]; then
    mem_kb="$(awk '$1 == "MemTotal:" { print $2 }' "$R/proc/meminfo")"
    [[ "$mem_kb" =~ ^[0-9]+$ ]] || die "the box's memory could not be read (MemTotal of /proc/meminfo)"
    # The memory in whole GiB, rounded up, at least 2 and at most 8.
    size_g=$(((mem_kb + 1048575) / 1048576))
    if [ "$size_g" -lt 2 ]; then size_g=2; fi
    if [ "$size_g" -gt 8 ]; then size_g=8; fi
    (
      umask 077
      fallocate -l "${size_g}G" -- "$R$SWAPFILE"
    )
    mkswap -- "$R$SWAPFILE" > /dev/null
    echo "swap: a ${size_g} GiB swap file"
  fi
  mode="$(stat -c '%a %U' -- "$R$SWAPFILE")"
  [ "$mode" = "600 root" ] || die "$SWAPFILE is $mode, not 600 root: look at it"
  if ! awk -v f="$SWAPFILE" '$1 == f && $3 == "swap" { found = 1 } END { exit !found }' "$R/etc/fstab"; then
    printf '%s none swap sw 0 0\n' "$SWAPFILE" >> "$R/etc/fstab"
  fi
  swapon -- "$R$SWAPFILE"
  changed=1
  swap_active || die "no swap is active after swapon $SWAPFILE"
}

step_clock() {
  local unit
  if [ "$(timedatectl show --property=NTP --value)" != no ]; then
    timedatectl set-ntp false
    changed=1
  fi
  for unit in "${TIME_UNITS[@]}"; do
    unit_states "$unit"
    if is_enabled || is_running; then
      systemctl disable --now "$unit"
      changed=1
    fi
  done
  unit_states dantesync.service
  if [ "$file_state" != enabled ] || [ "$active_state" != active ]; then
    systemctl enable --now dantesync.service
    changed=1
    unit_states dantesync.service
    if [ "$file_state" != enabled ] || [ "$active_state" != active ]; then
      die "dantesync is $file_state and $active_state after its enable --now (journalctl -u dantesync on the box)"
    fi
  fi
}

# The interface deploy.sh wrote (IEM_DANTE_IFACE= of the env file, the last
# line of the key winning); never printed.
env_iface() {
  local line value="" found=0
  while IFS= read -r line || [ -n "$line" ]; do
    line="$(trim "$line")"
    case "$line" in IEM_DANTE_IFACE=*) ;; *) continue ;; esac
    value="$(trim "${line#*=}")"
    value="${value#\"}"
    value="${value%\"}"
    found=1
  done < "$R$ENVFILE"
  if [ "$found" != 1 ] || [[ ! "$value" =~ ^[A-Za-z0-9_.-]{1,15}$ ]]; then
    die "$ENVFILE has no IEM_DANTE_IFACE that is an interface name (deploy.sh --iface writes it)"
  fi
  printf '%s' "$value"
}

napi_check() {
  IEM_NAPI_SYS="$R/sys" IEM_NAPI_PROC="$R/proc" "$R$NAPI_HELPER" --check "$1"
}

step_napi() {
  local fresh iface
  put_file 0755 root root "$NAPI_HELPER" "$here/napi-priority"
  fresh=$wrote
  put_file 0644 root root "$NAPI_UNIT" "$here/iem-napi.service"
  if [ "$wrote" = 1 ]; then
    fresh=1
    systemctl daemon-reload
  fi
  unit_states irqbalance.service
  if [ "$load_state" != not-found ] && [ "$file_state" != masked ]; then
    systemctl mask --now irqbalance.service
    changed=1
    unit_states irqbalance.service
    if [ "$file_state" != masked ] || is_running; then
      die "irqbalance is $file_state and $active_state after its mask --now"
    fi
  fi
  unit_states iem-napi.service
  if [ "$file_state" != enabled ]; then
    systemctl enable iem-napi.service
    changed=1
    unit_states iem-napi.service
    [ "$file_state" = enabled ] || die "iem-napi is '$file_state' after its enable"
  fi
  if [ ! -f "$R$ENVFILE" ]; then
    echo "napi: no Dante interface yet ($ENVFILE, written by deploy.sh): the priority applies at the next boot, or at a provision run after the first deploy"
    return 0
  fi
  iface="$(env_iface)"
  if [ "$fresh" = 1 ] || [ "$active_state" != active ] || ! napi_check "$iface" 2> /dev/null; then
    systemctl restart iem-napi.service
    changed=1
  fi
  napi_check "$iface" ||
    die "the Dante interface's packet priority does not hold after iem-napi ran (above; journalctl -u iem-napi on the box)"
  unit_states iem-napi.service
  [ "$active_state" = active ] || die "iem-napi is '$active_state' after it ran, not active (journalctl -u iem-napi on the box)"
}

step_display() {
  put_file 0644 root root "$DISPLAY_CONF" "$work/iemmixer-display.conf"
  if [ ! -e "$R$POLL_PARAM" ]; then
    echo "display: no display output polling on this box (no $POLL_PARAM)"
    return 0
  fi
  [ "$(cat -- "$R$POLL_PARAM")" != N ] || return 0
  systemd-tmpfiles --create "$R$DISPLAY_CONF" ||
    die "systemd-tmpfiles --create $DISPLAY_CONF failed (exit $?)"
  changed=1
  [ "$(cat -- "$R$POLL_PARAM")" = N ] ||
    die "display output polling is still on after systemd-tmpfiles --create $DISPLAY_CONF ($POLL_PARAM is not N)"
}

step_stack() {
  local unit
  local -a todo=()
  for unit in "${STACK_UNITS[@]}"; do
    unit_states "$unit"
    if is_enabled; then todo+=("$unit"); fi
  done
  [ "${#todo[@]}" -gt 0 ] || return 0
  systemctl disable "${todo[@]}"
  changed=1
  for unit in "${todo[@]}"; do
    unit_states "$unit"
    if is_enabled; then die "$unit is still '$file_state' after its disable"; fi
  done
}

step_sudoers() {
  local final="$R$SUDOERS" staged="$R/etc/sudoers.d/.iemmixer-gen3.new"
  if [ -f "$final" ] && [ ! -L "$final" ] && cmp -s -- "$work/sudoers" "$final" &&
    [ "$(stat -c '%a %U %G' -- "$final")" = "440 root root" ]; then
    return 0
  fi
  # A name with a dot: sudo's includedir passes it over until the move.
  install -D -m 0440 -o root -g root -- "$work/sudoers" "$staged"
  if ! visudo -cf "$staged" > /dev/null; then
    rm -f -- "$staged"
    die "the sudoers rule does not pass visudo -cf: not moved into place"
  fi
  mv -f -T -- "$staged" "$final"
  changed=1
}

for step in user folders state pepper phase-c journal apt sleep swap clock napi display stack sudoers; do
  changed=0
  "step_${step//-/_}"
  if [ "$changed" = 1 ]; then
    echo "$step changed"
  else
    echo "$step unchanged"
  fi
done
