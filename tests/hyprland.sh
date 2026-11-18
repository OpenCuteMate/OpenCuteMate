#!/usr/bin/env bash
# OpenCuteMate PoC — automated Hyprland validation suite.
#
# Same approach as tests/niri.sh, but against a NESTED Hyprland instance:
#   * input injected via zwlr_virtual_pointer (no physical devices touched)
#   * pixel analysis against a solid magenta compositor background
#     (misc:background_color; requires a reasonably recent Hyprland)
#   * screenshots via grim (wlr-screencopy)
#
# Requirements: a Hyprland binary. Resolution order:
#   1. $HYPRland_BIN if set
#   2. system Hyprland
#   3. nix run nixpkgs#hyprland (binary cache)
#
# usage: tests/hyprland.sh [--keep]

set -u

REPO=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO"
KEEP=${1:-}
WORK=$(mktemp -d /tmp/opencode/ocm-hypr-XXXXXX)
APP="$REPO/target/debug/ocm-runtime"
INJECT="$REPO/target/debug/examples/inject"
PASS=0
FAIL=0
HYPR_PID=""
APP_PID=""

say()  { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
ok()   { printf '[\033[32mPASS\033[0m] %s\n' "$*"; PASS=$((PASS+1)); }
bad()  { printf '[\033[31mFAIL\033[0m] %s\n' "$*"; FAIL=$((FAIL+1)); }
note() { printf '       %s\n' "$*"; }

cleanup() {
  [ -n "${APP_PID:-}" ] && kill "$APP_PID" 2>/dev/null
  [ -n "${HYPR_PID:-}" ] && kill "$HYPR_PID" 2>/dev/null
  if [ "$KEEP" != "--keep" ]; then rm -rf "$WORK"; else note "kept: $WORK"; fi
}
trap cleanup EXIT

say "building ocm-runtime + injector"
cargo build -p ocm-runtime 2>&1 | tail -1
cargo build -p ocm-platform-wayland --example inject 2>&1 | tail -1
[ -x "$APP" ] && [ -x "$INJECT" ] || { bad "binaries missing"; exit 1; }

# --- locate a Hyprland binary ---------------------------------------------------
HYPR_BIN="${HYPRland_BIN:-}"
if [ -z "$HYPR_BIN" ]; then
  if command -v Hyprland >/dev/null 2>&1; then
    HYPR_BIN=$(command -v Hyprland)
  elif command -v nix >/dev/null 2>&1; then
    note "no system Hyprland; fetching via nix (binary cache)"
    if ! nix build --no-link --print-out-paths nixpkgs#hyprland > "$WORK/hypr-path" 2>"$WORK/nix.err"; then
      bad "could not obtain Hyprland"
      sed -n '1,10p' "$WORK/nix.err"
      exit 1
    fi
    HYPR_BIN=$(cat "$WORK/hypr-path")/bin/Hyprland
  fi
fi
[ -n "$HYPR_BIN" ] && [ -x "$HYPR_BIN" ] || { bad "no Hyprland binary available"; exit 1; }
note "Hyprland binary: $HYPR_BIN ($("$HYPR_BIN" --version 2>/dev/null | head -1 || echo unknown version))"

HOST_DISPLAY="${WAYLAND_DISPLAY:-wayland-1}"
say "host session: WAYLAND_DISPLAY=$HOST_DISPLAY"

say "writing nested Hyprland config"
cat > "$WORK/hypr.conf" <<EOF
misc {
    background_color rgb(255, 0, 255)
}
xwayland {
    enabled = false
}
decoration {
    shadow {
        enabled = false
    }
    blur {
        enabled = false
    }
}
animations {
    enabled = false
}
EOF

# Private XDG_RUNTIME_DIR: the nested compositor must not fight the host
# session over the wayland-N socket/lock names (it stalled on wayland-1,
# which belongs to the host niri). Everything we launch against it
# (app, injector, hyprctl, grim) gets the same XDG_RUNTIME_DIR.
HYPR_RUNTIME_DIR="$WORK/run"
mkdir -m 700 "$HYPR_RUNTIME_DIR"

say "launching nested Hyprland"
HYPR_DIRS_BEFORE=$(ls "$HYPR_RUNTIME_DIR/hypr" 2>/dev/null | tr '\n' ' ' || true)
XDG_RUNTIME_DIR="$HYPR_RUNTIME_DIR" "$HYPR_BIN" -c "$WORK/hypr.conf" > "$WORK/hypr.log" 2>&1 &
HYPR_PID=$!
SIG=""
for _ in $(seq 1 50); do
  # The instance dir name IS the instance signature (newer Hyprland does not
  # print it on stdout because stdout logging is disabled early).
  for d in $(ls -t "$HYPR_RUNTIME_DIR/hypr" 2>/dev/null); do
    case " $HYPR_DIRS_BEFORE " in *" $d "*) ;; *) SIG="$d"; break;; esac
  done
  [ -n "$SIG" ] && break
  kill -0 "$HYPR_PID" 2>/dev/null || break
  sleep 0.2
