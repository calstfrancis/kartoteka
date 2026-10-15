#!/usr/bin/env bash
# capture-screenshots.sh — capture light + dark screenshots of Kartoteka against demo data
#
# Runs target/release/kartoteka-gtk (build first: `cargo build --release -p kartoteka-ui-gtk
# -p kartoteka-cli`) under a fully isolated home — HOME and all four XDG dirs point at a
# throwaway directory (the app is GLib-based, so HOME alone is not enough; see root
# CLAUDE.md). The demo library is built with the real CLI from screenshots/demo-library.bib
# (entirely fictional) plus two hand-written collections, so the seed can't drift from the
# on-disk format. The theme is set via the app's own config ("theme" field), no portal
# involved. dbus-run-session keeps GApplication single-instance from relaying to a real
# running Kartoteka.
#
# Requires: Xvfb, xdotool, ImageMagick (magick), built release binaries.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

BINARY="target/release/kartoteka-gtk"
CLI="target/release/kartoteka"
for b in "$BINARY" "$CLI"; do
  [[ -x "$b" ]] || { echo "ERROR: $b not found. Run 'cargo build --release' first." >&2; exit 1; }
done

BINARY="$SCRIPT_DIR/$BINARY"; CLI="$SCRIPT_DIR/$CLI"
DEMO_HOME=$(mktemp -d /tmp/kartoteka-demo-home.XXXXXX)
LIB="$DEMO_HOME/demo-library"
OUT="screenshots/kartoteka-main.png"
OUT_DARK="screenshots/kartoteka-main-dark.png"
SCREEN_W=1500; SCREEN_H=950
APP_W=1300; APP_H=800
BIB="$SCRIPT_DIR/screenshots/demo-library.bib"

cleanup() {
  [[ -n "${APP_PID:-}" ]] && kill -TERM -- "-$APP_PID" 2>/dev/null || true
  [[ -n "${XVFB_PID:-}" ]] && kill -- "-$XVFB_PID" 2>/dev/null || true
  rm -rf "$DEMO_HOME"
}
trap cleanup EXIT INT TERM

export HOME="$DEMO_HOME"
export XDG_CONFIG_HOME="$DEMO_HOME/.config"
export XDG_DATA_HOME="$DEMO_HOME/.local/share"
export XDG_CACHE_HOME="$DEMO_HOME/.cache"
export XDG_STATE_HOME="$DEMO_HOME/.local/state"
mkdir -p "$XDG_CONFIG_HOME/kartoteka" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
export GIT_AUTHOR_NAME="Demo User" GIT_AUTHOR_EMAIL="demo@example.com"
export GIT_COMMITTER_NAME="Demo User" GIT_COMMITTER_EMAIL="demo@example.com"
printf '[user]\n\tname = Demo User\n\temail = demo@example.com\n' > "$DEMO_HOME/.gitconfig"

echo "==> Seeding demo library in $LIB"
"$CLI" -L "$LIB" init "$LIB" >/dev/null
"$CLI" -L "$LIB" import --from-bibtex "$BIB" --no-attachments >/dev/null
cat > "$LIB/collections/methods-chapter.yml" <<Y
name: Methods Chapter
description: Sources for the methods chapter.
keys:
- demo1
- demo5
- demo8
Y
cat > "$LIB/collections/reading-habits.yml" <<Y
name: Reading Habits
keys:
- demo2
- demo6
Y
"$CLI" -L "$LIB" reindex >/dev/null
"$CLI" -L "$LIB" commit -m "Demo library" >/dev/null 2>&1 || true

DISPLAY_NUM=243
while [[ -e "/tmp/.X${DISPLAY_NUM}-lock" ]]; do DISPLAY_NUM=$((DISPLAY_NUM + 1)); done
echo "==> Starting isolated Xvfb on :$DISPLAY_NUM"
setsid Xvfb ":$DISPLAY_NUM" -screen 0 "${SCREEN_W}x${SCREEN_H}x24" >/dev/null 2>&1 &
XVFB_PID=$!
sleep 2

nonblank() { # file -> true if std-dev > 0.01
  local sd; sd=$(magick "$1" -format "%[fx:standard_deviation]" info: 2>/dev/null || echo 0)
  awk -v v="$sd" 'BEGIN { exit !(v > 0.01) }'
}

capture_scheme() {
  local theme="$1" out="$2"
  cat > "$XDG_CONFIG_HOME/kartoteka/gui.json" <<CONF
{
  "library_path": "$LIB",
  "theme": "$theme",
  "window_width": $APP_W,
  "window_height": $APP_H
}
CONF
  echo "==> Launching Kartoteka ($theme)"
  setsid env -u WAYLAND_DISPLAY GDK_BACKEND=x11 DISPLAY=":$DISPLAY_NUM" \
    dbus-run-session -- "$BINARY" >/dev/null 2>&1 &
  APP_PID=$!

  local attempt
  for attempt in 1 2 3 4 5 6 7 8; do
    DISPLAY=":$DISPLAY_NUM" magick x:root -crop "${APP_W}x${APP_H}+0+0" +repage /tmp/kartoteka-render-check.png
    if nonblank /tmp/kartoteka-render-check.png; then
      sleep 2
      DISPLAY=":$DISPLAY_NUM" magick x:root -crop "${APP_W}x${APP_H}+0+0" +repage /tmp/kartoteka-render-check.png
      nonblank /tmp/kartoteka-render-check.png && break
    fi
    [[ $attempt -eq 8 ]] && { echo "ERROR: window never rendered." >&2; exit 1; }
    sleep 5
  done

  # Select an entry so the detail pane is populated.
  DISPLAY=":$DISPLAY_NUM" xdotool mousemove ${CLICK_X:-600} ${CLICK_Y:-150} click 1 2>/dev/null || true
  sleep 2

  DISPLAY=":$DISPLAY_NUM" magick x:root -crop "${APP_W}x${APP_H}+0+0" +repage "$out"
  nonblank "$out" || { echo "ERROR: $out is blank." >&2; exit 1; }

  kill -TERM -- "-$APP_PID" 2>/dev/null || true
  wait "$APP_PID" 2>/dev/null || true
  APP_PID=
  DISPLAY=":$DISPLAY_NUM" xsetroot -solid black
}

mkdir -p screenshots
capture_scheme light "$OUT"
capture_scheme dark  "$OUT_DARK"
echo "Done. Wrote $OUT and $OUT_DARK"

SLUG="kartoteka"
WEBSITE_DIR="${WEBSITE_DIR:-$(dirname "$SCRIPT_DIR")/calstfrancis.github.io}"
if [[ -d "$WEBSITE_DIR" ]]; then
  echo "==> Publishing web images to $WEBSITE_DIR"
  cp "$OUT"      "$WEBSITE_DIR/$SLUG.png"
  cp "$OUT_DARK" "$WEBSITE_DIR/$SLUG-dark.png"
  magick "$OUT"      -quality 80 "$WEBSITE_DIR/$SLUG.webp"
  magick "$OUT_DARK" -quality 80 "$WEBSITE_DIR/$SLUG-dark.webp"
else
  echo "NOTE: website dir not found ($WEBSITE_DIR) — skipping web export."
fi
