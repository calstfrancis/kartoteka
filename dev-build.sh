#!/usr/bin/env bash
# dev-build.sh — build and install Kartoteka (GUI) locally for testing.
#
# Run after Claude has prepped the dev build (bumped version, updated CHANGELOG,
# committed, tagged). No arguments — the version is read from the GUI crate.
#
# Pushes to GitHub first (flatpak-builder pulls source from branch: main), then
# builds and installs locally. Does NOT publish to the flatpak repo.
#
# Prerequisite: packaging/cargo-sources.json must be current for the workspace's
# Cargo.lock (regenerate with flatpak-cargo-generator — see packaging/PACKAGING.md).

set -euo pipefail

MANIFEST="packaging/io.github.calstfrancis.Kartoteka.yml"

VERSION=$(grep '^version' kartoteka-ui-gtk/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "==> Building Kartoteka $VERSION (local dev install)"

echo "==> Pushing to GitHub (flatpak-builder needs this)..."
git push origin main
git push origin "v$VERSION" 2>/dev/null || true

flatpak-builder --force-clean --user --install build-flatpak "$MANIFEST"

echo ""
echo "Done! Kartoteka $VERSION is installed locally."
echo "Run it with: flatpak run io.github.calstfrancis.Kartoteka"
