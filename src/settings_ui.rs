//! The Settings window: a small modal dialog. Every change applies and is saved at once.

use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::catalog::Mode;
use crate::config::{self, Modifier};

const WHEELS: [(Mode, &str); 3] =
    [(Mode::Convert, "Convert wheel"), (Mode::Tools, "Tools wheel"), (Mode::Actions, "Actions wheel")];

fn row(title: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    line.append(&label);
    line.append(control);
    line
}

fn heading(text: &str) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_markup(&format!("<b>{}</b>", glib::markup_escape_text(text)));
    l.set_xalign(0.0);
    l.add_css_class("dim");
    l.set_margin_top(12);
    l
}

/// Shows the Settings window over `parent`. `changed` runs after every change, so the main
/// window can refresh its hints.
pub fn open(parent: &gtk::Window, changed: impl Fn() + 'static) {
    let changed: Rc<dyn Fn()> = Rc::new(changed);
    let window = gtk::Window::builder()
        .title("Settings")
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .default_width(380)
        .build();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.set_margin_start(16);
    page.set_margin_end(16);
    page.set_margin_top(16);
    page.set_margin_bottom(16);
    let cfg = config::get();

    // Modifier keys: one drop-down per wheel; a key another wheel already uses is refused.
    page.append(&heading("Modifier keys"));
    let note = gtk::Label::new(None);
    note.set_xalign(0.0);
    note.set_wrap(true);
    note.add_css_class("error");
    let names: Vec<&str> = Modifier::ALL.iter().map(|m| m.label()).collect();
    for (mode, title) in WHEELS {
        let dropdown = gtk::DropDown::from_strings(&names);
        dropdown.set_selected(Modifier::ALL.iter().position(|m| *m == cfg.key_for(mode)).unwrap_or(0) as u32);
        dropdown.set_width_request(100);
        page.append(&row(title, &dropdown));
        let (note, changed) = (note.clone(), changed.clone());
        dropdown.connect_selected_notify(move |d| {
            let key = Modifier::ALL[(d.selected() as usize).min(Modifier::ALL.len() - 1)];
            if config::get().key_for(mode) == key {
                return; // a repeat notification, or the drop-down being put back
            }
            let mut accepted = false;
            config::update(|c| accepted = c.assign(mode, key));
            if accepted {
                note.set_text("");
                changed();
            } else {
                // Put the drop-down back on the key this wheel really has.
                let current = config::get().key_for(mode);
                d.set_selected(Modifier::ALL.iter().position(|m| *m == current).unwrap_or(0) as u32);
                note.set_text(&format!("{} already opens another wheel.", key.label()));
            }
        });
    }
    page.append(&note);

    // Output location.
    page.append(&heading("Results"));
    let beside = gtk::CheckButton::with_label("Next to the original");
    let folder_radio = gtk::CheckButton::with_label("In a folder");
    folder_radio.set_group(Some(&beside));
    let folder_label = gtk::Label::new(None);
    folder_label.set_xalign(0.0);
    folder_label.set_hexpand(true);
    folder_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    folder_label.add_css_class("dim");
    let choose = gtk::Button::with_label("Choose…");
    let show_folder = {
        let (label, choose) = (folder_label.clone(), choose.clone());
        move |dir: &Option<PathBuf>| {
            label.set_text(&dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default());
            label.set_tooltip_text(dir.as_ref().map(|d| d.display().to_string()).as_deref());
            choose.set_sensitive(dir.is_some());
        }
    };
    show_folder(&cfg.output_dir);
    if cfg.output_dir.is_some() {
        folder_radio.set_active(true);
    } else {
        beside.set_active(true);
    }
    page.append(&beside);
    page.append(&folder_radio);
    let pick = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    pick.set_margin_start(26);
    pick.append(&folder_label);
    pick.append(&choose);
    page.append(&pick);

    let show_folder = Rc::new(show_folder);
    let chooser = {
        let (window, show_folder, changed, beside, folder_radio) =
            (window.clone(), show_folder.clone(), changed.clone(), beside.clone(), folder_radio.clone());
        Rc::new(move || {
            let dialog = gtk::FileDialog::builder().title("Save results in").modal(true).build();
            if let Some(dir) = config::get().output_dir {
                dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
            }
            let (show_folder, changed, beside, folder_radio) =
                (show_folder.clone(), changed.clone(), beside.clone(), folder_radio.clone());
            dialog.select_folder(Some(&window), gio::Cancellable::NONE, move |result| {
                let dir = result.ok().and_then(|f| f.path());
                match dir {
                    Some(dir) => {
                        config::update(|c| c.output_dir = Some(dir));
                        changed();
                    }
                    // Cancelled with no folder yet: stay with "next to the original".
                    None if config::get().output_dir.is_none() => beside.set_active(true),
                    None => {}
                }
                let current = config::get().output_dir;
                show_folder(&current);
                if current.is_some() {
                    folder_radio.set_active(true);
                }
            });
        })
    };
    {
        let (chooser, show_folder, changed) = (chooser.clone(), show_folder.clone(), changed.clone());
        folder_radio.connect_toggled(move |b| {
            if b.is_active() && config::get().output_dir.is_none() {
                chooser();
            } else if !b.is_active() {
                config::update(|c| c.output_dir = None);
                show_folder(&None);
                changed();
            }
        });
    }
    {
        let chooser = chooser.clone();
        choose.connect_clicked(move |_| chooser());
    }

    // The shelf.
    let shelf = gtk::Switch::new();
    shelf.set_active(cfg.add_results_to_shelf);
    shelf.set_valign(gtk::Align::Center);
    shelf.connect_active_notify({
        let changed = changed.clone();
        move |s| {
            config::update(|c| c.add_results_to_shelf = s.is_active());
            changed();
        }
    });
    page.append(&heading("Shelf"));
    page.append(&row("Add results to the shelf", &shelf));

    let done = gtk::Button::with_label("Close");
    done.set_halign(gtk::Align::End);
    done.set_margin_top(12);
    done.connect_clicked({
        let window = window.clone();
        move |_| window.close()
    });
    page.append(&done);

    let esc = gtk::EventControllerKey::new();
    esc.connect_key_pressed({
        let window = window.clone();
        move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    });
    window.add_controller(esc);
    window.set_child(Some(&page));
    window.present();
}
