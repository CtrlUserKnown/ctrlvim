# ctrlvim — build and install.
#
# The binary is `cvi`; the crate that produces it is `ctrlvim` (the workspace
# also holds twelve library crates, which is why every cargo invocation here is
# explicit about its package or scope).
#
#   make                     build the release binary
#   sudo make install        install to /usr/local
#   make install PREFIX=~/.local
#                            install without root (put ~/.local/bin on PATH)
#   sudo make uninstall      remove everything install placed
#   make user-config         seed ~/.config/ctrlvim/config.toml from the example
#
# On macOS the plain targets above build for the host arch and work as written.
# For a binary that runs on both Apple Silicon and Intel:
#
#   make macos               universal (arm64 + x86_64), ad-hoc signed
#   sudo make macos-install   build universal, then install it
#   make macos-deps          rustup target add for both slices
#
# Staged installs work as usual: `make install DESTDIR=/tmp/pkg` for packaging.

# --- knobs -------------------------------------------------------------------

PREFIX     ?= /usr/local
DESTDIR    ?=
CARGO      ?= cargo
INSTALL    ?= install
# Set STRIP=1 to drop debug symbols; a stripped binary is roughly a third the
# size, at the cost of useful backtraces in a bug report.
STRIP      ?= 0

BIN         = cvi
PKG         = ctrlvim
# Respect CARGO_TARGET_DIR when the caller has one, since cargo will.
TARGET_DIR ?= $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)

# --- platform ----------------------------------------------------------------

UNAME_S := $(shell uname -s)

# macOS differs from Linux in four places that matter here: the C toolchain
# ships with Xcode rather than the distro, `strip` needs -x or it mangles a
# Rust binary, anything modified after signing has to be re-signed, and a
# release build usually wants to cover two architectures.
ifeq ($(UNAME_S),Darwin)
MACOS          = 1
STRIP_FLAGS   ?= -x
# Ad-hoc ("-") is enough for a locally installed binary; pass a Developer ID
# here instead when the binary is going somewhere it has to be notarized.
MACOS_SIGN_ID ?= -
MACOS_ARCHS   ?= aarch64-apple-darwin x86_64-apple-darwin
else
MACOS          =
STRIP_FLAGS   ?=
endif

# Set UNIVERSAL=1 (or use the macos* targets) to build a fat binary.
UNIVERSAL ?= 0
UNIVERSAL_DIR = $(TARGET_DIR)/universal-apple-darwin/release
UNIVERSAL_BIN = $(UNIVERSAL_DIR)/$(BIN)

ifeq ($(UNIVERSAL),1)
RELEASE     = $(UNIVERSAL_BIN)
else
RELEASE     = $(TARGET_DIR)/release/$(BIN)
endif

BINDIR      = $(DESTDIR)$(PREFIX)/bin
DATADIR     = $(DESTDIR)$(PREFIX)/share/$(PKG)
DOCDIR      = $(DESTDIR)$(PREFIX)/share/doc/$(PKG)

# Where `make user-config` writes, following the XDG spec the editor itself
# reads at startup.
XDG_CONFIG_HOME ?= $(HOME)/.config
USER_CONFIG      = $(XDG_CONFIG_HOME)/$(PKG)/config.toml

.PHONY: all build install uninstall user-config test lint clean deps help \
        macos macos-install macos-deps universal site-docs

all: build

# --- build -------------------------------------------------------------------

build: deps
ifeq ($(UNIVERSAL),1)
	@$(MAKE) --no-print-directory universal
else
	$(CARGO) build --release --package $(PKG)
endif

# Both Lua and tree-sitter are vendored C, so a compiler is a hard build
# requirement even though nothing here is C. Failing early with a readable
# message beats failing deep inside a build script.
deps:
	@command -v $(CARGO) >/dev/null 2>&1 || { \
		echo "error: cargo not found — install Rust 1.80 or newer (https://rustup.rs)"; \
		exit 1; }
ifeq ($(MACOS),1)
	@# On macOS `cc` is always on PATH, but it is a stub that only errors out
	@# until the Command Line Tools are actually installed — so ask xcode-select
	@# rather than trusting the presence of the binary.
	@xcode-select -p >/dev/null 2>&1 || { \
		echo "error: Xcode Command Line Tools missing — ctrlvim vendors Lua 5.4"; \
		echo "       and tree-sitter, so it needs a C compiler:"; \
		echo "       xcode-select --install"; \
		exit 1; }
else
	@command -v cc >/dev/null 2>&1 || command -v gcc >/dev/null 2>&1 || \
		command -v clang >/dev/null 2>&1 || { \
		echo "error: no C compiler found — ctrlvim vendors Lua 5.4 and tree-sitter"; \
		exit 1; }
endif

# --- macOS -------------------------------------------------------------------

# `make macos` is the release build: one binary that runs on both Apple Silicon
# and Intel. Everyday development wants plain `make`, which is a host-arch build
# and roughly twice as fast.
macos:
	@$(MAKE) --no-print-directory build UNIVERSAL=1

macos-install:
	@$(MAKE) --no-print-directory install UNIVERSAL=1

macos-deps:
	@[ "$(MACOS)" = "1" ] || { echo "error: macos-deps only applies on macOS"; exit 1; }
	rustup target add $(MACOS_ARCHS)

