# Windows Runtime (Phase 3d)

**Document status:** Phase 3d implementation document.
**Date:** 2026-11-21.
**Validation status: implemented, cross-compiled and fixture-tested —
NOT yet executed on Windows hardware.** The project owner performs the
manual validation described in §8. Nothing in this document claims a
Windows runtime result.

---

## 1. What was built

OpenCuteMate's character application now has a native Windows runtime
path with **no duplicated character logic**: one platform-neutral
application core, two platform drivers.

```text
                ┌──────────────────────────────────────────────┐
                │ apps/ocm-runtime                             │
                │  main.rs    CLI/config (platform-independent)│
                │  app.rs     App: character, interaction,     │
                │             scheduler, input regions,        │
                │             desktop integration (neutral)    │
                │  desktop.rs per-OS provider attachment       │
                │                                              │
                │  wayland.rs (cfg unix)  windows.rs (cfg win) │
                └───────┬───────────────────────┬──────────────┘
                        │                       │
        ┌───────────────┴────────┐   ┌──────────┴──────────────────┐
        │ ocm-platform-wayland   │   │ ocm-platform-windows (new)   │
        │ layer-shell + wl events│   │ WS_POPUP overlay + Win32     │
        │ calloop source         │   │ messages + waitable timer    │
        └───────────────┬────────┘   └──────────┬──────────────────┘
                        │                       │
        ┌───────────────┴────────┐   ┌──────────┴──────────────────┐
        │ ocm-desktop-niri       │   │ ocm-desktop-windows          │
        │ IPC provider           │   │ Win32 hook provider          │
        └────────────────────────┘   └─────────────────────────────┘
```

Both drivers feed the same neutral `App` and the same
`EnvironmentEvent` vocabulary; renderers (`wgpu` / `bevy`) are selected
through one shared constructor (`app::create_renderer`).

### Scheduling model

`App` expresses scheduling as **deadlines** (`next_deadline` /
`on_deadline`): frame pacing, the 60 s sleep timer and the periodic
environment refresh. Each driver maps them to its own mechanism:

- **Linux**: one one-shot calloop timer re-armed after every dispatch;
  compositor frame callbacks pace animation. Parked = timer disarmed,
  `dispatch(None)` blocks.
- **Windows**: a high-resolution waitable timer
  (`CreateWaitableTimerExW(CREATE_WAITABLE_TIMER_HIGH_RESOLUTION)`) armed
  to the next deadline, waited on via `MsgWaitForMultipleObjectsEx`
  together with the provider wake event. Parked = timer cancelled,
  `INFINITE` wait. Win32 has no presentation callback, so after each
  present the app arms its own next frame opportunity (frame interval
  while animating, one immediate pass to park when settled).

## 2. Windows platform backend (`ocm-platform-windows`)

### Window

One `WS_POPUP` window per monitor (the monitor analogue of the Wayland
layer surface):

- `WS_EX_TOPMOST` — above normal windows;
- `WS_EX_NOACTIVATE` — clicking the character never steals focus;
- `WS_EX_TOOLWINDOW` — no taskbar entry;
- `WS_EX_NOREDIRECTIONBITMAP` — the per-pixel-alpha path (DWM composites
  the swapchain);
- no border, no title bar, monitor-sized so the character can render and
  cast its shadow anywhere on the monitor; the character's position stays
  normalized inside the surface exactly as on Wayland.

`raw-window-handle` carries the HWND to wgpu/Bevy; there is no second
window.

### Transparency: two modes, never silent

