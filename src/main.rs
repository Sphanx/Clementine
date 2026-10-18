//! Susen: drag a file in, tap a modifier, drop it on a format.
//!
//! Shift opens the conversion wheel, Ctrl the tools wheel, Alt the actions wheel.
//! Wayland never tells a window about keys pressed while a drag from another app
//! hovers it, so the wheels can also be opened by dragging over the three chips
//! at the bottom of the window. Without a wheel, a drop just puts the files on
//! the shelf, where Enter opens the wheel from the keyboard.

mod actions;
mod backend;
mod catalog;
mod cli;
mod config;
mod jobs_ui;
mod messages;
mod params;
mod params_ui;
mod progress;
mod settings_ui;
mod wheel;

#[cfg(test)]
mod qa_tests;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use catalog::{Mode, Op};
use wheel::Wheel;

const APP_ID: &str = "io.github.sphanx.Susen";

const CSS: &str = r#"
.drop-hint { font-size: 1.4em; font-weight: bold; opacity: 0.8; }
.chip { padding: 6px 14px; border-radius: 999px; background: alpha(currentColor, 0.08); font-weight: bold; }
.chip.hot { background: #6b5b95; color: white; }
.dim { opacity: 0.6; }
.section { font-weight: bold; opacity: 0.7; margin: 8px 12px 2px 12px; }
.error { color: @error_color; }
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
    // `--convert`, `--tool`, `--list` and `--wheel` are handled before GTK sees the command line.
    let wheel_files = match cli::main_hook() {
        cli::Next::Exit(code) => return glib::ExitCode::from(code),
        cli::Next::Gui => None,
        cli::Next::Wheel(files) => Some(files),
    };
    let app = gtk::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    app.connect_activate(|app| window(app).present());
    // `susen file…` and "Open With" both land here; `--wheel` sends the hint "wheel".
    app.connect_open(|app, files, hint| {
        window(app).present();
        if let Some(state) = state() {
            let files: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
            add_to_shelf(&state, files.clone());
            if hint == cli::WHEEL_HINT {
                open_wheel_when_ready(&state, files);
            }
        }
    });
    if let Some(files) = wheel_files {
        // Open through the running instance when there is one, so the wheel appears there.
        if app.register(gio::Cancellable::NONE).is_err() {
            return glib::ExitCode::FAILURE;
        }
        let gfiles: Vec<gio::File> = files.iter().map(gio::File::for_path).collect();
        app.open(&gfiles, cli::WHEEL_HINT);
        if app.is_remote() {
            return glib::ExitCode::SUCCESS;
        }
        let argv0: Vec<String> = std::env::args().take(1).collect();
        return app.run_with_args(&argv0);
    }
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
        .title("Susen")
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

    // The wheel is painted over everything and never takes events itself; the overlay's
    // gestures do. It takes keyboard focus while open so screen readers follow it (see wheel::sync).
    let area = gtk::DrawingArea::builder()
        .accessible_role(gtk::AccessibleRole::Menu)
        .can_target(false)
        .focusable(true)
        .visible(false)
        .build();

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
    jobs_ui::guard_close(&window);

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
        wheel::draw(area, cr, &state.wheel.borrow());
    });

    refresh_hints(&state);
    wire_drop(&state);
    wire_keys(&state);
    wire_pointer(&state);

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
    let geo = wheel::geometry_for(&state.area);
    let center = wheel::clamp_center(at, geo, state.overlay.width() as f64, state.overlay.height() as f64);
    {
        let mut w = state.wheel.borrow_mut();
        w.geo = geo;
        w.show(mode, center, files, keyboard);
        if keyboard && w.return_focus.is_none() {
            w.return_focus = gtk::prelude::GtkWindowExt::focus(&state.window);
        }
    }
    redraw(state);
    if keyboard {
        state.area.grab_focus();
    }
}

fn close_wheel(state: &App) {
    let back = state.wheel.borrow_mut().return_focus.take();
    state.wheel.borrow_mut().hide();
    redraw(state);
    // Back to the shelf row the wheel was opened from, with its focus ring showing.
    if let Some(widget) = back {
        widget.grab_focus();
        state.window.set_focus_visible(true);
    }
}

