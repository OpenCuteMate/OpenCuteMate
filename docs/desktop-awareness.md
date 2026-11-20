# Desktop Environment Awareness (Phases 3b + 3c)

**Document status:** Phase 3b (Niri) + Phase 3c (Windows) design and
implementation document.
**Verification dates:** 2026-11-19 (Niri), 2026-11-20 (Windows code,
**not runtime-validated on Windows** — see §8/§11). Every capability
claim is labeled:

- **[Verified — Niri]** — exercised against the live Niri 26.04 compositor
  (dev session or the automated suite).
- **[Verified — source]** — confirmed in compositor source/docs, not yet
  exercised here at runtime.
- **[Implemented]** — exists in this codebase; for Windows rows it means
  type-checked cross-platform + fixture-tested, **not yet executed on a
  Windows session**.
- **[Planned]** — designed for, not yet built.
- **[Unavailable]** — the platform cannot provide it; never faked.

The phase's product is the environmental foundation: a platform-neutral
model of the desktop the character lives in, providers that feed it
from Niri's IPC and from native Win32, and a runtime integration that
consumes it without waking the character or the renderer.

---

## 1. Architecture

```text
┌────────────────────────────────────────────────────────────────────┐
│ ocm-runtime                                                        │
│  DesktopState: DesktopEnvironment + provider tokens                │
│  bounds reconciliation · window-overlap reports · output re-poll   │
│  platform seam: apps/ocm-runtime/src/desktop.rs                    │
└──────▲──────────────────────────────────────▲──────────────────────┘
       │ EnvironmentEvent (neutral)           │ DesktopBounds
       │                                      │ (output-local, unchanged)
┌──────┴───────────────────┐         ┌────────┴─────────┐
│ ocm-desktop-niri         │         │ ocm-character    │
│ Niri IPC ⇄ neutral model │         │ physics/phase    │
│ NiriSource (calloop)     │         │ (unchanged)      │
└──────▲───────────────────┘         └──────────────────┘
       │ JSON lines over $NIRI_SOCKET (Unix socket, no subprocesses)
┌──────┴───────────────────┐
│ Niri 26.04               │
└──────────────────────────┘

┌────────────────────────────────────────────────────────────────────┐
│ any consumer (win-probe today; a future Windows runtime)           │
│  WindowsProvider: snapshot + event-driven poll                     │
└──────▲─────────────────────────────────────────────────────────────┘
       │ EnvironmentEvent (the same neutral vocabulary)
┌──────┴───────────────────┐
│ ocm-desktop-windows      │
│ translate (pure) + api   │
│ (cfg(windows) Win32)     │
└──────▲───────────────────┘
       │ EnumWindows / EnumDisplayMonitors / DWM / SetWinEventHook
┌──────┴───────────────────┐
│ Windows 10/11            │
└──────────────────────────┘
```

New crates:

| Crate | Owns | Depends on | Never touches |
|---|---|---|---|
| `ocm-desktop` | The neutral model (`model`), event vocabulary (`events`), the state accumulator `DesktopEnvironment`, the region-based `DesktopLayout`, the `DesktopProvider` contract | `ocm-platform` (the `Rect` type) | any compositor, protocol, renderer, OS |
| `ocm-desktop-niri` | The Niri IPC client (`ipc`), wire-format mirrors + conversion (`convert`), `NiriProvider`, the calloop `NiriSource` | `ocm-desktop`, `serde`/`serde_json`, `calloop` | rendering, character behavior |
| `ocm-desktop-windows` | The pure translation layer (`translate`: raw observations → neutral model, fixture-tested everywhere), the cfg(windows) Win32 API layer (`api`), `WindowsProvider` | `ocm-desktop` (+ `windows` on Windows targets) | rendering, character behavior |

Design rules:

- **Platform specifics stay in the provider crates.** `ocm-desktop`
  speaks in generic outputs/windows/workspaces/capabilities; nothing
  niri- or Win32-shaped crosses the boundary. The trait is
  transport-neutral (no file descriptors, sockets or OS types), so the
  same consumer code runs against any provider. The runtime consumes
  the neutral vocabulary only.
- **Truthfulness.** Information the platform does not expose is
  `None` / a `false` capability — never a guess. See §8.
