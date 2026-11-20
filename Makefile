CARGO := cargo
RELEASE := --release
CONTAINER ?= $(shell command -v podman || command -v docker)

NAME := claude-conversation-search

# Targets
LINUX_X86 := x86_64-unknown-linux-gnu
LINUX_ARM := aarch64-unknown-linux-gnu
WINDOWS := x86_64-pc-windows-gnu
MACOS := aarch64-apple-darwin

# Linkers
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER := aarch64-linux-gnu-gcc

CARGO_VERSION := $(shell grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
GIT_SHORT := $(shell git rev-parse --short HEAD)
GIT_TAG := $(shell git tag --points-at HEAD --list 'v*' | head -1)
GIT_DIRTY := $(shell git status --porcelain --untracked-files=no | head -1)
# A dev .deb must never be mistakable for the released one
DEB_VERSION := $(if $(GIT_DIRTY),$(CARGO_VERSION)+$(GIT_SHORT).dirty,$(if $(GIT_TAG),$(CARGO_VERSION),$(CARGO_VERSION)+$(GIT_SHORT)))

# pacman versions allow no '+' or '-'. Deferred (=) so only the arch targets need a v* tag.
ARCH_SINCE_TAG = $(shell git rev-list --count $(shell git describe --tags --match 'v*' --abbrev=0)..HEAD)
ARCH_PKGVER = $(CARGO_VERSION)$(if $(GIT_DIRTY)$(filter-out 0,$(ARCH_SINCE_TAG)),.r$(ARCH_SINCE_TAG).g$(GIT_SHORT))$(if $(GIT_DIRTY),.dirty)
ARCH_DIR := target/arch

DEB_AMD64 := $(NAME)_$(DEB_VERSION)_amd64.deb
DEB_ARM64 := $(NAME)_$(DEB_VERSION)_arm64.deb
PKG := package.tmp

COMPLETIONS := dist/completions/bash dist/completions/zsh dist/completions/fish
DIST := dist/$(NAME).amd64 dist/$(NAME).arm64 \
	dist/glibc-floor.amd64 dist/glibc-floor.arm64 \
	dist/depends-extra.amd64 dist/depends-extra.arm64 \
	$(COMPLETIONS)

.PHONY: all clean linux windows macos linux-x86 linux-arm check test fmt clippy dist deb deb-amd64 deb-arm64 arch-pkg arch-install aur-check

all: linux windows macos

linux: linux-x86 linux-arm

linux-x86:
	$(CARGO) build $(RELEASE) --target $(LINUX_X86)

linux-arm:
	$(CARGO) build $(RELEASE) --target $(LINUX_ARM)

windows:
	$(CARGO) build $(RELEASE) --target $(WINDOWS)

macos:
	cargo zigbuild $(RELEASE) --target $(MACOS)

check:
	$(CARGO) check --message-format=short

test:
	$(CARGO) test

fmt:
	$(CARGO) fmt

clippy:
	$(CARGO) clippy --fix --allow-dirty --message-format=short

dist: $(DIST)

# Grouped target (&:), GNU make >= 4.3: one container pass produces all of
# these. Older make parses & as another target name and builds a wrong graph.
$(DIST) &: Containerfile Cargo.toml Cargo.lock $(shell find src -name '*.rs') packaging/$(NAME)-index.service
	rm -rf dist
	$(CONTAINER) build -f Containerfile --output type=local,dest=dist .

deb: deb-amd64 deb-arm64

deb-amd64: $(DEB_AMD64)

deb-arm64: $(DEB_ARM64)

# $(1) is the Debian architecture, and selects the binary, the floor and the
# derived dependency list that go with it.
define build-deb
	rm -rf "$(PKG)"
	install -D -m 755 -T "dist/$(NAME).$(1)" "$(PKG)/usr/bin/$(NAME)"
	install -D -m 644 -T dist/completions/bash "$(PKG)/usr/share/bash-completion/completions/$(NAME)"
	install -D -m 644 -T dist/completions/zsh "$(PKG)/usr/share/zsh/vendor-completions/_$(NAME)"
	install -D -m 644 -T dist/completions/fish "$(PKG)/usr/share/fish/vendor_completions.d/$(NAME).fish"
	install -D -m 644 -T packaging/$(NAME)-index.service "$(PKG)/usr/lib/systemd/user/$(NAME)-index.service"
	install -D -m 644 -T LICENSE "$(PKG)/usr/share/doc/$(NAME)/copyright"
	install -D -m 644 -T packaging/control "$(PKG)/DEBIAN/control"
	sed -i -e "s/^Version:.*/Version: $(DEB_VERSION)/" \
		-e "s/^Architecture:.*/Architecture: $(1)/" \
		-e "s/^Depends:.*/Depends: libc6 (>= $$(cat dist/glibc-floor.$(1)))$$(cat dist/depends-extra.$(1))/" \
		"$(PKG)/DEBIAN/control"
	@if grep -rq "$$PWD" "$(PKG)"; then echo "ERROR: package contains build path ($$PWD)" >&2; exit 1; fi
	dpkg-deb --build --root-owner-group "$(PKG)" "$@"
	rm -rf "$(PKG)"
	@for stale in $(NAME)_*.deb; do \
		[ -e "$$stale" ] || continue; \
		case "$$stale" in $(DEB_AMD64)|$(DEB_ARM64)) ;; *) rm -f "$$stale" ;; esac; \
	done
endef

$(DEB_AMD64): dist/$(NAME).amd64 dist/glibc-floor.amd64 dist/depends-extra.amd64 $(COMPLETIONS) packaging/control packaging/$(NAME)-index.service LICENSE
	$(call build-deb,amd64)

$(DEB_ARM64): dist/$(NAME).arm64 dist/glibc-floor.arm64 dist/depends-extra.arm64 $(COMPLETIONS) packaging/control packaging/$(NAME)-index.service LICENSE
	$(call build-deb,arm64)

# Build the AUR source recipe from the working tree: tracked files, committed or
# not, packed as the release tarball it expects, with a placeholder signature.
arch-pkg:
	rm -rf "$(ARCH_DIR)"
	install -D -m 644 packaging/aur/$(NAME)/PKGBUILD "$(ARCH_DIR)/PKGBUILD"
	sed -i 's/^pkgver=.*/pkgver=$(ARCH_PKGVER)/; s/^pkgrel=.*/pkgrel=1/' "$(ARCH_DIR)/PKGBUILD"
	tree=$$(git stash create); git archive --format=tar --prefix="$(NAME)-$(ARCH_PKGVER)/" "$${tree:-HEAD}" \
		| xz -c > "$(ARCH_DIR)/$(NAME)-$(ARCH_PKGVER).tar.xz"
	touch "$(ARCH_DIR)/$(NAME)-$(ARCH_PKGVER).tar.xz.asc"
	cd "$(ARCH_DIR)" && PKGDEST="$(CURDIR)/$(ARCH_DIR)/out" makepkg -f --check --skipchecksums --skippgpcheck

# Install that package, replacing either AUR package.
arch-install: arch-pkg
	sudo pacman -U "$(ARCH_DIR)"/out/$(NAME)-$(ARCH_PKGVER)-*.pkg.tar.zst

# Both AUR recipes must at least parse; the -bin one fetches release assets, so
# it is only fully exercised by publish-aur.sh.
aur-check:
	for recipe in packaging/aur/*/PKGBUILD; do \
		(cd "$${recipe%/PKGBUILD}" && makepkg --printsrcinfo > /dev/null) || exit 1; \
	done

clean:
	$(CARGO) clean
	rm -rf "$(PKG)" dist $(NAME)_*.deb
