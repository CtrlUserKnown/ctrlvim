#!/bin/sh
# ctrlvim installer — portable, POSIX sh. Safe to run via:
#   curl -fsSL https://ctrluserknown.github.io/ctrlvim/install.sh | sh
#
# There are no prebuilt release binaries yet (github.com/CtrlUserKnown/ctrlvim
# has no Releases), so this always builds from source with cargo — the exact
# same thing `make install` does. This script *is* `make install`: it clones
# the repo (or updates an existing clone) and hands off to the Makefile,
# rather than reimplementing install logic that could drift from it.
#
# Installs to ~/.local by default — no root, ever, on this script's own
# initiative. For a system install, run it as root yourself:
#   sudo sh -c "$(curl -fsSL https://ctrluserknown.github.io/ctrlvim/install.sh)" -- --prefix /usr/local
set -eu

# ── configuration ───────────────────────────────────────────────────────────

OWNER="CtrlUserKnown"
REPO="ctrlvim"
REPO_URL="https://github.com/${OWNER}/${REPO}"
SRC_DIR="${CTRLVIM_SRC_DIR:-$HOME/.cache/ctrlvim/src}"
PREFIX="${CTRLVIM_PREFIX:-$HOME/.local}"

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
        --dir)    SRC_DIR="${2:?--dir needs a value}"; shift 2 ;;
        -h|--help)
            printf 'usage: install.sh [--prefix <path>] [--dir <path>]\n\n'
            printf 'Installs to %s by default (no root needed).\n' "$HOME/.local"
            printf 'For a system-wide install, run this script as root yourself.\n'
            exit 0 ;;
        *) printf 'unknown option: %s\n' "$1" >&2; exit 1 ;;
    esac
done

# ── output helpers ───────────────────────────────────────────────────────────

if [ -t 1 ]; then B=$(printf '\033[1m'); D=$(printf '\033[2m'); R=$(printf '\033[0m'); else B=; D=; R=; fi
info() { printf '  %s→%s %s\n' "$D" "$R" "$*"; }
ok()   { printf '  ✓ %s\n' "$*"; }
die()  { printf '\n%sError:%s %s\n' "$B" "$R" "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# ── platform + prerequisites ─────────────────────────────────────────────────
# Mirrors the Makefile's own `deps` target exactly, so whichever entry point a
# user hits (this script, or `make install` after a manual clone) fails with
# the same message for the same reason.

detect_platform() {
    OS=$(uname -s | tr '[:upper:]' '[:lower:]')
    case "$OS" in
        linux|darwin) ;;
        *) die "unsupported OS '$OS' — clone the repo and try \`make install\` yourself:
    ${REPO_URL}" ;;
    esac
}

check_prereqs() {
    have git   || die "git is required — install it and re-run"
    have cargo || die "cargo not found — install Rust 1.80 or newer (https://rustup.rs)"
    if [ "$OS" = "darwin" ]; then
        xcode-select -p >/dev/null 2>&1 || die "Xcode Command Line Tools missing — ctrlvim vendors Lua 5.4
    and tree-sitter, so it needs a C compiler:
    xcode-select --install"
    else
        have cc || have gcc || have clang ||
            die "no C compiler found — ctrlvim vendors Lua 5.4 and tree-sitter"
    fi
}

# ── get the source ───────────────────────────────────────────────────────────

fetch_source() {
    if [ -d "$SRC_DIR/.git" ]; then
        info "Updating existing checkout at ${SRC_DIR}…"
        git -C "$SRC_DIR" fetch --depth 1 origin main
        git -C "$SRC_DIR" reset --hard origin/main
    else
        info "Cloning ${REPO_URL}…"
        mkdir -p "$(dirname "$SRC_DIR")"
        git clone --depth 1 "$REPO_URL" "$SRC_DIR"
    fi
}

# ── build + install (delegates to the Makefile — see it for what this runs) ──

build_and_install() {
    info "Building and installing to $PREFIX (a clean build takes a few minutes)…"
    make -C "$SRC_DIR" install PREFIX="$PREFIX"
    # Never clobbers an existing config — see the Makefile's own comment on
    # why (`user-config` target).
    make -C "$SRC_DIR" user-config >/dev/null 2>&1 || true
}

# ── put cvi on PATH for the user's actual shell ──────────────────────────────

append_line() {  # append_line <file> <line>
    file="$1"; line="$2"
    [ -f "$file" ] || : > "$file"
    if ! grep -qF "$BIN_DIR" "$file" 2>/dev/null; then
        printf '\n%s\n' "$line" >> "$file"
        ok "Added $BIN_DIR to PATH in ${file#"$HOME"/~}"
    else
        ok "PATH already configured in ${file#"$HOME"/~}"
    fi
}

configure_path() {
    BIN_DIR="$PREFIX/bin"
    case ":$PATH:" in
        *":$BIN_DIR:"*) NEEDS_SOURCE=0; return ;;
        *) NEEDS_SOURCE=1 ;;
    esac
    shell_name=$(basename "${SHELL:-sh}")
    export_line="export PATH=\"$BIN_DIR:\$PATH\""
    case "$shell_name" in
        zsh)
            SHELL_RC="$HOME/.zshrc"
            append_line "$SHELL_RC" "$export_line" ;;
        bash)
            SHELL_RC="$HOME/.bashrc"
            append_line "$SHELL_RC" "$export_line"
            [ "$OS" = "darwin" ] && [ -f "$HOME/.bash_profile" ] &&
                append_line "$HOME/.bash_profile" "$export_line" ;;
        fish)
            SHELL_RC="$HOME/.config/fish/config.fish"
            mkdir -p "$(dirname "$SHELL_RC")"
            append_line "$SHELL_RC" "fish_add_path $BIN_DIR" ;;
        *)
            SHELL_RC="$HOME/.profile"
            append_line "$SHELL_RC" "$export_line" ;;
    esac
}

# ── run ───────────────────────────────────────────────────────────────────────

printf '\n%sInstalling ctrlvim%s\n\n' "$B" "$R"
detect_platform
check_prereqs
fetch_source
build_and_install
configure_path

CVI="$PREFIX/bin/cvi"
printf '\n  ✓ %sctrlvim installed%s — %s\n\n' "$B" "$R" "$("$CVI" --version 2>/dev/null || echo "$PREFIX/bin/cvi")"
if [ "${NEEDS_SOURCE:-1}" -eq 1 ]; then
    printf '  Restart your shell or run:\n    %ssource %s%s\n\n' "$B" "$SHELL_RC" "$R"
fi
printf '  Then run %scvi%s to get started.\n\n' "$B" "$R"