- **The character stays pure.** `ocm-character` still knows only
  `DesktopBounds` (its output's logical frame). Multi-output movement is
  a future phase (§12); the environment is ready for it, the character
  physics are not claimed to be.

## 2. What already existed vs what was added

Existing (Phase 2/3a, unchanged in behavior):

- Output *discovery* via `wl_output` + `zxdg_output` → `MonitorInfo`
  (`ocm-platform-wayland`): used for output **selection** only.
- `DesktopBounds` (`ocm-character::desktop`): the logical size of the
  character's own output, fed by the layer-shell configure — the
  physics floor/walls/clamps.
- The scheduler: event-driven, parks at zero wakeups.

Added in this phase:

- A **complete** output representation (scale as a fraction, transform,
  physical mode + refresh, work area, primary status — all `Option`
  because they are optional per compositor).
- Window and workspace awareness, where the compositor can honestly
  provide it (Niri: yes, with limits — §5).
- A desktop coordinate model that handles multiple outputs, negative
  positions, gaps and per-output scales (`DesktopLayout`, §3).
- An event stream from the compositor (Niri IPC), consumed
  event-driven with zero polling for the families the stream covers
  (§6).
- The runtime integration (§7).

## 3. Coordinate spaces

Four spaces, all documented in `ocm-desktop/src/layout.rs` and pinned
by unit tests:

| Space | Origin | Units | Used by |
|---|---|---|---|
| **Global logical** | compositor output layout, top-left | logical px, y down | output layout, window geometry, pointer mapping |
| **Output-local logical** | the output's logical position | logical px | the character, physics, layer-shell surface (a full-output overlay's *surface-local == output-local*) |
| **Output-local physical** | same origin | device px | buffer sizing (existing `buffer_size_for`) |
| **Normalized surface** | surface top-left | fraction [0,1] | renderer camera, character config |

Rules:

- The desktop is a **collection of output regions**, never one assumed
  rectangle. `DesktopLayout::output_at` is the containment query;
  `nearest_output` handles points in gaps between displays.
- Containment uses right/bottom-exclusive edges, so a point on the
  shared edge of adjacent outputs belongs to exactly one.
- Conversions are exact `f64` math (`global = local + output.position`,
  `physical = logical × scale`). Rounding happens only at buffer
  boundaries (buffer size = `ceil(logical × scale)`, matching the
  existing fractional-scale path). Nothing in the conversion loop
  rounds — character positions never drift through conversions.
- Output logical positions may be negative (outputs left of / above the
  layout origin); tested.
- Per-output scales differ freely; a 1536×864 logical output at 1.25×
  (the dev machine) converts exactly to 1920×1080 physical — tested
  against the dev machine's actual numbers.
- **Windows has no global logical space of its own**: its virtual-screen
  coordinates are physical. The Windows provider therefore converts
  (physical ÷ per-monitor scale, exact) so every provider feeds the
  same global-logical model — Niri native, Windows converted. Mixed-DPI
  monitor arrangements are fixture-tested; the straddling-window
  approximation and the integer work-area rounding are documented in
  §9.2.

Unit test coverage (`ocm-desktop/src/layout.rs`): single-output
roundtrip exactness, negative positions, side-by-side edge split,
vertical stacking, gaps (containment refuses, conversion stays
arithmetic, nearest picks deterministically), L-shaped arrangements
(no bounding-box assumption), per-output scales, geometry-less outputs
excluded from spatial math, corner clamping.

## 4. Outputs and work areas

Per output, the neutral model carries (all optional):

- stable id (`"eDP-1"` — the same name on the Wayland side and in the
  Niri IPC; **[Verified — Niri]** both report `eDP-1`; Windows uses the
  device name `\\.\DISPLAYN` **[Implemented]**),
- logical position/size **[Verified — Niri]** (matches `niri msg
  outputs` in the suite), scale (fractional, e.g. 1.25) **[Verified —
  Niri]**, transform (the wire uses `"Normal"`, `"90"`…,
  `"Flipped180"` — verified on both a host output and a nested output)
  **[Verified — Niri]**; **[Implemented on Windows]** with the DPI/96
  scale and the clockwise→counter-clockwise orientation inversion
  (direction pending validation on rotated hardware),