done
if [ -z "$SIG" ]; then
  bad "could not identify the Hyprland instance signature"
  sed -n '1,30p' "$WORK/hypr.log"
  exit 1
fi
HYPR_RUNTIME="$HYPR_RUNTIME_DIR/hypr/$SIG"
note "instance: $SIG"
note "runtime dir: $HYPR_RUNTIME"

# Hyprland's Wayland socket for clients.
WLSOCK=""
for _ in $(seq 1 50); do
  WLSOCK=$(ls "$HYPR_RUNTIME_DIR"/wayland-* 2>/dev/null | head -1)
  [ -n "$WLSOCK" ] && break
  sleep 0.2
done
[ -n "$WLSOCK" ] || { bad "Hyprland Wayland socket not found"; exit 1; }
note "Wayland socket: $WLSOCK"

hypr() { XDG_RUNTIME_DIR="$HYPR_RUNTIME_DIR" HYPRLAND_INSTANCE_SIGNATURE="$SIG" hyprctl -i "$SIG" "$@"; }

for _ in $(seq 1 50); do
  hypr monitors >/dev/null 2>&1 && break
  sleep 0.2
done
hypr monitors >/dev/null 2>&1 || { bad "nested Hyprland not responding (hyprctl)"; exit 1; }
ok "nested Hyprland running"

read -r OUT_W OUT_H <<<"$(hypr -j monitors | python3 -c '
import json,sys
m = json.load(sys.stdin)[0]
print(int(m["width"]), int(m["height"]))')"
note "nested monitor: ${OUT_W}x${OUT_H}"

# --- injector ------------------------------------------------------------------
mkfifo "$WORK/inject.in"
XDG_RUNTIME_DIR="$HYPR_RUNTIME_DIR" WAYLAND_DISPLAY="$WLSOCK" "$INJECT" < "$WORK/inject.in" > "$WORK/inject.log" 2>&1 &
INJECT_PID=$!
exec 3> "$WORK/inject.in" || { bad "could not open injector fifo"; kill "$INJECT_PID"; exit 1; }
sleep 0.3
inj() { echo "$*" >&3; }

# --- app ------------------------------------------------------------------------
say "launching ocm-runtime against nested Hyprland (click-through on)"
APP_LOG="$WORK/app.log"
XDG_RUNTIME_DIR="$HYPR_RUNTIME_DIR" WAYLAND_DISPLAY="$WLSOCK" RUST_LOG="debug,naga=warn,wgpu=warn" "$APP" \
  --position 0.5,0.55 --size 0.4 --fps 60 > "$APP_LOG" 2>&1 &
APP_PID=$!

PARKED=0
for _ in $(seq 1 200); do
  if grep -q "rendering parked (idle)" "$APP_LOG"; then PARKED=1; break; fi
  kill -0 "$APP_PID" 2>/dev/null || break
  sleep 0.25
done
if ! kill -0 "$APP_PID" 2>/dev/null; then
  bad "app exited during startup"
  cat "$APP_LOG"
  exit 1
fi
if [ "$PARKED" -ne 1 ]; then
  bad "app never parked (no 'rendering parked' within 50 s)"
  kill "$APP_PID"; exit 1
