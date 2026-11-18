# Wayland Feasibility Study

**Document status:** Phase 1 research document.
**Verification date:** 2026-11-18.

Sources verified locally unless noted:

- Core protocol: `/usr/share/wayland/wayland.xml` (Wayland 1.2x release as
  shipped by the distribution).
- `wlr-layer-shell-unstable-v1.xml` **version 5**, bundled in
  `wayland-protocols-wlr-0.3.12` (cargo registry source).
- `fractional-scale-v1.xml` (staging), `viewporter.xml` (stable),
  `xdg-output-unstable-v1.xml` bundled in `wayland-protocols-0.32.13`.
- Crate sources: `wayland-client-0.31.15`, `wayland-backend-0.3.17`,
  `wgpu-30.0.2`, `wgpu-types-30.0.2`, `calloop-0.14.5` (cargo registry).
- Vulkan: `vulkaninfo` on this machine (Vulkan 1.4.363, NVIDIA proprietary
  620.70.06, `VK_KHR_wayland_surface` rev 6).
- Environment: live Niri 26.04 session, `WAYLAND_DISPLAY=wayland-1`.

Labels: **[Verified]** (source cited), **[Assumption]** (plausible, untested),
**[Unresolved]** (needs experiment; the PoC is that experiment).

---

## 1. Layer Shell (`wlr-layer-shell-unstable-v1`)

**Protocol version landscape [Verified]:** XML is v5 (`set_exclusive_edge`
since 5; `on_demand` keyboard interactivity since 4; `set_layer` since 2).
We bind `1..=4` — everything the PoC needs, present in every layer-shell
implementation since ~2019 (wlroots 0.10 era).

### 1.1 Layer selection

[Verified, XML] `zwlr_layer_shell_v1.layer`: background(0) < bottom(1) <
top(2) < overlay(3). Compositors stack within a layer by map order.

- **Overlay** (our default): above all normal windows and `top` panels.
- **Top** (configurable alternative): above windows, below overlay — useful if
  a user's overlay-layer bars should win stacking over the character.

### 1.2 Anchoring, margins, sizing

[Verified, XML]

- `set_anchor(bitfield)`: top(1), bottom(2), left(4), right(8). All four →
  the surface fills the anchor rectangle (the whole output, margins aside).
- `set_size(0, 0)` with all edges anchored: "If you pass 0 for either value,
  the compositor will assign it and inform you of the assignment in the
  configure event." → the compositor assigns the full output size — exactly
  the desktop-canvas model we want. You must anchor opposite edges in
  dimensions you omit, else protocol error (`invalid_size`/`invalid_anchor`).
- `set_margin`: offsets from the anchor point; not used (0 everywhere), but
  supported for future framing.

### 1.3 Exclusive zones

[Verified, XML] `set_exclusive_zone(value)`:

- `> 0` reserves space for the layer (panel use) — meaningless for us: "If the
  surface is not anchored, anchored to only two perpendicular edges … or
  anchored to all edges, a positive value will be treated the same as zero."
- `0`: no space reserved.
- `-1`: the surface **ignores other layers' exclusive zones** — the character
  may overlap bars' reserved areas rather than being inset by them.

**PoC uses `-1`** per the approved plan (overlay above everything, full
output, ignoring reservations).

### 1.4 Keyboard interactivity

[Verified, XML] `keyboard_interactivity`: `none` (default — layer surfaces
receive no keyboard focus), `exclusive` (grabs all keyboard), `on_demand`
(since v4: keyboard when clicked). "Layer surfaces receive pointer, touch,
and tablet events normally. If you do not want to receive them, set the input
region on your surface to an empty region."

**PoC:** `none` by default (`window.keyboard_interactivity = "none"`);
`ondemand` configurable. Right-click-quit uses pointer only, so keyboard
interactivity is not required for any PoC behavior.

### 1.5 Configure cycle

[Verified, XML] `configure(width, height, serial)` is sent after the first
commit and on any later size-affecting change; the client **must**
`ack_configure(serial)` "sometime before the commit request" that responds.
Multiple configures → ack the last. `closed` = the surface will not be shown
again (output destroyed etc.) → quit.

**Consequence for wgpu interop:** the buffer-attaching commit is performed by
wgpu's present (Vulkan WSI attaches the swapchain image and commits). Our
ack is sent in the configure handler *before* the next render/present —
ordering is safe because renders are serialized on our thread. [Verified by
PoC run.]

