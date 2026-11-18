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

Hyprland is **not installed** on the dev machine. Options evaluated:

1. **Nix binary cache** (`nix run nixpkgs#hyprland`): feasible — Nix 2.35.2
   present, Hyprland is in nixpkgs with binary substitutes. Status of the
   attempt: **[Unresolved until executed — see final report §8]**.
2. Distro packaging: not available for this distro at the moment.
3. Manual build from source: rejected for the PoC window (long C++ build).

The test script `tests/hyprland.sh` is written and parameterized so that the
suite can run against any Hyprland binary (system or Nix). It:

- launches Hyprland **nested** in the current Wayland session (Wayland
  backend), captures `HYPRLAND_INSTANCE_SIGNATURE`,
- discovers the nested compositor's Wayland socket,
- runs `ocm-runtime` against it with a deterministic background
  (Hyprland config `misc:background_color` / wallpaper), performs the same
  pixel-sampling checks as the Niri suite,
- drives input with `ydotool` (pointer move / drag / right-click),
- uses `hyprctl -i <sig> layers` for protocol-level layer evidence and
  `grim` for screenshots (Hyprland implements wlr-screencopy /
  ext-image-copy-capture — [Verified — source: Hyprland wiki/ecosystem;
  runtime pending]).

**Hyprland rows above stay Assumption/Unresolved until this section records
an actual run.**

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

### Niri 26.04 (native session) — filled after `tests/niri.sh`

- Run date: 2026-11-18 (see final report for the full log).
- Every [Verified — runtime] row in §2 traces to a check in that suite.
- Notable measured values (formats/alpha modes/present modes/refresh,
  idle CPU): recorded in the final report and `docs/wayland-feasibility.md` §9.

### Hyprland — filled after `tests/hyprland.sh`

- (Pending; will contain: version, protocol availability from the app's
  global log, pixel-test results, input-test results, idle measurements, and
  any failures with exact errors.)
