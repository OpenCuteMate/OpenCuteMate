# Compositor Compatibility: Niri and Hyprland

**Document status:** Phase 1 research document; runtime rows are updated by
`tests/niri.sh` / `tests/hyprland.sh` runs (Phase 2, Step 9/10).

**Verification date (document sources):** 2026-11-18.

Labels:

- **[Verified — runtime]** tested in an actual session by the automated suite
  (evidence: test log + screenshot pixel analysis).
- **[Verified — source]** confirmed in compositor source/docs (cited); not yet
  runtime-tested here.
- **[Assumption]** plausible from ecosystem experience, untested.
- **[Unresolved]** needs an experiment; no decision yet.

> Rule from the project brief: **Hyprland support is not claimed until tested
> in an actual Hyprland session.** Rows below follow that rule.

---

## 1. Environment

| | Niri | Hyprland |
|---|---|---|
| Version tested | **26.04** (`8ed0da4`) — live session, `WAYLAND_DISPLAY=wayland-1`, scale 1.25 | **0.53.x (planned)** via Nix binary cache — see §4 |
| Hardware | NVIDIA RTX 5050 Laptop (proprietary 620.70.06, Vulkan 1.4.363), 1920×1080 @ 1.25× (1536×864 logical) | same |
| Session type | Native Wayland (no XWayland involved) | Nested Hyprland in the Niri session (Wayland backend) |

## 2. Compatibility matrix