- physical mode (resolution + refresh mHz) **[Verified — Niri]**,
  **[Implemented on Windows]** (`EnumDisplaySettingsW`),
- work area — **[Unavailable on Niri]**: niri's IPC exposes neither
  reserved areas nor a primary-output concept. Consumers must treat the
  full logical output as usable, which is what the runtime does.
  **[Implemented on Windows]**: `rcWork` gives the taskbar-reserved
  region directly; the runtime already prefers it over the full output.
- primary — **[Unavailable on Niri]**; **[Implemented on Windows]**
  (`MONITORINFOF_PRIMARY`).

Multi-output: the model and layout fully support it (unit-tested);
Niri runtime coverage is single-output on the dev machine — the nested
compositor also enumerates as one output (`winit`). Multi-monitor
runtime validation remains as in Phase 2: discovery verified on the
live two-output session, full multi-seat behavior limited.

**Output changes** (connect/disconnect/mode/scale): the neutral event
vocabulary and the environment diffing handle them (`OutputAdded`/
`Changed`/`Removed`, `OutputsChanged`), but Niri's event stream
contains **no output events** **[Verified — Niri source + live
session]**, so the runtime re-polls on a 30 s one-shot timer (`{`
`"Outputs":null}` on a fresh socket connection — ~0.06 ms median per
poll, no process spawned; `NiriProvider::fetch_outputs`). Polling is
required and documented per the phase rules; the interval only trades
output-change latency against wakeups, and the scheduler owns it (the
timer is removed on `ProviderLost`).

## 5. Windows

*Windows-side window semantics are in §9; this section is the Niri
findings plus what the neutral model therefore carries for both
providers.*

What the Niri IPC gives per window **[Verified — Niri]**: id, title,
app id, pid, workspace id, focused / floating / urgent flags, sizes
always (`tile_size` incl. decorations, `window_size`), and — only for
**floating** windows — the tile's position in its workspace view.

Verified facts that shape the model:

- **Tiled windows never get pixel positions.** Verified in niri's
  layout sources (`layout/scrolling.rs::tiles_with_ipc_layouts` fills
  `tile_pos_in_workspace_view: None`; only `floating.rs` fills it) and
  against the live compositor (a focused, rendered tiled window still
  reported `null`). A tiled window therefore reports `geometry: None`
  — sizes are known, position is not, and no position is invented.
- **Floating window geometry is pixel-accurate.** The reported tile
  rect `(66,32) 426×886` matched the compositor screenshot: the window
  content sits inside the rect, background outside (suite check).
- **Minimized / maximized / fullscreen flags do not exist in the IPC**
  **[Verified — Niri source]**: `WindowState` keeps them `None`; the
  capability matrix reports them unavailable.
- **Workspace view == the output's logical area** (floating positions
  map 1:1 onto output-local logical coordinates; suite pixel check).

The neutral `WindowInfo` therefore carries: id, title, app id, pid,
workspace id, a `WindowState` of `Option<bool>` flags (Niri fills
focused/floating/urgent; Windows fills focused/minimized/maximized/
visible; minimized/maximized/fullscreen are unavailable on Niri and
fullscreen unavailable on Windows), and
`geometry: Option<WindowGeometry>` (global logical, fractional; Niri:
the tile rect incl. decorations, floating windows only; Windows: the
DWM visible bounds). `pid` was added in Phase 3c for cross-provider
parity (Niri already reported it).

Future interactions (walking on windows, sitting on edges, reacting to
a window moving underneath) are *prepared*, not implemented: the
runtime logs `windows near the character` (count + whether any
geometry-carrying window overlaps the character anchor) whenever the
window set on the character's output changes. No behavior reads it
yet; no window collision physics exist; nothing is assumed about
windows without geometry.

## 6. Niri integration

Transport **[Verified — Niri 26.04]**: JSON over the Unix socket at
`$NIRI_SOCKET`. One request line → one reply line
(`{"Ok":<payload>}` / `{"Err":"..."}`; successful payloads are the
`Response` enum wrapper or `"Handled"`). After
`{"EventStream":null}` the connection becomes a one-way stream of
JSON event lines with the **full current state up-front**
(`WorkspacesChanged`, `WindowsChanged`, …).

