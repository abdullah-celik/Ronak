#!/usr/bin/env python3
"""A tiny GTK 4 "editor" for scripts/headless-test.sh: a window whose title
looks like an editor's and a focused text view with some code in it, so the
daemon has something to observe and read through the accessibility bus."""
import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gtk  # noqa: E402

CODE = 'fn main() {\n    println!("hello from the test app");\n}\n'


def activate(app):
    win = Gtk.ApplicationWindow(application=app, title="main.rs (~/src/ronak) - Scratch Editor")
    win.set_default_size(900, 700)
    view = Gtk.TextView(monospace=True)
    view.get_buffer().set_text(CODE)
    scrolled = Gtk.ScrolledWindow(child=view)
    win.set_child(scrolled)
    win.present()
    view.grab_focus()


app = Gtk.Application(application_id="dev.ronak.TestTextEditor")
app.connect("activate", activate)
app.run([])
