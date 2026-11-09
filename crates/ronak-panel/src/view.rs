//! Turns surfaces into GTK widgets.
//!
//! Each component type maps to one native widget, styled by libadwaita so
//! the panel looks like the rest of GNOME. The catalog is closed: there is no
//! way for a surface to run code, load images from the network or draw
//! outside its card.

use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::pango;
use ronak_core::surface::{ActionName, Body, Component, Surface, SurfaceKind, TextStyle, Tone, UiAction};

/// Called when the user presses a button, picks a chip or submits an entry.
pub type OnAction = Rc<dyn Fn(UiAction, Option<String>)>;

pub const CSS: &str = r#"
.ronak-surfaces { padding: 12px; }
.ronak-card { padding: 12px; }
.ronak-card > .heading { margin-bottom: 2px; }
.ronak-proposal { border: 1px solid alpha(@accent_color, 0.55); }
.ronak-badge {
  padding: 1px 8px;
  border-radius: 999px;
  font-size: 0.85em;
  font-weight: 700;
  background-color: alpha(currentColor, 0.10);
}
.ronak-badge.neutral { color: alpha(@window_fg_color, 0.75); }
.ronak-badge.accent { color: @accent_color; background-color: alpha(@accent_bg_color, 0.16); }
.ronak-badge.success { color: @success_color; background-color: alpha(@success_bg_color, 0.16); }
.ronak-badge.warning { color: @warning_color; background-color: alpha(@warning_bg_color, 0.18); }
.ronak-chip { padding: 2px 12px; min-height: 28px; border-radius: 999px; }
.ronak-chips flowboxchild { padding: 0; }
.ronak-meter progress, .ronak-meter trough { min-height: 6px; border-radius: 3px; }
"#;

pub fn render(surface: &Surface, on_action: &OnAction) -> gtk::Widget {
    let map: HashMap<&str, &Component> = surface.components.iter().map(|c| (c.id.as_str(), c)).collect();
    let widget = build(&surface.root, &map, on_action, 0)
        .unwrap_or_else(|| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast());
    if surface.kind == SurfaceKind::Proposal {
        widget.add_css_class("ronak-proposal");
    }
    widget.set_widget_name(&surface.id);
    widget
}

fn build(id: &str, map: &HashMap<&str, &Component>, on_action: &OnAction, depth: usize) -> Option<gtk::Widget> {
    // The validator already rejects cycles and deep trees; this is a second fence.
    if depth > ronak_core::surface::MAX_DEPTH {
        return None;
    }
    let c = map.get(id)?;
    let children = |b: &gtk::Box, ids: &[String]| {
        for child in ids {
            if let Some(w) = build(child, map, on_action, depth + 1) {
                b.append(&w);
            }
        }
    };
    let w: gtk::Widget = match &c.body {
        Body::Column { children: ids } => {
            let b = gtk::Box::new(gtk::Orientation::Vertical, if depth == 0 { 12 } else { 8 });
            children(&b, ids);
            b.upcast()
        }
        Body::Row { children: ids } => {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            children(&b, ids);
            b.upcast()
        }
        Body::Card { title, children: ids } => {
            let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
            b.add_css_class("card");
            b.add_css_class("ronak-card");
            if let Some(t) = title {
                let l = label(t);
                l.add_css_class("heading");
                b.append(&l);
            }
            children(&b, ids);
            b.upcast()
        }
        Body::Text { text, style } => {
            let l = label(text);
            match style {
                TextStyle::Body => {}
                TextStyle::Caption => {
                    l.add_css_class("caption");
                    l.add_css_class("dim-label");
                }
                TextStyle::Heading => l.add_css_class("heading"),
                TextStyle::Title => l.add_css_class("title-3"),
            }
            l.upcast()
        }
        Body::Badge { text, tone } => {
            let l = gtk::Label::new(Some(text));
            l.add_css_class("ronak-badge");
            l.add_css_class(match tone {
                Tone::Neutral => "neutral",
                Tone::Accent => "accent",
                Tone::Success => "success",
                Tone::Warning => "warning",
            });
            l.set_halign(gtk::Align::Start);
            l.upcast()
        }
        Body::Meter { value, label: text } => {
            let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
            b.add_css_class("ronak-meter");
            if let Some(t) = text {
                let l = label(t);
                l.add_css_class("caption");
                l.add_css_class("dim-label");
                b.append(&l);
            }
            let bar = gtk::ProgressBar::new();
            bar.set_fraction(value.clamp(0.0, 1.0) as f64);
            bar.update_property(&[gtk::accessible::Property::Label(text.as_deref().unwrap_or("level"))]);
            b.append(&bar);
            b.upcast()
        }
        Body::Button { label: text, action, icon } => button(text, icon.as_deref(), action, on_action).upcast(),
        Body::Chips { options } => {
            let flow = gtk::FlowBox::new();
            flow.add_css_class("ronak-chips");
            flow.set_selection_mode(gtk::SelectionMode::None);
            flow.set_column_spacing(6);
            flow.set_row_spacing(6);
            flow.set_max_children_per_line(8);
            flow.set_homogeneous(false);
            for chip in options {
                let b = gtk::Button::with_label(&chip.label);
                b.add_css_class("ronak-chip");
                if chip.selected {
                    b.add_css_class("suggested-action");
                }
                let (action, cb) = (chip.action.clone(), on_action.clone());
                b.connect_clicked(move |_| cb(action.clone(), None));
                flow.insert(&b, -1);
            }
            flow.upcast()
        }
        Body::Entry { placeholder, action } => {
            let e = gtk::Entry::new();
            e.set_placeholder_text(Some(placeholder));
            e.set_secondary_icon_name(Some("go-next-symbolic"));
            e.set_secondary_icon_tooltip_text(Some("Send"));
            e.update_property(&[gtk::accessible::Property::Label(placeholder)]);
            let submit = {
                let (action, cb) = (action.clone(), on_action.clone());
                move |e: &gtk::Entry| {
                    let text = e.text().trim().to_string();
                    if !text.is_empty() {
                        cb(action.clone(), Some(text));
                        e.set_text("");
                    }
                }
            };
            let s2 = submit.clone();
            e.connect_activate(move |e| submit(e));
            e.connect_icon_release(move |e, _| s2(e));
            e.upcast()
        }
        Body::Icon { name } => {
            let i = gtk::Image::from_icon_name(name);
            i.set_pixel_size(16);
            i.upcast()
        }
        Body::Divider => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
    };
    Some(w)
}

fn label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_wrap(true);
    l.set_wrap_mode(pango::WrapMode::WordChar);
    l.set_xalign(0.0);
    l.set_halign(gtk::Align::Fill);
    l
}

fn button(text: &str, icon: Option<&str>, action: &UiAction, on_action: &OnAction) -> gtk::Button {
    let b = gtk::Button::new();
    match icon {
        // Transport controls read better as round icon buttons.
        Some(i) if i.starts_with("media-") => {
            b.set_icon_name(i);
            b.add_css_class("circular");
            b.set_tooltip_text(Some(text));
            b.update_property(&[gtk::accessible::Property::Label(text)]);
        }
        Some(i) => b.set_child(Some(&adw::ButtonContent::builder().icon_name(i).label(text).build())),
        None => b.set_label(text),
    }
    if matches!(
        action.name,
        ActionName::AcceptSuggestion | ActionName::AcceptProposal | ActionName::EnableAccessibility
    ) {
        b.add_css_class("suggested-action");
    }
    let (action, cb) = (action.clone(), on_action.clone());
    b.connect_clicked(move |_| cb(action.clone(), None));
    b
}