**Positioning limitation [Verified]:** Layer Shell has no
`set_position(x, y)`. A layer surface is anchored, not placed. The character
is therefore positioned in the rendered viewport (normalized coordinates →
camera transform), and moves without touching the Wayland surface. This is a
deliberate architectural consequence, not a workaround: it also gives us
free sub-pixel movement and animation between pixels of the desktop.

## 2. Transparency and alpha compositing

### 2.1 Buffers

[Verified, wayland.xml] `wl_shm` formats include `argb8888`
(8-bit channels, alpha-capable) and `xrgb8888` (no alpha). For GPU rendering
the relevant path is not wl_shm but **linux-dmabuf / Vulkan WSI**: wgpu's
Vulkan swapchain images are exported as dmabufs and attached; the format
negotiation below carries the alpha capability. [Verified mechanism;
behavior on this NVIDIA driver validated by the PoC pixel tests.]

### 2.2 Opaque region

[Verified, wayland.xml, exact text]

> "The initial value for an opaque region is empty. Setting the pending
> opaque region has copy semantics, and the wl_region object can be destroyed
> immediately. A NULL wl_region causes the pending opaque region to be set to
> empty."

So by default a surface is treated as **entirely non-opaque** — the compositor
must blend it. The opaque region is purely a repaint optimization; marking
transparent content opaque causes artifacts, never the reverse. The PoC
explicitly sets an **empty opaque region** (self-documenting; also correct by
default).

### 2.3 wgpu surface formats

[Verified, wgpu-types 30.0.2 `src/surface.rs`]

- `SurfaceCapabilities::formats` lists supported formats with the adapter;
  `SurfaceConfiguration::format` is our choice. The compositor advertises
  ARGB8888-family formats for the dmabuf path, which surface as
  `Bgra8Unorm`/`Rgba8Unorm` (± Srgb) in wgpu.
- Only `TextureUsages::RENDER_ATTACHMENT` is guaranteed — the only usage we
  need.
- **Selection strategy (implemented):** pick the first listed format that has
  an alpha channel, preferring non-sRGB for direct color math; log the full
  list; hard-error if no alpha-capable format exists (transparency would be
  impossible — fail loudly, never silently).

### 2.4 Alpha modes and premultiplication

[Verified, wgpu-types 30.0.2 `src/surface.rs:113-138`, enum definitions]

- `CompositeAlphaMode::PreMultiplied` — "non-alpha channels are expected to
  already be multiplied by the alpha channel by the application."
- `CompositeAlphaMode::PostMultiplied` — "the compositor will multiply the
  non-alpha channels by the alpha channel during compositing."
- `Opaque` — alpha ignored (useless for us).
- `Inherit` — platform default via native WSI; on Wayland the platform
  default is premultiplied compositing.
- `Auto` — wgpu picks (may resolve to Opaque/Inherit) — avoided; we want
  deterministic behavior.

[Verified, wgpu-types] `SurfaceCapabilities::alpha_modes` "will return at
least one element, `Opaque` or `Inherit`" — so a premultiplied-capable mode
is *likely* but **must be queried, never assumed**: Vulkan's
`VkSurfaceCapabilitiesKHR::supportedCompositeAlpha` decides, and drivers
differ. The PoC queries and logs the actual set, then picks
`PreMultiplied → PostMultiplied → Inherit` in that order and adapts the
shader (premultiplied output iff the chosen mode expects it). **No alpha mode
is hardcoded.**

[Verified, protocol convention] Wayland compositors composite client buffers
with premultiplied alpha (the ARGB8888 convention; `Inherit` resolves to it).
A correct premultiplied clear is `(0,0,0,0)`; unpremultiplied black `(0,0,0,0)`
is the same value, so the clear is safe under both conventions — but
*partial* alpha content differs, hence the mode-driven shader switch above.

### 2.5 Validating real transparency (not just a clear color)

The PoC scene guarantees all three pixel classes in one frame:
fully transparent (background, desktop visible), partially transparent
(radial shadow quad under the character), fully opaque (lit cube faces).
The automated test samples pixels against a known compositor background
color (see `tests/niri.sh`) — a transparent *clear color* alone would fail
the "desktop visible" check. [Validated in Phase 2.]

## 3. GPU surface creation (wgpu 30 on Wayland)

### 3.1 The API path

[Verified, wgpu-30.0.2 `src/api/instance.rs:271`, `src/api/surface.rs:383-403`]

