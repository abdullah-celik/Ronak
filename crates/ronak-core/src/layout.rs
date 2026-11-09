//! The layout solver: turns window roles into rectangles.
//!
//! The AI decides *what matters* (each window's role and weight). This solver
//! decides *where things go*, and it is deliberately boring:
//!
//! - **Deterministic**: the same input always produces the same layout.
//! - **Stable**: if every window is already within `stability_px` of where the
//!   solver would put it, the plan is empty and nothing moves.
//! - **Respectful**: if the user is dragging a window that is part of the
//!   task, the solver backs off entirely rather than fight them.
//! - **Constrained**: no tile is smaller than the minimum; lower-priority
//!   windows are demoted to the background until everything fits.
//!
//! Shape: primary windows share a main area; references and monitors stack in
//! a side column (or row, on portrait screens), sized by weight.

use serde::{Deserialize, Serialize};

use crate::model::{Rect, Role};
use crate::protocol::Placement;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    /// Space between tiles and around the edges, in logical pixels.
    pub gap: i32,
    /// If no window would move by more than this, do nothing.
    pub stability_px: i32,
    pub min_tile_w: i32,
    pub min_tile_h: i32,
    /// Share of the work area given to the main (primary) region.
    pub main_ratio: f32,
    /// Monitor-role windows get this fraction of a reference's share.
    pub monitor_share: f32,
    /// Never arrange more than this many windows at once.
    pub max_tiles: usize,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            gap: 8,
            stability_px: 24,
            min_tile_w: 480,
            min_tile_h: 300,
            main_ratio: 0.6,
            monitor_share: 0.6,
            max_tiles: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayoutItem {
    pub window_id: String,
    pub role: Role,
    pub weight: f32,
    /// Where the window is now.
    pub current: Rect,
    pub maximized: bool,
    /// The user is moving this window right now; do not touch the layout.
    pub pinned: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutPlan {
    /// Windows to move, in priority order. Empty means "leave everything".
    pub placements: Vec<Placement>,
    /// Task windows that did not fit and were treated as background.
    pub demoted: Vec<String>,
}

impl LayoutPlan {
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }
}

pub fn solve(area: Rect, items: &[LayoutItem], cfg: &LayoutConfig) -> LayoutPlan {
    if items.iter().any(|i| i.pinned && i.role != Role::Background) {
        return LayoutPlan::default();
    }
    let mut visible: Vec<&LayoutItem> = items.iter().filter(|i| i.role != Role::Background).collect();
    visible.sort_by(|a, b| a.role.cmp(&b.role).then(b.weight.total_cmp(&a.weight)).then(a.window_id.cmp(&b.window_id)));
    if visible.is_empty() || area.w <= 0 || area.h <= 0 {
        return LayoutPlan::default();
    }

    // How many tiles can physically fit at the minimum size?
    let fit = ((area.w / cfg.min_tile_w.max(1)).max(1) * (area.h / cfg.min_tile_h.max(1)).max(1)) as usize;
    let cap = cfg.max_tiles.min(fit).max(1);
    let mut demoted: Vec<String> = visible.iter().skip(cap).map(|i| i.window_id.clone()).collect();
    visible.truncate(cap);

    // Try the layout; while any tile is too small, demote the least important.
    let tiles = loop {
        if let Some(tiles) = attempt(area, &visible, cfg) {
            break tiles;
        }
        let last = visible.pop().expect("attempt always succeeds for one window");
        demoted.push(last.window_id.clone());
    };

    // Whole-plan stability: move nothing unless something moves noticeably.
    let by_id = |id: &str| visible.iter().find(|i| i.window_id == id).copied();
    let noticeable = tiles.iter().any(|(id, rect)| {
        let item = by_id(id).expect("tile ids come from visible items");
        item.maximized || item.current.max_edge_delta(rect) > cfg.stability_px
    });
    let placements = if noticeable {
        tiles.into_iter().map(|(window_id, rect)| Placement { window_id, rect, maximized: false }).collect()
    } else {
        vec![]
    };
    LayoutPlan { placements, demoted }
}

