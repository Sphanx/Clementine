//! The radial menu: its state, its geometry and how it is drawn.

use std::f64::consts::{PI, TAU};
use std::path::PathBuf;

use gtk::cairo;

use crate::backend;
use crate::catalog::{Mode, Op, options};

pub const INNER: f64 = 46.0;
pub const OUTER: f64 = 150.0;
/// How far past the rim the pointer may stray and still pick a slice.
pub const SLACK: f64 = 60.0;

pub struct Slice {
    pub op: Op,
    pub label: String,
    /// `Some` names the programs to install before this slice can be used.
    pub missing: Option<String>,
}

#[derive(Default)]
pub struct Wheel {
    pub open: bool,
    pub mode: Option<Mode>,
    pub center: (f64, f64),
    pub files: Vec<PathBuf>,
    pub slices: Vec<Slice>,
    pub hovered: Option<usize>,
    /// Opened from the keyboard: arrows move the selection, the pointer is ignored.
    pub keyboard: bool,
}

impl Wheel {
    pub fn show(&mut self, mode: Mode, center: (f64, f64), files: Vec<PathBuf>, keyboard: bool) {
        self.open = true;
        self.center = center;
        self.keyboard = keyboard;
        self.files = files;
        self.set_mode(mode);
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = Some(mode);
        self.refill();
    }

    pub fn set_files(&mut self, files: Vec<PathBuf>) {
        if files != self.files {
            self.files = files;
            self.refill();
        }
    }

    fn refill(&mut self) {
        let Some(mode) = self.mode else { return };
        self.slices = options(mode, &self.files)
            .into_iter()
            .map(|op| {
                let missing = match op {
                    Op::Action(_) => Vec::new(),
                    _ => backend::missing_programs(op, &self.files),
                };
                Slice { op, label: op.label(), missing: (!missing.is_empty()).then(|| missing.join(", ")) }
            })
            .collect();
        self.hovered = if self.keyboard { self.first_enabled() } else { None };
    }

    pub fn hide(&mut self) {
        *self = Wheel::default();
    }

    fn first_enabled(&self) -> Option<usize> {
        self.slices.iter().position(|s| s.missing.is_none())
    }

    pub fn hover_at(&mut self, x: f64, y: f64) {
        self.hovered = slice_at(self.slices.len(), self.center, (x, y));
    }

    /// Moves the keyboard selection one slice around, skipping greyed-out ones.
    pub fn step(&mut self, forward: bool) {
        let n = self.slices.len();
        if n == 0 {
            return;
        }
        let mut i = self.hovered.unwrap_or(if forward { n - 1 } else { 0 });
        for _ in 0..n {
            i = if forward { (i + 1) % n } else { (i + n - 1) % n };
            if self.slices[i].missing.is_none() {
                self.hovered = Some(i);
                return;
            }
        }
    }

    /// The op under the pointer, if it can run.
    pub fn chosen(&self) -> Option<Op> {
        let s = &self.slices[self.hovered?];
        s.missing.is_none().then_some(s.op)
    }
}

/// Which of `n` slices (the first centred at the top, going clockwise) holds `point`.
/// The hub picks nothing, and so does anything well past the rim.
pub fn slice_at(n: usize, center: (f64, f64), point: (f64, f64)) -> Option<usize> {
    let (dx, dy) = (point.0 - center.0, point.1 - center.1);
    let r = dx.hypot(dy);
    if n == 0 || !(INNER..=OUTER + SLACK).contains(&r) {
        return None;
    }
    let width = TAU / n as f64;
    // 0 at the top, growing clockwise (screen y points down).
    let angle = (dx.atan2(-dy) + width / 2.0).rem_euclid(TAU);
    Some(((angle / width) as usize).min(n - 1))
}

/// Keeps the wheel fully inside a `w` × `h` area.
pub fn clamp_center(center: (f64, f64), w: f64, h: f64) -> (f64, f64) {
    let m = OUTER + 8.0;
    let fit = |v: f64, size: f64| if size <= 2.0 * m { size / 2.0 } else { v.clamp(m, size - m) };
    (fit(center.0, w), fit(center.1, h))
}

const ACCENT: (f64, f64, f64) = (0.996, 0.502, 0.098);

