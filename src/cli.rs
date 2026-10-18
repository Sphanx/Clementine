//! The command line: headless conversions for file managers and scripts.
//!
//! ```text
//! susen --convert png FILE…   convert, print each output path
//! susen --tool compress FILE… run a tool (compress, rotate, zip…), print each output path
//! susen --list FILE…          print what the wheels would offer, one option per line
//! susen --wheel FILE…         open the window with the files and the Convert wheel open
//! ```
//!
//! These flags are read here, before GTK starts, so GApplication never sees them. Anything
//! else (plain file arguments, GApplication's own flags) is left to GApplication.
//!
//! An option id is `convert:<ext>` or `tool:<name>`. A tool name is the same for every kind of
//! file: `tool:compress` is CompressImage on a photo and CompressPdf on a PDF, so a file manager
//! can ask for "compress" without knowing what Susen calls it internally.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use crate::backend;
use crate::catalog::{Mode, Op, Tool, canonical, options};
use crate::config;
use crate::params::{self, Crop, Params, Resize, SplitBy};

/// The hint passed to `GApplication::open` so the window opens the Convert wheel.
pub const WHEEL_HINT: &str = "wheel";

const USAGE: &str = "\
Usage:
  susen [FILE…]                 open the window, files on the shelf
  susen --wheel FILE…           open the window with the Convert wheel open
  susen --convert FORMAT FILE…  convert (e.g. png, mp3, pdf) and print the results
  susen --tool TOOL FILE…       run a tool and print the results
  susen --list FILE…            print the options for these files, one per line

Tools (each only where it applies): compress, resize, crop, rotate, strip-metadata, half-size,
mute, thumbnail, normalize, mono, fade, trim, split, speed, merge, pages, extract, zip.

Tools that ask for settings in the window take them as flags here; a flag left out keeps the
window's default (resize 50%, split into 2 parts, speed 2×…):
  trim       --start TIME --end TIME          (TIME: 90, 1:30, 1:02:03.5)
  split      --parts N | --every TIME         (audio, video; PDF split needs nothing)
  thumbnail  --at TIME
  speed      --speed 0.25…4
  fade       --fade-in TIME --fade-out TIME
  resize     --percent N | --fit WxH
  crop       --aspect W:H | --rect X,Y,W,H
  rotate     --degrees 90|180|270             (PDF; images always turn 90°)
  pages      --pages FIRST-LAST

--list prints `id<TAB>label`, then `<TAB>params` for a tool that takes the flags above, and
`<TAB>unavailable: missing <programs>` when a program the option needs is not installed.

--notify (with --convert or --tool) also shows the result as a desktop notification.
Exit status: 0 on success, 1 when a conversion fails, 2 on a usage error.";

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Convert { target: String, files: Vec<PathBuf>, notify: bool },
    Tool { name: String, files: Vec<PathBuf>, notify: bool, params: Vec<(String, String)> },
    List { files: Vec<PathBuf> },
    Wheel { files: Vec<PathBuf> },
    Help,
}

/// What `main` does next.
pub enum Next {
    /// Not a Susen flag: start the GUI with the arguments as they are.
    Gui,
    /// Start the GUI with these files and open the Convert wheel.
    Wheel(Vec<PathBuf>),
    /// Done without GTK; exit with this status.
    Exit(u8),
}