```rust
let surface = unsafe {
    instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(raw_display_handle),  // Option<RawDisplayHandle>
        raw_window_handle,                             // RawWindowHandle
    })
}?;  // Result<Surface<'window>, CreateSurfaceError>
```

- `Instance::new(desc: InstanceDescriptor)` takes the descriptor **by value**
  [Verified, `src/api/instance.rs:63`]. `InstanceDescriptor` fields:
  `backends`, `flags`, `memory_budget_thresholds`, `backend_options`,
  `display: Option<Box<dyn HasDisplayHandle>>` — the docs state for `display`:
  "On Vulkan, Metal and Dx12, this is currently unused" [Verified,
  wgpu-types `src/instance.rs`], so the PoC passes none and supplies handles
  per-surface.
- There is **no** `Surface::from_raw_window_handle` in wgpu 30 (older docs
  online refer to winit-era APIs; nothing of that name exists in the pinned
  source — [Verified by grep over wgpu-30.0.2]).
- `request_adapter` returns `Result<Adapter, RequestAdapterError>` (async;
  we block with `pollster`) [Verified, `src/api/instance.rs:167`].
- `get_current_texture()` returns the enum `CurrentSurfaceTexture`:
  `Success | Suboptimal | Timeout | Occluded | Outdated | Lost | Validation`
  — *not* a `Result` [Verified, `src/api/surface_texture.rs:48`]. The PoC
  matches on it; `Timeout`/`Occluded` skip the frame, `Outdated` reconfigures,
  `Lost`/`Validation` are errors.

### 3.2 Wayland raw handles

[Verified, raw-window-handle 0.6]

- Display: `RawDisplayHandle::Wayland(WaylandDisplayHandle::new(NonNull<c_void>))`
  — payload is the `wl_display*`.
- Window: `RawWindowHandle::Wayland(WaylandWindowHandle::new(NonNull<c_void>))`
  — payload is the `wl_surface*`.

[Verified, wayland-client 0.31.15 with the `system` feature (uses
libwayland)] The pointers are obtainable in safe-ish API terms:

- `Connection::backend().display_ptr() -> *mut wl_display` (sys backend)
  [Verified, wayland-backend 0.3.17].
- `WlSurface::id().as_ptr() -> *mut wl_proxy` [Verified — compiles and runs
  in this configuration].

Under the hood wgpu-hal creates a `VkWaylandSurfaceKHR` via
`vkCreateWaylandSurfaceKHR` with exactly these two pointers [Verified:
`VK_KHR_wayland_surface` present in `vulkaninfo` instance extensions].

### 3.3 Lifetime requirements (the unsafe contract)

[Verified, wgpu-30.0.2 `SurfaceTargetUnsafe` safety docs]

> "raw_window_handle & raw_display_handle must be valid objects to create a
> surface upon … must remain valid until after the returned Surface is
> dropped."

Guarantees implemented by the PoC (see architecture.md §6): the `wl_surface`
and `Connection` live in a shared state object outliving the renderer; the
renderer is dropped explicitly *before* the platform/session teardown; all on
one thread. The single `unsafe` call site documents these invariants.

### 3.4 Present and commits

[Verified mechanism] wgpu present on Wayland/Vulkan: the WSI queues the
swapchain image, attaches it to the `wl_surface`, sets damage and commits —
all inside the driver's presentation code. Consequence: **every wgpu present
is a `wl_surface.commit`**, which is what applies our double-buffered state
(input regions, viewport, buffer scale) at exactly the right times.

## 4. Input

### 4.1 Pointer events

