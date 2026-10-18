//! The rows of the activity list: a running job's progress bar and cancel button, and what
//! the row turns into when the job ends. Also keeps the window from closing on running jobs.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::backend;
use crate::catalog::Op;
use crate::params::Params;
use crate::progress::{self, Cancel, Progress};

/// How often a running row is redrawn; progress arriving faster is coalesced.
const TICK: Duration = Duration::from_millis(100);

thread_local! {
    /// Cancel tokens of the jobs still running, by job number.
    static RUNNING: RefCell<Vec<(u64, Cancel)>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };
    /// Set once the user chose to cancel everything and quit: the window closes when the last
    /// job has cleaned up after itself.
    static QUITTING: RefCell<Option<gtk::Window>> = const { RefCell::new(None) };
}

/// How many jobs are running.
pub fn running() -> usize {
    RUNNING.with(|r| r.borrow().len())
}

/// Cancels every running job.
pub fn cancel_all() {
    RUNNING.with(|r| r.borrow().iter().for_each(|(_, c)| c.cancel()));
}

/// What a job ended with: its outputs, or why there are none.
pub type Outcome = anyhow::Result<Vec<PathBuf>>;

/// What happens when a job ends, besides its row changing.
pub struct Finish {
    /// Called once with the result.
    pub done: Box<dyn FnOnce(&Outcome)>,
    /// Called when the finished row's Show button is pressed.
    pub show: Rc<dyn Fn(Vec<PathBuf>)>,
}

/// Adds a row titled `title` to the top of `list` and runs `op` off the UI thread, with a
/// progress bar and a cancel button until it ends.
pub fn start(
    list: &gtk::ListBox,
    title: &str,
    op: Op,
    files: Vec<PathBuf>,
    params: Option<Params>,
    dest: Option<PathBuf>,
    finish: Finish,
) {
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let count = gtk::Label::new(None);
    count.add_css_class("dim");
    count.add_css_class("numeric");
    let stop = gtk::Button::from_icon_name("window-close-symbolic");
    stop.add_css_class("flat");
    stop.set_tooltip_text(Some("Cancel"));
    stop.update_property(&[gtk::accessible::Property::Label("Cancel conversion")]);
    let bar = gtk::ProgressBar::new();
    bar.set_pulse_step(0.08);
    bar.update_property(&[gtk::accessible::Property::Label(title)]);

    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.append(&label);
    line.append(&count);
    line.append(&stop);
    let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    row.set_margin_top(4);
    row.set_margin_bottom(4);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.append(&line);
    row.append(&bar);
    list.prepend(&row);

    let cancel = Cancel::new();
    let id = NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    RUNNING.with(|r| r.borrow_mut().push((id, cancel.clone())));

    let c = cancel.clone();
    let stopping = count.clone();
    stop.connect_clicked(move |b| {
        c.cancel();
        b.set_sensitive(false);
        stopping.set_text("Cancelling…");
    });

    // The worker only stores the latest progress; the tick below draws it at most 10 times a second.
    let latest: Arc<Mutex<Option<Progress>>> = Arc::default();
    let ended = Rc::new(Cell::new(false));
    {
        let (latest, ended, bar, count, cancel) =
            (latest.clone(), ended.clone(), bar.clone(), count.clone(), cancel.clone());
        glib::timeout_add_local(TICK, move || {
            if ended.get() {
                return glib::ControlFlow::Break;
            }
            if cancel.is_cancelled() {
                return glib::ControlFlow::Continue;
            }
            let now = *latest.lock().unwrap_or_else(|e| e.into_inner());
            match now.and_then(|p| p.overall()) {
                Some(f) => bar.set_fraction(f),
                None => bar.pulse(),
            }
            if let Some(p) = now {
                count.set_text(&p.label());
            }
            glib::ControlFlow::Continue
        });
    }

    let window = list.root().and_downcast::<gtk::Window>();
    glib::spawn_future_local(async move {
        let worker = cancel.clone();
        let report = latest.clone();
        let result = gio::spawn_blocking(move || {
            let report = move |p: Progress| *report.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
            backend::run_with(op, &files, params.as_ref(), dest.as_deref(), &worker, &report)
        })
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("The converter crashed")));
        ended.set(true);
        RUNNING.with(|r| r.borrow_mut().retain(|(i, _)| *i != id));
        row.remove(&bar);
        line.remove(&stop);
        count.set_text("");
        match &result {
            Ok(outputs) => {
                count.set_visible(false);
                let show = gtk::Button::with_label("Show");
                show.add_css_class("flat");
                let (outputs, on_show) = (outputs.clone(), finish.show.clone());
                show.connect_clicked(move |_| on_show(outputs.clone()));
                line.append(&show);
            }
            Err(e) if progress::is_cancelled(e) => count.set_text("Cancelled"),
            Err(e) => {
                count.set_text("Failed");
                count.remove_css_class("dim");
                count.add_css_class("error");
                count.set_tooltip_text(Some(&format!("{e:#}")));
            }
        }
        if let Some(window) = &window {
            notify(window, &label.text(), &result);
        }
        (finish.done)(&result);
        if running() == 0
            && let Some(window) = QUITTING.with(|q| q.borrow_mut().take())
        {
            window.close();
        }
    });
}

/// Tells the desktop a job ended, when the window is not in front to show it.
fn notify(window: &gtk::Window, title: &str, result: &Outcome) {
    if window.is_active() {
        return;
    }
    let (Some(app), heading) = (
        window.application(),
        match result {
            Ok(_) => "Conversion finished",
            Err(e) if progress::is_cancelled(e) => return,
            Err(_) => "Conversion failed",
        },
    ) else {
        return;
    };
    let note = gio::Notification::new(heading);
    note.set_body(Some(title));
    app.send_notification(None, &note);
}

/// Asks before closing `window` while jobs run: cancel them all and quit once they have cleaned
/// up, or keep the window open.
pub fn guard_close(window: &impl IsA<gtk::Window>) {
    window.connect_close_request(|window| {
        if running() == 0 {
            return glib::Propagation::Proceed;
        }
        let n = running();
        let dialog = gtk::AlertDialog::builder()
            .message("Cancel running conversions?")
            .detail(if n == 1 {
                "A conversion is still running. Its unfinished file will be removed.".to_string()
            } else {
                format!("{n} conversions are still running. Their unfinished files will be removed.")
            })
            .buttons(["Keep Running", "Cancel and Quit"])
            .cancel_button(0)
            .default_button(0)
            .modal(true)
            .build();
        let win = window.clone().upcast::<gtk::Window>();
        dialog.choose(Some(window), gio::Cancellable::NONE, move |choice| {
            if choice == Ok(1) {
                QUITTING.with(|q| *q.borrow_mut() = Some(win.clone()));
                cancel_all();
                if running() == 0 {
                    win.close();
                }
            }
        });
        glib::Propagation::Stop
    });
}
