#!/usr/bin/env bash
# Prints the body of `## [<version>]` in CHANGELOG.md (the release notes).
#
#   scripts/release/changelog-section.sh 0.0.1
#
# A pre-release (a version with `-`) without a section of its own prints `[Unreleased]`.
# Exits 1 when the section is missing or empty.
set -euo pipefail
version="${1:?usage: changelog-section.sh <version>}"
file="${CHANGELOG:-CHANGELOG.md}"

section() {
  awk -v want="$1" '
    /^## \[/ { inside = (index($0, "## [" want "]") == 1); next }
    /^\[[^]]+\]: / { inside = 0 }
    inside { print }
  ' "$file" | sed -e '/./,$!d' | awk '{ l[NR] = $0 } END { n = NR; while (n > 0 && l[n] ~ /^[[:space:]]*$/) n--; for (i = 1; i <= n; i++) print l[i] }'
}

body="$(section "$version")"
if [ -z "$body" ] && [[ "$version" == *-* ]]; then
  body="$(section Unreleased)"
fi
if [ -z "$body" ]; then
  echo "no CHANGELOG section for $version" >&2
  exit 1
fi
printf '%s\n' "$body"
