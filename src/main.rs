//! Clementine: drag a file in, tap a modifier, drop it on a format.
//!
//! Shift opens the conversion wheel, Ctrl the tools wheel, Alt the actions wheel.
//! Wayland never tells a window about keys pressed while a drag from another app
//! hovers it, so the wheels can also be opened by dragging over the three chips
//! at the bottom of the window. Without a wheel, a drop just puts the files on
//! the shelf, where Enter opens the wheel from the keyboard.

mod actions;
mod backend;
mod catalog;
mod config;
mod params;
mod params_ui;
mod settings_ui;
mod wheel;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use catalog::{Mode, Op};
use wheel::Wheel;

const APP_ID: &str = "io.github.sphanx.Clementine";

const CSS: &str = r#"
.drop-hint { font-size: 1.4em; font-weight: bold; opacity: 0.8; }
.chip { padding: 6px 14px; border-radius: 999px; background: alpha(currentColor, 0.08); font-weight: bold; }
.chip.hot { background: #fe8019; color: white; }
.dim { opacity: 0.6; }
.section { font-weight: bold; opacity: 0.7; margin: 8px 12px 2px 12px; }
.error { color: #e01b24; }
"#;

struct App {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    area: gtk::DrawingArea,
    stack: gtk::Stack,
    shelf: gtk::ListBox,
    shelf_files: RefCell<Vec<PathBuf>>,
    jobs: gtk::ListBox,
    status: gtk::Label,
    chips: gtk::Box,
    hint: gtk::Label,
    wheel: RefCell<Wheel>,
    /// Files of the drag hovering the window; empty until GTK has read them.
    drag_files: RefCell<Vec<PathBuf>>,
    dragging: Cell<bool>,
    pointer: Cell<(f64, f64)>,
}

thread_local! {
    static STATE: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn state() -> Option<Rc<App>> {
    STATE.with(|s| s.borrow().clone())
}

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    app.connect_activate(|app| window(app).present());
    // `clementine file…` and "Open With" both land here.
    app.connect_open(|app, files, _| {
        window(app).present();
        if let Some(state) = state() {
            add_to_shelf(&state, files.iter().filter_map(|f| f.path()).collect());
        }
    });
    app.run()
}

/// The one window, built on first use.
fn window(app: &gtk::Application) -> gtk::ApplicationWindow {
    if let Some(state) = state() {
        return state.window.clone();
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("no display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    gtk::Window::set_default_icon_name(APP_ID);
    let state = build(app);
    STATE.with(|s| *s.borrow_mut() = Some(state.clone()));
    state.window.clone()
}

fn build(app: &gtk::Application) -> Rc<App> {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Clementine")
        .default_width(560)
        .default_height(620)
        .build();

    let header = gtk::HeaderBar::new();
    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.set_tooltip_text(Some("Add files"));
    header.pack_start(&add);
    let gear = gtk::Button::from_icon_name("emblem-system-symbolic");
    gear.set_tooltip_text(Some("Settings"));
    header.pack_end(&gear);
    window.set_titlebar(Some(&header));

    let empty = gtk::Box::new(gtk::Orientation::Vertical, 8);
    empty.set_valign(gtk::Align::Center);
    let icon = gtk::Image::from_icon_name("document-send-symbolic");
    icon.set_pixel_size(64);
    icon.add_css_class("dim");
    let title = gtk::Label::new(Some("Drop files here"));
    title.add_css_class("drop-hint");
    let sub = gtk::Label::new(None);
    sub.set_justify(gtk::Justification::Center);
    sub.add_css_class("dim");
    empty.append(&icon);
    empty.append(&title);
    empty.append(&sub);

    let shelf = gtk::ListBox::new();
    shelf.set_selection_mode(gtk::SelectionMode::Multiple);
    shelf.set_activate_on_single_click(false);
    let shelf_scroll = gtk::ScrolledWindow::builder().child(&shelf).vexpand(true).build();

    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&shelf_scroll, Some("shelf"));
    stack.set_vexpand(true);

    let jobs = gtk::ListBox::new();
    jobs.set_selection_mode(gtk::SelectionMode::None);
    let jobs_scroll = gtk::ScrolledWindow::builder().child(&jobs).min_content_height(130).build();
    let activity = gtk::Label::new(Some("Activity"));
    activity.set_xalign(0.0);
    activity.add_css_class("section");

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_margin_start(12);
    status.set_margin_end(12);
    status.set_margin_bottom(6);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.add_css_class("dim");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&stack);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&activity);
    content.append(&jobs_scroll);
    content.append(&status);

    // The wheel is painted over everything and never takes events itself.
    let area = gtk::DrawingArea::new();
    area.set_can_target(false);

    let chips = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    chips.set_halign(gtk::Align::Center);
    chips.set_valign(gtk::Align::End);
    chips.set_margin_bottom(18);
    chips.set_can_target(false);
    chips.set_visible(false);
    for mode in [Mode::Convert, Mode::Tools, Mode::Actions] {
        let chip = gtk::Label::new(None);
        chip.add_css_class("chip");
        chip.set_widget_name(mode.title());
        chips.append(&chip);
    }

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&content));
    overlay.add_overlay(&area);
    overlay.add_overlay(&chips);
    window.set_child(Some(&overlay));

    let state = Rc::new(App {
        window,
        overlay,
        area,
        stack,
        shelf,
        shelf_files: RefCell::new(Vec::new()),
        jobs,
        status,
        chips,
        hint: sub,
        wheel: RefCell::new(Wheel::default()),
        drag_files: RefCell::new(Vec::new()),
        dragging: Cell::new(false),
        pointer: Cell::new((0.0, 0.0)),
    });

    let weak = Rc::downgrade(&state);
    state.area.set_draw_func(move |area, cr, _, _| {
        let Some(state) = weak.upgrade() else { return };
        let fg = area.color();
        let dark = fg.red() + fg.green() + fg.blue() > 1.5;
        wheel::draw(cr, &state.wheel.borrow(), dark);
    });

    refresh_hints(&state);
    wire_drop(&state);
    wire_keys(&state);

    let weak = Rc::downgrade(&state);
    gear.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else { return };
        let weak = Rc::downgrade(&state);
        settings_ui::open(state.window.upcast_ref(), move || {
            if let Some(state) = weak.upgrade() {
                refresh_hints(&state);
            }
        });
    });

    let weak = Rc::downgrade(&state);
    state.shelf.connect_row_activated(move |_, row| {
        let Some(state) = weak.upgrade() else { return };
        if !row.is_selected() {
            state.shelf.unselect_all();
            state.shelf.select_row(Some(row));
        }
        open_keyboard_wheel(&state);
    });

    let weak = Rc::downgrade(&state);
    add.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else { return };
        let dialog = gtk::FileDialog::builder().title("Add files").modal(true).build();
        let weak = Rc::downgrade(&state);
        dialog.open_multiple(Some(&state.window), gio::Cancellable::NONE, move |result| {
            let (Some(state), Ok(list)) = (weak.upgrade(), result) else { return };
            let files = list.iter::<gio::File>().filter_map(|f| f.ok()?.path()).collect();
            add_to_shelf(&state, files);
        });
    });

    state
}