/// Parses everything after the program name. `Ok(None)` means "not ours, start the GUI".
pub fn parse(args: &[OsString]) -> Result<Option<Command>, String> {
    let mut mode: Option<Command> = None;
    let mut files = Vec::new();
    let mut notify = false;
    let mut settings: Vec<(String, String)> = Vec::new();
    let mut only_files = false;
    let mut it = args.iter();
    // A Susen flag must come first, so `susen --convert.png` (a file) still opens the GUI.
    let Some(first) = args.first().and_then(|a| a.to_str()) else { return Ok(None) };
    if !is_ours(first) {
        return Ok(None);
    }
    while let Some(arg) = it.next() {
        let s = arg.to_str().unwrap_or("");
        if only_files || !s.starts_with("--") && s != "-h" {
            files.push(PathBuf::from(arg));
            continue;
        }
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (s, None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            match inline.clone().or_else(|| it.next().and_then(|v| v.to_str()).map(str::to_string)) {
                Some(v) if !v.is_empty() && !v.starts_with("--") => Ok(v),
                _ => Err(format!("{name} needs a value")),
            }
        };
        let next = match flag {
            "--" => {
                only_files = true;
                continue;
            }
            "--notify" => {
                notify = true;
                continue;
            }
            "--help" | "-h" => Command::Help,
            "--convert" => Command::Convert { target: value("--convert")?, files: Vec::new(), notify: false },
            "--tool" => Command::Tool { name: value("--tool")?, files: Vec::new(), notify: false, params: Vec::new() },
            f if PARAM_FLAGS.contains(&f) => {
                let v = value(f)?;
                settings.push((f.trim_start_matches('-').to_string(), v));
                continue;
            }
            "--list" => Command::List { files: Vec::new() },
            "--wheel" => Command::Wheel { files: Vec::new() },
            other => return Err(format!("unknown option {other}")),
        };
        if mode.is_some() {
            return Err("give only one of --convert, --tool, --list, --wheel".into());
        }
        mode = Some(next);
    }
    let mut cmd = mode.ok_or("nothing to do")?;
    match &mut cmd {
        Command::Help => {}
        Command::Convert { files: f, notify: n, .. } => {
            *f = files;
            *n = notify;
        }
        Command::Tool { files: f, notify: n, params: p, .. } => {
            *f = files;
            *n = notify;
            *p = std::mem::take(&mut settings);
        }
        Command::List { files: f } | Command::Wheel { files: f } => *f = files,
    }
    if !settings.is_empty() {
        return Err("tool settings (--start, --percent…) only go with --tool".into());
    }
    let needs_files = !matches!(cmd, Command::Help);
    if needs_files && cmd_files(&cmd).is_empty() {
        return Err("no files given".into());
    }
    Ok(Some(cmd))
}

/// The flags that carry a tool's settings; see [`apply_settings`].
const PARAM_FLAGS: &[&str] = &[
    "--start",
    "--end",
    "--parts",
    "--every",
    "--at",
    "--speed",
    "--fade-in",
    "--fade-out",
    "--percent",
    "--fit",
    "--aspect",
    "--rect",
    "--degrees",
    "--pages",
];

fn is_ours(arg: &str) -> bool {
    let flag = arg.split_once('=').map_or(arg, |(f, _)| f);
    matches!(flag, "--convert" | "--tool" | "--list" | "--wheel" | "--help" | "-h" | "--notify")
}

fn cmd_files(cmd: &Command) -> &[PathBuf] {
    match cmd {
        Command::Convert { files, .. }
        | Command::Tool { files, .. }
        | Command::List { files }
        | Command::Wheel { files } => files,
        Command::Help => &[],
    }
}

/// The stable command-line name of a tool. Every kind's "compress" shares one name.
pub fn tool_name(tool: Tool) -> &'static str {
    match tool {
        Tool::CompressImage | Tool::CompressVideo | Tool::CompressAudio | Tool::CompressPdf => "compress",
        Tool::Resize => "resize",
        Tool::CropImage => "crop",
        Tool::Rotate90 | Tool::RotatePdf => "rotate",
        Tool::StripMetadata => "strip-metadata",
        Tool::HalfResVideo => "half-size",
        Tool::MuteVideo => "mute",
        Tool::Thumbnail => "thumbnail",
        Tool::NormalizeAudio => "normalize",
        Tool::MonoAudio => "mono",
        Tool::FadeAudio => "fade",
        Tool::Trim => "trim",
        Tool::SplitMedia | Tool::SplitPdf => "split",
        Tool::Speed => "speed",
        Tool::ExtractPages => "pages",
        Tool::MergePdf => "merge",
        Tool::Extract => "extract",
        Tool::Zip => "zip",
    }
}

/// `convert:png`, `tool:compress`, `action:…`.
pub fn option_id(op: Op) -> String {
    match op {
        Op::Convert(ext) => format!("convert:{ext}"),
        Op::Tool(t) => format!("tool:{}", tool_name(t)),
        Op::Action(a) => format!("action:{a:?}").to_lowercase(),
    }
}

/// Every convert and tool option the wheels offer for this selection, in wheel order.
pub fn offered(files: &[PathBuf]) -> Vec<Op> {
    let mut ops = options(Mode::Convert, files);
    ops.extend(options(Mode::Tools, files));
    ops
}

