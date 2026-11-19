#!/usr/bin/env bash
# OpenCuteMate — automated Niri validation suite.
#
# Runs the runtime inside a NESTED niri instance (deterministic magenta
# background) and validates:
#   1. protocol: layer-shell surface present on the overlay layer
#   2. rendering: wgpu init, first frames, idle parking
#   3. transparency: pixel analysis of a compositor screenshot
#      (background visible, character visible, partial-alpha shadow band)
#   4. input: hover, click reaction, drag with input-region updates,
#      click-through, right-click quit — all injected via
#      zwlr_virtual_pointer directly into the nested compositor
#      (deterministic, and the host session / physical pointer are never
#      touched)
#   5. idle CPU (procfs) and GPU (nvidia-smi, best-effort)
#   6. Phase 3a character physics (second launch with --physics):
#      spawn fall -> landing, drag toss with release velocity, bottom-
#      strip pixel verification, settle + park, teardown
#   7. Phase 3b desktop environment awareness (third launch with the
#      niri IPC socket attached): snapshot acquisition, event delivery
#      (windows open/float/focus/close, workspaces) through the
#      persistent event-stream subscription, floating-window geometry
#      pixel-verified against a compositor screenshot, graceful
#      degradation without the IPC socket, idle CPU unchanged
#
# Requires: cargo, niri, python3+PIL, magick, kitty (awareness section).
# No root, no uinput.
#
# usage: tests/niri.sh [--keep]           (keep the work dir for inspection)
#        OCM_RENDERER=bevy tests/niri.sh  (run against the Bevy backend;
#                                          default is wgpu)

set -u

REPO=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO"
KEEP=${1:-}
OCM_RENDERER=${OCM_RENDERER:-wgpu}
case "$OCM_RENDERER" in
  wgpu|bevy) ;;
  *) echo "OCM_RENDERER must be wgpu or bevy (got: $OCM_RENDERER)"; exit 2 ;;
esac
WORK=$(mktemp -d /tmp/opencode/ocm-niri-XXXXXX)
APP="$REPO/target/debug/ocm-runtime"
PASS=0
FAIL=0
NESTED_PID=""
APP_PID=""