fn mode_for(mods: gdk::ModifierType) -> Option<Mode> {
    config::get().mode_for(mods)
}

/// Chip labels and the empty-state hint follow the configured modifier keys.
fn refresh_hints(state: &App) {
    let cfg = config::get();
    state.hint.set_text(&format!("{}\nEverything happens on this computer.", cfg.hint()));
    let mut child = state.chips.first_child();
    while let Some(chip) = child {
        if let (Some(label), Some(mode)) = (
            chip.downcast_ref::<gtk::Label>(),
            [Mode::Convert, Mode::Tools, Mode::Actions].into_iter().find(|m| m.title() == chip.widget_name()),
        ) {
            label.set_text(&cfg.chip_label(mode));
        }
        child = chip.next_sibling();
    }
}

fn files_of(value: &glib::Value) -> Vec<PathBuf> {
    value.get::<gdk::FileList>().map(|l| l.files().iter().filter_map(|f| f.path()).collect()).unwrap_or_default()
}

/// The chip under the pointer, lit up; the others go dark.
fn chip_at(state: &App, x: f64, y: f64) -> Option<Mode> {
    let point = gtk::graphene::Point::new(x as f32, y as f32);
    let mut found = None;
    let mut child = state.chips.first_child();
    while let Some(chip) = child {
        let hot = chip.compute_bounds(&state.overlay).is_some_and(|b| b.contains_point(&point));
        if hot {
            chip.add_css_class("hot");
            found = [Mode::Convert, Mode::Tools, Mode::Actions].into_iter().find(|m| m.title() == chip.widget_name());
        } else {
            chip.remove_css_class("hot");
        }
        child = chip.next_sibling();
    }
    found
}

