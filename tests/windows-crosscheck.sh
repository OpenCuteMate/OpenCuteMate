#!/usr/bin/env bash
# Windows-target type check.
#
# All cfg(windows) code in the workspace (Win32 overlay, desktop provider,
# runtime driver, renderer backends, probes) never compiles on a Linux
# host — a broken Windows build would go unnoticed. This harness
# cross-compiles every Windows-targeted crate and example against
# x86_64-pc-windows-gnu on every run.
#
# Requires: the rust target (rustup target add x86_64-pc-windows-gnu).
# `cargo check` needs no linker; actual execution on Windows hardware is
# the manual step documented in docs/windows-runtime.md.
#
# Deliberately excluded: ocm-platform-wayland and ocm-desktop-niri (Unix
# sockets/Wayland; cfg(unix), covered by the Niri suite) and their
# dependents' Linux-only paths.
#
# Usage: tests/windows-crosscheck.sh
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO"

TOOLCHAIN=$(rustup show active-toolchain | awk '{print $1}')

say() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

say "installing the windows-gnu std for $TOOLCHAIN (if missing)"
rustup target add x86_64-pc-windows-gnu

say "type-checking the neutral desktop model"
RUSTUP_TOOLCHAIN="$TOOLCHAIN" cargo check -p ocm-desktop \
    --target x86_64-pc-windows-gnu --all-targets

say "type-checking the Win32 desktop provider + its probe"
RUSTUP_TOOLCHAIN="$TOOLCHAIN" cargo check -p ocm-desktop-windows \
    --target x86_64-pc-windows-gnu --all-targets

say "type-checking the Win32 platform (overlay) + its diagnostic probe"
RUSTUP_TOOLCHAIN="$TOOLCHAIN" cargo check -p ocm-platform-windows \
    --target x86_64-pc-windows-gnu --all-targets

say "type-checking the renderer backends for Windows"
RUSTUP_TOOLCHAIN="$TOOLCHAIN" cargo check -p ocm-renderer \
    --target x86_64-pc-windows-gnu --all-targets

say "type-checking the runtime with its Windows driver"
RUSTUP_TOOLCHAIN="$TOOLCHAIN" cargo check -p ocm-runtime \
    --target x86_64-pc-windows-gnu --all-targets

say "PASS: all Windows-targeted code type-checks"
