#!/usr/bin/env bash
# The remote commands name this script's own constant paths, expanded here on purpose.
# shellcheck disable=SC2029
set -euo pipefail
# Puts dantesync on the Gen 3 box as its one PTP owner, a follower of the
# band's audio grandmaster, and reads it back (#72 phase B, plan Task 2;
# .claude/rules/gen3.md). The device reads PTP time from it (UDP 31900) and
# never opens UDP 319/320 itself.
#
#   install-dantesync.sh --host <user@host> --version <vX.Y.Z>
#                        --sha256 <the release asset's sha256>
#                        --config <the private follower config.json>
#
# - The release is pinned (the fleet's DANTESYNC_VERSION_PIN, read by hand
#   from camera-box), never "latest" and never a source build: the dev box
#   downloads <tag>/dantesync-linux-amd64 and its .sha256 from the release
#   ($DANTESYNC_RELEASE_BASE, a test seam), and both the release's .sha256 and
#   the downloaded binary must equal --sha256 before anything reaches the box
#   (camera-box's dantesync-fleet-upgrade.sh order).
# - Refused (exit 1, nothing changed on the box) while the "ide event" flag
#   exists ($IEMMIXER_EVENT_NOW, else ~/.config/iemmixer/EVENT-NOW; again
#   right before the install), when ptp4l, phc2sys or statime is installed
#   (a binary or a unit), when anything but dantesync holds UDP 319 or 320
#   (`sudo -n ss -Hulnp`; dantesync's own sockets are an update), when the
#   box has no python3 (the read back's probe), and when the staged binary's
#   --version is not the tag's.
# - The config must be a follower (exit 2 otherwise, values never printed):
#   ntp_server set (the fleet's NTP master), ntp_server_mode.enabled false,
#   system.gm_allowlist not empty (the audio grandmaster),
#   system.clock_discipline absent or ptp_phase_lock (only that mode carries
#   the date offset the media clock needs), http_status on 8898.
# - Installed as dantesync's install.sh does it: /usr/local/bin/dantesync,
#   /etc/dantesync/config.json, its unit (root, Restart=always, FIFO 50),
#   daemon-reload; the other time services it disables (systemd-timesyncd,
#   chrony, ntp, time-sync-coordinator: those present, `disable --now`) and
#   `timedatectl set-ntp false`; then enable and restart dantesync. Each
#   step is `sudo -n`, from ~/.cache/dantesync-install (overwritten, never
#   deleted); unlike install.sh nothing failing is ignored.
# - Read back: the installed sha256, `dantesync --version`, `systemctl
#   is-active dantesync` after GEN3_SETTLE_S (3) seconds, then
#   dantesync_probe.py on the box (python3 -, within GEN3_DANTESYNC_WAIT_S,
#   300): a DSYX reply on UDP 31900, locked, with the date-offset extension,
#   and the HTTP status on 8898 a follower of an allowed grandmaster.
#
# dantesync keeps running through events (receive-only, as the fleet's other
# followers); "ide event" stops only iem-dante. Any failure stops the script:
# exit 1, 2 for its command line or config. Site values (host, user, NTP
# master, grandmaster) come only from the arguments and the private config;
# none is printed or in this repository (P6).

RELEASE_BASE="${DANTESYNC_RELEASE_BASE:-https://github.com/zbynekdrlik/dantesync/releases/download}"
ASSET=dantesync-linux-amd64
STAGE=.cache/dantesync-install
BIN=/usr/local/bin/dantesync
CONFIG=/etc/dantesync/config.json
UNIT=/etc/systemd/system/dantesync.service
# The time services dantesync's install.sh stops and disables, besides ptp4l
# and phc2sys (refused before: a second PTP daemon is never replaced here).
TIME_UNITS="systemd-timesyncd chrony ntp time-sync-coordinator"
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10)

die() {
  echo "install-dantesync.sh: $*" >&2
  exit 1
}

usage() {
  echo "install-dantesync.sh: $*" >&2
  echo "usage: install-dantesync.sh --host <user@host> --version <vX.Y.Z> --sha256 <hex> --config <path>" >&2
  exit 2
}

host="" version="" sha256="" config=""
while [ $# -gt 0 ]; do
  case "$1" in
    --host | --version | --sha256 | --config)
      [ $# -ge 2 ] || usage "$1 needs a value"
      case "$1" in
        --host) host="$2" ;;
        --version) version="$2" ;;
        --sha256) sha256="$2" ;;
        --config) config="$2" ;;
      esac
      shift 2
      ;;
    *) usage "unknown argument '$1'" ;;
  esac
done
for name in host version sha256 config; do
  [ -n "${!name}" ] || usage "--$name is missing"
