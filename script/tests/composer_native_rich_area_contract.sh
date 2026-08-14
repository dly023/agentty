#!/usr/bin/env bash
set -euo pipefail
rg -Fq 'Input::new(&state.input).appearance(false)' src/ui/composer.rs
if rg -Fq '.child(Input::new(&state.input))' src/ui/composer.rs; then
  echo 'Composer must not render the default bordered Input appearance' >&2
  exit 1
fi
rg -Fq 'ComposerMode::Auto => true' crates/agentty-core/src/core/config.rs
echo 'composer native rich area contract passed'
