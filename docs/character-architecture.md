# Character & Interaction Architecture (Phase 3a)

**Document status:** Phase 3a design + implementation document.
**Verification date:** 2026-11-19. Claims about runtime behavior reference
the automated Niri suite (`tests/niri.sh`) and the `ocm-character` unit
tests unless stated otherwise.

The phase's product is the **first real character** — a cube with a face —
built on foundations that are *not cube-shaped*: semantic model elements, an
explicit state machine, a mechanical interaction system, deterministic
physics, and an animation seam that future glTF clips and VRM expressions
can plug into without touching the interaction or behavior code.

---

## 1. Crate layout and responsibilities

```text
┌──────────────────────────────────────────────────────────────┐
│ ocm-runtime (apps/)                                          │
│  CLI/config · scheduler · sleep timer · teardown order       │
│  input-region policy (grab ↔ element rects, deduplicated)    │
└───────▲──────────────────▲───────────────────▲────────────────┘
        │ PlatformEvent    │ SceneState        │ Renderer
┌───────┴────────┐  ┌──────┴─────────┐  ┌──────┴──────────────┐
│ ocm-platform-   │  │ ocm-renderer   │  │ ocm-platform        │
│ wayland        │  │ wgpu │ bevy    │  │ (neutral traits)    │
│ (unchanged)    │  │ part drawing + │  │                     │
└────────────────┘  │ hitboxes      │  └─────────────────────┘
                    └──────▲─────────┘
                           │ model + AnimatedPose types
                    ┌──────┴─────────────────────────────────┐
                    │ ocm-character  (NEW — pure simulation)  │
                    │  model · state machine · interaction ·  │
                    │  physics · animation · desktop bounds   │
                    └─────────────────────────────────────────┘
```

| Responsibility | Owner | Notes |
|---|---|---|
| Character identity/config/state/transforms/behavior/animation requests | `ocm-character::Character` | The only owner of character state anywhere in the workspace |
| Pointer interaction: hover/click/drag/release, element identification | `ocm-character::interaction` | Consumes `ocm_platform::PointerEvent` (neutral) + renderer hitboxes |
| Model representation: hierarchy, parts, semantic mapping | `ocm-character::model` | The prototype cube is data, not code, in the renderers |
| Gravity/velocity/falling/boundary collision | `ocm-character::physics` | Deterministic, pure; no Rapier |
| Animation: procedural now, clip/expression later | `ocm-character::animation` | Renderer-neutral `AnimatedPose` output |
| Desktop environment abstraction | `ocm-character::desktop` | Output-local bounds for physics; the neutral environment lives in `ocm-desktop` (Phase 3b, §7) |
| Wayland events, Layer Shell, input regions, scheduling, teardown | `ocm-platform-wayland` + `ocm-runtime` | **Unchanged from Phase 2**; input regions stay runtime-owned |

Dependency rules (enforced by manifests):

- `ocm-character` depends only on `ocm-core` (config) and `ocm-platform`
  (the neutral `Rect`/`PointerEvent` types). It never sees a Wayland or
  GPU type.
- `ocm-renderer` depends on `ocm-character` for the *model and pose
  types* only, and renders whatever model it is constructed with.
- Nothing depends on `ocm-platform-wayland` except the runtime; no
  character API leaks Wayland-specific types.

Bevy's role is unchanged from Phase 2b (documented there): a rendering
backend behind the same `Renderer` contract, driven one `app.update()` per
scheduler-decided frame. Character *simulation* deliberately stays in
pure Rust so both backends consume identical behavior and the regression
suite validates it once. Moving simulation into Bevy's ECS would rewrite
validated runtime code without a phase requirement to do so; the seam for
that change (the `SceneState`/`AnimatedPose` contract) is already the
boundary it would happen at.

## 2. Semantic model elements

```rust
pub enum ElementRole { Root, Head, Body, LeftArm, RightArm,
                       LeftLeg, RightLeg, Tail, Custom(String) }
pub struct ElementId(pub u32);   // index into the model's element table
```