universal: deps
	@[ "$(MACOS)" = "1" ] || { \
		echo "error: a universal binary can only be linked on macOS (need lipo)"; \
		exit 1; }
	@for t in $(MACOS_ARCHS); do \
		rustc --print target-libdir --target $$t >/dev/null 2>&1 || { \
			echo "error: missing std for $$t — run: make macos-deps"; exit 1; }; \
	done
	@for t in $(MACOS_ARCHS); do \
		echo "$(CARGO) build --release --package $(PKG) --target $$t"; \
		$(CARGO) build --release --package $(PKG) --target $$t || exit 1; \
	done
	@mkdir -p $(UNIVERSAL_DIR)
	lipo -create -output $(UNIVERSAL_BIN) \
		$(foreach t,$(MACOS_ARCHS),$(TARGET_DIR)/$(t)/release/$(BIN))
	@# lipo emits an unsigned binary, and on Apple Silicon an unsigned binary is
	@# killed on exec — so signing here is not optional.
	codesign --force --sign $(MACOS_SIGN_ID) $(UNIVERSAL_BIN)
	@lipo -info $(UNIVERSAL_BIN)

test:
	$(CARGO) test --workspace

lint:
	$(CARGO) clippy --workspace --all-targets

# --- install -----------------------------------------------------------------

install: build
	$(INSTALL) -d $(BINDIR)
	$(INSTALL) -m 755 $(RELEASE) $(BINDIR)/$(BIN)
ifeq ($(STRIP),1)
	strip $(STRIP_FLAGS) $(BINDIR)/$(BIN)
endif
ifeq ($(MACOS),1)
	@# Both stripping and copying invalidate a Mach-O signature, so re-sign
	@# whatever ended up in BINDIR. Harmless when nothing was stripped.
	codesign --force --sign $(MACOS_SIGN_ID) $(BINDIR)/$(BIN)
endif
	$(INSTALL) -d $(DATADIR)
	$(INSTALL) -m 644 docs/config.example.toml $(DATADIR)/config.example.toml
	$(INSTALL) -d $(DOCDIR)
	$(INSTALL) -m 644 README.md $(DOCDIR)/README.md
	@echo
	@echo "installed $(BIN) to $(BINDIR)"
	@echo "example config: $(DATADIR)/config.example.toml"
	@echo "run 'make user-config' to copy it to $(USER_CONFIG)"

uninstall:
	rm -f $(BINDIR)/$(BIN)
	rm -f $(DATADIR)/config.example.toml
	rm -f $(DOCDIR)/README.md
	# Only remove the directories we created, and only if nothing else landed
	# in them — an install must not take a shared prefix down with it.
	-rmdir $(DATADIR) $(DOCDIR) 2>/dev/null || true
	@echo "removed $(BIN) from $(BINDIR)"

# Deliberately never overwrites: a config file is the user's, and clobbering a
# hand-written one to "help" is the kind of thing an installer only gets to do
# once.
user-config:
	@if [ -e "$(USER_CONFIG)" ]; then \
		echo "$(USER_CONFIG) already exists — leaving it alone"; \
	else \
		mkdir -p "$(dir $(USER_CONFIG))"; \
		cp docs/config.example.toml "$(USER_CONFIG)"; \
		echo "wrote $(USER_CONFIG)"; \
	fi

# --- site --------------------------------------------------------------------

# The docs page on the site is the wiki, rendered into site/index.html. The
# wiki is a separate repository and GitHub Pages runs nothing at deploy time,
# so the result is generated here and committed. Point WIKI at your checkout.
WIKI ?= $(HOME)/development/wikis/ctrlvim.wiki

site-docs:
	@command -v node >/dev/null 2>&1 || { \
		echo "error: node not found — needed to render the wiki into site/"; \
		exit 1; }
	@test -d "$(WIKI)" || { \
		echo "error: no wiki checkout at $(WIKI)"; \
		echo "       git clone https://github.com/CtrlUserKnown/ctrlvim.wiki.git $(WIKI)"; \
		echo "       or: make site-docs WIKI=/path/to/ctrlvim.wiki"; \
		exit 1; }
	@node site/build-docs.mjs "$(WIKI)"

# --- housekeeping ------------------------------------------------------------

clean:
	$(CARGO) clean

help:
	@echo "targets:"
	@echo "  build         build the release binary ($(RELEASE))"
	@echo "  install       install to \$$PREFIX (default $(PREFIX))"
	@echo "  uninstall     remove the installed files"
	@echo "  user-config   seed $(USER_CONFIG) if it does not exist"
	@echo "  test          run the workspace test suite"
	@echo "  lint          run clippy over the workspace"
	@echo "  site-docs     re-render the wiki into the site's docs page"
	@echo "  clean         remove build artifacts"
ifeq ($(MACOS),1)
	@echo
	@echo "macOS targets:"
	@echo "  macos         universal arm64 + x86_64 binary, ad-hoc signed"
	@echo "  macos-install build universal, then install it"
	@echo "  macos-deps    rustup target add $(MACOS_ARCHS)"
endif
	@echo
	@echo "variables: PREFIX DESTDIR CARGO INSTALL STRIP TARGET_DIR UNIVERSAL"
ifeq ($(MACOS),1)
	@echo "           MACOS_SIGN_ID MACOS_ARCHS"
endif
