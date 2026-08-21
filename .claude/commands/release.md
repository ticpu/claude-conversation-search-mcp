Cut a release of claude-conversation-search. Never start this process without explicit instruction — "commit the fix" is not a release request.

Version lives ONLY in Cargo.toml. If $ARGUMENTS names a version or bump level (patch/minor/major), use it; otherwise ask which bump is wanted before touching anything.

1. Preflight: working tree clean, on master, and CI green **for the commits being released** — not merely for whatever master last ran. Check both:
   - `git rev-list --count @{upstream}..HEAD` — unpushed commits.
   - `gh run list --branch master --limit 1` — and confirm the run's commit is HEAD.
   If anything is unpushed, push it and wait for CI to go green **before** touching the version. A failing CI then costs nothing; a failing CI after the release commit leaves a `release:` commit on master that never shipped, which has to be unwound by hand.
2. Edit `version` in Cargo.toml.
3. `cargo update -p claude-conversation-search --precise X.Y.Z` — Cargo.lock records the crate's own version, and the release workflow builds with `--locked`, so a stale lockfile fails the release build rather than warning. Never `cargo generate-lockfile` here: it regenerates every entry and silently folds a full dependency bump into what is supposed to be a version-only commit. Confirm the diff is the single `version` line before staging.
4. Commit as `release: vX.Y.Z`, staging Cargo.toml and Cargo.lock explicitly.
5. `git push`, then WAIT for CI to pass on master (`gh run watch`).
6. `git tag -as vX.Y.Z` — changelog goes in the tag message: features, fixes, API changes for someone not following development. No commit lists or hashes.
7. `git push --tags`, then WAIT for the Release workflow to complete successfully (`gh run watch`). It leaves the release a **draft** — step 8 publishes it.
8. `./sign-release.sh` — detach-signs every asset with the key from `git config user.signingkey`, uploads the `.asc` files, then publishes the draft. `--dry-run` signs and verifies without uploading. Nothing may be published unsigned: both AUR PKGBUILDs carry `validpgpkeys` and fail without the signatures, and with release immutability enabled a published release's assets can no longer be added to.
9. Update **both** AUR packages — the from-source one and the `-bin` one, which repackages this release's binaries. Releases may have been cut from another machine, so bring each clone up to date first; if one is missing entirely, clone it:
   - `git clone ssh://aur@aur.archlinux.org/claude-conversation-search.git ~/.cache/paru/clone/claude-conversation-search`
   - `git clone ssh://aur@aur.archlinux.org/claude-conversation-search-bin.git ~/.cache/paru/clone/claude-conversation-search-bin`

   Then, for each:
   - `cd ~/.cache/paru/clone/claude-conversation-search/ && ./update-pkg.sh 2>&1 | grep -v Compiling`
   - `cd ~/.cache/paru/clone/claude-conversation-search-bin/ && ./update-pkg.sh`

   Each should print the new version; troubleshoot only if it fails. AUR commits get no Co-Authored-By trailer.
10. `git push` in **each** AUR clone — `update-pkg.sh` commits but does not push, so the release is not on AUR until this runs. Verify with `git status -sb` showing no ahead count, or `curl -s 'https://aur.archlinux.org/rpc/v5/search/claude-conversation-search?by=name' | jq -r '.results[] | "\(.Name) \(.Version)"'` reporting the new version for both.
