//! The radial menu: its state, its geometry, how it is drawn and how it is spoken.
//!
//! The wheel is painted with Cairo on a `DrawingArea`, so it has no widgets of its own for
//! a screen reader to find. [`sync`] makes up for that: it names the area after the open
//! wheel, describes the selected slice, and announces every change.

use std::f64::consts::{PI, TAU};
use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{cairo, gdk, pango};

use crate::backend;
use crate::catalog::{Mode, Op, options};

/// Radii at the default text size; they grow with larger text (see [`Geometry::scaled`]).
const BASE_INNER: f64 = 46.0;
const BASE_OUTER: f64 = 150.0;
/// How far past the rim the pointer may stray and still pick a slice.
const BASE_SLACK: f64 = 60.0;
/// The logical height of a line of the default UI font (Cantarell or DejaVu Sans, 11 pt at 96 dpi).
const BASE_LINE: f64 = 17.0;
/// Labels shrink to no less than this before they are ellipsized.
const MIN_LABEL_SCALE: f64 = 0.75;
/// WCAG AA for normal text.
pub const MIN_CONTRAST: f64 = 4.5;

/// The wheel's size, scaled with the text so that larger fonts get a larger wheel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub inner: f64,
    pub outer: f64,
    pub slack: f64,
    pub scale: f64,
}

impl Geometry {
    pub fn scaled(scale: f64) -> Geometry {
        Geometry { inner: BASE_INNER * scale, outer: BASE_OUTER * scale, slack: BASE_SLACK * scale, scale }
    }
}

impl Default for Geometry {
    fn default() -> Geometry {
        Geometry::scaled(1.0)
    }
}

/// How much bigger than the default the wheel should be, given the UI font's line height.
/// Never smaller than the default, never more than twice it.
pub fn scale_for_line_height(line: f64) -> f64 {
    if line.is_finite() { (line / BASE_LINE).clamp(1.0, 2.0) } else { 1.0 }
}

/// The wheel geometry for the font `widget` draws with (system font, large-text setting).
pub fn geometry_for(widget: &impl IsA<gtk::Widget>) -> Geometry {
    let (_, h) = widget.create_pango_layout(Some("Mg")).pixel_size();
    Geometry::scaled(scale_for_line_height(h as f64))
}

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
    pub geo: Geometry,
    pub files: Vec<PathBuf>,
    pub slices: Vec<Slice>,
    pub hovered: Option<usize>,
    /// Opened from the keyboard or a click: keys move the selection and Enter runs it.
    pub keyboard: bool,
    /// Where keyboard focus goes back to when the wheel closes.
    pub return_focus: Option<gtk::Widget>,
    /// What was last announced: (mode, selection, slice count).
    spoken: Option<(Option<Mode>, Option<usize>, usize)>,
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
        self.hovered = slice_at(self.slices.len(), self.geo, self.center, (x, y));
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

    /// Selects slice `i` (greyed-out ones too, so the reason can be read out). False if there is none.
    pub fn select(&mut self, i: usize) -> bool {
        let ok = i < self.slices.len();
        if ok {
            self.hovered = Some(i);
        }
        ok
    }

    /// Home and End: the first or last slice that can run.
    pub fn select_edge(&mut self, last: bool) {
        let found = if last {
            self.slices.iter().rposition(|s| s.missing.is_none())
        } else {
            self.slices.iter().position(|s| s.missing.is_none())
        };
        if found.is_some() {
            self.hovered = found;
        }
    }

    /// The op under the pointer, if it can run.
    pub fn chosen(&self) -> Option<Op> {
        let s = self.slices.get(self.hovered?)?;
        s.missing.is_none().then_some(s.op)
    }

    /// "Convert wheel, 7 options"
    pub fn summary(&self) -> String {
        let title = self.mode.map(Mode::title).unwrap_or("Conversion");
        match self.slices.len() {
            0 => format!("{title} wheel, nothing here for these files"),
            1 => format!("{title} wheel, 1 option"),
            n => format!("{title} wheel, {n} options"),
        }
    }

    /// "PNG, 2 of 7", or "WEBP, 3 of 7, unavailable: needs ffmpeg".
    pub fn selection_text(&self) -> Option<String> {
        let i = self.hovered?;
        let s = self.slices.get(i)?;
        let label = s.label.trim_end_matches('…');
        let n = self.slices.len();
        Some(match &s.missing {
            Some(m) => format!("{label}, {} of {n}, unavailable: needs {m}", i + 1),
            None => format!("{label}, {} of {n}", i + 1),
        })
    }

    /// What to say now, if anything changed since the last call: the whole wheel when it
    /// opens or switches, just the selection when only that moved.
    pub fn speech(&mut self) -> Option<String> {
        if !self.open {
            self.spoken = None;
            return None;
        }
        let now = (self.mode, self.hovered, self.slices.len());
        if self.spoken == Some(now) {
            return None;
        }
        let fresh = self.spoken.map(|(m, _, n)| (m, n)) != Some((now.0, now.2));
        self.spoken = Some(now);
        match (fresh, self.selection_text()) {
            (true, Some(sel)) => Some(format!("{}. {sel}", self.summary())),
            (true, None) => Some(self.summary()),
            (false, sel) => sel,
        }
    }
}

