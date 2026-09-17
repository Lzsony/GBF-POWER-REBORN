#!/bin/sh
# Invoked through verified management SSH; this stage cannot require Python.
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
LC_ALL=C
export LC_ALL
GPR_OS_RELEASE=/etc/os-release
GPR_MACHINE_ID=/etc/machine-id
GPR_SYSTEMD=/run/systemd/system
GPR_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt
GPR_LOCK=/run/lock/gbf-reborn-dependencies.lock.d
GPR_LOG_DIR=/var/log/gbf-reborn-dependencies
GPR_TEMP_ROOT=/tmp

fail() { printf 'GPR-DEPS:%s\n' "$1" >&2; exit 1; }
capability() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf 'missing\t%s\t%s\n' "$1" "$2"
    fi
}
scan() {
    capability python3 python3
    capability openssl openssl
    capability ssh-keygen openssh-client
    capability ss iproute2
    capability useradd passwd
    capability runuser util-linux
    capability tar tar
    if [ ! -s "$GPR_CA_BUNDLE" ]; then printf 'missing\tca-bundle\tca-certificates\n'; fi
}
package_state() { dpkg-query -W -f '${db:Status-Status}' "$1" 2>/dev/null || true; }

[ "$(id -u)" = 0 ] || fail ROOT_REQUIRED
for tool in cat uname tr grep awk sha256sum mkdir rmdir mktemp chmod rm sleep timeout dpkg dpkg-query dpkg-deb apt-get systemctl journalctl; do
    command -v "$tool" >/dev/null 2>&1 || fail BASE_TOOLS_REQUIRED
done
[ -d "$GPR_SYSTEMD" ] || fail SYSTEMD_REQUIRED
[ -r "$GPR_OS_RELEASE" ] && [ -r "$GPR_MACHINE_ID" ] || fail HOST_FACTS_UNAVAILABLE
. "$GPR_OS_RELEASE"
case "$ID:$VERSION_ID" in debian:12|debian:13|ubuntu:24.04|ubuntu:26.04) ;; *) fail UNSUPPORTED_OS ;; esac
case "$(uname -m)" in x86_64) architecture=amd64 ;; aarch64) architecture=arm64 ;; *) fail UNSUPPORTED_ARCH ;; esac
machine=$(tr 'A-F' 'a-f' < "$GPR_MACHINE_ID" | tr -d '\n')
printf '%s' "$machine" | grep -Eq '^[0-9a-f]{32}$' || fail INVALID_MACHINE_ID
[ "$machine" != 00000000000000000000000000000000 ] || fail INVALID_MACHINE_ID
identity=$(printf '%s' "$machine" | sha256sum | awk '{print $1}')
printf 'host\t%s\t%s\t%s\t%s\n' "$ID" "$VERSION_ID" "$architecture" "$identity"
mode=${1:-probe}
case "$mode" in probe|install) ;; *) fail INVALID_MODE ;; esac
if [ "$mode" = probe ]; then
    scan
    printf 'ready\n'
    exit 0
fi
[ "${2:-}" = "$identity" ] || fail HOST_IDENTITY_CHANGED

# A mkdir lock works before util-linux/Python exist. Never remove another run's lock.
attempt=0
until mkdir -m 700 "$GPR_LOCK" 2>/dev/null; do
    attempt=$((attempt + 1))
    [ "$attempt" -le 120 ] || fail HOST_LOCK_TIMEOUT
    sleep 1
done
work=''
cleanup() {
    if [ -n "$work" ]; then rm -rf -- "$work"; fi
    rm -f -- "$GPR_LOCK/owner.pid"
    rmdir "$GPR_LOCK" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP
printf '%s\n' "$$" > "$GPR_LOCK/owner.pid"
locked_identity=$(tr 'A-F' 'a-f' < "$GPR_MACHINE_ID" | tr -d '\n' | sha256sum | awk '{print $1}')
[ "$locked_identity" = "$identity" ] || fail HOST_IDENTITY_CHANGED
missing=$(scan)
if [ -z "$missing" ]; then printf 'changed\tfalse\nready\n'; exit 0; fi
packages=$(printf '%s\n' "$missing" | awk -F '\t' '!seen[$3]++ {print $3}')
for package in $packages; do
    case "$(package_state "$package")" in
        installed) fail INSTALLED_PACKAGE_BROKEN ;;
        ''|not-installed|config-files) ;;
        *) fail PACKAGE_DATABASE_BROKEN ;;
    esac