/// Resolves an option id (`convert:png`, `tool:compress`, or a bare `png` / `compress`)
/// to the op the wheel would run on these files.
pub fn resolve(id: &str, files: &[PathBuf]) -> Result<Op, String> {
    let id = id.trim().to_lowercase();
    let (kind, name) = match id.split_once(':') {
        Some((k, n)) => (Some(k), n.to_string()),
        None => (None, id.clone()),
    };
    let offered = offered(files);
    let found = offered.iter().copied().find(|op| match (op, kind) {
        (Op::Convert(ext), None | Some("convert")) => *ext == canonical(&name),
        (Op::Tool(t), None | Some("tool")) => tool_name(*t) == name,
        _ => false,
    });
    found.ok_or_else(|| {
        let ids: Vec<String> = offered.iter().map(|o| option_id(*o)).collect();
        if ids.is_empty() {
            format!("nothing applies to these files as one selection (mixed kinds?): {id}")
        } else {
            format!("{id} does not apply to these files; try one of: {}", ids.join(", "))
        }
    })
}

/// Applies `--start 1:30`-style settings on top of a tool's defaults. A setting that does not
/// belong to this tool is an error, so a typo never runs silently with the default.
pub fn apply_settings(base: Params, settings: &[(String, String)]) -> Result<Params, String> {
    let mut p = base;
    for (key, raw) in settings {
        let bad = || format!("--{key}: cannot read {raw:?}");
        let time = || params::parse_time(raw).ok_or_else(bad);
        let int = || raw.trim().trim_end_matches('%').parse::<u32>().map_err(|_| bad());
        let pair = |sep: char| -> Result<(u32, u32), String> {
            let (a, b) = raw.split_once(sep).ok_or_else(bad)?;
            Ok((a.trim().parse().map_err(|_| bad())?, b.trim().parse().map_err(|_| bad())?))
        };
        p = match (p, key.as_str()) {
            (Params::Trim { end, .. }, "start") => Params::Trim { start: time()?, end },
            (Params::Trim { start, .. }, "end") => Params::Trim { start, end: time()? },
            (Params::Split(_), "parts") => Params::Split(SplitBy::Parts(int()?)),
            (Params::Split(_), "every") => Params::Split(SplitBy::Every(time()?)),
            (Params::Thumbnail { .. }, "at") => Params::Thumbnail { at: time()? },
            (Params::Speed(_), "speed") => {
                Params::Speed(raw.trim().trim_end_matches(['x', '×']).parse().map_err(|_| bad())?)
            }
            (Params::Fade { fade_out, .. }, "fade-in") => Params::Fade { fade_in: time()?, fade_out },
            (Params::Fade { fade_in, .. }, "fade-out") => Params::Fade { fade_in, fade_out: time()? },
            (Params::Resize(_), "percent") => Params::Resize(Resize::Percent(int()?)),
            (Params::Resize(_), "fit") => {
                let (width, height) = pair('x')?;
                Params::Resize(Resize::Fit { width, height })
            }
            (Params::Crop(_), "aspect") => {
                let (a, b) = pair(':')?;
                Params::Crop(Crop::Aspect(a, b))
            }
            (Params::Crop(_), "rect") => {
                let v: Vec<u32> =
                    raw.split(',').map(|n| n.trim().parse()).collect::<Result<_, _>>().map_err(|_| bad())?;
                let [x, y, w, h] = v[..] else { return Err(bad()) };
                Params::Crop(Crop::Rect { x, y, w, h })
            }
            (Params::RotatePages(_), "degrees") => Params::RotatePages(int()?),
            (Params::Pages { .. }, "pages") => match raw.split_once('-') {
                Some(_) => {
                    let (first, last) = pair('-')?;
                    Params::Pages { first, last }
                }
                None => Params::Pages { first: int()?, last: int()? },
            },
            _ => return Err(format!("--{key} does not apply to this tool")),
        };
    }
    Ok(p)
}

/// The settings `op` runs with: `None` for an op that asks nothing, else the window's defaults
/// with `settings` applied, checked the way the dialog checks them.
pub fn settings_for(op: Op, files: &[PathBuf], settings: &[(String, String)]) -> Result<Option<Params>, String> {
    let tool = match op {
        Op::Tool(t) if t.needs_params() => t,
        _ if settings.is_empty() => return Ok(None),
        _ => return Err(format!("{} takes no settings", op.label())),
    };
    let facts = backend::facts(tool, files);
    let base = params::defaults(tool, &facts).ok_or_else(|| format!("{} has no defaults", op.label()))?;
    let p = apply_settings(base, settings)?;
    params::validate(&p, &facts)?;
    Ok(Some(p))
}

