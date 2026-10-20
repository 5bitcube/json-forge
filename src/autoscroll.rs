//! Middle-button autoscroll, modelled on Chrome.
//!
//! Pressing the wheel over a scrollable area drops an anchor. While the
//! button is held, moving the pointer scrolls that area at a speed that
//! grows with the distance from the anchor. Releasing after a move ends the
//! mode; releasing without moving makes it sticky until any mouse button or
//! key is pressed. The exiting click is swallowed by a full-window overlay,
//! which also keeps the widgets underneath from reacting to hover. The OS
//! cursor is hidden and a one-way arrow is painted instead.

use eframe::egui;
use egui::scroll_area::{ScrollAreaOutput, State as ScrollState};
use egui::{Color32, CursorIcon, Id, LayerId, Order, Painter, PointerButton, Pos2, Rect, Sense, Shape, Stroke, Vec2};

pub const MAIN: &str = "main";
pub const TREE: &str = "tree";

/// No scrolling while the pointer is within this radius of the anchor.
const DEADZONE: f32 = 12.0;
/// Pointer travel that turns a press into a drag (release then ends the mode).
const MOVE_THRESHOLD: f32 = 4.0;
/// Points per second of scrolling per point beyond the dead zone.
const GAIN: f32 = 10.0;
const MAX_SPEED: f32 = 5000.0;

const FILL: Color32 = Color32::from_rgb(250, 250, 250);
const INK: Color32 = Color32::from_rgb(45, 45, 45);

/// A scroll area that can be autoscrolled, recorded by the view that drew it.
#[derive(Clone, Copy)]
pub struct AreaInfo {
    pub id: Id,
    pub rect: Rect,
    /// Content overflows horizontally / vertically.
    pub axes: [bool; 2],
}

fn key(name: &str) -> Id {
    Id::new(("autoscroll_area", name))
}

/// Call right after showing a scroll area so autoscroll can find it.
pub fn record<R>(ui: &egui::Ui, name: &str, out: &ScrollAreaOutput<R>) {
    let info = AreaInfo {
        id: out.id,
        rect: out.inner_rect,
        axes: [
            out.content_size.x > out.inner_rect.width() + 0.5,
            out.content_size.y > out.inner_rect.height() + 0.5,
        ],
    };
    ui.data_mut(|d| d.insert_temp(key(name), info));
}

/// Forget last frame's areas; views re-record the ones they draw.
pub fn clear(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        for name in [MAIN, TREE] {
            d.remove::<AreaInfo>(key(name));
        }
    });
}

fn area_at(ctx: &egui::Context, pos: Pos2) -> Option<AreaInfo> {
    ctx.data(|d| {
        [MAIN, TREE]
            .iter()
            .filter_map(|name| d.get_temp::<AreaInfo>(key(name)))
            .find(|a| a.rect.contains(pos) && (a.axes[0] || a.axes[1]))
    })
}

pub struct AutoScroll {
    origin: Pos2,
    area: AreaInfo,
    moved: bool,
    sticky: bool,
    /// A button went down in sticky mode; end on its release so the click
    /// never reaches the widgets underneath.
    exiting: bool,
}

/// Per-frame logic: start, scroll, and end the mode. Call before the UI.
pub fn update(state: &mut Option<AutoScroll>, ctx: &egui::Context) {
    let (mid_pressed, mid_released, any_pressed, any_released, pos, dt, key_pressed) = ctx.input(|i| {
        (
            i.pointer.button_pressed(PointerButton::Middle),
            i.pointer.button_released(PointerButton::Middle),
            i.pointer.any_pressed(),
            i.pointer.any_released(),
            i.pointer.latest_pos(),
            i.stable_dt.min(0.1),
            i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })),
        )
    });

    let Some(a) = state.as_mut() else {
        if mid_pressed {
            if let Some(p) = pos {
                if let Some(area) = area_at(ctx, p) {
                    *state = Some(AutoScroll { origin: p, area, moved: false, sticky: false, exiting: false });
                    ctx.request_repaint();
                }
            }
        }
        return;
    };

    if key_pressed || (a.exiting && any_released) {
        *state = None;
        ctx.request_repaint();
        return;
    }
    if a.exiting {
        ctx.request_repaint();
        return;
    }
    if a.sticky {
        if any_pressed {
            a.exiting = true;
        }
    } else {
        if pos.is_some_and(|p| p.distance(a.origin) > MOVE_THRESHOLD) {
            a.moved = true;
        }
        if mid_released {
            if a.moved {
                *state = None;
                ctx.request_repaint();
                return;
            }
            a.sticky = true;
        }
    }

    if let Some(p) = pos {
        let d = p - a.origin;
        let v = Vec2::new(speed(d.x), speed(d.y));
        if v != Vec2::ZERO {
            if let Some(mut st) = ScrollState::load(ctx, a.area.id) {
                for axis in 0..2 {
                    if a.area.axes[axis] {
                        st.offset[axis] += v[axis] * dt;
                    }
                }
                st.store(ctx, a.area.id);
            }
        }
    }
    ctx.request_repaint();
}

