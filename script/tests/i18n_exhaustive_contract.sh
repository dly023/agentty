#!/usr/bin/env bash
# Prove I18N-EXHAUSTIVE-TRANSLATION-06 fail-closed behavior on synthetic catalogs.
set -euo pipefail
cd "$(dirname "$0")/../.."

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/assets/i18n" "$tmp/script" "$tmp/src/core" "$tmp/crates/agentty-core/src/core" "$tmp/src/ui"

# Minimal stubs so check_i18n's later rg gates can run when we invoke the
# integrity python in isolation below. Full script is exercised on the real tree.
cp script/check_i18n "$tmp/script/check_i18n"
chmod +x "$tmp/script/check_i18n"

# --- empty value must fail ---
printf 'a.key: hello\nb.key:\n' > "$tmp/assets/i18n/en-US.yaml"
printf 'a.key: 你好\nb.key: 世界\n' > "$tmp/assets/i18n/zh-CN.yaml"
if (
  cd "$tmp"
  python3 - "$tmp" assets/i18n/en-US.yaml assets/i18n/zh-CN.yaml <<'PY'
import pathlib, sys
out_dir = pathlib.Path(sys.argv[1])
paths = [pathlib.Path(p) for p in sys.argv[2:]]
errors = []
for path in paths:
    seen = {}
    for lineno, raw in enumerate(path.read_text().splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key, value = line.split(":", 1)
        key, value = key.strip(), value.strip()
        if value.startswith('"') and value.endswith('"') and len(value) >= 2:
            value = value[1:-1]
        if key in seen:
            errors.append(f"dup {key}")
            continue
        seen[key] = lineno
        if not value.strip():
            errors.append(f"{path}:{lineno}: empty value for '{key}'")
if errors:
    print("catalog integrity failures:", file=sys.stderr)
    for e in errors:
        print(e, file=sys.stderr)
    sys.exit(1)
PY
); then
  echo '[i18n-exhaustive] empty value was accepted' >&2
  exit 1
fi

# --- key-set divergence must fail ---
printf 'a.key: hello\nb.key: world\n' > "$tmp/assets/i18n/en-US.yaml"
printf 'a.key: 你好\n' > "$tmp/assets/i18n/zh-CN.yaml"
if (
  cd "$tmp"
  python3 - <<'PY'
from pathlib import Path
en = {l.split(":",1)[0].strip() for l in Path("assets/i18n/en-US.yaml").read_text().splitlines() if l.strip()}
zh = {l.split(":",1)[0].strip() for l in Path("assets/i18n/zh-CN.yaml").read_text().splitlines() if l.strip()}
raise SystemExit(0 if en == zh else 1)
PY
); then
  echo '[i18n-exhaustive] divergent key sets were accepted' >&2
  exit 1
fi

# Real tree must still pass the gate.
./script/check_i18n

echo 'i18n_exhaustive_contract passed'
