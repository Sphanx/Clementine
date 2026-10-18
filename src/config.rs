//! User settings, kept as JSON in `$XDG_CONFIG_HOME/susen/config.json`.
//!
//! A missing, unreadable or corrupt file gives the defaults, and so does any key missing from it:
//! settings must never stop the app from starting. The live copy is behind [`get`] / [`update`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gtk::gdk;
use serde::{Deserialize, Serialize};

use crate::catalog::Mode;

/// A modifier key that can open a wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Modifier {
    Shift,
    Ctrl,
    Alt,
    Super,
}

impl Modifier {
    pub const ALL: [Modifier; 4] = [Modifier::Shift, Modifier::Ctrl, Modifier::Alt, Modifier::Super];

    pub fn label(self) -> &'static str {
        match self {
            Modifier::Shift => "Shift",
            Modifier::Ctrl => "Ctrl",
            Modifier::Alt => "Alt",
            Modifier::Super => "Super",
        }
    }

    fn mask(self) -> gdk::ModifierType {
        match self {
            Modifier::Shift => gdk::ModifierType::SHIFT_MASK,
            Modifier::Ctrl => gdk::ModifierType::CONTROL_MASK,
            Modifier::Alt => gdk::ModifierType::ALT_MASK,
            Modifier::Super => gdk::ModifierType::SUPER_MASK,
        }
    }

    fn matches_key(self, key: gdk::Key) -> bool {
        match self {
            Modifier::Shift => matches!(key, gdk::Key::Shift_L | gdk::Key::Shift_R),
            Modifier::Ctrl => matches!(key, gdk::Key::Control_L | gdk::Key::Control_R),
            Modifier::Alt => matches!(key, gdk::Key::Alt_L | gdk::Key::Alt_R),
            Modifier::Super => matches!(key, gdk::Key::Super_L | gdk::Key::Super_R),
        }
    }
}

/// Every setting. Unknown keys are ignored and missing ones take their default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The modifier that opens each wheel. Three different keys; see [`Config::assign`].
    pub convert_key: Modifier,
    pub tools_key: Modifier,
    pub actions_key: Modifier,
    /// Where results go; `None` puts them next to the original.
    pub output_dir: Option<PathBuf>,
    /// Whether finished results are added to the shelf.
    pub add_results_to_shelf: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            convert_key: Modifier::Shift,
            tools_key: Modifier::Ctrl,
            actions_key: Modifier::Alt,
            output_dir: None,
            add_results_to_shelf: true,
        }
    }
}

impl Config {
    pub fn key_for(&self, mode: Mode) -> Modifier {
        match mode {
            Mode::Convert => self.convert_key,
            Mode::Tools => self.tools_key,
            Mode::Actions => self.actions_key,
        }
    }

    /// Gives `mode` the modifier `key`. Refused (`false`, nothing changed) when another wheel has it.
    pub fn assign(&mut self, mode: Mode, key: Modifier) -> bool {
        let taken =
            [Mode::Convert, Mode::Tools, Mode::Actions].into_iter().any(|m| m != mode && self.key_for(m) == key);
        if taken {
            return false;
        }
        match mode {
            Mode::Convert => self.convert_key = key,
            Mode::Tools => self.tools_key = key,
            Mode::Actions => self.actions_key = key,
        }
        true
    }

    /// Hand-edited files can repeat a key; fall back to the defaults rather than make two wheels fight.
    fn sanitized(self) -> Config {
        let keys = [self.convert_key, self.tools_key, self.actions_key];
        if keys[0] == keys[1] || keys[0] == keys[2] || keys[1] == keys[2] {
            let d = Config::default();
            return Config { convert_key: d.convert_key, tools_key: d.tools_key, actions_key: d.actions_key, ..self };
        }
        self
    }

    /// The wheel the held modifiers open, if any.
    pub fn mode_for(&self, mods: gdk::ModifierType) -> Option<Mode> {
        [Mode::Convert, Mode::Tools, Mode::Actions].into_iter().find(|m| mods.contains(self.key_for(*m).mask()))
    }

    /// The wheel opened by pressing `key` itself.
    pub fn mode_for_key(&self, key: gdk::Key) -> Option<Mode> {
        [Mode::Convert, Mode::Tools, Mode::Actions].into_iter().find(|m| self.key_for(*m).matches_key(key))
    }

    /// Text of the chip for `mode`, e.g. `Shift Convert`.
    pub fn chip_label(&self, mode: Mode) -> String {
        format!("{} {}", self.key_for(mode).label(), mode.title())
    }

    /// The line under "Drop files here".
    pub fn hint(&self) -> String {
        format!(
            "Hold {} to convert · {} for tools · {} for actions",
            self.convert_key.label(),
            self.tools_key.label(),
            self.actions_key.label()
        )
    }
}