/// `'1'`–`'9'` pick slices 0–8.
pub fn digit_slice(c: char) -> Option<usize> {
    c.to_digit(10).filter(|d| (1..=9).contains(d)).map(|d| d as usize - 1)
}

/// Which of `n` slices (the first centred at the top, going clockwise) holds `point`.
/// The hub picks nothing, and so does anything well past the rim.
pub fn slice_at(n: usize, geo: Geometry, center: (f64, f64), point: (f64, f64)) -> Option<usize> {
    let (dx, dy) = (point.0 - center.0, point.1 - center.1);
    let r = dx.hypot(dy);
    if n == 0 || !(geo.inner..=geo.outer + geo.slack).contains(&r) {
        return None;
    }
    let width = TAU / n as f64;
    // 0 at the top, growing clockwise (screen y points down).
    let angle = (dx.atan2(-dy) + width / 2.0).rem_euclid(TAU);
    Some(((angle / width) as usize).min(n - 1))
}

/// Keeps the wheel fully inside a `w` × `h` area.
pub fn clamp_center(center: (f64, f64), geo: Geometry, w: f64, h: f64) -> (f64, f64) {
    let m = geo.outer + 8.0;
    let fit = |v: f64, size: f64| if size <= 2.0 * m { size / 2.0 } else { v.clamp(m, size - m) };
    (fit(center.0, w), fit(center.1, h))
}

/// How wide a slice label may be: the chord across the slice at the label's radius, or the
/// ring's thickness, whichever is smaller, less some padding.
pub fn label_width(n: usize, geo: Geometry) -> f64 {
    let ring = geo.outer - geo.inner;
    let r = (geo.inner + geo.outer) / 2.0;
    let chord = if n <= 2 { 2.0 * r } else { 2.0 * r * (PI / n as f64).sin() };
    0.86 * chord.min(ring)
}

/// How a label is fitted into its slice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fit {
    /// It fits on one line.
    One,
    /// It fits once wrapped onto at most `max_lines` lines.
    Wrap,
    /// Shrink the font by this factor, wrap, and ellipsize whatever still does not fit.
    Shrink(f64),
}

/// Pure: `natural` is the unwrapped width; `wrapped` and `lines` are the width and line count
/// once wrapped at `avail`.
pub fn fit_label(natural: f64, wrapped: f64, lines: usize, avail: f64, max_lines: usize) -> Fit {
    if natural <= avail {
        return Fit::One;
    }
    let max_lines = max_lines.max(1);
    if max_lines > 1 && lines <= max_lines && wrapped <= avail {
        return Fit::Wrap;
    }
    let by_width = if max_lines > 1 { avail / wrapped.max(avail) } else { avail / natural };
    let by_lines = if max_lines > 1 { max_lines as f64 / lines.max(max_lines) as f64 } else { 1.0 };
    Fit::Shrink(by_width.min(by_lines).clamp(MIN_LABEL_SCALE, 1.0))
}