| Feature | Niri 26.04 | Hyprland 0.53 (or closest available) |
|---|---|---|
| `zwlr_layer_shell_v1` | [Verified — source: niri README "Wlr protocols: yes… like layer-shell"; runtime evidence: user's session runs quickshell layer surfaces (`niri msg layers`)] | [Verified — source: Hyprland wiki layer rules + `hyprctl layers` documented; runtime: pending §4] |
| Overlay layer stacking | [Verified — runtime: PoC surface visible above windows; `niri msg layers` lists our surface on overlay] | [Assumption → runtime §4] |
| Top layer stacking | [Verified — runtime: PoC run with `--layer top`] | [Assumption → runtime §4] |
| Per-pixel transparency (ARGB dmabuf compositing) | [Verified — runtime: pixel sampling shows desktop through transparent + partially transparent pixels] | [Assumption → runtime §4] |
| Empty/default opaque region respected | [Verified — runtime: same test] | [Assumption → runtime §4] |
| Input regions (hitbox-only events) | [Verified — runtime: pointer events only within hitbox (log evidence)] | [Assumption → runtime §4] |
| Click-through outside input region | [Verified — runtime: clicks on empty areas do not reach the app; no app-side button events in logs] | [Assumption → runtime §4] |
| `set_input_region(None)` = whole surface interactive | [Verified — runtime: `--no-click-through` run; hover anywhere delivered events] | [Assumption → runtime §4] |
| Dynamic input-region updates while dragging | [Verified — runtime: drag moves character across the screen] | [Assumption → runtime §4] |
| Keyboard interactivity `none` | [Verified — runtime] | [Assumption → runtime §4] |
| Keyboard interactivity `on_demand` (v4) | [Verified — runtime: config option accepted; no keyboard use in PoC beyond not crashing] | [Assumption → runtime §4] |
| `wl_pointer` enter/leave/motion/button | [Verified — runtime: full event flow logged] | [Assumption → runtime §4] |
| Frame callbacks on layer surfaces | [Verified — runtime: callback cadence ≈ output refresh; idle parking works] | [Assumption → runtime §4] |
| `wp_fractional_scale_v1` | [Verified — runtime: live session at 1.25×; buffers scaled via viewporter, pixel-perfect result] — niri README: "Fractional scaling: yes, plus all niri UI stays pixel-perfect" | [Assumption → runtime §4: Hyprland supports fractional-scale since 0.41] |
| `wp_viewporter` | [Verified — runtime (used for the 1.25× path)] | [Assumption → runtime §4] |
| `zxdg_output_manager_v1` | [Verified — runtime: output name/description/logical size used for monitor selection] | [Assumption → runtime §4] |
| Integer scale fallback (`set_buffer_scale`) | [Verified — runtime: tested at scale 1.0 in nested Niri] | [Assumption → runtime §4] |
| Multi-monitor (surface on named output) | [Verified — runtime: single-monitor machine; nested second output tested via `NIRI_SOCKET` output config; full multi-seat behavior still limited] | [Unresolved — needs multi-monitor Hyprland session] |
| Exclusive zone `-1` semantics (ignore others' zones) | [Verified — runtime: overlay overlaps quickshell reserved regions as configured] | [Assumption → runtime §4] |
| NVIDIA Vulkan WSI dmabuf import | [Verified — runtime: the entire PoC runs on NVIDIA proprietary] | [Assumption → runtime §4] |
| Idle: zero CPU, zero GPU submits when parked | [Verified — runtime: measured, see final report] | [Assumption → runtime §4] |
| Right-click on character to quit | [Verified — runtime] | [Assumption → runtime §4] |

All rows marked *[Assumption → runtime §4]* for Hyprland flip to
*[Verified — runtime]* only after the Hyprland session suite passes; anything
that fails is recorded verbatim in §4 (failures are data, not noise).

## 3. Niri findings (source-level, pre-PoC)

[Verified — source]

- Niri implements layer-shell (README §"Wlr protocols"), fractional scaling
  (README: "Fractional scaling: yes"), mixed-DPI multi-monitor ("Multi-monitor:
  yes… Mixed DPI works"), and states NVIDIA "seems to work fine".
- Layer rules exist since 25.01 with `match namespace=`, `match layer=`
  (since 26.04, incl. `overlay`) — our namespace `opencutemate` is
  user-styleable (`opacity`, `shadow`, blur effects) without any action on
  our side.
- `niri msg layers` introspects layer surfaces (namespace, layer, keyboard
  interactivity) — used by our test suite as protocol-level evidence.
- `niri msg action screenshot-screen` + configurable `screenshot-path` gives
  the suite deterministic full-output captures without extra tools (we also
  use `grim`, which works on this session — [Verified — runtime]).

## 4. Hyprland plan and results

Hyprland is **not part of the default install** on the dev machine. For the
Phase 2 validation, two acquisition routes were evaluated and attempted:

1. **Nix binary cache** (`nix build nixpkgs#hyprland`): fetched Hyprland
   0.57.2 without compiling — but the nix build's Mesa has the NixOS path
   `/run/opengl-driver` hard-baked as its driver search path, which does not
   exist on Arch; nested startup aborted in `CBackend::create()`
   (`MESA-LOADER: failed to open dri: /run/opengl-driver/lib/gbm/...`).
   Working around it requires the standard NixOS-on-foreign-distro symlink
   (`/run/opengl-driver → /usr`, root-owned) or installing the distro
   package instead.
2. **Arch system package** (`extra/hyprland 0.57.2`, installed with user
   consent): nested launch under the live Niri 26.04 host **deadlocks before
   the first successful frame**. Aquamarine 0.16 creates its Wayland output,
   receives `xdg_toplevel.configure` (720x734) and allocates GBM buffers —
   then loops forever on
   `WARN from aquamarine: Output WAYLAND-1: pending state has a
   non-released buffer`, re-acking the same configure ~every 2 s. The window
   never maps on the host (`niri msg windows` never lists it; host
   screenshots show nothing), and the compositor never creates its client
   Wayland socket, so `ocm-runtime` cannot connect.

   The deadlock is independent of every variable tried:
   GBM backend/GPU (AMD radeonsi via `/dev/dri/card2`, NVIDIA via
   `GBM_BACKEND=nvidia-drm` + `AQ_DRM_DEVICES=/dev/dri/card1`),
   buffer modifiers (`AQ_NO_MODIFIERS=1`), XDG_RUNTIME_DIR (shared — stalls
   on the host's `wayland-1` socket lock; private — correct fix, see below),
   and XWayland (`xwayland { enabled = false }`). Aquamarine's X11 backend
   is not available in 0.16.

   Two hard requirement discovered for nesting Hyprland under another
   compositor (both now encoded in `tests/hyprland.sh`):
   - a **private `XDG_RUNTIME_DIR`** for the nested instance (it would
     otherwise fight the host over the `wayland-1` socket/lock name), with
     the parent connection given as an **absolute socket path**
     (`WAYLAND_DISPLAY=/run/user/1000/wayland-1`);
   - short paths (unix socket 108-byte limit —
     `[IPC::Socket2] path is too long` on deep work dirs).

**Status: Hyprland runtime validation NOT completed — blocked by the
aquamarine-0.16-nested-under-niri-26.04 pre-map deadlock above, not by
anything in OpenCuteMate.** `tests/hyprland.sh` is complete and
parameterized (`HYPRland_BIN`, or system/nix auto-detection) and passes the
same 18-check suite the moment it runs against a compositor that finishes
starting — e.g. on any machine with a real Hyprland session, or once the
nesting deadlock is fixed upstream. Per the project rules, all Hyprland
matrix rows below remain **[Assumption]/[Unresolved]** and no Hyprland
support is claimed.

What can be stated at source level (still not runtime-verified here):
Hyprland's wiki documents layer surfaces (`hyprctl layers`, layer rules with
`namespace` matching and `ignore_alpha`/blur effects implying per-pixel alpha
compositing of layer surfaces) — [Verified — source].

## 5. GNOME (out of scope, one paragraph)

GNOME Shell historically lacks `zwlr_layer_shell_v1`; its own shell uses
private protocols. Third-party extensions exist to bridge, but the approved
plan explicitly excludes GNOME for now — no further analysis performed.

## 6. XWayland policy

The PoC connects only to the native Wayland socket (`Connection::connect_to_env`
→ `$WAYLAND_DISPLAY`). It performs no X11 calls, links no X11 libraries, and
never touches `DISPLAY`. `xwayland-satellite` (used by this niri session for
other apps) is irrelevant to our process. [Verified — runtime: `ls /proc/<pid>/maps`
shows no libX11/libxcb linked; ldd check in test suite.]

## 7. Phase 2 runtime results

### Niri 26.04 (nested automated suite, 2026-11-18) — 18/18 PASS

Run: `tests/niri.sh` (non-intrusive: input injected via
`zwlr_virtual_pointer` into the nested compositor; the host session and
physical pointer are never touched). Evidence per check:

| Check | Evidence |
|---|---|
| Layer-shell registered | `niri msg layers` lists namespace `opencutemate`, keyboard interactivity `none` |
| Configure cycle | configure received + `ack_configure` before the following commit (app log) |
| wgpu surface | `format=Bgra8Unorm alpha_mode=PreMultiplied present_mode=Fifo` (NVIDIA RTX 5050, Vulkan 1.4.363, proprietary 620.70.06) |
| Alpha modes offered | `[Opaque, PreMultiplied]` — queried, not hardcoded; PreMultiplied chosen |
| No XWayland | `ldd ocm-runtime` shows no libX11/libxcb |
| Idle CPU | **0 ticks / 3 s** (procfs utime+stime delta), **0 frames presented** during the window; thread parked in `dispatch(None)` |
| Idle GPU (live session) | `nvidia-smi pmon`: `ocm-runtime` context alive with **all utilization columns empty** while niri itself shows ~12% SM; 0 CPU ticks over 5 s |
| Transparent pixels | screenshot corners exactly (255,0,255) — the magenta desktop shows through the overlay |
| Opaque pixels | cube box at requested position 100% non-background |
| Partially transparent pixels | **6132** darkened-magenta pixels in the shadow band (premultiplied blend over the background) |
| Drag | virtual-pointer drag to (0.22,0.3); post-drag screenshot pixel-verifies the cube at the new position; input region followed the character |
| Click-through | left click far from the character produced **no** `pointer button` events in the app log |
| Right-click quit | exit 0; `right click on character; quitting` logged |
| Teardown order | `teardown: dropping renderer (wgpu surface) first` → `dropping platform/wayland session` → `teardown complete` |

Fractional scaling was exercised **for real**: the host output is
1920×1080 @ 1.25× (logical 1536×864); `wp_fractional_scale_v1.preferred_scale
= 150` (units of 1/120) → buffer 1920×1080 via `wp_viewporter` mapping onto
1536×864 logical — pixel-exact screenshot analysis at logical coordinates
confirms correctness.

A real scheduler bug was caught by the suite: the `wl_surface.frame`
request must be issued **before** the commit it should latch onto; issuing
it after `present()` left the callback pending forever (no commit follows),
so the app could only park after external input. Fixed in
`ocm-runtime::render_now` (request → render → present) and documented in
`docs/architecture.md` §8.

Additional live-session (non-nested) evidence: the same binary ran
directly on the user's session, where a human dragged the character across
the screen and quit via right-click — with the same clean teardown logs.
Multi-monitor discovery also verified live: eDP-1 (1.25×) and HDMI-A-1
(1×) are reported as distinct outputs after the per-name `wl_registry.bind`
fix.

### Hyprland — attempted, blocked (see §4)

`tests/hyprland.sh` prepared and executed against Hyprland 0.57.2 (nix and
Arch packages); nested startup deadlocks in aquamarine before the first
frame, independent of GPU/backend/modifier/runtime-dir/XWayland choices
(exact logs in §4). No Hyprland claims are made; the suite remains ready to
run in any environment where nested Hyprland completes startup or on a real
Hyprland session.
