#!/bin/bash
# Starts the installed codenotch on a display with nobody at it, and fails
# unless it is still up, listening and has placed its notch. For a container:
# it starts its own display, as an ordinary user.
#
#   smoke-test.sh [directory for the screenshot and logs]
#
# The display is Hyprland, where the notch has to come up as a layer surface.
# Hyprland wants a display device (/dev/dri/card*): on a machine without a GPU
# `modprobe vkms` on the host gives it one.
set -u

out=${1:-$(mktemp -d)}
mkdir -p "$out"
# An empty home, as on a first run, in a folder of its own: the directory named
# above only ever has files added to it.
scratch=$(mktemp -d) || exit 1
export HOME=$scratch/home XDG_RUNTIME_DIR=$scratch/run
export XDG_CONFIG_HOME=$HOME/.config
mkdir -p "$XDG_CONFIG_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
unset DISPLAY WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE
# No GPU here.
export LIBGL_ALWAYS_SOFTWARE=1

log=$XDG_CONFIG_HOME/codenotch/run.log
pids=()

fail() {
  echo "FAIL: $*" >&2
  for f in "$log" "$out/codenotch.out" "$out/display.log"; do
    [ -s "$f" ] && { echo "--- $f" >&2; tail -n 40 "$f" >&2; }
  done
  [ -s "$log" ] && cp "$log" "$out/run.log"
  exit 1
}

# wait_for <seconds> <command...>: until the command succeeds.
wait_for() {
  local tries=$(($1 * 5))
  shift
  while ! "$@" >/dev/null 2>&1; do
    tries=$((tries - 1))
    [ "$tries" -gt 0 ] || return 1
    sleep 0.2
  done
}

cleanup() {
  if [ ${#pids[@]} -gt 0 ]; then
    kill "${pids[@]}" 2>/dev/null
    # Not everything here leaves when asked, and the wait below has no end.
    sleep 1
    kill -9 "${pids[@]}" 2>/dev/null
  fi
  wait 2>/dev/null
  rm -rf "$scratch"
}
trap cleanup EXIT

command -v codenotch >/dev/null || fail "codenotch is not installed"
# Install hooks looks for the helper beside the app.
[ -x "$(dirname "$(command -v codenotch)")/codenotch-hook" ] || fail "codenotch-hook is not beside codenotch"
codenotch doctor >"$out/doctor-before.txt" 2>&1 || fail "codenotch doctor failed"
grep -q 'port: free' "$out/doctor-before.txt" || fail "the port is taken before the app starts"

command -v Hyprland >/dev/null || fail "Hyprland is not installed"
compgen -G '/dev/dri/card*' >/dev/null || fail "no display device in /dev/dri: modprobe vkms on the host"

# No login session in a container: libseat opens the device itself.
export LIBSEAT_BACKEND=noop
cat >"$out/hyprland.lua" <<'LUA'
hl.monitor({ output = "", mode = "1920x1080@60", position = "auto", scale = 1 })
hl.config({ misc = { disable_hyprland_logo = true, force_default_wallpaper = 0 } })
LUA
Hyprland -c "$out/hyprland.lua" >"$out/display.log" 2>&1 &
pids+=($!)
signature() { ls "$XDG_RUNTIME_DIR/hypr" 2>/dev/null | head -n1; }
started() { [ -n "$(signature)" ]; }
wait_for 30 started || fail "Hyprland did not start"
HYPRLAND_INSTANCE_SIGNATURE=$(signature)
export HYPRLAND_INSTANCE_SIGNATURE
wait_for 30 hyprctl monitors || fail "Hyprland does not answer"
WAYLAND_DISPLAY=$(basename "$(ls "$XDG_RUNTIME_DIR"/wayland-* | grep -v '\.lock$' | head -n1)")
export WAYLAND_DISPLAY
wait_for 10 sh -c "hyprctl monitors | grep -q '^Monitor'" || fail "Hyprland has no monitor"

codenotch >"$out/codenotch.out" 2>&1 &
app=$!
pids+=("$app")

listening() { codenotch doctor 2>/dev/null | grep -q 'port: in use'; }
wait_for 60 listening || fail "codenotch is not listening after 60 s"
wait_for 60 grep -q 'notch placed' "$log" || fail "run.log has no 'notch placed' line"

grep -q 'hyprland: the notch is a layer surface' "$log" || fail "the notch is not a layer surface"
wait_for 30 sh -c "hyprctl layers | grep -q 'namespace: codenotch'" || fail "Hyprland lists no codenotch layer"
hyprctl layers >"$out/layers.txt"
# The page needs a moment after the surface is there. Only the monitor the
# notch is on, and not for ever: a machine's own display device is a second
# monitor that may never draw a frame, and grim waits for one from each.
sleep 5
monitor=$(sed -n 's/.*notch placed.*monitor=Some("\([^"]*\)").*/\1/p' "$log" | head -n1)
if command -v grim >/dev/null; then
  timeout 20 grim ${monitor:+-o "$monitor"} "$out/screenshot.png" || echo "no screenshot: grim gave up on ${monitor:-the display}"
fi

kill -0 "$app" 2>/dev/null || fail "codenotch exited"
cp "$log" "$out/run.log"
grep -E 'notch placed|hyprland:' "$log" | head -n 5
echo "PASS: codenotch $(pacman -Q codenotch-git 2>/dev/null | cut -d' ' -f2) is a layer surface on Hyprland $(hyprctl version | sed -n 's/^Hyprland \([0-9.]*\).*/\1/p')"
