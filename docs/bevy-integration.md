# Bevy Integration Architecture (Phase 2b)

**Document status:** Phase 2b research document, written before implementation.
**Verification date:** 2026-11-18. All sources inspected on this date.

Bevy version investigated: **0.20.1** (published 2026-10-28; the version
current at Phase 2b start, per `docs/engine-comparison.md`).

Every claim is labeled:

- **[Verified — source]** — confirmed in the pinned crate sources, cited as
  `crate-0.20.1/src/file.rs[:line]` under `~/.cargo/registry/src/`.
- **[Verified — probe]** — confirmed by compiling a scratch project that
  composes the exact types/plugins/feature set the integration will use
  (no winit, no X11). The probe is not part of this repository.
- **[Assumption]** — technically plausible, not yet measured.
- **[Unresolved]** — requires the implementation experiment; open question.

---

## 1. Executive summary

Bevy 0.20.1's renderer is **windowing-backend-agnostic at exactly the seam
OpenCuteMate already has**: any main-world entity carrying the data
component `bevy_window::Window` plus a
`bevy_window::RawHandleWrapper` (raw-window-handle enums — the same
abstraction `ocm_platform::NativeHandles` provides) gets a wgpu surface
created from those handles, on the main thread, via the same
`create_surface_unsafe` call our wgpu renderer uses.
[Verified — source: `bevy_render-0.20.1/src/view/window/mod.rs:119-155`
(`extract_windows` requires `(RenderEntity, &Window, &RawHandleWrapper,
Has<PrimaryWindow>)`) and `mod.rs:368-470` (`create_surfaces` →
`SurfaceTargetUnsafe::RawHandle` → `create_surface_unsafe`).]

`bevy_winit` — the window *backend* — is therefore optional. Nothing in
`bevy_render` depends on it; `bevy_winit` is "only" the winit event-loop
runner, window creation, input translation, cursors, accessibility and
monitor enumeration. [Verified — source: `bevy_winit-0.20.1/src/` is
self-contained (≈4100 lines) and no `bevy_render` source references it.]

**Recommendation: Approach B — drive Bevy's existing render integration
against OpenCuteMate's existing Wayland surface.** Keep
`ocm-platform-wayland` as the sole owner of the connection, layer shell,
input and frame callbacks; give Bevy a `Window` + `RawHandleWrapper`
entity describing our surface; call `app.update()` exactly when our
scheduler decides a frame should happen. No second window, no
intermediate render target, no engine-owned loop.

Comparison and justification in §5; the concrete design in §6.

---

## 2. Dependency situation

| Fact | Status |
|---|---|
| Bevy 0.20.1 renders through **wgpu 30.0.2** — the same version OpenCuteMate pins | [Verified — source: `wgpu-30.0.2` in the fetched dependency graph of `bevy-0.20.1`] |
| Both renderers can coexist in one binary (single wgpu copy, no symbol/version conflict) | [Verified — probe: probe crate depended on `bevy 0.20.1` and `raw-window-handle 0.6` alongside each other] |
| `raw-window-handle` version used by Bevy 0.20 (`0.6.2`) matches OpenCuteMate's | [Verified — source: registry] |
| Feature `3d` (the default 3D preset) includes `default_platform` → `bevy_winit`, `x11`, `gilrs`, clipboard, `multi_threaded` | [Verified — source: `bevy-0.20.1/Cargo.toml` features `3d`, `default_platform`] |
| Feature **`3d_bevy_render`** provides the 3D stack (bevy_render, bevy_core_pipeline, bevy_pbr, bevy_camera, bevy_material, meshes) *without* any winit/x11 | [Verified — source: `bevy-0.20.1/Cargo.toml` features `3d_bevy_render`] |
| OpenCuteMate's planned feature set: `3d_bevy_render` (+ `bevy_asset`, `bevy_log`, `bevy_transform`, `bevy_window`, `bevy_tasks` as pulled by the stack) — **no** `bevy_winit`, **no** `wayland` feature (that only feeds winit), **no** `x11`, **no** `multi_threaded` requirement | [Verified — probe compiled with `bevy = { default-features = false, features = ["bevy_render", "bevy_pbr", "bevy_core_pipeline", "bevy_camera", "bevy_log", "bevy_asset"] }`] |

