//! The command line: headless conversions for file managers and scripts.
//!
//! ```text
//! clementine --convert png FILE…   convert, print each output path
//! clementine --tool compress FILE… run a tool (compress, rotate, zip…), print each output path
//! clementine --list FILE…          print what the wheels would offer, one option per line
//! clementine --wheel FILE…         open the window with the files and the Convert wheel open
//! ```
//!
//! These flags are read here, before GTK starts, so GApplication never sees them. Anything
//! else (plain file arguments, GApplication's own flags) is left to GApplication.
//!
//! An option id is `convert:<ext>` or `tool:<name>`. A tool name is the same for every kind of
//! file: `tool:compress` is CompressImage on a photo and CompressPdf on a PDF, so a file manager
//! can ask for "compress" without knowing what Clementine calls it internally.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use crate::backend;
use crate::catalog::{Mode, Op, Tool, canonical, options};
use crate::config;

/// The hint passed to `GApplication::open` so the window opens the Convert wheel.
pub const WHEEL_HINT: &str = "wheel";

const USAGE: &str = "\
Usage:
  clementine [FILE…]                 open the window, files on the shelf
  clementine --wheel FILE…           open the window with the Convert wheel open
  clementine --convert FORMAT FILE…  convert (e.g. png, mp3, pdf) and print the results
  clementine --tool TOOL FILE…       run a tool and print the results
  clementine --list FILE…            print the options for these files, one per line

Tools: compress, resize, rotate, strip-metadata, half-size, mute, normalize, split, merge,
extract, zip (each only where it applies).

--list prints `id<TAB>label`, plus `<TAB>unavailable: missing <programs>` when a program the
option needs is not installed.

--notify (with --convert or --tool) also shows the result as a desktop notification.
Exit status: 0 on success, 1 when a conversion fails, 2 on a usage error.";

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Convert { target: String, files: Vec<PathBuf>, notify: bool },
    Tool { name: String, files: Vec<PathBuf>, notify: bool },
    List { files: Vec<PathBuf> },
    Wheel { files: Vec<PathBuf> },
    Help,
}

/// What `main` does next.
pub enum Next {
    /// Not a Clementine flag: start the GUI with the arguments as they are.
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
    let mut only_files = false;
    let mut it = args.iter();
    // A Clementine flag must come first, so `clementine --convert.png` (a file) still opens the GUI.
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
            "--tool" => Command::Tool { name: value("--tool")?, files: Vec::new(), notify: false },
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
        Command::Convert { files: f, notify: n, .. } | Command::Tool { files: f, notify: n, .. } => {
            *f = files;
            *n = notify;
        }
        Command::List { files: f } | Command::Wheel { files: f } => *f = files,
    }
    let needs_files = !matches!(cmd, Command::Help);
    if needs_files && cmd_files(&cmd).is_empty() {
        return Err("no files given".into());
    }
    Ok(Some(cmd))
}

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
        Tool::ResizeHalf => "resize",
        Tool::Rotate90 => "rotate",
        Tool::StripMetadata => "strip-metadata",
        Tool::HalfResVideo => "half-size",
        Tool::MuteVideo => "mute",
        Tool::NormalizeAudio => "normalize",
        Tool::SplitPdf => "split",
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

/// One line of `--list`.
pub fn list_line(op: Op, missing: &[String]) -> String {
    let mut line = format!("{}\t{}", option_id(op), op.label());
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
            eprintln!("clementine: {e}\n\n{USAGE}");
            Next::Exit(2)
        }
    }
}

fn absolute(files: Vec<PathBuf>) -> Vec<PathBuf> {
    files.into_iter().map(|f| std::path::absolute(&f).unwrap_or(f)).collect()
}

fn run(cmd: Command) -> Next {
    let (op_id, files, notify) = match cmd {
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
        Command::Convert { target, files, notify } => (format!("convert:{target}"), files, notify),
        Command::Tool { name, files, notify } => (format!("tool:{name}"), files, notify),
    };
    let files = absolute(files);
    let fail = |msg: String| {
        eprintln!("clementine: {msg}");
        if notify {
            notify_send("Clementine: failed", &msg);
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
    match backend::run(op, &files, config::get().output_dir.as_deref()) {
        Ok(outputs) => {
            for out in &outputs {
                println!("{}", out.display());
            }
            if notify {
                let names: Vec<String> = outputs.iter().map(|o| display_name(o)).collect();
                notify_send(&format!("Clementine: {}", op.label()), &names.join("\n"));
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
    if std::io::stderr().is_terminal() && std::env::var_os("CLEMENTINE_FORCE_NOTIFY").is_none() {
        return;
    }
    let _ = Process::new("notify-send")
        .args(["--app-name=Clementine", "--icon=io.github.sphanx.Clementine", summary, body])
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Ok(Some(Command::Tool { name: "compress".into(), files: vec!["a.pdf".into()], notify: true }))
        );
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
    fn list_lines() {
        assert_eq!(list_line(Op::Convert("png"), &[]), "convert:png\tPNG");
        assert_eq!(
            list_line(Op::Tool(Tool::CompressPdf), &["gs".into()]),
            "tool:compress\tCompress\tunavailable: missing gs"
        );
    }
}
