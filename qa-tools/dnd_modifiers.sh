#!/bin/sh
# Modifier keys during real drags (bug B11), checked: exits non-zero when a scenario fails.
#   1. external drag, no key: the drop lands on the shelf, no wheel
#   2. external drag, Shift tapped with the pointer still: Convert wheel, stays open; drop on JPG
#   3. external drag, Ctrl tapped: Tools wheel; drop on its top slice (Compress)
#   4. external drag, Alt held: Actions wheel; drop on the hub cancels
#   5. a shelf row dragged inside the window, Shift tapped: Convert wheel; drop on JPG
# The source (qa-tools/drag_source.py) offers copy and move like a file manager, so on X11 Shift
# turns the offer into move-only: the drop target must still take it.
# Run: xvfb-run -a -s "-screen 0 900x800x24" sh qa-tools/gui_session.sh qa-tools/dnd_modifiers.sh
set -u
QA=$(cd "$(dirname "$0")/.." && pwd)
SHOTS=$QA/shots; mkdir -p "$SHOTS"
WORK=$(mktemp -d); export XDG_CONFIG_HOME=$WORK/config XDG_CACHE_HOME=$WORK/cache SUSEN_DEBUG_DND=1
IN=$WORK/in; mkdir -p "$IN"
convert -size 64x48 gradient:orange-white "$IN/photo.png"
"$QA/target/debug/susen" "$IN/photo.png" >> "$WORK/log" 2>&1 &  # append: keeps the marks
APP=$!
python3.12 "$QA/qa-tools/drag_source.py" "$IN/photo.png" > "$WORK/src.log" 2>&1 &
SRC=$!
sleep 3
SW=
for i in $(seq 1 40); do SW=$(xdotool search --pid $SRC 2>/dev/null | tail -1); [ -n "$SW" ] && break; sleep 0.25; done
[ -n "$SW" ] || { echo "FAIL setup: the drag source window never appeared"; kill $APP $SRC; exit 1; }
sleep 0.5; xdotool windowmove "$SW" 590 40; sleep 0.5
FAIL=0
shot() { import -window root "$SHOTS/mods-$1.png"; }
mark() { echo "=== $1" >> "$WORK/log"; }
# Lines of the log written since the last mark.
since() { awk -v m="=== $1" '$0 == m { on = 1; next } /^=== / { on = 0 } on' "$WORK/log"; }
check() { # $1 name, $2 description, then a command
    name=$1 what=$2; shift 2
    if "$@"; then echo "PASS $name: $what"; else echo "FAIL $name: $what"; FAIL=1; fi
}
files() { ls "$IN" | sort; }
logged() { since "$1" | grep -qF -- "$2"; } # $1 section, $2 text
unlogged() { ! logged "$1" "$2"; }
# Picks up the label of the drag source and brings it over Susen at ($1, $2).
grab_external() {
    xdotool mousemove 690 100; sleep 0.3; xdotool mousedown 1; sleep 0.2
    for i in 1 2 3 4 5; do xdotool mousemove_relative -- -10 5; sleep 0.05; done
    xdotool mousemove 400 300; sleep 0.3; xdotool mousemove "$1" "$2"; sleep 0.3
    xdotool mousemove $(( $1 + 2 )) $(( $2 + 2 )); sleep 0.5
}
tap() { xdotool keydown "$1"; sleep 0.2; xdotool keyup "$1"; sleep 0.5; }
wait_for() { # $1 file name in $IN
    for i in $(seq 1 40); do [ -e "$IN/$1" ] && return 0; sleep 0.25; done; return 1
}

mark plain
before=$(files)
grab_external 280 330; shot 1-plain-hover
xdotool mouseup 1; sleep 1.5
check plain "no wheel opened" unlogged plain 'wheel=Some'
check plain "dropped without a wheel" logged plain 'drop chosen=None'
check plain "no file was written" test "$before" = "$(files)"

mark shift
grab_external 280 330
tap shift
shot 2-shift-tapped
check shift "the tap opened the Convert wheel" logged shift 'dnd: modifier Convert'
# Key released: the wheel must stay. Move onto the top slice (JPG) and drop.
xdotool mousemove 282 230; sleep 0.3; xdotool mousemove 283 228; sleep 0.5; shot 2-shift-slice
check shift "the wheel stays open after the key is released" logged shift 'mods=ModifierType(0x0) wheel=Some(Convert)'
xdotool mouseup 1
check shift "photo.jpg written" wait_for photo.jpg
sleep 1; shot 2-shift-done

mark ctrl
before=$(files)
grab_external 280 330
tap ctrl
shot 3-ctrl-tapped
check ctrl "the tap opened the Tools wheel" logged ctrl 'dnd: modifier Tools'
xdotool mousemove 282 230; sleep 0.3; xdotool mousemove 283 228; sleep 0.5; shot 3-ctrl-slice
xdotool mouseup 1
check ctrl "dropped on a tool" logged ctrl 'drop chosen=Some(Some(Tool'
sleep 4; shot 3-ctrl-done
check ctrl "the tool wrote a new file" test "$before" != "$(files)"

mark alt
before=$(files)
xdotool mousemove 690 100; sleep 0.3; xdotool mousedown 1; sleep 0.2
for i in 1 2 3 4 5; do xdotool mousemove_relative -- -10 5; sleep 0.05; done
xdotool mousemove 400 300; sleep 0.3; xdotool keydown alt
xdotool mousemove 280 330; sleep 0.3; xdotool mousemove 282 332; sleep 0.5; shot 4-alt-held
xdotool mouseup 1; sleep 0.3; xdotool keyup alt; sleep 1
check alt "Alt opened the Actions wheel" logged alt 'wheel=Some(Actions)'
check alt "a drop on the hub cancels" logged alt 'drop chosen=Some(None)'
check alt "nothing was written" test "$before" = "$(files)"

mark shelf
rm -f "$IN/photo.jpg"
# The first shelf row is photo.png (passed on the command line).
xdotool mousemove 100 75; sleep 0.3; xdotool mousedown 1; sleep 0.2
for i in 1 2 3 4 5; do xdotool mousemove_relative -- 10 20; sleep 0.05; done
xdotool mousemove 280 330; sleep 0.3; xdotool mousemove 282 332; sleep 0.5
tap shift
shot 5-shelf-tapped
check shelf "the tap opened the Convert wheel" logged shelf 'dnd: modifier Convert'
xdotool mousemove 282 230; sleep 0.3; xdotool mousemove 283 228; sleep 0.5; shot 5-shelf-slice
xdotool mouseup 1
check shelf "photo.jpg written" wait_for photo.jpg
sleep 1; shot 5-shelf-done

kill $APP $SRC 2>/dev/null
if [ $FAIL -ne 0 ]; then
    echo "--- susen log"; grep -v libEGL "$WORK/log" | uniq -c
    echo "--- files"; files
fi
exit $FAIL
