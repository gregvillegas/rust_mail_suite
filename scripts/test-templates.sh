#!/usr/bin/env bash
# Render every deploy template with the example settings and fail on any
# undefined variable or leftover template syntax. Needs only bash and perl.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$(mktemp -d)"; trap 'rm -rf "$OUT"' EXIT
ENVF="$OUT/mta.env"
sed 's/CHANGE_ME/example-password-1/' "$ROOT/deploy/mta.env.example" >"$ENVF"
"$ROOT/scripts/setup-mta.sh" --env-file "$ENVF" --render-to "$OUT/root"
# Every .j2 under the deploy config directories must be in the render list.
listed=$(find "$OUT/root" -type f | wc -l | tr -d ' ')
present=$(find "$ROOT/deploy/postfix" "$ROOT/deploy/dovecot" "$ROOT/deploy/amavis" "$ROOT/deploy/spamassassin" "$ROOT/deploy/clamav" -name '*.j2' | wc -l | tr -d ' ')
[ "$listed" = "$present" ] || { echo "FAIL: $present templates exist but $listed are rendered" >&2; exit 1; }
# A missing variable must be an error.
sed -i.bak '/^VMAIL_UID=/d' "$ENVF"
if "$ROOT/scripts/setup-mta.sh" --env-file "$ENVF" --render-to "$OUT/x" 2>/dev/null; then
  echo "FAIL: missing variable was accepted" >&2; exit 1; fi
echo "OK: $listed templates render cleanly; missing variables are rejected"