[Verified, wayland.xml] `wl_seat` (global) → `wl_pointer` via `get_pointer`.
Events: `enter` (serial, surface, surface_x/y), `leave`, `motion`
(time, surface_x/y), `button` (serial, time, button, state), `axis*`, and
`frame` (v5) which groups a logical batch ("A client is expected to
accumulate the data in all events within the frame before proceeding").
Coordinates are **surface-local**; with a viewport in use, surface-local =
the destination (logical) coordinate space [Verified, viewporter.xml mapping
semantics].

The PoC handles enter/leave/motion/button; `frame` ends the batch (we act
per-batch in practice because our reactions are idempotent).

Button codes [Verified, Linux input-event-codes, well-known constants]:
`BTN_LEFT 0x110`, `BTN_RIGHT 0x111`, `BTN_MIDDLE 0x112`; others mapped to
`Other(code)`.

### 4.2 Input regions and click-through

[Verified, wayland.xml, exact text]

> "The initial value for an input region is infinite. … A NULL wl_region
> causes the input region to be set to infinite."
> "Input events happening outside of this region will try the next surface in
> the server surface stack."

- Click-through **enabled** (default): input region = the character hitbox
  rectangle. Everything outside passes through to whatever is below —
  compositor-enforced, zero client-side filtering. (Fully empty input region
  would make even the character click-through; used by `set_input_region(Some(&[]))`
  if ever needed.)
- Click-through **disabled**: `set_input_region(None)` → infinite region →
  whole surface interactive.
- Copy semantics: "Setting the pending input region has copy semantics, and
  the wl_region object can be destroyed immediately" — we still destroy
  regions after use, cheap either way.
- Double-buffered: takes effect at the next commit. The PoC commits
  (and flushes) immediately after each region update so the hitbox can never
  lag behind a drag (a bare commit without a new buffer is legal; the current
  buffer stays attached).

### 4.3 Dynamic updates while dragging

[Verified design, validated in PoC] On every drag motion: update character
position → recompute hitbox (renderer's projection math, exposed as
`character_hitbox()`) → `set_input_region(Some(&[rect]))` + commit + flush.
The compositor then delivers subsequent motion/button events against the new
region; if the pointer leaves the rect mid-drag it would get `leave` — but
the pointer is inside the rect by construction while dragging.

## 5. Frame scheduling

### 5.1 `wl_surface.frame`

[Verified, wayland.xml] "Request a notification when it is a good time to
start drawing a new frame … The frame request will take effect on the next
`wl_surface.commit`." The callback carries a timestamp (ms).

The PoC's per-render sequence:

```text
render (CPU+GPU) → present (WSI attaches buffer + commits)
  → request frame callback (latches onto *next* commit)
  → callback fires (compositor processed the new frame / next repaint)
  → decide: render again (dirty/animated) or park (idle)
```

Requesting the callback *after* present means it is delivered after the
compositor has consumed the presented buffer — the correct pacing signal.
[Verified empirically in the PoC: callback cadence tracks the compositor's
output refresh.]

### 5.2 Interaction with wgpu presentation

- Present mode `Fifo` (default, vsync on): the Vulkan WSI paces internally
  against the compositor's frame clock; combined with our callback-driven
  schedule this yields at most one render queued ahead.
- `AutoNoVsync` when `render.vsync = false` — [Verified, wgpu-types]:
  "gracefully [does] a designed set of fallbacks if their primary modes are
  unsupported" (tries Immediate/Mailbox, falls back to Fifo).
- `desired_maximum_frame_latency = 2` [Verified field, wgpu-types] — bounded
  frames in flight; the docs' recommended default.
- **Idle:** no callback armed → no `get_current_texture()` → no GPU
  submissions of any kind (architectural, see architecture.md §8);
  measured, not assumed, in Phase 2.

### 5.3 FPS cap

Callback cadence is capped additionally in the app: if a callback (or
interaction) arrives sooner than `1000/fps_limit` after the last present, a
one-shot calloop timer defers the render. The cap composes with Fifo
(min of the two rates).

## 6. Display handling

Discovery only — output *configuration* (modes, scale changes) is the
compositor's/user's business, not ours.

### 6.1 `wl_output`

[Verified, wayland.xml] Events: `geometry`, `mode`, `done`, `scale` (v2+),
`name`/`description` (v4). Bind 1..=4; use `scale` as the integer fallback
when fractional-scale is unsupported.

### 6.2 `zxdg_output_manager_v1` / `zxdg_output_v1`

[Verified, XML] Adds `logical_position`, `logical_size`, `name`,
`description` per output — the authoritative logical geometry for
multi-monitor mapping. Used for: monitor list (`window.output` selection by
name), and understanding where our output's logical rectangle sits. This is
**not** output configuration (no requests that change anything; the manager
only creates `zxdg_output_v1` descriptors).

### 6.3 `wp_fractional_scale_manager_v1` / `wp_fractional_scale_v1`

[Verified, XML] `get_fractional_scale(wl_surface)` → `preferred_scale(scale)`
where "the sent scale is the numerator of a fraction with a denominator of
120" (e.g. 150 = 1.25×). This is the modern (staging) replacement for
non-integer `wl_output.scale`, which the protocol deprecates for this
purpose. The PoC listens and reacts by resizing buffers.

### 6.4 `wp_viewporter` / `wp_viewport`

[Verified, XML] `set_source(x, y, w, h)` (fixed-point, buffer space) and
`set_destination(w, h)` (integers, logical space) crop-and-scale the buffer
onto the surface. Canonical fractional-scaling recipe used by the PoC:

```text
logical size L (from layer-shell configure / xdg-output)
scale S (from preferred_scale/120)
buffer  = ceil(L * S) pixels          ← wgpu SurfaceConfiguration size
viewport: source (0, 0, L*S, L*S)     ← in buffer (fixed) coordinates
          destination (L)             ← logical, matches input regions
```

Integer scales (S = k) instead use `wl_surface.set_buffer_scale(k)` and no
viewport — the classic path. **The dev machine's primary session is exactly
the fractional case** (1080p panel at scale 1.25 → 1536×864 logical
[Verified, `niri msg outputs`]), so this is validated on real hardware, not
synthetically.

### 6.5 Why not xdg-output logical size for the surface size

The layer-shell `configure` already reports the logical size for the anchored
output; xdg-output is used only for discovery/selection. Two sources of truth
would drift on hotplug; the PoC treats `configure` as authoritative for the
surface extent.

## 7. Event loop integration (calloop 0.14 + wayland-client 0.31)

[Verified, calloop-0.14.5 sources] `EventLoop::try_new`,
`LoopHandle::insert_source`, `EventLoop::dispatch(None, data)` (blocks in
epoll), `Timer::from_duration`, `Generic::new(fd, Interest::READ, Mode::Level)`,
`EventSource` trait shape (`Event`, `Metadata`, `Ret`, `Error`,
`process_events`/`register`/`reregister`/`unregister`).

[Verified, wayland-client 0.31.15 sources]

- `Connection::connect_to_env()`, `registry_queue_init::<State>` →
  (GlobalList, EventQueue<State>).
- `GlobalList::bind::<I, State, U>(qh, 1..=v, udata)` — binds at
  `min(range.end, advertised)`; errors if advertised < range.start.
- Dispatching on our thread: `EventQueue::dispatch_pending(&mut state)`;
  reading events: `Connection::prepare_read() -> Option<ReadEventsGuard>` →
  `guard.read()`; `Connection::flush()` to push requests.
- `Connection::roundtrip()` performs the sync **without** dispatching app
  queues — pair it with `dispatch_pending` (or use
  `EventQueue::roundtrip(&mut state)`, which does both) during setup.
- The socket fd for epoll: `Connection::backend().poll_fd() -> BorrowedFd`
  [Verified, wayland-backend 0.3.17 `src/client_api.rs:191`]; the PoC dups it
  into an `OwnedFd` owned by the calloop `Generic` source.

Design (implemented as `WaylandSource`, see architecture.md §7):
readable → flush → guarded read (EAGAIN-tolerant) → `dispatch_pending` →
drain `Vec<PlatformEvent>` → per-event calloop callback. A known limitation
documented in code: a fully-blocked socket flush (`EAGAIN` on `flush`)
retries on next wakeup instead of adding EPOLLOUT interest — our request
traffic is tiny (tens of bytes between reads), so this cannot practically
trigger; noted as a robustness TODO, not a correctness issue for the PoC.

## 8. Compositor risk register (what could break, and the plan)

| Risk | Basis | Mitigation |
|---|---|---|
| NVIDIA WSI advertises only non-premultiplied alpha modes | Driver-specific | Query & adapt at runtime (§2.4); pixel test would catch wrong compositing |
| Layer-shell v4 not advertised by some compositor | Unlikely (all targets ≥ v4) | Bind range checked at startup with a clear error |
| Frame callbacks not delivered while surface has no new commits | Protocol-legal behavior | We arm callbacks after each present; if a compositor never repaints an idle layer, we are already parked — desired |
| Compositor ignores empty input regions | Would violate core protocol | Test explicitly in the click-through suite |
| Fractional-scale absent (Hyprland pre-0.41 era) | Historical | Fallback to wl_output integer scale; logged capability |
| dmabuf import issues on hybrid GPUs | Possible | Single-GPU NVIDIA here; Hyprland test will reveal |

## 9. Phase 2 results

(Filled in after validation runs — see `compositor-compatibility.md` and the
final report: actual capabilities, alpha modes, formats, and measured
behavior.)