Consequence: **no dependency compromise is required**. The Bevy renderer
enters the workspace as an additional feature-gated backend of
`ocm-renderer`; every other crate stays Bevy-free (see §4.1 of the phase
brief; enforced by `Cargo.toml`).

---

## 3. How Bevy 0.20 renders to a window (verified internals)

The whole path, in execution order:

1. **Main-world window contract.** An entity with
   `bevy_window::Window` (pure data: resolution, `present_mode`,
   `composite_alpha_mode`, …) + `RawHandleWrapper` (+ `PrimaryWindow`).
   [Verified — source: `bevy_render-0.20.1/src/view/window/mod.rs:123`.]

2. **`RawHandleWrapper`** = raw-window-handle enums plus an
   `Arc<dyn Any + Send + Sync>` whose only purpose is to keep the windowing
   object alive while a pipelined renderer holds frames in flight.
   Constructed via `RawHandleWrapper::new(&WindowWrapper<W>)` where
   `W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static`.
   [Verified — source: `bevy_window-0.20.1/src/raw_handle.rs:22-74`.]
   Note `W` must be `Send + Sync`: winit 0.30's `Window` is documented
   `Send + Sync` (`winit-0.30.13/src/window.rs:23-27`), and both winit
   (`SendSyncRawWindowHandle`, `window.rs:174-176`) and Bevy
   (`RawHandleWrapper` itself, `raw_handle.rs:122-129`) use documented
   `unsafe impl Send/Sync` over raw handles — the established ecosystem
   pattern for exactly this type.

3. **Extraction.** `extract_windows` (in `ExtractSchedule`, before camera
   extraction) copies window data into the render world's `ExtractedWindow`,
   including `alpha_mode`, and removes stale surfaces on window close.
   [Verified — source: `view/window/mod.rs:119-225`.]

4. **Surface creation.** `create_surfaces` (in the `Render` schedule, gated
   by `need_surface_configuration`) creates one wgpu surface per extracted
   window via `create_surface_unsafe(SurfaceTargetUnsafe::RawHandle)`,
   queries capabilities, **picks an sRGB format**
   (`Rgba8UnormSrgb`/`Bgra8UnormSrgb` preferred), maps
   `bevy_window::CompositeAlphaMode` → `wgpu::CompositeAlphaMode`
   verbatim (no capability fallback on alpha modes — the requested mode is
   trusted), configures with `desired_maximum_frame_latency` defaulting to
   2, and reconfigures on size/present-mode change.
   [Verified — source: `view/window/mod.rs:368-470`, latency constant at
   `mod.rs:365`.]

5. **Per-frame.** `prepare_windows` (in `RenderSystems::PrepareViews`)
   acquires the swapchain texture (`get_current_texture`; handles
   `Outdated` by reconfiguring, ignores `Occluded`, logs and continues on
   other errors). A camera with `CameraOutputMode::Write` targeting the
   window renders into it.
   [Verified — source: `view/window/mod.rs:258-350`.]
   `ExtractedWindow.needs_initial_present` guarantees at least one present
   on Wayland before anything else is expected — the field exists *because*
   of Wayland's mapping rules. [Verified — source: `mod.rs:84-88`.]

6. **Present.** `render_system` runs the `RenderGraph` schedule, submits
   command buffers, then calls `ExtractedWindow::present` for windows whose
   camera view needs a present. [Verified — source:
   `bevy_render-0.20.1/src/renderer/mod.rs:82-138`.]

7. **Frame driving.** `App::update()` → `sub_apps.update()`: main-world
   schedules run, then the render sub-app runs *inline on the calling
   thread* (Extract → Render → present). `PipelinedRenderingPlugin` is the
   *optional* multi-threaded variant.
   [Verified — source: `bevy_app-0.20.1/src/app.rs:158-164`;
   `bevy_render-0.20.1/src/pipelined_rendering.rs:99-140` documents the
   plugin as opt-in.]
   The manual-driving pattern (`app.finish()`, `app.cleanup()`, then one
   `app.update()` per tick) is a supported, documented usage — it is what
   `bevy_app`'s own `ScheduleRunnerPlugin` runner does.
   [Verified — source: `bevy_app-0.20.1/src/schedule_runner.rs:76-85`;
   `App::finish` docs: "can be useful for situations where you want to use
   `App::update`" (`app.rs:259-261`).] [Verified — probe: the exact
   `finish()`/`cleanup()`/`update()` sequence compiles against 0.20.1.]