A `CharacterModel` is an ordered element tree; each element carries a
role, a name, a parent, a local offset (in character-height units), any
number of visual `VisualPart` boxes, and an optional **hit extent** (the
interactive rectangle, element-local).

Two explicit orderings:

- **Render order** — storage order, parents before children. Both
  renderers paint in this order (painter's algorithm; boxes are convex
  and attached, so no depth buffer is needed).
- **Hit priority** — `hit_priority()` yields interactive elements
  *children before parents*. Hit-testing takes the first rect containing
  the point, so the eyes win over the face, the face over the body.

The prototype cube (`model::prototype_cube()`):

| Element | Role | Contents |
|---|---|---|
| root | Root | anchor only, never interactive |
| body | Body | the coral 1×1×1 cube |
| left_arm / right_arm | LeftArm / RightArm | warm-yellow side boxes |
| face | Head | cream face plate + mouth on the front (+Z) |
| eyes | Custom("eyes") | two dark boxes above the face center |

The front direction is unmistakable (cream face, dark eyes) while the
cube silhouette stays exactly the Phase 2 size, so the pixel-validated
placement and shadow footprint carried over unchanged.

**Future imports:** a GLB/glTF node or VRM bone maps onto `ModelElement`
one-for-one — role from user mapping or naming conventions (`Head`,
`右手`…), parts become real meshes, `hit_extent` from the mesh bounds.
The importer (Phase 4+) produces this same structure; nothing downstream
changes.

## 3. Character state machine

```text
            enter/leave                click
  Idle ◄──────────────► Hover{element} ─────► Interacting{element}
   │  ▲                    │  ▲                  │ (bounded 0.35 s)
   │  │ try_sleep (60 s)   │  │ release          ▼
   ▼  │                    ▼  │            base phase
 Sleeping                Dragging{element}
   │                          │ release (physics, airborne)
   │ pointer contact          ▼
   └──────► Hover          Falling ──land──► base phase
```

- `Phase` lives in the `Character` aggregate; **all** transitions go
  through `handle_interaction` / `update` / `try_sleep` / `set_bounds`.
  No character booleans exist anywhere else in the workspace.
- **Hover is also a separate tint channel** (`hovered_element`): the
  element under the pointer is highlighted even while the behavioral
  phase is `Dragging`/`Falling`/`Interacting`. The phase drives *what
  the character does*; the channel drives *what the pointer touches*.
- Characters do not have to implement every phase: a phase is only
  ever *entered* by its trigger, and an animator maps phases it knows
  to poses while unknown ones degrade to the neutral pose. A future
  character's animation table simply provides different mappings for
  the same states.
- `Sleeping` is entered by a one-shot 60 s idle timer in the runtime
  (`try_sleep()`), refused unless the phase is exactly `Idle`, and left
  by any pointer contact or click. The sleeping pose flattens and dims
  the eyes element via the element-pose slot.

## 4. Interaction event flow

```text
compositor (wl_pointer, inside the input region only)
   │  ocm-platform-wayland (unchanged) → PlatformEvent::Pointer
   ▼
ocm-runtime::on_pointer
   │  hitboxes ← renderer.character_hitboxes(scene)   ← same camera math
   ▼
InteractionSystem::on_pointer(now, event, hitboxes)
   │  InteractionEvent::{HoverChanged, Press, Click,
   │                     DragStart, DragMove, DragEnd}   (ElementId-tagged)
   ▼
Character::handle_interaction(now, event)
   │  phase transitions · drag anchoring · release velocity
   │  → InteractionOutcome { dirty, needs_frames }
   ▼
runtime scheduler → render_now() → SceneState{position, size, hover, pose}
```

Mechanical details the tests pin down:

- **Click vs drag**: a left press promotes to a drag after 4 px of
  pointer travel; a release before that is a `Click` on the pressed
  element. The drag is anchored to the *press-time* grab offset, so the
  same point of the character stays under the pointer.
- **Element-center space**: drag positions are stated in the grabbed
  element's hitbox-rect-center space; the character maps them back to
  its anchor through the model's element offsets (the renderer places
  rect centers exactly at those offsets, so the mapping is exact).
