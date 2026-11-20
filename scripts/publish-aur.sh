#!/bin/bash
# Publish both AUR packages for a released tag from the recipes in packaging/aur/.
#
# Usage: ./scripts/publish-aur.sh [-n] [vX.Y.Z]
#          -n   prepare and build in each clone, but do not commit or push
#          default tag: v$(version from Cargo.toml)
#
# packaging/aur/<package>/PKGBUILD is the canonical recipe; `make arch-pkg`
# builds the source one from the working tree. The AUR clones under
# ~/.cache/paru/clone/ only receive it: each is reset to the AUR's master, gets
# the recipe with pkgver set to the tag and pkgrel to 1, checksums of the
# published release assets, a fresh .SRCINFO and a test build, then a commit
# and a push. The reset discards anything uncommitted in a clone. The build
# downloads the release assets, so it fails until the GitHub release is
# published and signed.

set -euo pipefail

cd "$(dirname "$0")/.."
REPO=$PWD

DRY_RUN=
case "${1:-}" in
	-h|--help) sed -n '2,15p' "$0"; exit 0 ;;
	-n) DRY_RUN=1; shift ;;
	-*) echo "unknown option: $1" >&2; exit 1 ;;
esac

TAG="${1:-v$(grep -Po '^version = "\K[^"]+' Cargo.toml)}"
VERSION="${TAG#v}"
PACKAGES=(claude-conversation-search claude-conversation-search-bin)

for package in "${PACKAGES[@]}"; do
	clone="$HOME/.cache/paru/clone/$package"
	recipe="$REPO/packaging/aur/$package/PKGBUILD"
	if [[ ! -d "$clone" ]]; then
		echo "missing AUR clone: $clone" >&2
		echo "git clone ssh://aur@aur.archlinux.org/$package.git $clone first" >&2
		exit 1
	fi

	echo "== $package $VERSION"
	git -C "$clone" fetch --prune
	git -C "$clone" reset --hard origin/master
	sed -e "s/^pkgver=.*/pkgver=$VERSION/" -e "s/^pkgrel=.*/pkgrel=1/" "$recipe" > "$clone/PKGBUILD"
	(
		cd "$clone"
		updpkgsums
		makepkg --printsrcinfo > .SRCINFO
		makepkg -f
	)
	git -C "$clone" add PKGBUILD .SRCINFO

	if [[ -n "$DRY_RUN" ]]; then
		git -C "$clone" diff --cached --stat
		continue
	fi
	git -C "$clone" commit -m "update to $VERSION"
	git -C "$clone" push
done