/// Repaints the wheel and tells assistive technology what changed.
fn redraw(state: &App) {
    wheel::sync(&state.area, &mut state.wheel.borrow_mut());
}

/// Every action a drop may arrive with. We only ever read the files, but on X11 the drag source
/// turns the held modifier into the action and offers only that one: Shift = move, Ctrl = copy,
/// Shift+Ctrl = link. Accepting copy alone made the window refuse any drag with Shift held.
const DROP_ACTIONS: gdk::DragAction = gdk::DragAction::COPY.union(gdk::DragAction::MOVE).union(gdk::DragAction::LINK);

/// The action to answer a drag with: one the source offers, copy when it can.
fn drop_action(offered: gdk::DragAction) -> gdk::DragAction {
    [gdk::DragAction::COPY, gdk::DragAction::MOVE, gdk::DragAction::LINK]
        .into_iter()
        .find(|a| offered.contains(*a))
        .unwrap_or(gdk::DragAction::COPY)
}

fn debug_dnd() -> bool {
    std::env::var_os("SUSEN_DEBUG_DND").is_some()
}

/// The modifier keys held right now, as far as this window can know.
///
/// Drag events carry no modifier state (`current_event_state()` is empty on X11), so the
/// keyboard device is asked as well. X11 reports its state for the whole display, so it is right
/// even while another app's drag holds the keyboard; Wayland only tells the focused window, so
/// there it is trusted only while this window is active.
fn held_modifiers(state: &App, target: &gtk::DropTarget) -> gdk::ModifierType {
    let mut mods = target.current_event_state();
    if let Some(drop) = target.current_drop() {
        mods |= drop.device().modifier_state();
    }
    let display = WidgetExt::display(&state.window);
    if display.type_().name() == "GdkX11Display" || state.window.is_active() {
        mods |= display
            .default_seat()
            .and_then(|s| s.keyboard())
            .map_or(gdk::ModifierType::empty(), |k| k.modifier_state());
    }
    mods
}

/// A wheel's modifier went down mid-drag: open that wheel at the pointer, or switch to it.
/// It stays open after the key is released, until the drop or the drag leaves.
fn modifier_during_drag(state: &App, mode: Mode) {
    if debug_dnd() {
        eprintln!("dnd: modifier {mode:?}");
    }
    if !state.wheel.borrow().open {
        open_wheel(state, mode, state.pointer.get(), state.drag_files.borrow().clone(), false);
        return;
    }
    if state.wheel.borrow().mode != Some(mode) {
        state.wheel.borrow_mut().set_mode(mode);
        redraw(state);
    }
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
    let mods = held_modifiers(state, target);
    let keyed = mode_for(mods);
    if debug_dnd() {
        let offered = target.current_drop().map(|d| d.actions());
        eprintln!("dnd: motion offered={offered:?} mods={mods:?} wheel={:?}", state.wheel.borrow().mode);
    }
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
    redraw(state);
}

fn end_drag(state: &App) {
    state.dragging.set(false);
    state.chips.set_visible(false);
    state.drag_files.borrow_mut().clear();
    close_wheel(state);
}

