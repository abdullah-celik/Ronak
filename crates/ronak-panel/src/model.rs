//! What the panel shows, decoded from the daemon's `surfaces` notification.
//!
//! Pure data, no GTK, so it is tested without a display.

use ronak_core::surface::{self, PanelStyle, Surface, SurfaceKind};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct PanelUpdate {
    pub surfaces: Vec<Surface>,
    pub visible: bool,
    pub style: PanelStyle,
    pub width: i32,
    /// Surfaces that failed validation here (they should never arrive; the
    /// daemon validates first). Counted so the panel can log it.
    pub rejected: usize,
}

/// Decodes a notification. Accepts the full JSON-RPC message, just its
/// params, a list of surfaces or a single surface (the last three are handy
/// for `--demo` files).
pub fn parse_update(msg: &Value) -> Result<PanelUpdate, String> {
    let params = msg.get("params").unwrap_or(msg);
    let (list, panel) = match params {
        Value::Array(list) => (list.clone(), None),
        Value::Object(o) if o.contains_key("surfaces") => {
            (o["surfaces"].as_array().cloned().ok_or("'surfaces' must be a list")?, o.get("panel"))
        }
        Value::Object(o) if o.contains_key("components") => (vec![params.clone()], None),
        _ => return Err("not a surfaces message".into()),
    };
    let mut surfaces = vec![];
    let mut rejected = 0;
    for v in &list {
        // Defense in depth: render only what passes the catalog's validator.
        match surface::parse(v) {
            Ok(s) => surfaces.push(s),
            Err(_) => rejected += 1,
        }
    }
    surfaces.sort_by_key(|s| order(s.kind));
    let style =
        panel.and_then(|p| p.get("style")).and_then(|s| serde_json::from_value(s.clone()).ok()).unwrap_or_default();
    Ok(PanelUpdate {
        surfaces,
        visible: panel.and_then(|p| p.get("visible")).and_then(Value::as_bool).unwrap_or(true),
        style,
        width: panel
            .and_then(|p| p.get("width"))
            .and_then(Value::as_i64)
            .map(|w| w.clamp(240, 720) as i32)
            .unwrap_or(360),
        rejected,
    })
}

/// Intent first, then what agents proposed, then glances.
fn order(kind: SurfaceKind) -> u8 {
    match kind {
        SurfaceKind::IntentPanel => 0,
        SurfaceKind::Proposal => 1,
        SurfaceKind::ContextGlances => 2,
        SurfaceKind::Custom => 3,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn surface(id: &str, kind: &str) -> Value {
        json!({
            "schema_version": "0.2", "id": id, "kind": kind, "composer": "rules",
            "created_at": "2026-11-09T10:00:00Z", "root": "t",
            "components": [ { "id": "t", "type": "text", "text": "hi" } ]
        })
    }

    #[test]
    fn decodes_and_orders_surfaces() {
        let msg = json!({ "jsonrpc": "2.0", "method": "surfaces", "params": {
            "surfaces": [ surface("g", "context_glances"), surface("i", "intent_panel") ],
            "panel": { "visible": false, "style": "floating_card", "width": 400 }
        }});
        let u = parse_update(&msg).unwrap();
        assert_eq!(u.surfaces.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["i", "g"]);
        assert_eq!((u.visible, u.style, u.width), (false, PanelStyle::FloatingCard, 400));
    }

    #[test]
    fn invalid_surfaces_are_dropped() {
        let mut bad = surface("b", "custom");
        bad["components"][0] =
            json!({ "id": "t", "type": "button", "label": "x", "action": { "name": "format_disk" } });
        let u = parse_update(&json!([surface("ok", "custom"), bad])).unwrap();
        assert_eq!((u.surfaces.len(), u.rejected), (1, 1));
        assert!(u.visible, "demo files show the panel");
        assert!(parse_update(&json!({ "nope": 1 })).is_err());
    }
}
