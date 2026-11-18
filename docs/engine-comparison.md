# Engine Comparison: Bevy vs Godot vs raw wgpu

**Document status:** Phase 1 research document.
**Verification date:** 2026-11-18. Versions checked against crates.io on this date:

- Bevy **0.20.1** (published 2026-10-28, `rust-version 1.97.1`, MIT OR Apache-2.0;
  features include `wayland`, `x11`, `raw_vulkan_init`) — [Verified, crates.io API]
- Rust bindings for Godot **4**: crate `godot` **0.5.5** (published 2026-08-09,
  MPL-2.0, supports Godot API levels 4.2–4.7 via feature flags) — [Verified,
  crates.io API]
- wgpu **30.0.2** (the pinned PoC renderer) — [Verified, local registry source]

Labels: **[Verified]** confirmed in cited sources; **[Assumption]** plausible,
untested; **[Unresolved]** needs an experiment.

---

## 1. Summary judgment

| Criterion | Raw wgpu (PoC) | Bevy 0.20 (target runtime) | Godot 4.7 (via `godot` crate) |
|---|---|---|---|
| Rust integration | Native | Native (the engine *is* Rust) | Foreign — engine hosts Rust as GDExtension |
| ECS | None needed for PoC | Excellent, archetypal ECS | Scene tree + nodes (not ECS) |
| Rendering | Direct, minimal | wgpu-based (same backend!) | Own renderer (Vulkan/GL, C++) |
| Transparency to compositor | Full control (PoC proves it) | Possible; needs custom surface path [Unresolved] | Not controllable from Rust layer |
| Animation | None in PoC | Built-in (clips, blend, morph) | Mature (AnimationTree, retargeting) |
| glTF/GLB | Via `gltf` crate later | First-class asset support | First-class importer |
| VRM | Community crates | Community crates (bevy_vrm etc.) [Assumption] | Well-established add-ons [Assumption] |
| Physics | None | Rapier via bevy_rapier/avian | Built-in (Godot Physics, optional Jolt) |
| Rapier compatibility | Direct (it's Rust) | First-class integration | None (different stack) |
| Event-driven rendering | Fully manual, fully ours | Reactive run modes exist; external frame-callback pacing is our design | Engine-driven; coarse control |
| Resource consumption | Minimal (~tens of MB) | Moderate (full engine) | Heaviest (separate runtime binary) |
| Cross-platform | Ours to build | Excellent | Excellent (but as *its* app) |
| Licensing | MIT/Apache-2.0 | MIT OR Apache-2.0 | Engine MIT; bindings MPL-2.0 |
| Community ecosystem | Low-level | Large, fast-moving | Large, mature, game-centric |

**Recommendation (as anticipated in the approved plan): Bevy for the long-term
runtime; raw wgpu for the PoC.** Godot is rejected as the *primary
architecture* because it is a separate engine/runtime that would have to host
us, not be embedded by us — details in §3.

## 2. Raw wgpu — the PoC choice

- **Rust integration [Verified]:** wgpu 30.0.2 is a pure Rust facade over
  Vulkan/Metal/DX12/GL via wgpu-hal. No foreign engine, no scripting boundary.
- **Rendering [Verified]:** we control every byte of the frame — required for
  the transparency/click-through/idle proof, which is the whole point of
  Phase 2.
- **What it does not give us [Verified]:** scenes, hierarchies, animation,
  asset loading, culling, materials — everything a character runtime needs
  eventually. Hand-rolling all of that would be re-creating an engine.
- **Why it is still the right PoC renderer:** the *hard* unknowns of
  OpenCuteMate are compositor integration (layer-shell, alpha, input regions,
  frame callbacks), not scene management. Those are proven cheapest without an
  engine between us and the protocol. Every finding transfers to Bevy, because
  Bevy renders through the same wgpu (below).

## 3. Godot — rejected as primary architecture

- **Architecture mismatch [Verified, from godot-rust docs/role]:** the `godot`
  crate implements GDExtension — Rust code compiled as a library *loaded by the
  Godot engine binary*. The engine owns the process, the main loop, the
  windowing and the renderer. OpenCuteMate requires the opposite: our own
  process, our own Wayland layer-shell surface, our own lifetime and
  scheduling. Making Godot create a `zwlr_layer_surface_v1` would require
  modifying the engine's display server layer (C++), not just "using" it.
- **Wayland specifics [Assumption]:** Godot targets xdg-shell (and, on X11,
  XWayland) through its own windowing code; there is no supported layer-shell
  path. A desktop-pet overlay with click-through regions is not expressible
  without engine patches.
- **What Godot is genuinely good at [Verified, upstream features]:** a mature
  editor, excellent 3D/2D animation tooling, skeleton retargeting, built-in
  physics, glTF/VRM import pipelines, audio, UI — far ahead of Bevy for
  character content *authoring*.
- **Licensing [Verified]:** the engine is MIT; the Rust bindings are MPL-2.0
  (file-level copyleft). Embedding a MPL-2.0 extension in a MIT/Apache-2.0
  product is workable but adds a compliance surface Bevy does not.
- **Residual idea (not adopted):** use Godot *offline* as a content pipeline
  (export characters to glTF). Nothing in the approved architecture prevents
  that later; it is out of scope for now.

## 4. Bevy — the target runtime (Phase 2b+)

- **Rust integration [Verified]:** Bevy is a Rust-native engine; apps are Rust
  binaries with engine crates linked in — the embedding direction we need.
- **Same GPU substrate [Verified]:** Bevy's renderer is built on wgpu. The
  surface-creation, alpha-mode and present-mode knowledge from the PoC
  transfers 1:1. The `NativeHandles` bridge (docs/architecture.md §4) is
  exactly the seam a Bevy integration would use.
- **ECS [Verified]:** archetypal ECS with schedules, systems, states — a
  desktop pet's behaviors (idle/walk/drag/react) map naturally onto state
  machines driving animation components.
- **Animation [Verified]:** keyframe clips, blend trees (bevy_animation),
  morph targets, glTF animation import (`gltf_animation` feature,
  [Verified] in Bevy 0.20.1 feature list).
- **glTF/GLB [Verified]:** first-class (`bevy_gltf` feature).
- **VRM [Assumption]:** VRM is glTF 2.0 with extensions; community crates
  (e.g. bevy_vrm line) exist and are usable but not audited here. A VRM
  extension layer in our own code is explicitly reserved for later (the
  approved plan defers VRM).
- **Physics [Verified]:** Rapier is Rust-native and integrates via
  `bevy_rapier` / `avian` physics layers — the approved plan already names
  Rapier for future phases. A character pet needs light physics (collide with
  screen edges, "falling" states); Rapier fits without new foreign
  dependencies.
- **Event-driven rendering [Verified + Unresolved]:** Bevy 0.20 ships reactive
  winit modes (render only on changes, `WinitSettings::Reactive*`), which
  aligns with our idle-park requirement. **However** driving Bevy's render
  schedule from *our* `wl_surface.frame` callbacks, on a surface we created
  ourselves (not winit's xdg-shell window), is exactly the open integration
  question of Phase 2b. The PoC's scheduler design (architecture.md §8) is
  written so it can gate a Bevy app instead.
- **Resource consumption [Assumption]:** a Bevy pet app plausibly idles at
  moderate RAM (~100–200 MB with 3D + assets) but zero CPU when parked;
  measured only after Phase 2b.
- **Cross-platform [Verified]:** Windows/macOS/Linux first-class; our platform
  traits mirror that structure.
- **Licensing [Verified]:** MIT OR Apache-2.0 — same as our crates, no
  compliance friction.
- **Ecosystem [Verified]:** large crate ecosystem, rapid release cadence
  (0.20.1 is the 67th release), strong community. The cost is churn: minor
  versions regularly break APIs; pinning and slow upgrades are required
  discipline for an app like ours.

## 5. Decision records

1. **PoC = raw wgpu** [Approved plan, reaffirmed here]: cheapest way to prove
   the Wayland/alpha/input/scheduler unknowns; no engine churn during the
   highest-risk phase.
2. **Target runtime = Bevy** [Approved plan]: Rust-native, wgpu-based, ECS
   fits character behaviors, Rapier path, permissive license, strong glTF
   support. Deferred until the PoC succeeds — deliberately, per the approved
   sequencing.
3. **Godot = not the architecture**; at most a future offline content tool.
4. **No engine dependency enters the workspace in Phase 2.** The PoC crates
   (`ocm-platform`, `ocm-renderer`) are the contract Bevy will implement
   against in Phase 2b.

## 6. Phase 2 results relevant to this decision

(Filled after validation: confirm that the wgpu-direct PoC validated the
surface/alpha/input/scheduler unknowns that this decision rests on.)