- The provider opens the socket itself — **zero subprocesses**, unlike
  `niri msg` (which would spawn one process per poll).
- Event coverage (live): workspaces (full set, activation, active
  window), windows (full set, open-or-changed, closed, focus,
  layouts), plus keyboard/overview/casts/screenshot events that the
  neutral model does not represent (skipped, logged at trace).
- Delivery is event-driven: the runtime registers the socket fd with
  calloop (`NiriSource`, mirroring the hand-rolled `WaylandSource`
  pattern). `poll_events` is a non-blocking drain; partial lines are
  buffered across wakeups.
- **No output events exist** — hence the one-shot output re-poll (§4).
- Forward compatibility: unknown event variants are skipped (niri adds
  variants across releases); malformed lines are logged and dropped, a
  parse failure never takes the app down.
- Connection loss: `NiriSource` emits `EnvironmentEvent::ProviderLost`
  and returns `PostAction::Remove` — calloop aborts the *whole loop* on
  a source error (verified in calloop 0.14.5 `loop_logic.rs`), so an
  erroring source must never surface as `Err`.
- Requests are per-connection; the snapshot does three one-shot
  requests (Outputs/Windows/Workspaces), each self-consistent; the
  event stream's up-front state immediately re-syncs anything that
  raced between them.

Window layout changes arrive as `(id, layout)` pairs; the provider
keeps a small window cache (seeded by the full sets, patched by
layout events) to emit complete neutral `WindowChanged` events. The
cache is provider-internal; the environment never sees niri types.

## 7. Runtime integration

- Awareness attaches automatically when an IPC socket exists;
  `--no-desktop` disables it. An unavailable socket logs the absence
  and the app runs exactly as Phase 3a (**Verified — suite**: dead
  socket → "desktop awareness unavailable" logged, app parks, exits 0).
- The initial snapshot establishes the environment; subsequent events
  update it.
- **Bounds reconciliation**: the character's bounds become the
  environment's work area when the compositor reports one, else the
  reported logical output — and only when that actually differs from
  the layer-shell configure. On Niri (no work areas) this is
  value-identical to the Phase 2 behavior; the integration boundary is
  in place for compositors that do report reserved areas (e.g. a
  future Hyprland provider, `hyprctl`-style work areas).
- **Scheduler neutrality**: window/workspace/focus events update the
  environment and log at debug; they never schedule a frame
  (**Verified — suite**: a 6-window churn storm schedules 0 frames).
  Only a real change to the character's output geometry can touch the
  character (`Character::set_bounds` — which may start a fall, exactly
  like a layer-shell configure).
- **Window-overlap report** (the Phase 3b integration boundary, §5):
  logged only when the counts change, on window changes only.

## 8. Capability matrix

Cross-platform view (labels: **Verified** = executed against the real
platform; **Implemented** = code exists, **not yet executed on that
platform**; **Unavailable** = the platform cannot provide it):

| Capability | Niri 26.04 | Windows 10/11 |
|---|---|---|
| Output discovery | Verified | Implemented, pending Windows validation |
| Output geometry (position/size) | Verified | Implemented (physical ÷ scale, exact) |
| Per-output scale | Verified (fractional 1.25) | Implemented (DPI/96; mixed-DPI fixture-tested) |
| Output mode + refresh | Verified | Implemented (`EnumDisplaySettingsW`) |
| Output transform/orientation | Verified | Implemented — direction convention pending validation on rotated hardware |
| Work areas | **Unavailable** (no IPC) | **Implemented** (`rcWork`: taskbar-reserved regions) |
| Primary output | **Unavailable** (no concept) | **Implemented** (`MONITORINFOF_PRIMARY`) |
| Output change events | **Unavailable** (no output events; 30 s re-poll) | **Unavailable** (no plain-client push; 10 s pump tick) |
| Window enumeration | Verified | Implemented (`EnumWindows`, documented filter) |
| Window geometry | Verified — floating windows only, pixel-accurate | Implemented — DWM extended frame bounds (visible window), `GetWindowRect` fallback; minimized = none |
| Window state (minimized/maximized) | **Unavailable** (no IPC fields) | Implemented (`IsIconic`/`IsZoomed`) |
| Window state (fullscreen) | **Unavailable** | **Unavailable** (rect-vs-monitor would be a heuristic — refused) |
| Window visibility | **Unavailable** (not directly reported) | Implemented (`IsWindowVisible`) |
| App identity | Verified (app_id) | Implemented (lowercased exe file stem; elevated processes = None) |
| Process id | Verified | Implemented (`GetWindowThreadProcessId`) |
| Window focus | Verified | Implemented (`GetForegroundWindow` + `EVENT_SYSTEM_FOREGROUND`) |
| Window events (open/close/move/state) | Verified (event stream) | Implemented (`SetWinEventHook`, coalesced re-queries) |
| Workspaces / virtual desktops | Verified (niri-native workspaces) | **Unavailable** — no documented API for foreign windows (§9.5) |
| Provider capability reporting | Verified | Implemented (asserted in fixtures) |
| Graceful degradation / teardown | Verified (dead socket, suite) | Implemented by construction (hook lifecycle; requires Windows validation) |