fn open_wheel(state: &App, mode: Mode, at: (f64, f64), files: Vec<PathBuf>, keyboard: bool) {
    let center = wheel::clamp_center(at, state.overlay.width() as f64, state.overlay.height() as f64);
    state.wheel.borrow_mut().show(mode, center, files, keyboard);
    state.area.queue_draw();
}

fn close_wheel(state: &App) {
    state.wheel.borrow_mut().hide();
    state.area.queue_draw();
}

/// Called on every drag event over the window: opens, switches and aims the wheel.
fn track_drag(state: &App, target: &gtk::DropTarget, x: f64, y: f64) {
    state.dragging.set(true);
    state.pointer.set((x, y));
    state.chips.set_visible(true);
    if let Some(files) = target.value().map(|v| files_of(&v)).filter(|f| !f.is_empty()) {
        *state.drag_files.borrow_mut() = files;
    }
    let files = state.drag_files.borrow().clone();
    let keyed = mode_for(target.current_event_state());
    let chip = chip_at(state, x, y);
    if !state.wheel.borrow().open {
        if let Some(mode) = chip {
            // Opened from a chip: the middle of the window, away from the chips.
            let mid = (state.overlay.width() as f64 / 2.0, state.overlay.height() as f64 / 2.0 - 30.0);
            open_wheel(state, mode, mid, files, false);
        } else if let Some(mode) = keyed {
            open_wheel(state, mode, (x, y), files, false);
        }
        return;
    }
    // Once open the wheel stays until the drop or the drag leaves; a key or chip only switches it.
    let mut wheel = state.wheel.borrow_mut();
    if let Some(mode) = keyed.or(chip).filter(|m| Some(*m) != wheel.mode) {
        wheel.set_mode(mode);
    }
    wheel.set_files(files);
    wheel.hover_at(x, y);
    drop(wheel);
    state.area.queue_draw();
}

fn end_drag(state: &App) {
    state.dragging.set(false);
    state.chips.set_visible(false);
    state.drag_files.borrow_mut().clear();
    close_wheel(state);
}

fn wire_drop(state: &Rc<App>) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    target.set_preload(true);

    let weak = Rc::downgrade(state);
    target.connect_enter(move |t, x, y| {
        if let Some(state) = weak.upgrade() {
            track_drag(&state, t, x, y);
        }
        gdk::DragAction::COPY
    });
    let weak = Rc::downgrade(state);
    target.connect_motion(move |t, x, y| {
        if let Some(state) = weak.upgrade() {
            track_drag(&state, t, x, y);
        }
        gdk::DragAction::COPY
    });
    let weak = Rc::downgrade(state);
    target.connect_leave(move |_| {
        if let Some(state) = weak.upgrade() {
            end_drag(&state);
        }
    });
    let weak = Rc::downgrade(state);
    target.connect_drop(move |_, value, x, y| {
        let Some(state) = weak.upgrade() else { return false };
        let files = files_of(value);
        let chosen = {
            let mut wheel = state.wheel.borrow_mut();
            wheel.open.then(|| {
                wheel.hover_at(x, y);
                wheel.chosen()
            })
        };
        end_drag(&state);
        match chosen {
            // Dropped on a slice.
            Some(Some(op)) => execute(&state, op, files),
            // Dropped on the hub or a greyed-out slice: cancel.
            Some(None) => {}
            // No wheel: keep the files for later.
            None => add_to_shelf(&state, files),
        }
        true
    });
    state.overlay.add_controller(target);
}

