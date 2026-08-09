#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

while IFS='|' read -r path forbidden; do
  if rg -n -F "$forbidden" "$path"; then
    echo "[dead-path] unowned production path remains in $path: $forbidden" >&2
    exit 1
  fi
done <<'PATHS'
src/ui/host_ops.rs|pub fn run_or_notify
src/ui/host_registry.rs|pub fn local(cx: &mut App)
src/ui/host_registry.rs|pub fn len(cx: &mut App)
src/core/keychain.rs|fn delete_ref(&self
src/ui/settings.rs|pub(crate) for_id:
src/ui/app.rs|fn open_config_file(&self
PATHS

echo 'production_dead_path_contract passed'