/// `$XDG_CONFIG_HOME/susen/config.json`, falling back to `~/.config`. Relative or empty
/// values are ignored, as the XDG spec says.
pub fn path_from(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = xdg
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(PathBuf::from).filter(|p| p.is_absolute()).map(|h| h.join(".config")))?;
    Some(base.join("susen").join("config.json"))
}

pub fn config_path() -> Option<PathBuf> {
    path_from(std::env::var_os("XDG_CONFIG_HOME"), std::env::var_os("HOME"))
}

/// Reads `path`; anything wrong with it gives the defaults.
pub fn load_from(path: &Path) -> Config {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Config>(&text).ok())
        .map(Config::sanitized)
        .unwrap_or_default()
}

/// Writes `config` to `path` through a temporary file, so a crash never leaves half a file.
pub fn save_to(path: &Path, config: &Config) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(config).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, path)
}

static LIVE: Mutex<Option<Config>> = Mutex::new(None);

fn live() -> std::sync::MutexGuard<'static, Option<Config>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The current settings, loaded from disk on first use.
pub fn get() -> Config {
    live().get_or_insert_with(|| config_path().map(|p| load_from(&p)).unwrap_or_default()).clone()
}

/// Changes the settings and saves them. A failed save is reported on stderr; the change still applies.
pub fn update(change: impl FnOnce(&mut Config)) {
    let mut config = get();
    change(&mut config);
    if let Some(path) = config_path()
        && let Err(e) = save_to(&path, &config)
    {
        eprintln!("susen: could not save {}: {e}", path.display());
    }
    *live() = Some(config);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/susen/config.json");
        let config = Config {
            convert_key: Modifier::Super,
            tools_key: Modifier::Shift,
            output_dir: Some("/tmp/out".into()),
            add_results_to_shelf: false,
            ..Config::default()
        };
        save_to(&path, &config).unwrap();
        assert_eq!(load_from(&path), config);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_from(&dir.path().join("nope.json")), Config::default());
    }

    #[test]
    fn corrupt_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        for junk in ["", "{not json", "[1,2]", r#"{"convert_key": "Hyper"}"#, "\u{0}\u{1}"] {
            std::fs::write(&path, junk).unwrap();
            assert_eq!(load_from(&path), Config::default(), "{junk:?}");
        }
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(load_from(&path), Config::default());
    }

    #[test]
    fn missing_keys_use_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"add_results_to_shelf": false, "future_setting": 1}"#).unwrap();
        let loaded = load_from(&path);
        assert!(!loaded.add_results_to_shelf);
        assert_eq!(Config { add_results_to_shelf: true, ..loaded }, Config::default());
    }

    #[test]
    fn duplicate_keys_in_a_file_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"convert_key": "Alt", "output_dir": "/x"}"#).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.convert_key, Modifier::Shift);
        assert_eq!(loaded.output_dir, Some("/x".into()));
    }

    #[test]
    fn duplicates_are_refused() {
        let mut c = Config::default();
        assert!(!c.assign(Mode::Convert, Modifier::Ctrl));
        assert_eq!(c, Config::default());
        assert!(c.assign(Mode::Convert, Modifier::Super));
        assert!(c.assign(Mode::Tools, Modifier::Shift));
        assert!(c.assign(Mode::Convert, Modifier::Super), "keeping a key is not a duplicate");
    }

    #[test]
    fn modifiers_pick_the_wheel() {
        let mut c = Config::default();
        assert_eq!(c.mode_for(gdk::ModifierType::SHIFT_MASK), Some(Mode::Convert));
        assert_eq!(c.mode_for(gdk::ModifierType::empty()), None);
        c.assign(Mode::Convert, Modifier::Super);
        assert_eq!(c.mode_for(gdk::ModifierType::SHIFT_MASK), None);
        assert_eq!(c.mode_for(gdk::ModifierType::SUPER_MASK), Some(Mode::Convert));
        assert_eq!(c.mode_for_key(gdk::Key::Super_L), Some(Mode::Convert));
        assert_eq!(c.mode_for_key(gdk::Key::Control_R), Some(Mode::Tools));
        assert_eq!(c.chip_label(Mode::Convert), "Super Convert");
        assert!(c.hint().starts_with("Hold Super to convert"));
    }

    #[test]
    fn config_path_follows_xdg() {
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(path_from(os("/xdg"), os("/home/u")), Some("/xdg/susen/config.json".into()));
        assert_eq!(path_from(None, os("/home/u")), Some("/home/u/.config/susen/config.json".into()));
        assert_eq!(path_from(os("relative"), os("/home/u")), Some("/home/u/.config/susen/config.json".into()));
        assert_eq!(path_from(os(""), None), None);
    }
}
