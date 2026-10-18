//! The small modal window that asks a tool for its settings before it runs.
//!
//! What it knows about the files (length, size, pages) is probed off the UI thread
//! first; the checks themselves are [`params::validate`], which is pure and tested.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use crate::backend;
use crate::catalog::{Op, Tool};
use crate::params::{self, ASPECTS, Crop, Facts, Params, Resize, SPEEDS, SplitBy, format_time, parse_time};

type Reader = Box<dyn Fn() -> Result<Params, String>>;

/// Asks for `tool`'s settings in a modal window over `parent`, then calls `done` with them.
/// `done` is never called when the window is cancelled or closed.
pub fn ask(parent: &gtk::Window, tool: Tool, files: Vec<PathBuf>, done: impl FnOnce(Params) + 'static) {
    let parent = parent.clone();
    glib::spawn_future_local(async move {
        let probed = files.clone();
        let facts = gtk::gio::spawn_blocking(move || backend::facts(tool, &probed)).await.unwrap_or_default();
        show(&parent, tool, &files, facts, Box::new(done));
    });
}

fn show(parent: &gtk::Window, tool: Tool, files: &[PathBuf], facts: Facts, done: Box<dyn FnOnce(Params)>) {
    let title = Op::Tool(tool).label().trim_end_matches('…').to_string();
    let window = gtk::Window::builder()
        .title(&title)
        .modal(true)
        .transient_for(parent)
        .resizable(false)
        .default_width(360)
        .build();

    let what = match files {
        [one] => one.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        _ => format!("{} files", files.len()),
    };
    let heading = gtk::Label::new(Some(&format!("{title} {what}")));
    heading.set_xalign(0.0);
    heading.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    heading.add_css_class("heading");

    let grid = gtk::Grid::builder().row_spacing(8).column_spacing(12).build();
    let initial = params::defaults(tool, &facts);
    let read = fields(tool, &facts, initial, &grid);

    let info = gtk::Label::new(info_text(&facts, files.len()).as_deref());
    info.set_xalign(0.0);
    info.add_css_class("dim");
    let error = gtk::Label::new(None);
    error.set_xalign(0.0);
    error.set_wrap(true);
    error.add_css_class("error");

    let cancel = gtk::Button::with_label("Cancel");
    let run = gtk::Button::with_label(&title);
    run.add_css_class("suggested-action");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&run);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(&heading);
    content.append(&grid);
    if facts != Facts::default() {
        content.append(&info);
    }
    content.append(&error);
    content.append(&buttons);
    window.set_child(Some(&content));
    window.set_default_widget(Some(&run));

    let done = Rc::new(RefCell::new(Some(done)));
    let w = window.clone();
    run.connect_clicked(move |_| {
        let checked = read().and_then(|p| params::validate(&p, &facts).map(|()| p));
        match checked {
            Ok(p) => {
                w.close();
                if let Some(done) = done.borrow_mut().take() {
                    done(p);
                }
            }
            Err(msg) => error.set_text(&msg),
        }
    });
    let w = window.clone();
    cancel.connect_clicked(move |_| w.close());

    let keys = gtk::EventControllerKey::new();
    let w = window.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            w.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);
    window.present();
}

fn info_text(facts: &Facts, count: usize) -> Option<String> {
    let shortest = if count > 1 { " (shortest file)" } else { "" };
    if let Some(d) = facts.duration {
        return Some(format!("Length {}{shortest}", format_time(d)));
    }
    if let Some((w, h)) = facts.size {
        return Some(format!("{w} × {h} pixels"));
    }
    facts
        .pages
        .map(|n| format!("{n} page{}{}", if n == 1 { "" } else { "s" }, if count > 1 { " (fewest)" } else { "" }))
}