8. **Renderer creation.** `RenderPlugin { render_creation }`:
   `RenderCreation::Automatic(WgpuSettings)` blocks synchronously until
   the GPU adapter/device/queue exist (native); `WgpuSettings.backends`
   lets us restrict to **Vulkan only**, matching the project constraint and
   the wgpu renderer's `Backends::VULKAN`.
   `RenderCreation::Manual(RenderResources)` exists if we ever want to own
   adapter selection outright.
   [Verified — source: `bevy_render-0.20.1/src/settings.rs:222-310`
   (`create_render` blocks via `bevy_tasks::block_on` on non-wasm);
   `renderer/mod.rs:199-388` (`initialize_renderer`).]
   Pipeline compilation can be forced synchronous
   (`RenderPlugin::synchronous_pipeline_compilation: true`), avoiding
   "pipeline not ready yet" frames. [Verified — source: `lib.rs:160-167`.]

9. **Alpha/blend correctness.** A camera with
   `ClearColorConfig::Custom(Color::NONE)` clears to (0,0,0,0); a material
   with `AlphaMode::Blend` blends straight-alpha fragments over that
   clear, which *produces premultiplied-correct values in the buffer* for
   the surface alpha mode `PreMultiplied`. `AlphaMode::Premultiplied` is
   also available if we author premultiplied content directly.
   [Verified — source for the enum semantics: `bevy_material-0.20.1/
   src/alpha.rs:9-58`; the composited-output claim is [Assumption → to be
   pixel-verified by the existing `analyze_screenshot.py` partial-alpha
   band check, which exists for exactly this].]
   Color parity with the wgpu renderer: `Color::srgb(r,g,b)` +
   `unlit: true` on an sRGB surface encodes back to the same buffer
   values the wgpu renderer writes directly into an UNORM surface —
   near-identical output pixels. [Assumption → pixel-measure during
   validation; the regression suite is color-agnostic (distance from
   magenta), so both pass regardless.]

---

## 4. What `bevy_winit` actually provides (and why we don't need it)

| `bevy_winit` provides | OpenCuteMate equivalent (already validated, 19/19) |
|---|---|
| Event-loop runner (owns the main thread, redraw-request driven) | `ocm-runtime`'s calloop loop + scheduler [Verified — Phase 2] |
| Window creation (xdg-shell via winit) | `ocm-platform-wayland` layer-shell surface [Verified — Phase 2] |
| Pointer/keyboard event translation | `ocm-platform-wayland` wl_seat/wl_pointer dispatch [Verified — Phase 2] |
| Monitor enumeration | `ocm-platform-wayland` wl_output + zxdg_output discovery [Verified — Phase 2] |
| Scale-factor handling | `ocm-platform-wayland` fractional-scale + viewporter [Verified — Phase 2] |
| Frame pacing (redraw requests) | `ocm-runtime` frame-callback scheduler [Verified — Phase 2] |
| RawHandleWrapper production | Our own `WindowWrapper` + `HasWindowHandle`/`HasDisplayHandle` impl [Verified — probe] |

Approach A (below) would duplicate the left column; Approach B reuses the
right column unchanged.

---

## 5. Approach comparison

### Approach A — custom window backend (replace/bypass bevy_winit)

Implement a "winit-shaped" backend over `ocm-platform-wayland`: produce
`Window` entities + `RawHandleWrapper`s from our surface, translate our
`PlatformEvent`s into `bevy_window`/`bevy_input` events, and provide a
runner that pumps our calloop loop instead of winit's.

*What it actually buys:* Bevy-native *input semantics* —
`bevy_input`/`bevy_picking` see pointer events, systems like
`CursorEntered`/`WindowFocused` fire, picking can drive hover/drag.

*Costs:*
- Reimplementation surface ≈ all of `bevy_winit`'s responsibilities
  (~4100 lines upstream, [Verified — source]) minus what we truly need.
- A second event vocabulary (`bevy_input` Pointer events) layered on our
  existing, validated `PlatformEvent` flow — a translation bug class that
  does not exist today.
- Hover/drag/pick logic would migrate into Bevy systems, while hitbox
  input regions must stay in `ocm-runtime` (they are *Wayland input
  regions*, not picking) — splitting one interaction across two worlds.
