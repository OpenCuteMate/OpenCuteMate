# Desktop Environment Awareness (Phase 3b)

**Document status:** Phase 3b design + implementation document.
**Verification date:** 2026-11-19. Every capability claim is labeled:

- **[Verified — Niri]** — exercised against the live Niri 26.04 compositor
  (dev session or the automated suite).
- **[Verified — source]** — confirmed in compositor source/docs, not yet
  exercised here at runtime.
- **[Implemented]** — exists in this codebase, generic (not
  compositor-provided).
- **[Planned]** — designed for, not yet built.
- **[Unavailable]** — the compositor cannot provide it; never faked.

The phase's product is the environmental foundation: a compositor-neutral
model of the desktop the character lives in, a provider that feeds it
from Niri's IPC, and a runtime integration that consumes it without
waking the character or the renderer.

---

## 1. Architecture

```text
┌────────────────────────────────────────────────────────────────────┐
│ ocm-runtime                                                        │
│  DesktopState: DesktopEnvironment + provider tokens                │
│  bounds reconciliation · window-overlap reports · output re-poll   │
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
```

New crates:

| Crate | Owns | Depends on | Never touches |
|---|---|---|---|
| `ocm-desktop` | The neutral model (`model`), event vocabulary (`events`), the state accumulator `DesktopEnvironment`, the region-based `DesktopLayout`, the `DesktopProvider` contract | `ocm-platform` (the `Rect` type) | any compositor, protocol, renderer |
| `ocm-desktop-niri` | The Niri IPC client (`ipc`), wire-format mirrors + conversion (`convert`), `NiriProvider`, the calloop `NiriSource` | `ocm-desktop`, `serde`/`serde_json`, `calloop` | rendering, character behavior |

Design rules:

- **Compositor specifics stay in the provider crate.** `ocm-desktop`
  speaks in generic outputs/windows/workspaces/capabilities; nothing
  niri-shaped crosses the boundary. The runtime consumes the neutral
  vocabulary only (it names `NiriProvider` for construction, exactly
  like it names `WaylandPlatform`).
- **Truthfulness.** Information the compositor does not expose is
  `None` / a `false` capability — never a guess. See §8.
