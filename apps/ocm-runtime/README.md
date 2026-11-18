# ocm-runtime

The OpenCuteMate PoC runtime: a native Wayland layer-shell overlay with a
selectable rendering backend (wgpu-direct or Bevy), a transparent
procedural character (currently a cube), pointer interaction,
click-through, and an event-driven render scheduler.

## Build

```sh
cargo build -p ocm-runtime            # debug (both backends)
cargo build --release -p ocm-runtime
cargo build -p ocm-runtime --no-default-features --features renderer-wgpu  # bevy-less build
```

Requires a Rust toolchain matching `rust-toolchain.toml` and a Vulkan
driver. No system Wayland libraries are needed to build (wayland-client
links libwayland dynamically at runtime — the session already provides
it). The default build includes both backends; `--renderer` selects at
runtime.

## Run

From a Wayland session (Niri, or any wlr-layer-shell compositor):

```sh
cargo run -p ocm-runtime                      # wgpu backend (default)
cargo run -p ocm-runtime -- --renderer bevy  # Bevy backend
```

You should see a floating cube near the center of the screen:

- the desktop is visible everywhere except the cube and its soft shadow;
- hover the cube — it brightens (renders on demand, then parks);
- **drag** it with the left mouse button — the input region follows;
- **right-click the cube** to quit (clean teardown, see the log);
- clicks anywhere else pass through to windows below.

### CLI

```
--renderer <NAME>       rendering backend: "wgpu" (default) or "bevy"
--config <FILE>        TOML configuration file
--layer <overlay|top>  layer-shell layer (default overlay)
--click-through         restrict input to the character hitbox (default)
--no-click-through     make the whole surface interactive
--fps <FPS>            scheduler FPS cap (default 60; compositor vsync applies)
--output <NAME>        target output by name (default: first output)
--position <X,Y>       initial character position in [0,1] (default 0.5,0.5)
--size <FRAC>          character height as a fraction of output height (default 0.4)
--animate              continuous bob + spin (frame-callback driven)
--no-animate           event-driven only (default)
--vsync / --no-vsync   present mode (default vsync/Fifo)
--log-level <LEVEL>    default tracing filter when RUST_LOG is unset
```

`RUST_LOG` takes precedence, e.g. `RUST_LOG=debug,naga=warn`.

### Configuration

Every CLI option maps to a TOML file (`--config`); unknown/missing fields
fall back to defaults:

```toml
[window]
layer = "overlay"             # or "top"
click_through = true
keyboard_interactivity = "none"  # none | ondemand | exclusive
output = ""                   # empty = first advertised output

[render]
fps_limit = 60
vsync = true

[character]
model = ""                    # reserved for a later PoC iteration
position_x = 0.5              # normalized [0,1]
position_y = 0.5
size = 0.4                    # fraction of output logical height
animate = false
```

### What the log tells you

At `info`, the runtime logs the negotiated GPU adapter, the surface
capabilities and the chosen format/alpha mode/present mode, the layer-shell
configure sizes (logical + buffer + scale), idle parking
(`rendering parked (idle)`), drag/quit events, and the three-step teardown
order (renderer before platform/session — the documented lifetime contract
for the unsafe wgpu surface creation).

## Automated validation

```sh
tests/niri.sh                  # nested Niri: 19 automated checks (wgpu backend)
OCM_RENDERER=bevy tests/niri.sh  # the same suite against the Bevy backend
tests/perf.sh [--release]      # repeatable renderer performance comparison
tests/hyprland.sh               # same suite against nested Hyprland (see docs for status)
```

Both suites inject input via `zwlr_virtual_pointer` into the *nested*
compositor — your physical mouse is never touched. See
`docs/compositor-compatibility.md` for current results and
requirements, and `docs/bevy-integration.md` §9 for the backend
comparison.