- **Every Bevy minor release re-aligns this custom backend** (the
  `bevy_winit`↔`bevy_window` contract is not a stable public API;
  `WindowRef`, required-components and event/message shapes churn every
  version). [Assumption based on the 0.11→0.20 history of this interface
  changing; strong precedent.]

### Approach B — custom render integration (recommended)

Keep OpenCuteMate's platform, scheduler and input untouched. Add, inside
`ocm-renderer`, a feature-gated `bevy` backend that:
1. builds the Bevy `App` with the plugin stack from §2/§3 (no winit),
2. spawns `Window + RawHandleWrapper + PrimaryWindow` backed by
   `ocm_platform::NativeHandles`,
3. spawns the PoC scene as entities (camera + character cuboid + shadow),
4. mirrors `SceneState` into components (Transforms, material params)
   using the **existing `scene.rs` placement math** (§6.3),
5. exposes the same constructor/`render`/`resize`/`character_hitbox`
   contract as `WgpuRenderer`,
6. renders one Bevy frame per `app.update()`, called by the existing
   scheduler exactly when it would have called `WgpuRenderer::render`.

*Costs:*
- Bevy's input/picking ecosystem stays unused for now (hover/drag remain
  in `ocm-runtime`). Deliberate: Phase 2b's scope is the render path;
  input already works and is tested. Re-evaluate picking in a later phase
  once there are character-scene interactions that need it.
- Two renderers to keep passing the same suite (that is exactly the
  phase's regression requirement).

### Comparison against the brief's criteria

| Criterion | A (custom backend) | B (render integration) |
|---|---|---|
| API stability | Worst: mirrors bevy_winit's private-facing contract each release | Best: consumes public, documented seams (`RawHandleWrapper`, `App::update`, plugin stack) |
| Complexity | ~a backend's worth of translation code + dual event worlds | A thin backend: scene sync (~100 lines) + plugin composition |
| Performance | Same GPU path; slightly more CPU per event (translation) | Same GPU path; same event path as today |
| Maintenance | Re-verified at every Bevy bump | Plugin-list diff at every Bevy bump |
| Compatibility with future Bevy | Manual re-derivation per release | Mechanical upgrade (public APIs) |
| Ownership/lifetime safety | RawHandleWrapper produced by us (same unsafe class as B) — no advantage | Same unsafe class as our existing `create_surface_unsafe`, same teardown contract §6.5 |
| Second window / offscreen target | None | None — renders directly to our layer-shell surface [Verified — source: surfaces are created *from* window entities; nothing else can exist] |

**Decision: Approach B.** It is the strictly smaller, more stable surface,
and it matches the phase brief's "Bevy must remain an implementation
detail of the rendering layer".

---

## 6. Design of the Bevy backend (Approach B)

### 6.1 Renderer abstraction evolution

```text
crates/ocm-renderer/
├── Cargo.toml         features: default = ["wgpu-backend"], optional "bevy-backend"
├── src/
│   ├── lib.rs         shared contract: RenderOpts, SceneState, Hitbox,
│   │                  FrameOutcome, RendererError, Renderer trait
│   ├── traits.rs      the Renderer trait (constructor + resize + render +
│   │                  character_hitbox) — one page, no backend types in it
│   ├── math.rs        (unchanged, shared by both backends)
│   ├── scene.rs       (unchanged, shared placement/camera/hitbox math)
│   ├── wgpu/          the validated wgpu renderer, moved as-is
│   │   ├── mod.rs     WgpuRenderer: Renderer impl (code unchanged)
│   │   └── cube.wgsl
│   └── bevy/          the new backend
│       ├── mod.rs     BevyRenderer: Renderer impl; app construction,
│       │              window entity, plugin stack, teardown
│       ├── scene.rs   scene spawn + per-frame SceneState sync (entities,
│       │              transforms, materials)
│       └── handles.rs WindowWrapper/Has* glue over NativeHandles
```

Both backends implement:

```rust
pub trait Renderer {
    fn new(handles: &NativeHandles, logical: (u32, u32), buffer: (u32, u32),
           opts: &RenderOpts) -> Result<Self, RendererError> where Self: Sized;
    fn resize(&mut self, logical: (u32, u32), buffer: (u32, u32));
    fn render(&mut self, scene: &SceneState) -> Result<FrameOutcome, RendererError>;
    fn character_hitbox(&self, scene: &SceneState) -> Hitbox;
}
```

`ocm-runtime` selects the backend via CLI/config (`--renderer wgpu|bevy`,
default `wgpu`); the platform/scheduler/input code paths are shared and
untouched. [Design; compile-time shape verified by probe for the Bevy side.]

### 6.2 Ownership and teardown

- The Bevy `App` (its `RenderDevice`/`RenderQueue`, pipeline cache, and the
  wgpu `Surface` created from our handles) lives inside `BevyRenderer`,
  dropped exactly where `WgpuRenderer` is dropped today: before the
  Wayland session. The existing teardown order in `ocm-runtime::main`
  (renderer → event loop → overlay handle) already satisfies Bevy's
  "handles must remain valid until after the returned `Surface` is
  dropped" contract. [Verified — source: the same contract text as our
  wgpu renderer, `bevy_render`'s SAFETY comment at
  `view/window/mod.rs:389-395`.]