- **The character stays pure.** `ocm-character` still knows only
  `DesktopBounds` (its output's logical frame). Multi-output movement is
  a future phase (§9); the environment is ready for it, the character
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

Unit test coverage (`ocm-desktop/src/layout.rs`): single-output
roundtrip exactness, negative positions, side-by-side edge split,
vertical stacking, gaps (containment refuses, conversion stays
arithmetic, nearest picks deterministically), L-shaped arrangements
(no bounding-box assumption), per-output scales, geometry-less outputs
excluded from spatial math, corner clamping.

## 4. Outputs and work areas

Per output, the neutral model carries (all optional):

- stable id (`"eDP-1"` — the same name on the Wayland side and in the
  Niri IPC; **[Verified — Niri]** both report `eDP-1`),
- logical position/size **[Verified — Niri]** (matches `niri msg
  outputs` in the suite), scale (fractional, e.g. 1.25) **[Verified —
  Niri]**, transform (the wire uses `"Normal"`, `"90"`…,
  `"Flipped180"` — verified on both a host output and a nested output)
  **[Verified — Niri]**,
- physical mode (resolution + refresh mHz) **[Verified — Niri]**,
- work area — **[Unavailable on Niri]**: niri's IPC exposes neither
  reserved areas nor a primary-output concept. Consumers must treat the
  full logical output as usable, which is what the runtime does.
- primary — **[Unavailable on Niri]**.

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

The neutral `WindowInfo` therefore carries: id, title, app id,
workspace id, a `WindowState` of `Option<bool>` flags (focused/floating/
urgent filled by Niri; minimized/maximized/fullscreen unavailable), and
`geometry: Option<WindowGeometry>` (global logical, fractional, tile
rect including compositor decorations for Niri).

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

| Capability | Status | Evidence |
|---|---|---|
| Neutral environment model (outputs/windows/workspaces/events/capabilities) | **Implemented** | `ocm-desktop`, 21 unit tests |
| Region-based desktop layout (multi-output, gaps, negatives, scales) | **Implemented** | `DesktopLayout`, 10 unit tests |
| Niri provider (IPC socket, event stream, snapshot, re-poll) | **Implemented**, **Verified — Niri** | suite + live tests |
| Output enumeration + logical geometry | **Verified — Niri** | suite: probe == `niri msg outputs` |
| Output scale (fractional) | **Verified — Niri** | 1.25 live; suite |
| Output transform | **Verified — Niri** | host `Normal`, nested `Flipped180` both parsed |
| Physical mode + refresh | **Verified — Niri** | 1920×1080@144.003 live |
| Work areas | **Unavailable (Niri)** / **Implemented (model)** | suite asserts `work_area=None`; Hyprland-style providers planned |
| Primary output | **Unavailable (Niri)** | no such concept in niri IPC |
| Output change events | **Unavailable (Niri)** / **Implemented (model + re-poll)** | niri-ipc source + live: no output events |
| Window enumeration, identity, workspace, focus | **Verified — Niri** | suite + live |
| Window geometry | **Partial — Verified — Niri**: floating windows only, pixel-accurate | suite pixel check; niri source |
| Minimized/maximized/fullscreen | **Unavailable (Niri)** / **Implemented (model)** | niri-ipc source: no fields |
| Window focus/layout/urgency events | **Verified — Niri** (focus, layouts; urgency not represented in the neutral model yet — **Planned**) | suite |
| Workspace events (activation, active window) | **Verified — Niri** | suite |
| Provider capability reporting | **Implemented** | `niri_capabilities()`, asserted by tests |
| Graceful degradation without the compositor API | **Verified — Niri** (dead socket) | suite |
| Hyprland provider | **Planned** (no claims) | blocked upstream since Phase 2 (§4 of compositor-compatibility.md) |
| Window-based character behavior (surfing, collision) | **Planned** (data only) | §5, §9 |

## 9. Multi-output movement (investigated, not implemented)

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

## 10. Performance

All measurements on the dev machine (Niri 26.04, RTX 5050, debug build
unless noted):

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

## 11. Limitations

- Niri window geometry: floating windows only (tiled → `None`).
  Position-accurate "walking on windows" for tiled layouts needs either
  an upstream niri change or a helper; nothing is faked meanwhile.
- No minimized/maximized/fullscreen knowledge on Niri.
- No work areas / primary output on Niri (model + runtime plumbing
  ready for providers that have them).
- No output events on Niri → 30 s re-poll latency for output changes.
  (The character's own output changes still arrive instantly via the
  layer-shell configure/fractional-scale events.)
- Workspace urgency is parsed but not represented in the neutral event
  set (planned; `WorkspaceInfo.urgent` exists for snapshots).
- The runtime window-overlap report is logging-only: no behavior reads
  windows yet (per phase scope).
- Multi-output runtime validation remains single-output on the dev
  machine (§9).

## 12. Adding a provider (guide)

1. New crate depending on `ocm-desktop` (+ your transport).
2. Implement `DesktopProvider`: truthful `capabilities()`, blocking
   `snapshot()`, non-blocking `poll_events()` (+ `watch_fd()`) when the
   compositor pushes, and `refresh_outputs()` when its stream misses
   outputs.
3. Translate compositor structures into the neutral model; `None` for
   anything the compositor does not expose — never defaults.
4. Add a calloop source mirroring `NiriSource` (readiness-driven,
   `ProviderLost` + `PostAction::Remove` on connection death — a
   source `Err` kills the whole loop).
5. Tests: wire-format fixtures captured from the real compositor
   (mocked, run everywhere) + live tests that skip without the
   compositor socket, clearly separated.
6. Document the capability rows in §8 with the same labels, and update
   the runtime wiring if the provider needs construction flags.
