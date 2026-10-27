#!/usr/bin/env bash
# The remote commands name this script's own constant paths, expanded here on purpose.
# shellcheck disable=SC2029
set -euo pipefail
# Puts Cloudflare's tunnel connector (cloudflared) on the Gen 3 box with its
# unit, iem-tunnel.service, installed and never enabled or started (#72 phase
# E, plan docs/superpowers/plans/2026-10-27-gen3-phase-e-band-test.md Task 6;
# .claude/rules/gen3.md). The switch tool starts it while the box serves the
# band, once the PC's connector stopped: exactly one connector of the band's
# tunnel runs at a time.
#
#   install-cloudflared.sh --host <user@host> --version <X.Y.Z>
#                          --sha256 <the release .deb's sha256>
#
# - The release is pinned, never "latest", and no apt source is added to the
#   box: the dev box downloads <version>/cloudflared-linux-amd64.deb from
#   cloudflared's GitHub releases ($CLOUDFLARED_RELEASE_BASE, a test seam;
#   cloudflared's tags have no "v") and its sha256 must equal --sha256 before
#   anything reaches the box (the release names its sums in its notes, not
#   in a file).
# - Refused (exit 1, nothing changed on the box) while the "ide event" flag
#   exists ($IEMMIXER_EVENT_NOW, else ~/.config/iemmixer/EVENT-NOW; again
#   right before the install), and when the staged files' sha256 on the box
#   are not the downloaded .deb's and this checkout's unit's.
# - Installed from ~/.cache/cloudflared-install (overwritten, never deleted),
#   each step `sudo -n` (sudo-lines.txt): `dpkg -i` of the .deb (its binary is
#   /usr/bin/cloudflared; its postinst starts nothing), `apt-mark hold
#   cloudflared` (no apt run replaces the pin), `install -D -m 0644` of the
#   unit to /etc/systemd/system/iem-tunnel.service, `systemctl
#   daemon-reload`. Never `enable`, never `start`: a connector that runs stays
#   as it is, the new binary and unit apply at its next start.
# - Read back: `cloudflared --version` names --version, `apt-mark showhold`
#   lists cloudflared, the installed unit's sha256 is this checkout's.
#
# Any failure stops the script: exit 1, 2 for its command line. The box's
# host and user come only from the arguments (P6). The tunnel's token is no
# part of this: a later tool makes it the unit's credential. Nothing printed
# is secret: fixed words, the version, sums.

RELEASE_BASE="${CLOUDFLARED_RELEASE_BASE:-https://github.com/cloudflare/cloudflared/releases/download}"
ASSET=cloudflared-linux-amd64.deb
STAGE=.cache/cloudflared-install
BIN=/usr/bin/cloudflared
UNIT_NAME=iem-tunnel.service
UNIT=/etc/systemd/system/$UNIT_NAME
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10)

die() {
  echo "install-cloudflared.sh: $*" >&2
  exit 1
}

usage() {
  echo "install-cloudflared.sh: $*" >&2
  echo "usage: install-cloudflared.sh --host <user@host> --version <X.Y.Z> --sha256 <hex>" >&2
  exit 2
}

host="" version="" sha256=""
while [ $# -gt 0 ]; do
  case "$1" in
    --host | --version | --sha256)
      [ $# -ge 2 ] || usage "$1 needs a value"
      case "$1" in
        --host) host="$2" ;;
        --version) version="$2" ;;
        --sha256) sha256="$2" ;;
      esac
      shift 2
      ;;
    *) usage "unknown argument '$1'" ;;
  esac
done
for name in host version sha256; do
  [ -n "${!name}" ] || usage "--$name is missing"
done
case "$host" in -*) usage "--host must be an ssh destination, not an option" ;; esac
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage "--version is not a release X.Y.Z (cloudflared's tags have no v)"
[[ "$sha256" =~ ^[0-9a-f]{64}$ ]] || usage "--sha256 is not 64 lowercase hex digits"

flag="${IEMMIXER_EVENT_NOW:-$HOME/.config/iemmixer/EVENT-NOW}"
refuse_event() {
  [ ! -e "$flag" ] || die "$flag exists: an event is on; cloudflared is installed only in dev time ($1)"
}
refuse_event "nothing was downloaded or changed"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ -f "$here/$UNIT_NAME" ] || die "$here/$UNIT_NAME is missing"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
curl -fsSL -o "$work/$ASSET" "$RELEASE_BASE/$version/$ASSET" ||
  die "the download of $ASSET $version failed: nothing was copied"
actual="$(sha256sum "$work/$ASSET" | cut -d' ' -f1)"
[ "$actual" = "$sha256" ] || die "the downloaded $ASSET's sha256 is $actual, not $sha256: nothing was copied"
cp -- "$here/$UNIT_NAME" "$work/$UNIT_NAME"
unit_sha="$(sha256sum "$work/$UNIT_NAME" | cut -d' ' -f1)"

ssh "${SSH_OPTS[@]}" "$host" mkdir -p "$STAGE" || die "the box's ~/$STAGE could not be made: nothing was copied"
scp -q "${SSH_OPTS[@]}" "$work/$ASSET" "$work/$UNIT_NAME" "$host:$STAGE/" ||
  die "the copy to the box's ~/$STAGE failed: nothing installed"
echo "install-cloudflared.sh: copied to the box's ~/$STAGE"

staged="$(ssh "${SSH_OPTS[@]}" "$host" sha256sum "$STAGE/$ASSET" "$STAGE/$UNIT_NAME")" ||
  die "the staged files' sha256 could not be read on the box: nothing installed"
want="$sha256  $STAGE/$ASSET
$unit_sha  $STAGE/$UNIT_NAME"
[ "$staged" = "$want" ] ||
  die "the files staged on the box are not the downloaded $ASSET and this checkout's $UNIT_NAME (sha256): nothing installed"

refuse_event "copied to ~/$STAGE, nothing installed"
# dpkg's and apt-mark's own lines are not printed (their errors are).
ssh "${SSH_OPTS[@]}" "$host" "set -e
sudo -n dpkg -i $STAGE/$ASSET
sudo -n apt-mark hold cloudflared
sudo -n install -D -m 0644 $STAGE/$UNIT_NAME $UNIT
sudo -n systemctl daemon-reload" > /dev/null ||
  die "the install on the box stopped (its error above); the steps before it stay done: run again once fixed"

said="$(ssh "${SSH_OPTS[@]}" "$host" "$BIN" --version)" || die "$BIN --version did not run on the box"
said_version="$(grep -oE '[0-9]+\.[0-9]+\.[0-9]+' <<< "$said" | head -n 1 || true)"
[ "$said_version" = "$version" ] || die "$BIN --version names '${said_version:-no version}', not $version"
holds="$(ssh "${SSH_OPTS[@]}" "$host" apt-mark showhold)" || die "apt-mark showhold failed on the box"
grep -qx cloudflared <<< "$holds" ||
  die "apt-mark showhold does not list cloudflared: an apt run could replace the pinned release"
installed="$(ssh "${SSH_OPTS[@]}" "$host" sha256sum "$UNIT")" || die "the installed $UNIT could not be read"
installed="${installed%% *}"
[ "$installed" = "$unit_sha" ] || die "the installed $UNIT's sha256 is $installed, not this checkout's $unit_sha"
echo "install-cloudflared.sh: cloudflared $version installed and held; $UNIT_NAME installed, not enabled, not started"
