#!/bin/sh
# Runs a command inside a fresh Xvfb with Susen's GTK settings for screenshots.
# Usage: xvfb-run -a -s "-screen 0 800x700x24" qa-tools/gui_session.sh SCRIPT
export GDK_BACKEND=x11 GSK_RENDERER=cairo NO_AT_BRIDGE=1 GTK_A11Y=none
exec sh "$@"