/// One layout attempt; `None` if some tile ends up below the minimum size.
fn attempt(area: Rect, visible: &[&LayoutItem], cfg: &LayoutConfig) -> Option<Vec<(String, Rect)>> {
    let inner = area.inset(cfg.gap);
    let mut primaries: Vec<&LayoutItem> = visible.iter().copied().filter(|i| i.role == Role::Primary).collect();
    let mut side: Vec<&LayoutItem> = visible.iter().copied().filter(|i| i.role != Role::Primary).collect();
    if primaries.is_empty() {
        // The most important remaining window becomes the primary.
        primaries.push(side.remove(0));
    }

    let landscape = inner.w >= inner.h;
    let mut out = Vec::with_capacity(visible.len());
    if side.is_empty() {
        let even = vec![1.0; primaries.len()];
        for (item, r) in primaries.iter().zip(split(inner, &even, landscape, cfg.gap)) {
            out.push((item.window_id.clone(), r));
        }
    } else {
        let ratio = if primaries.len() >= 2 { cfg.main_ratio.max(0.7) } else { cfg.main_ratio };
        let (main, rest) = split_ratio(inner, ratio, landscape, cfg.gap);
        let even = vec![1.0; primaries.len()];
        let main_horizontal = main.w >= main.h;
        for (item, r) in primaries.iter().zip(split(main, &even, main_horizontal, cfg.gap)) {
            out.push((item.window_id.clone(), r));
        }
        let weights: Vec<f32> = side
            .iter()
            .map(|i| {
                let share = if i.role == Role::Monitor { cfg.monitor_share } else { 1.0 };
                (i.weight.max(0.2)) * share
            })
            .collect();
        // Stack along the long side of the side region (vertical in a column).
        let side_horizontal = rest.w > rest.h;
        for (item, r) in side.iter().zip(split(rest, &weights, side_horizontal, cfg.gap)) {
            out.push((item.window_id.clone(), r));
        }
    }

    if out.len() > 1 && out.iter().any(|(_, r)| r.w < cfg.min_tile_w || r.h < cfg.min_tile_h) {
        return None;
    }
    Some(out)
}

/// Splits `rect` into a leading part with `ratio` of the space and the rest.
fn split_ratio(rect: Rect, ratio: f32, horizontal: bool, gap: i32) -> (Rect, Rect) {
    let parts = split(rect, &[ratio, 1.0 - ratio], horizontal, gap);
    (parts[0], parts[1])
}

