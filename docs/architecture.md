# OpenCuteMate Architecture

**Document status:** Phase 1 research document.
**Verification date:** 2026-11-18.

Every technical claim in this document is labeled:

- **[Verified]** — confirmed against upstream protocol specifications, pinned crate
  source code, or a running implementation (source named inline).
- **[Assumption]** — technically plausible, not yet tested.
- **[Unresolved]** — requires an implementation experiment; explicitly open.

A follow-up section ("Phase 2 results") records what the PoC actually demonstrated.

---

## 1. Goal and constraints

OpenCuteMate places an animated 3D character on the desktop as a compositor
overlay. The architectural bet that Phase 1/2 must prove:

> A native Wayland client using wgpu and `wlr-layer-shell` can render a
> transparent 3D surface that floats above the desktop, receives pointer input
> only where the character is, lets clicks pass through everywhere else, and
> consumes no CPU/GPU while idle — without XWayland, under Niri and Hyprland.

Hard constraints derived from the approved plan:

- Rust, Cargo workspace, wgpu-direct PoC (Bevy later).
- Vulkan where available.
- No Tokio, no unnecessary dependencies.
- Rendering never depends on Wayland types; Wayland never depends on rendering.
- Safe Rust wherever possible; `unsafe` isolated and documented.

## 2. Crate graph

```text
                    ┌────────────────────┐
                    │     ocm-core       │  config (TOML/serde), error types,
                    │                    │  logging init (tracing)
                    └────────▲───────────┘
                             │
        ┌────────────────────┼───────────────────────┐
        │                    │                       │
┌───────┴────────┐   ┌───────┴────────┐      ┌───────┴─────────────┐
│ ocm-platform   │   │ ocm-renderer   │      │ ocm-platform-wayland│
│ (abstraction)  │   │ (wgpu, Vulkan) │      │ (Wayland client)    │
│ traits, events │   │ cube, alpha,   │      │ layer-shell, input, │
│ rwh handles    │   │ frame pacing   │      │ outputs, dispatch   │
└───▲────────▲───┘   └───────▲────────┘      └───────▲─────────────┘
    │        │               │                       │
    │        └───────────────┼───────────────────────│
    │                        │                       │
┌───┴────────────────────────┴───────────────────────┴──────────┐
│                        ocm-runtime                            │
│  CLI (clap), config loading, platform init, renderer init,    │
│  main loop (calloop), scheduler, teardown ordering            │
└───────────────────────────────────────────────────────────────┘
```

Dependency directions (enforced by Cargo manifests, [Verified] by inspection):

- `ocm-runtime` → `ocm-core`, `ocm-platform`, `ocm-platform-wayland`, `ocm-renderer`.
- `ocm-platform-wayland` → `ocm-core`, `ocm-platform` (+ `wayland-client`,
  `wayland-protocols`, `wayland-protocols-wlr`, `calloop`, `raw-window-handle`).
- `ocm-renderer` → `ocm-core`, `ocm-platform` (+ `wgpu`, `raw-window-handle`,
  `bytemuck`, `pollster`).
- `ocm-renderer` does **not** depend on `wayland-*` crates of any kind.
- `ocm-platform-wayland` does **not** depend on `wgpu` or any renderer crate.

## 3. Responsibilities

| Crate | Owns | Never touches |
|---|---|---|
| `ocm-core` | `Config` (TOML schema), `Layer`, `KeyboardInteractivity`, error enums, `tracing` init | Wayland, wgpu |
| `ocm-platform` | `PlatformEvent`, `PointerEvent`, `Rect`, `NativeHandles`, `OverlaySurface`/`Platform` traits, `MonitorInfo` | Any concrete windowing stack |
| `ocm-platform-wayland` | Connection, registry, globals, layer surface, `wl_seat`/`wl_pointer`, `wl_output` + `zxdg_output`, fractional-scale + viewporter, frame callbacks, calloop `EventSource` | wgpu, render math |
| `ocm-renderer` | wgpu instance/device/queue, surface creation from raw handles, pipelines, cube + shadow scene, camera math, hitbox projection | Wayland client types |
| `ocm-runtime` | CLI/config merge, platform+renderer construction, `App` state (character, drag, hover), scheduler, input→scene→input-region wiring, teardown | Protocol details |

