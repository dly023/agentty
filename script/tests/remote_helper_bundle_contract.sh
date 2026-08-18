#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

# test: remote_helper_bundle_contract
tmp="$(mktemp -d "${TMPDIR:-/tmp}/agentty-helper-bundle.XXXXXX")"
cleanup() {
  rm -rf "$tmp"
}
trap cleanup EXIT

reference="$tmp/agentty-server"
helpers="$tmp/bundled-server"
mkdir -p "$helpers"
printf '%s\n' \
  '#!/usr/bin/env bash' \
  '# jcode grok tab_transfer pane_set_provider_title' \
  'printf '\''%s\n'\'' '\''{"control":10,"protocol":5,"build":"0.0.1+source123"}'\''' \
  > "$reference"
chmod +x "$reference"

write_helper() {
  local path="$1"
  local build="$2"
  local dialect="$3"
  local vocabulary="${4:-jcode grok tab_transfer pane_set_provider_title}"
  printf 'ELF-fixture\0%s\0%s\0%s\0' "$build" "$dialect" "$vocabulary" > "$path"
}

x86="$helpers/agentty-server-linux-x86_64-musl"
arm="$helpers/agentty-server-linux-aarch64-musl"
write_helper "$x86" '0.0.1+source123' 'agentty-remote-dialect:c10p5'
write_helper "$arm" '0.0.1+source123' 'agentty-remote-dialect:c10p5'

bash script/check_bundled_remote_helpers "$reference" "$helpers" >/dev/null

write_helper "$arm" '0.0.1+stale456' 'agentty-remote-dialect:c10p5'
if bash script/check_bundled_remote_helpers "$reference" "$helpers" >/dev/null 2>&1; then
  echo '[remote-helper-bundle-test] stale source identity was accepted' >&2
  exit 1
fi

write_helper "$arm" '0.0.1+source123' 'agentty-remote-dialect:c9p5'
if bash script/check_bundled_remote_helpers "$reference" "$helpers" >/dev/null 2>&1; then
  echo '[remote-helper-bundle-test] mixed helper dialect was accepted' >&2
  exit 1
fi

rm -f "$arm"
if bash script/check_bundled_remote_helpers "$reference" "$helpers" >/dev/null 2>&1; then
  echo '[remote-helper-bundle-test] missing architecture was accepted' >&2
  exit 1
fi

write_helper "$arm" '0.0.1+source123' 'agentty-remote-dialect:c10p5' 'jcode tab_transfer'
if bash script/check_bundled_remote_helpers "$reference" "$helpers" >/dev/null 2>&1; then
  echo '[remote-helper-bundle-test] incomplete provider/request vocabulary was accepted' >&2
  exit 1
fi

echo 'remote helper bundle contract passed'
