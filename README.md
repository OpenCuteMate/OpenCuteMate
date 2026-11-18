# OpenCuteMate

An open-source desktop companion (in the spirit of Desktop Mate) — animated
characters that live on your desktop.

**Status: Phase 1 + 2 complete, Phase 2b (Bevy backend) implemented —
native Wayland PoC validated on Niri with two interchangeable renderers.**

Phase 2 proved the core architectural assumption:

> A native Wayland client using wgpu and `wlr-layer-shell` renders a
> transparent 3D character over the desktop, receives pointer input only on
> the character, passes clicks through everywhere else, and consumes no
> CPU/GPU while idle — without XWayland.

Phase 2b added a Bevy 0.20 rendering backend behind the same platform
contracts and the same scheduler (no `bevy_winit`, no second window —
the runtime drives `app.update()` exactly when a frame is due). Both
backends pass the same 19-check automated suite, and both park at
zero CPU/GPU. On the development machine (Niri 26.04, NVIDIA RTX 5050,
1.25× fractional scaling) the automated suite passes 19/19 for both
`--renderer wgpu` and `--renderer bevy`: pixel-verified transparency
(including partially transparent pixels), dragging with input-region
updates, click-through, right-click quit, and measured idle behavior of
0 CPU ticks with 0 frames presented. Hyprland validation is prepared but
currently blocked by a nested-Hyprland-under-Niri startup deadlock
outside this project's code — see
`docs/compositor-compatibility.md` §4.

## Repository layout

```
crates/ocm-core              config (TOML), errors, logging
crates/ocm-platform          platform-neutral traits, events, raw-handle bridge
crates/ocm-platform-wayland  layer-shell, input, outputs, fractional scale, calloop source
crates/ocm-renderer          wgpu 30 backend + Bevy 0.20 backend (shared placement math)
apps/ocm-runtime             CLI + main loop + scheduler + teardown ordering
tests/niri.sh                automated validation suite (nested Niri, both backends)
tests/perf.sh                repeatable renderer performance comparison
tests/hyprland.sh            the same suite against Hyprland
docs/                        research + results (all claims labeled Verified/Assumption/Unresolved)
```

## Quick start

```sh
cargo run -p ocm-runtime                      # wgpu backend (default)
cargo run -p ocm-runtime -- --renderer bevy  # Bevy backend
tests/niri.sh                                 # full automated validation (wgpu)
OCM_RENDERER=bevy tests/niri.sh               # full automated validation (bevy)
tests/perf.sh [--release]                     # renderer performance comparison
```

## Documentation

- `docs/architecture.md` — crate graph, lifetimes, teardown order, scheduler
- `docs/bevy-integration.md` — Phase 2b research, pitfalls, results, perf
- `docs/engine-comparison.md` — Bevy vs Godot vs raw wgpu
- `docs/wayland-feasibility.md` — layer shell, alpha modes, raw-handle GPU surfaces
- `docs/compositor-compatibility.md` — Niri/Hyprland matrix with evidence

License: MIT OR Apache-2.0.
