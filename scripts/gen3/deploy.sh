#!/usr/bin/env bash
# The remote commands name this script's own constant paths, expanded here on purpose.
# shellcheck disable=SC2029
set -euo pipefail
# Puts a CI build of iem-dante-dev on the Gen 3 box and (re)starts its
# service (#72 phase A, plan Task 5; .claude/rules/gen3.md):
#
#   deploy.sh --host <user@host> --artifact <iem-dante-dev from CI>
#             --sha256 <its sha256> --config <the private device config>
#             --iface <the box's Dante interface>
#
# - Dev time only: refused while the "ide event" flag exists (iempc's
#   EVENT-NOW: $IEMMIXER_EVENT_NOW, else ~/.config/iemmixer/EVENT-NOW), and
#   checked again right before anything is installed.
# - The artifact must have the given sha256 (the CI job `dante-bin` prints
#   it; the artifact is `iem-dante-dev-<sha>`).
# - The binary, the unit (beside this script), the config, the interface
#   (written as /etc/iemmixer/iem-dante.env) and the drop-in naming the box's
#   user (the ssh user, never root) go by scp into ~/.cache/iem-dante-deploy
#   on the box, then `sudo -n install` puts each in place, and `sudo -n
#   systemctl` reloads, enables and restarts iem-dante.
# - Read back: the installed binary's sha256, its --version, and `systemctl
#   is-active iem-dante` after GEN3_SETTLE_S (3) seconds, which must say
#   active.
#
# Any failure stops the script: exit 1, 2 for its command line. Site values
# (host, user, interface, names) come only from the arguments and the
# private config; none is in this repository (P6).

STAGE=.cache/iem-dante-deploy
BIN=/opt/iemmixer/bin/iem-dante-dev
UNIT=/etc/systemd/system/iem-dante.service
DROPIN=/etc/systemd/system/iem-dante.service.d/10-user.conf
CONFIG=/etc/iemmixer/dante-device.toml
ENVFILE=/etc/iemmixer/iem-dante.env
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10)

die() {
  echo "deploy.sh: $*" >&2
  exit 1
}

usage() {
  echo "deploy.sh: $*" >&2
  echo "usage: deploy.sh --host <user@host> --artifact <path> --sha256 <hex> --config <path> --iface <name>" >&2
  exit 2
}

host="" artifact="" sha256="" config="" iface=""
while [ $# -gt 0 ]; do
  case "$1" in
    --host | --artifact | --sha256 | --config | --iface)
      [ $# -ge 2 ] || usage "$1 needs a value"
      case "$1" in
        --host) host="$2" ;;
        --artifact) artifact="$2" ;;
        --sha256) sha256="$2" ;;
        --config) config="$2" ;;
        --iface) iface="$2" ;;
      esac
      shift 2
      ;;
    *) usage "unknown argument '$1'" ;;
  esac
done
for name in host artifact sha256 config iface; do
  [ -n "${!name}" ] || usage "--$name is missing"
done
case "$host" in -*) usage "--host must be an ssh destination, not an option" ;; esac
[[ "$sha256" =~ ^[0-9a-f]{64}$ ]] || usage "--sha256 is not 64 lowercase hex digits"
[[ "$iface" =~ ^[A-Za-z0-9_.-]{1,15}$ ]] || usage "--iface is not an interface name"
[ -f "$artifact" ] || usage "--artifact is not a file"
[ -f "$config" ] || usage "--config is not a file"

flag="${IEMMIXER_EVENT_NOW:-$HOME/.config/iemmixer/EVENT-NOW}"
refuse_event() {
  [ ! -e "$flag" ] || die "$flag exists: an event is on; the Gen 3 device is deployed only in dev time ($1)"
}
refuse_event "nothing was copied"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
unit="$here/iem-dante.service"
[ -f "$unit" ] || die "$unit is missing"
actual="$(sha256sum "$artifact" | cut -d' ' -f1)"
[ "$actual" = "$sha256" ] || die "the artifact's sha256 is $actual, not $sha256: nothing was copied"

user="$(ssh "${SSH_OPTS[@]}" "$host" id -un)"
[[ "$user" =~ ^[a-z_][a-z0-9_.-]*$ ]] || die "the box's user is not a plain user name: nothing was copied"
[ "$user" != root ] || die "the service runs as the box's admin user, not root: nothing was copied"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
printf 'IEM_DANTE_IFACE=%s\n' "$iface" > "$work/iem-dante.env"
printf '[Service]\nUser=%s\n' "$user" > "$work/10-user.conf"
cp -- "$artifact" "$work/iem-dante-dev"
cp -- "$config" "$work/dante-device.toml"
cp -- "$unit" "$work/iem-dante.service"

ssh "${SSH_OPTS[@]}" "$host" mkdir -p "$STAGE"
scp -q "${SSH_OPTS[@]}" "$work/iem-dante-dev" "$work/iem-dante.service" "$work/10-user.conf" \
  "$work/dante-device.toml" "$work/iem-dante.env" "$host:$STAGE/"
echo "deploy.sh: copied to the box's ~/$STAGE"

refuse_event "copied to ~/$STAGE, nothing installed"
ssh "${SSH_OPTS[@]}" "$host" "set -e
sudo -n install -D -m 0755 $STAGE/iem-dante-dev $BIN
sudo -n install -D -m 0644 $STAGE/iem-dante.service $UNIT
sudo -n install -D -m 0644 $STAGE/10-user.conf $DROPIN
sudo -n install -D -m 0644 $STAGE/dante-device.toml $CONFIG
sudo -n install -D -m 0644 $STAGE/iem-dante.env $ENVFILE
sudo -n systemctl daemon-reload
sudo -n systemctl enable iem-dante
sudo -n systemctl restart iem-dante"
echo "deploy.sh: installed; iem-dante restarted"

installed="$(ssh "${SSH_OPTS[@]}" "$host" sha256sum "$BIN" | cut -d' ' -f1)"
[ "$installed" = "$sha256" ] || die "the installed binary's sha256 is $installed, not $sha256"
version="$(ssh "${SSH_OPTS[@]}" "$host" "$BIN" --version)"
[[ "$version" == "iem-dante-dev "* ]] || die "$BIN --version said '$version'"
sleep "${GEN3_SETTLE_S:-3}"
active="$(ssh "${SSH_OPTS[@]}" "$host" systemctl is-active iem-dante)" || true
[ "$active" = active ] || die "iem-dante is '$active' after ${GEN3_SETTLE_S:-3} s, not active (journalctl -u iem-dante on the box)"
echo "deploy.sh: $version; sha256 $installed; iem-dante active"
