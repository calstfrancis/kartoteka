#!/usr/bin/env bash
# publish-flatpak.sh — push a release; GitHub Actions builds and publishes it
#
# Usage:
#   ./publish-flatpak.sh 0.6.0
#
# What this script does NOT do (Claude's job, done before running this):
#   - Write the CHANGELOG entry / metainfo release note
#   - Bump the version / commit / tag
#
# What this script DOES do:
#   1. Verify the version you pass matches the GUI crate (sanity check)
#   2. Push main and the version tag to GitHub
#
# Pushing the tag is what triggers .github/workflows/release-flatpak.yml, which does
# everything this script used to do locally: build the flatpak, export it into the public
# repo, GPG-sign it, and push. Watch it at:
#   https://github.com/calstfrancis/kartoteka/actions/workflows/release-flatpak.yml
#
# Needs CI to have already passed for this commit — release-flatpak.yml checks this itself
# and refuses to publish otherwise, so there's no separate manual check to remember here
# anymore. If GitHub Actions is down or you need to debug the build locally, use
# publish-flatpak-local.sh instead (does the full build+publish here, same as this script
# used to).

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <version>   e.g.  $0 0.6.0"
  exit 1
fi
VERSION="$1"

CARGO_VERSION=$(awk '/^\[package\]/{p=1;next} /^\[/{p=0} p && /^version *=/{gsub(/^version *= *"|"$/,""); print; exit}' kartoteka-ui-gtk/Cargo.toml)
if [[ "$CARGO_VERSION" != "$VERSION" ]]; then
  echo "ERROR: kartoteka-ui-gtk/Cargo.toml says '$CARGO_VERSION', but you passed '$VERSION'."
  echo "Did you forget the version bump? (Ask Claude to do the version bump + docs first.)"
  exit 1
fi

# Refuse to publish from a dirty tree, or if the tag is missing / not at HEAD — otherwise
# `git push origin main` can go out and then fail on the tag, leaving a half-done release.
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "ERROR: uncommitted changes in the working tree — commit or stash them first."
  exit 1
fi
if ! git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
  echo "ERROR: tag v$VERSION does not exist locally — tag the release commit first."
  exit 1
fi
if [[ "$(git rev-parse "v$VERSION^{commit}")" != "$(git rev-parse HEAD)" ]]; then
  echo "ERROR: v$VERSION does not point at HEAD — refusing to publish a different commit."
  exit 1
fi

echo "==> Publishing Kartoteka $VERSION"
git push origin main
git push origin "v$VERSION"

echo ""
echo "Done! GitHub Actions is building and publishing $VERSION now:"
echo "  https://github.com/calstfrancis/kartoteka/actions/workflows/release-flatpak.yml"
