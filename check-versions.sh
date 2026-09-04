#!/usr/bin/env bash
#
# check-versions.sh — guards against the two version-drift bugs documented in
# ../CLAUDE.md ("App capability matrix" / dev-build workflow):
#
#   1. A pre-release version (e.g. 0.7.2-dev1) must NEVER appear in a metainfo
#      <release> entry. AppStream's version comparison has no concept of
#      pre-release ordering, so it reads "-dev1" as *higher* than the clean
#      "0.7.2" and tools like `flatpak info` then show the wrong Version.
#   2. On a clean release (version has no "-" suffix), the app version must have
#      a matching metainfo <release> entry — catches "tagged a release but forgot
#      to add the metainfo entry."
#
# Kartoteka doesn't fit the other apps' single-Cargo.toml template: it's a
# workspace with no [package] version at the root (see ../CLAUDE.md, "Version
# files by project" — version lives in kartoteka-ui-gtk/Cargo.toml *and*
# kartoteka-cli/Cargo.toml, which must be kept in sync). So this port adds a
# third check for that instead of reading a single root Cargo.toml.
#
# Runs on every push/PR via .github/workflows/ci.yml. Also safe to run locally.

set -euo pipefail

# --- locate the metainfo file (ignore build artifacts) ---
METAINFO=$(find . -name '*.metainfo.xml' \
  -not -path '*/.flatpak-builder/*' \
  -not -path '*/build-flatpak/*' \
  -not -path '*/build/*' 2>/dev/null | head -1)
[ -n "$METAINFO" ] || { echo "ERROR: no *.metainfo.xml found"; exit 1; }

# --- read the app version from both crates that carry one ---
read_crate_version() {
  grep -m1 '^version[[:space:]]*=[[:space:]]*"' "$1" | sed -E 's/.*"([^"]+)".*/\1/' || true
}

UI_TOML="kartoteka-ui-gtk/Cargo.toml"
CLI_TOML="kartoteka-cli/Cargo.toml"
[ -f "$UI_TOML" ] || { echo "ERROR: $UI_TOML not found"; exit 1; }
[ -f "$CLI_TOML" ] || { echo "ERROR: $CLI_TOML not found"; exit 1; }

UI_VERSION=$(read_crate_version "$UI_TOML")
CLI_VERSION=$(read_crate_version "$CLI_TOML")
[ -n "$UI_VERSION" ] || { echo "ERROR: could not determine version from $UI_TOML"; exit 1; }
[ -n "$CLI_VERSION" ] || { echo "ERROR: could not determine version from $CLI_TOML"; exit 1; }

echo "UI version  ($UI_TOML)  : $UI_VERSION"
echo "CLI version ($CLI_TOML) : $CLI_VERSION"
echo "Metainfo    : $METAINFO"
echo

fail=0

# --- check 0: the two crate versions must be kept in sync ---
if [ "$UI_VERSION" != "$CLI_VERSION" ]; then
  echo "ERROR: version mismatch between $UI_TOML ($UI_VERSION) and $CLI_TOML ($CLI_VERSION)"
  fail=1
fi
APP_VERSION="$UI_VERSION"

# --- check 1: no *stable* pre-release <release> entry ---
# A pre-release version is only safe in metainfo if it carries type="development"
# (AppStream then excludes it from the stable-version calc). A pre-release entry
# WITHOUT that attribute is the dangerous kind that mis-sorts above the real
# release. We flag those; type="development" history is tolerated.
bad=$(grep -oE '<release[^>]*>' "$METAINFO" \
  | grep -E 'version="[^"]*-(dev|rc|alpha|beta|pre)' \
  | grep -v 'type="development"' || true)
if [ -n "$bad" ]; then
  echo "ERROR: metainfo has pre-release <release> entries that are NOT type=\"development\""
  echo "       (AppStream sorts these above the real release — wrong 'Version' in flatpak info):"
  echo "$bad" | sed 's/^/  /'
  fail=1
fi

# --- check 2: a clean release must have a matching metainfo <release> entry ---
case "$APP_VERSION" in
  *-*)
    echo "Dev build ($APP_VERSION) — skipping the metainfo-entry match check."
    ;;
  *)
    if grep -qE "<release[[:space:]]+version=\"$APP_VERSION\"" "$METAINFO"; then
      echo "Clean release $APP_VERSION has a matching <release> entry."
    else
      echo "ERROR: clean release $APP_VERSION has no matching <release> entry in $METAINFO"
      fail=1
    fi
    ;;
esac

echo
if [ "$fail" -eq 0 ]; then
  echo "Version consistency OK."
else
  echo "Version consistency FAILED — see errors above."
  exit 1
fi