done
case "$host" in -*) usage "--host must be an ssh destination, not an option" ;; esac
[[ "$version" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] || usage "--version is not a release tag vX.Y.Z"
semver="${BASH_REMATCH[1]}"
[[ "$sha256" =~ ^[0-9a-f]{64}$ ]] || usage "--sha256 is not 64 lowercase hex digits"
[ -f "$config" ] || usage "--config is not a file"

# The config is a follower's (its values are never printed: P6).
why="$(python3 - "$config" <<'PY'
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as f:
        c = json.load(f)
except (OSError, ValueError) as e:
    print(f"is not JSON ({type(e).__name__})")
    sys.exit(0)
def problem():
    if not isinstance(c, dict):
        return "is not a JSON object"
    if not isinstance(c.get("ntp_server"), str) or not c["ntp_server"].strip():
        return "has no ntp_server (the fleet's NTP master)"
    mode = c.get("ntp_server_mode")
    if not isinstance(mode, dict) or mode.get("enabled") is not False:
        return "is not a follower's: ntp_server_mode.enabled must be false"
    status = c.get("http_status")
    if not isinstance(status, dict) or status.get("enabled") is not True or status.get("port") != 8898:
        return "must keep http_status enabled on port 8898 (the read back)"
    system = c.get("system")
    allow = system.get("gm_allowlist") if isinstance(system, dict) else None
    if not isinstance(allow, list) or not allow or not all(isinstance(a, str) and a.strip() for a in allow):
        return "has no system.gm_allowlist (the audio grandmaster)"
    discipline = system.get("clock_discipline", "ptp_phase_lock")
    if not isinstance(discipline, str) or discipline.strip().lower() != "ptp_phase_lock":
        return "must keep system.clock_discipline ptp_phase_lock (only it carries the date offset)"
    return ""
print(problem())
PY
)" || usage "--config could not be checked (python3 above)"
[ -z "$why" ] || usage "--config $why"

flag="${IEMMIXER_EVENT_NOW:-$HOME/.config/iemmixer/EVENT-NOW}"
refuse_event() {
  [ ! -e "$flag" ] || die "$flag exists: an event is on; dantesync is installed only in dev time ($1)"
}
refuse_event "nothing was downloaded or changed"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
probe="$here/dantesync_probe.py"
[ -f "$probe" ] || die "$probe is missing"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
url="$RELEASE_BASE/$version/$ASSET"
curl -fsSL -o "$work/dantesync" "$url" || die "the download of $ASSET $version failed: nothing was copied"
curl -fsSL -o "$work/dantesync.sha256" "$url.sha256" ||
  die "the download of $ASSET.sha256 $version failed: nothing was copied"
released="$(awk 'NR == 1 { print tolower($1) }' "$work/dantesync.sha256")"
[ "$released" = "$sha256" ] ||
  die "the release's $ASSET.sha256 names '$released', not --sha256 $sha256: nothing was copied"
actual="$(sha256sum "$work/dantesync" | cut -d' ' -f1)"
[ "$actual" = "$sha256" ] || die "the downloaded $ASSET's sha256 is $actual, not $sha256: nothing was copied"
chmod 0755 "$work/dantesync"
cp -- "$config" "$work/config.json"
# The unit exactly as dantesync's install.sh writes it.
cat > "$work/dantesync.service" <<EOF
[Unit]
Description=DanteSync PTP Time Sync Service v$semver
After=network-online.target
Wants=network-online.target

[Service]
# Run as root for port 319, adjtimex, and RTC ioctl access
User=root
Group=root
ExecStart=$BIN
Restart=always
RestartSec=5
# High priority for timestamping accuracy
CPUSchedulingPolicy=fifo
CPUSchedulingPriority=50

[Install]
WantedBy=multi-user.target
EOF

