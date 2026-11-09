//! `ronak-panel`: the Ronak panel, a small GTK 4 + libadwaita app.
//!
//! It subscribes to the daemon's surfaces over the control socket and draws
//! them with native widgets. It never decides anything itself: every button
//! sends an action back to the daemon, which checks it and acts. The GNOME
//! Shell extension positions the window (docked on the right, or floating
//! near the top bar) because Wayland apps cannot place their own windows.
//!
//! ```text
//! ronak-panel                      normal use (started by ronakd)
//! ronak-panel --demo FILE          show surfaces from a JSON file, no daemon
//! ronak-panel --demo FILE --screenshot OUT.png [--scale 2]
//! ```

mod client;
mod model;
mod view;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use ronak_core::config::PANEL_APP_ID;
use ronak_core::surface::{ActionName, PanelStyle, UiAction};
use serde_json::{Value, json};

use crate::client::Incoming;
use crate::model::PanelUpdate;

#[derive(Default, Clone)]
struct Args {
    demo: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    scale: f64,
    style: Option<PanelStyle>,
    height: Option<i32>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { scale: 1.0, ..Args::default() };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--demo" => a.demo = Some(value()?.into()),
            "--screenshot" => a.screenshot = Some(value()?.into()),
            "--scale" => a.scale = value()?.parse().map_err(|_| "--scale needs a number")?,
            "--height" => a.height = Some(value()?.parse().map_err(|_| "--height needs a number")?),
            "--style" => {
                a.style = Some(match value()?.as_str() {
                    "side" | "side_panel" => PanelStyle::SidePanel,
                    "floating" | "floating_card" => PanelStyle::FloatingCard,
                    other => return Err(format!("unknown style {other:?} (side or floating)")),
                })
            }
            "-h" | "--help" => {
                println!(
                    "usage: ronak-panel [--demo FILE [--style side|floating] [--height PX] [--screenshot OUT.png [--scale N]]]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(a)
}

fn main() -> glib::ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ronak-panel: {e}");
            return glib::ExitCode::FAILURE;
        }
    };
    let mut flags = gio::ApplicationFlags::empty();
    if args.demo.is_some() {
        // A demo can run next to the real panel.
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder().application_id(PANEL_APP_ID).flags(flags).build();
    app.connect_startup(|_| {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(view::CSS);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
    app.connect_activate(move |app| {
        // A second launch (for example from the app grid) just shows the panel.
        if let Some(w) = app.active_window() {
            w.present();
            return;
        }
        Panel::build(app, args.clone());
    });
    // Our flags are not GTK's; do not let GTK parse them.
    app.run_with_args::<&str>(&[])
}

struct Panel {
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    list: gtk::Box,
    toasts: adw::ToastOverlay,
    style_button: gtk::Button,
    style: Cell<PanelStyle>,
    width: Cell<i32>,
    socket: PathBuf,
    demo: bool,
    fixed_height: Option<i32>,
    _hold: RefCell<Option<gio::ApplicationHoldGuard>>,
}

impl Panel {
    fn build(app: &adw::Application, args: Args) -> Rc<Panel> {
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new("Ronak", "")));
        // A panel has nothing to minimize or maximize.
        header.set_decoration_layout(Some(":close"));
        let style_button = gtk::Button::new();
        style_button.add_css_class("flat");
        header.pack_start(&style_button);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
        list.add_css_class("ronak-surfaces");
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .propagate_natural_height(true)
            .child(&list)
            .build();
        let offline = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .title("Ronak isn't running")
            .description("Start it with <tt>systemctl --user start ronakd</tt>. The panel reconnects by itself.")
            .build();
        offline.add_css_class("compact");
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.add_named(&offline, Some("offline"));
        stack.add_named(&scrolled, Some("surfaces"));

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&stack));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&view));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Ronak")
            .default_width(360)
            .default_height(720)
            .content(&toasts)
            .build();

        let panel = Rc::new(Panel {
            window,
            stack,
            list,
            toasts,
            style_button,
            style: Cell::new(args.style.unwrap_or_default()),
            width: Cell::new(360),
            socket: client::socket_path(),
            demo: args.demo.is_some(),
            fixed_height: args.height,
            _hold: RefCell::new(Some(app.hold())),
        });
        panel.wire();
        panel.show_style();

        if let Some(file) = &args.demo {
            match load_demo(file) {
                Ok(mut u) => {
                    u.visible = true;
                    if let Some(s) = args.style {
                        u.style = s;
                    }
                    panel.update(u);
                }
                Err(e) => {
                    eprintln!("ronak-panel: {e}");
                    app.quit();
                }
            }
            if let Some(out) = args.screenshot.clone() {
                let p = panel.clone();
                let scale = args.scale;
                glib::timeout_add_local_once(Duration::from_millis(900), move || {
                    match save_png(&p.window, &out, scale) {
                        Ok(()) => println!("wrote {}", out.display()),
                        Err(e) => eprintln!("ronak-panel: screenshot failed: {e}"),
                    }
                    if let Some(app) = p.window.application() {
                        app.quit();
                    }
                });
            }
        } else {
            let (tx, rx) = async_channel::bounded(8);
            client::listen(panel.socket.clone(), tx);
            let p = panel.clone();
            glib::spawn_future_local(async move {
                while let Ok(msg) = rx.recv().await {
                    match msg {
                        Incoming::Update(u) => p.update(u),
                        Incoming::Offline(why) => {
                            p.stack.set_visible_child_name("offline");
                            eprintln!("ronak-panel: {why}");
                        }
                    }
                }
            });
        }
        panel
    }

    fn wire(self: &Rc<Self>) {
        // Closing the window hides the panel; the daemon remembers that.
        let p = self.clone();
        self.window.connect_close_request(move |w| {
            w.set_visible(false);
            p.send("panel.set", json!({ "visible": false }));
            glib::Propagation::Stop
        });
        let keys = gtk::EventControllerKey::new();
        let p = self.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                p.window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);
        let p = self.clone();
        self.style_button.connect_clicked(move |_| {
            let next = match p.style.get() {
                PanelStyle::SidePanel => PanelStyle::FloatingCard,
                PanelStyle::FloatingCard => PanelStyle::SidePanel,
            };
            if p.demo {
                p.style.set(next);
                p.show_style();
            }
            p.send("panel.set", json!({ "style": next }));
        });
    }

    fn show_style(&self) {
        let (icon, tip) = match self.style.get() {
            PanelStyle::SidePanel => ("view-restore-symbolic", "Float as a card"),
            PanelStyle::FloatingCard => ("sidebar-show-right-symbolic", "Dock to the side"),
        };
        self.style_button.set_icon_name(icon);
        self.style_button.set_tooltip_text(Some(tip));
        self.style_button.update_property(&[gtk::accessible::Property::Label(tip)]);
    }

    fn update(self: &Rc<Self>, u: PanelUpdate) {
        if u.rejected > 0 {
            eprintln!("ronak-panel: dropped {} invalid surface(s)", u.rejected);
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let p = self.clone();
        let on_action: view::OnAction = Rc::new(move |action, text| p.act(action, text));
        for s in &u.surfaces {
            self.list.append(&view::render(s, &on_action));
        }
        self.stack.set_visible_child_name("surfaces");

        let restyled = self.style.get() != u.style || self.width.get() != u.width;
        self.style.set(u.style);
        self.width.set(u.width);
        self.show_style();
        if restyled || !self.window.is_visible() {
            let height = self.fixed_height.unwrap_or(match u.style {
                PanelStyle::SidePanel => 720,
                PanelStyle::FloatingCard => 480,
            });
            self.window.set_default_size(u.width, height);
        }
        if u.visible && !self.window.is_visible() {
            self.window.present();
        } else if !u.visible && self.window.is_visible() {
            self.window.set_visible(false);
        }
    }

    fn act(self: &Rc<Self>, action: UiAction, text: Option<String>) {
        if action.name == ActionName::ClosePanel {
            self.window.close();
            return;
        }
        let params = json!({ "action": action, "text": text });
        if self.demo {
            println!("action: {params}");
            return;
        }
        self.send("surface.action", params);
    }

    /// Sends a request on a worker thread; errors become a toast.
    fn send(self: &Rc<Self>, method: &'static str, params: Value) {
        if self.demo && method != "surface.action" {
            return;
        }
        let socket = self.socket.clone();
        let p = self.clone();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || client::call(&socket, method, params)).await;
            let error = match result {
                Ok(Ok(_)) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some("the request failed".to_string()),
            };
            if let Some(e) = error {
                let toast = adw::Toast::new(&glib::markup_escape_text(&e));
                toast.set_timeout(5);
                p.toasts.add_toast(toast);
            }
        });
    }
}

fn load_demo(path: &PathBuf) -> Result<PanelUpdate, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", path.display()))?;
    model::parse_update(&v)
}

/// Renders the window into a PNG, at `scale` times its size.
fn save_png(window: &adw::ApplicationWindow, path: &PathBuf, scale: f64) -> Result<(), String> {
    let (w, h) = (window.width() as f64, window.height() as f64);
    if w < 1.0 || h < 1.0 {
        return Err("the window has no size yet".into());
    }
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    snapshot.scale(scale as f32, scale as f32);
    paintable.snapshot(&snapshot, w, h);
    let node = snapshot.to_node().ok_or("nothing was drawn")?;
    let renderer = window.renderer().ok_or("no renderer")?;
    let viewport = gtk::graphene::Rect::new(0.0, 0.0, (w * scale) as f32, (h * scale) as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));
    texture.save_to_png(path).map_err(|e| e.to_string())
}
