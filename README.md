# OpenCuteMate

An open-source desktop companion (in the spirit of Desktop Mate) — animated
characters that live on your desktop.

**Status: Phases 1, 2, 2b, 3a and 3b complete; Phase 3c added native
Windows desktop awareness; Phase 3d added the native Windows runtime
— the character application now has one platform-neutral core and two
drivers: Wayland layer-shell/calloop/Niri on Linux and a Win32
overlay/message-loop/WinEvent provider on Windows, running the same
character, interaction, physics and scheduler on both. Windows
transparency negotiates per-pixel alpha with a documented color-key
fallback, click-through is per-point via hit testing, and idle remains
event-driven (no timers armed while parked). Linux is runtime-validated
by the Niri suite; the Windows runtime is implemented, fixture-tested
and cross-compiled but has **not yet been executed on Windows
hardware** — the manual checklist in `docs/windows-runtime.md` is the
pending validation step.**

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
- **`ocm-desktop`** + providers (Phases 3b/3c, new crates): the
  environmental foundation. `ocm-desktop` is pure and platform-neutral
  (outputs, work areas, windows, workspaces, capability reports,
  environmental events, the `DesktopEnvironment` accumulator, and the
  region-based `DesktopLayout` for multi-display coordinate math —
  gaps, negative positions, per-output scales). `ocm-desktop-niri`
  implements the provider over Niri's JSON IPC socket directly (no
  subprocesses): full state up-front, then windows/workspaces/focus
  events pushed; outputs re-polled cheaply (the event stream has no
  output events). **`ocm-desktop-windows`** (Phase 3c) feeds the same
  model from native Win32: `EnumDisplayMonitors`/`GetMonitorInfoW` with
  real work areas and primary detection, `EnumWindows` with DWM
  visible-bounds geometry, and `SetWinEventHook` tracking on its own
  message-pump thread — no subprocesses. Everything a platform can't
  provide (Niri: work areas, primary output, minimized/maximized/
  fullscreen flags, tiled window positions; Windows: fullscreen state,
  virtual desktops for foreign windows) is reported as unavailable,
  never faked. The Windows provider is type-checked cross-platform
  (`tests/windows-crosscheck.sh`) and awaits runtime validation on real
  Windows hardware (`win-probe`).
- Both renderers draw arbitrary element/part models and compute
  per-element hitboxes from the same camera math — the single source
  of truth for what is where on screen.
- Dragging uses a client-side pointer grab (input region = whole
  surface while dragging), so a drag can never outrun the character;
  input-region commits are deduplicated.
- The scheduler still parks at zero CPU/GPU; animations are bounded
  one-shots that request frames only while they run, and
  environmental events never schedule a frame (verified: a
  window-churn storm schedules zero frames).
- **Phase 3d** split the runtime into a neutral core (`app.rs`: the same
  character/scheduler/input code) plus platform drivers, and added the
  native Windows backend: `WS_POPUP` topmost, non-activating,
  taskbar-free overlay per monitor; per-point click-through via
  `WM_NCHITTEST`; per-monitor DPI mapping to the shared logical
  coordinate model; a high-resolution waitable timer + message wait for
  zero-idle scheduling; both renderers alpha-negotiated (Vulkan
  per-pixel alpha preferred, DX12 color-key fallback).

On the development machine (Niri 26.04, NVIDIA RTX 5050, 1.25×
fractional scaling) the automated suite passes **45/45 for both
`--renderer wgpu` and `--renderer bevy`**: pixel-verified transparency,
element-level hover/click reactions, dragging with input-region
updates, click-through, right-click quit, physics (spawn fall →
landing, drag-toss with release velocity → wall/floor settle), idle
0-tick/0-frame parking, teardown order — plus the Phase 3b checks:
desktop snapshot vs the compositor's own view, window
open/float/focus/close and workspace events through the persistent
subscription, floating-window geometry pixel-verified against a
compositor screenshot, 0 idle CPU with the event stream attached, and
graceful operation without the compositor IPC. Hyprland validation
remains prepared but blocked by a nested-Hyprland startup deadlock
outside this project's code — see `docs/compositor-compatibility.md`
§4.

## Repository layout

```
crates/ocm-core              config (TOML), errors, logging
crates/ocm-character        model elements, state machine, interaction,
                             physics, animation (pure, Phase 3a)
crates/ocm-desktop          neutral desktop environment model, layout
                             math, provider contract (Phases 3b/3c)
crates/ocm-desktop-niri     Niri IPC desktop provider + calloop source
                             (Phase 3b)
crates/ocm-desktop-windows  native Win32 desktop provider + win-probe
                             diagnostic (Phase 3c; runtime-validated on
                             Windows separately — see desktop-awareness.md)
crates/ocm-platform          platform-neutral traits, events, raw-handle bridge
crates/ocm-platform-wayland  layer-shell, input, outputs, fractional scale, calloop source
crates/ocm-renderer          wgpu 30 backend + Bevy 0.20 backend (shared placement math)
apps/ocm-runtime             CLI + main loop + scheduler + input-region policy + teardown
tests/niri.sh                automated validation suite (nested Niri, both backends)
tests/windows-crosscheck.sh  type-checks all Windows-targeted code on Linux
crates/ocm-platform-windows  Win32 overlay, input, DPI, message loop (Phase 3d)
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

Desktop awareness attaches automatically when a Niri IPC socket exists
(`--no-desktop` disables it): the runtime acquires an environment
snapshot and stays updated via the compositor's event stream, logs
window-overlap awareness for future behaviors, and reconciles the
character's bounds with the compositor's work area when one is
reported (Niri does not expose work areas — behavior is unchanged
there).

## Documentation

- `docs/architecture.md` — crate graph, lifetimes, teardown order, scheduler
- `docs/character-architecture.md` — character system, state machine,
  semantic elements, interaction flow, physics, animation seam, future
  glTF/VRM integration, current limitations
- `docs/desktop-awareness.md` — Phase 3b: environment model, coordinate
  spaces, Niri IPC findings, provider guide, capability matrix, perf
- `docs/windows-runtime.md` — Phase 3d: Windows overlay/event
  loop/renderer support, build+run instructions, manual validation
  checklist, exact validation matrix
- `docs/bevy-integration.md` — Phase 2b research, pitfalls, results, perf
- `docs/engine-comparison.md` — Bevy vs Godot vs raw wgpu
- `docs/wayland-feasibility.md` — layer shell, alpha modes, raw-handle GPU surfaces
- `docs/compositor-compatibility.md` — Niri/Hyprland matrix with evidence

License: MIT OR Apache-2.0.
