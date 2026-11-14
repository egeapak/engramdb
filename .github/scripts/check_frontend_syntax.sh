#!/usr/bin/env bash
# Parse every JavaScript module of the dashboard (the embedded frontend under
# web/js and the bench scripts) and fail on a syntax error. Run from the
# repository root; needs only `node`.
#
# The frontend has no bundler and no build step, so nothing parses these files
# before a browser does: a stray brace ships as a blank dashboard. The Rust
# tests read the files as text and do not parse them either.
#
# Each file is fed to `node --input-type=module --check` on stdin, on purpose.
# `node --check file.js` treats a `.js` file with no package.json as CommonJS,
# and for a file that contains `import`/`export` it then exits 0 without
# reporting anything, even when the file has a syntax error (checked on Node
# 22.23: `export const x = ;` passes). Reading from stdin with the module type
# forced has no such fallback.
set -euo pipefail
count=0
bad=0
while IFS= read -r -d '' f; do
  count=$((count + 1))
  if ! node --input-type=module --check < "$f"; then
    echo "::error file=$f::syntax error"
    bad=$((bad + 1))
  fi
done < <(find crates/engram-dashboard/web/js crates/engram-dashboard/bench \
           -type f \( -name '*.js' -o -name '*.mjs' \) -not -path '*/node_modules/*' -print0 | sort -z)
if [ "$count" -lt 40 ]; then
  echo "::error::only $count JavaScript files found; the dashboard has more than 40. Did web/js move?"
  exit 1
fi
if [ "$bad" -gt 0 ]; then
  echo "::error::$bad of $count files do not parse"
  exit 1
fi
echo "OK: $count files parse"
