# Susen for GNOME Files (Nautilus) and Caja.
#
# Adds a "Susen" submenu to the right-click menu of local files:
#   Open in Susen wheel…   -> susen --wheel FILES
#   Convert to ▸ PNG, JPG, …    -> susen --convert png --notify FILES
#   Tools ▸ Compress, Zip, …    -> susen --tool compress --notify FILES
# The formats and tools are whatever `susen --list FILES` reports, so the menu always
# matches the wheel. Tools that need settings (Trim…, Resize…) are left to the wheel.
#
# Install (system): /usr/share/nautilus-python/extensions/  (needs python3-nautilus)
#                   /usr/share/caja-python/extensions/      (needs python3-caja)
# Install (user):   ~/.local/share/nautilus-python/extensions/
# Then: nautilus -q   (or caja -q)
#
# Set SUSEN_COMMAND to override the command, e.g. "flatpak run io.github.sphanx.Susen".

import os
import shlex
import shutil
import subprocess
from collections import OrderedDict
from urllib.parse import unquote, urlparse

import gi


def _host():
    """Which file manager loaded us. Both may be installed, so ask the process, not gi."""
    try:
        with open("/proc/self/comm") as f:
            name = f.read().strip()
    except OSError:
        name = ""
    return "Caja" if name.startswith("caja") else "Nautilus"


HOST = _host()
if HOST == "Caja":
    gi.require_version("Caja", "2.0")
    from gi.repository import Caja as FM, GObject  # noqa: E402
else:
    try:
        gi.require_version("Nautilus", "4.0")
    except ValueError:
        gi.require_version("Nautilus", "3.0")
    from gi.repository import Nautilus as FM, GObject  # noqa: E402

APP_ID = "io.github.sphanx.Susen"
LIST_TIMEOUT = 3  # seconds; the menu is built on the UI thread
CACHE_SIZE = 32


def find_command():
    """The command that runs Susen, as a list. Empty when it is not installed."""
    custom = os.environ.get("SUSEN_COMMAND")
    if custom:
        return shlex.split(custom)
    found = shutil.which("susen")
    if found:
        return [found]
    if shutil.which("flatpak"):
        try:
            ok = subprocess.run(
                ["flatpak", "info", APP_ID],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=LIST_TIMEOUT,
            ).returncode == 0
        except (OSError, subprocess.TimeoutExpired):
            ok = False
        if ok:
            return ["flatpak", "run", APP_ID]
    return []


def parse_list(text):
    """Turns `susen --list` output into (id, label, needs_params, missing) tuples."""
    options = []
    for line in text.splitlines():
        fields = line.split("\t")
        if len(fields) < 2 or ":" not in fields[0]:
            continue
        extra = fields[2:]
        needs_params = "params" in extra
        missing = next((e for e in extra if e.startswith("unavailable")), None)
        options.append((fields[0], fields[1], needs_params, missing))
    return options


def local_paths(files):
    """Local paths of the selection, or None when any item is not a local file."""
    paths = []
    for f in files:
        uri = f.get_uri()
        if not uri.startswith("file://"):
            return None
        paths.append(unquote(urlparse(uri).path))
    return paths


class SusenMenu(GObject.GObject, FM.MenuProvider):
    def __init__(self):
        super().__init__()
        self.command = find_command()
        self.cache = OrderedDict()

    def options_for(self, paths):
        key = tuple(paths)
        if key in self.cache:
            self.cache.move_to_end(key)
            return self.cache[key]
        try:
            out = subprocess.run(
                self.command + ["--list", "--"] + paths,
                capture_output=True,
                text=True,
                timeout=LIST_TIMEOUT,
            ).stdout
        except (OSError, subprocess.TimeoutExpired):
            out = ""
        options = parse_list(out)
        self.cache[key] = options
        while len(self.cache) > CACHE_SIZE:
            self.cache.popitem(last=False)
        return options

    def launch(self, _item, args, paths):
        # Detached: the conversion outlives the menu, and the file manager never waits on it.
        subprocess.Popen(
            self.command + args + ["--"] + paths,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )

    def item(self, name, label, tip, args, paths, sensitive=True):
        item = FM.MenuItem(name="Susen::" + name, label=label, tip=tip)
        item.set_property("sensitive", sensitive)
        if sensitive:
            item.connect("activate", self.launch, args, paths)
        return item

    def get_file_items(self, *args):
        # Nautilus 4 passes (files); Nautilus 3 and Caja pass (window, files).
        files = args[-1]
        if not self.command or not files:
            return []
        paths = local_paths(files)
        if not paths:
            return []

        top = FM.MenuItem(name="Susen::Menu", label="Susen", tip="Convert with Susen")
        menu = FM.Menu()
        top.set_submenu(menu)
        menu.append_item(
            self.item("Wheel", "Open in Susen wheel…", "Choose from the conversion wheel", ["--wheel"], paths)
        )

        options = self.options_for(paths)
        groups = (
            ("convert", "Convert to", "--convert", "Convert"),
            ("tool", "Tools", "--tool", "Tool"),
        )
        for prefix, title, flag, name in groups:
            entries = [o for o in options if o[0].startswith(prefix + ":") and not o[2]]
            if not entries:
                continue
            sub_item = FM.MenuItem(name="Susen::" + name, label=title, tip="")
            sub = FM.Menu()
            sub_item.set_submenu(sub)
            for oid, label, _needs, missing in entries:
                value = oid.split(":", 1)[1]
                sub.append_item(
                    self.item(
                        name + "::" + value,
                        label,
                        missing or "",
                        [flag, value, "--notify"],
                        paths,
                        sensitive=missing is None,
                    )
                )
            menu.append_item(sub_item)
        return [top]

    def get_background_items(self, *args):
        return []
