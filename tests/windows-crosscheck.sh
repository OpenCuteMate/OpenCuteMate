#!/usr/bin/env bash
# Windows-target type check for ocm-desktop-windows.
#
# The cfg(windows) code (Win32 API layer, provider state, win-probe)
# never compiles on a Linux host — which means a broken Windows build
# would go unnoticed. This harness cross-compiles it against
# x86_64-pc-windows-gnu so every Windows-only line is type-checked here,
# on every CI run and before every commit.
#
# Requires: the rust target (rustup target add x86_64-pc-windows-gnu)
# and, for the actual link/execution on Windows hardware, nothing —
# `cargo check` needs no linker. Running the unit tests ON Windows is
# the user's manual step (docs/desktop-awareness.md §10).
#
# Usage: tests/windows-crosscheck.sh
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO"

# The workspace pins its toolchain via rust-toolchain.toml; make sure
# the Windows std for THAT toolchain is installed.
TOOLCHAIN=$(rustup show active-toolchain | awk '{print $1}')

say() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

say "installing the windows-gnu std for $TOOLCHAIN (if missing)"
rustup target add x86_64-pc-windows-gnu

say "type-checking ocm-desktop-windows for x86_64-pc-windows-gnu"
RUSTUP_TOOLCHAIN="$TOOLCHAIN" cargo check -p ocm-desktop-windows \
    --target x86_64-pc-windows-gnu \
    --all-targets

say "type-checking the win-probe diagnostic example"
RUSTUP_TOOLCHAIN="$TOOLCHAIN" cargo check -p ocm-desktop-windows \
    --target x86_64-pc-windows-gnu \
    --example win-probe

say "type-checking the neutral desktop model for the Windows target"
# The shared model must compile on Windows (it is the contract both
# providers feed). The niri provider is Unix-only by nature (Unix-socket
# transport, cfg(unix) at the crate root) and is therefore NOT checked
# here — its regression surface is the Linux suite.
RUSTUP_TOOLCHAIN="$TOOLCHAIN" cargo check -p ocm-desktop \
    --target x86_64-pc-windows-gnu \
    --all-targets

say "PASS: all Windows-targeted code type-checks"