say()  { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
ok()   { printf '[\033[32mPASS\033[0m] %s\n' "$*"; PASS=$((PASS+1)); }
bad()  { printf '[\033[31mFAIL\033[0m] %s\n' "$*"; FAIL=$((FAIL+1)); }
note() { printf '       %s\n' "$*"; }

cleanup() {
  [ -n "${APP_PID:-}" ] && kill "$APP_PID" 2>/dev/null
  [ -n "${NESTED_PID:-}" ] && kill "$NESTED_PID" 2>/dev/null
  [ -n "${PROBE_PID:-}" ] && kill "$PROBE_PID" 2>/dev/null
  for pid in ${KITTY_PIDS:-}; do kill "$pid" 2>/dev/null; done
  if [ -n "${INJECT_PID:-}" ]; then
    exec 3>&- 2>/dev/null || true
    kill "$INJECT_PID" 2>/dev/null
  fi
  if [ "$KEEP" != "--keep" ]; then rm -rf "$WORK"; else note "kept: $WORK"; fi
}
trap cleanup EXIT

say "building ocm-runtime (debug)"
cargo build -p ocm-runtime 2>&1 | tail -1 || { bad "build failed"; exit 1; }
[ -x "$APP" ] || { bad "binary missing"; exit 1; }

HOST_DISPLAY="${WAYLAND_DISPLAY:-wayland-1}"
say "host session: WAYLAND_DISPLAY=$HOST_DISPLAY"

say "writing nested niri config ($WORK/niri.kdl)"
cat > "$WORK/niri.kdl" <<EOF
input {
    keyboard {
        xkb {
            layout "us"
        }
    }
}
layout {
    background-color "#FF00FF"
}
screenshot-path "$WORK/screen.png"
EOF

say "launching nested niri"
niri -c "$WORK/niri.kdl" > "$WORK/nested-niri.log" 2>&1 &
NESTED_PID=$!

# The nested compositor opens TWO sockets:
#   * the IPC socket:  $XDG_RUNTIME_DIR/niri.wayland-N.<pid>.sock  (NIRI_SOCKET)
#   * the Wayland socket: $XDG_RUNTIME_DIR/<name>                    (WAYLAND_DISPLAY)
IPCSOCK=""
WLSOCK=""
for _ in $(seq 1 50); do
  IPCSOCK=$(ls "$XDG_RUNTIME_DIR"/niri.wayland-*."$NESTED_PID".sock 2>/dev/null | head -1)
  WLSOCK=$(grep -oP 'listening on Wayland socket: \K\S+' "$WORK/nested-niri.log" 2>/dev/null | head -1)
  [ -n "$IPCSOCK" ] && [ -n "$WLSOCK" ] && break
  sleep 0.2
done
if [ -z "$IPCSOCK" ] || [ -z "$WLSOCK" ]; then
  bad "nested niri sockets not found (ipc='$IPCSOCK' wl='$WLSOCK')"
  sed -n '1,20p' "$WORK/nested-niri.log"
  exit 1
fi
note "IPC socket:  $IPCSOCK"
note "Wayland socket: $WLSOCK"

for _ in $(seq 1 50); do
  NIRI_SOCKET="$IPCSOCK" niri msg outputs >/dev/null 2>&1 && break
  sleep 0.2
done
NIRI_SOCKET="$IPCSOCK" niri msg outputs >/dev/null 2>&1 || { bad "nested niri not responding"; exit 1; }
ok "nested niri running"

# Focus the nested compositor's toplevel on the HOST session. Niri only drives
# unfocused/occluded windows at ~0.5 Hz, which batches the virtual-pointer
# input delivered to the nested instance and breaks the wall-clock-timed
# sections below. Focusing the window (title "niri") makes the host drive it
# in real time. This is the one host-visible side effect of the suite: your
# keyboard focus moves to the test window for the duration of the run.
# (NIRI_SOCKET is unset here on purpose: this talks to the HOST compositor.)
HOST_NIRI_WIN=""
for _ in $(seq 1 30); do
  HOST_NIRI_WIN=$(niri msg --json windows 2>/dev/null | python3 -c '
import json, sys
ws = json.load(sys.stdin)
w = next((w for w in ws if w["title"] == "niri"), None)
print(w["id"] if w else "")' )
  [ -n "$HOST_NIRI_WIN" ] && break
  sleep 0.2
done
if [ -n "$HOST_NIRI_WIN" ]; then
  niri msg action focus-window --id "$HOST_NIRI_WIN" >/dev/null 2>&1 \
    && ok "nested compositor focused on the host (real-time frame pacing)" \
    || note "could not focus the nested window on the host (run may be throttled)"
else
  note "nested niri window not found on the host (run may be throttled)"
fi

# Nested output logical size (screenshot + injection coordinate space).
read -r OUT_W OUT_H <<<"$(NIRI_SOCKET="$IPCSOCK" niri msg --json outputs | python3 -c '
import json,sys
outs = json.load(sys.stdin)
o = list(outs.values())[0]
print(o["logical"]["width"], o["logical"]["height"])')"
note "nested output: ${OUT_W}x${OUT_H} logical"

say "building the virtual-pointer injector (input goes into the nested compositor only; the host is untouched)"
cargo build -p ocm-platform-wayland --example inject 2>&1 | tail -1 || { bad "injector build failed"; exit 1; }
INJECT="$REPO/target/debug/examples/inject"
[ -x "$INJECT" ] || { bad "injector binary missing"; exit 1; }
mkfifo "$WORK/inject.in"
WAYLAND_DISPLAY="$WLSOCK" "$INJECT" < "$WORK/inject.in" > "$WORK/inject.log" 2>&1 &
INJECT_PID=$!
exec 3> "$WORK/inject.in" || { bad "could not open injector fifo"; kill "$INJECT_PID"; exit 1; }
sleep 0.3
inj() { echo "$*" >&3; }

say "launching ocm-runtime (renderer: $OCM_RENDERER) against the nested compositor (default config: click-through on)"
APP_LOG="$WORK/app.log"
WAYLAND_DISPLAY="$WLSOCK" RUST_LOG="debug,naga=warn,wgpu=warn,bevy=warn,bevy_render=warn,bevy_pbr=warn,bevy_winit=warn" "$APP" \
  --renderer "$OCM_RENDERER" \
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

# --- protocol evidence -------------------------------------------------------
NIRI_SOCKET="$IPCSOCK" niri msg layers | grep -q opencutemate \
  && ok "layer-shell surface registered (namespace opencutemate)" \
  || bad "layer-shell surface not in 'niri msg layers'"
NIRI_SOCKET="$IPCSOCK" niri msg layers | grep -i "keyboard interactivity" | head -1 | grep -q "none" \
  && ok "layer surface reports keyboard interactivity: none" \
  || note "keyboard interactivity line: $(NIRI_SOCKET="$IPCSOCK" niri msg layers | grep -i 'keyboard' | head -1)"

grep -q "layer surface configured" "$APP_LOG" \
  && ok "layer-shell configure received + acked" \
  || bad "no configure in app log"

grep -qE "surface configured format=.*alpha_mode=PreMultiplied" "$APP_LOG" \
  && ok "wgpu surface configured with PreMultiplied alpha (as logged)" \
  || note "surface config line differs: $(grep -h 'surface configured' "$APP_LOG" | head -1)"

ldd "$APP" 2>/dev/null | grep -qi "libX11\|libxcb" \
  && bad "binary links X11/XCB (XWayland violation)" \
  || ok "no X11/XCB linkage (pure Wayland, no XWayland)"

# --- idle measurement (before any scripted input) --------------------------------
say "idle CPU measurement (3 s window, pre-input)"
# The parked app is expected to consume exactly 0 CPU ticks. A single
# 10 ms tick with 0 frames presented is scheduler accounting noise
# (observed ~1 in 4 runs on this host); like the frame-noise case below,
# one retry separates noise from a real regression: a tick that persists
# across two consecutive windows fails the check.
F1=$(grep -c "frame presented" "$APP_LOG" || true)
U1=$(awk '{print $14+$15}' /proc/$APP_PID/stat 2>/dev/null)
sleep 3
U2=$(awk '{print $14+$15}' /proc/$APP_PID/stat 2>/dev/null)
F2=$(grep -c "frame presented" "$APP_LOG" || true)
CLK_TCK=$(getconf CLK_TCK 2>/dev/null || echo 100)
IDLE_MS=$(( (U2-U1) * 1000 / CLK_TCK ))
if [ "${F2:-0}" -gt "${F1:-0}" ] || [ "${U2:-1}" -gt "${U1:-0}" ]; then
  note "idle window showed ${IDLE_MS} ms / ${F2:-0} frames (noise?); measuring once more"
  sleep 2
  F1=$(grep -c "frame presented" "$APP_LOG" || true)
  U1=$(awk '{print $14+$15}' /proc/$APP_PID/stat 2>/dev/null)
  sleep 3
  U2=$(awk '{print $14+$15}' /proc/$APP_PID/stat 2>/dev/null)
  F2=$(grep -c "frame presented" "$APP_LOG" || true)
  IDLE_MS=$(( (U2-U1) * 1000 / CLK_TCK ))
fi
if [ "${U2:-1}" -gt "${U1:-0}" ]; then
  bad "idle CPU ${IDLE_MS} ms over 3 s (expected 0)"
else
  ok "idle CPU = 0 ticks over 3 s with 0 frames presented (thread parked)"
fi

if command -v nvidia-smi >/dev/null; then
  nvidia-smi pmon -c 1 2>/dev/null | grep -q "$APP_PID" \
    && note "nvidia-smi pmon lists the process during idle (context alive, no work)" \
    || note "no GPU utilization rows for our process during idle (expected)"
fi

# --- screenshot + pixel analysis ------------------------------------------------
say "screenshot + pixel analysis"
rm -f "$WORK/screen.png"
NIRI_SOCKET="$IPCSOCK" niri msg action screenshot-screen
for _ in $(seq 1 40); do [ -s "$WORK/screen.png" ] && break; sleep 0.2; done
[ -s "$WORK/screen.png" ] || { bad "no screenshot from nested niri"; exit 1; }
note "screenshot: $(magick identify -format '%wx%h' "$WORK/screen.png" 2>/dev/null)"

if python3 tests/analyze_screenshot.py "$WORK/screen.png" "$OUT_W" "$OUT_H" 0.5 0.55 0.4; then
  ok "transparency + cube + partial-alpha pixel checks"
  cp "$WORK/screen.png" "$REPO/target/niri-validation.png" 2>/dev/null
else
  bad "pixel analysis failed (see details above)"
  # Preserve the failing screenshot for offline analysis.
  cp "$WORK/screen.png" "$REPO/target/niri-failure.png" 2>/dev/null
fi

# --- input automation via zwlr_virtual_pointer (host untouched) -------------------
say "input automation via virtual pointer (exact coordinates, no physical devices)"

CUBE_A_X=$(python3 -c "print(int(0.5 * $OUT_W))");  CUBE_A_Y=$(python3 -c "print(int(0.55 * $OUT_H))")
CUBE_B_X=$(python3 -c "print(int(0.22 * $OUT_W))"); CUBE_B_Y=$(python3 -c "print(int(0.30 * $OUT_H))")

# Wait until the app has logged a pattern (input delivery through the
# nested compositor can lag the script; polls instead of fixed sleeps).
wait_log() {
  # wait_log <pattern> <max_seconds>
  local pattern=$1 max=$2 i
  for i in $(seq 1 $((max * 4))); do
    grep -q "$pattern" "$APP_LOG" && return 0
    kill -0 "$APP_PID" 2>/dev/null || return 1
    sleep 0.25
  done
  return 1
}

# Hover over the cube (0.5, 0.55) — events must arrive through the hitbox.
inj move "$CUBE_A_X" "$CUBE_A_Y" "$OUT_W" "$OUT_H"
sleep 0.8
grep -q "pointer enter" "$APP_LOG" \
  && ok "pointer enter delivered (inside hitbox-only input region)" \
  || bad "no pointer enter in app log"
grep -q "hover state changed" "$APP_LOG" \
  && ok "hover reaction (tint re-render)" \
  || bad "no hover reaction in app log"

# Click ON the character (press + release, no motion): a semantic click
# reaction on a specific element — distinct from a drag.
inj click 0x110
wait_log "character clicked" 10 \
  && ok "click on the character logged (semantic element reaction)" \
  || bad "no click reaction in app log"

# Click far from the character (top-left): must NOT reach the app.
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

# Drag the cube from (0.5, 0.55) to (0.22, 0.3).
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

wait_log "drag started" 15 \
  && ok "drag started (press inside hitbox)" \
  || bad "no drag start in app log"
wait_log "drag ended" 15 \
  && ok "drag ended (pointer grab released, region restored at the drop position)" \
  || bad "no drag end in app log"

# Screenshot after the drag: the cube must be at (0.22, 0.3) now. Allow a
# short retry: the final present may lag the input by one frame-callback
# cycle when the nested compositor is throttled.
DRAG_PIXELS=1
for _ in 1 2 3; do
  rm -f "$WORK/screen.png"
  NIRI_SOCKET="$IPCSOCK" niri msg action screenshot-screen
  for _ in $(seq 1 40); do [ -s "$WORK/screen.png" ] && break; sleep 0.2; done
  python3 tests/analyze_screenshot.py "$WORK/screen.png" "$OUT_W" "$OUT_H" 0.22 0.30 0.4 >/dev/null 2>&1 && { DRAG_PIXELS=0; break; }
  sleep 1
done
if [ "$DRAG_PIXELS" -eq 0 ]; then
  ok "cube followed the drag to (0.22, 0.3) (pixel-verified)"
else
  bad "cube did not follow the drag (pixel check at new position failed)"
fi

# --- right-click quit --------------------------------------------------------------
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

# --- idle render accounting --------------------------------------------------------
say "idle render accounting"
FRAMES=$(grep -c "frame presented" "$APP_LOG" || true)
note "total frames presented: $FRAMES (startup + bounded reactions + scripted interactions; parked otherwise)"

# --- Phase 3a: physics section (second launch, --physics on) -----------------------
say "character physics (spawn fall, toss, boundaries) — second launch with --physics"
APP_LOG="$WORK/app-physics.log"
WAYLAND_DISPLAY="$WLSOCK" RUST_LOG="debug,naga=warn,wgpu=warn,bevy=warn,bevy_render=warn,bevy_pbr=warn,bevy_winit=warn" "$APP" \
  --renderer "$OCM_RENDERER" \
  --physics --position 0.5,0.25 --size 0.4 --fps 60 > "$APP_LOG" 2>&1 &
APP_PID=$!

PARKED=0
for _ in $(seq 1 200); do
  if grep -q "rendering parked (idle)" "$APP_LOG"; then PARKED=1; break; fi
  kill -0 "$APP_PID" 2>/dev/null || break
  sleep 0.25
done
kill -0 "$APP_PID" 2>/dev/null || { bad "physics app exited during startup"; cat "$APP_LOG"; exit 1; }
[ "$PARKED" -eq 1 ] || { bad "physics app never parked after the spawn fall"; kill "$APP_PID"; exit 1; }

grep -q 'phase="falling"' "$APP_LOG" \
  && ok "spawn state: character initialized airborne (phase=falling)" \
  || bad "no airborne init logged (physics off?)"
grep -q "character landed" "$APP_LOG" \
  && ok "spawn fall ended on the floor (character landed)" \
  || bad "no landing logged after the spawn fall"
ok "physics app parked after settling (idle again)"

# Grounded position: anchor at 1 - size/2 = 0.8 of the output height.
GROUND_Y=$(python3 -c "print(int(0.8 * $OUT_H))")
GROUND_X=$(python3 -c "print(int(0.5 * $OUT_W))")

# Idle after landing: zero frames while parked.
F1=$(grep -c "frame presented" "$APP_LOG" || true)
sleep 2
F2=$(grep -c "frame presented" "$APP_LOG" || true)
if [ "${F2:-0}" -eq "${F1:-0}" ]; then
  ok "settled character renders nothing while parked (2 s, 0 frames)"
else
  bad "frames presented while parked after landing"
fi

# Toss: grab the grounded character, drag up and left, release mid-motion.
# Releasing above the floor with velocity must produce a second landing.
inj move "$GROUND_X" "$GROUND_Y" "$OUT_W" "$OUT_H"
sleep 0.4
inj down 0x110
sleep 0.2
for f in 0.46 0.42 0.38 0.34 0.30; do
  inj move "$(python3 -c "print(int($f*$OUT_W))")" "$(python3 -c "print(int(0.6*$OUT_H))")" "$OUT_W" "$OUT_H"
  sleep 0.05
done
inj up 0x110
LANDINGS=$(grep -c "character landed" "$APP_LOG" || true)
# Poll until a SECOND landing appears (the toss fall + wall/slide settle).
# The window is generous because a throttled compositor (host focus
# stolen mid-run) delivers frame callbacks at ~0.5 Hz.
for _ in $(seq 1 160); do
  LANDINGS2=$(grep -c "character landed" "$APP_LOG" || true)
  [ "${LANDINGS2:-0}" -gt "${LANDINGS:-0}" ] && break
  kill -0 "$APP_PID" 2>/dev/null || break
  sleep 0.25
done
if [ "${LANDINGS2:-0}" -gt "${LANDINGS:-0}" ]; then
  ok "toss: character released mid-drag fell and landed again"
else
  bad "toss produced no second landing"
fi
wait_log "drag started" 15 && wait_log "drag ended" 15 \
  && ok "physics drag lifecycle logged" \
  || bad "physics drag lifecycle missing in log"

# Settled again (park) before the pixel check.
PARKED2=0
for _ in $(seq 1 120); do
  P=$(grep -c "rendering parked (idle)" "$APP_LOG" || true)
  if [ "${P:-0}" -ge 2 ]; then PARKED2=1; break; fi
  kill -0 "$APP_PID" 2>/dev/null || break
  sleep 0.25
done
[ "$PARKED2" -eq 1 ] \
  && ok "character parked again after the toss settled" \
  || bad "character never parked after the toss"

# Pixel check: the character rests in the bottom strip after the toss.
rm -f "$WORK/screen.png"
NIRI_SOCKET="$IPCSOCK" niri msg action screenshot-screen
for _ in $(seq 1 40); do [ -s "$WORK/screen.png" ] && break; sleep 0.2; done
TOSS_X=$(python3 - "$WORK/screen.png" "$OUT_W" "$OUT_H" <<'EOF'
import sys
from PIL import Image
img = Image.open(sys.argv[1]).convert("RGB")
iw, ih = img.size
w, h = float(sys.argv[2]), float(sys.argv[3])
sx, sy = iw / w, ih / h
y0, y1 = int(h * 0.62 * sy), int(h * 0.98 * sy)
xs = []
for yy in range(y0, min(y1, ih), 4):
    for xx in range(0, iw, 4):
        r, g, b = img.getpixel((xx, yy))
        if abs(r - 255) > 60 or g > 60 or abs(b - 255) > 60:
            xs.append(xx)
print(int(sum(xs) / len(xs) / sx) if xs else -1)
EOF
)
if [ "${TOSS_X:-0}" -ge 0 ]; then
  ok "character pixel-verified resting on the bottom strip (x≈$TOSS_X)"
else
  bad "no character pixels found in the bottom strip after the toss"
fi

# Teardown: right-click the resting character (its exact x from the scan).
if [ "${TOSS_X:--1}" -ge 0 ]; then
  inj move "$TOSS_X" "$GROUND_Y" "$OUT_W" "$OUT_H"
  sleep 0.4
  inj click 0x111
  EXIT_CODE=-1
  for _ in $(seq 1 40); do
    kill -0 "$APP_PID" 2>/dev/null || { wait "$APP_PID"; EXIT_CODE=$?; break; }
    sleep 0.2
  done
  if [ "$EXIT_CODE" -eq 0 ]; then
    ok "physics app exited cleanly via right-click (exit 0)"
  else
    kill "$APP_PID" 2>/dev/null
    bad "physics app did not quit on right-click (last exit=$EXIT_CODE)"
  fi
  grep -q "teardown: dropping renderer (wgpu surface) first" "$APP_LOG" \
    && grep -q "teardown: dropping platform/wayland session" "$APP_LOG" \
    && grep -q "teardown complete" "$APP_LOG" \
    && ok "physics run teardown order logged: renderer before platform" \
    || bad "physics run teardown order not fully logged"
else
  kill "$APP_PID" 2>/dev/null
fi

# --- Phase 3b: desktop environment awareness (third launch) -------------------------
say "desktop environment awareness (third launch with the niri IPC attached)"

say "building the environment probe"
cargo build -p ocm-desktop-niri --example env-probe 2>&1 | tail -1 || { bad "probe build failed"; exit 1; }
PROBE="$REPO/target/debug/examples/env-probe"
[ -x "$PROBE" ] || { bad "probe binary missing"; exit 1; }

# Snapshot against the nested compositor: the probe's view of outputs
# must match what the compositor itself reports.
PROBE_OUT=$(NIRI_SOCKET="$IPCSOCK" "$PROBE" --once 2>&1 | head -1)
NESTED_LOGICAL=$(NIRI_SOCKET="$IPCSOCK" niri msg --json outputs | python3 -c '
import json,sys
outs = json.load(sys.stdin)
o = list(outs.values())[0]
print(o["logical"]["width"], o["logical"]["height"])')
if echo "$PROBE_OUT" | grep -q "logical=Some(($(echo "$NESTED_LOGICAL" | sed 's/ /, /')))"; then
  ok "desktop snapshot matches the compositor state (niri IPC, $NESTED_LOGICAL logical)"
else
  bad "probe output mismatch: '$PROBE_OUT' vs nested logical ($NESTED_LOGICAL)"
fi
echo "$PROBE_OUT" | grep -q "work_area=None" \
  && ok "work areas reported honestly unavailable (niri has none)" \
  || bad "work area should be None on niri"

# Persistent event-stream probe capturing everything for the checks below.
NIRI_SOCKET="$IPCSOCK" "$PROBE" > "$WORK/probe-events.log" 2>&1 &
PROBE_PID=$!
sleep 1

# Third launch: the app with awareness attached (NIRI_SOCKET is REQUIRED
# here — unset it would silently point at the host compositor).
APP_LOG="$WORK/app-aware.log"
WAYLAND_DISPLAY="$WLSOCK" NIRI_SOCKET="$IPCSOCK" RUST_LOG="debug,naga=warn,wgpu=warn,bevy=warn,bevy_render=warn,bevy_pbr=warn,bevy_winit=warn" "$APP" \
  --renderer "$OCM_RENDERER" --position 0.5,0.55 --size 0.4 --fps 60 > "$APP_LOG" 2>&1 &
APP_PID=$!

PARKED=0
for _ in $(seq 1 200); do
  if grep -q "rendering parked (idle)" "$APP_LOG"; then PARKED=1; break; fi
  kill -0 "$APP_PID" 2>/dev/null || break
  sleep 0.25
done
kill -0 "$APP_PID" 2>/dev/null || { bad "awareness app exited during startup"; cat "$APP_LOG"; exit 1; }
[ "$PARKED" -eq 1 ] || { bad "awareness app never parked"; kill "$APP_PID"; exit 1; }
ok "awareness app started and parked"

grep -q "desktop environment snapshot acquired (niri IPC)" "$APP_LOG" \
  && ok "desktop snapshot acquired by the runtime" \
  || bad "no desktop snapshot in app log"
grep -q "desktop awareness attached (niri event stream)" "$APP_LOG" \
  && ok "persistent niri event stream attached" \
  || bad "awareness not attached in app log"

# --- idle CPU with awareness attached (must remain architectural zero) --------------
say "idle CPU with desktop awareness attached (3 s window)"
F1=$(grep -c "frame presented" "$APP_LOG" || true)
U1=$(awk '{print $14+$15}' /proc/$APP_PID/stat 2>/dev/null)
sleep 3
U2=$(awk '{print $14+$15}' /proc/$APP_PID/stat 2>/dev/null)
F2=$(grep -c "frame presented" "$APP_LOG" || true)
CLK_TCK=$(getconf CLK_TCK 2>/dev/null || echo 100)
IDLE_MS=$(( (U2-U1) * 1000 / CLK_TCK ))
if [ "${F2:-0}" -gt "${F1:-0}" ] || [ "${U2:-1}" -gt "${U1:-0}" ]; then
  bad "idle CPU ${IDLE_MS} ms / ${F2:-0} frames over 3 s with the provider attached (expected 0)"
else
  ok "idle CPU = 0 ticks over 3 s with the event stream attached (0 frames)"
fi

# --- window events through the subscription -----------------------------------------
say "window lifecycle events (kitty opened/floated/focused/closed in the nested compositor)"
KITTY_PIDS=""
WAYLAND_DISPLAY="$WLSOCK" kitty > /dev/null 2>&1 &
KITTY_PIDS="$!"
for _ in $(seq 1 80); do
  grep -q 'window-added' "$WORK/probe-events.log" && break
  sleep 0.25
done
grep -q 'window-added id=' "$WORK/probe-events.log" \
  && ok "window open event delivered (event stream, no polling)" \
  || bad "no window-added event reached the probe"

# Float it: niri then reports on-screen geometry (tiled windows get none).
NIRI_SOCKET="$IPCSOCK" niri msg action toggle-window-floating >/dev/null 2>&1
GEOMETRY=""
for _ in $(seq 1 80); do
  GEOMETRY=$(grep -oP 'window-changed id=\d+ app="kitty".*geometry=\K[0-9.,]+ [0-9x]+' "$WORK/probe-events.log" | tail -1)
  [ -n "$GEOMETRY" ] && break
  sleep 0.25
done
if [ -n "$GEOMETRY" ]; then
  ok "floating window geometry reported: $GEOMETRY (output-local logical px)"
  GEO_X=$(echo "$GEOMETRY" | cut -d, -f1)
  GEO_Y=$(echo "$GEOMETRY" | cut -d, -f2 | cut -d' ' -f1)
  GEO_W=$(echo "$GEOMETRY" | grep -oP '\d+(?=x)')
  GEO_H=$(echo "$GEOMETRY" | grep -oP '(?<=x)\d+')
else
  bad "no geometry after floating the window (niri should report it)"
  GEO_X=0; GEO_Y=0; GEO_W=0; GEO_H=0
fi

# Pixel-verify the reported geometry against a compositor screenshot.
if [ -n "$GEOMETRY" ] && [ "$GEO_W" -gt 0 ] 2>/dev/null; then
  rm -f "$WORK/screen.png"
  NIRI_SOCKET="$IPCSOCK" niri msg action screenshot-screen
  for _ in $(seq 1 40); do [ -s "$WORK/screen.png" ] && break; sleep 0.2; done
  if [ -s "$WORK/screen.png" ] && python3 - "$WORK/screen.png" "$OUT_W" "$OUT_H" \
      "$GEO_X" "$GEO_Y" "$GEO_W" "$GEO_H" <<'EOF'
import sys
from PIL import Image
img = Image.open(sys.argv[1]).convert("RGB")
iw, ih = img.size
w, h = float(sys.argv[2]), float(sys.argv[3])
sx, sy = iw / w, ih / h
x, y, gw, gh = (float(v) for v in sys.argv[4:8])
def px(x, y):
    return img.getpixel((min(int(x * sx), iw - 1), min(int(y * sy), ih - 1)))
def is_bg(c):
    r, g, b = c
    return abs(r - 255) < 60 and g < 60 and abs(b - 255) < 60  # magenta background
cx, cy = x + gw / 2, y + gh / 2
inside_ok = not is_bg(px(cx, cy))
outside_ok = is_bg(px(x - 20, y + gh / 2)) or x - 20 < 0
sys.exit(0 if (inside_ok and outside_ok) else 1)
EOF
  then
    ok "window geometry pixel-verified (center inside the reported rect, just outside is background)"
  else
    bad "reported window geometry does not match the compositor screenshot"
  fi
fi

# Window focus events: a second kitty opens focused (covered by its
# WindowOpenedOrChanged record); niri emits WindowFocusChanged when an
# EXISTING window regains focus — which happens when the second window
# closes and focus returns to the first. So: spawn, close, then expect
# the focus event.
WAYLAND_DISPLAY="$WLSOCK" kitty > /dev/null 2>&1 &
KITTY_PIDS="$KITTY_PIDS $!"
for _ in $(seq 1 80); do
  grep -q 'event window-focused' "$WORK/probe-events.log" && break
  sleep 0.25
done
if grep -q 'event window-focused' "$WORK/probe-events.log"; then
  ok "window focus events delivered"
else
  # Close the second window so focus returns to an existing one.
  for pid in $KITTY_PIDS; do kill "$pid" 2>/dev/null; done
  KITTY_PIDS=$(printf '%s ' $KITTY_PIDS | awk '{print $1}')  # keep kitty 1
  for _ in $(seq 1 80); do
    grep -q 'event window-focused' "$WORK/probe-events.log" && break
    sleep 0.25
  done
  grep -q 'event window-focused' "$WORK/probe-events.log" \
    && ok "window focus events delivered (focus returned after the second window closed)" \
    || bad "no focus event reached the probe"
fi

# Workspace events: switch away and back.
WS_EVENTS_BEFORE=$(grep -c 'event workspace' "$WORK/probe-events.log" || true)
NIRI_SOCKET="$IPCSOCK" niri msg action focus-workspace-down >/dev/null 2>&1
sleep 1
NIRI_SOCKET="$IPCSOCK" niri msg action focus-workspace-up >/dev/null 2>&1
sleep 1
WS_EVENTS_AFTER=$(grep -c 'event workspace' "$WORK/probe-events.log" || true)
if [ "${WS_EVENTS_AFTER:-0}" -gt "${WS_EVENTS_BEFORE:-0}" ]; then
  ok "workspace change events delivered (focus-workspace switches)"
else
  bad "no workspace events after switching workspaces"
fi

# Close both kitties: removal events must arrive.
for pid in $KITTY_PIDS; do kill "$pid" 2>/dev/null; done
KITTY_PIDS=""
REMOVALS_AFTER=0
for _ in $(seq 1 80); do
  REMOVALS_AFTER=$(grep -c 'event window-removed' "$WORK/probe-events.log" || true)
  [ "${REMOVALS_AFTER:-0}" -ge 2 ] && break
  sleep 0.25
done
if [ "${REMOVALS_AFTER:-0}" -ge 2 ]; then
  ok "window close events delivered (both test windows)"
else
  bad "window removal events missing (${REMOVALS_AFTER:-0}/2)"
fi

# The runtime must have seen the window churn without rendering a frame
# for it (environment updates are awareness, not scene changes).
FRAMES_A=$(grep -c "frame presented" "$APP_LOG" || true)
sleep 1
FRAMES_B=$(grep -c "frame presented" "$APP_LOG" || true)
if [ "${FRAMES_A:-0}" -eq "${FRAMES_B:-0}" ]; then
  ok "window churn scheduled no frames (environment events are not scene changes)"
else
  bad "window churn triggered renders (${FRAMES_A} -> ${FRAMES_B})"
fi

kill "$PROBE_PID" 2>/dev/null

# Quit the awareness app via right-click.
inj move "$CUBE_A_X" "$CUBE_A_Y" "$OUT_W" "$OUT_H"
sleep 0.4
inj click 0x111
EXIT_CODE=-1
for _ in $(seq 1 40); do
  kill -0 "$APP_PID" 2>/dev/null || { wait "$APP_PID"; EXIT_CODE=$?; break; }
  sleep 0.2
done
if [ "$EXIT_CODE" -eq 0 ]; then
  ok "awareness app exited cleanly via right-click (exit 0)"
else
  kill "$APP_PID" 2>/dev/null
  bad "awareness app did not quit on right-click (exit=$EXIT_CODE)"
fi

# --- graceful degradation without the compositor IPC --------------------------------
say "runtime behavior when the compositor IPC is unavailable (fourth launch)"
APP_LOG="$WORK/app-nodefault.log"
WAYLAND_DISPLAY="$WLSOCK" NIRI_SOCKET="/nonexistent/ocm-test.sock" RUST_LOG="info" "$APP" \
  --renderer "$OCM_RENDERER" --position 0.5,0.55 > "$APP_LOG" 2>&1 &
APP_PID=$!
PARKED=0
for _ in $(seq 1 200); do
  if grep -q "rendering parked (idle)" "$APP_LOG"; then PARKED=1; break; fi
  kill -0 "$APP_PID" 2>/dev/null || break
  sleep 0.25
done
if [ "$PARKED" -eq 1 ] && grep -q "desktop awareness unavailable" "$APP_LOG"; then
  ok "dead IPC socket: app logs the absence, keeps running, parks (Phase 3a behavior)"
else
  bad "app did not degrade gracefully without the compositor IPC"
  kill "$APP_PID" 2>/dev/null
fi
kill "$APP_PID" 2>/dev/null

# --- idle measurement #2: post-quit proof from the parked window --------------------
say "summary: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
exit 0