pub fn draw(cr: &cairo::Context, wheel: &Wheel, dark: bool) {
    if !wheel.open {
        return;
    }
    let (cx, cy) = wheel.center;
    let n = wheel.slices.len();
    let (bg, fg) =
        if dark { ((0.16, 0.16, 0.17), (0.95, 0.95, 0.95)) } else { ((0.98, 0.97, 0.96), (0.13, 0.13, 0.13)) };
    let _ = cr.save();

    // Soft shadow, then the disc.
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.18);
    cr.arc(cx, cy + 4.0, OUTER + 4.0, 0.0, TAU);
    let _ = cr.fill();
    cr.set_source_rgb(bg.0, bg.1, bg.2);
    cr.arc(cx, cy, OUTER, 0.0, TAU);
    let _ = cr.fill();

    let width = if n > 0 { TAU / n as f64 } else { TAU };
    for (i, slice) in wheel.slices.iter().enumerate() {
        // Cairo angles start at 3 o'clock; slice 0 is centred at 12.
        let mid = -PI / 2.0 + i as f64 * width;
        let (a0, a1) = (mid - width / 2.0, mid + width / 2.0);
        if wheel.hovered == Some(i) {
            let (r, g, b) = ACCENT;
            cr.set_source_rgba(r, g, b, if slice.missing.is_some() { 0.35 } else { 1.0 });
            cr.arc(cx, cy, OUTER - 3.0, a0, a1);
            cr.arc_negative(cx, cy, INNER + 3.0, a1, a0);
            cr.close_path();
            let _ = cr.fill();
        }
        if n > 1 {
            cr.set_source_rgba(fg.0, fg.1, fg.2, 0.10);
            cr.set_line_width(1.0);
            cr.move_to(cx + INNER * a0.cos(), cy + INNER * a0.sin());
            cr.line_to(cx + OUTER * a0.cos(), cy + OUTER * a0.sin());
            let _ = cr.stroke();
        }

        let text_colour = if wheel.hovered == Some(i) && slice.missing.is_none() { (1.0, 1.0, 1.0) } else { fg };
        let alpha = if slice.missing.is_some() { 0.35 } else { 1.0 };
        cr.set_source_rgba(text_colour.0, text_colour.1, text_colour.2, alpha);
        let r = (INNER + OUTER) / 2.0 + 4.0;
        let size = if n > 8 { 11.0 } else { 13.0 };
        centred_text(cr, &slice.label, cx + r * mid.cos(), cy + r * mid.sin(), size, true);
    }

    // The hub: what this wheel is, or why it is empty.
    cr.set_source_rgba(fg.0, fg.1, fg.2, 0.06);
    cr.arc(cx, cy, INNER, 0.0, TAU);
    let _ = cr.fill();
    cr.set_source_rgba(fg.0, fg.1, fg.2, 0.75);
    let title = wheel.mode.map(Mode::title).unwrap_or_default();
    centred_text(cr, title, cx, cy - if n == 0 { 8.0 } else { 0.0 }, 12.0, true);
    if n == 0 {
        centred_text(cr, "nothing here", cx, cy + 9.0, 10.0, false);
    }

    // Say why a greyed-out slice is grey.
    if let Some(missing) = wheel.hovered.and_then(|i| wheel.slices[i].missing.as_deref()) {
        cr.set_source_rgba(fg.0, fg.1, fg.2, 0.85);
        centred_text(cr, &format!("needs {missing}"), cx, cy + OUTER + 22.0, 12.0, false);
    }
    let _ = cr.restore();
}

fn centred_text(cr: &cairo::Context, text: &str, x: f64, y: f64, size: f64, bold: bool) {
    let weight = if bold { cairo::FontWeight::Bold } else { cairo::FontWeight::Normal };
    cr.select_font_face("Sans", cairo::FontSlant::Normal, weight);
    cr.set_font_size(size);
    if let Ok(ext) = cr.text_extents(text) {
        cr.move_to(x - ext.width() / 2.0 - ext.x_bearing(), y - ext.height() / 2.0 - ext.y_bearing());
        let _ = cr.show_text(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_go_clockwise_from_the_top() {
        let c = (200.0, 200.0);
        assert_eq!(slice_at(4, c, (200.0, 100.0)), Some(0)); // up
        assert_eq!(slice_at(4, c, (300.0, 200.0)), Some(1)); // right
        assert_eq!(slice_at(4, c, (200.0, 300.0)), Some(2)); // down
        assert_eq!(slice_at(4, c, (100.0, 200.0)), Some(3)); // left
    }

    #[test]
    fn hub_and_far_away_pick_nothing() {
        let c = (0.0, 0.0);
        assert_eq!(slice_at(6, c, (10.0, 10.0)), None);
        assert_eq!(slice_at(6, c, (0.0, -(OUTER + SLACK + 1.0))), None);
        assert_eq!(slice_at(0, c, (0.0, -100.0)), None);
    }

    #[test]
    fn wheel_stays_on_screen() {
        assert_eq!(clamp_center((0.0, 1000.0), 800.0, 600.0), (OUTER + 8.0, 600.0 - OUTER - 8.0));
        assert_eq!(clamp_center((5.0, 5.0), 200.0, 200.0), (100.0, 100.0));
    }
}
