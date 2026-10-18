//! QA tests: catalog consistency, more real formats, hostile file names, broken inputs and
//! concurrent runs. Everything that touches real converters is `#[ignore]`d (slow); tests named
//! `bug_…` reproduce known bugs that are not fixed yet and are expected to fail:
//!
//! ```sh
//! cargo test -- --ignored --skip bug_     # everything that should pass
//! cargo test bug_ -- --ignored            # the open bugs
//! ```

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::backend::{
    self, Step, has_program, image_size, media_duration, missing_programs, pdf_page_count, plan, run,
};
use crate::catalog::{Kind, Mode, Op, Tool, canonical, extension, kind_of, options, targets};
use crate::params::{Crop, Params, Resize, SplitBy};

/// Every kind with a wheel, and a representative extension list for it (aliases included).
const EXTS: &[(Kind, &[&str])] = &[
    (Kind::Image, &["jpg", "jpeg", "png", "webp", "heic", "heif", "tif", "tiff", "avif", "bmp", "gif", "svg"]),
    (Kind::Audio, &["mp3", "m4a", "wav", "flac", "ogg", "oga", "opus", "aif", "aiff", "wma", "aac"]),
    (Kind::Video, &["mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "mpg", "mpeg", "flv"]),
    (Kind::Pdf, &["pdf"]),
    (Kind::Document, &["docx", "odt", "rtf", "md", "markdown", "txt", "html", "htm", "epub"]),
    (Kind::Spreadsheet, &["xlsx", "xls", "ods", "csv", "tsv"]),
    (Kind::Slides, &["pptx", "ppt", "odp"]),
    (Kind::Subtitle, &["srt", "vtt"]),
    (Kind::Archive, &["zip", "tar", "tar.gz", "gz", "tgz", "rar", "7z"]),
];

fn fake(name: &str) -> PathBuf {
    PathBuf::from(format!("/nonexistent-qa/{name}"))
}

// ---------------------------------------------------------------------------------------------
// Catalog consistency (fast)
// ---------------------------------------------------------------------------------------------

#[test]
fn every_extension_has_the_kind_it_is_listed_under() {
    for (kind, exts) in EXTS {
        for ext in *exts {
            for name in [format!("a.{ext}"), format!("A.{}", ext.to_uppercase())] {
                assert_eq!(kind_of(&fake(&name)), *kind, "{name}");
            }
        }
    }
}

#[test]
fn aliases_fold_to_a_member_of_the_same_kind() {
    for (kind, exts) in EXTS {
        for ext in *exts {
            let c = canonical(ext);
            // The canonical form must itself be recognised as the same kind…
            assert_eq!(kind_of(&fake(&format!("a.{c}"))), *kind, "{ext} → {c}");
            // …and canonical must be idempotent.
            assert_eq!(canonical(c), c, "{ext}");
        }
    }
    // Every target is already canonical, so `own_format_is_not_offered` can compare them.
    for (kind, _) in EXTS {
        for t in targets(*kind) {
            assert_eq!(canonical(t), *t, "{kind:?} target {t}");
        }
    }
}

#[test]
fn an_alias_is_never_offered_its_own_format() {
    for (alias, canon) in
        [("jpeg", "jpg"), ("tif", "tiff"), ("heif", "heic"), ("htm", "html"), ("tgz", "tar.gz"), ("aif", "aiff")]
    {
        let ops = options(Mode::Convert, &[fake(&format!("a.{alias}"))]);
        assert!(!ops.contains(&Op::Convert(canon)), "a.{alias} is offered {canon}: {ops:?}");
        let upper = options(Mode::Convert, &[fake(&format!("A.{}", alias.to_uppercase()))]);
        assert_eq!(ops, upper, "{alias} upper case");
    }
}

#[test]
fn every_target_of_every_extension_plans_and_never_overwrites() {
    let scratch = Path::new("/nonexistent-qa-scratch");
    for (_, exts) in EXTS {
        for ext in *exts {
            let files = vec![fake(&format!("a.{ext}"))];
            for mode in [Mode::Convert, Mode::Tools] {
                for op in options(mode, &files) {
                    let jobs = plan(op, &files, None, scratch).unwrap_or_else(|e| panic!("a.{ext} {op:?}: {e}"));
                    assert!(!jobs.is_empty(), "a.{ext} {op:?}");
                    for out in jobs.iter().flat_map(|j| &j.outputs) {
                        assert_ne!(out, &files[0], "a.{ext} {op:?} overwrites its input");
                        assert_eq!(out.parent(), files[0].parent(), "a.{ext} {op:?} writes elsewhere: {out:?}");
                    }
                    // A conversion's output carries the target's extension (or is a folder of pages).
                    if let Op::Convert(t) = op {
                        let out = &jobs[0].outputs[0];
                        let ok = extension(out) == t || out.extension().is_none();
                        assert!(ok, "a.{ext} → {t} is named {out:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn labels_are_unique_within_every_wheel() {
    let mut selections: Vec<Vec<PathBuf>> = Vec::new();
    for (_, exts) in EXTS {
        selections.push(vec![fake(&format!("a.{}", exts[0]))]);
        selections.push(vec![fake(&format!("a.{}", exts[0])), fake(&format!("b.{}", exts[exts.len() - 1]))]);
    }
    selections.push(vec![fake("a.png"), fake("b.mp3")]);
    selections.push(vec![fake("folderish")]);
    for files in &selections {
        for mode in [Mode::Convert, Mode::Tools, Mode::Actions] {
            let ops = options(mode, files);
            let labels: HashSet<String> = ops.iter().map(Op::label).collect();
            assert_eq!(labels.len(), ops.len(), "{mode:?} {files:?}: {ops:?}");
        }
    }
}

#[test]
fn mixed_selections_of_one_kind_convert_and_offer_tools() {
    // Aliases and different formats of one kind still share a wheel.
    let files = [fake("a.jpeg"), fake("b.JPG")];
    let ops = options(Mode::Convert, &files);
    assert!(!ops.contains(&Op::Convert("jpg")) && ops.contains(&Op::Convert("png")), "{ops:?}");
    let files = [fake("a.png"), fake("b.webp")];
    assert!(options(Mode::Convert, &files).contains(&Op::Convert("png")));
    assert!(options(Mode::Tools, &files).contains(&Op::Tool(Tool::Resize)));
    // Two kinds: actions and Zip only.
    let files = [fake("a.pdf"), fake("b.png")];
    assert!(options(Mode::Convert, &files).is_empty());
    assert_eq!(options(Mode::Tools, &files), vec![Op::Tool(Tool::Zip)]);
    // Nothing selected: nothing at all.
    assert!(options(Mode::Convert, &[]).is_empty() && options(Mode::Tools, &[]).is_empty());
}

#[test]
fn an_ops_programs_are_reported_missing_by_name() {
    // `missing_programs` must never panic or plan into the scratch stand-in for any op.
    for (_, exts) in EXTS {
        for ext in *exts {
            let files = vec![fake(&format!("a.{ext}"))];
            for op in [Mode::Convert, Mode::Tools].into_iter().flat_map(|m| options(m, &files)) {
                for p in missing_programs(op, &files) {
                    assert!(!p.is_empty() && !p.contains('/'), "{p}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Names and arguments (fast)
// ---------------------------------------------------------------------------------------------

#[test]
fn stem_keeps_unicode_and_odd_names() {
    for (name, want) in [
        ("çğıöşü İ.png", "çğıöşü İ"),
        ("emoji 🍊.PNG", "emoji 🍊"),
        (".hidden.png", ".hidden"),
        (".bashrc", ".bashrc"),
        ("noext", "noext"),
        ("a.tar.gz", "a"),
        ("a.TAR.GZ", "a"),
        ("a.gz", "a"),
        ("pct%d [0].png", "pct%d [0]"),
    ] {
        assert_eq!(backend::stem(&fake(name)), want, "{name}");
    }
}

/// Regression: an extension whose lower case is longer in bytes (`İ` → `i̇`) cut the stem
/// inside a character and panicked on the worker thread.
#[test]
fn stem_survives_extensions_that_grow_when_lowered() {
    assert_eq!(backend::stem(&fake("çğıöşü.İ")), "çğıöşü");
    assert_eq!(backend::stem(&fake("x.İMG")), "x");
    let jobs = plan(Op::Tool(Tool::Zip), &[fake("çğıöşü.İ")], None, Path::new("/s")).unwrap();
    assert_eq!(jobs[0].outputs, vec![fake("çğıöşü.zip")]);
}

fn run_args(op: Op, files: &[PathBuf]) -> Vec<Vec<String>> {
    let jobs = plan(op, files, None, Path::new("/s")).unwrap();
    jobs.iter()
        .flat_map(|j| &j.steps)
        .filter_map(|s| match s {
            Step::Run(c) => Some(c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()),
            _ => None,
        })
        .collect()
}

/// Regression: Zip stores bare names, so `-v.png` was read by zip as an option.
#[test]
fn zip_never_reads_a_file_name_as_an_option() {
    let args = run_args(Op::Tool(Tool::Zip), &[fake("-v.png"), fake("b.png")]);
    let zip = &args[0];
    let dashdash = zip.iter().position(|a| a == "--").expect("zip args end options with --");
    assert!(zip.iter().position(|a| a == "-v.png").unwrap() > dashdash, "{zip:?}");
}

/// Regression: Ghostscript expands `%d` in `-sOutputFile`, so `scan%d.pdf` became one file per page.
#[test]
fn ghostscript_output_escapes_percent() {
    let args = run_args(Op::Tool(Tool::CompressPdf), &[fake("scan%d.pdf")]);
    let out = args[0].iter().find(|a| a.starts_with("-sOutputFile=")).unwrap();
    assert_eq!(out, "-sOutputFile=/nonexistent-qa/scan%%d (compressed).pdf");
}

// ---------------------------------------------------------------------------------------------
// Real-file helpers
// ---------------------------------------------------------------------------------------------

fn sh_in(dir: &Path, program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn sh(dir: &Path, program: &str, args: &[&str]) {
    assert!(sh_in(dir, program, args), "making a sample with {program} {args:?} failed");
}

fn im() -> &'static str {
    if has_program("magick") { "magick" } else { "convert" }
}

/// Whether this ImageMagick can write `format` (HEIC/AVIF need libheif encoder plugins).
fn im_writes(format: &str) -> bool {
    let out = Command::new(im()).args(["-list", "format"]).output().map(|o| o.stdout).unwrap_or_default();
    String::from_utf8_lossy(&out).lines().any(|l| {
        let mut w = l.split_whitespace();
        w.next().is_some_and(|f| f.trim_end_matches('*').eq_ignore_ascii_case(format))
            && l.split_whitespace().any(|m| m.len() == 3 && m.as_bytes()[1] == b'w')
    })
}

fn ff(dir: &Path, args: &[&str]) {
    let mut all = vec!["-hide_banner", "-loglevel", "error", "-y"];
    all.extend_from_slice(args);
    sh(dir, "ffmpeg", &all);
}

/// Runs `f` on a thread; `None` if it did not finish in time (the thread is left behind).
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs)).ok()
}

/// Settings for a tool that asks, sized for 1-second, 64×48, 1+ page samples.
fn params_for(op: Op) -> Option<Params> {
    let Op::Tool(t) = op else { return None };
    Some(match t {
        Tool::Trim => Params::Trim { start: 0.25, end: 0.75 },
        Tool::SplitMedia => Params::Split(SplitBy::Parts(2)),
        Tool::Speed => Params::Speed(2.0),
        Tool::FadeAudio => Params::Fade { fade_in: 0.2, fade_out: 0.3 },
        Tool::Thumbnail => Params::Thumbnail { at: 0.5 },
        Tool::CropImage => Params::Crop(Crop::Aspect(1, 1)),
        Tool::Resize => Params::Resize(Resize::Percent(50)),
        Tool::RotatePdf => Params::RotatePages(90),
        Tool::ExtractPages => Params::Pages { first: 1, last: 1 },
        _ => return None,
    })
}

/// Whether `out` really is a file of the kind its name says (not just non-empty).
fn looks_valid(out: &Path) -> Result<(), String> {
    if out.is_dir() {
        let entries: Vec<PathBuf> =
            std::fs::read_dir(out).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
        if entries.is_empty() {
            return Err(format!("{} is an empty folder", out.display()));
        }
        return entries.iter().try_for_each(|e| looks_valid(e));
    }
    let len = out.metadata().map(|m| m.len()).map_err(|e| format!("{}: {e}", out.display()))?;
    if len == 0 {
        return Err(format!("{} is empty", out.display()));
    }
    let ok = match kind_of(out) {
        // `image_size` reads a name with `%` or glob characters through a safely named link.
        Kind::Image => image_size(out).is_some() || image_on_stdin(out),
        Kind::Audio | Kind::Video => media_duration(out).is_some(),
        Kind::Pdf => pdf_page_count(out).is_some_and(|n| n > 0),
        Kind::Archive if extension(out) == "zip" => sh_in(Path::new("/"), "unzip", &["-tqq", &out.to_string_lossy()]),
        Kind::Archive => sh_in(Path::new("/"), "tar", &["-tf", &out.to_string_lossy()]),
        _ => true,
    };
    if ok { Ok(()) } else { Err(format!("{} does not read back as {}", out.display(), extension(out))) }
}

fn image_on_stdin(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else { return false };
    Command::new(im())
        .args(["-[0]", "-format", "%w", "info:"])
        .stdin(file)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// Everything under `dir`, recursively, as relative paths.
fn listing(dir: &Path) -> HashSet<PathBuf> {
    fn walk(base: &Path, d: &Path, acc: &mut HashSet<PathBuf>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            acc.insert(p.strip_prefix(base).unwrap().to_path_buf());
            if p.is_dir() {
                walk(base, &p, acc);
            }
        }
    }
    let mut acc = HashSet::new();
    walk(dir, dir, &mut acc);
    acc
}

/// Runs `op` on `files` with a timeout and checks the result: outputs valid, and nothing
/// appeared in `dir` that is not one of them (or inside one of them).
fn run_checked(op: Op, files: &[PathBuf], dir: &Path, secs: u64) -> Result<Vec<PathBuf>, String> {
    isolate_libreoffice();
    let before = listing(dir);
    let params = params_for(op);
    let f = files.to_vec();
    let result = within(secs, move || run(op, &f, params.as_ref(), None).map_err(|e| format!("{e:#}")))
        .ok_or_else(|| format!("timed out after {secs} s"))??;
    for out in &result {
        looks_valid(out)?;
    }
    let stray: Vec<PathBuf> = listing(dir)
        .difference(&before)
        .filter(|p| !result.iter().any(|o| dir.join(p).starts_with(o)))
        .cloned()
        .collect();
    if !stray.is_empty() {
        return Err(format!("left files it did not report: {stray:?} (reported {result:?})"));
    }
    Ok(result)
}

/// Like [`run_checked`] for an input that must fail: an error, in time, with nothing left behind.
fn run_failing(op: Op, files: &[PathBuf], dir: &Path, secs: u64) -> Result<String, String> {
    match run_any(op, files, dir, secs)? {
        Outcome::Failed(e) => Ok(e),
        Outcome::Worked(outs) => Err(format!("succeeded: {outs:?}")),
    }
}

enum Outcome {
    /// Failed cleanly, with this message.
    Failed(String),
    /// Worked, and its outputs read back.
    Worked(Vec<PathBuf>),
}

/// Runs once, in time, without a panic: a clean failure (nothing left behind, a message) or a
/// success whose outputs read back and that left nothing else behind. Anything else is `Err`.
fn run_any(op: Op, files: &[PathBuf], dir: &Path, secs: u64) -> Result<Outcome, String> {
    isolate_libreoffice();
    let before = listing(dir);
    let params = params_for(op);
    let f = files.to_vec();
    let result = within(secs, move || {
        std::panic::catch_unwind(|| run(op, &f, params.as_ref(), None).map_err(|e| format!("{e:#}")))
    })
    .ok_or_else(|| format!("timed out after {secs} s"))?
    .map_err(|_| "panicked".to_string())?;
    let new: Vec<PathBuf> = listing(dir).difference(&before).cloned().collect();
    match result {
        Err(_) if !new.is_empty() => Err(format!("failed but left {new:?}")),
        Err(e) if e.trim().is_empty() => Err("failed with an empty message".into()),
        Err(e) => Ok(Outcome::Failed(e)),
        Ok(outs) => {
            outs.iter().try_for_each(|o| looks_valid(o)).map_err(|e| format!("succeeded with a bad result: {e}"))?;
            let stray: Vec<_> = new.iter().filter(|p| !outs.iter().any(|o| dir.join(p).starts_with(o))).collect();
            if stray.is_empty() {
                Ok(Outcome::Worked(outs))
            } else {
                Err(format!("left files it did not report: {stray:?} (reported {outs:?})"))
            }
        }
    }
}

/// Other Susen processes (a second checkout, the GUI, the file manager menus) share the
/// LibreOffice profile in `~/.cache/susen`, and LibreOffice fails silently when two
/// processes use one profile (fixed with a `flock`, see `qa_two_processes_convert_with_libreoffice_at_once`). The
/// tests here get a cache folder of their own.
fn isolate_libreoffice() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let cache = std::env::temp_dir().join(format!("susen-qa-cache-{}", std::process::id()));
        std::fs::create_dir_all(&cache).unwrap();
        // SAFETY: set once, before any test of this module starts a converter thread; other
        // threads only read the variable.
        unsafe { std::env::set_var("XDG_CACHE_HOME", cache) };
    });
}

fn every_op(files: &[PathBuf]) -> Vec<Op> {
    [Mode::Convert, Mode::Tools].into_iter().flat_map(|m| options(m, files)).collect()
}

/// Ops whose programs are present (HEIC/AVIF also need an ImageMagick that can write them).
fn runnable(op: Op, files: &[PathBuf]) -> Result<(), String> {
    let missing = missing_programs(op, files);
    if !missing.is_empty() {
        return Err(format!("needs {}", missing.join(", ")));
    }
    if let Op::Convert(t @ ("heic" | "avif")) = op
        && kind_of(&files[0]) == Kind::Image
        && !im_writes(t)
    {
        return Err(format!("ImageMagick cannot write {t} here"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// More formats, for real
// ---------------------------------------------------------------------------------------------

/// A minimal ODP (one slide with a title), zipped with `zip` the way ODF wants it.
fn write_odp(dir: &Path, name: &str) {
    let src = dir.join(format!("{name}-src"));
    std::fs::create_dir_all(src.join("META-INF")).unwrap();
    std::fs::write(src.join("mimetype"), "application/vnd.oasis.opendocument.presentation").unwrap();
    std::fs::write(
        src.join("META-INF/manifest.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">
 <manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.presentation"/>
 <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#,
    )
    .unwrap();
    std::fs::write(
        src.join("content.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0"
 xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" office:version="1.2">
 <office:body><office:presentation>
  <draw:page draw:name="One">
   <draw:frame svg:x="2cm" svg:y="2cm" svg:width="20cm" svg:height="3cm"><draw:text-box>
    <text:p>Hello slides</text:p></draw:text-box></draw:frame>
  </draw:page>
  <draw:page draw:name="Two">
   <draw:frame svg:x="2cm" svg:y="2cm" svg:width="20cm" svg:height="3cm"><draw:text-box>
    <text:p>Second</text:p></draw:text-box></draw:frame>
  </draw:page>
 </office:presentation></office:body>
</office:document-content>"#,
    )
    .unwrap();
    let out = dir.join(format!("{name}.odp"));
    sh(&src, "zip", &["-X", "-0", "-q", &out.to_string_lossy(), "mimetype"]);
    sh(&src, "zip", &["-X", "-r", "-q", &out.to_string_lossy(), "content.xml", "META-INF"]);
    std::fs::remove_dir_all(&src).unwrap();
}

/// LibreOffice into `dir`, from a sample already there; the result is renamed to `to`.
fn lo(dir: &Path, from: &str, ext: &str, to: &str) -> bool {
    let tmp = dir.join(format!("lo-{to}"));
    std::fs::create_dir_all(&tmp).unwrap();
    let ok = sh_in(
        dir,
        "soffice",
        &[
            &format!(
                "-env:UserInstallation=file://{}/susen-qa-lo-{}",
                std::env::temp_dir().display(),
                std::process::id()
            ),
            "--headless",
            "--norestore",
            "--convert-to",
            ext,
            "--outdir",
            &tmp.to_string_lossy(),
            from,
        ],
    );
    let made = std::fs::read_dir(&tmp).unwrap().flatten().next().map(|e| e.path());
    let ok = ok && made.is_some_and(|m| std::fs::rename(m, dir.join(to)).is_ok());
    std::fs::remove_dir_all(&tmp).unwrap();
    ok
}

/// Samples of the formats the stock real test does not cover. Each has its own stem, so no two
/// samples ever compete for an output name.
fn more_samples(dir: &Path) -> Vec<PathBuf> {
    isolate_libreoffice();
    let mut names: Vec<String> = Vec::new();
    let mut add = |n: &str| names.push(n.to_string());
    let lavfi = |s: &str| ["-f", "lavfi", "-i", s].map(String::from);

    // Images.
    sh(dir, im(), &["-size", "64x48", "gradient:orange-white", "base.png"]);
    for (ext, name) in [("webp", "i_webp.webp"), ("tif", "i_tif.TIF"), ("bmp", "i_bmp.bmp"), ("jpg", "i_jpeg.jpeg")] {
        sh(dir, im(), &["base.png", &format!("{ext}:{name}")]);
        add(name);
    }
    for (fmt, name) in [("heic", "i_heic.heic"), ("avif", "i_avif.avif")] {
        if im_writes(fmt) {
            sh(dir, im(), &["base.png", name]);
            add(name);
        } else {
            println!("no {fmt} sample: ImageMagick cannot write it");
        }
    }
    std::fs::write(
        dir.join("i_svg.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="48"><rect width="64" height="48" fill="orange"/></svg>"#,
    )
    .unwrap();
    add("i_svg.svg");
    // A two-frame TIFF, to check only the first frame is taken.
    sh(dir, im(), &["base.png", "base.png", "i_multi.tiff"]);
    add("i_multi.tiff");
    std::fs::remove_file(dir.join("base.png")).unwrap();

    // Audio.
    let sine: Vec<String> = [&lavfi("sine=frequency=440:duration=1")[..], &["-ac".into(), "2".into()]].concat();
    for (name, codec) in [
        ("a_ogg.ogg", &["-c:a", "libvorbis"][..]),
        ("a_opus.opus", &["-c:a", "libopus"]),
        ("a_flac.flac", &["-c:a", "flac"]),
        ("a_m4a.m4a", &["-c:a", "aac"]),
        ("a_wma.wma", &["-c:a", "wmav2"]),
        ("a_aac.aac", &["-c:a", "aac"]),
        ("a_aiff.aif", &["-c:a", "pcm_s16be"]),
    ] {
        let mut args: Vec<&str> = sine.iter().map(String::as_str).collect();
        args.extend_from_slice(codec);
        args.push(name);
        ff(dir, &args);
        add(name);
    }

    // Video.
    let src: Vec<String> = [
        &lavfi("testsrc=size=128x96:rate=25:duration=1")[..],
        &lavfi("sine=duration=1"),
        &["-shortest".into(), "-pix_fmt".into(), "yuv420p".into()],
    ]
    .concat();
    for (name, codec) in [
        ("v_mov.mov", &["-c:v", "libx264", "-c:a", "aac"][..]),
        ("v_mkv.mkv", &["-c:v", "libx264", "-c:a", "aac"]),
        ("v_webm.webm", &["-c:v", "libvpx-vp9", "-c:a", "libopus"]),
        ("v_avi.avi", &["-c:v", "mpeg4", "-c:a", "libmp3lame"]),
        ("v_wmv.wmv", &["-c:v", "wmv2", "-c:a", "wmav2"]),
        ("v_mpg.mpg", &["-c:v", "mpeg2video", "-c:a", "mp2"]),
        ("v_mpeg.mpeg", &["-c:v", "mpeg1video", "-c:a", "mp2"]),
        ("v_flv.flv", &["-c:v", "flv1", "-c:a", "libmp3lame", "-ar", "44100"]),
        ("v_m4v.m4v", &["-c:v", "libx264", "-c:a", "aac"]),
    ] {
        let mut args: Vec<&str> = src.iter().map(String::as_str).collect();
        args.extend_from_slice(codec);
        args.push(name);
        ff(dir, &args);
        add(name);
    }

    // Subtitles and documents.
    std::fs::write(
        dir.join("s_vtt.vtt"),
        "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n<i>Hello</i> there\n\n00:00:03.000 --> 00:00:04.500\nSecond line\n",
    )
    .unwrap();
    add("s_vtt.vtt");
    std::fs::write(dir.join("d_txt.txt"), "Plain text\n\nSecond paragraph with ümlaut and İ.\n").unwrap();
    add("d_txt.txt");
    std::fs::write(
        dir.join("d_html.html"),
        "<!doctype html><html><head><meta charset=utf-8><title>T</title></head><body><h1>Title</h1><p>Some <b>bold</b> text.</p><table><tr><td>1</td><td>2</td></tr></table></body></html>",
    )
    .unwrap();
    add("d_html.html");
    std::fs::write(dir.join("src.md"), "# Book\n\n## Chapter\n\nSome *text*.\n").unwrap();
    sh(dir, "pandoc", &["src.md", "-o", "d_epub.epub", "--metadata", "title=Book"]);
    add("d_epub.epub");
    sh(dir, "pandoc", &["src.md", "-o", "d_odt.odt"]);
    add("d_odt.odt");
    std::fs::rename(dir.join("src.md"), dir.join("d_markdown.markdown")).unwrap();
    add("d_markdown.markdown");

    // Spreadsheets and slides (LibreOffice).
    std::fs::write(dir.join("t_tsv.tsv"), "a\tb\n1\t2\n3\t4\n").unwrap();
    add("t_tsv.tsv");
    if has_program("soffice") {
        std::fs::write(dir.join("t.csv"), "a,b\n1,2\n3,4\n").unwrap();
        for (ext, name) in [("xls", "t_xls.xls"), ("xlsx", "t_xlsx.xlsx"), ("ods", "t_ods.ods")] {
            assert!(lo(dir, "t.csv", ext, name), "making {name}");
            add(name);
        }
        std::fs::remove_file(dir.join("t.csv")).unwrap();
        write_odp(dir, "p_odp");
        add("p_odp.odp");
        for (ext, name) in [("pptx", "p_pptx.pptx"), ("ppt", "p_ppt.ppt")] {
            assert!(lo(dir, "p_odp.odp", ext, name), "making {name}");
            add(name);
        }
    } else {
        println!("no spreadsheet or slide samples: LibreOffice is not installed");
    }

    // Archives.
    std::fs::write(dir.join("payload.txt"), "payload\n").unwrap();
    sh(dir, "tar", &["-cf", "z_tar.tar", "payload.txt"]);
    add("z_tar.tar");
    sh(dir, "tar", &["-czf", "z_tgz.tgz", "payload.txt"]);
    add("z_tgz.tgz");
    sh(dir, "gzip", &["-k", "payload.txt"]);
    std::fs::rename(dir.join("payload.txt.gz"), dir.join("z_gz.gz")).unwrap();
    add("z_gz.gz");
    if has_program("7z") {
        sh(dir, "7z", &["a", "-bd", "z_7z.7z", "payload.txt"]);
        add("z_7z.7z");
    }
    if has_program("rar") {
        sh(dir, "rar", &["a", "-idq", "z_rar.rar", "payload.txt"]);
        add("z_rar.rar");
    }
    std::fs::remove_file(dir.join("payload.txt")).unwrap();

    names.iter().map(|n| dir.join(n)).collect()
}

/// Every offered op on every extra sample, outputs checked by reading them back.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_every_op_on_more_formats() {
    let dir = tempfile::tempdir().unwrap();
    let samples = more_samples(dir.path());
    let failures = std::sync::Mutex::new(Vec::new());
    let ran = std::sync::atomic::AtomicUsize::new(0);
    // Four samples at a time; LibreOffice calls queue on their lock.
    std::thread::scope(|s| {
        for chunk in samples.chunks(samples.len().div_ceil(4)) {
            let (failures, ran) = (&failures, &ran);
            s.spawn(move || {
                for file in chunk {
                    let files = vec![file.clone()];
                    for op in every_op(&files) {
                        let name = format!("{} → {}", file.file_name().unwrap().to_string_lossy(), op.label());
                        if let Err(why) = runnable(op, &files) {
                            println!("skip {name}: {why}");
                            continue;
                        }
                        ran.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        match within(180, {
                            let files = files.clone();
                            move || run(op, &files, params_for(op).as_ref(), None).map_err(|e| format!("{e:#}"))
                        }) {
                            None => failures.lock().unwrap().push(format!("{name}: timed out")),
                            Some(Err(e)) => failures.lock().unwrap().push(format!("{name}: {e}")),
                            Some(Ok(outs)) => match outs.iter().try_for_each(|o| looks_valid(o)) {
                                Ok(()) => println!("ok   {name}"),
                                Err(e) => failures.lock().unwrap().push(format!("{name}: {e}")),
                            },
                        }
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    println!("{} ops ran, {} failed", ran.into_inner(), failures.len());
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// A two-frame TIFF converts its first frame only (one file, not `name-0`/`name-1`).
#[test]
#[ignore = "slow: runs the converters"]
fn qa_multi_frame_images_give_one_still() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    sh(d, im(), &["-size", "8x8", "xc:red", "xc:blue", "multi.tiff"]);
    let outs = run_checked(Op::Convert("png"), &[d.join("multi.tiff")], d, 60).unwrap();
    assert_eq!(outs, vec![d.join("multi.png")]);
}

/// VTT → SRT keeps the cue text; SRT → VTT gets a header.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_subtitles_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("a.vtt"), "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n").unwrap();
    let srt = run_checked(Op::Convert("srt"), &[d.join("a.vtt")], d, 30).unwrap();
    let text = std::fs::read_to_string(&srt[0]).unwrap();
    assert!(text.contains("00:00:01,000 --> 00:00:02,000") && text.contains("Hello"), "{text}");
    let vtt = run_checked(Op::Convert("vtt"), &srt, d, 30).unwrap();
    assert!(std::fs::read_to_string(&vtt[0]).unwrap().starts_with("WEBVTT"));
}

// ---------------------------------------------------------------------------------------------
// Hostile names
// ---------------------------------------------------------------------------------------------

/// Stems that every program must take literally.
const HOSTILE: &[&str] = &[
    "with space",
    "çğıöşü İ",
    "emoji 🍊",
    "quote ' \" end",
    "dollar $HOME",
    "semi;colon && ls",
    "-leading",
    "br[0]",
    "paren (2)",
    "back\\slash",
    "hash #1",
    ".hidden",
    "tab\there",
    "newline\nhere",
    "colon: a",
];

/// Stems with `%` sequences: ImageMagick, pdfseparate and ffmpeg's image writer read them as patterns.
const PERCENT: &[&str] = &["pct%d", "pct %03d", "pct%s%x"];

/// One sample of each kind under `stem`, and the ops that are tried on it.
fn hostile_samples(dir: &Path, stem: &str, ext_case_upper: bool) -> Vec<(PathBuf, Vec<Op>)> {
    let e = |ext: &str| if ext_case_upper { ext.to_uppercase() } else { ext.to_string() };
    let mut out = Vec::new();
    let mut put = |src: &Path, ext: &str, ops: Vec<Op>| {
        let to = dir.join(format!("{stem}.{}", e(ext)));
        std::fs::copy(src, &to).unwrap();
        out.push((to, ops));
    };
    let base = dir.parent().unwrap().join("base");
    put(
        &base.join("s.png"),
        "png",
        vec![
            Op::Convert("jpg"),
            Op::Convert("pdf"),
            Op::Tool(Tool::CompressImage),
            Op::Tool(Tool::Resize),
            Op::Tool(Tool::Zip),
        ],
    );
    put(
        &base.join("s.pdf"),
        "pdf",
        vec![
            Op::Convert("png"),
            Op::Convert("txt"),
            Op::Tool(Tool::SplitPdf),
            Op::Tool(Tool::CompressPdf),
            Op::Tool(Tool::ExtractPages),
            Op::Tool(Tool::RotatePdf),
        ],
    );
    put(
        &base.join("s.mp4"),
        "mp4",
        vec![Op::Convert("mp3"), Op::Tool(Tool::Thumbnail), Op::Tool(Tool::SplitMedia), Op::Tool(Tool::Trim)],
    );
    put(&base.join("s.md"), "md", vec![Op::Convert("html"), Op::Convert("docx")]);
    put(&base.join("s.srt"), "srt", vec![Op::Convert("vtt"), Op::Convert("txt")]);
    put(&base.join("s.zip"), "zip", vec![Op::Tool(Tool::Extract), Op::Convert("tar.gz")]);
    put(&base.join("s.tar.gz"), "tar.gz", vec![Op::Tool(Tool::Extract), Op::Convert("zip")]);
    if has_program("soffice") {
        put(&base.join("s.docx"), "docx", vec![Op::Convert("pdf")]);
    }
    out
}

fn make_base(root: &Path) {
    let base = root.join("base");
    std::fs::create_dir_all(&base).unwrap();
    sh(&base, im(), &["-size", "64x48", "gradient:orange-white", "s.png"]);
    sh(&base, im(), &["s.png", "s.png", "s.pdf"]);
    ff(
        &base,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=128x96:rate=25:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=duration=1",
            "-shortest",
            "-pix_fmt",
            "yuv420p",
            "s.mp4",
        ],
    );
    std::fs::write(base.join("s.md"), "# Title\n\nText.\n").unwrap();
    std::fs::write(base.join("s.srt"), "1\n00:00:01,000 --> 00:00:02,000\nHello\n").unwrap();
    sh(&base, "zip", &["-q", "s.zip", "s.md"]);
    sh(&base, "tar", &["-czf", "s.tar.gz", "s.md"]);
    sh(&base, "pandoc", &["s.md", "-o", "s.docx"]);
}

/// Runs every hostile sample of `stems` in its own folder; returns the failures.
fn hostile_failures(stems: &[&str], upper: bool) -> Vec<String> {
    let root = tempfile::tempdir().unwrap();
    make_base(root.path());
    let mut failures = Vec::new();
    for (i, stem) in stems.iter().enumerate() {
        let dir = root.path().join(format!("case{i}"));
        std::fs::create_dir(&dir).unwrap();
        for (file, ops) in hostile_samples(&dir, stem, upper) {
            for op in ops {
                let files = vec![file.clone()];
                let name = format!("{:?} → {}", file.file_name().unwrap(), op.label());
                if let Err(why) = runnable(op, &files) {
                    println!("skip {name}: {why}");
                    continue;
                }
                match run_checked(op, &files, &dir, 120) {
                    Ok(_) => println!("ok   {name}"),
                    Err(e) => failures.push(format!("{name}: {e}")),
                }
            }
        }
    }
    failures
}

#[test]
#[ignore = "slow: runs the converters"]
fn qa_hostile_names_are_taken_literally() {
    let failures = hostile_failures(HOSTILE, false);
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
#[ignore = "slow: runs the converters"]
fn qa_upper_case_extensions_work() {
    let failures = hostile_failures(&["UPPER"], true);
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
#[ignore = "slow: runs the converters"]
fn qa_percent_names_are_taken_literally() {
    let failures = hostile_failures(PERCENT, false);
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// A stem of 250 bytes: outputs like `… (compressed).png` exceed NAME_MAX (255). That must be a
/// clean error with nothing left behind, never a panic or a hang.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_very_long_names_fail_cleanly() {
    let root = tempfile::tempdir().unwrap();
    make_base(root.path());
    let dir = root.path().join("long");
    std::fs::create_dir(&dir).unwrap();
    let stem = "é".repeat(118) + "abcd"; // 240 bytes: inputs fit, `… (compressed).pdf` does not
    let mut bad = Vec::new();
    for (file, ops) in hostile_samples(&dir, &stem, false) {
        for op in ops {
            let files = vec![file.clone()];
            if runnable(op, &files).is_err() {
                continue;
            }
            // Either it works (the output name fits) or it fails cleanly.
            match run_any(op, &files, &dir, 120) {
                Ok(Outcome::Failed(e)) => println!("ok   long → {}: {e}", op.label()),
                Ok(Outcome::Worked(_)) => println!("ok   long → {}", op.label()),
                Err(e) => bad.push(format!("long.{} → {}: {e}", extension(&file), op.label())),
            }
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

#[test]
#[ignore = "slow: runs the converters"]
fn qa_dotfiles_and_files_without_extension() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join(".bashrc"), "x\n").unwrap();
    std::fs::write(d.join("README"), "x\n").unwrap();
    for name in [".bashrc", "README"] {
        let files = vec![d.join(name)];
        assert!(options(Mode::Convert, &files).is_empty());
        assert_eq!(options(Mode::Tools, &files), vec![Op::Tool(Tool::Zip)]);
        let outs = run_checked(Op::Tool(Tool::Zip), &files, d, 30).unwrap();
        assert_eq!(outs, vec![d.join(format!("{name}.zip"))]);
    }
}

/// `name.gz` (one compressed file) and `name.tar.gz` (an archive) both extract.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_gz_and_tar_gz_extract_differently() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("notes.txt"), "hi\n").unwrap();
    sh(d, "tar", &["-czf", "pack.tar.gz", "notes.txt"]);
    sh(d, "gzip", &["-k", "notes.txt"]);
    let outs = run_checked(Op::Tool(Tool::Extract), &[d.join("pack.tar.gz")], d, 30).unwrap();
    assert!(outs[0].join("notes.txt").is_file());
    let outs = run_checked(Op::Tool(Tool::Extract), &[d.join("notes.txt.gz")], d, 30).unwrap();
    assert_eq!(std::fs::read_to_string(outs[0].join("notes.txt")).unwrap(), "hi\n");
}

/// Relative paths (a command line, or a future CLI) must work like absolute ones.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_relative_paths_work() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // Deep enough that `..`s meant for the test's working folder cannot land back in place.
    let sub = d.join("sub/deeper/deepest/still");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("notes.md"), "# x\n").unwrap();
    sh(&sub, "zip", &["-q", "bundle.zip", "notes.md"]);
    let mut failures = Vec::new();
    for (file, op) in [
        ("sub/deeper/deepest/still/notes.md", Op::Tool(Tool::Zip)),
        ("sub/deeper/deepest/still/bundle.zip", Op::Convert("tar")),
        ("sub/deeper/deepest/still/notes.md", Op::Convert("html")),
    ] {
        // Relative to the test process' working folder.
        let rel = pathdiff(&d.join(file));
        match run(op, std::slice::from_ref(&rel), None, None) {
            Ok(outs) => {
                if let Err(e) = outs.iter().try_for_each(|o| looks_valid(o)) {
                    failures.push(format!("{file} → {}: {e}", op.label()));
                }
            }
            Err(e) => failures.push(format!("{file} → {}: {e:#}", op.label())),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// `to` as a path relative to the current working folder (`../../tmp/…`).
fn pathdiff(to: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let ups = cwd.components().count() - 1;
    let mut p = PathBuf::new();
    for _ in 0..ups {
        p.push("..");
    }
    p.join(to.strip_prefix("/").unwrap())
}

/// The file vanished between the drop and the run.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_a_deleted_input_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let gone = [d.join("gone.png"), d.join("gone.pdf"), d.join("gone.mp4"), d.join("gone.docx"), d.join("gone.zip")];
    let mut bad = Vec::new();
    for f in &gone {
        for op in every_op(std::slice::from_ref(f)) {
            if runnable(op, std::slice::from_ref(f)).is_err() || matches!(op, Op::Tool(Tool::Zip)) {
                continue;
            }
            match run_failing(op, std::slice::from_ref(f), d, 120) {
                Ok(msg) => println!("ok   {} → {}: {msg}", f.display(), op.label()),
                Err(e) => bad.push(format!("{} → {}: {e}", f.display(), op.label())),
            }
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

/// Results asked to go to a folder on another file system (`/dev/shm`): the LibreOffice path
/// moves its result out of the scratch folder, which must fall back to a copy.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_results_on_another_file_system() {
    isolate_libreoffice();
    let shm = Path::new("/dev/shm");
    if !shm.is_dir() {
        return;
    }
    let dest = tempfile::tempdir_in(shm).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("t.csv"), "a,b\n1,2\n").unwrap();
    std::fs::write(d.join("n.md"), "# x\n").unwrap();
    let mut cases = vec![(d.join("n.md"), Op::Convert("html"))];
    if has_program("soffice") {
        cases.push((d.join("t.csv"), Op::Convert("xlsx")));
        cases.push((d.join("n.md"), Op::Convert("pdf")));
    }
    for (file, op) in cases {
        let outs = run(op, std::slice::from_ref(&file), None, Some(dest.path())).unwrap();
        assert!(outs.iter().all(|o| o.starts_with(dest.path())), "{outs:?}");
        outs.iter().try_for_each(|o| looks_valid(o)).unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// Broken inputs
// ---------------------------------------------------------------------------------------------

/// Broken samples: (file name, how to make it). Every op on them must fail cleanly, in time.
fn broken_samples(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    // Zero bytes, every kind.
    for ext in [
        "png", "jpg", "gif", "svg", "mp4", "mp3", "wav", "pdf", "zip", "tar.gz", "7z", "rar", "docx", "xlsx", "pptx",
        "epub",
    ] {
        let f = dir.join(format!("zero_{}.{ext}", ext.replace('.', "_")));
        std::fs::write(&f, b"").unwrap();
        files.push(f);
    }
    // Truncated real files.
    let good = dir.join("good");
    std::fs::create_dir(&good).unwrap();
    sh(&good, im(), &["-size", "640x480", "plasma:", "g.png"]);
    sh(&good, im(), &["g.png", "g.png", "g.pdf"]);
    ff(&good, &["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=3", "-pix_fmt", "yuv420p", "g.mp4"]);
    sh(&good, "zip", &["-q", "g.zip", "g.png", "g.pdf"]);
    for (src, name) in [("g.png", "trunc.png"), ("g.pdf", "trunc.pdf"), ("g.mp4", "trunc.mp4"), ("g.zip", "trunc.zip")]
    {
        let bytes = std::fs::read(good.join(src)).unwrap();
        let f = dir.join(name);
        std::fs::write(&f, &bytes[..bytes.len() / 3]).unwrap();
        files.push(f);
    }
    // Wrong content for the extension.
    let f = dir.join("text.pdf");
    std::fs::write(&f, "this is not a PDF\n").unwrap();
    files.push(f);
    let f = dir.join("text.mp4");
    std::fs::write(&f, "this is not a video\n").unwrap();
    files.push(f);
    let f = dir.join("text.zip");
    std::fs::write(&f, "this is not a zip\n").unwrap();
    files.push(f);
    std::fs::remove_dir_all(&good).unwrap();
    files
}

/// Some ops cannot fail on garbage (Zip packs anything; subtitles to text read any text).
fn always_works(op: Op) -> bool {
    matches!(op, Op::Tool(Tool::Zip))
}

#[test]
#[ignore = "slow: runs the converters"]
fn qa_broken_inputs_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let files = broken_samples(d);
    let mut bad = Vec::new();
    let mut lenient: HashMap<String, Vec<String>> = HashMap::new();
    for f in &files {
        let one = std::slice::from_ref(f);
        for op in every_op(one) {
            if always_works(op) || runnable(op, one).is_err() {
                continue;
            }
            let name = format!("{} → {}", f.file_name().unwrap().to_string_lossy(), op.label());
            match run_any(op, one, d, 120) {
                Ok(Outcome::Failed(msg)) => println!("ok   {name}: {msg}"),
                // A converter that accepts an empty or cut file and writes a valid result is fine.
                Ok(Outcome::Worked(outs)) => lenient.entry(name).or_default().push(format!("{outs:?}")),
                Err(e) => bad.push(format!("{name}: {e}")),
            }
        }
    }
    for (name, how) in &lenient {
        println!("lenient {name}: {how:?}");
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

/// A PNG named `.jpg`: ImageMagick's decoder is pinned to the extension (a security choice, so a
/// file's content cannot pick another parser), so this fails, and must fail cleanly.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_wrong_extension_with_real_content_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    sh(d, im(), &["-size", "16x16", "xc:red", "png:really-png.jpg"]);
    match run_any(Op::Convert("webp"), &[d.join("really-png.jpg")], d, 60) {
        Ok(Outcome::Failed(msg)) => assert!(msg.contains("JPEG"), "{msg}"),
        Ok(Outcome::Worked(outs)) => assert_eq!(image_size(&outs[0]), Some((16, 16))),
        Err(e) => panic!("{e}"),
    }
}

/// Regression: `*` and `?` in an image's name. With the decoder pinned (`png:/a/b*.png`),
/// ImageMagick globs, so siblings matching the pattern were read too and written as `name-0.png`,
/// `name-1.png`…, reported as `name.png`. Such inputs are now read through a safely named link.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_glob_characters_in_image_names() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    sh(d, im(), &["-size", "64x48", "xc:red", "star*?.png"]);
    sh(d, im(), &["-size", "32x32", "xc:blue", "star*? (old).png"]);
    sh(d, im(), &["-size", "32x32", "xc:blue", "star*?x.png"]);
    let file = d.join("star*?.png");
    expect_all_work(vec![(file.clone(), Op::Tool(Tool::Resize)), (file, Op::Convert("jpg"))], d);
}

fn encrypted_samples(dir: &Path) -> Vec<PathBuf> {
    std::fs::write(dir.join("secret.txt"), "secret\n").unwrap();
    let mut files = Vec::new();
    sh(dir, "zip", &["-q", "-P", "pw", "locked.zip", "secret.txt"]);
    files.push(dir.join("locked.zip"));
    if has_program("7z") {
        sh(dir, "7z", &["a", "-bd", "-ppw", "locked.7z", "secret.txt"]);
        sh(dir, "7z", &["a", "-bd", "-ppw", "-mhe=on", "locked-names.7z", "secret.txt"]);
        files.push(dir.join("locked.7z"));
        files.push(dir.join("locked-names.7z"));
    }
    if has_program("rar") {
        sh(dir, "rar", &["a", "-idq", "-ppw", "locked.rar", "secret.txt"]);
        files.push(dir.join("locked.rar"));
    }
    if has_program("qpdf") {
        sh(dir, im(), &["-size", "32x32", "xc:red", "plain.pdf"]);
        sh(dir, "qpdf", &["--encrypt", "user", "owner", "256", "--", "plain.pdf", "locked.pdf"]);
        std::fs::remove_file(dir.join("plain.pdf")).unwrap();
        files.push(dir.join("locked.pdf"));
    }
    std::fs::remove_file(dir.join("secret.txt")).unwrap();
    files
}

/// Password-protected inputs fail with an error and never wait for a password. Programs are
/// started without a terminal (`setsid`), like the GUI is.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_password_protected_inputs_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut bad = Vec::new();
    for f in encrypted_samples(d) {
        let one = std::slice::from_ref(&f);
        for op in every_op(one) {
            if always_works(op) || runnable(op, one).is_err() {
                continue;
            }
            let name = format!("{} → {}", f.file_name().unwrap().to_string_lossy(), op.label());
            match run_failing(op, one, d, 60) {
                Ok(msg) if msg.to_lowercase().contains("password") || msg.to_lowercase().contains("encrypt") => {
                    println!("ok   {name}: {msg}")
                }
                Ok(msg) => bad.push(format!("{name}: fails without naming the password: {msg}")),
                Err(e) => bad.push(format!("{name}: {e}")),
            }
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

// ---------------------------------------------------------------------------------------------
// Concurrency
// ---------------------------------------------------------------------------------------------

/// Runs `n` copies of `op` on `file` at once; returns every output reported.
fn race(op: Op, file: &Path, n: usize) -> Vec<PathBuf> {
    isolate_libreoffice();
    let barrier = std::sync::Barrier::new(n);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..n)
            .map(|_| {
                s.spawn(|| {
                    barrier.wait();
                    run(op, &[file.to_path_buf()], params_for(op).as_ref(), None).map_err(|e| format!("{e:#}"))
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap().unwrap()).collect()
    })
}

/// Eight conversions of one file to one format at the same time must give eight files.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_concurrent_runs_never_share_an_output() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    sh(d, im(), &["-size", "400x300", "plasma:", "photo.png"]);
    let outs = race(Op::Convert("jpg"), &d.join("photo.png"), 8);
    let distinct: HashSet<&PathBuf> = outs.iter().collect();
    let on_disk = std::fs::read_dir(d).unwrap().count() - 1;
    assert_eq!((distinct.len(), on_disk), (8, 8), "outputs reported: {outs:?}");
}

/// Two LibreOffice conversions at once (different files) both succeed: the LibreOffice lock queues them.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_two_libreoffice_jobs_at_once() {
    isolate_libreoffice();
    if !has_program("soffice") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("a.csv"), "a,b\n1,2\n").unwrap();
    std::fs::write(d.join("b.tsv"), "a\tb\n1\t2\n").unwrap();
    let (a, b) = (d.join("a.csv"), d.join("b.tsv"));
    let (ra, rb) = std::thread::scope(|s| {
        let ha = s.spawn(|| run(Op::Convert("xlsx"), std::slice::from_ref(&a), None, None));
        let hb = s.spawn(|| run(Op::Convert("ods"), std::slice::from_ref(&b), None, None));
        (ha.join().unwrap(), hb.join().unwrap())
    });
    for outs in [ra.unwrap(), rb.unwrap()] {
        outs.iter().try_for_each(|o| looks_valid(o)).unwrap();
    }
}

/// Different files in one folder, converted at once, each get their own output.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_concurrent_runs_on_different_files() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let files: Vec<PathBuf> = (0..8)
        .map(|i| {
            let f = d.join(format!("p{i}.png"));
            sh(d, im(), &["-size", "64x48", "plasma:", &f.to_string_lossy()]);
            f
        })
        .collect();
    let outs: Vec<PathBuf> = std::thread::scope(|s| {
        let hs: Vec<_> = files
            .iter()
            .map(|f| s.spawn(move || run(Op::Convert("jpg"), std::slice::from_ref(f), None, None)))
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap().unwrap()).collect()
    });
    let distinct: HashSet<&PathBuf> = outs.iter().collect();
    assert_eq!(distinct.len(), 8);
    outs.iter().try_for_each(|o| looks_valid(o)).unwrap();
}

/// `facts` reads lengths, sizes and page counts, and gives nothing (not a panic) for junk.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_facts_on_real_and_broken_files() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    sh(d, im(), &["-size", "64x48", "xc:red", "a.png"]);
    sh(d, im(), &["a.png", "a.png", "a.pdf"]);
    std::fs::write(d.join("junk.pdf"), "junk").unwrap();
    std::fs::write(d.join("junk.mp4"), "junk").unwrap();
    assert_eq!(backend::facts(Tool::Resize, &[d.join("a.png")]).size, Some((64, 48)));
    assert_eq!(backend::facts(Tool::ExtractPages, &[d.join("a.pdf")]).pages, Some(2));
    assert_eq!(backend::facts(Tool::ExtractPages, &[d.join("a.pdf"), d.join("junk.pdf")]).pages, None);
    assert_eq!(backend::facts(Tool::Trim, &[d.join("junk.mp4")]).duration, None);
    assert!(media_duration(OsStr::new("/nonexistent-qa/x.mp4").as_ref()).is_none());
}

// ---------------------------------------------------------------------------------------------
// Open bugs found on real files
// ---------------------------------------------------------------------------------------------

/// Makes `name` in `dir` with ffmpeg from a 1-second test pattern (and tone) and these codecs.
fn ff_sample(dir: &Path, name: &str, codecs: &[&str]) -> PathBuf {
    let mut args = vec!["-f", "lavfi", "-i", "testsrc=size=128x96:rate=25:duration=1", "-f", "lavfi", "-i"];
    args.extend_from_slice(&["sine=duration=1", "-shortest", "-pix_fmt", "yuv420p"]);
    args.extend_from_slice(codecs);
    args.push(name);
    ff(dir, &args);
    dir.join(name)
}

fn expect_all_work(cases: Vec<(PathBuf, Op)>, dir: &Path) {
    let failures: Vec<String> = cases
        .into_iter()
        .filter_map(|(f, op)| {
            run_checked(op, std::slice::from_ref(&f), dir, 120)
                .err()
                .map(|e| format!("{} → {}: {e}", f.file_name().unwrap().to_string_lossy(), op.label()))
        })
        .collect();
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Regression: Resize… at its default 50% on an animated GIF or a multi-page TIFF. ImageMagick
/// read the `%` in `name (50%).gif` as a frame pattern and wrote `name (50%)-0.gif`. It now writes
/// a safe scratch name that is moved into place, and the animation stays one animated file.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_resize_on_animations_and_multi_page_images() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    sh(d, im(), &["-size", "64x48", "xc:red", "xc:blue", "-set", "delay", "20", "anim.gif"]);
    sh(d, im(), &["-size", "64x48", "xc:red", "xc:blue", "pages.tiff"]);
    expect_all_work(
        vec![(d.join("anim.gif"), Op::Tool(Tool::Resize)), (d.join("pages.tiff"), Op::Tool(Tool::Resize))],
        d,
    );
    for out in ["anim (50%).gif", "pages (50%).tiff"] {
        let file = std::fs::File::open(d.join(out)).unwrap();
        let frames = Command::new(im()).args(["-", "-format", "%w\\n", "info:"]).stdin(file).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&frames.stdout).lines().collect::<Vec<_>>(), ["32", "32"], "{out}");
    }
}

/// Normalize: `loudnorm` outputs 192 kHz, which the WMA encoder refused; the source's rate is
/// now asked for with `-ar`.
#[test]
#[ignore = "slow: runs the converters"]
fn normalize_wma() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    ff(d, &["-f", "lavfi", "-i", "sine=duration=1", "-c:a", "wmav2", "a.wma"]);
    expect_all_work(vec![(d.join("a.wma"), Op::Tool(Tool::NormalizeAudio))], d);
}

/// Normalize keeps the sample rate of every audio format it is offered on (Opus is always
/// 48 kHz), at 44.1 and 22.05 kHz.
#[test]
#[ignore = "slow: runs the converters"]
fn normalize_keeps_the_sample_rate() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut failures = Vec::new();
    for rate in ["44100", "22050"] {
        for ext in ["mp3", "m4a", "wav", "flac", "ogg", "opus", "aiff", "wma", "aac"] {
            let name = format!("a{rate}.{ext}");
            ff(d, &["-f", "lavfi", "-i", &format!("sine=duration=1:sample_rate={rate}"), &name]);
            let file = d.join(&name);
            let want = backend::sample_rate(&file).unwrap();
            match run(Op::Tool(Tool::NormalizeAudio), std::slice::from_ref(&file), None, None) {
                Ok(outs) => match backend::sample_rate(&outs[0]) {
                    Some(got) if got == want => println!("ok   {name}: {got} Hz"),
                    got => failures.push(format!("{name}: {got:?} Hz, wanted {want}")),
                },
                Err(e) => failures.push(format!("{name}: {e:#}")),
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Speed…: `-fps_mode passthrough` handed AVI, WMV, MPG and FLV encoders duplicate timestamps
/// ("Invalid pts"); they now get variable frame rate.
#[test]
#[ignore = "slow: runs the converters"]
fn speed_on_avi_wmv_mpg_flv() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let cases = vec![
        (ff_sample(d, "a.avi", &["-c:v", "mpeg4", "-c:a", "libmp3lame"]), Op::Tool(Tool::Speed)),
        (ff_sample(d, "a.wmv", &["-c:v", "wmv2", "-c:a", "wmav2"]), Op::Tool(Tool::Speed)),
        (ff_sample(d, "a.mpg", &["-c:v", "mpeg2video", "-c:a", "mp2"]), Op::Tool(Tool::Speed)),
        (ff_sample(d, "a.flv", &["-c:v", "flv1", "-c:a", "libmp3lame", "-ar", "44100"]), Op::Tool(Tool::Speed)),
    ];
    expect_all_work(cases, d);
}

/// The video samples of every container the catalog accepts, one second long.
fn video_samples(d: &Path) -> Vec<PathBuf> {
    [
        ("v.mp4", &["-c:v", "libx264", "-c:a", "aac"][..]),
        ("v.m4v", &["-c:v", "libx264", "-c:a", "aac"]),
        ("v.mov", &["-c:v", "libx264", "-c:a", "aac"]),
        ("v.mkv", &["-c:v", "libx264", "-c:a", "aac"]),
        ("v.webm", &["-c:v", "libvpx-vp9", "-c:a", "libopus"]),
        ("v.avi", &["-c:v", "mpeg4", "-c:a", "libmp3lame"]),
        ("v.wmv", &["-c:v", "wmv2", "-c:a", "wmav2"]),
        ("v.mpg", &["-c:v", "mpeg2video", "-c:a", "mp2"]),
        ("v.mpeg", &["-c:v", "mpeg1video", "-c:a", "mp2"]),
        ("v.flv", &["-c:v", "flv1", "-c:a", "libmp3lame", "-ar", "44100"]),
    ]
    .into_iter()
    .map(|(name, codecs)| ff_sample(d, name, codecs))
    .collect()
}

/// Speed… (extremes), Trim…, Split… and Fade… on every audio and video container: each output
/// lands within 0.15 s of the length asked, plus the padding the sample's encoder added.
#[test]
#[ignore = "slow: runs the converters"]
fn media_tools_keep_lengths_in_every_container() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut files = video_samples(d);
    for ext in ["mp3", "m4a", "wav", "flac", "ogg", "opus", "aiff", "wma", "aac"] {
        let name = format!("a.{ext}");
        ff(d, &["-f", "lavfi", "-i", "sine=duration=1", &name]);
        files.push(d.join(name));
    }
    let mut failures = Vec::new();
    for file in files {
        // Every sample holds 1 s; what a container reports beyond that is encoder padding (MP3,
        // WMA), which a speed change stretches with the rest.
        let source = media_duration(&file).unwrap();
        let pad = (source - 1.0).abs();
        let cases = [
            (Tool::Speed, Params::Speed(0.25), 4.0, pad * 4.0),
            (Tool::Speed, Params::Speed(4.0), 0.25, pad),
            (Tool::Trim, Params::Trim { start: 0.25, end: 0.75 }, 0.5, pad),
            (Tool::SplitMedia, Params::Split(SplitBy::Parts(2)), source / 2.0, pad),
            (Tool::FadeAudio, Params::Fade { fade_in: 0.2, fade_out: 0.3 }, source, 0.0),
        ];
        for (tool, params, want, pad) in cases {
            let name = format!("{} {params:?}", file.file_name().unwrap().to_string_lossy());
            let outs = match run(Op::Tool(tool), std::slice::from_ref(&file), Some(&params), None) {
                Ok(outs) => outs,
                Err(e) => {
                    failures.push(format!("{name}: {e:#}"));
                    continue;
                }
            };
            // A split gives a folder; its first part is checked.
            let out = if outs[0].is_dir() {
                let mut parts: Vec<PathBuf> = std::fs::read_dir(&outs[0]).unwrap().map(|e| e.unwrap().path()).collect();
                parts.sort();
                parts[0].clone()
            } else {
                outs[0].clone()
            };
            match media_duration(&out) {
                Some(got) if (got - want).abs() <= 0.15 + pad => println!("ok   {name}: {got:.3} s"),
                got => failures.push(format!("{name}: {got:?} s, wanted {want:.3}")),
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Speed… at 0.5× and 2× in every video container lands within 0.15 s of the asked length.
#[test]
#[ignore = "slow: runs the converters"]
fn speed_keeps_the_asked_length_in_every_video_container() {
    let dir = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    for file in video_samples(dir.path()) {
        let source = media_duration(&file).unwrap();
        for f in [0.5, 2.0] {
            let name = format!("{} at {f}x", file.file_name().unwrap().to_string_lossy());
            match run(Op::Tool(Tool::Speed), std::slice::from_ref(&file), Some(&Params::Speed(f)), None) {
                Ok(outs) => {
                    let got = media_duration(&outs[0]);
                    match got {
                        Some(g) if (g - source / f).abs() <= 0.15 => println!("ok   {name}: {g:.3} s"),
                        _ => failures.push(format!("{name}: {got:?} s, wanted {:.3}", source / f)),
                    }
                    std::fs::remove_file(&outs[0]).unwrap();
                }
                Err(e) => failures.push(format!("{name}: {e:#}")),
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// MPG → MKV stream-copied packets without timestamps, which Matroska cannot store; ffmpeg now
/// generates them (`-fflags +genpts`).
#[test]
#[ignore = "slow: runs the converters"]
fn mpg_to_mkv() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mpg = ff_sample(d, "a.mpg", &["-c:v", "mpeg2video", "-c:a", "mp2"]);
    expect_all_work(vec![(mpg, Op::Convert("mkv"))], d);
}

/// Every video container → MKV, plus the awkward ones: an MP4 with its own subtitle format, a
/// QuickTime file with a timecode track and a DVD program stream with navigation packets. Each
/// copy must decode without an error and keep its length.
#[test]
#[ignore = "slow: runs the converters"]
fn every_video_container_to_mkv() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut files = video_samples(d);
    std::fs::write(d.join("t.srt"), "1\n00:00:00,200 --> 00:00:00,800\nHi\n").unwrap();
    ff(d, &["-i", "v.mp4", "-i", "t.srt", "-map", "0", "-map", "1", "-c", "copy", "-c:s", "mov_text", "subs.mp4"]);
    ff(d, &["-i", "v.mov", "-timecode", "01:00:00:00", "-c", "copy", "timecode.mov"]);
    files.push(d.join("subs.mp4"));
    files.push(d.join("timecode.mov"));
    files.push(ff_sample(d, "dvd.mpg", &["-target", "pal-dvd", "-s", "128x96"]));
    let mut failures = Vec::new();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let outs = match run(Op::Convert("mkv"), std::slice::from_ref(&file), None, None) {
            Ok(outs) => outs,
            Err(e) => {
                failures.push(format!("{name}: {e:#}"));
                continue;
            }
        };
        let decode = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(&outs[0])
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        let (want, got) = (media_duration(&file).unwrap(), media_duration(&outs[0]));
        if !decode.status.success() || !decode.stderr.is_empty() {
            failures.push(format!("{name}: does not decode: {}", String::from_utf8_lossy(&decode.stderr)));
        } else if !got.is_some_and(|g| (g - want).abs() <= 0.1) {
            failures.push(format!("{name}: {got:?} s, wanted {want:.3}"));
        } else {
            println!("ok   {name} → mkv: {:.3} s", got.unwrap());
        }
        std::fs::remove_file(&outs[0]).unwrap();
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

// ---------------------------------------------------------------------------------------------
// The command line (the built binary)
// ---------------------------------------------------------------------------------------------

/// The `susen` binary next to this test binary, built once if needed.
fn binary() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let bin = exe.parent().unwrap().parent().unwrap().join("susen");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let ok = Command::new(cargo)
            .args(["build", "--quiet", "--bin", "susen"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok && bin.is_file(), "could not build {}", bin.display());
        bin
    })
    .clone()
}

struct CliResult {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the CLI with its own config and cache folders under `home`, without a terminal.
fn cli(home: &Path, args: &[&OsStr], secs: u64) -> Option<CliResult> {
    let mut cmd = Command::new(binary());
    cmd.args(args)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().ok()?;
    let id = child.id();
    let out = within(secs, move || child.wait_with_output());
    let Some(out) = out else {
        let _ = Command::new("kill").arg(id.to_string()).status();
        return None;
    };
    let out = out.ok()?;
    Some(CliResult {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// `--convert` / `--tool` on every hostile name: exit 0, each printed path exists, nothing else
/// appears.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_cli_takes_hostile_names_literally() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    make_base(root.path());
    let base = root.path().join("base");
    let mut bad = Vec::new();
    // A name that starts with `--` must come after `--`.
    let mut stems: Vec<&str> = HOSTILE.iter().copied().filter(|s| !s.contains('\n')).collect();
    stems.push("--double-dash");
    for (i, stem) in stems.iter().enumerate() {
        let dir = root.path().join(format!("c{i}"));
        std::fs::create_dir(&dir).unwrap();
        for (src, ext, flag, what) in [
            ("s.png", "png", "--convert", "jpg"),
            ("s.pdf", "pdf", "--tool", "split"),
            ("s.mp4", "mp4", "--tool", "thumbnail"),
            ("s.zip", "zip", "--tool", "extract"),
            ("s.png", "png", "--tool", "zip"),
        ] {
            let file = dir.join(format!("{stem}.{ext}"));
            if !file.exists() {
                std::fs::copy(base.join(src), &file).unwrap();
            }
            let before = listing(&dir);
            let args = [OsStr::new(flag), OsStr::new(what), OsStr::new("--"), file.as_os_str()];
            let name = format!("{stem:?}.{ext} {flag} {what}");
            let Some(r) = cli(home.path(), &args, 120) else {
                bad.push(format!("{name}: timed out"));
                continue;
            };
            let outs: Vec<PathBuf> = r.stdout.lines().map(PathBuf::from).collect();
            if r.code != Some(0) || outs.is_empty() {
                bad.push(format!("{name}: exit {:?}: {}", r.code, r.stderr.trim()));
                continue;
            }
            if let Err(e) = outs.iter().try_for_each(|o| looks_valid(o)) {
                bad.push(format!("{name}: {e}"));
            }
            let stray: Vec<_> = listing(&dir)
                .difference(&before)
                .filter(|p| !outs.iter().any(|o| dir.join(p).starts_with(o)))
                .cloned()
                .collect();
            if !stray.is_empty() {
                bad.push(format!("{name}: left {stray:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

/// Broken, missing and unsuitable inputs: a non-zero exit, a message on stderr, nothing left behind.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_cli_fails_cleanly() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let files = broken_samples(d);
    let mut bad = Vec::new();
    let mut cases: Vec<(Vec<std::ffi::OsString>, i32)> = Vec::new();
    for f in &files {
        let target = match kind_of(f) {
            Kind::Image => "webp",
            Kind::Pdf | Kind::Slides => "png",
            Kind::Video => "gif",
            Kind::Audio => "flac",
            Kind::Archive => "tar",
            Kind::Document => "html",
            Kind::Spreadsheet => "csv",
            _ => continue,
        };
        cases.push((vec!["--convert".into(), target.into(), f.clone().into()], 1));
    }
    cases.push((vec!["--convert".into(), "png".into(), d.join("missing.jpg").into()], 1));
    cases.push((vec!["--convert".into(), "mp3".into(), d.join("text.pdf").into()], 1));
    cases.push((vec!["--tool".into(), "trim".into(), "--start".into(), "5".into(), d.join("trunc.mp4").into()], 1));
    cases.push((vec!["--convert".into()], 2));
    for (args, want) in cases {
        let before = listing(d);
        let a: Vec<&OsStr> = args.iter().map(|s| s.as_os_str()).collect();
        let name = format!("{args:?}");
        let Some(r) = cli(home.path(), &a, 120) else {
            bad.push(format!("{name}: timed out"));
            continue;
        };
        let left: Vec<_> = listing(d).difference(&before).cloned().collect();
        if r.code == Some(0) {
            // Lenient converters may accept junk; then the result must at least read back.
            let outs: Vec<PathBuf> = r.stdout.lines().map(PathBuf::from).collect();
            match outs.iter().try_for_each(|o| looks_valid(o)) {
                Ok(()) => println!("lenient {name}"),
                Err(e) => bad.push(format!("{name}: exit 0 with a bad result: {e}")),
            }
            for o in outs {
                let _ = if o.is_dir() { std::fs::remove_dir_all(o) } else { std::fs::remove_file(o) };
            }
            continue;
        }
        if r.code != Some(want) {
            bad.push(format!("{name}: exit {:?}, wanted {want}: {}", r.code, r.stderr.trim()));
        }
        if !r.stderr.contains("susen") {
            bad.push(format!("{name}: no message: {:?}", r.stderr));
        }
        if !left.is_empty() {
            bad.push(format!("{name}: left {left:?}"));
        }
        if r.stderr.contains("panicked") {
            bad.push(format!("{name}: panicked: {}", r.stderr));
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

/// Two Susen processes (two file-manager menu clicks, or the CLI next to the window) convert
/// with LibreOffice at once. Both share `~/.cache/susen/libreoffice`, and without a lock across
/// processes the second LibreOffice hands its job to the first and exits 1 without a message;
/// the `flock` on `libreoffice.lock` queues them.
#[test]
#[ignore = "slow: runs the converters"]
fn qa_two_processes_convert_with_libreoffice_at_once() {
    if !has_program("soffice") {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let files: Vec<PathBuf> = (0..4)
        .map(|i| {
            let f = d.join(format!("t{i}.csv"));
            std::fs::write(&f, format!("a,b\n{i},2\n")).unwrap();
            f
        })
        .collect();
    // Warm the profile first, so this is not about the first start.
    let warm = cli(home.path(), &[OsStr::new("--convert"), OsStr::new("ods"), files[0].as_os_str()], 180).unwrap();
    assert_eq!(warm.code, Some(0), "{}", warm.stderr);
    let results: Vec<Option<CliResult>> = std::thread::scope(|s| {
        let hs: Vec<_> = files
            .iter()
            .map(|f| {
                let home = home.path();
                s.spawn(move || cli(home, &[OsStr::new("--convert"), OsStr::new("xlsx"), f.as_os_str()], 180))
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let bad: Vec<String> = results
        .iter()
        .zip(&files)
        .filter_map(|(r, f)| match r {
            None => Some(format!("{}: timed out", f.display())),
            Some(r) if r.code != Some(0) => Some(format!("{}: exit {:?}: {}", f.display(), r.code, r.stderr.trim())),
            Some(_) => None,
        })
        .collect();
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}