fn wire_drop(state: &Rc<App>) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), DROP_ACTIONS);
    target.set_preload(true);

    let weak = Rc::downgrade(state);
    target.connect_enter(move |t, x, y| {
        if let Some(state) = weak.upgrade() {
            track_drag(&state, t, x, y);
        }
        drop_action(t.current_drop().map_or(gdk::DragAction::COPY, |d| d.actions()))
    });
    let weak = Rc::downgrade(state);
    target.connect_motion(move |t, x, y| {
        if let Some(state) = weak.upgrade() {
            track_drag(&state, t, x, y);
        }
        drop_action(t.current_drop().map_or(gdk::DragAction::COPY, |d| d.actions()))
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
        if debug_dnd() {
            eprintln!("dnd: drop chosen={chosen:?}");
        }
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

    // A tap with the pointer still: no drag event follows, and during a drag (ours or, on X11,
    // another app's) the drag holds the keyboard grab, so no key event reaches the window either.
    // The keyboard device still announces its new modifier state.
    let keyboard = WidgetExt::display(&state.window).default_seat().and_then(|s| s.keyboard());
    if let Some(keyboard) = keyboard {
        let weak = Rc::downgrade(state);
        keyboard.connect_modifier_state_notify(move |kb| {
            let Some(state) = weak.upgrade() else { return };
            if state.dragging.get()
                && let Some(mode) = mode_for(kb.modifier_state())
            {
                modifier_during_drag(&state, mode);
            }
        });
    }
}

fn wire_keys(state: &Rc<App>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(state);
    keys.connect_key_pressed(move |_, key, _, mods| {
        let Some(state) = weak.upgrade() else { return glib::Propagation::Proceed };
        let (open, keyboard) = {
            let w = state.wheel.borrow();
            (w.open, w.keyboard)
        };
        // A modifier tapped while one of our own shelf rows is being dragged.
        if state.dragging.get()
            && let Some(mode) = config::get().mode_for_key(key)
        {
            modifier_during_drag(&state, mode);
            return glib::Propagation::Stop;
        }
        if !open {
            if key == gdk::Key::Delete {
                remove_selected(&state);
                return glib::Propagation::Stop;
            }
            // The context-menu key and Shift+F10 open the wheel, as they would a context menu.
            let menu_key =
                key == gdk::Key::Menu || (key == gdk::Key::F10 && mods.contains(gdk::ModifierType::SHIFT_MASK));
            if menu_key && !selected_files(&state).is_empty() {
                open_keyboard_wheel(&state);
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
            gdk::Key::Home | gdk::Key::KP_Home => state.wheel.borrow_mut().select_edge(false),
            gdk::Key::End | gdk::Key::KP_End => state.wheel.borrow_mut().select_edge(true),
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space => {
                run_chosen(&state);
                return glib::Propagation::Stop;
            }
            // 1–9 run slice N; a greyed-out one is only selected, so its reason is read out.
            other => {
                let Some(i) = other.to_unicode().and_then(wheel::digit_slice) else { return glib::Propagation::Stop };
                let runs = {
                    let mut w = state.wheel.borrow_mut();
                    w.select(i) && w.chosen().is_some()
                };
                if runs {
                    run_chosen(&state);
                    return glib::Propagation::Stop;
                }
            }
        }
        redraw(&state);
        glib::Propagation::Stop
    });
    state.window.add_controller(keys);
}

/// Closes the wheel and runs its selected slice, if that can run.
fn run_chosen(state: &Rc<App>) {
    let (op, files) = {
        let w = state.wheel.borrow();
        (w.chosen(), w.files.clone())
    };
    close_wheel(state);
    if let Some(op) = op {
        execute(state, op, files);
    }
}

/// Pointer users without modifier keys: right-click (or long-press on a touch screen) a shelf
/// row to open the wheel there, then click a slice to run it, or anywhere else to cancel.
fn wire_pointer(state: &Rc<App>) {
    let secondary = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
    let weak = Rc::downgrade(state);
    secondary.connect_pressed(move |g, _, x, y| {
        if let Some(state) = weak.upgrade()
            && open_from_shelf(&state, x, y)
        {
            g.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    state.shelf.add_controller(secondary);

    let long = gtk::GestureLongPress::new();
    long.set_touch_only(true);
    let weak = Rc::downgrade(state);
    long.connect_pressed(move |g, x, y| {
        if let Some(state) = weak.upgrade()
            && open_from_shelf(&state, x, y)
        {
            g.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    state.shelf.add_controller(long);

    // While a wheel opened this way is up, it gets every click before the widgets below it.
    let click = gtk::GestureClick::builder().button(0).propagation_phase(gtk::PropagationPhase::Capture).build();
    let weak = Rc::downgrade(state);
    click.connect_pressed(move |g, _, x, y| {
        let Some(state) = weak.upgrade() else { return };
        if !state.wheel.borrow().open || state.dragging.get() {
            return;
        }
        g.set_state(gtk::EventSequenceState::Claimed);
        let (hit, runs) = {
            let mut w = state.wheel.borrow_mut();
            let hit = wheel::slice_at(w.slices.len(), w.geo, w.center, (x, y));
            if let Some(i) = hit {
                w.select(i);
            }
            (hit, w.chosen().is_some())
        };
        match (hit, runs) {
            (Some(_), true) => run_chosen(&state),
            // A greyed-out slice: stay open and say why.
            (Some(_), false) => redraw(&state),
            (None, _) => close_wheel(&state),
        }
    });
    state.overlay.add_controller(click);

    let motion = gtk::EventControllerMotion::new();
    let weak = Rc::downgrade(state);
    motion.connect_motion(move |_, x, y| {
        let Some(state) = weak.upgrade() else { return };
        if state.dragging.get() {
            return;
        }
        let changed = {
            let mut w = state.wheel.borrow_mut();
            let hit = wheel::slice_at(w.slices.len(), w.geo, w.center, (x, y));
            let changed = w.open && w.keyboard && hit.is_some() && hit != w.hovered;
            if changed {
                w.hovered = hit;
            }
            changed
        };
        if changed {
            redraw(&state);
        }
    });
    state.overlay.add_controller(motion);
}

/// Opens the Convert wheel at a pointer position on the shelf, for the row under it.
fn open_from_shelf(state: &App, x: f64, y: f64) -> bool {
    let Some(row) = state.shelf.row_at_y(y as i32) else { return false };
    if !row.is_selected() {
        state.shelf.unselect_all();
        state.shelf.select_row(Some(&row));
    }
    let files = selected_files(state);
    if files.is_empty() {
        return false;
    }
    row.grab_focus();
    let at = state
        .shelf
        .compute_point(&state.overlay, &gtk::graphene::Point::new(x as f32, y as f32))
        .map_or((x, y), |p| (p.x() as f64, p.y() as f64));
    open_wheel(state, Mode::Convert, at, files, true);
    true
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

/// Opens the keyboard Convert wheel on `files` once the window has a size to centre it in.
fn open_wheel_when_ready(state: &Rc<App>, files: Vec<PathBuf>) {
    if files.is_empty() {
        return;
    }
    let weak = Rc::downgrade(state);
    glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
        let Some(state) = weak.upgrade() else { return glib::ControlFlow::Break };
        if state.overlay.width() == 0 {
            return glib::ControlFlow::Continue;
        }
        let shelf = state.shelf_files.borrow().clone();
        state.shelf.unselect_all();
        for (i, f) in shelf.iter().enumerate() {
            if files.contains(f) {
                state.shelf.select_row(state.shelf.row_at_index(i as i32).as_ref());
            }
        }
        state.window.present();
        let mid = (state.overlay.width() as f64 / 2.0, state.overlay.height() as f64 / 2.0);
        open_wheel(&state, Mode::Convert, mid, files.clone(), true);
        glib::ControlFlow::Break
    });
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
    row.update_property(&[gtk::accessible::Property::Label(&file.file_name().unwrap_or_default().to_string_lossy())]);

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

/// Adds a line to the activity list and runs `op` off the UI thread (see [`jobs_ui`]).
fn start_job(state: &Rc<App>, op: Op, files: Vec<PathBuf>, params: Option<params::Params>) {
    let what = if files.len() == 1 {
        files[0].file_name().unwrap_or_default().to_string_lossy().into_owned()
    } else {
        format!("{} files", files.len())
    };
    let title = format!("{what} → {}", op.label().trim_end_matches('…'));
    let weak = Rc::downgrade(state);
    let done = move |result: &jobs_ui::Outcome| {
        let Some(state) = weak.upgrade() else { return };
        match result {
            Ok(outputs) if config::get().add_results_to_shelf => add_to_shelf(&state, outputs.clone()),
            Ok(_) => {}
            Err(e) if progress::is_cancelled(e) => state.status.set_text("Cancelled"),
            Err(e) => state.status.set_text(&messages::headline(&format!("{e:#}"))),
        }
    };
    let weak = Rc::downgrade(state);
    let show = move |outputs: Vec<PathBuf>| {
        let Some(state) = weak.upgrade() else { return };
        let status = state.status.clone();
        actions::perform(catalog::Action::ShowInFiles, outputs, &state.window, move |m| status.set_text(&m));
    };
    let finish = jobs_ui::Finish { done: Box::new(done), show: Rc::new(show) };
    jobs_ui::start(&state.jobs, &title, op, files, params, config::get().output_dir, finish);
}