fn wire_keys(state: &Rc<App>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(state);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(state) = weak.upgrade() else { return glib::Propagation::Proceed };
        let (open, keyboard) = {
            let w = state.wheel.borrow();
            (w.open, w.keyboard)
        };
        // A modifier tapped while one of our own shelf rows is being dragged.
        if state.dragging.get()
            && let Some(mode) = config::get().mode_for_key(key)
        {
            if open {
                state.wheel.borrow_mut().set_mode(mode);
                state.area.queue_draw();
            } else {
                open_wheel(&state, mode, state.pointer.get(), state.drag_files.borrow().clone(), false);
            }
            return glib::Propagation::Stop;
        }
        if !open {
            if key == gdk::Key::Delete {
                remove_selected(&state);
                return glib::Propagation::Stop;
            }
            return glib::Propagation::Proceed;
        }
        if key == gdk::Key::Escape {
            close_wheel(&state);
            return glib::Propagation::Stop;
        }
        if !keyboard {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::Right | gdk::Key::Down => state.wheel.borrow_mut().step(true),
            gdk::Key::Left | gdk::Key::Up => state.wheel.borrow_mut().step(false),
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                let mut w = state.wheel.borrow_mut();
                let next = w.mode.unwrap_or(Mode::Convert).next();
                w.set_mode(next);
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space => {
                let (op, files) = {
                    let w = state.wheel.borrow();
                    (w.chosen(), w.files.clone())
                };
                close_wheel(&state);
                if let Some(op) = op {
                    execute(&state, op, files);
                }
                return glib::Propagation::Stop;
            }
            _ => return glib::Propagation::Stop,
        }
        state.area.queue_draw();
        glib::Propagation::Stop
    });
    state.window.add_controller(keys);
}

fn selected_files(state: &App) -> Vec<PathBuf> {
    let files = state.shelf_files.borrow();
    state.shelf.selected_rows().iter().filter_map(|r| files.get(r.index() as usize).cloned()).collect()
}

fn open_keyboard_wheel(state: &App) {
    let files = selected_files(state);
    if files.is_empty() {
        return;
    }
    let mid = (state.overlay.width() as f64 / 2.0, state.overlay.height() as f64 / 2.0);
    open_wheel(state, Mode::Convert, mid, files, true);
}

fn remove_selected(state: &App) {
    let mut files = state.shelf_files.borrow_mut();
    for row in state.shelf.selected_rows().iter().rev() {
        files.remove(row.index() as usize);
        state.shelf.remove(row);
    }
    if files.is_empty() {
        state.stack.set_visible_child_name("empty");
    }
}

fn add_to_shelf(state: &App, files: Vec<PathBuf>) {
    for file in files {
        if state.shelf_files.borrow().contains(&file) {
            continue;
        }
        state.shelf.append(&shelf_row(&file));
        state.shelf_files.borrow_mut().push(file);
    }
    if !state.shelf_files.borrow().is_empty() {
        state.stack.set_visible_child_name("shelf");
    }
}