## 4. Platform abstraction without leaking Wayland types

The renderer must create a GPU surface from a native surface without seeing
`wayland-client` types. The bridge is `raw-window-handle` 0.6 — the same
abstraction wgpu itself consumes ([Verified]: `wgpu-30.0.2/src/api/surface.rs`
`SurfaceTargetUnsafe::RawHandle` takes `raw_window_handle::RawDisplayHandle` and
`raw_window_handle::RawWindowHandle`).

`ocm-platform` defines:

```rust
pub struct NativeHandles {
    pub display: RawDisplayHandle,   // enum, not a Wayland type
    pub window: RawWindowHandle,     // enum, not a Wayland type
}
```

`ocm-platform-wayland` fills the `Wayland` variants:

- display → `RawDisplayHandle::Wayland(WaylandDisplayHandle::new(NonNull<wl_display>))`
- window  → `RawWindowHandle::Wayland(WaylandWindowHandle::new(NonNull<wl_surface>))`

[Verified] against `raw-window-handle` 0.6: `WaylandDisplayHandle::new` /
`WaylandWindowHandle::new` are the constructors; `NonNull<c_void>` payloads.

The renderer receives `&NativeHandles` and forwards the raw enums to
`wgpu::Instance::create_surface_unsafe`. The renderer therefore never matches
on (or even names) a Wayland variant. On Windows the same struct would carry
`Win32` variants; the renderer code is unchanged.

## 5. Data flow

```text
 compositor (Niri/Hyprland)
    │ wl_display events (configure, pointer, frame callbacks, outputs)
    ▼
 WaylandSource (calloop EventSource over the Wayland socket fd)
    │ drains into: Vec<PlatformEvent>
    ▼
 ocm-runtime App::on_platform_event(PlatformEvent)
    │  ├─ Pointer ──► hover/drag state ─► character position (normalized)
    │  ├─ Configure/ScaleChanged ─► renderer.resize + input-region recompute
    │  └─ Frame ───► scheduler decision
    ▼
 SceneState (character x/y, hover, animation time)   [pure data, no Wayland]
    ▼
 WgpuRenderer::render(&SceneState)
    │  └─ get_current_texture → clear(0,0,0,0) → shadow → cube → present
    ▼
 Vulkan swapchain image committed as a dmabuf on the wl_surface
    │ (attaches + commits on present; WSI does this internally)
    ▼
 compositor composites the ARGB buffer over the desktop
```

Pointer → scene → input-region feedback loop: when the character moves (drag),
the runtime recomputes the hitbox in **logical surface coordinates** (the
renderer exposes the projection math via `character_hitbox()`) and calls
`OverlaySurface::set_input_region`, which sets a `wl_region` and commits.

## 6. Surface ownership and GPU resource lifetime

Ownership graph (single-threaded; `Rc` interior state):

```text
 main()
  ├─ WaylandPlatform { Connection, GlobalList, EventQueue<DispatchState>, DispatchState }
  │     └─ Rc<RefCell<Shared>>  ◄──────────────┐
  │        (wl_surface, layer_surface, pointer,│
  │         output proxies, sizes, scale, ...) │
  ├─ WaylandOverlay { Rc<RefCell<Shared>>, ... }│ (OverlaySurface impl)
  │                                            │
  ├─ WgpuRenderer { wgpu Surface<'static>, Device, Queue, pipelines }
  │     └─ holds *raw pointers* to wl_display/wl_surface by value of
  │        the unsafe-created wgpu Surface — no Rust-level borrow!
  └─ calloop EventLoop { WaylandSource { queue, state } }
```

**The critical lifetime hazard [Verified]:** `create_surface_unsafe` is unsafe
because the created `wgpu::Surface` borrows nothing — the wgpu 30.0.2 source
documents the contract explicitly
(`wgpu-30.0.2/src/api/surface.rs`, `SurfaceTargetUnsafe`):

> `raw_window_handle` & `raw_display_handle` must remain valid until after the
> returned `Surface` is dropped.

Rust cannot enforce this; we do, by construction:

1. `Shared` (which owns the `wl_surface` and `Connection`) is created **before**
   the renderer and stored in `Rc` handles held by both the overlay and the
   calloop source.
2. The renderer is dropped **first** (`drop(app)` in `main`), releasing the
   Vulkan swapchain while the Wayland connection and surface are still alive.
3. The event loop (and with it the platform) is dropped **last**.

Destruction order, explicitly:

```text
1. WgpuRenderer dropped
   → wgpu Surface dropped (Vulkan swapchain destroyed)
   → Device/Queue dropped (all GPU buffers, pipelines, textures freed)
   → the raw wl_display/wl_surface pointers are no longer dereferenced by wgpu
2. WaylandOverlay dropped (one Rc<Shared> reference released)
3. EventLoop dropped
   → WaylandSource dropped → EventQueue + DispatchState dropped
   → Rc<Shared> hits 0 → proxies destroyed (wl_surface, layer surface,
     pointer, regions, viewport...) → Connection dropped → socket closed
```

Step 1 must precede step 3. `main()` performs this order explicitly and logs
each step, so teardown order is observable in the log. [Assumption → verified
in Phase 2: see teardown log lines.]

## 7. Event loop

**[Verified]** calloop 0.14.5 API (`calloop-0.14.5/src/loop_logic.rs`):
`EventLoop::try_new()`, `handle()`, `insert_source()`, `dispatch(timeout, data)`,
`run(timeout, data, should_continue)`.

The Wayland integration is a custom `calloop::EventSource` ("`WaylandSource`")
[Verified trait shape: `calloop-0.14.5/src/lib.rs:104` — `Event`, `Metadata`,
`Ret`, `Error` associated types; `process_events`, `register`, `reregister`,
`unregister`]:

- Construction: duplicate the Wayland socket fd
  (`Connection::backend().poll_fd().try_clone_to_owned()`,
  [Verified] `wayland-backend-0.3.17/src/client_api.rs:191`) and wrap it in
  `calloop::generic::Generic<OwnedFd, std::io::Error>` with
  `Interest::READ, Mode::Level` ([Verified]
  `calloop-0.14.5/src/sources/generic.rs:186-189`).
- `process_events` on readable:
  1. `Connection::flush()` (best effort — sends pending requests),
  2. `Connection::prepare_read()` → `ReadEventsGuard::read()` ([Verified]
     `wayland-client-0.31.15/src/conn.rs`; read errors are logged and treated
     as spurious wakeups; genuine protocol errors surface in step 3),
  3. `EventQueue::dispatch_pending(&mut DispatchState)` — routes protocol
     events into `Shared` ([Verified]
     `wayland-client-0.31.15/src/event_queue.rs:387`),
  4. drain `Shared::pending` and emit each `PlatformEvent` to the calloop
     callback.
- register/reregister/unregister delegate to the inner `Generic` (standard
  calloop composition pattern, per the `Generic` module docs).

Why not `calloop-wayland-source`: it is not pinned/verified for the
calloop 0.14 + wayland-client 0.31 pair in this environment; the hand-rolled
source is ~80 lines and fully under our control. [Assumption: none — this is a
design decision.]

**Blocking semantics:** `EventLoop::dispatch(None, &mut app)` parks the thread
in epoll with zero sources armed while idle — idle CPU is architectural, not a
polled busy-loop. [Verified → measured in Phase 2.]

Other sources:

- `calloop::timer::Timer` — FPS-cap pacing and delayed renders
  ([Verified] `Timer::from_duration`, `Timer::immediate`,
  `calloop-0.14.5/src/sources/timer.rs:56-63`).
- `calloop::signals::Signals` (feature `signals`) — SIGINT/SIGTERM for clean
  teardown during tests.

## 8. Rendering scheduling