- **Release velocity** is sampled over the last 120 ms of pointer
  motion and clamped (±2 output widths/s horizontally, ±2.5 heights/s
  vertically). A release after the pointer has been still for > 120 ms
  tosses with zero velocity — stale flicks are pruned.
- **Drag reliability**: while dragging, the runtime sets the input
  region to the **whole surface** (a client-side pointer grab), so the
  pointer can never outrun the character no matter how fast it moves.
  The old PoC re-committed the input region on every drag motion; now a
  drag commits it exactly twice (grab, release), with a dedup cache
  preventing identical commits otherwise.
- **Defensive leave**: a `wl_pointer.leave` during a drag (which cannot
  happen with the grab region) keeps the drag alive; a leave during a
  pending press aborts the press instead of dangling.

## 5. Physics

Pure, deterministic, in logical pixels: `physics::Body::step` is a
function of `(position, velocity, dt, bounds)`.

| Constant | Value | Meaning |
|---|---|---|
| Gravity | 3.2 output-heights/s² | top-to-floor fall ≈ 0.8 s |
| Terminal velocity | 4 heights/s | caps every throw |
| Bounce threshold | 0.18 heights/s | slower landings don't hop |
| Restitution | 0.45 walls / 0.28 ground | small, believable bounces |
| Ground deceleration | 2 heights/s² (constant) | quadratic stopping: gentle drops stick, hard tosses skitter |
| Step clamp | 50 ms | a park/stall never tunnels a boundary |

Events out: `Landed`, `BouncedGround`, `HitWall`, `Rest` — the state
machine uses `Landed` to leave `Falling` and `Rest`/settling to stop
scheduling frames. Collision uses the body box only (side elements are a
documented simplification). Identical input sequences produce bit-equal
trajectories (`physics_is_deterministic_for_identical_input_sequences`).

**Scheduling contract:** physics only runs while the body is unsettled
— the scheduler enters continuous mode (frame callbacks at the FPS cap)
when the character reports `needs_frames`, and parks again the frame it
settles. A resting character costs exactly zero physics work.

Physics is config-gated (`[character] physics`, CLI `--physics`,
**default off**): the default keeps the validated Phase 2 behavior
(the character rests where placed/dropped) and the regression suite's
original checks unchanged; the suite's physics section launches a second
instance with `--physics` to validate falling and tossing for real.

## 6. Animation abstraction

The renderer-facing contract is one fully-resolved snapshot per frame:

```rust
struct AnimatedPose {
    root: RootPose,               // offset (camera right/up, char-height
                                 // units), yaw/pitch/roll, squash scale
    elements: Vec<ElementPose>,   // per-element scale + tint slots
}
```

`Animator` (procedural, the prototype's) maps `Phase` + context to a
pose. Every animation is either continuous (`animate` mode, physics) or
**a bounded one-shot** with a reported remaining time:

| Phase | Procedural animation | Frames while it runs |
|---|---|---|
| Idle, `animate` on | bob + slow spin (the exact Phase 2 formula) | continuous |
| Hover | 0.30 s pop (scale up ~5%) | ~18 @ 60 fps |
| Interacting (click) | 0.35 s squash & stretch | ~21 |
| Dragging | scale-up + velocity lean (roll) | pointer-driven only |
| Falling | fixed nose-down pitch + velocity roll | while airborne |
| Sleeping | eyes element flattened/dimmed + slump | one frame |

The hover tint is a channel, not an animation: it costs one render when
it changes and nothing while it persists. An idle character without
`animate` renders *exactly* the neutral pose — the zero-cost idle
invariant is asserted by unit test.

**Integration path for real model animation** (Phase 4+, out of scope
here): a glTF clip player or VRM expression controller consumes the same
`(Phase, context)` inputs, selects clips/expressions, and emits the same
`AnimatedPose`. Two extensions are anticipated and already accounted
for: per-element transforms beyond scale (arms waving) fit the existing
slot; skeletal animation will grow a bone section in `AnimatedPose`
while renderers grow joint-driven hierarchies — the Bevy backend already
stores elements as a real entity hierarchy, so bones map onto it.

## 7. Desktop environment interactions

**Phase 3b update:** the character now has a real desktop environment
behind it — see `docs/desktop-awareness.md` for the full design. The
summary:

- **`ocm-desktop`** (new pure crate) is the neutral environmental
  model: outputs (geometry/scale/transform/physical mode/work area),
  windows (identity, workspace, state flags, optional geometry),
  workspaces, capability reports, environmental events, a state
  accumulator and a region-based coordinate model
  (`DesktopLayout`: multi-output, gaps, negative positions, per-output
  scales — the desktop is never assumed to be one rectangle).
- **`ocm-desktop-niri`** feeds it from the Niri IPC (event stream +
  snapshot + output re-poll). Verified truths: window pixel geometry
  only for floating windows; no minimized/maximized/fullscreen, no work
  areas, no primary output on Niri — all represented as `None`/false,
  never guessed.
- **The character itself is unchanged.** It still lives in
  `DesktopBounds` (its output's local logical frame, from the
  layer-shell configure), which the runtime now reconciles with the
  environment's work area when the compositor reports one (Niri does
  not, so behavior is value-identical). Window/workspace events update
  the environment and never wake the character or the scheduler; the
  runtime logs window-overlap awareness as the integration boundary
  for future behaviors (walking on windows, reacting to windows moving
  underneath — planned, not implemented).
