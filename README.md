# OpenCuteMate

An open-source desktop companion (in the spirit of Desktop Mate) — animated
characters that live on your desktop.

**Status: Phases 1, 2 and 2b complete; Phase 3a implemented — a first
character (the prototype cube with a face) runs on a generic
character/interaction architecture: semantic model elements, an explicit
state machine, pointer interaction with per-element identification,
deterministic physics and a renderer-neutral animation seam — across two
interchangeable renderers.**

Phase 2 proved the core architectural assumption:

> A native Wayland client using wgpu and `wlr-layer-shell` renders a
> transparent 3D character over the desktop, receives pointer input only on
> the character, passes clicks through everywhere else, and consumes no
> CPU/GPU while idle — without XWayland.

Phase 2b added a Bevy 0.20 rendering backend behind the same platform
contracts and the same scheduler (no `bevy_winit`, no second window —
the runtime drives `app.update()` exactly when a frame is due).

Phase 3a builds the character system on top:

- **`ocm-character`** (new crate, pure simulation — no Wayland, no GPU
  types): a semantic model (elements with roles: Root/Head/Body/arms/
  legs/Tail/Custom — the prototype cube is *data*, not special-cased
  code), the character state machine (Idle/Hover/Dragging/Interacting/
  Falling/Sleeping), the pointer interaction system (element-level
  hover/click/drag/release with click-vs-drag threshold and release
  velocity sampling), deterministic physics (gravity, toss, walls,
  floor; opt-in via `--physics`), and the animation seam
  (`AnimatedPose`) that future glTF clips and VRM expressions plug
  into.
- Both renderers draw arbitrary element/part models and compute
  per-element hitboxes from the same camera math — the single source
  of truth for what is where on screen.
- Dragging uses a client-side pointer grab (input region = whole
  surface while dragging), so a drag can never outrun the character;
  input-region commits are deduplicated.
- The scheduler still parks at zero CPU/GPU; animations are bounded
  one-shots that request frames only while they run.

On the development machine (Niri 26.04, NVIDIA RTX 5050, 1.25×
fractional scaling) the automated suite passes **30/30 for both
`--renderer wgpu` and `--renderer bevy`**: pixel-verified transparency,
element-level hover/click reactions, dragging with input-region
updates, click-through, right-click quit, physics (spawn fall →
landing, drag-toss with release velocity → wall/floor settle), idle
0-tick/0-frame parking, and teardown order. Hyprland validation
remains prepared but blocked by a nested-Hyprland startup deadlock
outside this project's code — see `docs/compositor-compatibility.md`
§4.

## Repository layout

```
crates/ocm-core              config (TOML), errors, logging
crates/ocm-character        model elements, state machine, interaction,
                             physics, animation (pure, Phase 3a)
crates/ocm-platform          platform-neutral traits, events, raw-handle bridge
crates/ocm-platform-wayland  layer-shell, input, outputs, fractional scale, calloop source
crates/ocm-renderer          wgpu 30 backend + Bevy 0.20 backend (shared placement math)
apps/ocm-runtime             CLI + main loop + scheduler + input-region policy + teardown
tests/niri.sh                automated validation suite (nested Niri, both backends)
tests/perf.sh                repeatable renderer performance comparison
tests/hyprland.sh            the same suite against Hyprland
docs/                        research + results (all claims labeled Verified/Assumption/Unresolved)
```

## Quick start

```sh
cargo run -p ocm-runtime                      # wgpu backend (default)
cargo run -p ocm-runtime -- --renderer bevy   # Bevy backend
cargo run -p ocm-runtime -- --physics         # gravity, toss, floor + walls
tests/niri.sh                                 # full automated validation (wgpu)
OCM_RENDERER=bevy tests/niri.sh               # full automated validation (bevy)
tests/perf.sh [--release]                     # renderer performance comparison
```

Pointer interactions: hover any part of the character (it reacts),
click it (squash reaction), drag it (it follows, tilts with speed;
release while moving to toss it), right-click it to quit. After 60 s
untouched it falls asleep; any pointer contact wakes it.

## Documentation

- `docs/architecture.md` — crate graph, lifetimes, teardown order, scheduler
- `docs/character-architecture.md` — character system, state machine,
  semantic elements, interaction flow, physics, animation seam, future
  glTF/VRM integration, current limitations
- `docs/bevy-integration.md` — Phase 2b research, pitfalls, results, perf
- `docs/engine-comparison.md` — Bevy vs Godot vs raw wgpu
- `docs/wayland-feasibility.md` — layer shell, alpha modes, raw-handle GPU surfaces
- `docs/compositor-compatibility.md` — Niri/Hyprland matrix with evidence

License: MIT OR Apache-2.0.