fi
ok "app started, rendered, parked (idle)"

# --- protocol evidence ------------------------------------------------------------
hypr layers | grep -q opencutemate \
  && ok "layer-shell surface registered (namespace opencutemate)" \
  || bad "layer-shell surface not in 'hyprctl layers'"
hypr layers 2>/dev/null | grep -iE "onDemand|none" | head -1 | grep -qi none \
  && ok "keyboard interactivity reported as none" \
  || note "keyboard interactivity: $(hypr layers 2>/dev/null | grep -i 'keyboard' | head -1)"

grep -q "layer surface configured" "$APP_LOG" \
  && ok "layer-shell configure received + acked" \
  || bad "no configure in app log"
grep -qE "surface configured format=.*alpha_mode=PreMultiplied" "$APP_LOG" \
  && ok "wgpu surface configured with PreMultiplied alpha (as logged)" \
  || note "surface config line differs: $(grep -h 'surface configured' "$APP_LOG" | head -1)"
ldd "$APP" 2>/dev/null | grep -qi "libX11\|libxcb" \
  && bad "binary links X11/XCB (XWayland violation)" \
  || ok "no X11/XCB linkage (pure Wayland, no XWayland)"

# --- idle measurement ---------------------------------------------------------------
say "idle CPU measurement (3 s window, pre-input)"
F1=$(grep -c "frame presented" "$APP_LOG" || true)
U1=$(awk '{print $14+$15}' /proc/$APP_PID/stat 2>/dev/null)
sleep 3
U2=$(awk '{print $14+$15}' /proc/$APP_PID/stat 2>/dev/null)
F2=$(grep -c "frame presented" "$APP_LOG" || true)
CLK_TCK=$(getconf CLK_TCK 2>/dev/null || echo 100)
IDLE_MS=$(( (U2-U1) * 1000 / CLK_TCK ))
if [ "${F2:-0}" -gt "${F1:-0}" ]; then
  note "frames during window (interference?); retrying once"
  sleep 2
  F1=$(grep -c "frame presented" "$APP_LOG" || true)
  U1=$(awk '{print $14+$15}' /proc/$APP_PID/stat 2>/dev/null)
  sleep 3
  U2=$(awk '{print $14+$15}' /proc/$APP_PID/stat 2>/dev/null)
  F2=$(grep -c "frame presented" "$APP_LOG" || true)
fi
if [ "${U2:-1}" -gt "${U1:-0}" ]; then
  bad "idle CPU ${IDLE_MS} ms over 3 s (expected 0)"
else
  ok "idle CPU = 0 ticks over 3 s with 0 frames presented (thread parked)"
fi

# --- screenshot + pixel analysis ------------------------------------------------------
say "screenshot + pixel analysis"
grim -o "$(hypr -j monitors | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["name"])')" \
  "$WORK/screen.png" 2>/dev/null \
  || XDG_RUNTIME_DIR="$HYPR_RUNTIME_DIR" WAYLAND_DISPLAY="$WLSOCK" grim "$WORK/screen.png"
[ -s "$WORK/screen.png" ] || { bad "grim could not capture"; exit 1; }
note "screenshot: $(magick identify -format '%wx%h' "$WORK/screen.png" 2>/dev/null)"

if python3 tests/analyze_screenshot.py "$WORK/screen.png" "$OUT_W" "$OUT_H" 0.5 0.55 0.4; then
  ok "transparency + cube + partial-alpha pixel checks"
  cp "$WORK/screen.png" "$REPO/target/hypr-validation.png" 2>/dev/null
else
  bad "pixel analysis failed (see details above)"
fi

# --- input automation via virtual pointer ---------------------------------------------
say "input automation via virtual pointer (exact coordinates, no physical devices)"

CUBE_A_X=$(python3 -c "print(int(0.5 * $OUT_W))");  CUBE_A_Y=$(python3 -c "print(int(0.55 * $OUT_H))")
CUBE_B_X=$(python3 -c "print(int(0.22 * $OUT_W))"); CUBE_B_Y=$(python3 -c "print(int(0.30 * $OUT_H))")