/// Adds the fields for `tool` to `grid` and returns how to read them back.
fn fields(tool: Tool, facts: &Facts, initial: Option<Params>, grid: &gtk::Grid) -> Reader {
    let row = Rc::new(std::cell::Cell::new(0));
    let add = {
        let grid = grid.clone();
        move |label: &str, widget: &gtk::Widget| {
            let l = gtk::Label::new(Some(label));
            l.set_xalign(0.0);
            let r = row.get();
            grid.attach(&l, 0, r, 1, 1);
            widget.set_hexpand(true);
            grid.attach(widget, 1, r, 1, 1);
            row.set(r + 1);
        }
    };
    let duration = facts.duration.unwrap_or(36_000.0);
    match (tool, initial) {
        (Tool::Trim, Some(Params::Trim { start, end })) => {
            let from = time_entry(start);
            let to = time_entry(end);
            add("Start", from.upcast_ref());
            add("End", to.upcast_ref());
            Box::new(move || Ok(Params::Trim { start: read_time(&from)?, end: read_time(&to)? }))
        }
        (Tool::Thumbnail, Some(Params::Thumbnail { at })) => {
            let at = time_entry(at);
            add("At", at.upcast_ref());
            Box::new(move || Ok(Params::Thumbnail { at: read_time(&at)? }))
        }
        (Tool::SplitMedia, _) => {
            let how = gtk::DropDown::from_strings(&["Into equal parts", "Every few seconds"]);
            let parts = spin(2.0, 100.0, 1.0, 2.0, 0);
            let every = spin(0.1, duration.max(0.1), 1.0, (duration / 2.0).clamp(0.1, 60.0).round().max(0.1), 1);
            every.set_sensitive(false);
            add("Split", how.upcast_ref());
            add("Parts", parts.upcast_ref());
            add("Seconds each", every.upcast_ref());
            let (p, e) = (parts.clone(), every.clone());
            how.connect_selected_notify(move |d| {
                p.set_sensitive(d.selected() == 0);
                e.set_sensitive(d.selected() == 1);
            });
            Box::new(move || {
                Ok(Params::Split(if how.selected() == 0 {
                    SplitBy::Parts(spin_value(&parts) as u32)
                } else {
                    SplitBy::Every(spin_value(&every))
                }))
            })
        }
        (Tool::Speed, _) => {
            let names: Vec<String> = SPEEDS.iter().map(|s| params::speed_name(*s).replace('x', "×")).collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let speed = gtk::DropDown::from_strings(&names);
            speed.set_selected(SPEEDS.iter().position(|s| *s == 2.0).unwrap_or(0) as u32);
            add("Speed", speed.upcast_ref());
            Box::new(move || Ok(Params::Speed(SPEEDS[speed.selected() as usize])))
        }
        (Tool::CropImage, _) => {
            let mut names: Vec<String> = ASPECTS.iter().map(|(a, b)| format!("{a}:{b}, centred")).collect();
            names.push("Custom rectangle".into());
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let shape = gtk::DropDown::from_strings(&names);
            let (w, h) = facts.size.unwrap_or((1000, 1000));
            let big = 100_000.0;
            let x = spin(0.0, big, 1.0, 0.0, 0);
            let y = spin(0.0, big, 1.0, 0.0, 0);
            let cw = spin(1.0, big, 1.0, f64::from(w), 0);
            let ch = spin(1.0, big, 1.0, f64::from(h), 0);
            add("Shape", shape.upcast_ref());
            add("Left", x.upcast_ref());
            add("Top", y.upcast_ref());
            add("Width", cw.upcast_ref());
            add("Height", ch.upcast_ref());
            let custom = ASPECTS.len() as u32;
            let rect = [x.clone(), y.clone(), cw.clone(), ch.clone()];
            for s in &rect {
                s.set_sensitive(false);
            }
            shape.connect_selected_notify(move |d| {
                for s in &rect {
                    s.set_sensitive(d.selected() == custom);
                }
            });
            Box::new(move || {
                let i = shape.selected() as usize;
                Ok(Params::Crop(match ASPECTS.get(i) {
                    Some(&(a, b)) => Crop::Aspect(a, b),
                    None => Crop::Rect {
                        x: spin_value(&x) as u32,
                        y: spin_value(&y) as u32,
                        w: spin_value(&cw) as u32,
                        h: spin_value(&ch) as u32,
                    },
                }))
            })
        }
        (Tool::Resize, _) => {
            let how = gtk::DropDown::from_strings(&["By percentage", "To fit inside"]);
            let percent = spin(1.0, 1000.0, 5.0, 50.0, 0);
            let (w, h) = facts.size.map(|(w, h)| (w / 2, h / 2)).unwrap_or((1920, 1080));
            let width = spin(1.0, 100_000.0, 10.0, f64::from(w.max(1)), 0);
            let height = spin(1.0, 100_000.0, 10.0, f64::from(h.max(1)), 0);
            width.set_sensitive(false);
            height.set_sensitive(false);
            add("Resize", how.upcast_ref());
            add("Percent", percent.upcast_ref());
            add("Width", width.upcast_ref());
            add("Height", height.upcast_ref());
            let (p, wi, he) = (percent.clone(), width.clone(), height.clone());
            how.connect_selected_notify(move |d| {
                p.set_sensitive(d.selected() == 0);
                wi.set_sensitive(d.selected() == 1);
                he.set_sensitive(d.selected() == 1);
            });
            Box::new(move || {
                Ok(Params::Resize(if how.selected() == 0 {
                    Resize::Percent(spin_value(&percent) as u32)
                } else {
                    Resize::Fit { width: spin_value(&width) as u32, height: spin_value(&height) as u32 }
                }))
            })
        }
        (Tool::FadeAudio, Some(Params::Fade { fade_in, fade_out })) => {
            let fin = spin(0.0, duration, 0.5, fade_in, 1);
            let fout = spin(0.0, duration, 0.5, fade_out, 1);
            add("Fade in (s)", fin.upcast_ref());
            add("Fade out (s)", fout.upcast_ref());
            Box::new(move || Ok(Params::Fade { fade_in: spin_value(&fin), fade_out: spin_value(&fout) }))
        }
        (Tool::RotatePdf, _) => {
            let angle = gtk::DropDown::from_strings(&["90° clockwise", "180°", "90° counter-clockwise"]);
            add("Turn every page", angle.upcast_ref());
            Box::new(move || Ok(Params::RotatePages([90, 180, 270][angle.selected() as usize % 3])))
        }
        (Tool::ExtractPages, Some(Params::Pages { first, last })) => {
            let max = f64::from(facts.pages.unwrap_or(9999));
            let from = spin(1.0, max, 1.0, f64::from(first), 0);
            let to = spin(1.0, max, 1.0, f64::from(last), 0);
            add("First page", from.upcast_ref());
            add("Last page", to.upcast_ref());
            Box::new(move || Ok(Params::Pages { first: spin_value(&from) as u32, last: spin_value(&to) as u32 }))
        }
        _ => Box::new(move || Err(format!("{tool:?} has no settings"))),
    }
}

fn time_entry(seconds: f64) -> gtk::Entry {
    let entry = gtk::Entry::new();
    entry.set_text(&format_time(seconds));
    entry.set_placeholder_text(Some("m:ss or seconds"));
    entry.set_activates_default(true);
    entry
}

fn read_time(entry: &gtk::Entry) -> Result<f64, String> {
    parse_time(&entry.text()).ok_or_else(|| format!("“{}” is not a time like 1:30 or 90.5", entry.text()))
}

fn spin(min: f64, max: f64, step: f64, value: f64, digits: u32) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(min, max, step);
    spin.set_digits(digits);
    spin.set_value(value);
    spin.set_activates_default(true);
    spin
}

/// The value as typed, even if Enter was pressed before the spin button noticed the text.
fn spin_value(spin: &gtk::SpinButton) -> f64 {
    spin.update();
    spin.value()
}
