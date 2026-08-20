#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

if sed -n '/pub(crate) fn render_composer/,/^    }/p' src/ui/composer.rs | grep -F '.absolute()' >/dev/null; then
  echo 'composer must participate in terminal-column flow, not overlay terminal cells' >&2
  exit 1
fi

if sed -n '/fn ensure_composer_open/,/^    }/p' src/ui/composer.rs | grep -F 'weak.update' >/dev/null; then
  echo 'composer input callback must not reacquire AgenttyApp during its existing lease' >&2
  exit 1
fi

grep -F 'render_pane_with_docks' src/ui/app.rs >/dev/null
grep -F 'render_resizable_split' src/ui/composer_dock.rs >/dev/null
grep -F 'INPUT-COMPOSER-DOCK-REFLOW-11' docs/specs/INPUT_EXPERIENCE_SPEC.yaml >/dev/null

echo 'composer_dock_reflow_contract passed'