// ---------------------------------------------------------------------------------------------
// Colour

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub f64, pub f64, pub f64);

impl Rgb {
    pub const BLACK: Rgb = Rgb(0.0, 0.0, 0.0);
    pub const WHITE: Rgb = Rgb(1.0, 1.0, 1.0);

    fn from_gdk(c: &gdk::RGBA) -> Rgb {
        Rgb(c.red() as f64, c.green() as f64, c.blue() as f64)
    }
}

/// `a` moved towards `b` by `t` (0 = `a`, 1 = `b`). Also composites `b` with alpha `t` over `a`.
pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    Rgb(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t)
}

/// WCAG relative luminance.
pub fn luminance(c: Rgb) -> f64 {
    let lin = |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
}

/// WCAG contrast ratio, 1 to 21.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `fg`, pushed towards black or white until it reads on `bg` at `min`.
pub fn readable(fg: Rgb, bg: Rgb, min: f64) -> Rgb {
    if contrast(fg, bg) >= min {
        return fg;
    }
    let toward = if contrast(Rgb::BLACK, bg) >= contrast(Rgb::WHITE, bg) { Rgb::BLACK } else { Rgb::WHITE };
    (1..=20).map(|i| mix(fg, toward, i as f64 / 20.0)).find(|c| contrast(*c, bg) >= min).unwrap_or(toward)
}

/// A selection colour and the text on it: `bg` is darkened or lightened (away from `fg`)
/// until the pair reads at `min`; if that is not enough, the text goes black or white.
pub fn readable_pair(bg: Rgb, fg: Rgb, min: f64) -> (Rgb, Rgb) {
    if contrast(fg, bg) >= min {
        return (bg, fg);
    }
    let away = if luminance(fg) > luminance(bg) { Rgb::BLACK } else { Rgb::WHITE };
    if let Some(b) = (1..=12).map(|i| mix(bg, away, i as f64 / 20.0)).find(|b| contrast(fg, *b) >= min) {
        return (b, fg);
    }
    (bg, readable(fg, bg, min))
}

/// The wheel's colours, all taken from the theme and then checked for contrast.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub disc: Rgb,
    pub text: Rgb,
    /// Labels of slices that cannot run (exempt from the contrast rule, but still legible).
    pub dim: Rgb,
    pub accent: Rgb,
    pub on_accent: Rgb,
    /// Dividers and the rim.
    pub line: Rgb,
    pub high_contrast: bool,
}

/// Susen's accent: a muted purple. White on it is 5.9:1.
pub const ACCENT_LIGHT: Rgb = Rgb(0x6b as f64 / 255.0, 0x5b as f64 / 255.0, 0x95 as f64 / 255.0);
/// A lighter muted purple for dark themes. White on it is only 3.8:1, so its label is black (5.5:1).
pub const ACCENT_DARK: Rgb = Rgb(0x8a as f64 / 255.0, 0x7b as f64 / 255.0, 0xb0 as f64 / 255.0);

/// Black or white, whichever reads better on `bg`.
fn best_text(bg: Rgb) -> Rgb {
    if contrast(Rgb::WHITE, bg) >= contrast(Rgb::BLACK, bg) { Rgb::WHITE } else { Rgb::BLACK }
}

