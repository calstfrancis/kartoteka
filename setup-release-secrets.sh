#!/usr/bin/env bash
# setup-release-secrets.sh — one-shot setup of the three release-flatpak.yml secrets
#
# Cal-only. Wraps the three `gh secret set` calls from RELEASE-CI-SETUP.md into one
# script so this is one command instead of three to remember. Safe to re-run any time —
# `gh secret set` overwrites in place.
#
# The GPG key/passphrase are the same across every app in the suite (one shared signing
# key) and the repo token is scoped to calstfrancis/flatpak, not to whichever app-repo
# it's stored under — so if you already have this token from setting up another app
# (Zerkalo), paste that same value instead of minting a new one.
#
# Usage:
#   ./setup-release-secrets.sh

set -euo pipefail

REPO="calstfrancis/kartoteka"
GPG_KEY="A2918A9B43B199ADF9879F934AC9D5173DE4BC41"

echo "==> Setting up release secrets for $REPO"
echo ""

echo "==> 1/3: FLATPAK_GPG_PRIVATE_KEY (exported directly from gpg, no prompt)"
gpg --export-secret-keys --armor "$GPG_KEY" | gh secret set FLATPAK_GPG_PRIVATE_KEY --repo "$REPO"
echo "    done"
echo ""

echo "==> 2/3: FLATPAK_GPG_PASSPHRASE"
echo "    Paste the signing key's passphrase, then Ctrl-D:"
gh secret set FLATPAK_GPG_PASSPHRASE --repo "$REPO"
echo "    done"
echo ""

echo "==> 3/3: FLATPAK_REPO_TOKEN"
echo "    Paste your calstfrancis/flatpak-scoped token (reuse the one from Zerkalo's"
echo "    setup if you still have it — same scope works here), then Ctrl-D:"
gh secret set FLATPAK_REPO_TOKEN --repo "$REPO"
echo "    done"
echo ""

echo "All three secrets set on $REPO. Test with:"
echo "  gh workflow run release-flatpak.yml --repo $REPO -f version=\$(grep '^version' kartoteka-ui-gtk/Cargo.toml | head -1 | sed 's/version = \"\\(.*\\)\"/\\1/')"
