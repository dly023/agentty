#!/bin/bash
# Usage: assert-static.sh <path-to-elf>
# Fail unless the binary is a fully static ELF — no dynamic loader, no shared
# library dependencies.
#
# This is the mechanical guard behind decision D10: one `tty7-server` binary
# is pushed to arbitrary remote machines and must run
# there regardless of what libc, and what *version* of it, that machine has. A
# build that silently picked up a dynamic dependency would still pass a
# compile-only CI job and then fail on the first old box a user connects to —
# far from the change that caused it. Cheap to assert, expensive to discover.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: assert-static.sh <path-to-elf>' >&2
  exit 2
fi
BIN="$1"

if [ ! -f "$BIN" ]; then
  echo "::error::assert-static.sh: $BIN does not exist"
  exit 1
fi

# Read each inspector exactly once; partial output from a failed command is not
# proof. LC_ALL keeps classification and header tokens independent of locale.
if ! classification="$(LC_ALL=C file -b -- "$BIN")"; then
  echo 'assert-static: file inspection failed' >&2
  exit 1
fi
if ! headers="$(LC_ALL=C readelf -lW -- "$BIN")"; then
  echo 'assert-static: program-header inspection failed' >&2
  exit 1
fi
if ! dynamic="$(LC_ALL=C readelf -dW -- "$BIN")"; then
  echo 'assert-static: dynamic-section inspection failed' >&2
  exit 1
fi

# `file` says "statically linked" for a classic static binary and "static-pie
# linked" for a position-independent one. Rust's musl targets have shipped both
# shapes depending on toolchain version, and both are equally self-contained, so
# accept either — but nothing else.
if [[ "$classification" != ELF\ * ]] ||
   ! printf '%s\n' "$classification" | grep -Eq '(^|[ ,])(statically linked|static-pie linked)(,|$)'; then
  echo "::error::$BIN is not statically linked (D10 requires a self-contained binary)"
  exit 1
fi

# The decisive check: a static binary has no PT_INTERP segment, i.e. no
# request for /lib/ld-musl-*.so or ld-linux-*.so. This catches the case `file`
# alone would not, where a dynamic loader is still required.
if printf '%s\n' "$headers" | grep -Eq '^[[:space:]]*INTERP[[:space:]]'; then
  echo "::error::$BIN requires a dynamic loader (PT_INTERP present) — not a static build"
  exit 1
fi

# Belt and braces: no DT_NEEDED entries, i.e. no shared libraries to resolve.
if printf '%s\n' "$dynamic" | grep -Eq '\(NEEDED\)'; then
  echo "::error::$BIN has shared-library dependencies (DT_NEEDED) — not a static build"
  exit 1
fi

echo 'static ELF linkage verified; build identity and runtime behavior not checked'