/// Builds the palette from theme colours. `fg_alpha` is the alpha of the theme's text colour
/// (Adwaita draws text at 80 % black). Missing theme colours fall back to neutral ones.
/// The selection is Susen's purple accent; only a high-contrast theme's own accent
/// (`accent`, `on_accent`) replaces it.
pub fn palette(
    fg: Rgb,
    fg_alpha: f64,
    bg: Option<Rgb>,
    accent: Option<Rgb>,
    on_accent: Option<Rgb>,
    high_contrast: bool,
) -> Palette {
    let light_text = luminance(fg) > 0.4;
    let disc = bg.unwrap_or(if light_text { Rgb(0.16, 0.16, 0.17) } else { Rgb(0.98, 0.98, 0.98) });
    let text = readable(mix(disc, fg, fg_alpha), disc, MIN_CONTRAST);
    let dark = luminance(disc) < 0.2;
    let (accent, on_accent) = match (high_contrast, accent) {
        (true, Some(a)) => (a, on_accent.unwrap_or_else(|| best_text(a))),
        _ if dark => (ACCENT_DARK, best_text(ACCENT_DARK)),
        _ => (ACCENT_LIGHT, Rgb::WHITE),
    };
    let (accent, on_accent) = readable_pair(accent, on_accent, MIN_CONTRAST);
    Palette {
        disc,
        text,
        dim: mix(disc, text, 0.55),
        accent,
        on_accent,
        line: mix(disc, text, if high_contrast { 1.0 } else { 0.22 }),
        high_contrast,
    }
}

#[allow(deprecated)] // There is no other way to read a theme's named colours in GTK 4.14.
fn theme_colour(widget: &gtk::Widget, names: &[&str]) -> Option<Rgb> {
    let style = widget.style_context();
    names.iter().find_map(|n| style.lookup_color(n)).map(|c| Rgb::from_gdk(&c))
}

fn is_high_contrast(widget: &gtk::Widget) -> bool {
    let name = widget.settings().gtk_theme_name().map(|s| s.to_string()).unwrap_or_default();
    let env = std::env::var("GTK_THEME").unwrap_or_default();
    [name, env].iter().any(|t| t.to_lowercase().contains("highcontrast"))
}

/// The palette for the theme `widget` is drawn with.
pub fn palette_for(widget: &impl IsA<gtk::Widget>) -> Palette {
    let w = widget.upcast_ref::<gtk::Widget>();
    let fg = w.color();
    palette(
        Rgb::from_gdk(&fg),
        fg.alpha() as f64,
        theme_colour(w, &["popover_bg_color", "window_bg_color", "theme_bg_color"]),
        theme_colour(w, &["accent_bg_color", "theme_selected_bg_color"]),
        theme_colour(w, &["accent_fg_color", "theme_selected_fg_color"]),
        is_high_contrast(w),
    )
}

// ---------------------------------------------------------------------------------------------
// Drawing

fn set(cr: &cairo::Context, c: Rgb, alpha: f64) {
    cr.set_source_rgba(c.0, c.1, c.2, alpha);
}

/// A layout in the widget's own font (so the system font, its size and scaling apply).
fn layout(area: &gtk::DrawingArea, text: &str, bold: bool, rel: f64) -> pango::Layout {
    let layout = area.create_pango_layout(Some(text));
    let mut fd = area.pango_context().font_description().unwrap_or_default();
    if bold {
        fd.set_weight(pango::Weight::Bold);
    }
    let size = (fd.size() as f64 * rel).round() as i32;
    if fd.is_size_absolute() {
        fd.set_absolute_size(size as f64);
    } else {
        fd.set_size(size.max(pango::SCALE));
    }
    layout.set_font_description(Some(&fd));
    layout.set_alignment(pango::Alignment::Center);
    layout
}

