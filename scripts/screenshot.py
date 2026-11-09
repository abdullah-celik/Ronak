#!/usr/bin/env python3
"""Takes a screenshot of a (headless) GNOME Shell for scripts/headless-test.sh.

GNOME only lets a few well-known services use its screenshot API. Inside the
test's private D-Bus session we take one of those names first, which is
fine there and impossible to do against your real session's shell."""
import sys

from gi.repository import Gio, GLib

path = sys.argv[1]
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
              GLib.Variant("(su)", ("org.gnome.Screenshot", 0)), None, Gio.DBusCallFlags.NONE, 5000, None)
# The shell notices the new owner asynchronously.
loop = GLib.MainLoop()
GLib.timeout_add(500, loop.quit)
loop.run()
ok, used = bus.call_sync("org.gnome.Shell.Screenshot", "/org/gnome/Shell/Screenshot", "org.gnome.Shell.Screenshot",
                         "Screenshot", GLib.Variant("(bbs)", (False, False, path)),
                         GLib.VariantType("(bs)"), Gio.DBusCallFlags.NONE, 10000, None).unpack()
print("screenshot:", used if ok else "failed")
sys.exit(0 if ok else 1)