## 9. The Windows provider (Phase 3c)

`ocm-desktop-windows` implements the same neutral contract with native
Win32 APIs — no subprocesses, no PowerShell. The crate splits into a
**pure translation layer** (`translate.rs`: plain-data structs +
conversion functions, fixture-tested on every OS) and a **cfg(windows)
API layer** (`api.rs`: the Win32 calls that fill the raw structs). The
Windows build is type-checked on every Linux run
(`tests/windows-crosscheck.sh`, x86_64-pc-windows-gnu); **executing it
is the user's manual step** — nothing in this document claims Windows
runtime validation.

### 9.1 APIs used

| Data | API |
|---|---|
| Monitors, rects, work areas, primary | `EnumDisplayMonitors`, `GetMonitorInfoW` |
| Mode: resolution, refresh, orientation | `EnumDisplaySettingsW` (via the monitor's device name) |
| Per-monitor DPI | `GetDpiForMonitor` (`MDT_EFFECTIVE_DPI`; 96 fallback) |
| Windows, titles, states | `EnumWindows`, `IsWindowVisible`, `IsIconic`, `IsZoomed`, `GetWindowTextW`, `GetWindowLongPtrW` |
| Visible window bounds | `DwmGetWindowAttribute` (`DWMWA_EXTENDED_FRAME_BOUNDS`), `GetWindowRect` fallback |
| Cloaking (suspended UWP) | `DwmGetWindowAttribute` (`DWMWA_CLOAKED`) |
| Process identity | `GetWindowThreadProcessId`, `OpenProcess` + `QueryFullProcessImageNameW` (per-pid cached) |
| Focus | `GetForegroundWindow`, `EVENT_SYSTEM_FOREGROUND` |
| Change events | `SetWinEventHook` / `UnhookWinEvent` (`WINEVENT_OUTOFCONTEXT`) |
| Pump + monitor tick | `GetMessageW` / `TranslateMessage` / `DispatchMessageW`, `SetTimer`/`KillTimer`, `PostThreadMessageW` (`WM_QUIT`) |

All geometry reads run under a per-thread
`DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2` context (set and restored)
so rects are true physical pixels regardless of process DPI
virtualization.

### 9.2 Geometry semantics

`WindowInfo.geometry` is the **visible window rectangle**: DWM extended
frame bounds when available (excludes the invisible resize borders that
`GetWindowRect` includes), falling back to `GetWindowRect`. Minimized
windows report `geometry: None` (Win32 parks them at −32000 — not an
on-screen position, so none is claimed). Conversion to the neutral
global-logical space divides by the window's monitor scale (exact);
a window straddling two monitors with different scales converts with
the scale of `MonitorFromWindow`'s pick — the same approximation
Wayland mixed-DPI makes, documented.

### 9.3 Window filtering

Excluded, exactly and fixture-tested:

- **invisible** (`IsWindowVisible` covers the parent chain) — not on
  screen;
- **cloaked** (`DWMWA_CLOAKED`) — suspended UWP apps that are alive
  per Win32 but not rendered;
- **tool windows** (`WS_EX_TOOLWINDOW`) — palettes/toolbars the shell
  itself excludes from task switching.

Everything else `EnumWindows` reports is kept (owned dialogs, console
windows, browser popups included). Untitled-but-visible windows stay in
the model with `title: None`. No over-filtering of legitimate
applications.

### 9.4 Event tracking shape

One hook over both consumed event families (system 0x0003.. and object
0x8000..) on a dedicated thread running a `GetMessageW` pump — the
shape Win32 requires (callbacks are delivered to the hook thread and
only while it pumps). Child-object traffic (`idObject != OBJID_WINDOW`,
the bulk of WinEvent volume) is filtered at the callback. Raw events
flow over a bounded channel (4096); overflow drops and counts, logged
once, never blocking the hook thread. The same pump owns a `SetTimer`
tick (10 s) for monitor re-enumeration — Windows has no monitor-change
push for plain clients, so this is the documented required poll. The
provider maps raw events to **coalesced actions** (one re-query per
window per poll; removals before refreshes), keeping a window cache
for add/change/remove detection — the same shape as the Niri
provider.

Startup handshake: the pump reports its thread id and the hook
outcome; a failed hook surfaces as a connect error (awareness
unavailable, never fatal). Teardown posts `WM_QUIT`, unhooks, kills
the timer and joins. Hook failures and pump death end with
`ProviderLost`-style behavior, not crashes or busy loops.

### 9.5 Virtual desktops: status

Windows virtual desktops have no documented API for *third-party*
windows: `IVirtualDesktopManager` is documented only for the caller's
own windows and fails with foreign hwnds. The reverse-engineered
`IVirtualDesktopManagerInternal` route is out of scope per the phase
rules (maintenance risk without stability guarantees). Therefore:
`workspaces: false`, `window_workspace: false` — unavailable and
documented, not faked. Core Windows awareness does not depend on this.

## 10. Runtime portability status

The desktop-awareness **architecture** is platform-neutral: `ocm-desktop`
compiles everywhere (cross-checked for the Windows target), providers
implement one neutral trait, the runtime consumes only neutral types.

The **runtime binary** remains Wayland-hosted (layer-shell surface,
calloop, Vulkan-Wayland renderer). Its provider wiring moved behind an
explicit seam (`apps/ocm-runtime/src/desktop.rs`): one neutral
`attach()` call, one cfg-gated implementation per platform — Niri on
Linux (unchanged, suite-validated), a logged absence elsewhere.
Running the character app on Windows needs, in addition to the provider
(which is done): a layered-window platform backend in `ocm-platform`,
a Win32 message loop implementing the event-source contract, and
DXGI/`raw-window-handle` surface wiring in the renderer. That is the
documented next-phase porting surface — **no fake Windows overlay
exists, and full Windows compatibility is not claimed**.

## 11. Windows manual validation (for the user)

On a real Windows machine:

```sh
cargo run -p ocm-desktop-windows --example win-probe
```

Checks, in order:

1. **Monitors**: one line per display — verify physical resolution,
   scale (`DPI/96`), logical position/size, work area (taskbar
   subtracted), primary flag against the real arrangement. Repeat with
   mixed-DPI monitors and a monitor placed at negative offsets /
   with a gap.
2. **Windows snapshot**: titles, app ids, pids, geometry, states.
   Compare a window's reported geometry with its visible bounds.
3. **Events**: open and close a window (`window-added` /
   `window-removed`), minimize and restore it (`window-changed` with
   geometry none/present), maximize it, focus between two windows
   (`window-focused`), drag a window across monitors (continuous
   `window-changed`), then unplug or change a monitor (outputs-changed
   within ~10 s).
4. **Idle behavior**: with nothing happening, the probe prints
   nothing — no polling noise; CPU near zero.
5. **Teardown**: Ctrl+C exits cleanly.

Also run the unit test binary natively (`cargo test -p
ocm-desktop-windows`) — the 11 pure-translation fixture tests execute
on Windows as well.

## 12. Multi-output movement (investigated, not implemented)

The environment and layout are ready (regions, per-output scales,
`nearest_output`, conversion both ways), but actual cross-output
character movement needs one of:

1. one overlay surface per output, or
2. layer-shell output re-assignment (destroy + recreate on the target
   output, with a position hand-off across differently-scaled frames).

Both are renderer/platform work with real positioning-semantics
questions (how the anchor maps when scales differ), deferred to the
phase that implements cross-output behavior — with the same honesty
rule: nothing moves between outputs until that mechanism exists and is
validated.

## 13. Performance

Niri measurements on the dev machine (Niri 26.04, RTX 5050, debug
build unless noted):

| Measurement | Result |
|---|---|
| Idle CPU, awareness attached, parked (10 s, nested compositor) | **0 ms, 0 frames** |
| Idle CPU, awareness attached, parked (10 s, live host session) | **0 ms, 0 wakeups** (idle desktop) |
| Idle CPU, awareness disabled (same machine, same window) | 0 ms (baseline unchanged) |
| Window-event storm (6× kitty open/close + workspace switches ≈ 40 events) | **+20 ms total CPU** vs 0 baseline (≈ 0.5 ms per event batch: read+parse+diff+log; **0 frames scheduled**) |
| Output re-poll (one-shot IPC request) | 0.037–0.286 ms (p95), 1.1 ms max — once per 30 s, no process |
| Event-stream up-front state | 862 B; no ongoing traffic on an idle desktop |
| RSS delta vs `--no-desktop` | +0.8 MB (wgpu backend, debug) |

Environmental subscriptions wake the app only when the environment
actually changes (event-driven fd, no polling loop); a busy desktop
produces proportionally small parse+diff work and **zero frames** —
awareness never renders.

**Windows measurements: none.** The provider design keeps the same
invariants (event-driven hook, coalesced re-queries, a 10 s monitor
tick that is one enumeration ≈ 0.1 ms), but idle CPU/wakeup/memory
numbers require a real Windows session and have **not** been measured —
the `win-probe` session in §11 is where they get recorded (add a
procfs-style reading from Task Manager or `typeperf` during the idle
phase and note it here).

## 14. Limitations

- Niri window geometry: floating windows only (tiled → `None`).
  Position-accurate "walking on windows" for tiled layouts needs either
  an upstream niri change or a helper; nothing is faked meanwhile.
- No minimized/maximized/fullscreen knowledge on Niri.
- No work areas / primary output on Niri (model + runtime plumbing
  ready for providers that have them — Windows exercises both paths in
  fixtures, pending runtime validation).
- No output events on Niri → 30 s re-poll latency for output changes.
  (The character's own output changes still arrive instantly via the
  layer-shell configure/fractional-scale events.)
- Windows: no monitor-change push → 10 s pump-tick latency (§9.4).
- Windows: fullscreen deliberately undetected (would be a heuristic);
  virtual desktops unavailable (§9.5).
- Windows: everything in §9 is type-checked cross-platform and
  fixture-tested, but **not yet executed on Windows hardware** — the
  capability matrix says "Implemented, pending Windows validation"
  until the user's `win-probe` run (§11).
- Workspace urgency is parsed but not represented in the neutral event
  set (planned; `WorkspaceInfo.urgent` exists for snapshots).
- The runtime window-overlap report is logging-only: no behavior reads
  windows yet (per phase scope).
- Multi-output runtime validation remains single-output on the dev
  machine (§12).
- The character application is not Windows-hostable yet (§10): desktop
  awareness is, the runtime/overlay is not.

## 15. Adding a provider (guide)

1. New crate depending on `ocm-desktop` (+ your transport); keep the
   translation layer pure (plain-data structs) and the API layer
   platform-gated, mirroring `translate.rs`/`api.rs` in
   `ocm-desktop-windows`.
2. Implement `DesktopProvider`: truthful `capabilities()`, blocking
   `snapshot()`, non-blocking `poll_events()`, and `refresh_outputs()`
   when your push stream misses outputs. (The trait is
   transport-neutral: no file descriptors, sockets or OS types.)
3. Translate platform structures into the neutral model; `None` for
   anything the platform does not expose — never defaults.
4. Event-loop integration is the provider crate's business (see
   `NiriSource` for the calloop shape; the Windows provider pumps its
   own hook thread and exposes only `poll_events`).
5. Tests: fixtures over captured real data (run everywhere) + live
   tests that skip without the platform, clearly separated; for
   non-host platforms add a cross-check harness
   (`tests/windows-crosscheck.sh` is the pattern).
6. Document the capability rows in §8 with the same labels, and update
   the runtime seam if the provider needs construction flags.