- Multi-output movement remains future work (one surface per output or
  layer-shell re-assignment — see desktop-awareness.md §9).

Historical context (Phase 3a, still true): a plain layer-shell client
has no standard protocol for foreign windows; standard Wayland cannot
provide this. What changed in Phase 3b is the *delivery mechanism*:
Niri's compositor-specific IPC provides honest, verified window and
workspace data to a provider, which is why the awareness lives behind
the neutral `DesktopProvider` contract — compositor-specific code stays
in `ocm-desktop-niri`, the character never sees it. No fake window
detection exists anywhere; every `Option` gap is a documented
compositor limitation.

## 8. Scheduler integration (idle cost)

The Phase 2 scheduler modes are preserved; `Animated` now means
"the character needs frames":

| Situation | Behavior |
|---|---|
| Idle, no reactions, physics settled | parked, 0 wakeups (suite-verified 0 ticks / 0 frames) |
| Pointer enter/leave/click | one-shot renders + bounded reaction windows, then park |
| Drag motion | renders exactly on motion events; a motionless drag parks |
| Falling / sliding | frame callbacks at the FPS cap until settled |
| `--animate` | continuous (unchanged) |
| Sleeping transition | one frame, park; the 60 s timer is a single calloop wakeup |

**Measured (2026-11-19, nested Niri focused on the host, release
build, `tests/perf.sh`)**:

| Metric | wgpu | bevy |
|---|---|---|
| Idle CPU (parked, 10 s) | 0.0 ms/s | 0.0 ms/s |
| Active CPU @ 60 fps (`--animate`, 10 s) | 30 ms/s | 107 ms/s |
| Active GPU (nvidia-smi sm) | ~12% | ~13% |
| Frame pacing mean | 16.8 ms | 18.5 ms |
| RSS while animating | 137 MB | 350 MB |
| Startup → event loop | 0.14 s | 0.32 s |

The active CPU numbers match the Phase 2b engine-floor measurements
(27/113 ms/s) within a few ms/s: the character's seven parts add draw
calls, not systems, and the wgpu path stays essentially at its
hand-rolled floor. Both backends idle at exactly 0.0 ms/s. A scripted
default-config session (startup, hover, click reaction, drag across
the screen, drop, quit) presents ~110 frames total — bounded one-shot
reactions (~18–21 frames each) plus one render per pointer event —
and parks between every burst; the suite's 3-second idle windows
measure 0 ticks / 0 frames throughout.

## 9. Renderer changes

Both backends now draw the model they are handed:

