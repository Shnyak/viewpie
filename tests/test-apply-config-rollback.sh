#!/usr/bin/env bash
set -Eeuo pipefail

# Integration-style regression test for rollback metadata preservation.
# Uses temporary files and stubbed systemctl/curl; does not touch the host service.
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
bin_dir="$tmp/bin"
mkdir -p "$bin_dir" "$tmp/config"
state="$tmp/start-count"

cat >"$bin_dir/systemctl" <<'STUB'
#!/usr/bin/env bash
set -eu
case "$1" in
  stop) exit 0 ;;
  start) n=0; [[ -f "$STATE_FILE" ]] && n=$(cat "$STATE_FILE"); echo $((n + 1)) >"$STATE_FILE" ;;
  *) exit 2 ;;
esac
STUB
cat >"$bin_dir/curl" <<'STUB'
#!/usr/bin/env bash
set -eu
n=0
[[ -f "$STATE_FILE" ]] && n=$(cat "$STATE_FILE")
# The first start is the candidate; fail its health check. The second is rollback.
[[ "$n" -ge 2 ]]
STUB
cat >"$bin_dir/sleep" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
cat >"$tmp/viewpie" <<'STUB'
#!/usr/bin/env bash
set -eu
[[ "${1:-}" == "--validate-config" ]] && exit 0
exit 2
STUB
chmod +x "$bin_dir/systemctl" "$bin_dir/curl" "$bin_dir/sleep" "$tmp/viewpie"

export STATE_FILE="$state"
export PATH="$bin_dir:$PATH"
config="$tmp/config/viewpie.yaml"
candidate="$tmp/candidate.yaml"
printf 'old: config\n' >"$config"
printf 'new: config\n' >"$candidate"
chmod 0640 "$config"

set +e
output=$("$repo_root/scripts/viewpie-apply-config.sh" "$candidate" "$config" test.service "$tmp/viewpie" http://127.0.0.1:8080/api/status 2>&1)
status=$?
set -e

[[ "$status" -eq 2 ]] || { echo "expected rollback exit 2, got $status: $output" >&2; exit 1; }
grep -q '^old: config$' "$config" || { echo "old configuration was not restored" >&2; exit 1; }
mode=$(stat -c '%a' "$config")
[[ "$mode" == "640" ]] || { echo "expected restored mode 640, got $mode" >&2; exit 1; }
grep -q 'ROLLED_BACK:' <<<"$output" || { echo "rollback success marker missing: $output" >&2; exit 1; }
echo "PASS: rollback restores prior content and mode"
