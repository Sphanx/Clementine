#!/bin/sh
# Variant of dnd.sh: the modifier is held from before the drag starts to after the drop.
set -u
QA=$(cd "$(dirname "$0")/.." && pwd)
SHOTS=$QA/shots; mkdir -p "$SHOTS"
WORK=$(mktemp -d); export XDG_CONFIG_HOME=$WORK/config XDG_CACHE_HOME=$WORK/cache
convert -size 64x48 gradient:orange-white "$WORK/photo.png"
"$QA/target/debug/susen" > "$WORK/log" 2>&1 &
APP=$!
python3.12 "$QA/qa-tools/drag_source.py" ${SRC_FLAGS:-} "$WORK/photo.png" > "$WORK/src.log" 2>&1 &
SRC=$!
sleep 3
xdotool windowmove "$(xdotool search --pid $SRC | tail -1)" 590 40; sleep 0.5
for key in shift ctrl; do
    xdotool mousemove 690 100; sleep 0.3
    xdotool keydown $key; sleep 0.2
    xdotool mousedown 1; sleep 0.2
    for i in 1 2 3 4 5; do xdotool mousemove_relative -- -10 5; sleep 0.05; done
    xdotool mousemove 400 300; sleep 0.3; xdotool mousemove 280 300; sleep 0.3; xdotool mousemove 282 302; sleep 0.8
    import -window root "$SHOTS/dnd-keyfirst-$key-hover.png"
    xdotool mousemove 282 200; sleep 0.5
    xdotool mouseup 1; sleep 0.3; xdotool keyup $key; sleep 3
    echo "after $key:"; ls "$WORK" | grep photo
done
echo "--- source log"; cat "$WORK/src.log"
kill $APP $SRC