Three states, driven by `wl_surface.frame` callbacks
([Verified] protocol: `wayland.xml` — "request a frame throttling hint… will
take effect on the next wl_surface.commit"):

| State | Trigger | Behavior |
|---|---|---|
| **Idle** | no dirty state, no animation, last frame callback found nothing to do | No frame callback armed. No timer armed. Thread parked in `dispatch(None)`. Zero renders. |
| **Interaction** | pointer enter/leave/motion/button, configure, scale change | Mark dirty → render at next opportunity → arm one frame callback → on callback, render again only if still dirty, else park. |
| **Animated** | `character.animate = true` (config/CLI) | Continuous: render → request frame callback → on callback render next frame, subject to FPS cap (`render.fps_limit`). |

FPS cap: enforced by deferring renders that arrive too soon with a one-shot
`Timer` (`1000/fps_limit` since last present). Present mode is Fifo
(vsync on) by default; `vsync = false` selects `PresentMode::AutoNoVsync`
which wgpu documents as gracefully falling back
([Verified] `wgpu-types-30.0.2/src/surface.rs:28-40`).

Frame-callback ordering: the callback is requested **before** the render
that follows, so it latches onto that render's present-commit (protocol:
"will take effect on the next `wl_surface.commit`"; wgpu's present performs
the commit via the Vulkan WSI). It then fires once the compositor has
consumed that frame. (Requesting after the present would leave the request
pending with no commit to latch onto — a real bug the automated suite
caught: the app could never park without external input.)

Why renders stop naturally in event-driven mode: after the last render we arm
exactly one more frame callback; when it fires with nothing dirty we do not
render and do not arm another. Nothing else can wake the loop. [Verified by
measurement in Phase 2.]

## 9. Wayland surface configuration (layer-shell)

[All Verified against `wlr-layer-shell-unstable-v1.xml`, version 5, bundled in
`wayland-protocols-wlr-0.3.12`]