- Pipelined rendering stays **off** (default), so the render world runs on
  the main thread inside `app.update()`; no render thread can hold our
  handles across the teardown boundary.
- `RawHandleWrapper` keeps an `Arc` alive precisely to make the pipelined
  case safe (`raw_handle.rs:46-62`); with pipelining off, our existing
  single-threaded lifetime rules suffice. [Verified — source + reasoning.]

### 6.3 Scene parity (reusing existing math)

- Camera entity: `Transform::from_xyz(EYE)` + `looking_at(TARGET)`,
  `PerspectiveProjection { fov: FOV_Y, near: NEAR, far: FAR }` — the exact
  constants from `ocm-renderer::scene`. [Verified — probe: `Camera3d`,
  `Transform::looking_at` compile; constants are ours.]
- Character entity: per frame, `Transform::from_translation(center + bob)`
  `* rotation(yaw) * scale(world_h)` from `scene::Placement` — the same
  anchor-plane math, so the character renders at exactly the world point
  that maps to `(u·W, v·H)` logical pixels and `character_hitbox()` stays
  correct for input regions. [Verified — probe: composition compiles;
  math is shared code.]
- Shadow entity: same billboard placement (`center − up·0.72·h`,
  camera-facing, axes `0.95h × 0.30h`), a flat semi-transparent unlit
  material (`AlphaMode::Blend`, base color rgba(0.03, 0.03, 0.06, 0.42)).
  The existing pixel analysis's partial-alpha band check passes with a
  uniform-alpha quad (darkened magenta ≈ 148,148,148-band → inside the
  accepted range). [Assumption → pixel-verified during validation.]
- Hover: brightened base color swap on the character material.
  [Implementation detail; the suite checks the runtime log line, which is
  renderer-independent.]
- Transparent background: `Camera::clear_color =
  ClearColorConfig::Custom(Color::NONE)` + surface
  `CompositeAlphaMode::PreMultiplied`. [Verified — probe compiles the
  exact camera setup.]

### 6.4 Scheduling contract (zero rendering while idle)

The scheduler in `ocm-runtime` remains authoritative and unchanged:

- `RenderMode::Idle` — we never call `app.update()`; the thread parks in
  `dispatch(None)`; Bevy runs no systems. Frame callbacks are not armed.
- Event-driven — dirty → `overlay.request_frame()` → `app.update()` →
  Bevy extracts, renders, presents. The `wl_surface.frame` request is
  issued by *our* platform before the update, so it latches onto the
  present-commit Bevy performs inside the update — identical ordering to
  the wgpu path. [Verified — source: present happens inside
  `app.update()` (`renderer/mod.rs:82-138`); ordering reasoning is the
  same already-validated Phase 2 argument.]
- Animated — frame callback → `app.update()`, subject to the FPS cap
  timer, as today.