done
audit=$(dpkg --audit) || fail PACKAGE_DATABASE_BROKEN
[ -z "$audit" ] || fail PACKAGE_DATABASE_BROKEN
umask 077
[ ! -L "$GPR_LOG_DIR" ] || fail LOG_PATH_CONFLICT
mkdir -p "$GPR_LOG_DIR"
# Only a root-owned, non-writable-by-others log directory is accepted.
command -v stat >/dev/null 2>&1 || fail BASE_TOOLS_REQUIRED
[ "$(stat -c %u "$GPR_LOG_DIR")" = 0 ] || fail LOG_PATH_CONFLICT
[ "$(stat -c %a "$GPR_LOG_DIR")" = 700 ] || fail LOG_PATH_CONFLICT
log=$(mktemp "$GPR_LOG_DIR/install.XXXXXX")
printf 'log\t%s\n' "$log"
work=$(mktemp -d "$GPR_TEMP_ROOT/gpr-deps.XXXXXX")
DEBIAN_FRONTEND=noninteractive
export DEBIAN_FRONTEND
apt_failure() {
    if grep -Eqi 'could not get lock|unable to acquire.*lock|waiting for.*lock' "$log"; then fail APT_LOCK_TIMEOUT; fi
    fail "$1"
}
if ! apt-get -o DPkg::Lock::Timeout=120 -o Acquire::Retries=0 -o APT::Update::Error-Mode=any update >>"$log" 2>&1; then
    apt_failure APT_UPDATE_FAILED
fi
# shellcheck disable=SC2086 -- names come exclusively from scan's fixed package map.
if ! apt-get -s --no-remove --no-upgrade --no-install-recommends install $packages >"$work/simulation" 2>>"$log"; then
    cat "$work/simulation" >>"$log"
    apt_failure APT_SIMULATION_FAILED
fi
cat "$work/simulation" >>"$log"
if grep -Eq '^Remv |^Inst [^ ]+ \[' "$work/simulation"; then fail EXISTING_PACKAGE_CHANGE; fi

# Recheck the real transaction after APT acquires its package lock. Simulation
# alone is not a guarantee: the package database may change between commands.
cat >"$work/guard" <<'GUARD'
#!/bin/sh
set -eu
while IFS= read -r archive; do
    [ -n "$archive" ] || continue
    package=$(dpkg-deb -f "$archive" Package)
    architecture=$(dpkg-deb -f "$archive" Architecture)
    state=$(dpkg-query -W -f '${db:Status-Status}' "$package:$architecture" 2>/dev/null || dpkg-query -W -f '${db:Status-Status}' "$package" 2>/dev/null || true)
    case "$state" in ''|not-installed|config-files) ;; *) echo 'GPR-DEPS:EXISTING_PACKAGE_CHANGE' >&2; exit 1 ;; esac
done
GUARD
chmod 700 "$work/guard"
# APT itself rejects removal; the pre-install hook rejects upgrades/reinstalls.
# shellcheck disable=SC2086
if ! apt-get -y --no-remove --no-upgrade --no-install-recommends \
    -o DPkg::Lock::Timeout=120 -o Acquire::Retries=0 \
    -o 'Dpkg::Options::=--force-confdef' -o 'Dpkg::Options::=--force-confold' \
    -o "DPkg::Pre-Install-Pkgs::=$work/guard" install $packages >>"$log" 2>&1; then
    if grep -q 'GPR-DEPS:EXISTING_PACKAGE_CHANGE' "$log"; then fail EXISTING_PACKAGE_CHANGE; fi
    apt_failure APT_INSTALL_FAILED
fi
missing=$(scan)
if [ -n "$missing" ]; then printf '%s\n' "$missing"; fail STILL_MISSING; fi
printf 'changed\ttrue\nready\n'
