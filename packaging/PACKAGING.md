# Packaging — Kartoteka (GTK / flatpak)

The GTK frontend (`kartoteka-gtk`) ships as a flatpak to the personal repo at
<https://calstfrancis.github.io/flatpak/>, matching the rest of Fond.

## Files here

- `io.github.calstfrancis.Kartoteka.yml` — flatpak-builder manifest.
- `io.github.calstfrancis.Kartoteka.desktop` — desktop entry.
- `io.github.calstfrancis.Kartoteka.metainfo.xml` — AppStream metainfo.
- `kartoteka.svg` — **placeholder** icon (replace with final artwork before release).
- `cargo-sources.json` — **generated**, offline cargo dependency vendor manifest (see below).

## Before the first build: generate `cargo-sources.json`

flatpak-builder runs offline, so cargo dependencies must be vendored. Regenerate this
file whenever `Cargo.lock` changes:

```sh
# one-time: get the generator
curl -O https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py

python3 flatpak-cargo-generator.py Cargo.lock -o packaging/cargo-sources.json
```

The manifest expects the vendored sources at `/run/build/kartoteka/cargo/vendor` (the
generator's default layout with `CARGO_HOME=/run/build/kartoteka/cargo`).

## PDFium

The manifest downloads a pinned PDFium prebuilt (BSD-3-Clause) and installs
`libpdfium.so` into `/app/lib`; `finish-args` sets `PDFIUM_LIB_PATH=/app/lib`. PDFium is
never vendored into the git repo. If you bump the pinned `chromium/NNNN` URL, update the
`sha256` to match.

## Building

Per the Fond workflow, **Claude does the version bump + docs + commit + tag; Cal runs the
build.** flatpak-builder is never run by Claude.

- Dev build: `./dev-build.sh` (builds + installs locally; does not publish).
- Release: `./publish-flatpak.sh <version>` (builds, exports into the OSTree repo, pushes).

Both read the version from `kartoteka-ui-gtk/Cargo.toml`.

## Status

This scaffold is authored but **not yet validated against a real `flatpak-builder` run**.
Expected first-build follow-ups: generate `cargo-sources.json`, confirm the PDFium archive
layout (`pdfium/lib/libpdfium.so`), and confirm the GNOME `runtime-version` matches an
installed platform.