# What the box holds now (read only).
survey="$(ssh "${SSH_OPTS[@]}" "$host" ": install-dantesync survey
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
for name in ptp4l phc2sys statime; do
  if command -v \"\$name\" > /dev/null; then echo \"daemon \$name\"; fi
  if [ \"\$(systemctl show --property=LoadState --value \"\$name.service\")\" = loaded ]; then echo \"daemon \$name.service\"; fi
done
for unit in $TIME_UNITS; do
  if [ \"\$(systemctl show --property=LoadState --value \"\$unit.service\")\" = loaded ]; then echo \"time \$unit\"; fi
done
if command -v python3 > /dev/null; then echo python3; fi")" ||
  die "the survey of the box failed: nothing was copied"
daemons="$(awk '$1 == "daemon" { printf "%s%s", sep, $2; sep = ", " }' <<< "$survey")"
[ -z "$daemons" ] ||
  die "a PTP daemon is installed on the box ($daemons): dantesync must be its one PTP owner; remove it first: nothing was copied"
grep -qx python3 <<< "$survey" || die "the box has no python3, which the read back runs: nothing was copied"
present="$(awk '$1 == "time" { printf "%s%s", sep, $2; sep = " " }' <<< "$survey")"

sockets="$(ssh "${SSH_OPTS[@]}" "$host" sudo -n ss -Hulnp)" ||
  die "the box's sudo does not allow 'sudo -n ss -Hulnp' (who holds UDP 319/320): add it to the sudoers rule (.claude/rules/gen3.md): nothing was copied"
# Each socket line: state, queues, local address, peer, users:(("name",pid=…,fd=…),…).
held="" own=""
while read -r _ _ _ local _ users; do
  case "$local" in *:319 | *:320) ;; *) continue ;; esac
  names="" others="" rest="$users"
  while [[ "$rest" =~ \"([^\"]*)\"(.*) ]]; do
    names="$names ${BASH_REMATCH[1]}"
    [ "${BASH_REMATCH[1]}" = dantesync ] || others="$others ${BASH_REMATCH[1]}"
    rest="${BASH_REMATCH[2]}"
  done
  if [ -n "$names" ] && [ -z "$others" ]; then
    own=1
  else
    others="${others# }"
    held="${held:+$held, }UDP ${local##*:} by ${others:-an unknown process}"
  fi
done <<< "$sockets"
[ -z "$held" ] ||
  die "UDP 319/320 is already held on the box ($held): dantesync must be its one PTP owner: nothing was copied"
[ -z "$own" ] || echo "install-dantesync.sh: dantesync already holds UDP 319/320: updating it"

ssh "${SSH_OPTS[@]}" "$host" mkdir -p "$STAGE"
scp -q "${SSH_OPTS[@]}" "$work/dantesync" "$work/config.json" "$work/dantesync.service" "$host:$STAGE/"
echo "install-dantesync.sh: copied to the box's ~/$STAGE"

staged="$(ssh "${SSH_OPTS[@]}" "$host" "$STAGE/dantesync" --version)" ||
  die "the staged dantesync did not run (--version): nothing installed"
[ "$(grep -oE '[0-9]+\.[0-9]+\.[0-9]+' <<< "$staged" | tail -n 1)" = "$semver" ] ||
  die "the staged dantesync says '$staged', not $semver: nothing installed"

refuse_event "copied to ~/$STAGE, nothing installed"
disable=""
for unit in $present; do
  disable="$disable
sudo -n systemctl disable --now $unit"
done
ssh "${SSH_OPTS[@]}" "$host" "set -e
sudo -n install -D -m 0755 $STAGE/dantesync $BIN
sudo -n install -D -m 0644 $STAGE/config.json $CONFIG
sudo -n install -D -m 0644 $STAGE/dantesync.service $UNIT
sudo -n systemctl daemon-reload$disable
sudo -n timedatectl set-ntp false
sudo -n systemctl enable dantesync
sudo -n systemctl restart dantesync"
echo "install-dantesync.sh: installed; dantesync restarted${present:+; disabled: $present}"

installed="$(ssh "${SSH_OPTS[@]}" "$host" sha256sum "$BIN" | cut -d' ' -f1)"
[ "$installed" = "$sha256" ] || die "the installed dantesync's sha256 is $installed, not $sha256"
said="$(ssh "${SSH_OPTS[@]}" "$host" "$BIN" --version)"
[ "$(grep -oE '[0-9]+\.[0-9]+\.[0-9]+' <<< "$said" | tail -n 1)" = "$semver" ] ||
  die "$BIN --version said '$said', not $semver"
sleep "${GEN3_SETTLE_S:-3}"
active="$(ssh "${SSH_OPTS[@]}" "$host" systemctl is-active dantesync)" || true
[ "$active" = active ] || die "dantesync is '$active' after ${GEN3_SETTLE_S:-3} s, not active (journalctl -u dantesync on the box)"
wait_s="${GEN3_DANTESYNC_WAIT_S:-300}"
[[ "$wait_s" =~ ^[0-9]+$ ]] || die "GEN3_DANTESYNC_WAIT_S is not a whole number of seconds"
reading="$(ssh "${SSH_OPTS[@]}" "$host" python3 - --wait "$wait_s" < "$probe")" ||
  die "dantesync is installed and active but not yet a locked follower of an allowed grandmaster within $wait_s s (${reading:-no reading}; the reasons above); read again: ssh <host> python3 - --wait 600 < scripts/gen3/dantesync_probe.py"
echo "install-dantesync.sh: $said; sha256 $installed; dantesync active; $reading"