fn speed(d: f32) -> f32 {
    let m = d.abs();
    if m <= DEADZONE {
        0.0
    } else {
        ((m - DEADZONE) * GAIN).min(MAX_SPEED) * d.signum()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    fn unit(self) -> Vec2 {
        match self {
            Dir::Up => Vec2::new(0.0, -1.0),
            Dir::Down => Vec2::new(0.0, 1.0),
            Dir::Left => Vec2::new(-1.0, 0.0),
            Dir::Right => Vec2::new(1.0, 0.0),
        }
    }
}

/// Dominant scroll direction for the pointer offset, if outside the dead zone.
fn direction(d: Vec2, axes: [bool; 2]) -> Option<Dir> {
    let (ax, ay) = (d.x.abs(), d.y.abs());
    let vertical = axes[1] && ay > DEADZONE && (ay >= ax || !axes[0]);
    let horizontal = axes[0] && ax > DEADZONE && !vertical;
    if vertical {
        Some(if d.y < 0.0 { Dir::Up } else { Dir::Down })
    } else if horizontal {
        Some(if d.x < 0.0 { Dir::Left } else { Dir::Right })
    } else {
        None
    }
}

/// Draw the overlay, anchor, and cursor. Call after every other widget.
pub fn overlay(state: &Option<AutoScroll>, ctx: &egui::Context) {
    let Some(a) = state else { return };
    let screen = ctx.content_rect();

    // Catches hover and clicks so nothing underneath reacts while scrolling.
    egui::Area::new(Id::new("autoscroll_overlay"))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ctx, |ui| {
            ui.allocate_rect(screen, Sense::click_and_drag());
        });

    // Nothing is drawn at the anchor itself: the cursor becomes the
    // circle-with-arrows icon inside the dead zone and a one-way arrow
    // outside it.
    let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("autoscroll_paint")));
    if let Some(p) = ctx.input(|i| i.pointer.latest_pos()) {
        ctx.set_cursor_icon(CursorIcon::None);
        match direction(p - a.origin, a.area.axes) {
            Some(dir) => paint_arrow(&painter, p, dir),
            None => paint_neutral(&painter, p, a.area.axes),
        }
    }
}

fn tri(tip: Pos2, dir: Dir, size: f32) -> Shape {
    let u = dir.unit();
    let v = Vec2::new(-u.y, u.x);
    let base = tip - u * size * 1.25;
    Shape::convex_polygon(vec![tip, base + v * size, base - v * size], INK, Stroke::NONE)
}

/// Dead-zone cursor: circle with a dot and a triangle for each scrollable direction.
fn paint_neutral(p: &Painter, c: Pos2, axes: [bool; 2]) {
    let r = 14.0;
    let tri_size = 4.0;
    p.circle_filled(c, r, FILL.gamma_multiply(0.92));
    p.circle_stroke(c, r, Stroke::new(1.0, INK));
    p.circle_filled(c, 2.0, INK);
    let off = r - 5.0;
    if axes[1] {
        p.add(tri(c + Vec2::new(0.0, -off), Dir::Up, tri_size));
        p.add(tri(c + Vec2::new(0.0, off), Dir::Down, tri_size));
    }
    if axes[0] {
        p.add(tri(c + Vec2::new(-off, 0.0), Dir::Left, tri_size));
        p.add(tri(c + Vec2::new(off, 0.0), Dir::Right, tri_size));
    }
}

/// One-way arrow centred on `c`, pointing in `dir`.
fn paint_arrow(p: &Painter, c: Pos2, dir: Dir) {
    let u = dir.unit();
    let v = Vec2::new(-u.y, u.x);
    let stroke = Stroke::new(1.2, INK);

    let tip = c + u * 11.0;
    let neck = c + u * 1.0;
    let tail = c - u * 9.0;
    let head_half = 6.5;
    let stem_half = 2.5;

    // Stem first, then the head on top so the join is hidden.
    p.add(Shape::convex_polygon(
        vec![tail + v * stem_half, neck + v * stem_half, neck - v * stem_half, tail - v * stem_half],
        FILL,
        stroke,
    ));
    p.add(Shape::convex_polygon(vec![tip, neck + v * head_half, neck - v * head_half], FILL, stroke));
}
