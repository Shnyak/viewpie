#!/usr/bin/env bash
# Apply a candidate Viewpie YAML config transactionally.
# Install this script root-owned and invoke it only from a trusted local supervisor.
set -Eeuo pipefail
umask 077

if [[ $# -lt 2 || $# -gt 5 ]]; then
  echo "usage: viewpie-apply-config CANDIDATE CONFIG_PATH [SERVICE] [BINARY] [HEALTH_URL]" >&2
  exit 64
fi

candidate=$1
config_path=$2
service=${3:-viewpie.service}
binary=${4:-/usr/local/bin/viewpie}
health_url=${5:-http://127.0.0.1:8080/api/status}

die() { echo "ERROR: $*" >&2; exit 1; }

wait_for_health() {
  local url=$1
  for _ in $(seq 1 30); do
    if curl --fail --silent --show-error --max-time 2 "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  return 1
}
[[ -f "$candidate" && -r "$candidate" ]] || die "candidate file is missing or unreadable"
[[ -x "$binary" ]] || die "Viewpie binary is not executable: $binary"
[[ -f "$config_path" ]] || die "current config does not exist: $config_path"

config_dir=$(dirname -- "$config_path")
backup="${config_path}.backup.$(date -u +%Y%m%dT%H%M%SZ)"
candidate_copy=$(mktemp "$config_dir/.viewpie-candidate.XXXXXX")
restore_copy=$(mktemp "$config_dir/.viewpie-restore.XXXXXX")
cleanup() {
  rm -f -- "$candidate_copy" "$restore_copy"
}
trap cleanup EXIT

# Keep candidate on the same filesystem as the target for atomic rename.
install -m 0600 -- "$candidate" "$candidate_copy"
"$binary" --validate-config "$candidate_copy" || die "candidate failed Viewpie validation; active config was not changed"

# Backup before stopping or touching the active file.
cp -p -- "$config_path" "$backup" || die "could not create backup"
if ! systemctl stop "$service"; then
  die "could not stop $service; active config was not changed"
fi

# Replace the config atomically while the service is stopped.
if ! mv -f -- "$candidate_copy" "$config_path"; then
  systemctl start "$service" || true
  die "could not install candidate config; attempted to restart existing service"
fi

if systemctl start "$service" && wait_for_health "$health_url"; then
  echo "OK: configuration applied; backup=$backup"
  exit 0
fi

echo "ERROR: new configuration did not become healthy; restoring backup=$backup" >&2
systemctl stop "$service" || true
install -m 0600 -- "$backup" "$restore_copy" || die "CRITICAL: backup exists but could not be staged for restore"
mv -f -- "$restore_copy" "$config_path" || die "CRITICAL: backup exists but could not be restored"
systemctl start "$service" || die "CRITICAL: old config restored but service failed to start"
wait_for_health "$health_url" || die "CRITICAL: old config restored but health check still fails"
echo "ROLLED_BACK: previous configuration restored and service is healthy" >&2
exit 2

