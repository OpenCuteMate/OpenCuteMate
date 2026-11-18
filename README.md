# OpenCuteMate

An open-source desktop companion (in the spirit of Desktop Mate) — animated
characters that live on your desktop.

**Status: Phase 1 + Phase 2 complete — native Wayland PoC validated on Niri.**

Phase 2 proves the core architectural assumption:

> A native Wayland client using wgpu and `wlr-layer-shell` renders a
> transparent 3D character over the desktop, receives pointer input only on
> the character, passes clicks through everywhere else, and consumes no
> CPU/GPU while idle — without XWayland.

On the development machine (Niri 26.04, NVIDIA RTX 5050, 1.25× fractional
scaling) the automated suite passes 18/18 checks: pixel-verified
transparency (including partially transparent pixels), dragging with
input-region updates, click-through, right-click quit, and measured
idle behavior of 0 CPU ticks with 0 frames presented. Hyprland validation
is prepared but currently blocked by a nested-Hyprland-under-Niri startup
deadlock outside this project's code — see
`docs/compositor-compatibility.md` §4.

## Repository layout

```
crates/ocm-core              config (TOML), errors, logging
crates/ocm-platform          platform-neutral traits, events, raw-handle bridge
crates/ocm-platform-wayland  layer-shell, input, outputs, fractional scale, calloop source
crates/ocm-renderer          wgpu 30 renderer (procedural cube + shadow)
apps/ocm-runtime             CLI + main loop + scheduler + teardown ordering
tests/niri.sh                automated validation suite (nested Niri)
tests/hyprland.sh            the same suite against Hyprland
docs/                        Phase 1 research (all claims labeled Verified/Assumption/Unresolved)
```

## Quick start

```sh
cargo run -p ocm-runtime        # see apps/ocm-runtime/README.md for options
tests/niri.sh                   # full automated validation
```

## Documentation

- `docs/architecture.md` — crate graph, lifetimes, teardown order, scheduler
- `docs/engine-comparison.md` — Bevy vs Godot vs raw wgpu
- `docs/wayland-feasibility.md` — layer shell, alpha modes, raw-handle GPU surfaces
- `docs/compositor-compatibility.md` — Niri/Hyprland matrix with evidence

Rendering is wgpu-direct in the PoC by design; Bevy is the intended runtime
once the platform layer is proven (Phase 2b).

License: MIT OR Apache-2.0.
