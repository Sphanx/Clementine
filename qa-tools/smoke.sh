#!/bin/sh
# GUI smoke test, keyboard only: shelf, wheel, Tab, run a conversion, Delete, settings.
# Run: xvfb-run -a -s "-screen 0 800x700x24" sh qa-tools/gui_session.sh qa-tools/smoke.sh
set -u
QA=$(cd "$(dirname "$0")/.." && pwd)
SHOTS=$QA/shots; mkdir -p "$SHOTS"
WORK=$(mktemp -d); export XDG_CONFIG_HOME=$WORK/config XDG_CACHE_HOME=$WORK/cache
convert -size 64x48 gradient:orange-white "$WORK/photo.png"
"$QA/target/debug/clementine" "$WORK/photo.png" > "$WORK/log" 2>&1 &
APP=$!
sleep 3
WID=$(xdotool search --pid $APP | tail -1)
echo "window: $WID"; xdotool getwindowgeometry "$WID"
shot() { import -window root "$SHOTS/$1.png"; echo "shot $1"; }
shot 01-start
# Focus: click on the shelf row (window has no WM; click gives focus).
eval "$(xdotool getwindowgeometry --shell "$WID")"
xdotool mousemove $((X + 100)) $((Y + 90)) click 1; sleep 0.5
shot 02-clicked
xdotool key Return; sleep 0.7; shot 03-wheel
xdotool key Tab; sleep 0.5; shot 04-tools
xdotool key Tab; sleep 0.5; shot 05-actions
xdotool key Tab; sleep 0.5; xdotool key Right; sleep 0.3; shot 06-convert-right
xdotool key Return; sleep 4; shot 07-ran
ls "$WORK"
xdotool mousemove $((X + 100)) $((Y + 90)) click 1; sleep 0.3
xdotool key Delete; sleep 0.5; shot 08-deleted
echo "--- app log"; cat "$WORK/log"
kill $APP
