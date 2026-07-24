#!/usr/bin/env bash
# publish-flatpak.sh — build and publish Kartoteka to the personal flatpak repo.
#
# Usage:
#   ./publish-flatpak.sh 0.2.0
#
# What this does NOT do (Claude's job, done before running this):
#   - Write the CHANGELOG entry / metainfo release note
#   - Bump the version / commit / tag
#
# What this DOES do:
#   1. Verify the version matches the GUI crate (sanity check)
#   2. Push this repo to GitHub (flatpak-builder pulls sources from there)
#   3. Build the flatpak
#   4. Pull/clone the public flatpak repo, export, regenerate summary, push
#
# Prerequisite: packaging/cargo-sources.json must be current (see packaging/PACKAGING.md).

set -euo pipefail

GPG_KEY="A2918A9B43B199ADF9879F934AC9D5173DE4BC41"
FLATPAK_REPO="/tmp/flatpak-checkout"
MANIFEST="packaging/io.github.calstfrancis.Kartoteka.yml"
APP_LABEL="Kartoteka"

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <version>   e.g.  $0 0.2.0"
  exit 1
fi
VERSION="$1"

CARGO_VERSION=$(grep '^version' kartoteka-ui-gtk/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
if [[ "$CARGO_VERSION" != "$VERSION" ]]; then
  echo "ERROR: kartoteka-ui-gtk/Cargo.toml says '$CARGO_VERSION', but you passed '$VERSION'."
  echo "Did you forget the version bump? (Ask Claude to do the version bump + docs first.)"
  exit 1
fi

echo "==> Publishing $APP_LABEL $VERSION"

echo "==> Pushing source repo to GitHub..."
git push origin main
git push origin "v$VERSION" 2>/dev/null || true

echo "==> Building flatpak (this will take a while)..."
flatpak-builder --force-clean --user --install build-flatpak "$MANIFEST"

echo "==> Syncing public flatpak repo..."
if [[ -d "$FLATPAK_REPO/.git" ]]; then
  git -C "$FLATPAK_REPO" pull
else
  git clone https://github.com/calstfrancis/flatpak "$FLATPAK_REPO"
fi

echo "==> Exporting build..."
flatpak build-export \
  --gpg-sign="$GPG_KEY" \
  "$FLATPAK_REPO" \
  build-flatpak \
  master

echo "==> Regenerating OSTree summary..."
flatpak build-update-repo \
  --gpg-sign="$GPG_KEY" \
  "$FLATPAK_REPO"

echo "==> Pushing flatpak repo..."
cd "$FLATPAK_REPO"
git add -A
git commit -m "$APP_LABEL $VERSION"
git push origin main

echo ""
echo "Done! $APP_LABEL $VERSION is live at https://calstfrancis.github.io/flatpak/"
