#!/usr/bin/env bash
# publish-flatpak-local.sh — manual fallback for building and publishing Kartoteka locally
#
# As of the release-flatpak.yml GitHub Actions workflow, publish-flatpak.sh just pushes
# and lets CI do this same sequence — this script is what CI actually runs, kept here as
# a fallback for when CI is down or you need to debug the build locally. Requires your own
# GPG key and a local flatpak-builder/runtime setup, same as before.
#
# Usage:
#   ./publish-flatpak-local.sh 0.6.0
#
# What this script does NOT do (Claude's job, done before running this):
#   - Write the CHANGELOG entry / metainfo release note
#   - Bump the version / commit / tag
#
# What this script DOES do:
#   1. Verify the version you pass matches the GUI crate (sanity check)
#   2. Push this repo to GitHub (flatpak-builder pulls sources from there)
#   3. Build the flatpak
#   4. Pull/clone the public flatpak repo
#   5. Export the build into it
#   6. Regenerate the OSTree summary
#   7. Commit and push the flatpak repo
#
# Prerequisite: packaging/cargo-sources.json must be current (see packaging/PACKAGING.md).

set -euo pipefail

GPG_KEY="A2918A9B43B199ADF9879F934AC9D5173DE4BC41"
# Per-user, not a fixed world-shared /tmp path another user (or a stale run) could own.
FLATPAK_REPO="${TMPDIR:-/tmp}/flatpak-checkout-$(id -u)"
MANIFEST="packaging/io.github.calstfrancis.Kartoteka.yml"
APP_LABEL="Kartoteka"

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

echo "==> Publishing $APP_LABEL $VERSION (local build)"

echo "==> Pushing source repo to GitHub..."
git push origin main
git push origin "v$VERSION"   # no `|| true`: a real push failure must stop the release

# The manifest's git source is `branch: main`; build from a copy pinned to the tagged commit
# so the published flatpak is exactly the tag, not whatever main has moved on to.
PINNED="${MANIFEST%.yml}.pinned.yml"
trap 'rm -f "$PINNED"' EXIT
sed "s/^        branch: main$/        commit: $(git rev-parse "v$VERSION^{commit}")/" "$MANIFEST" > "$PINNED"
grep -q "commit: " "$PINNED" || { echo "ERROR: could not pin the manifest to the tag."; exit 1; }

echo "==> Building flatpak (this will take a while)..."
flatpak-builder --force-clean --user --install build-flatpak "$PINNED"

echo "==> Syncing public flatpak repo..."
if [[ -d "$FLATPAK_REPO/.git" ]]; then
  git -C "$FLATPAK_REPO" pull --ff-only
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

# ── verify the commit actually got signed ──────────────────────────────────
# build-export produces an unsigned commit if --gpg-sign is missing or the key
# is unavailable, and says nothing. The repo summary still signs fine, so the
# breakage only surfaces later as a GPG failure on someone else's install.
APP_ID="$(basename "$MANIFEST" .yml)"
COMMIT="$(cat "$FLATPAK_REPO/refs/heads/app/$APP_ID/x86_64/master")"
if [[ ! -f "$FLATPAK_REPO/objects/${COMMIT:0:2}/${COMMIT:2}.commitmeta" ]]; then
  echo "ERROR: commit $COMMIT for $APP_ID carries no GPG signature."
  echo "Refusing to push. Re-run build-export with --gpg-sign=\"$GPG_KEY\"."
  exit 1
fi
echo "==> Signature verified for $APP_ID"

# `git add -A` below stages ANY difference on disk, including a file that went
# missing for reasons unrelated to this publish (an interrupted checkout, the
# shared /tmp/flatpak-checkout getting cleared, etc.) — it would otherwise get
# silently committed as if the deletion were intentional. This is exactly how
# Zerkalo's own `.Debug` extension ref ended up with a missing object for
# months before anyone noticed (found 2026-09-17).
#
# A whole-repo `ostree fsck` is the wrong tool for this: it has no way to scope
# to just this app's ref, and the shared repo carries that same pre-existing
# Zerkalo `.Debug` breakage (confirmed 2026-09-22 publishing Pereplyot — an
# orphaned ref pointing at a `.dirtree` object deleted from the repo since
# around Zerkalo 0.13.9, unrelated to any app's publish) — so a blanket fsck
# refuses to publish anything, for any app, until that's fixed separately.
# Instead: verify this app's own freshly-exported commit is actually
# traversable, and refuse to publish if `git add -A` is about to stage any
# object *deletion* — objects are content-addressed and append-only, so a
# normal publish should never remove one; a deletion showing up here is
# exactly the failure mode above.
echo "==> Verifying this export is traversable..."
if ! ostree --repo="$FLATPAK_REPO" ls -R "$COMMIT" > /dev/null; then
  echo "ERROR: commit $COMMIT for $APP_ID is not fully traversable — refusing to publish."
  echo "Investigate and repair the export before committing; do not just re-run this script."
  exit 1
fi
echo "==> Checking for unintended object deletions..."
cd "$FLATPAK_REPO"
DELETED_OBJECTS="$(git status --porcelain -- objects/ | awk '$1 == "D" {print $2}')"
if [[ -n "$DELETED_OBJECTS" ]]; then
  echo "ERROR: this publish would delete objects the repo still references:"
  echo "$DELETED_OBJECTS"
  echo "Refusing to publish. Investigate before committing; do not just re-run this script."
  exit 1
fi
cd - > /dev/null

echo "==> Pushing flatpak repo..."
cd "$FLATPAK_REPO"
git add -A
git commit -m "$APP_LABEL $VERSION"
git push origin main

echo ""
echo "Done! $APP_LABEL $VERSION is live at https://calstfrancis.github.io/flatpak/"