1. **Per-pixel alpha (preferred).** The window uses the
   no-redirection path and the renderer negotiates a compositing alpha
   mode (`PreMultiplied` / `Inherit` / `PostMultiplied`). *(Vulkan Win32
   surfaces report these on mainstream drivers; wgpu's DX12 HWND path
   reports `Opaque` only — verified in wgpu-hal 30's source.)*
2. **Color-key fallback.** If the negotiated surface is `Opaque`, the
   renderer clears to magenta and the platform applies
   `WS_EX_LAYERED` + `SetLayeredWindowAttributes(LWA_COLORKEY)`. The
   soft radial shadow is disabled in this mode (binary keying cannot
   keep partial alpha). This is logged loudly at startup and reported by
   the probes.

The renderer exposes `has_per_pixel_alpha()`; the Windows driver applies
the color key only when it is `false`. Nothing falls back silently, and
the Bevy backend refuses an Opaque surface unless the fallback is
allowed, instead of presenting an opaque rectangle.

### Input and click-through

Click-through is decided **per point**, not per window: `WM_NCHITTEST`
converts the screen point to the logical surface frame and answers
`HTCLIENT` inside the character's current input region, `HTTRANSPARENT`
elsewhere (the system then routes the click to the window below). The
region is exactly the rectangle set the runtime already commits for the
compositor input region, so hit-testing cannot disagree with what is
drawn.

- Hover enter/leave: `WM_MOUSEMOVE` + `TrackMouseEvent(TME_LEAVE)`.
- Buttons → neutral `PointerEvent` press/release; `SetCapture` during a
  press keeps drag motion flowing across monitors.
- Right-click on the character is still the exit affordance.
- `WM_NCHITTEST` also drives the window cursor; the window never
  activates, so the user's focused application keeps focus.

### DPI, coordinates, monitors

- Process per-monitor-DPI-aware (v2): Win32 rects are physical pixels.
- The overlay exposes the logical frame `physical ÷ (DPI/96)` — the same
  logical convention the character, physics, camera and hitboxes use on
  Wayland. The only conversions live in `hittest.rs` (pure, unit-tested
  on every OS) plus `ScreenToClient`.
- `WM_DPICHANGED`: update scale/logical size, resize the window to the
  suggested rect, emit `ScaleChanged` + `Configure` (renderer resizes,
  character bounds update).
- `WM_DISPLAYCHANGE`: re-find the covered monitor by device name, apply
  its new rect/DPI, emit `ScaleChanged` + `Configure`. A disappeared
  monitor keeps the last known geometry (a closed lid does not corrupt
  state).
- `--output` selects a monitor by device name (`\\.\DISPLAY1`); the
  default is the primary monitor.

### Event loop primitives

`set_deadline(Option<Duration>)` + `wait_and_dispatch` (blocks in
`MsgWaitForMultipleObjectsEx`, `INFINITE` when no deadline; dispatches
messages and hands neutral events to the driver). `WakeHandle` gives the
desktop provider a thread-safe signal so provider events wake the wait
without polling. The user-visible lifetime mirrors Wayland: overlay
close (`WM_CLOSE`) or `WM_QUIT` ends the loop; teardown drops the
renderer before the window.

## 3. Desktop awareness (Windows)

The Phase 3c provider is attached by the Windows driver exactly as the
Niri provider is on Linux: snapshot at startup, pushed events (window
open/close/focus/motion/state, monitor ticks) drained on each wake via
the wake event, scheduler-driven output re-polls, `ProviderLost`
degrades to "run without awareness" (the character keeps working).
Provider and overlay remain separate responsibilities.

## 4. Renderers

| Renderer | Linux | Windows |
|---|---|---|
| `wgpu` (default) | validated | implemented, cross-compiled; backend set Vulkan + DX12; alpha negotiated, color-key fallback available |
| `bevy` | validated | implemented, cross-compiled; same manual adapter/surface creation, alpha pre-flighted before Bevy configures anything; refuses Opaque surfaces without the fallback permission rather than presenting an opaque window |

No new rendering window: both backends attach to the overlay HWND
through the shared `Renderer` contract.

## 5. Dependencies and build

- `ocm-platform-windows` and `ocm-desktop-windows` use the official
  `windows` crate 0.62 (Win32 bindings); everything is `cfg(windows)`.
  `ocm-platform-wayland`, `ocm-desktop-niri` and `calloop` are
  Unix-only dependencies of the runtime.
- `wgpu` enables `vulkan` + `dx12` (DX12 was added in this phase);
  `wgpu = { features = ["vulkan", "dx12", "wgsl", "std"] }`.
- No subprocesses, no PowerShell, no XWayland.

### Building on Windows

```powershell
# Stable Rust with the MSVC toolchain (rustup default stable-msvc), VS Build Tools
git clone <repo> && cd OpenCuteMate
cargo build --release -p ocm-runtime
```

Cross-checking the Windows code from Linux (what CI runs here):

```sh
rustup target add x86_64-pc-windows-gnu
tests/windows-crosscheck.sh
```

## 6. Running

```powershell
# transparent overlay, character on the primary monitor
cargo run -p ocm-runtime

# other renderer
cargo run -p ocm-runtime -- --renderer bevy

# choose a monitor (device name from the probes)
cargo run -p ocm-runtime -- --output "\\.\DISPLAY2"

# with physics and a starting position
cargo run -p ocm-runtime -- --physics --position 0.5,0.35

# diagnostics (separate tools)
cargo run -p ocm-platform-windows --example win-overlay-probe
cargo run -p ocm-desktop-windows --example win-probe
```

All existing CLI options keep their meaning (`--fps`, `--size`,
`--animate`, `--physics`, `--no-click-through`, `--no-desktop`,
`--no-vsync`, `--log-level`, `--renderer`). `--layer` has no Windows
meaning and is ignored by the Windows driver.

## 7. Diagnostics

- `win-overlay-probe` — platform-only: prints monitors (logical
  position/size/scale), the created overlay's physical/logical frame,
  applies an interactive centre square, then streams pointer/platform
  events. `--color-key` demonstrates the fallback visually (a magenta
  hole).
- `win-probe` — desktop awareness: monitors with work areas/primary,
  windows with titles/pids/geometry/states, live event stream.
- Runtime logs report the adapter, surface capabilities, chosen alpha
  mode/transparency mode, overlay geometry and every teardown step.

## 8. Manual validation checklist (for the project owner)

Run `win-overlay-probe` first, then the app with each renderer.

1. **Window creation** — no border, no taskbar entry, no console window
   flash; the character appears on the primary monitor.
2. **Transparency** — the desktop shows through around the character.
   With per-pixel alpha the soft shadow is visible; if the log says
   "Opaque color-key fallback", transparency is binary and the shadow is
   absent (expected degraded mode — note which renderer/GPU produced it).
3. **Layering** — the character stays above normal windows; open a
   maximized browser and confirm. Alt-Tab to another app and confirm the
   character is not focusable/was not activated.
4. **Click-through** — click and drag on empty desktop area *inside the
   overlay's rectangle*; the clicks reach the desktop/other windows
   (icons, browser) with no character reaction.
5. **Hover/click** — hovering the cube highlights parts; clicking it
   squashes; right-click quits cleanly (check the log prints the exit
   reason and the teardown order).
6. **Dragging** — drag the cube across the screen; it follows exactly,
   clamps at monitor edges, and a fast drag never escapes. Release while
   moving with `--physics` → it flies and settles.
7. **Multi-monitor** — `--output "\\.\DISPLAY2"`: the character lives on
   that monitor; clicks pass through on DISPLAY1 entirely (the overlay is
   monitor-sized). With both monitors at different DPI: drag within the
   selected monitor (mixed-DPI straddling is approximated by the
   monitor's scale — documented).
8. **DPI changes** — change the monitor's scale in Windows settings while
   running: the character keeps its normalized position, stays the same
   physical size fraction, no jump or blur.
9. **Monitor config changes** — rotate a monitor or change resolution:
   the app logs `scale`/`configure`, resizes, keeps running.
10. **Sleep/wake** — leave it untouched for 60 s: it sleeps (eyes
    flatten). Hover/click wakes it.
11. **Rendering stability** — run `--animate` for a minute; no flicker,
    no stale frames, no growing memory; check the log for surface
    errors.
12. **Idle CPU** — when parked (log: "rendering parked (idle)"), Task
    Manager CPU ≈ 0% and no repaint; wakeups should be zero until a
    deadline (sleep timer / 30 s output poll is only armed... note: the
    Windows output re-poll is provider-driven; the 30 s deadline exists
    on Windows too).
13. **Exit** — right-click quits with exit code 0 and the documented
    teardown log.
14. **Bevy** — repeat 2/3/5/6/11/12 with `--renderer bevy`. If it
    reports "Opaque composition only", record it and use wgpu.

Record per test: pass/fail, renderer, GPU/driver, monitor arrangement,
per-pixel vs color-key mode.

## 9. Validation matrix (exact status)

| Item | Implemented | Compiles (Windows target) | Fixture/unit tested | Cross-compiled (Linux) | Runtime-validated Linux | Runtime-validated Windows |
|---|---|---|---|---|---|---|
| Neutral app core (`app.rs`) | yes | yes (via runtime) | — | yes | yes (Niri 45/45) | **no** |
| `ocm-platform-windows` overlay | yes | yes | hit-test/coords (4) | yes | n/a | **no** |
| Win32 event loop + deadlines | yes | yes | — | yes | n/a | **no** |
| Desktop provider attachment (Win32) | yes | yes | provider fixtures (11) | yes | n/a | **no** |
| wgpu renderer on Windows | yes | yes | — | yes | yes (Wayland) | **no** |
| Bevy renderer on Windows | yes | yes | — | yes | yes (Wayland) | **no** |
| Transparent overlay (per-pixel) | yes | yes | — | yes | n/a | **no** |
| Transparent overlay (color key) | yes | yes | — | yes | n/a | **no** |
| Click-through / input | yes | yes | hit-test (4) | yes | yes (Wayland input) | **no** |
| Multi-monitor / mixed DPI | yes | yes | hit-test/coords (4) | yes | yes (Niri multi-output model) | **no** |

## 10. Known limitations

- Everything Windows-side is **unvalidated at runtime** until §8 is
  performed; treat any Windows issue report as first data, not a
  regression.
- The color-key fallback loses the soft shadow and is binary. Which mode
  a given GPU/backend lands in is only knowable at runtime; the logs and
  probes report it.
- No per-monitor overlay selection by clicking; `--output` uses device
  names. The character lives on one monitor (like one Wayland output);
  cross-monitor walking remains future work (§12 of
  `desktop-awareness.md`).
- Mixed-DPI straddling windows/overlays: the overlay is single-monitor,
  so the character's coordinate space is exact; cross-monitor pointer
  drags are clamped at the overlay edge (same class of limitation as
  Wayland's single-output overlay).
- Windows virtual desktops / workspace awareness remains unavailable
  (documented in `desktop-awareness.md` §9.5).
- Fullscreen detection remains unavailable (deliberate).
- The Bevy backend on Windows depends on the surface exposing a
  compositing alpha mode; with Opaque-only backends it refuses instead
  of showing an opaque overlay (use wgpu or the color-key path there).