/// Draws `text` centred on (x, y), fitted into `avail` pixels and `max_lines` lines.
#[allow(clippy::too_many_arguments)]
fn centred_text(
    area: &gtk::DrawingArea,
    cr: &cairo::Context,
    text: &str,
    (x, y): (f64, f64),
    bold: bool,
    rel: f64,
    avail: f64,
    max_lines: usize,
) {
    let avail_pango = (avail.max(1.0) * pango::SCALE as f64) as i32;
    let mut l = layout(area, text, bold, rel);
    let (natural, _) = l.pixel_size();
    let fit = if natural as f64 <= avail {
        Fit::One
    } else {
        l.set_width(avail_pango);
        l.set_wrap(pango::WrapMode::Word);
        let (wrapped, _) = l.pixel_size();
        fit_label(natural as f64, wrapped as f64, l.line_count() as usize, avail, max_lines)
    };
    if let Fit::Shrink(s) = fit {
        l = layout(area, text, bold, rel * s);
    }
    if fit != Fit::One {
        l.set_width(avail_pango);
        l.set_wrap(pango::WrapMode::Word);
        l.set_height(-(max_lines.max(1) as i32));
        l.set_ellipsize(pango::EllipsizeMode::End);
    }
    let (_, logical) = l.pixel_extents();
    cr.move_to(
        x - logical.x() as f64 - logical.width() as f64 / 2.0,
        y - logical.y() as f64 - logical.height() as f64 / 2.0,
    );
    pangocairo::functions::show_layout(cr, &l);
}

fn wedge(cr: &cairo::Context, c: (f64, f64), r0: f64, r1: f64, a0: f64, a1: f64) {
    cr.new_path();
    cr.arc(c.0, c.1, r1, a0, a1);
    cr.arc_negative(c.0, c.1, r0, a1, a0);
    cr.close_path();
}

pub fn draw(area: &gtk::DrawingArea, cr: &cairo::Context, wheel: &Wheel) {
    if !wheel.open {
        return;
    }
    let pal = palette_for(area);
    let g = wheel.geo;
    let s = g.scale;
    let (cx, cy) = wheel.center;
    let n = wheel.slices.len();
    let _ = cr.save();

    // Soft shadow, then the disc and its rim.
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.18);
    cr.arc(cx, cy + 4.0 * s, g.outer + 4.0 * s, 0.0, TAU);
    let _ = cr.fill();
    set(cr, pal.disc, 1.0);
    cr.arc(cx, cy, g.outer, 0.0, TAU);
    let _ = cr.fill_preserve();
    set(cr, pal.line, 1.0);
    cr.set_line_width(if pal.high_contrast { 2.0 } else { 1.0 });
    let _ = cr.stroke();

    let width = if n > 0 { TAU / n as f64 } else { TAU };
    let avail = label_width(n, g);
    let rel = if n > 8 { 0.9 } else { 1.0 };
    for (i, slice) in wheel.slices.iter().enumerate() {
        // Cairo angles start at 3 o'clock; slice 0 is centred at 12.
        let mid = -PI / 2.0 + i as f64 * width;
        let (a0, a1) = (mid - width / 2.0, mid + width / 2.0);
        let selected = wheel.hovered == Some(i);
        if selected {
            set(cr, pal.accent, if slice.missing.is_some() { 0.35 } else { 1.0 });
            wedge(cr, (cx, cy), g.inner + 3.0 * s, g.outer - 3.0 * s, a0, a1);
            let _ = cr.fill_preserve();
            // In high contrast the selection is outlined too, not only coloured.
            set(cr, pal.text, if pal.high_contrast { 1.0 } else { 0.0 });
            cr.set_line_width(2.0 * s);
            let _ = cr.stroke();
        }
        if n > 1 {
            set(cr, pal.line, 1.0);
            cr.set_line_width(1.0);
            cr.move_to(cx + g.inner * a0.cos(), cy + g.inner * a0.sin());
            cr.line_to(cx + g.outer * a0.cos(), cy + g.outer * a0.sin());
            let _ = cr.stroke();
        }

        let colour = match (selected, slice.missing.is_some()) {
            (_, true) => pal.dim,
            (true, false) => pal.on_accent,
            (false, false) => pal.text,
        };
        set(cr, colour, 1.0);
        let r = (g.inner + g.outer) / 2.0 + 4.0 * s;
        centred_text(area, cr, &slice.label, (cx + r * mid.cos(), cy + r * mid.sin()), true, rel, avail, 2);
    }

    // The hub: what this wheel is, or why it is empty.
    set(cr, pal.text, 0.06);
    cr.arc(cx, cy, g.inner, 0.0, TAU);
    let _ = cr.fill();
    set(cr, pal.text, 1.0);
    let title = wheel.mode.map(Mode::title).unwrap_or_default();
    let hub = 1.7 * g.inner;
    centred_text(area, cr, title, (cx, cy - if n == 0 { 8.0 * s } else { 0.0 }), true, 0.95, hub, 1);
    if n == 0 {
        centred_text(area, cr, "nothing here", (cx, cy + 9.0 * s), false, 0.8, hub, 1);
    }

    // Say why a greyed-out slice is grey, below the wheel or above it when there is no room.
    if let Some(missing) = wheel.hovered.and_then(|i| wheel.slices.get(i)?.missing.as_deref()) {
        let below = cy + g.outer + 22.0 * s;
        let y = if below + 12.0 * s > area.height() as f64 { cy - g.outer - 22.0 * s } else { below };
        set(cr, pal.text, 1.0);
        centred_text(area, cr, &format!("needs {missing}"), (cx, y), false, 0.95, 2.0 * g.outer, 1);
    }
    let _ = cr.restore();
}

