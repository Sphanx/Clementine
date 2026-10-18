//! File actions: the Alt wheel. They never convert, so they also take folders and mixed selections.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::catalog::Action;

/// Runs `action` and reports one line of outcome through `report`.
pub fn perform(
    action: Action,
    files: Vec<PathBuf>,
    window: &gtk::ApplicationWindow,
    report: impl Fn(String) + 'static,
) {
    if files.is_empty() {
        return;
    }
    match action {
        Action::CopyPath => {
            let text: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
            window.clipboard().set_text(&text.join("\n"));
            report(format!("Copied {} path{}", files.len(), plural(files.len())));
        }
        Action::CopyFiles => {
            window.clipboard().set_content(Some(&file_clipboard(&files))).ok();
            report(format!("Copied {} item{} — paste them in your file manager", files.len(), plural(files.len())));
        }
        Action::CopyTo | Action::MoveTo => {
            let moving = action == Action::MoveTo;
            let dialog =
                gtk::FileDialog::builder().title(if moving { "Move to" } else { "Copy to" }).modal(true).build();
            dialog.select_folder(Some(window), gio::Cancellable::NONE, move |result| {
                let Some(dest) = result.ok().and_then(|f| f.path()) else { return };
                glib::spawn_future_local(async move {
                    let n = files.len();
                    let done = gio::spawn_blocking(move || transfer(&files, &dest, moving)).await;
                    report(match done {
                        Ok(Ok(())) => format!("{} {n} item{}", if moving { "Moved" } else { "Copied" }, plural(n)),
                        Ok(Err(e)) => format!("Failed: {e}"),
                        Err(_) => "Failed: the copy crashed".into(),
                    });
                });
            });
        }
        Action::ShowInFiles => {
            if !show_items(&files) {
                let dir = folder_of(&files[0]);
                let _ =
                    gio::AppInfo::launch_default_for_uri(&gio::File::for_path(&dir).uri(), gio::AppLaunchContext::NONE);
            }
        }
        Action::Terminal => {
            let dir = folder_of(&files[0]);
            report(match open_terminal(&dir) {
                Some(t) => format!("Opened {t} in {}", dir.display()),
                None => "No terminal found — set $TERMINAL".into(),
            });
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn folder_of(path: &Path) -> PathBuf {
    if path.is_dir() { path.into() } else { path.parent().map(Path::to_path_buf).unwrap_or_else(|| "/".into()) }
}

/// Offers the files the way GTK apps, GNOME Files and KDE's Dolphin each expect to paste them.
fn file_clipboard(files: &[PathBuf]) -> gdk::ContentProvider {
    let gfiles: Vec<gio::File> = files.iter().map(gio::File::for_path).collect();
    let uris: Vec<String> = gfiles.iter().map(|f| f.uri().to_string()).collect();
    let gnome = format!("copy\n{}", uris.join("\n"));
    let uri_list = format!("{}\r\n", uris.join("\r\n"));
    gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_value(&gdk::FileList::from_array(&gfiles).to_value()),
        gdk::ContentProvider::for_bytes("x-special/gnome-copied-files", &glib::Bytes::from_owned(gnome.into_bytes())),
        gdk::ContentProvider::for_bytes("text/uri-list", &glib::Bytes::from_owned(uri_list.into_bytes())),
    ])
}

/// Asks the file manager to open the folder with these items selected (freedesktop `FileManager1`).
fn show_items(files: &[PathBuf]) -> bool {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return false };
    let uris: Vec<String> = files.iter().map(|f| gio::File::for_path(f).uri().to_string()).collect();
    bus.call_sync(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
        "ShowItems",
        Some(&(uris, "").to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        2000,
        gio::Cancellable::NONE,
    )
    .is_ok()
}

const TERMINALS: &[&str] = &[
    "x-terminal-emulator",
    "gnome-terminal",
    "kgx",
    "ptyxis",
    "konsole",
    "xfce4-terminal",
    "alacritty",
    "kitty",
    "wezterm",
    "foot",
    "xterm",
];

/// Starts a terminal in `dir`; every terminal above honours the working directory it is started in.
fn open_terminal(dir: &Path) -> Option<String> {
    let preferred = std::env::var("TERMINAL").ok().filter(|t| !t.is_empty());
    preferred.iter().map(String::as_str).chain(TERMINALS.iter().copied()).find_map(|t| {
        Command::new(t)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
            .map(|_| t.to_string())
    })
}

/// Copies or moves every item into `dest`, never overwriting: a clash gets a `name (2)` name.
pub fn transfer(files: &[PathBuf], dest: &Path, moving: bool) -> std::io::Result<()> {
    for f in files {
        let Some(name) = f.file_name() else { continue };
        if std::fs::symlink_metadata(f)?.is_dir() && dest.canonicalize()?.starts_with(f.canonicalize()?) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("cannot put {} inside itself", f.display()),
            ));
        }
        let mut to = dest.join(name);
        let (stem, ext) = split_name(&name.to_string_lossy());
        let mut n = 2;
        // `symlink_metadata`: a dangling link counts as taken, so nothing is written through it.
        while std::fs::symlink_metadata(&to).is_ok() {
            to = dest.join(format!("{stem} ({n}){ext}"));
            n += 1;
        }
        if moving && std::fs::rename(f, &to).is_ok() {
            continue;
        }
        copy_recursive(f, &to)?;
        if moving {
            if std::fs::symlink_metadata(f)?.is_dir() { std::fs::remove_dir_all(f)? } else { std::fs::remove_file(f)? }
        }
    }
    Ok(())
}