/// Splits `rect` into consecutive parts proportional to `weights`, side by
/// side (`horizontal`) or stacked, with `gap` between parts. Boundaries are
/// rounded cumulatively so the parts always fill the rect exactly.
fn split(rect: Rect, weights: &[f32], horizontal: bool, gap: i32) -> Vec<Rect> {
    let n = weights.len() as i32;
    if n == 0 {
        return vec![];
    }
    let length = if horizontal { rect.w } else { rect.h };
    let usable = (length - gap * (n - 1)).max(0) as f64;
    let sum: f64 = weights.iter().map(|w| w.max(0.0) as f64).sum::<f64>().max(1e-9);
    let mut acc = 0.0;
    let mut start = 0i32;
    let mut parts = Vec::with_capacity(weights.len());
    for (i, w) in weights.iter().enumerate() {
        acc += w.max(0.0) as f64;
        let end = ((acc / sum) * usable).round() as i32;
        let offset = start + gap * i as i32;
        let size = end - start;
        parts.push(if horizontal {
            Rect::new(rect.x + offset, rect.y, size, rect.h)
        } else {
            Rect::new(rect.x, rect.y + offset, rect.w, size)
        });
        start = end;
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAPTOP: Rect = Rect::new(0, 32, 1920, 1168);

    fn item(id: &str, role: Role, weight: f32) -> LayoutItem {
        LayoutItem { window_id: id.into(), role, weight, current: LAPTOP, maximized: false, pinned: false }
    }

    fn assert_sane(area: Rect, plan: &LayoutPlan, cfg: &LayoutConfig) {
        for (i, a) in plan.placements.iter().enumerate() {
            assert!(area.contains(&a.rect), "{:?} outside {:?}", a.rect, area);
            if plan.placements.len() > 1 {
                assert!(a.rect.w >= cfg.min_tile_w && a.rect.h >= cfg.min_tile_h, "{:?} too small", a.rect);
            }
            for b in &plan.placements[i + 1..] {
                assert!(!a.rect.intersects(&b.rect), "{:?} overlaps {:?}", a.rect, b.rect);
            }
        }
    }

    #[test]
    fn coding_layout_on_a_laptop() {
        let cfg = LayoutConfig::default();
        let items = [
            item("editor", Role::Primary, 1.0),
            item("term", Role::Reference, 0.6),
            item("docs", Role::Reference, 0.4),
            item("chat", Role::Monitor, 0.2),
            item("files", Role::Background, 0.1),
        ];
        let plan = solve(LAPTOP, &items, &cfg);
        assert_sane(LAPTOP, &plan, &cfg);
        // 1168px of height fits three side tiles at 300px minimum, so the
        // monitor-role chat window is demoted rather than squeezed.
        let ids: Vec<_> = plan.placements.iter().map(|p| p.window_id.as_str()).collect();
        assert_eq!(ids, ["editor", "term", "docs"]);
        assert_eq!(plan.demoted, ["chat"]);
        let editor = plan.placements[0].rect;
        assert_eq!((editor.x, editor.y), (8, 40));
        assert!(editor.w > 1050 && editor.w < 1160, "main area width {}", editor.w);
    }

    #[test]
    fn single_window_fills_the_work_area() {
        let cfg = LayoutConfig::default();
        let small = LayoutItem { current: Rect::new(200, 200, 600, 400), ..item("a", Role::Primary, 1.0) };
        let plan = solve(LAPTOP, &[small], &cfg);
        assert_eq!(plan.placements[0].rect, LAPTOP.inset(cfg.gap));
        // A window that already fills the area (within tolerance) stays put.
        assert!(solve(LAPTOP, &[item("a", Role::Primary, 1.0)], &cfg).is_empty());
    }

    #[test]
    fn deterministic_and_stable() {
        let cfg = LayoutConfig::default();
        let items = vec![item("a", Role::Primary, 1.0), item("b", Role::Reference, 0.5)];
        let first = solve(LAPTOP, &items, &cfg);
        assert_eq!(first, solve(LAPTOP, &items, &cfg));
        // Put the windows where the plan said, give or take a few pixels.
        let settled: Vec<LayoutItem> = items
            .iter()
            .map(|i| {
                let mut r = first.placements.iter().find(|p| p.window_id == i.window_id).unwrap().rect;
                r.x += 5;
                LayoutItem { current: r, ..i.clone() }
            })
            .collect();
        assert!(solve(LAPTOP, &settled, &cfg).is_empty(), "small offsets must not cause moves");
    }

    #[test]
    fn backs_off_while_user_drags() {
        let mut items = vec![item("a", Role::Primary, 1.0), item("b", Role::Reference, 0.5)];
        items[1].pinned = true;
        assert!(solve(LAPTOP, &items, &LayoutConfig::default()).is_empty());
    }

    #[test]
    fn portrait_and_tiny_screens_stay_valid() {
        let cfg = LayoutConfig::default();
        let items = [item("a", Role::Primary, 1.0), item("b", Role::Reference, 0.8), item("c", Role::Monitor, 0.3)];
        for area in [Rect::new(0, 0, 1080, 1920), Rect::new(0, 0, 800, 600), Rect::new(0, 0, 3440, 1440)] {
            let plan = solve(area, &items, &cfg);
            assert!(!plan.placements.is_empty());
            assert_sane(area, &plan, &cfg);
        }
    }
}