// ---------------------------------------------------------------------------------------------
// Accessibility

const HELP: &str = "Arrow keys or 1 to 9 choose, Enter runs, Tab switches wheel, Escape cancels.";

/// GTK 4.14 crashes (calls a null function) when announcing through any accessibility
/// backend but AT-SPI, e.g. with `NO_AT_BRIDGE=1` or `GTK_A11Y=none`, so only AT-SPI is asked.
///
/// With `GTK_A11Y=none` a widget has no AT context at all, and gtk-rs' `at_context()` panics on
/// the null pointer (an abort, inside a GTK callback), so the context is fetched by hand.
fn can_announce(area: &gtk::DrawingArea) -> bool {
    use gtk::glib::translate::{ToGlibPtr, from_glib_full};
    let accessible = area.upcast_ref::<gtk::Accessible>();
    // SAFETY: a valid GtkAccessible; the result is a new reference or null.
    let ptr = unsafe { gtk::ffi::gtk_accessible_get_at_context(accessible.to_glib_none().0) };
    if ptr.is_null() {
        return false;
    }
    // SAFETY: non-null and owned (transfer full).
    let context: gtk::ATContext = unsafe { from_glib_full(ptr) };
    context.type_().name() == "GtkAtSpiContext"
}

/// Shows or hides the wheel's area, keeps its accessible name and description current, and
/// announces whatever changed (GTK 4.14 `gtk_accessible_announce`).
pub fn sync(area: &gtk::DrawingArea, wheel: &mut Wheel) {
    let appearing = wheel.open && !area.is_visible();
    area.set_visible(wheel.open);
    if wheel.open {
        let description = match wheel.selection_text() {
            Some(sel) => format!("{sel}. {HELP}"),
            None => HELP.to_string(),
        };
        area.update_property(&[
            gtk::accessible::Property::Label(&wheel.summary()),
            gtk::accessible::Property::Description(&description),
        ]);
        if let Some(text) = wheel.speech()
            && can_announce(area)
        {
            if appearing {
                // A widget that was hidden has no AT-SPI object yet; speak once it has.
                let area = area.clone();
                gtk::glib::idle_add_local_once(move || {
                    area.announce(&text, gtk::AccessibleAnnouncementPriority::Medium)
                });
            } else {
                area.announce(&text, gtk::AccessibleAnnouncementPriority::Medium);
            }
        }
    } else {
        wheel.speech();
    }
    area.queue_draw();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Tool;

    const G: Geometry = Geometry { inner: BASE_INNER, outer: BASE_OUTER, slack: BASE_SLACK, scale: 1.0 };

    #[test]
    fn slices_go_clockwise_from_the_top() {
        let c = (200.0, 200.0);
        assert_eq!(slice_at(4, G, c, (200.0, 100.0)), Some(0)); // up
        assert_eq!(slice_at(4, G, c, (300.0, 200.0)), Some(1)); // right
        assert_eq!(slice_at(4, G, c, (200.0, 300.0)), Some(2)); // down
        assert_eq!(slice_at(4, G, c, (100.0, 200.0)), Some(3)); // left
    }

    #[test]
    fn hub_and_far_away_pick_nothing() {
        let c = (0.0, 0.0);
        assert_eq!(slice_at(6, G, c, (10.0, 10.0)), None);
        assert_eq!(slice_at(6, G, c, (0.0, -(BASE_OUTER + BASE_SLACK + 1.0))), None);
        assert_eq!(slice_at(0, G, c, (0.0, -100.0)), None);
    }

    #[test]
    fn bigger_text_bigger_wheel() {
        let g = Geometry::scaled(scale_for_line_height(34.0));
        assert_eq!(g.outer, 2.0 * BASE_OUTER);
        assert_eq!(slice_at(4, g, (0.0, 0.0), (0.0, -1.5 * BASE_INNER)), None); // now in the hub
        assert_eq!(scale_for_line_height(10.0), 1.0);
        assert_eq!(scale_for_line_height(100.0), 2.0);
        assert_eq!(scale_for_line_height(f64::NAN), 1.0);
    }

    #[test]
    fn wheel_stays_on_screen() {
        assert_eq!(clamp_center((0.0, 1000.0), G, 800.0, 600.0), (BASE_OUTER + 8.0, 600.0 - BASE_OUTER - 8.0));
        assert_eq!(clamp_center((5.0, 5.0), G, 200.0, 200.0), (100.0, 100.0));
    }

    #[test]
    fn digits_pick_slices() {
        assert_eq!(digit_slice('1'), Some(0));
        assert_eq!(digit_slice('9'), Some(8));
        assert_eq!(digit_slice('0'), None);
        assert_eq!(digit_slice('a'), None);
    }

    fn wheel(labels: &[(&str, Option<&str>)]) -> Wheel {
        Wheel {
            open: true,
            mode: Some(Mode::Convert),
            slices: labels
                .iter()
                .map(|(l, m)| Slice { op: Op::Tool(Tool::Zip), label: l.to_string(), missing: m.map(Into::into) })
                .collect(),
            keyboard: true,
            ..Wheel::default()
        }
    }

    #[test]
    fn selection_keys() {
        let mut w = wheel(&[("A", Some("x")), ("B", None), ("C", None), ("D", Some("y"))]);
        w.select_edge(false);
        assert_eq!(w.hovered, Some(1));
        w.select_edge(true);
        assert_eq!(w.hovered, Some(2));
        assert!(w.select(3));
        assert_eq!(w.chosen(), None); // greyed out: selected, but will not run
        assert!(!w.select(4));
        assert_eq!(w.hovered, Some(3));
    }

    #[test]
    fn spoken_text() {
        let mut w = wheel(&[("PNG", None), ("WEBP", Some("ffmpeg")), ("Resize…", None)]);
        assert_eq!(w.summary(), "Convert wheel, 3 options");
        w.hovered = Some(0);
        assert_eq!(w.speech().as_deref(), Some("Convert wheel, 3 options. PNG, 1 of 3"));
        assert_eq!(w.speech(), None); // nothing changed
        w.hovered = Some(1);
        assert_eq!(w.speech().as_deref(), Some("WEBP, 2 of 3, unavailable: needs ffmpeg"));
        w.hovered = Some(2);
        assert_eq!(w.speech().as_deref(), Some("Resize, 3 of 3"));
        w.mode = Some(Mode::Tools);
        assert_eq!(w.speech().as_deref(), Some("Tools wheel, 3 options. Resize, 3 of 3"));
        w.slices.clear();
        w.hovered = None;
        assert_eq!(w.speech().as_deref(), Some("Tools wheel, nothing here for these files"));
    }

    #[test]
    fn labels_fit() {
        assert_eq!(fit_label(50.0, 50.0, 1, 80.0, 2), Fit::One);
        assert_eq!(fit_label(120.0, 70.0, 2, 80.0, 2), Fit::Wrap);
        // One unbreakable word, wider than the slice: shrink.
        assert_eq!(fit_label(100.0, 100.0, 1, 80.0, 2), Fit::Shrink(0.8));
        // Three lines where two fit: shrink by 2/3, but never below the floor.
        assert_eq!(fit_label(300.0, 80.0, 3, 80.0, 2), Fit::Shrink(MIN_LABEL_SCALE));
        // Single-line text (the hub) shrinks against its natural width.
        assert_eq!(fit_label(100.0, 0.0, 0, 90.0, 1), Fit::Shrink(0.9));
        assert_eq!(fit_label(1000.0, 0.0, 0, 90.0, 1), Fit::Shrink(MIN_LABEL_SCALE));
    }

    #[test]
    fn label_width_shrinks_with_more_slices() {
        assert!(label_width(12, G) < label_width(6, G));
        assert!(label_width(2, G) <= 0.86 * (BASE_OUTER - BASE_INNER) + 1e-9);
        assert!(label_width(7, G) > 60.0);
    }

    fn hex(v: u32) -> Rgb {
        Rgb(((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0)
    }

    #[test]
    fn contrast_math() {
        assert!((contrast(Rgb::BLACK, Rgb::WHITE) - 21.0).abs() < 1e-9);
        assert!((contrast(hex(0x777777), Rgb::WHITE) - 4.48).abs() < 0.01);
    }

    #[test]
    fn palettes_read() {
        let cases = [
            // GTK 4.14 Adwaita light: 80 % black text, white popovers, blue accent with white text (3.9:1).
            ("light", palette(Rgb::BLACK, 0.8, Some(hex(0xffffff)), Some(hex(0x3584e4)), Some(Rgb::WHITE), false)),
            ("dark", palette(Rgb::WHITE, 1.0, Some(hex(0x383838)), Some(hex(0x3584e4)), Some(Rgb::WHITE), false)),
            ("high contrast", palette(Rgb::BLACK, 1.0, Some(Rgb::WHITE), Some(hex(0x1c71d8)), Some(Rgb::WHITE), true)),
            ("no theme colours", palette(hex(0xeeeeec), 1.0, None, None, None, false)),
            (
                "bad theme",
                palette(hex(0x888888), 0.5, Some(hex(0x999999)), Some(hex(0xffff00)), Some(Rgb::WHITE), false),
            ),
        ];
        for (name, p) in cases {
            assert!(contrast(p.text, p.disc) >= MIN_CONTRAST, "{name}: text {}", contrast(p.text, p.disc));
            assert!(
                contrast(p.on_accent, p.accent) >= MIN_CONTRAST,
                "{name}: selection {}",
                contrast(p.on_accent, p.accent)
            );
        }
        // The purple accent, as the owner chose it: unchanged, because it already passes.
        let light = &cases[0].1;
        assert_eq!((light.accent, light.on_accent), (hex(0x6b5b95), Rgb::WHITE));
        assert!(contrast(Rgb::WHITE, hex(0x6b5b95)) > 5.8);
        let dark = &cases[1].1;
        assert_eq!((dark.accent, dark.on_accent), (hex(0x8a7bb0), Rgb::BLACK));
        assert!(contrast(Rgb::BLACK, hex(0x8a7bb0)) >= MIN_CONTRAST);
        assert!(contrast(Rgb::WHITE, hex(0x8a7bb0)) < MIN_CONTRAST); // why its label is black
        assert_eq!(dark.text, Rgb::WHITE);
        // High contrast keeps the theme's own selection colour.
        assert_eq!(cases[2].1.accent, hex(0x1c71d8));
    }
}