Bevy-side CPU maintenance (asset/tick/pipeline work) only happens inside
`app.update()`, i.e. only when a frame is being produced. Between updates
Bevy contributes background **threads** (task pools) but no wakeups are
expected from them (std thread parks). [Assumption → measure idle CPU in
the regression run; the suite's 0-tick check will falsify it if wrong.]

### 6.5 Unsafe inventory (Bevy backend)

| Site | Kind | Safety argument |
|---|---|---|
| `handles.rs`: `HasWindowHandle`/`HasDisplayHandle` impls | `unsafe` (`WindowHandle::borrow_raw`) | The handles are produced from live Wayland objects owned by `Shared`; valid until the renderer drops (teardown order). Same contract as the wgpu backend. |
| `handles.rs`: `unsafe impl Send/Sync` for the handle wrapper | unsafe impl | Required by `WindowWrapper<W: Send + Sync>` (`raw_handle.rs:28`). The pointers are only copied into wgpu and dereferenced by Vulkan WSI while the owners are alive; non-pipelined rendering keeps all use on the main thread. Identical in kind to Bevy's own `RawHandleWrapper` and winit's `SendSyncRawWindowHandle` unsafe impls (§3 item 2). |
| Bevy-internal: `create_surface_unsafe` | upstream unsafe | Bevy's documented SAFETY contract; satisfied by our teardown order (§6.2). |

No other unsafe enters the workspace through this design. Both unsafe
blocks we add are in one file with full SAFETY comments, extending the
existing §6 lifetime contract (docs/architecture.md).

---

## 7. Risks / open questions

| # | Question | Status |
|---|---|---|
| 1 | Does a Bevy `App` with our plugin stack actually initialize (asset server, task pools, pipeline cache) without any winit-era assumptions? | [Unresolved → first implementation milestone; compile probe passed] |
| 2 | First-frame behavior: pipeline compilation may delay the first rendered frame even with `synchronous_pipeline_compilation` (compilation happens when the mesh material is first queued, inside `update()`). Mitigation: the runtime's startup already renders at least one frame immediately and the suite waits for "rendering parked" — the park only happens after a successful present; if the first update presents nothing, the app would park unrendered. Countermeasure: drive `update()` until the frame presented before parking (bounded loop). | [Unresolved → implementation detail; observable and testable via the existing suite's park check] |
| 3 | Idle CPU: task-pool threads and any background task must not spin. | [Unresolved → measured by the suite's 0-tick idle check] |
| 4 | Task pool requirement: `bevy_asset`'s server usage at plugin-build time (internal shader assets) may require `TaskPoolPlugin`; if absent → panic. Include it (configurable thread counts) unless measurement shows harm. | [Unresolved → implementation] |
| 5 | `Color::srgb` + sRGB surface vs wgpu renderer's UNORM surface: near-identical output; suite is color-agnostic. | [Assumption → pixel-measured] |
| 6 | Bevy trusts the requested `CompositeAlphaMode` without capability fallback (unlike our wgpu renderer, which queries and degrades). On compositors without `PreMultiplied` the configure would fail loudly. Our supported compositors (Niri: `[Opaque, PreMultiplied]`, verified Phase 2) are fine; document as a known Bevy-path limitation. | [Verified — source: `view/window/mod.rs:429-435` maps without checking caps] |
| 7 | Resize/scale handling: our runtime already calls `renderer.resize(logical, buffer)` on Configure/ScaleChanged; the Bevy backend implements it by updating the `Window` component's resolution, which flows through `extract_windows` → `create_surfaces` reconfigure. | [Verified — source for the flow; runtime behavior to be validated by the suite's configure + fractional-scale checks] |
| 8 | Does `get_current_texture` ever block long enough inside `app.update()` to hurt interaction latency (known Bevy caveat when GPU-bound, source comment at `view/window/mod.rs:237-244`)? Our scene is one cuboid; expected negligible. | [Assumption → measure interaction latency] |

---

## 8. Implementation plan (ordered)

1. Refactor `ocm-renderer` into `wgpu/` module + `traits.rs` (pure code
   movement; no behavior change). Run the full suite — must stay 19/19.
2. Add the `bevy` feature + backend skeleton: plugin stack, window entity,
   `handles.rs` glue, `BevyRenderer` with `render`/`resize`/`hitbox`.
   Smoke-test on the live session.
3. Scene parity: camera/character/shadow from shared `scene.rs` math,
   hover tint, drag (runtime drives position as today).
4. Regression: `tests/niri.sh` against `--renderer bevy`; fix until 19/19.
5. Performance comparison (startup, idle CPU/GPU, active CPU, memory,
   frame pacing, latency) — measured, then documented.
6. Docs: update architecture/renderer docs with results.

The wgpu renderer remains the default backend and the regression
reference throughout.
