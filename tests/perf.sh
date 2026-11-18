#!/usr/bin/env bash
# Repeatable renderer performance comparison (nested Niri, focused on the
# host for real-time pacing). Same harness for both backends.
#
# Measures, per backend: startup-to-first-frame time, idle CPU (parked),
# active CPU + GPU while animating at the requested fps, RSS, and frame
# pacing (log timestamps of consecutive presents).
#
# usage: tests/perf.sh [--release]
set -u
REPO=$(cd "$(dirname "$0")/.." && pwd)
PROFILE=debug
[ "${1:-}" = "--release" ] && PROFILE=release
APP="$REPO/target/$PROFILE/ocm-runtime"
CLK=$(getconf CLK_TCK)
WORK=$(mktemp -d /tmp/opencode/ocm-perf-XXXXXX)

measure_backend() {
  local NAME=$1
  niri -c "$WORK/niri.kdl" > "$WORK/nested.log" 2>&1 &
  local NPID=$!
  for _ in $(seq 1 30); do
    grep -q 'listening on Wayland socket' "$WORK/nested.log" && break
    sleep 0.2
  done
  local WLSOCK
  WLSOCK=$(grep -oP 'listening on Wayland socket: \K\S+' "$WORK/nested.log" | head -1)
  sleep 1
  niri msg action focus-window --id "$(niri msg --json windows |
    python3 -c 'import json,sys; ws=json.load(sys.stdin); w=[w for w in ws if w["title"]=="niri"]; print(w[0]["id"] if w else "")')" \
    >/dev/null 2>&1

  # animated run: startup + active CPU/GPU + pacing
  WAYLAND_DISPLAY="$WLSOCK" \
    RUST_LOG="debug,naga=warn,wgpu=warn,bevy=warn,bevy_render=warn,bevy_pbr=warn" \
    "$APP" --renderer "$NAME" --animate --fps 60 --position 0.5,0.55 --size 0.4 \
    > "$WORK/anim.log" 2>&1 &
  local PID=$!
  sleep 4
  local A0
  A0=$(awk '{print $14+$15}' /proc/$PID/stat)
  nvidia-smi pmon -c 5 2>/dev/null | grep -E "(^| )$PID " > "$WORK/gpu.txt" &
  local GPID=$!
  sleep 10
  local A1 RSS
  A1=$(awk '{print $14+$15}' /proc/$PID/stat)
  wait $GPID 2>/dev/null
  RSS=$(awk '/VmRSS/{print $2}' /proc/$PID/status)
  kill $PID 2>/dev/null; wait $PID 2>/dev/null

  # parked run: idle CPU
  WAYLAND_DISPLAY="$WLSOCK" \
    RUST_LOG="debug,naga=warn,wgpu=warn,bevy=warn,bevy_render=warn,bevy_pbr=warn" \
    "$APP" --renderer "$NAME" --position 0.5,0.55 --size 0.4 \
    > "$WORK/idle.log" 2>&1 &
  PID=$!
  sleep 4
  local I0
  I0=$(awk '{print $14+$15}' /proc/$PID/stat)
  sleep 10
  local I1
  I1=$(awk '{print $14+$15}' /proc/$PID/stat)
  kill $PID 2>/dev/null; wait $PID 2>/dev/null
  kill $NPID 2>/dev/null; wait $NPID 2>/dev/null

  python3 - "$NAME" "$CLK" "$A0" "$A1" "$I0" "$I1" "$RSS" \
    "$WORK/anim.log" "$WORK/gpu.txt" <<'EOF'
import re
import statistics
import sys
from datetime import datetime

name, clk, a0, a1, i0, i1, rss = sys.argv[1:8]
a0, a1, i0, i1 = map(int, (a0, a1, i0, i1))


def cpu_ms_per_s(t0, t1, window_s):
    return (t1 - t) * 1000.0 / int(clk) / window_s if (t := t0) is not None else 0


def cpu(t0, t1, window_s):
    return (t1 - t0) * 1000.0 / int(clk) / window_s


def parse(path):
    start = loop = None
    presents = []
    for line in open(path):
        m = re.match(r"(\S+)", line)
        if not m:
            continue
        t = datetime.fromisoformat(m.group(1).rstrip("Z"))
        if "starting OpenCuteMate" in line and start is None:
            start = t
        if "event loop running" in line and loop is None:
            loop = t
        if "frame presented" in line:
            presents.append(t)
    return start, loop, presents


def gpu_sm(path):
    vals = []
    for line in open(path):
        parts = line.split()
        if len(parts) >= 4 and parts[3] != "-":
            try:
                vals.append(float(parts[3]))
            except ValueError:
                pass
    return sum(vals) / len(vals) if vals else None


s, l, pr = parse(sys.argv[8])
presents = pr[-60:]
deltas = sorted((b - a).total_seconds() * 1000 for a, b in zip(presents, presents[1:]))
print(f"== {name} ==")
if s and l:
    print(f"startup (process start -> first frame): {(l - s).total_seconds():.2f} s")
print(f"active CPU:  {cpu(a0, a1, 10):7.1f} ms/s  (60 fps animation, 10 s window)")
print(f"idle CPU:    {cpu(i0, i1, 10):7.1f} ms/s  (parked, 10 s window)")
sm = gpu_sm(sys.argv[9])
print(f"active GPU: {sm if sm is not None else 'n/a'}% sm (nvidia-smi pmon, 5 samples)")
print(f"RSS: {int(int(rss) / 1024)} MB")
if deltas:
    print(
        f"pacing: mean {statistics.mean(deltas):.1f} ms, "
        f"p50 {deltas[len(deltas) // 2]:.1f} ms, p95 {deltas[int(len(deltas) * 0.95)]:.1f} ms"
    )
print()
EOF
}

cat > "$WORK/niri.kdl" <<EOF
input { keyboard { xkb { layout "us" } } }
layout { background-color "#FF00FF" }
EOF

measure_backend wgpu
measure_backend bevy
rm -rf "$WORK"
