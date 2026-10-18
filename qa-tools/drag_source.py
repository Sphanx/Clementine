#!/usr/bin/env python3.12
"""A tiny GTK 4 window whose label can be dragged: it offers the files given on the command
line (copy or move, like a file manager; `--copy-only` for copy only) as a file list. Used to
drive a real drag and drop onto Susen under Xvfb with xdotool."""
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, Gio, GObject, Gtk  # noqa: E402

files = [Gio.File.new_for_path(p) for p in sys.argv[1:] if not p.startswith("--")]


def prepare(_source, _x, _y):
    value = GObject.Value(Gdk.FileList, Gdk.FileList.new_from_array(files))
    return Gdk.ContentProvider.new_for_value(value)


def activate(app):
    win = Gtk.ApplicationWindow(application=app, title="drag-source")
    win.set_default_size(200, 120)
    label = Gtk.Label(label="DRAG ME")
    label.set_vexpand(True)
    actions = Gdk.DragAction.COPY | (Gdk.DragAction.MOVE if "--copy-only" not in sys.argv else 0)
    source = Gtk.DragSource(actions=actions)
    source.connect("prepare", prepare)
    source.connect("drag-end", lambda *_: print("drag-end", flush=True))
    label.add_controller(source)
    win.set_child(label)
    win.present()


app = Gtk.Application(application_id="io.github.sphanx.SusenQaDragSource")
app.connect("activate", activate)
app.run([])
