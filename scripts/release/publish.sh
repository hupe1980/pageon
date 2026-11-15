#!/usr/bin/env bash
# Publishes the crates to crates.io in dependency order (release.yml, job crates). A crate
# version already published is skipped, so a re-run after a partial publish goes on from there.
#
#   CARGO_REGISTRY_TOKEN=… scripts/release/publish.sh 0.0.1
set -euo pipefail
version="${1:?usage: publish.sh <version>}"
published() {
  curl -fsS -o /dev/null -A "pageon-release (github.com/hupe1980/pageon)" \
    "https://crates.io/api/v1/crates/$1/$version"
}
for crate in pageon-dom pageon-env pageon-agent pageon-net pageon-layout pageon-ax pageon-js pageon; do
  if published "$crate"; then
    echo "$crate $version is already published"
  else
    cargo publish --locked -p "$crate"
  fi
done