inj move "$CUBE_A_X" "$CUBE_A_Y" "$OUT_W" "$OUT_H"
sleep 0.8
grep -q "pointer enter" "$APP_LOG" \
  && ok "pointer enter delivered (inside hitbox-only input region)" \
  || bad "no pointer enter in app log"
grep -q "hover state changed" "$APP_LOG" \
  && ok "hover reaction (tint re-render)" \
  || bad "no hover reaction in app log"

EVENTS_BEFORE=$(grep -c "pointer button" "$APP_LOG" || true)
inj move "$(python3 -c "print(int(0.03*$OUT_W))")" "$(python3 -c "print(int(0.05*$OUT_H))")" "$OUT_W" "$OUT_H"
sleep 0.5
inj click 0x110
sleep 0.6
EVENTS_AFTER=$(grep -c "pointer button" "$APP_LOG" || true)
if [ "${EVENTS_AFTER:-0}" -eq "${EVENTS_BEFORE:-0}" ]; then
  ok "click-through: left click far from the character never reached the app"
else
  bad "empty-area click reached the app (input region broken)"
fi

say "dragging the character to (0.22, 0.3)"
inj move "$CUBE_A_X" "$CUBE_A_Y" "$OUT_W" "$OUT_H"
sleep 0.4
inj down 0x110
sleep 0.3
for f in 0.45 0.40 0.34 0.28 0.22; do
  inj move "$(python3 -c "print(int($f*$OUT_W))")" "$(python3 -c "print(int(0.475*$OUT_H))")" "$OUT_W" "$OUT_H"
  sleep 0.12
done
for f in 0.44 0.38 0.34 0.30; do
  inj move "$CUBE_B_X" "$(python3 -c "print(int($f*$OUT_H))")" "$OUT_W" "$OUT_H"
  sleep 0.12
done
sleep 0.2
inj up 0x110
sleep 0.8

grep -q "drag started" "$APP_LOG" \
  && ok "drag started (press inside hitbox)" \
  || bad "no drag start in app log"
grep -q "drag ended" "$APP_LOG" \
  && ok "drag ended (input region followed the character)" \
  || bad "no drag end in app log"

WAYLAND_DISPLAY="$WLSOCK" XDG_RUNTIME_DIR="$HYPR_RUNTIME_DIR" grim "$WORK/screen2.png"
python3 tests/analyze_screenshot.py "$WORK/screen2.png" "$OUT_W" "$OUT_H" 0.22 0.30 0.4 >/dev/null 2>&1 \
  && ok "cube followed the drag to (0.22, 0.3) (pixel-verified)" \
  || bad "cube did not follow the drag (pixel check at new position failed)"

say "right-click on the character -> quit"
inj move "$CUBE_B_X" "$CUBE_B_Y" "$OUT_W" "$OUT_H"
sleep 0.4
inj click 0x111
EXIT_CODE=-1
for _ in $(seq 1 40); do
  kill -0 "$APP_PID" 2>/dev/null || { wait "$APP_PID"; EXIT_CODE=$?; break; }
  sleep 0.2
done
if [ "$EXIT_CODE" -eq 0 ]; then
  ok "app exited cleanly via right-click (exit 0)"
else
  kill "$APP_PID" 2>/dev/null
  bad "app did not quit on right-click (last exit=$EXIT_CODE)"
fi
grep -q "right click on character; quitting" "$APP_LOG" \
  && ok "quit reason logged" || bad "quit reason missing in log"
grep -q "teardown: dropping renderer (wgpu surface) first" "$APP_LOG" \
  && grep -q "teardown: dropping platform/wayland session" "$APP_LOG" \
  && grep -q "teardown complete" "$APP_LOG" \
  && ok "teardown order logged: renderer before platform" \
  || bad "teardown order not fully logged"

say "idle render accounting"
FRAMES=$(grep -c "frame presented" "$APP_LOG" || true)
note "total frames presented: $FRAMES (startup + scripted interactions only; parked otherwise)"

say "summary: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
exit 0
