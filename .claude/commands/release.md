Cut a release of claude-conversation-search. Never start this process without explicit instruction — "commit the fix" is not a release request.

Version lives ONLY in Cargo.toml. If $ARGUMENTS names a version or bump level (patch/minor/major), use it; otherwise ask which bump is wanted before touching anything.

**Nothing generated reaches master.** `Cargo.lock` lives only on the tag's own commit, a detached child of the green master commit, where `--locked` builds need it. Checksums live nowhere in this repository.

1. Preflight: working tree clean, on master, nothing unpushed (`git rev-list --count @{upstream}..HEAD`). If anything is unpushed, push it and wait for CI **on that commit** (`./scripts/watch-ci.sh`) before touching the version. A failing CI then costs nothing; a failing CI after the release commit leaves a `release:` commit on master that never shipped, which has to be unwound by hand.
2. Edit `version` in Cargo.toml. Commit as `release: vX.Y.Z`, staging Cargo.toml alone.
3. Push the release commit and wait for CI on it:

```sh
git push
./scripts/watch-ci.sh
```

   `watch-ci.sh` pins the workflow and the commit SHA and passes `--exit-status`. Never select a run with `--branch ... --limit 1`: that reads whatever ran most recently on the branch, which need not be the commit being released.
4. Build the tag on a detached child of that green commit. Run these as **separate** commands, never chained with `&&`: if a chained command is rejected part-way the untried half is silently skipped, and the failure mode is committing the lockfile onto master because the detach never ran.

```sh
git checkout --detach
git symbolic-ref -q HEAD          # must FAIL — that is the confirmation the detach took
cargo generate-lockfile
git add -f Cargo.lock
git commit --no-verify -m "build: pin Cargo.lock for vX.Y.Z"
git tag -as vX.Y.Z                # changelog in the tag message, see below
git push --tags
git switch master
```

   The tag is pushed once and never moved. `Cargo.lock` belongs to the tagged tree so the release workflow's `cargo build --locked` and the AUR PKGBUILD pin the set CI validated; it never lands on master, where it would conflict on every dependency bump.
5. Wait for the Release workflow, which leaves the release a **draft**:

```sh
./scripts/watch-ci.sh vX.Y.Z release.yml
```

   Check the draft carries both `.deb` assets before signing: apt.ticpu.net ingests them from the release, so a draft missing them strands the archive on the previous version, and immutability means they cannot be added after publishing.
6. `./sign-release.sh` — detach-signs every asset with the key from `git config user.signingkey`, uploads the `.asc` files, then publishes the draft. `--dry-run` signs and verifies without uploading. Nothing may be published unsigned: both AUR PKGBUILDs carry `validpgpkeys` and fail without the signatures, and with release immutability enabled a published release's assets can no longer be added to.
7. Update **both** AUR packages — the from-source one and the `-bin` one, which repackages this release's binaries — and push them:

```sh
./scripts/publish-aur.sh vX.Y.Z
```

   It runs each clone's `update-pkg.sh`, refuses to push a PKGBUILD that upgraded to a version other than the tag, and pushes. A missing clone stops it with the `git clone` line to run. `update-pkg.sh` regenerates the PKGBUILD, so any hand edit to it must be re-applied **after** the script runs, followed by `makepkg --printsrcinfo > .SRCINFO` and a push. AUR commits get no Co-Authored-By trailer. Do not poll the AUR RPC index to confirm: it refreshes minutes behind the push and will report the old version well after the release is live.
8. Publish the Debian packages to apt.ticpu.net, from `~/GIT/apt-ticpu-net`:

```sh
./ingest.sh claude-conversation-search-mcp vX.Y.Z
```

   It downloads the release's `.deb` assets, verifies their signatures, checks the package version against the tag, includes them in every suite `projects.yaml` lists for this project, and publishes. `-n` inspects without touching the archive. The packages are the ones CI built and `sign-release.sh` signed — nothing is rebuilt here, so what the archive serves is the release's own bytes. Ingest does not watch for tags, so the archive stays on the previous version until this runs; re-ingesting a tag already carried is a no-op.

## Changelog

Goes in the tag message, not the commit. Written for someone not following development: features, fixes, API changes. No commit lists, no hashes, no co-author lines.

```
vX.Y.Z

Installation
- what changed about how it is installed

Fixes
- what was fixed, in user-visible terms
```