fn shelf_row(file: &Path) -> gtk::ListBoxRow {
    let icon = gio::File::for_path(file)
        .query_info("standard::icon", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        .ok()
        .and_then(|i| i.icon())
        .map(|i| gtk::Image::from_gicon(&i))
        .unwrap_or_else(|| gtk::Image::from_icon_name("text-x-generic"));
    icon.set_pixel_size(32);

    let name = gtk::Label::new(Some(&file.file_name().unwrap_or_default().to_string_lossy()));
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let folder = gtk::Label::new(Some(&file.parent().map(|p| p.display().to_string()).unwrap_or_default()));
    folder.set_xalign(0.0);
    folder.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    folder.add_css_class("dim");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.append(&name);
    text.append(&folder);

    let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    line.set_margin_top(6);
    line.set_margin_bottom(6);
    line.set_margin_start(12);
    line.set_margin_end(12);
    line.append(&icon);
    line.append(&text);
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&line));

    // Shelf rows drag out to file managers, or back onto the window to open a wheel.
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::COPY);
    let path = file.to_path_buf();
    source.connect_prepare(move |source, _, _| {
        let row = source.widget()?.downcast::<gtk::ListBoxRow>().ok()?;
        let state = state()?;
        // Dragging a selected row takes the whole selection along.
        let files = if row.is_selected() { selected_files(&state) } else { vec![path.clone()] };
        let gfiles: Vec<gio::File> = files.iter().map(gio::File::for_path).collect();
        Some(gdk::ContentProvider::for_value(&gdk::FileList::from_array(&gfiles).to_value()))
    });
    row.add_controller(source);
    row
}

/// Runs `op`. Conversions and tools get a line in the activity list and run off the UI thread.
fn execute(state: &Rc<App>, op: Op, files: Vec<PathBuf>) {
    if files.is_empty() {
        return;
    }
    if let Op::Action(action) = op {
        let status = state.status.clone();
        actions::perform(action, files, &state.window, move |msg| status.set_text(&msg));
        return;
    }
    // Tools like Trim and Crop ask for their settings first.
    if let Op::Tool(tool) = op
        && tool.needs_params()
    {
        let weak = Rc::downgrade(state);
        params_ui::ask(state.window.upcast_ref(), tool, files.clone(), move |params| {
            if let Some(state) = weak.upgrade() {
                start_job(&state, op, files, Some(params));
            }
        });
        return;
    }
    start_job(state, op, files, None);
}

/// Adds a line to the activity list and runs `op` off the UI thread.
fn start_job(state: &Rc<App>, op: Op, files: Vec<PathBuf>, params: Option<params::Params>) {
    let what = if files.len() == 1 {
        files[0].file_name().unwrap_or_default().to_string_lossy().into_owned()
    } else {
        format!("{} files", files.len())
    };
    let title = gtk::Label::new(Some(&format!("{what} → {}", op.label().trim_end_matches('…'))));
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let spinner = gtk::Spinner::new();
    spinner.start();
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_margin_top(4);
    line.set_margin_bottom(4);
    line.set_margin_start(12);
    line.set_margin_end(12);
    line.append(&title);
    line.append(&spinner);
    state.jobs.prepend(&line);

    let state = state.clone();
    let dest = config::get().output_dir;
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || backend::run(op, &files, params.as_ref(), dest.as_deref())).await;
        line.remove(&spinner);
        match result {
            Ok(Ok(outputs)) => {
                let show = gtk::Button::with_label("Show");
                show.add_css_class("flat");
                let shown = outputs.clone();
                let weak = Rc::downgrade(&state);
                show.connect_clicked(move |_| {
                    let Some(state) = weak.upgrade() else { return };
                    let status = state.status.clone();
                    actions::perform(catalog::Action::ShowInFiles, shown.clone(), &state.window, move |m| {
                        status.set_text(&m)
                    });
                });
                line.append(&show);
                if config::get().add_results_to_shelf {
                    add_to_shelf(&state, outputs);
                }
            }
            Ok(Err(e)) => {
                let err = gtk::Label::new(Some("Failed"));
                err.add_css_class("error");
                err.set_tooltip_text(Some(&format!("{e:#}")));
                line.append(&err);
                state.status.set_text(&format!("{e:#}"));
            }
            Err(_) => state.status.set_text("The converter crashed"),
        }
    });
}
