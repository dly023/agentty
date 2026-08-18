#!/usr/bin/env bash
set -euo pipefail
rg -Fq 'Input::new(&state.input).appearance(false)' src/ui/composer.rs
rg -Fq 'deliver_agent_prompt_to' src/ui/composer.rs
composer_prod=$(awk 'BEGIN{p=1} /^#\[cfg\(test\)\]/{p=0} p' src/ui/composer.rs)
if rg -Fq 'send_agent_prompt(' <<<"$composer_prod"; then
  echo 'Composer must route AgentPrompt delivery through AgenttyApp::deliver_agent_prompt_to' >&2
  exit 1
fi
if rg -Fq '.child(Input::new(&state.input))' src/ui/composer.rs; then
  echo 'Composer must not render the default bordered Input appearance' >&2
  exit 1
fi
rg -Fq 'ComposerMode::Auto => true' crates/agentty-core/src/core/config.rs
rg -Fq 'auto_mode_hiding_new_plain_terminal' docs/specs/INPUT_EXPERIENCE_SPEC.yaml
rg -Fq 'fn composer_mode_cycles_and_auto_includes_plain_shells' src/ui/composer.rs
rg -Fq 'cargo test --bin agentty-app composer_auto_mode_is_available_for_plain_terminals --locked' docs/quality/traceability.yaml
if rg -Fq 'cargo test -p agentty-core composer_auto_mode_is_available_for_plain_terminals' docs/quality/traceability.yaml; then
  echo 'Composer UI tests must be verified in agentty-app, not filtered through agentty-core' >&2
  exit 1
fi
if rg -Fq 'auto_mode_showing_for_plain_shell_without_agent' docs/specs/INPUT_EXPERIENCE_SPEC.yaml; then
  echo 'Composer Auto must follow the native-rich-input model for plain terminals' >&2
  exit 1
fi
echo 'composer native rich area contract passed'