- **wgpu**: one unit-cube instance per visual part baked into shared
  vertex/index buffers (painter's order), one uniform slot per part
  (`view_proj`, composed model, tint) plus a shadow slot; per-frame
  writes are the part matrices/tints. `cube.wgsl` gained a part
  pipeline (per-part tint uniform) and kept the shadow pipeline
  verbatim.
- **Bevy**: a root entity (composed root pose) with element children
  and part grandchildren — Bevy's transform propagation composes the
  same math; per-part materials carry the tint (assigned only on
  change).
- **Hitboxes**: `Renderer::character_hitboxes` (per element, logical px,
  hit-priority order, **pose-independent**) replaces the old single
  `character_hitbox`. The body element's rect reproduces the Phase 2
  grab reach exactly (√2·1.12 margin), asserted by unit test.
- The anchor-plane placement math is untouched: the anchor still maps
  to exactly `(u·W, v·H)` logical pixels, which is what keeps input
  regions, hitboxes and pixels in agreement.

## 10. Current limitations

- The model importer is out of scope: only the procedural prototype
  cube exists; parts are boxes.
- Physics is opt-in (default off) pending Phase 3b stabilization; the
  body box is the only collider.
- Element animation supports scale/tint only (no per-element rotation
  offsets yet); skeletal animation does not exist yet.
- Hover re-evaluation happens on pointer events only: a character that
  lands under a stationary pointer does not re-highlight until the
  pointer moves (the compositor does not re-send enter for input-region
  changes under it).
- Sleeping visuals are prototype-grade (flattened eyes), and the sleep
  timeout is a constant, not config.
- The Bevy backend's per-frame cost (measured Phase 2b: ~4–6× wgpu per
  frame) is inherited unchanged; character parts add draw calls, not
  systems.
- Window/desktop awareness: delivered via the neutral `ocm-desktop`
  providers (Phase 3b); the character itself still consumes only
  output-local bounds, and window-based behaviors are future work (§7).

## 11. Phase 3a results (2026-11-19)

All automated evidence from `tests/niri.sh` on the development machine
(Niri 26.04 nested, focused on the host, NVIDIA RTX 5050, fractional
1.25× via the live host output): **30/30 PASS for `--renderer wgpu`
and 30/30 for `--renderer bevy`**.

| Suite section | Evidence |
|---|---|
| Transparency + placement | corners exactly magenta; character pixels 100% non-background at the anchor; partial-alpha shadow band 6132 px (unchanged from Phase 2 — the part-based scene reproduces the footprint) |
| Idle cost | 0 CPU ticks / 0 frames over 3 s parked (both backends; one run in four shows a single 10 ms accounting tick with 0 frames — the suite retries once and fails only if a second window also ticks) |
| Hover / click | pointer enter through the hitbox-only region; `hover state changed` + `character clicked` logged; click-through click far away never arrives |
| Drag | press → motion threshold → drag; cube pixel-verified at (0.22, 0.3) after the drop (pointer-grab region policy) |
| Physics (`--physics` second launch) | `phase="falling"` at init; spawn fall → `character landed` → parked; drag up-left + release mid-motion → second `character landed`; character pixel-verified resting on the bottom strip; settled-2 s window renders 0 frames; right-click quit + full teardown order |
| Teardown | renderer → platform/session order logged in both sections, both backends |

Unit coverage (`cargo test --workspace`, 76 tests): element ordering
contracts (render order vs hit priority), the full phase transition
matrix, scripted pointer sequences (click-vs-drag threshold, grab
anchoring, stale-sample release velocity), physics determinism
(bit-equal traces), parabolic/landing/wall analytics, bounded
animation windows, pose-independent hitboxes, and the neutral-pose
idle invariant.

Not tested / not claimed: Hyprland (unchanged, blocked — see
`docs/compositor-compatibility.md` §4), window awareness (§7),
per-element rotation animation, imported models.

## 12. Phase 3b suggestions

1. Make physics the default once the suite's physics section has soaked;
   tune gravity/restitution from real sessions.
2. Per-element rotation offsets in `ElementPose` (arm waves, head
   turns) — both renderers already compose element transforms.
3. Animation transitions (ease between phases) and a small library of
   idle behaviors (look around, wander within bounds — the physics and
   bounds seams already exist).
4. GLB/glTF import behind a `ModelSource` trait producing
   `CharacterModel` + animation clips mapped onto the phase table.
5. Config for the sleep timeout + a wake-up animation.
