#!/bin/sh
# Real drag and drop onto Susen under Xvfb: a GTK 4 drag source window offers a file; xdotool
# drags it onto Susen: plain, holding Shift, holding Ctrl, and over the Convert chip.
# A real file manager offers copy and move; pass --copy-only through SRC_FLAGS to offer copy only.
# Run: xvfb-run -a -s "-screen 0 800x700x24" sh qa-tools/gui_session.sh qa-tools/dnd.sh
set -u
QA=$(cd "$(dirname "$0")/.." && pwd)
SHOTS=$QA/shots; mkdir -p "$SHOTS"
WORK=$(mktemp -d); export XDG_CONFIG_HOME=$WORK/config XDG_CACHE_HOME=$WORK/cache
convert -size 64x48 gradient:orange-white "$WORK/photo.png"
shot() { import -window root "$SHOTS/$1.png"; echo "shot $1"; }
"$QA/target/debug/susen" > "$WORK/log" 2>&1 &
APP=$!
python3.12 "$QA/qa-tools/drag_source.py" ${SRC_FLAGS:-} "$WORK/photo.png" > "$WORK/src.log" 2>&1 &
SRC=$!
sleep 3
SW=$(xdotool search --pid $SRC | tail -1)
xdotool windowmove "$SW" 590 40; sleep 0.5
# drag from the source label (690,100) to a point inside Susen
drag() { # $1 x, $2 y, $3 extra key (or none), $4 name
    xdotool mousemove 690 100; sleep 0.3
    xdotool mousedown 1; sleep 0.2
    for i in 1 2 3 4 5; do xdotool mousemove_relative -- -10 5; sleep 0.05; done
    xdotool mousemove 400 300; sleep 0.3
    [ "$3" != none ] && xdotool keydown "$3"
    xdotool mousemove "$1" "$2"; sleep 0.3
    xdotool mousemove $(( $1 + 3 )) $(( $2 + 3 )); sleep 0.8
    shot "$4-hover"
    xdotool mouseup 1; sleep 0.3
    [ "$3" != none ] && xdotool keyup "$3"
    sleep 3; shot "$4-dropped"
    echo "after $4:"; ls "$WORK"
}
shot dnd-00-start
drag 280 300 none dnd-1-plain
# with Shift: wheel opens at the pointer; drop on the top slice (JPG) ~105 px above the centre
drag 280 330 shift dnd-2-shift-center
drag 280 225 shift dnd-3-shift-slice
drag 280 225 ctrl dnd-4-ctrl-slice
# over the Convert chip (bottom left) without a key, then up onto the wheel's top slice
chip() {
    xdotool mousemove 690 100; sleep 0.3
    xdotool mousedown 1; sleep 0.2
    for i in 1 2 3 4 5; do xdotool mousemove_relative -- -10 5; sleep 0.05; done
    xdotool mousemove 400 300; sleep 0.3
    xdotool mousemove 176 582; sleep 0.3; xdotool mousemove 178 583; sleep 0.8
    shot dnd-5-chip-hover
    xdotool mousemove 178 470; sleep 0.3; xdotool mousemove 179 468; sleep 0.8
    shot dnd-5-chip-slice
    xdotool mouseup 1; sleep 3; shot dnd-5-chip-dropped
    echo "after chip:"; ls "$WORK"
}
chip
# a shelf row dragged inside the window, Shift tapped mid-drag: the README says this opens the wheel
xdotool mousemove 100 75; sleep 0.3; xdotool mousedown 1; sleep 0.2
for i in 1 2 3 4 5; do xdotool mousemove_relative -- 10 20; sleep 0.05; done
xdotool mousemove 280 300; sleep 0.3; xdotool keydown shift; sleep 0.2; xdotool keyup shift
xdotool mousemove 282 302; sleep 0.5; shot dnd-6-shelf-shift-hover
xdotool mousemove 282 200; sleep 0.5; shot dnd-6-shelf-shift-slice
xdotool mouseup 1; sleep 3; shot dnd-6-shelf-dropped
echo "after shelf drag:"; ls "$WORK"
echo "--- susen log"; grep -v libEGL "$WORK/log"
echo "--- source log"; cat "$WORK/src.log"
kill $APP $SRC