/// One line of `--list`.
pub fn list_line(op: Op, missing: &[String]) -> String {
    let mut line = format!("{}\t{}", option_id(op), op.label());
    if matches!(op, Op::Tool(t) if t.needs_params()) {
        line.push_str("\tparams");
    }
    if !missing.is_empty() {
        line.push_str(&format!("\tunavailable: missing {}", missing.join(", ")));
    }
    line
}

/// Reads the real command line. Called first thing in `main`, before GTK.
pub fn main_hook() -> Next {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match parse(&args) {
        Ok(None) => Next::Gui,
        Ok(Some(cmd)) => run(cmd),
        Err(e) => {
            eprintln!("susen: {e}\n\n{USAGE}");
            Next::Exit(2)
        }
    }
}

fn absolute(files: Vec<PathBuf>) -> Vec<PathBuf> {
    files.into_iter().map(|f| std::path::absolute(&f).unwrap_or(f)).collect()
}

fn run(cmd: Command) -> Next {
    let (op_id, files, notify, settings) = match cmd {
        Command::Help => {
            println!("{USAGE}");
            return Next::Exit(0);
        }
        Command::Wheel { files } => return Next::Wheel(absolute(files)),
        Command::List { files } => {
            let files = absolute(files);
            for op in offered(&files) {
                println!("{}", list_line(op, &backend::missing_programs(op, &files)));
            }
            return Next::Exit(0);
        }
        Command::Convert { target, files, notify } => (format!("convert:{target}"), files, notify, Vec::new()),
        Command::Tool { name, files, notify, params } => (format!("tool:{name}"), files, notify, params),
    };
    let files = absolute(files);
    let fail = |msg: String| {
        eprintln!("susen: {msg}");
        if notify {
            notify_send("Susen: failed", &msg);
        }
        Next::Exit(1)
    };
    if let Some(gone) = files.iter().find(|f| !f.exists()) {
        return fail(format!("{}: no such file", gone.display()));
    }
    let op = match resolve(&op_id, &files) {
        Ok(op) => op,
        Err(e) => return fail(e),
    };
    let missing = backend::missing_programs(op, &files);
    if !missing.is_empty() {
        return fail(format!("{} needs {}, which is not installed", op.label(), missing.join(", ")));
    }
    let params = match settings_for(op, &files, &settings) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    match backend::run(op, &files, params.as_ref(), config::get().output_dir.as_deref()) {
        Ok(outputs) => {
            for out in &outputs {
                println!("{}", out.display());
            }
            if notify {
                let names: Vec<String> = outputs.iter().map(|o| display_name(o)).collect();
                notify_send(&format!("Susen: {}", op.label()), &names.join("\n"));
            }
            Next::Exit(0)
        }
        Err(e) => fail(format!("{e:#}")),
    }
}

fn display_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// Best effort: a file manager runs us without a terminal, so this is the only feedback there.
fn notify_send(summary: &str, body: &str) {
    if std::io::stderr().is_terminal() && std::env::var_os("SUSEN_FORCE_NOTIFY").is_none() {
        return;
    }
    let _ = Process::new("notify-send").args(notify_args(summary, body)).status();
}

