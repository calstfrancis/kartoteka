# Release CI setup — one-time, Cal only

`release-flatpak.yml` needs three repo secrets to build and publish releases. Claude can't
set these — they involve your GPG private key (and its passphrase) and a token with push
access to another repo, none of which should pass through a session transcript.

Run these from `~/Projects/kartoteka`. Requires `gh` to already be authenticated (it is, on
this machine). This mirrors Zerkalo's own setup (`zerkalo/RELEASE-CI-SETUP.md`) — same three
secrets, same shape, just scoped to `calstfrancis/kartoteka` instead.

## Fast path: `setup-release-secrets.sh`

If you still have the `calstfrancis/flatpak`-scoped token from Zerkalo's setup (it's not
tied to which app-repo it's stored under, so the same value works here too), this script
does all three secrets in one run instead of three separate commands:

```bash
./setup-release-secrets.sh
```

It'll still prompt you to paste the passphrase and the token — that part can't be
automated — but it's one invocation instead of hunting through this file for three
commands. The sections below are what it runs, spelled out, for reference or if you'd
rather do it by hand.

## 1. `FLATPAK_GPG_PRIVATE_KEY`

Exports your signing key (armored/text form) straight into the secret — nothing touches disk:

```bash
gpg --export-secret-keys --armor A2918A9B43B199ADF9879F934AC9D5173DE4BC41 \
  | gh secret set FLATPAK_GPG_PRIVATE_KEY --repo calstfrancis/kartoteka
```

**The key has a passphrase** (same key Zerkalo's workflow uses — this was confirmed the
first time that workflow ran for real, 2026-08-19). Importing it is fine on its own, but
signing later needs it unlocked, and there's nothing to prompt for non-interactively in CI —
so the workflow presets the passphrase into the agent's cache once, up front
(`gpg-agent --preset`), rather than trying to feed it interactively per signing call:

```bash
gh secret set FLATPAK_GPG_PASSPHRASE --repo calstfrancis/kartoteka
# paste the passphrase, Ctrl-D
```

## 2. `FLATPAK_REPO_TOKEN`

A token the workflow uses to push the built flatpak into `calstfrancis/flatpak`. Scope it as
narrowly as GitHub allows — a fine-grained PAT limited to just that one repo (you can reuse
the same token Zerkalo's setup created, or mint a fresh one; either works since the scope is
identical):

1. https://github.com/settings/personal-access-tokens/new
2. **Resource owner:** calstfrancis · **Repository access:** Only select repositories → `flatpak`
3. **Permissions:** Repository → Contents → **Read and write** (nothing else needed)
4. Generate, copy the token, then:

```bash
gh secret set FLATPAK_REPO_TOKEN --repo calstfrancis/kartoteka
# paste the token, Ctrl-D
```

## Testing before a real release

`release-flatpak.yml` also accepts manual dispatch, so you can test the whole pipeline
without cutting a real version tag:

```bash
gh workflow run release-flatpak.yml --repo calstfrancis/kartoteka -f version=0.6.0
```

(Use whatever version is currently in `kartoteka-ui-gtk/Cargo.toml` — the workflow checks it
matches.) Watch it at
https://github.com/calstfrancis/kartoteka/actions/workflows/release-flatpak.yml — a
manual-dispatch run skips the "CI must have passed" gate (there's no fresh commit to check
against) but does everything else for real, including the actual push to the public flatpak
repo. Worth doing once before relying on this for a real release.

## Until these secrets are set

`./publish-flatpak.sh` will push the tag and the workflow will run, but every job after
"Import GPG signing key" fails with an empty/missing secret. Use
`./publish-flatpak-local.sh` in the meantime — same build+publish sequence, run on your own
machine with your own already-configured GPG key.

## Rolling this out to the rest of the suite

Zerkalo and Kartoteka are now both CI-published (2026-08-20) — this is the intended standard
going forward for every flatpak app in the Fond/Cal suite, not just these two. The remaining
seven — Rubric, Gost, Kopilka, Iskra, Retseptura, Skrizhal, Chered — still use the manual
`publish-flatpak.sh` (build+export+push, all local). Same three secrets, same workflow
shape, would need repeating per-app (worth doing as a shared reusable workflow
(`workflow_call`) rather than copy-pasted seven more times, given how much of
`release-flatpak.yml` is now identical between the two apps that have it).