- Layer: `overlay` (default) or `top`, chosen at `get_layer_surface` time.
- Namespace: `opencutemate`.
- Anchor: all four edges → with `set_size(0, 0)` the compositor assigns the
  full output extent ("If you pass 0 for either value, the compositor will
  assign it and inform you of the assignment in the configure event").
- Exclusive zone: `-1` — per protocol, when the surface is anchored to all
  edges a positive value "will be treated the same as zero"; `-1` additionally
  means the surface wants to ignore *other* layers' exclusive zones.
- Keyboard interactivity: `none` (default; the PoC needs no keyboard) or
  `ondemand` (since layer-shell v4; we bind 1..=4).
- `configure(width, height, serial)` → ack immediately (`ack_configure` must
  precede the commit that follows), store logical size, mark dirty.
- `closed` → quit.

**Positioning:** Layer Shell has no `set_position`. The surface covers the
output; the character is positioned **inside the rendered viewport** via the
camera transform (normalized `character.position_x/y` ∈ [0,1]). Moving the
character therefore never recreates the Wayland surface. [Verified approach,
matching protocol semantics.]

## 10. Transparency

[Verified, wgpu-types 30.0.2 `src/surface.rs`]

- `SurfaceCapabilities::alpha_modes` is queried at runtime; the renderer picks
  by priority `PreMultiplied → PostMultiplied → Inherit`, errors clearly if
  only `Opaque` is available. **No alpha mode is hardcoded.**
- Shader behavior follows the *negotiated* mode: premultiplied output for
  `PreMultiplied`/`Inherit` (Wayland's default compositing convention),
  straight alpha for `PostMultiplied`.
- Surface format: first preferred format from `SurfaceCapabilities::formats`
  that has an alpha channel (Bgra8Unorm/Rgba8Unorm family). sRGB variants are
  deprioritized so shader color math is direct; the choice is logged.
- Opaque region: left **empty** (the protocol default is an empty opaque
  region — [Verified] `wayland.xml`: "The initial value for an opaque region
  is empty"); we also explicitly set an empty region for self-documentation.
- The scene clears to `(0,0,0,0)` and draws a semi-transparent radial shadow
  quad under an opaque cube, so fully transparent, partially transparent and
  opaque pixels all exist in every frame — validated by pixel sampling in
  `tests/niri.sh`.

## 11. Input and click-through

[Verified against `wayland.xml`]

- `wl_seat` → `wl_pointer`: enter/leave/motion/button events with
  surface-local (logical) coordinates; `wl_pointer.frame` (v5) batches events.
- Input region: "The initial value for an input region is infinite. … A NULL
  wl_region causes the input region to be set to infinite." →
  - click-through enabled: region = character hitbox rect only.
  - click-through disabled: `set_input_region(None)` → whole surface
    interactive.
- "Input events happening outside of this region will try the next surface in
  the server surface stack" → clicks on empty areas pass through to windows
  below, by compositor semantics, with no client-side filtering.
- The input region is double-buffered state; `WaylandOverlay::set_input_region`
  commits (and flushes) immediately so drags never outrun their hitbox
  (a bare commit without a new buffer is valid; the previously attached buffer
  remains current).
- Hitbox: computed from the same camera math the renderer uses
  (`WgpuRenderer::character_hitbox()`), converted from NDC to logical pixels.
- Right-click on the character → quit (the PoC's exit affordance).
- Left-drag on the character → move; normalized position clamped to [0,1].

## 12. Display handling

[Verified module paths in `wayland-protocols-0.32.13` (`src/xdg.rs:104`,
`src/wp.rs:153`, `src/wp.rs:544`); protocol XMLs bundled in the crate]

- `wl_output` (bind 1..=4): name/description (v4), scale, mode.
- `zxdg_output_manager_v1`/`zxdg_output_v1`: logical position/size per output —
  used for monitor discovery and selection (`window.output` config); this is
  output *discovery*, not configuration (we never change output settings).
- `wp_fractional_scale_manager_v1` → `wp_fractional_scale_v1.preferred_scale`
  (units of 1/120, [Verified] XML: "The sent scale is the numerator of a
  fraction with a denominator of 120").
- `wp_viewporter` → `wp_viewport` for fractional buffer scaling:
  buffer = ceil(logical × scale), viewport source = logical×scale (fixed),
  destination = logical. Integer scales instead use
  `wl_surface.set_buffer_scale` and no viewport.
- The dev machine's live Niri session runs 1920×1080 @ scale 1.25
  (logical 1536×864) [Verified via `niri msg outputs`], so the fractional path
  is exercised on real hardware, not just in theory.

## 13. Bevy integration (Phase 2b — implemented)

The renderer trait boundary now has two backends behind one contract
(`ocm-renderer::Renderer`), selected at runtime via
`ocm-runtime --renderer wgpu|bevy` (default: wgpu, the regression
reference):

- `ocm-renderer/src/wgpu/` — the Phase 2 renderer, unchanged.
- `ocm-renderer/src/bevy/` — Bevy 0.20.1 driven against the same
  `OverlaySurface`: a `Window` + `RawHandleWrapper` entity built from
  `NativeHandles` (no `bevy_winit`, no second window), scene entities
  placed by the *shared* `scene.rs` math, and one `app.update()` per
  scheduler-decided frame. The scheduler remains authoritative: zero
  updates while parked.
- Design, sources, pitfalls and the measured performance comparison:
  `docs/bevy-integration.md` (§9 has the Phase 2b results).

Both backends pass the full Niri regression suite (19/19 each).

## 14. Future: Windows and macOS

- `ocm-platform-win` would implement `Platform`/`OverlaySurface` over a
  always-on-top, click-through (`WS_EX_LAYERED|WS_EX_TRANSPARENT` +
  `SetWindowRgn` or hit-testing) window; `NativeHandles` carries
  `RawWindowHandle::Win32`. [Assumption — not started.]
- `ocm-platform-macos` would use `NSPanel` with `nonactivatingPanel` +
  `ignoresMouseEvents` toggling; `RawWindowHandle::AppKit`. [Assumption.]
- The renderer and runtime layers are already free of Unix/Wayland
  assumptions except: `calloop` (Unix-only) — a future abstraction or a
  platform loop (e.g. a Win32 message loop implementing the same event
  contract) is required. Noted as the largest cross-platform gap after
  surface creation. [Unresolved by design at this stage.]

## 15. Unsafe code inventory (PoC)

| Location | Unsafe | Why required | Safety argument |
|---|---|---|---|
| `ocm-renderer/wgpu`: `create_surface_unsafe(SurfaceTargetUnsafe::RawHandle { .. })` | 1 call | wgpu offers no safe path for externally-owned raw Wayland handles | Handles are guaranteed valid for the Surface's whole lifetime by the documented teardown order (§6); pointers obtained from live proxies owned by `Shared` |
| `ocm-renderer/bevy/handles.rs`: `HasWindowHandle`/`HasDisplayHandle` impls (`WindowHandle::borrow_raw` / `DisplayHandle::borrow_raw`) | 2 calls | Bevy's `RawHandleWrapper::new` consumes the standard rwh traits over our handles | Same contract as above: the handles come from live Wayland objects owned by `Shared`; valid until the renderer drops (teardown order §6) |
| `ocm-renderer/bevy/handles.rs`: `unsafe impl Send/Sync for NativeSurface` | 2 impls | Bevy's `WindowWrapper<W>` requires `W: Send + Sync + 'static` | The pointers are only copied into wgpu surface creation and dereferenced by the Vulkan WSI while the owning objects are alive; rendering is non-pipelined (all use on the main thread). Same in kind as Bevy's own `RawHandleWrapper` and winit's `SendSyncRawWindowHandle` unsafe impls |
| `ocm-platform-wayland`: reading `wl_display`/`wl_surface` raw pointers from `wayland-client` (`display_ptr()`, `ObjectId::as_ptr()`) | raw pointer → `NonNull` | wgpu needs the raw pointers; `wayland-client` (sys backend) provides them [Verified API] | Pointers are only converted to `NonNull` and copied into the rwh enums; they are dereferenced only by wgpu/Vulkan while the owning proxies are alive (teardown order §6) |

There are **no** `unsafe impl Send/Sync` on platform types (the one pair
above is on the renderer-side handle wrapper, documented in place).

## 16. Phase 2 results

All architecture-level assumptions of this document were validated by the
PoC on Niri 26.04 (2026-11-18), 18/18 automated checks plus live-session
interaction. Highlights (details and evidence: `docs/compositor-compatibility.md` §7):

- The teardown order (§6) is **observable in the logs** of every clean exit:
  renderer → platform/session, and was exercised by the automated
  right-click-quit test and by a human on the live session.
- The scheduler (§8) parks with **0 CPU ticks over 3–5 s and 0 frames
  presented**; interaction renders one frame per event and parks again
  (~14 frames total for a scripted startup+hover+drag+quit session).
- The platform/renderer split held: `ocm-renderer` never imports a Wayland
  crate; `ldd ocm-runtime` shows no X11/XCB linkage.
- One real bug was caught by the automated suite and fixed: the frame
  callback must be requested **before** the commit it latches onto (§8) —
  requesting it after `present()` left it pending forever, so parking
  required external input.
- Fractional scaling (1.25×) is exercised natively by the dev machine and
  verified pixel-exactly through the viewporter path (§12).

## 17. Phase 2b results

The Bevy backend (§13) was implemented against the same contracts and
validated by the same automated suite on the same day:

- **Regression:** nested Niri, 19/19 PASS for `--renderer wgpu` *and*
  `--renderer bevy` (the suite gained the `OCM_RENDERER` variable; both
  runs exit 0). Partial-alpha shadow band: 6132 pixels in both backends
  — the shared placement math keeps hitboxes and screen placement
  identical.
- **Scheduler authority preserved:** Bevy contributes zero CPU ticks
  while parked (procfs over 10 s, both backends); the runtime calls
  `app.update()` only when a frame is due.
- **Pitfalls found and fixed during integration** (each reproduced, then
  verified fixed): no-winit plugin ordering; the phantom
  `WindowPlugin::primary_window`; the `tonemapping_luts` dependency of
  the default tonemapper that silently stopped all presents; the
  first-present warm-up needed because the first frames present before
  the fullscreen-blit pipeline compiles; manual render creation for
  surface-compatible adapter selection. Full narrative:
  `docs/bevy-integration.md` §9.
- **Measured comparison** (`tests/perf.sh`, nested Niri; see
  `docs/bevy-integration.md` §9 for the full table): both backends idle
  at 0 CPU/0 GPU; Bevy costs ~4–6× more CPU per animated frame
  (113 vs 27 ms/s release @60 fps), ~250 MB more memory and ~0.9 s more
  startup, and paces at 18.4 ms mean (release) vs the wgpu renderer's
  17.0 ms.