/// `notify-send`'s arguments. File names and tool errors come from untrusted files, so `--` ends
/// the options: a name like `--action=…` is shown, never parsed.
fn notify_args<'a>(summary: &'a str, body: &'a str) -> [&'a str; 5] {
    ["--app-name=Susen", "--icon=io.github.sphanx.Susen", "--", summary, body]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text_is_never_an_option() {
        let args = notify_args("--wait", "-x.png");
        let dashes = args.iter().position(|a| *a == "--").expect("no -- before the text");
        assert!(dashes < args.iter().position(|a| *a == "--wait").unwrap());
    }

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(format!("/nonexistent/{s}"))
    }

    #[test]
    fn plain_files_and_gapplication_flags_are_left_alone() {
        assert_eq!(parse(&args(&[])), Ok(None));
        assert_eq!(parse(&args(&["a.png", "b.png"])), Ok(None));
        assert_eq!(parse(&args(&["--gapplication-service"])), Ok(None));
        // A file whose name merely starts like a flag is still a file.
        assert_eq!(parse(&args(&["--convert.png"])), Ok(None));
    }

    #[test]
    fn convert_and_tool_take_a_value() {
        assert_eq!(
            parse(&args(&["--convert", "png", "a.jpg", "b.jpg"])),
            Ok(Some(Command::Convert {
                target: "png".into(),
                files: vec!["a.jpg".into(), "b.jpg".into()],
                notify: false
            }))
        );
        assert_eq!(
            parse(&args(&["--tool=compress", "--notify", "a.pdf"])),
            Ok(Some(Command::Tool {
                name: "compress".into(),
                files: vec!["a.pdf".into()],
                notify: true,
                params: Vec::new()
            }))
        );
        assert_eq!(
            parse(&args(&["--tool", "trim", "--start", "0:02", "--end=0:10", "a.mp4"])),
            Ok(Some(Command::Tool {
                name: "trim".into(),
                files: vec!["a.mp4".into()],
                notify: false,
                params: vec![("start".into(), "0:02".into()), ("end".into(), "0:10".into())]
            }))
        );
        assert!(parse(&args(&["--convert", "png", "--start", "1", "a.jpg"])).is_err(), "settings without --tool");
        assert!(parse(&args(&["--convert"])).is_err());
        assert!(parse(&args(&["--convert", "--list", "a"])).is_err());
    }

    #[test]
    fn list_wheel_and_double_dash() {
        assert_eq!(parse(&args(&["--list", "a.png"])), Ok(Some(Command::List { files: vec!["a.png".into()] })));
        assert_eq!(
            parse(&args(&["--wheel", "--", "--odd name.png"])),
            Ok(Some(Command::Wheel { files: vec!["--odd name.png".into()] }))
        );
        assert_eq!(parse(&args(&["--help"])), Ok(Some(Command::Help)));
    }

    #[test]
    fn usage_errors() {
        assert!(parse(&args(&["--list"])).is_err(), "no files");
        assert!(parse(&args(&["--wheel", "--list", "a"])).is_err(), "two modes");
        assert!(parse(&args(&["--convert", "png", "--bogus", "a"])).is_err(), "unknown flag");
    }

    #[test]
    fn compress_resolves_per_kind() {
        let cases = [
            ("a.jpg", Tool::CompressImage),
            ("a.mp4", Tool::CompressVideo),
            ("a.flac", Tool::CompressAudio),
            ("a.pdf", Tool::CompressPdf),
        ];
        for (file, tool) in cases {
            assert_eq!(resolve("tool:compress", &[p(file)]), Ok(Op::Tool(tool)), "{file}");
            assert_eq!(resolve("compress", &[p(file)]), Ok(Op::Tool(tool)), "{file} bare");
        }
        assert!(resolve("tool:compress", &[p("a.zip")]).is_err());
    }

    #[test]
    fn other_tools_resolve() {
        assert_eq!(resolve("tool:merge", &[p("a.pdf"), p("b.pdf")]), Ok(Op::Tool(Tool::MergePdf)));
        assert!(resolve("tool:merge", &[p("a.pdf")]).is_err());
        assert_eq!(resolve("tool:zip", &[p("a.png"), p("b.mp3")]), Ok(Op::Tool(Tool::Zip)));
        assert_eq!(resolve("tool:extract", &[p("a.tar.gz")]), Ok(Op::Tool(Tool::Extract)));
        assert_eq!(resolve("tool:half-size", &[p("a.mkv")]), Ok(Op::Tool(Tool::HalfResVideo)));
        // One name, the right tool per kind.
        assert_eq!(resolve("tool:rotate", &[p("a.png")]), Ok(Op::Tool(Tool::Rotate90)));
        assert_eq!(resolve("tool:rotate", &[p("a.pdf")]), Ok(Op::Tool(Tool::RotatePdf)));
        assert_eq!(resolve("tool:split", &[p("a.pdf")]), Ok(Op::Tool(Tool::SplitPdf)));
        assert_eq!(resolve("tool:split", &[p("a.mp3")]), Ok(Op::Tool(Tool::SplitMedia)));
        assert_eq!(resolve("tool:trim", &[p("a.wav")]), Ok(Op::Tool(Tool::Trim)));
    }

    #[test]
    fn convert_ids_are_canonical_and_checked() {
        assert_eq!(resolve("convert:png", &[p("a.jpg")]), Ok(Op::Convert("png")));
        assert_eq!(resolve("convert:JPEG", &[p("a.png")]), Ok(Op::Convert("jpg")));
        assert_eq!(resolve("mp3", &[p("a.mp4")]), Ok(Op::Convert("mp3")));
        assert!(resolve("convert:jpg", &[p("a.jpeg")]).is_err(), "already that format");
        assert!(resolve("convert:png", &[p("a.png"), p("b.mp3")]).is_err(), "mixed kinds");
        assert!(resolve("tool:png", &[p("a.jpg")]).is_err(), "wrong namespace");
    }

    #[test]
    fn every_offered_option_round_trips() {
        for file in ["a.jpg", "a.mp4", "a.wav", "a.pdf", "a.docx", "a.csv", "a.pptx", "a.srt", "a.zip"] {
            let files = [p(file), p(&format!("b{file}"))];
            for op in offered(&files) {
                assert_eq!(resolve(&option_id(op), &files), Ok(op), "{file}: {}", option_id(op));
            }
        }
    }

    #[test]
    fn settings_override_defaults() {
        let s = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let trim = Params::Trim { start: 0.0, end: 60.0 };
        assert_eq!(
            apply_settings(trim, &s(&[("start", "0:02"), ("end", "10.5")])),
            Ok(Params::Trim { start: 2.0, end: 10.5 })
        );
        assert_eq!(
            apply_settings(Params::Resize(Resize::Percent(50)), &s(&[("fit", "800x600")])),
            Ok(Params::Resize(Resize::Fit { width: 800, height: 600 }))
        );
        assert_eq!(
            apply_settings(Params::Resize(Resize::Percent(50)), &s(&[("percent", "25%")])),
            Ok(Params::Resize(Resize::Percent(25)))
        );
        assert_eq!(
            apply_settings(Params::Crop(Crop::Aspect(1, 1)), &s(&[("rect", "1,2,30,40")])),
            Ok(Params::Crop(Crop::Rect { x: 1, y: 2, w: 30, h: 40 }))
        );
        assert_eq!(
            apply_settings(Params::Pages { first: 1, last: 9 }, &s(&[("pages", "2-4")])),
            Ok(Params::Pages { first: 2, last: 4 })
        );
        assert_eq!(apply_settings(Params::Speed(2.0), &s(&[("speed", "1.5x")])), Ok(Params::Speed(1.5)));
        assert!(apply_settings(trim, &s(&[("percent", "50")])).is_err(), "wrong tool");
        assert!(apply_settings(trim, &s(&[("start", "soon")])).is_err(), "unreadable");
    }

    #[test]
    fn param_tools_always_get_params() {
        let files = [p("a.png")];
        // Defaults stand in for the dialog when no flag is given.
        assert_eq!(settings_for(Op::Tool(Tool::Resize), &files, &[]), Ok(Some(Params::Resize(Resize::Percent(50)))));
        assert_eq!(settings_for(Op::Tool(Tool::CompressImage), &files, &[]), Ok(None));
        assert!(settings_for(Op::Convert("jpg"), &files, &[("start".into(), "1".into())]).is_err());
        // Validation runs: 100% is not a resize.
        assert!(settings_for(Op::Tool(Tool::Resize), &files, &[("percent".into(), "100".into())]).is_err());
        for op in offered(&[p("a.mp4")]).into_iter().chain(offered(&[p("a.pdf")])).chain(offered(&files)) {
            if let Op::Tool(t) = op
                && t.needs_params()
            {
                // Trim's default end is the probed length, unknown for a missing file.
                if t != Tool::Trim {
                    assert!(matches!(settings_for(op, &files, &[]), Ok(Some(_))), "{t:?}");
                }
            }
        }
    }

    #[test]
    fn list_lines() {
        assert_eq!(list_line(Op::Convert("png"), &[]), "convert:png\tPNG");
        assert_eq!(
            list_line(Op::Tool(Tool::CompressPdf), &["gs".into()]),
            "tool:compress\tCompress\tunavailable: missing gs"
        );
        assert_eq!(list_line(Op::Tool(Tool::Trim), &[]), "tool:trim\tTrim…\tparams");
    }
}