fn split_name(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

/// Copies a file or folder; links are copied as links, never followed (a link to `.` would loop,
/// and a link out of a downloaded folder would copy whatever it names).
fn copy_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    let kind = std::fs::symlink_metadata(from)?.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else if kind.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        // `create_new`: never write through something that appeared at `to` meanwhile.
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(to)?;
        std::io::copy(&mut std::fs::File::open(from)?, &mut out)?;
        out.set_permissions(std::fs::metadata(from)?.permissions())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_never_overwrites() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let a = src.path().join("a.txt");
        std::fs::write(&a, "new").unwrap();
        std::fs::write(dst.path().join("a.txt"), "old").unwrap();
        transfer(std::slice::from_ref(&a), dst.path(), true).unwrap();
        assert_eq!(std::fs::read_to_string(dst.path().join("a.txt")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(dst.path().join("a (2).txt")).unwrap(), "new");
        assert!(!a.exists());
    }

    #[test]
    fn links_are_copied_as_links() {
        let root = tempfile::tempdir().unwrap();
        let secret = root.path().join("secret");
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(secret.join("key"), "SENTINEL").unwrap();
        let dir = root.path().join("download");
        std::fs::create_dir(&dir).unwrap();
        std::os::unix::fs::symlink(&secret, dir.join("link")).unwrap();
        std::os::unix::fs::symlink(".", dir.join("loop")).unwrap();
        let dst = root.path().join("dst");
        std::fs::create_dir(&dst).unwrap();
        transfer(std::slice::from_ref(&dir), &dst, false).unwrap();
        for name in ["link", "loop"] {
            let copied = dst.join("download").join(name);
            assert!(std::fs::symlink_metadata(&copied).unwrap().file_type().is_symlink(), "{name} was followed");
        }
    }

    #[test]
    fn a_dangling_link_at_the_destination_is_not_written_through() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.txt");
        std::fs::write(&a, "new").unwrap();
        let dst = root.path().join("dst");
        std::fs::create_dir(&dst).unwrap();
        let elsewhere = root.path().join("elsewhere.txt");
        std::os::unix::fs::symlink(&elsewhere, dst.join("a.txt")).unwrap();
        transfer(std::slice::from_ref(&a), &dst, false).unwrap();
        assert!(!elsewhere.exists(), "the copy followed a link at the destination");
        assert_eq!(std::fs::read_to_string(dst.join("a (2).txt")).unwrap(), "new");
    }

    #[test]
    fn a_folder_is_never_moved_into_itself() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("photos");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("x"), "x").unwrap();
        for moving in [true, false] {
            assert!(transfer(std::slice::from_ref(&dir), &dir.join("inner"), moving).is_err());
            assert!(dir.join("x").is_file() && !dir.join("inner/photos").exists());
        }
    }

    #[test]
    fn folders_copy_whole() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let dir = src.path().join("photos");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("inner/x"), "x").unwrap();
        transfer(std::slice::from_ref(&dir), dst.path(), false).unwrap();
        assert!(dst.path().join("photos/inner/x").is_file());
        assert!(dir.exists());
    }
}
