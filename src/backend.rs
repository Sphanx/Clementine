//! Turns an [`Op`] on some files into external commands, and runs them.
//!
//! Nothing is converted in-process except subtitles to text: the heavy lifting
//! is ffmpeg, ImageMagick, pandoc, LibreOffice and poppler, all local. An op whose
//! programs are missing is still listed, but greyed out (see [`missing_programs`]).
//!
//! Outputs land next to the input, or in the folder chosen in Settings, under a name that is
//! never taken, so an input is never overwritten.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};

use crate::catalog::{Kind, Op, Tool, canonical, extension, kind_of};
use crate::params::{self, Crop, Facts, Params, Resize, atempo_chain, speed_name, split_points, trim_number};

/// One external program call.
#[derive(Debug, Clone)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Where stdout goes; discarded when `None`.
    pub stdout: Option<PathBuf>,
}

impl Cmd {
    fn new(program: &str) -> Self {
        Cmd { program: program.into(), args: Vec::new(), cwd: None, stdout: None }
    }

    fn arg(mut self, a: impl Into<OsString>) -> Self {
        self.args.push(a.into());
        self
    }

    fn args<I: IntoIterator<Item = S>, S: Into<OsString>>(mut self, a: I) -> Self {
        self.args.extend(a.into_iter().map(Into::into));
        self
    }
}

#[derive(Debug, Clone)]
pub enum Step {
    Run(Cmd),
    MkDir(PathBuf),
    Move {
        from: PathBuf,
        to: PathBuf,
    },
    SubtitleToText {
        from: PathBuf,
        to: PathBuf,
    },
    /// Refuses an archive with an entry named outside the folder it unpacks into.
    CheckArchive(PathBuf),
    /// Removes what an extraction left in this folder that leads outside it (see [`confine`]).
    Confine(PathBuf),
}

/// The steps for one output (or one group of outputs) and what they produce.
#[derive(Debug, Default)]
pub struct Job {
    pub steps: Vec<Step>,
    pub outputs: Vec<PathBuf>,
}

/// Hands out output paths that exist neither on disk nor earlier in the same batch.
#[derive(Default)]
pub struct Names {
    taken: HashSet<PathBuf>,
    /// When set, every claim lands here instead of in the folder asked for.
    dest: Option<PathBuf>,
}

impl Names {
    /// `dir/stem.ext`, or `dir/stem (2).ext`, `dir/stem (3).ext`… An empty `ext` names a folder.
    pub fn claim(&mut self, dir: &Path, stem: &str, ext: &str) -> PathBuf {
        let dir = self.dest.as_deref().unwrap_or(dir);
        for n in 1.. {
            let base = if n == 1 { stem.to_string() } else { format!("{stem} ({n})") };
            let name = if ext.is_empty() { base } else { format!("{base}.{ext}") };
            let path = dir.join(name);
            if !path.exists() && !self.taken.contains(&path) {
                self.taken.insert(path.clone());
                return path;
            }
        }
        unreachable!()
    }
}

/// File name without the extension that [`extension`] reports (`a.tar.gz` gives `a`).
pub fn stem(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = extension(path);
    if ext.is_empty() || path.is_dir() {
        return name;
    }
    name[..name.len() - ext.len() - 1].to_string()
}

fn parent(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

/// Whether `program` is on `PATH`.
pub fn has_program(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

/// ImageMagick 7 is `magick`; 6 (still what most distributions ship) is `convert`.
fn magick() -> &'static str {
    if has_program("magick") { "magick" } else { "convert" }
}

/// ImageMagick's name for an input, with its decoder fixed by the extension (`png:/a/b.png`), so
/// the content of an untrusted file can never pick a different parser. `frame` is `""` or `"[0]"`.
fn im_input(path: &Path, frame: &str) -> OsString {
    let ext = extension(path);
    let coder = match canonical(&ext) {
        "jpg" => "jpeg",
        other => other,
    };
    let mut s = OsString::from(format!("{coder}:"));
    s.push(path.as_os_str());
    s.push(frame);
    s
}

/// A folder holding Clementine's `policy.xml`, for `MAGICK_CONFIGURE_PATH`. Written once per run
/// of the app into a private temporary folder.
fn magick_policy_dir() -> Option<&'static Path> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::Builder::new().prefix("clementine-magick-").tempdir().ok()?;
        std::fs::write(dir.path().join("policy.xml"), include_str!("../data/imagemagick/policy.xml")).ok()?;
        Some(dir.keep())
    })
    .as_deref()
}

/// A [`Command`] for `program`; ImageMagick also gets Clementine's restrictive policy.
fn command(program: &str) -> Command {
    let mut c = Command::new(program);
    if matches!(program, "magick" | "convert")
        && let Some(dir) = magick_policy_dir()
    {
        c.env("MAGICK_CONFIGURE_PATH", dir);
    }
    c
}

fn ffmpeg(input: &Path) -> Cmd {
    ffmpeg_from(input, 0.0)
}

/// ffmpeg reading `input` from `start` seconds. Seeking before `-i` is fast, and exact
/// whenever the streams are re-encoded.
fn ffmpeg_from(input: &Path, start: f64) -> Cmd {
    let mut cmd = Cmd::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    if start > 0.0 {
        cmd = cmd.arg("-ss").arg(trim_number(start));
    }
    // Only plain local files: a disguised playlist or subtitle cannot make ffmpeg fetch URLs.
    cmd.args(["-protocol_whitelist", "file"]).arg("-i").arg(input)
}

const X264_HQ: &[&str] =
    &["-c:v", "libx264", "-crf", "18", "-preset", "medium", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k"];

/// Encoder settings that keep a cut, sped-up or filtered file in its own format, close to its quality.
fn reencode(input: &Path) -> &'static [&'static str] {
    let ext = extension(input);
    let ext = canonical(&ext);
    match kind_of(input) {
        Kind::Video => match ext {
            "mp4" | "m4v" | "mov" | "mkv" => X264_HQ,
            "webm" | "avi" => ffmpeg_codec(ext),
            // ffmpeg's defaults for the container.
            _ => &[],
        },
        _ => ffmpeg_codec(ext),
    }
}

/// A stream copy cuts at packet boundaries. That is exact only for PCM audio (and keeps its
/// bit depth); anything compressed is re-encoded so the cut lands where it was asked.
pub fn copy_is_exact(input: &Path) -> bool {
    matches!(canonical(&extension(input)), "wav" | "aiff")
}

/// `len` seconds of `input` from `start`, into `out`.
fn cut(input: &Path, start: f64, len: f64, out: &Path) -> Cmd {
    let cmd = ffmpeg_from(input, start).arg("-t").arg(trim_number(len));
    let cmd = if copy_is_exact(input) { cmd.args(["-c", "copy"]) } else { cmd.args(reencode(input).iter().copied()) };
    cmd.arg(out)
}

/// Encoder settings for a conversion target.
fn ffmpeg_codec(target: &str) -> &'static [&'static str] {
    match target {
        "mp3" => &["-vn", "-c:a", "libmp3lame", "-q:a", "2"],
        "m4a" => &["-vn", "-c:a", "aac", "-b:a", "192k"],
        "wav" => &["-vn", "-c:a", "pcm_s16le"],
        "flac" => &["-vn", "-c:a", "flac"],
        "ogg" => &["-vn", "-c:a", "libvorbis", "-q:a", "5"],
        "opus" => &["-vn", "-c:a", "libopus", "-b:a", "128k"],
        "aiff" => &["-vn", "-c:a", "pcm_s16be"],
        "mp4" | "mov" => &[
            "-c:v",
            "libx264",
            "-crf",
            "20",
            "-preset",
            "medium",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
        ],
        "mkv" => &["-map", "0", "-c", "copy"],
        "webm" => &["-c:v", "libvpx-vp9", "-crf", "32", "-b:v", "0", "-row-mt", "1", "-c:a", "libopus"],
        "avi" => &["-c:v", "mpeg4", "-q:v", "3", "-c:a", "libmp3lame"],
        "gif" => &[
            "-an",
            "-vf",
            "fps=12,scale='min(640,iw)':-2:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse",
            "-loop",
            "0",
        ],
        _ => &[],
    }
}

/// LibreOffice's profile lives in the user's cache, so it starts warm after the first run.
/// Two instances on one profile fight, so LibreOffice calls are serialised by [`LO_LOCK`].
fn soffice(input: &Path, convert_to: &str, outdir: &Path, infilter: Option<&str>) -> Cmd {
    let profile = lo_profile();
    let mut cmd = Cmd::new("soffice")
        .arg(format!("-env:UserInstallation=file://{}", profile.display()))
        .args(["--headless", "--norestore"]);
    if let Some(f) = infilter {
        cmd = cmd.arg(format!("--infilter={f}"));
    }
    cmd.arg("--convert-to").arg(convert_to).arg("--outdir").arg(outdir).arg(input)
}

static LO_LOCK: Mutex<()> = Mutex::new(());

fn lo_profile() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    cache.join("clementine").join("libreoffice")
}

/// Settings pinned in Clementine's own LibreOffice profile: documents are untrusted, so their
/// macros never run, whatever the profile was changed to.
const LO_LOCKDOWN: &str = r#"<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="DisableMacrosExecution" oor:op="fuse"><value>true</value></prop></item>
"#;

/// Writes [`LO_LOCKDOWN`] into the profile's `registrymodifications.xcu` before LibreOffice starts.
/// LibreOffice keeps entries it finds there, so this is a no-op after the first run.
fn lock_down_lo_profile(profile: &Path) -> Result<()> {
    let user = profile.join("user");
    let xcu = user.join("registrymodifications.xcu");
    let current = std::fs::read_to_string(&xcu).unwrap_or_default();
    let pinned = |key: &str| {
        current.lines().any(|l| {
            l.contains(&format!("\"{key}\""))
                && l.contains(if key == "MacroSecurityLevel" { "<value>3</value>" } else { "<value>true</value>" })
        })
    };
    if pinned("MacroSecurityLevel") && pinned("DisableMacrosExecution") {
        return Ok(());
    }
    let kept: String = current
        .lines()
        .filter(|l| {
            !l.contains("\"MacroSecurityLevel\"")
                && !l.contains("\"DisableMacrosExecution\"")
                && !l.contains("</oor:items>")
        })
        .map(|l| format!("{l}\n"))
        .collect();
    let head = if kept.contains("<oor:items") {
        kept
    } else {
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<oor:items xmlns:oor=\"http://openoffice.org/2001/registry\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\n".to_string()
    };
    std::fs::create_dir_all(&user)?;
    std::fs::write(&xcu, format!("{head}{LO_LOCKDOWN}</oor:items>\n"))?;
    Ok(())
}

/// LibreOffice writes `outdir/<input stem>.<ext>`; this converts and moves the result to `to`.
fn soffice_to(steps: &mut Vec<Step>, input: &Path, convert_to: &str, produced_ext: &str, scratch: &Path, to: PathBuf) {
    let infilter = match canonical(&extension(input)) {
        "csv" => Some("CSV:44,34,76,1"),
        "tsv" => Some("CSV:9,34,76,1"),
        "pdf" => Some("writer_pdf_import"),
        _ => None,
    };
    let outdir = scratch.join(format!("lo-{}", steps.len()));
    steps.push(Step::MkDir(outdir.clone()));
    steps.push(Step::Run(soffice(input, convert_to, &outdir, infilter)));
    steps.push(Step::Move { from: outdir.join(format!("{}.{produced_ext}", stem(input))), to });
}

/// Renders every page of a PDF into a fresh folder, 300 DPI.
fn pdf_pages(steps: &mut Vec<Step>, pdf: &Path, image: &str, dir: &Path, name: &str) {
    steps.push(Step::MkDir(dir.to_path_buf()));
    let format = if image == "png" { "-png" } else { "-jpeg" };
    steps.push(Step::Run(Cmd::new("pdftoppm").args(["-r", "300", format]).arg(pdf).arg(dir.join(name))));
}

/// Unpacks `input` into the empty folder `dest`, never outside it: entry names are checked before
/// anything is written, and links or device files leading out are removed afterwards.
fn extract(input: &Path, dest: &Path) -> Vec<Step> {
    let ext = extension(input);
    let cmd = match ext.as_str() {
        "zip" => Cmd::new("unzip").arg("-q").arg(input).arg("-d").arg(dest),
        "rar" => Cmd::new("unrar").args(["x", "-o+", "-idq", "-p-"]).arg(input).arg(format!("{}/", dest.display())),
        "7z" => Cmd::new("7z").args(["x", "-y", "-bd"]).arg(format!("-o{}", dest.display())).arg(input),
        "gz" => {
            let mut c = Cmd::new("gzip").args(["-dc"]).arg(input);
            c.stdout = Some(dest.join(stem(input)));
            return vec![Step::Run(c)];
        }
        _ => Cmd::new("tar").args(["--no-same-owner", "--no-same-permissions", "-xf"]).arg(input).arg("-C").arg(dest),
    };
    vec![Step::CheckArchive(input.to_path_buf()), Step::Run(cmd), Step::Confine(dest.to_path_buf())]
}

/// The entry names of an archive, from its own tool's listing.
fn archive_entries(input: &Path) -> Result<Vec<String>> {
    let (program, args, prefix): (&str, &[&str], Option<&str>) = match extension(input).as_str() {
        "zip" => ("unzip", &["-Z1"], None),
        "rar" => ("unrar", &["lb", "-p-"], None),
        "7z" => ("7z", &["l", "-ba", "-slt"], Some("Path = ")),
        _ => ("tar", &["-tf"], None),
    };
    let out = Command::new(program)
        .args(args)
        .arg(input)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("could not start {program}"))?;
    if !out.status.success() {
        bail!("{program} could not list {}", input.display());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .filter_map(|l| match prefix {
            Some(p) => l.strip_prefix(p),
            None => Some(l),
        })
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// An entry name that would land outside the folder it is extracted into.
fn unsafe_entry(name: &str) -> bool {
    name.starts_with('/') || name.starts_with('\\') || name.split(['/', '\\']).any(|part| part == "..")
}

/// Removes every symlink under `root` that leads outside it, and anything that is neither a file,
/// a folder nor a link (devices, pipes). Never follows a link while walking.
fn confine(root: &Path) -> Result<()> {
    let base = root.canonicalize()?;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let kind = std::fs::symlink_metadata(&path)?.file_type();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_symlink() {
                let target = std::fs::read_link(&path)?;
                let rel = path.parent().unwrap_or(root).strip_prefix(root).unwrap_or(Path::new("")).join(&target);
                let escapes = target.is_absolute()
                    || climbs_out(&rel)
                    || path.canonicalize().is_ok_and(|real| !real.starts_with(&base));
                if escapes {
                    std::fs::remove_file(&path)?;
                }
            } else if !kind.is_file() {
                std::fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

/// Whether a relative path, read without touching the disk, ever climbs above where it starts.
fn climbs_out(rel: &Path) -> bool {
    use std::path::Component;
    let mut depth = 0i32;
    for c in rel.components() {
        match c {
            Component::ParentDir => depth -= 1,
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return true,
        }
        if depth < 0 {
            return true;
        }
    }
    false
}

fn convert_job(input: &Path, target: &'static str, scratch: &Path, names: &mut Names) -> Result<Job> {
    let dir = parent(input);
    let name = stem(input);
    let src = canonical(&extension(input)).to_string();
    let mut steps = Vec::new();
    let mut out = names.claim(&dir, &name, target);

    match kind_of(input) {
        Kind::Image => {
            // Only the first frame of an animation or layered file becomes a still.
            let frame = if matches!(src.as_str(), "gif" | "tiff") && target != src { "[0]" } else { "" };
            steps.push(Step::Run(Cmd::new(magick()).arg(im_input(input, frame)).arg("-auto-orient").arg(&out)));
        }
        Kind::Audio | Kind::Video => {
            steps.push(Step::Run(ffmpeg(input).args(ffmpeg_codec(target).iter().copied()).arg(&out)));
        }
        Kind::Subtitle if target == "txt" => steps.push(Step::SubtitleToText { from: input.into(), to: out.clone() }),
        Kind::Subtitle => steps.push(Step::Run(ffmpeg(input).arg(&out))),
        Kind::Pdf => match target {
            "png" | "jpg" => {
                out = names.claim(&dir, &format!("{name} ({target})"), "");
                pdf_pages(&mut steps, input, target, &out, &name);
            }
            "txt" => steps.push(Step::Run(Cmd::new("pdftotext").arg("-layout").arg(input).arg(&out))),
            _ => soffice_to(&mut steps, input, target, target, scratch, out.clone()),
        },
        Kind::Document => {
            let office_native = matches!(src.as_str(), "docx" | "odt" | "rtf" | "txt" | "html");
            if target == "pdf" && office_native {
                soffice_to(&mut steps, input, "pdf", "pdf", scratch, out.clone());
            } else if target == "pdf" {
                // pandoc needs LaTeX for PDF; going through a DOCX needs only LibreOffice.
                let docx = scratch.join(format!("{name}.docx"));
                steps.push(Step::Run(pandoc(input, &src, "docx", &docx)));
                soffice_to(&mut steps, &docx, "pdf", "pdf", scratch, out.clone());
            } else if src == "rtf" && matches!(target, "docx" | "odt" | "html") {
                soffice_to(&mut steps, input, target, target, scratch, out.clone());
            } else if src == "rtf" {
                let docx = scratch.join(format!("{name}.docx"));
                soffice_to(&mut steps, input, "docx", "docx", scratch, docx.clone());
                steps.push(Step::Run(pandoc(&docx, "docx", target, &out)));
            } else {
                steps.push(Step::Run(pandoc(input, &src, target, &out)));
            }
        }
        Kind::Spreadsheet => {
            let (convert_to, produced) = match target {
                "csv" => ("csv:Text - txt - csv (StarCalc):44,34,76,1", "csv"),
                "tsv" => ("csv:Text - txt - csv (StarCalc):9,34,76,1", "csv"),
                other => (other, other),
            };
            soffice_to(&mut steps, input, convert_to, produced, scratch, out.clone());
        }
        Kind::Slides => match target {
            "png" | "jpg" => {
                let pdf = scratch.join(format!("{name}.pdf"));
                soffice_to(&mut steps, input, "pdf", "pdf", scratch, pdf.clone());
                out = names.claim(&dir, &format!("{name} ({target})"), "");
                pdf_pages(&mut steps, &pdf, target, &out, &name);
            }
            _ => soffice_to(&mut steps, input, target, target, scratch, out.clone()),
        },
        Kind::Archive => {
            let unpacked = scratch.join(format!("x-{name}"));
            steps.push(Step::MkDir(unpacked.clone()));
            steps.extend(extract(input, &unpacked));
            let pack = match target {
                // `-y` stores links as links: an archive's link must never pull in the file it names.
                "zip" => Cmd::new("zip").args(["-r", "-q", "-y"]).arg(&out).arg("."),
                "tar" => Cmd::new("tar").arg("-cf").arg(&out).arg("."),
                _ => Cmd::new("tar").arg("-czf").arg(&out).arg("."),
            };
            steps.push(Step::Run(Cmd { cwd: Some(unpacked), ..pack }));
        }
        Kind::Other => bail!("{} cannot be converted", input.display()),
    }
    Ok(Job { steps, outputs: vec![out] })
}

/// What keeps pandoc from reading files other than its input, or the network: `--sandbox` when it
/// works (2.15 and later; some 3.1 builds cannot read their own templates in it), otherwise a Lua
/// filter dropping images named outside the document's folder or by URL.
fn pandoc_guard() -> &'static [OsString] {
    static GUARD: std::sync::OnceLock<Vec<OsString>> = std::sync::OnceLock::new();
    GUARD.get_or_init(|| {
        let Ok(dir) = tempfile::Builder::new().prefix("clementine-pandoc-").tempdir() else { return Vec::new() };
        let probe_in = dir.path().join("probe.md");
        let probe_out = dir.path().join("probe.docx");
        let sandbox_works = std::fs::write(&probe_in, "probe\n").is_ok()
            && command("pandoc")
                .arg("--sandbox")
                .arg("-o")
                .arg(&probe_out)
                .arg(&probe_in)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
        if sandbox_works {
            return vec!["--sandbox".into()];
        }
        let filter = dir.path().join("local-only.lua");
        if std::fs::write(&filter, include_str!("../data/pandoc/local-only.lua")).is_err() {
            return Vec::new();
        }
        let _ = std::fs::remove_file(&probe_in);
        let mut arg = OsString::from("--lua-filter=");
        arg.push(dir.keep().join("local-only.lua"));
        vec![arg]
    })
}

fn pandoc(input: &Path, src: &str, target: &str, out: &Path) -> Cmd {
    let reader = match src {
        "md" | "txt" => "markdown",
        other => other,
    };
    let writer = match target {
        "md" => "gfm",
        "txt" => "plain",
        other => other,
    };
    let mut cmd = Cmd::new("pandoc").args(["-f", reader, "-t", writer]);
    cmd = cmd.args(pandoc_guard().iter().cloned());
    if matches!(target, "html" | "epub") {
        cmd = cmd.arg("--standalone").arg("--metadata").arg(format!("title={}", stem(input)));
    }
    cmd.arg("-o").arg(out).arg(input)
}

/// The settings a tool runs with, and whether durations may be probed (only for a real run;
/// planning for [`missing_programs`] must stay quick and touch no file).
struct Setup<'a> {
    params: Option<Params>,
    probe: bool,
    scratch: &'a Path,
}

impl Setup<'_> {
    /// `input`'s duration, or a stand-in while only planning.
    fn duration(&self, input: &Path) -> Result<f64> {
        if !self.probe {
            return Ok(1.0);
        }
        media_duration(input).ok_or_else(|| anyhow!("could not read the length of {}", input.display()))
    }
}

fn tool_job(input: &Path, tool: Tool, setup: &Setup, names: &mut Names) -> Result<Job> {
    let dir = parent(input);
    let name = stem(input);
    let ext = extension(input);
    let mut steps = Vec::new();
    let im = |out: &Path, args: &[&str]| {
        Step::Run(Cmd::new(magick()).arg(im_input(input, "")).arg("-auto-orient").args(args.iter().copied()).arg(out))
    };
    let out = match tool {
        Tool::CompressImage => {
            let out = names.claim(&dir, &format!("{name} (compressed)"), &ext);
            let extra: &[&str] = if ext == "png" { &["-define", "png:compression-level=9"] } else { &[] };
            steps.push(im(&out, &[&["-strip", "-quality", "75"], extra].concat()));
            out
        }
        Tool::Resize => {
            let (label, geometry) = match setup.params {
                Some(Params::Resize(Resize::Fit { width, height })) => {
                    (format!("{width}x{height}"), format!("{width}x{height}"))
                }
                Some(Params::Resize(Resize::Percent(p))) => (format!("{p}%"), format!("{p}%")),
                _ => bail!("Resize needs a size"),
            };
            let out = names.claim(&dir, &format!("{name} ({label})"), &ext);
            steps.push(im(&out, &["-resize", &geometry]));
            out
        }
        Tool::CropImage => {
            // An aspect crop is centred; `+repage` drops the offset the crop leaves behind.
            let (gravity, geometry) = match setup.params {
                Some(Params::Crop(Crop::Aspect(a, b))) => ("center", format!("{a}:{b}")),
                Some(Params::Crop(Crop::Rect { x, y, w, h })) => ("northwest", format!("{w}x{h}+{x}+{y}")),
                _ => bail!("Crop needs a size"),
            };
            let out = names.claim(&dir, &format!("{name} (cropped)"), &ext);
            steps.push(im(&out, &["-gravity", gravity, "-crop", &geometry, "+repage"]));
            out
        }
        Tool::Rotate90 => {
            let out = names.claim(&dir, &format!("{name} (rotated)"), &ext);
            steps.push(im(&out, &["-rotate", "90"]));
            out
        }
        Tool::StripMetadata => {
            let out = names.claim(&dir, &format!("{name} (clean)"), &ext);
            steps.push(im(&out, &["-strip"]));
            out
        }
        Tool::CompressVideo => {
            let out = names.claim(&dir, &format!("{name} (compressed)"), "mp4");
            let args = [
                "-c:v",
                "libx264",
                "-crf",
                "28",
                "-preset",
                "slow",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "128k",
                "-movflags",
                "+faststart",
            ];
            steps.push(Step::Run(ffmpeg(input).args(args).arg(&out)));
            out
        }
        Tool::HalfResVideo => {
            let out = names.claim(&dir, &format!("{name} (half size)"), &ext);
            steps.push(Step::Run(
                ffmpeg(input).args(["-vf", "scale=trunc(iw/4)*2:trunc(ih/4)*2", "-c:a", "copy"]).arg(&out),
            ));
            out
        }
        Tool::MuteVideo => {
            let out = names.claim(&dir, &format!("{name} (muted)"), &ext);
            steps.push(Step::Run(ffmpeg(input).args(["-c:v", "copy", "-an"]).arg(&out)));
            out
        }
        Tool::Thumbnail => {
            let Some(Params::Thumbnail { at }) = setup.params else { bail!("Thumbnail needs a time") };
            let out = names.claim(&dir, &format!("{name} (thumbnail)"), "jpg");
            let args = ["-frames:v", "1", "-q:v", "2", "-update", "1"];
            steps.push(Step::Run(ffmpeg_from(input, at).args(args).arg(&out)));
            out
        }
        Tool::Trim => {
            let Some(Params::Trim { start, end }) = setup.params else { bail!("Trim needs a range") };
            let out = names.claim(&dir, &format!("{name} (trimmed)"), &ext);
            steps.push(Step::Run(cut(input, start, end - start, &out)));
            out
        }
        Tool::SplitMedia => {
            let Some(Params::Split(by)) = setup.params else { bail!("Split needs a part length") };
            let out = names.claim(&dir, &format!("{name} (parts)"), "");
            steps.push(Step::MkDir(out.clone()));
            let parts = split_points(setup.duration(input)?, by);
            let width = parts.len().to_string().len();
            for (i, (start, len)) in parts.into_iter().enumerate() {
                let part = out.join(format!("{name} {:0width$}.{ext}", i + 1));
                steps.push(Step::Run(cut(input, start, len, &part)));
            }
            out
        }
        Tool::Speed => {
            let Some(Params::Speed(f)) = setup.params else { bail!("Speed needs a factor") };
            let out = names.claim(&dir, &format!("{name} ({})", speed_name(f)), &ext);
            let mut cmd = ffmpeg(input);
            if kind_of(input) == Kind::Video {
                // Passthrough keeps every frame at its new time; the default constant frame rate
                // drops or repeats frames and lands up to a few frames off the asked length.
                cmd = cmd
                    .arg("-filter:v")
                    .arg(format!("setpts=PTS/{}", trim_number(f)))
                    .args(["-fps_mode", "passthrough"]);
            }
            cmd = cmd.arg("-filter:a").arg(atempo_chain(f)).args(reencode(input).iter().copied());
            steps.push(Step::Run(cmd.arg(&out)));
            out
        }
        Tool::FadeAudio => {
            let Some(Params::Fade { fade_in, fade_out }) = setup.params else { bail!("Fade needs lengths") };
            let mut fades = Vec::new();
            if fade_in > 0.0 {
                fades.push(format!("afade=t=in:st=0:d={}", trim_number(fade_in)));
            }
            if fade_out > 0.0 {
                let from = (setup.duration(input)? - fade_out).max(0.0);
                fades.push(format!("afade=t=out:st={}:d={}", trim_number(from), trim_number(fade_out)));
            }
            let out = names.claim(&dir, &format!("{name} (faded)"), &ext);
            let cmd = ffmpeg(input).arg("-filter:a").arg(fades.join(",")).args(reencode(input).iter().copied());
            steps.push(Step::Run(cmd.arg(&out)));
            out
        }
        Tool::MonoAudio => {
            let out = names.claim(&dir, &format!("{name} (mono)"), &ext);
            steps.push(Step::Run(ffmpeg(input).args(["-ac", "1"]).args(reencode(input).iter().copied()).arg(&out)));
            out
        }
        Tool::RotatePdf => {
            let Some(Params::RotatePages(angle)) = setup.params else { bail!("Rotate needs an angle") };
            let out = names.claim(&dir, &format!("{name} (rotated)"), "pdf");
            let cmd = Cmd::new("qpdf").arg("--warning-exit-0").arg(input).arg(format!("--rotate=+{angle}")).arg(&out);
            steps.push(Step::Run(cmd));
            out
        }
        Tool::ExtractPages => {
            let Some(Params::Pages { first, last }) = setup.params else { bail!("Pages needs a range") };
            let range = if first == last { format!("page {first}") } else { format!("pages {first}-{last}") };
            let out = names.claim(&dir, &format!("{name} ({range})"), "pdf");
            if has_program("qpdf") {
                let cmd = Cmd::new("qpdf").args(["--warning-exit-0", "--empty", "--pages"]).arg(input);
                steps.push(Step::Run(cmd.arg(format!("{first}-{last}")).arg("--").arg(&out)));
            } else {
                // poppler: one file per page, then joined back together.
                let pages = setup.scratch.join(format!("pages-{name}"));
                steps.push(Step::MkDir(pages.clone()));
                let separate = Cmd::new("pdfseparate").arg("-f").arg(first.to_string()).arg("-l").arg(last.to_string());
                steps.push(Step::Run(separate.arg(input).arg(pages.join("p-%d.pdf"))));
                let unite = Cmd::new("pdfunite").args((first..=last).map(|n| pages.join(format!("p-{n}.pdf"))));
                steps.push(Step::Run(unite.arg(&out)));
            }
            out
        }
        Tool::CompressAudio => {
            let out = names.claim(&dir, &format!("{name} (compressed)"), "mp3");
            steps.push(Step::Run(ffmpeg(input).args(["-vn", "-c:a", "libmp3lame", "-b:a", "128k"]).arg(&out)));
            out
        }
        Tool::NormalizeAudio => {
            let out = names.claim(&dir, &format!("{name} (normalized)"), &ext);
            steps.push(Step::Run(ffmpeg(input).args(["-af", "loudnorm"]).arg(&out)));
            out
        }
        Tool::CompressPdf => {
            let out = names.claim(&dir, &format!("{name} (compressed)"), "pdf");
            let args = ["-sDEVICE=pdfwrite", "-dPDFSETTINGS=/ebook", "-dNOPAUSE", "-dBATCH", "-dQUIET"];
            let mut o = OsString::from("-sOutputFile=");
            o.push(&out);
            steps.push(Step::Run(Cmd::new("gs").args(args).arg(o).arg(input)));
            out
        }
        Tool::SplitPdf => {
            let out = names.claim(&dir, &format!("{name} (pages)"), "");
            steps.push(Step::MkDir(out.clone()));
            steps.push(Step::Run(Cmd::new("pdfseparate").arg(input).arg(out.join(format!("{name}-%d.pdf")))));
            out
        }
        Tool::Extract => {
            let out = names.claim(&dir, &name, "");
            steps.push(Step::MkDir(out.clone()));
            steps.extend(extract(input, &out));
            out
        }
        Tool::MergePdf | Tool::Zip => bail!("{tool:?} works on the whole selection"),
    };
    Ok(Job { steps, outputs: vec![out] })
}

/// Plans every job for `op`. `scratch` is a private folder for intermediate files.
///
/// `params` are the settings of a tool that asks for them ([`Tool::needs_params`]). With them,
/// tools that need a file's length probe it with ffprobe. Without them, a tool that asks is
/// planned with stand-in settings, only to learn which programs it calls.
pub fn plan(op: Op, files: &[PathBuf], params: Option<&Params>, scratch: &Path) -> Result<Vec<Job>> {
    plan_in(op, files, params, scratch, None)
}

/// Like [`plan`], with every output placed in `dest` instead of next to its input.
pub fn plan_in(
    op: Op,
    files: &[PathBuf],
    params: Option<&Params>,
    scratch: &Path,
    dest: Option<&Path>,
) -> Result<Vec<Job>> {
    // Inputs are claimed up front, so no output can ever be named after one.
    let mut names = Names { taken: files.iter().cloned().collect(), dest: dest.map(Path::to_path_buf) };
    let first = files.first().ok_or_else(|| anyhow!("nothing selected"))?;
    match op {
        Op::Convert(target) => files.iter().map(|f| convert_job(f, target, scratch, &mut names)).collect(),
        Op::Tool(Tool::MergePdf) => {
            let out = names.claim(&parent(first), "Merged", "pdf");
            let cmd = Cmd::new("pdfunite").args(files.iter().map(|f| f.as_os_str().to_owned())).arg(&out);
            Ok(vec![Job { steps: vec![Step::Run(cmd)], outputs: vec![out] }])
        }
        Op::Tool(Tool::Zip) => {
            let dir = parent(first);
            let base = if files.len() == 1 { stem(first) } else { "Archive".into() };
            let out = names.claim(&dir, &base, "zip");
            // Same folder: store bare names. Mixed folders: store full paths.
            let same = files.iter().all(|f| parent(f) == dir);
            let mut cmd = Cmd::new("zip").args(["-r", "-q"]).arg(&out);
            for f in files {
                cmd = cmd.arg(if same { f.file_name().unwrap().to_owned() } else { f.as_os_str().to_owned() });
            }
            cmd.cwd = Some(dir);
            Ok(vec![Job { steps: vec![Step::Run(cmd)], outputs: vec![out] }])
        }
        Op::Tool(tool) => {
            let setup = Setup {
                params: params.copied().or_else(|| params::defaults(tool, &Facts::default())),
                probe: params.is_some(),
                scratch,
            };
            files.iter().map(|f| tool_job(f, tool, &setup, &mut names)).collect()
        }
        Op::Action(_) => bail!("actions are handled by the window"),
    }
}

/// The programs `op` would call that are not installed. Empty means it can run.
pub fn missing_programs(op: Op, files: &[PathBuf]) -> Vec<String> {
    let Ok(jobs) = plan(op, files, None, Path::new("/nonexistent-scratch")) else { return Vec::new() };
    let mut programs: Vec<String> = jobs
        .iter()
        .flat_map(|j| &j.steps)
        .filter_map(|s| match s {
            Step::Run(c) => Some(c.program.clone()),
            _ => None,
        })
        .collect();
    // These read the file's length before they plan.
    if matches!(op, Op::Tool(Tool::SplitMedia | Tool::FadeAudio)) {
        programs.push("ffprobe".into());
    }
    let mut missing: Vec<String> = programs.into_iter().filter(|p| !has_program(p)).collect();
    missing.sort();
    missing.dedup();
    missing
}

fn run_cmd(cmd: &Cmd) -> Result<()> {
    let _lo = (cmd.program == "soffice").then(|| LO_LOCK.lock().unwrap_or_else(|e| e.into_inner()));
    if cmd.program == "soffice" {
        lock_down_lo_profile(&lo_profile()).context("preparing the LibreOffice profile")?;
    }
    let mut c = command(&cmd.program);
    c.args(&cmd.args).stdin(Stdio::null()).stderr(Stdio::piped());
    if let Some(dir) = &cmd.cwd {
        c.current_dir(dir);
    }
    match &cmd.stdout {
        Some(path) => c.stdout(std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?),
        None => c.stdout(Stdio::null()),
    };
    let out = c.output().with_context(|| format!("could not start {}", cmd.program))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = tail[tail.len().saturating_sub(3)..].join(" · ");
        bail!("{} failed ({}): {}", cmd.program, out.status, tail);
    }
    Ok(())
}

fn run_step(step: &Step) -> Result<()> {
    match step {
        Step::Run(cmd) => run_cmd(cmd),
        Step::MkDir(dir) => Ok(std::fs::create_dir_all(dir)?),
        Step::Move { from, to } => {
            if !from.exists() {
                bail!("the converter produced no {}", from.file_name().unwrap_or_default().to_string_lossy());
            }
            if std::fs::rename(from, to).is_err() {
                // Scratch and destination may sit on different file systems.
                std::fs::copy(from, to)?;
                std::fs::remove_file(from)?;
            }
            Ok(())
        }
        Step::SubtitleToText { from, to } => {
            let text = std::fs::read_to_string(from)?;
            Ok(std::fs::write(to, subtitle_text(&text))?)
        }
        Step::CheckArchive(archive) => match archive_entries(archive)?.into_iter().find(|e| unsafe_entry(e)) {
            Some(bad) => bail!("refusing {}: its entry {bad:?} points outside the folder", archive.display()),
            None => Ok(()),
        },
        Step::Confine(dir) => confine(dir),
    }
}

/// Runs `op` on `files` and returns what it produced. Blocks: call it off the UI thread.
/// A job that fails leaves nothing behind; earlier, successful jobs are kept.
/// `params` are the settings of a tool that asks for them. Results go to `dest` (created if
/// needed) or, when `None`, next to each input.
pub fn run(op: Op, files: &[PathBuf], params: Option<&Params>, dest: Option<&Path>) -> Result<Vec<PathBuf>> {
    if let Op::Tool(tool) = op
        && tool.needs_params()
        && params.is_none()
    {
        bail!("{} needs settings", op.label());
    }
    let scratch = tempfile::Builder::new().prefix("clementine-").tempdir()?;
    if let Some(dest) = dest {
        std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    }
    let mut produced = Vec::new();
    for job in plan_in(op, files, params, scratch.path(), dest)? {
        if let Err(e) = job.steps.iter().try_for_each(run_step) {
            for out in &job.outputs {
                let _ = if out.is_dir() { std::fs::remove_dir_all(out) } else { std::fs::remove_file(out) };
            }
            return Err(e);
        }
        produced.extend(job.outputs);
    }
    Ok(produced)
}

/// Runs a probe and returns its trimmed stdout, or `None` if it failed.
fn capture(program: &str, args: &[&OsStr]) -> Option<String> {
    let out = command(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A media file's length in seconds, from ffprobe.
pub fn media_duration(path: &Path) -> Option<f64> {
    let args =
        ["-v", "error", "-protocol_whitelist", "file", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"]
            .map(OsStr::new);
    let text = capture("ffprobe", &[&args[..], &[path.as_os_str()]].concat())?;
    text.lines().next()?.trim().parse().ok().filter(|d: &f64| d.is_finite() && *d > 0.0)
}

/// An image's width and height as the tools see it: first frame, upright.
pub fn image_size(path: &Path) -> Option<(u32, u32)> {
    let first = im_input(path, "[0]");
    let args = [
        first.as_os_str(),
        OsStr::new("-auto-orient"),
        OsStr::new("-format"),
        OsStr::new("%w %h"),
        OsStr::new("info:"),
    ];
    let text = capture(magick(), &args)?;
    let (w, h) = text.split_once(' ')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// A PDF's page count, from pdfinfo or qpdf.
pub fn pdf_page_count(path: &Path) -> Option<u32> {
    if let Some(info) = capture("pdfinfo", &[path.as_os_str()]) {
        let line = info.lines().find_map(|l| l.strip_prefix("Pages:"))?;
        return line.trim().parse().ok();
    }
    capture("qpdf", &[OsStr::new("--show-npages"), path.as_os_str()])?.parse().ok()
}

/// What the settings dialog for `tool` needs to know about `files`. Blocks on ffprobe and friends.
pub fn facts(tool: Tool, files: &[PathBuf]) -> Facts {
    let min_f = |v: Vec<f64>| v.into_iter().reduce(f64::min);
    match tool {
        Tool::Trim | Tool::SplitMedia | Tool::Thumbnail | Tool::FadeAudio | Tool::Speed => {
            let durations: Option<Vec<f64>> = files.iter().map(|f| media_duration(f)).collect();
            Facts { duration: durations.and_then(min_f), ..Facts::default() }
        }
        Tool::CropImage | Tool::Resize => Facts { size: files.first().and_then(|f| image_size(f)), ..Facts::default() },
        Tool::ExtractPages | Tool::RotatePdf => {
            let pages: Option<Vec<u32>> = files.iter().map(|f| pdf_page_count(f)).collect();
            Facts { pages: pages.and_then(|p| p.into_iter().min()), ..Facts::default() }
        }
        _ => Facts::default(),
    }
}

/// Strips cue numbers, timings, WebVTT headers and inline tags, keeping only the spoken lines.
pub fn subtitle_text(sub: &str) -> String {
    let mut lines = Vec::new();
    let mut in_note = false;
    for line in sub.lines() {
        let t = line.trim().trim_start_matches('\u{feff}');
        if t.is_empty() {
            in_note = false;
            continue;
        }
        if in_note
            || t == "WEBVTT"
            || t.starts_with("WEBVTT ")
            || t.contains("-->")
            || t.chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        if t.starts_with("NOTE") || t.starts_with("STYLE") || t.starts_with("REGION") {
            in_note = true;
            continue;
        }
        let mut clean = String::new();
        let mut in_tag = false;
        for c in t.chars() {
            match c {
                '<' => in_tag = true,
                '>' if in_tag => in_tag = false,
                _ if !in_tag => clean.push(c),
                _ => {}
            }
        }
        lines.push(clean);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Mode, options};

    #[test]
    fn names_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"").unwrap();
        let mut n = Names::default();
        assert_eq!(n.claim(dir.path(), "a", "png"), dir.path().join("a (2).png"));
        assert_eq!(n.claim(dir.path(), "a", "png"), dir.path().join("a (3).png"));
        assert_eq!(n.claim(dir.path(), "a", "jpg"), dir.path().join("a.jpg"));
    }

    #[test]
    fn outputs_go_to_the_chosen_folder_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("results");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("a.jpg"), b"keep me").unwrap();
        let files = vec![dir.path().join("a.png"), dir.path().join("sub/b.png")];
        let jobs = plan_in(Op::Convert("jpg"), &files, None, Path::new("/tmp/scratch"), Some(&out)).unwrap();
        let outputs: Vec<_> = jobs.iter().flat_map(|j| j.outputs.clone()).collect();
        assert_eq!(outputs, vec![out.join("a (2).jpg"), out.join("b.jpg")]);
        // Tools and whole-selection ops follow the same rule.
        let zip = plan_in(Op::Tool(Tool::Zip), &files, None, Path::new("/tmp/scratch"), Some(&out)).unwrap();
        assert_eq!(zip[0].outputs, vec![out.join("Archive.zip")]);
        // Without a folder, nothing changes.
        let jobs = plan(Op::Convert("jpg"), &files, None, Path::new("/tmp/scratch")).unwrap();
        assert_eq!(jobs[0].outputs, vec![dir.path().join("a.jpg")]);
    }

    #[test]
    fn stem_drops_double_extensions() {
        assert_eq!(stem(Path::new("/x/backup.tar.gz")), "backup");
        assert_eq!(stem(Path::new("/x/photo.v2.JPG")), "photo.v2");
        assert_eq!(stem(Path::new("/x/README")), "README");
    }

    #[test]
    fn subtitles_lose_their_timing() {
        let vtt = "WEBVTT\n\nNOTE a comment\nspanning lines\n\n1\n00:00:01.000 --> 00:00:02.000\n<i>Hello</i> there\n\n2\n00:00:03.000 --> 00:00:04.000\nGeneral Kenobi\n";
        assert_eq!(subtitle_text(vtt), "Hello there\nGeneral Kenobi\n");
    }

    fn args_of(op: Op, file: &str, params: Params) -> Vec<String> {
        let file = PathBuf::from(format!("/nonexistent/{file}"));
        let setup = Setup { params: Some(params), probe: false, scratch: Path::new("/s") };
        let Op::Tool(tool) = op else { unreachable!() };
        let job = tool_job(&file, tool, &setup, &mut Names::default()).unwrap();
        job.steps
            .iter()
            .filter_map(|s| match s {
                Step::Run(c) => Some(c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn trims_copy_only_pcm() {
        let range = Params::Trim { start: 1.5, end: 4.0 };
        let wav = args_of(Op::Tool(Tool::Trim), "a.wav", range);
        assert!(wav.windows(2).any(|w| w == ["-c", "copy"]), "{wav:?}");
        let mp4 = args_of(Op::Tool(Tool::Trim), "a.mp4", range);
        assert!(mp4.windows(2).any(|w| w == ["-ss", "1.5"]) && mp4.windows(2).any(|w| w == ["-t", "2.5"]), "{mp4:?}");
        assert!(mp4.iter().any(|a| a == "libx264") && !mp4.iter().any(|a| a == "copy"), "{mp4:?}");
        assert!(mp4.last().unwrap().ends_with("a (trimmed).mp4"));
    }

    #[test]
    fn splits_make_one_cut_per_part() {
        let files = vec![PathBuf::from("/nonexistent/a.mp3")];
        let params = Params::Split(params::SplitBy::Parts(4));
        let jobs = plan(Op::Tool(Tool::SplitMedia), &files, None, Path::new("/s")).unwrap();
        assert_eq!(jobs[0].outputs, vec![PathBuf::from("/nonexistent/a (parts)")]);
        let setup = Setup { params: Some(params), probe: false, scratch: Path::new("/s") };
        let job = tool_job(&files[0], Tool::SplitMedia, &setup, &mut Names::default()).unwrap();
        let cuts = job.steps.iter().filter(|s| matches!(s, Step::Run(_))).count();
        assert_eq!(cuts, 4);
    }

    #[test]
    fn asking_tools_honour_the_output_folder() {
        let out = PathBuf::from("/nonexistent-out");
        for (file, tool) in [
            ("a.mp4", Tool::Trim),
            ("a.wav", Tool::SplitMedia),
            ("a.png", Tool::CropImage),
            ("a.pdf", Tool::ExtractPages),
        ] {
            let files = vec![PathBuf::from(format!("/nonexistent/{file}"))];
            let jobs = plan_in(Op::Tool(tool), &files, None, Path::new("/s"), Some(&out)).unwrap();
            assert!(
                jobs[0].outputs.iter().all(|o| o.parent() == Some(out.as_path())),
                "{tool:?} {:?}",
                jobs[0].outputs
            );
        }
    }

    #[test]
    fn asking_tools_refuse_to_run_without_settings() {
        let err = run(Op::Tool(Tool::Trim), &[PathBuf::from("/nonexistent/a.mp3")], None, None).unwrap_err();
        assert!(err.to_string().contains("needs settings"));
    }

    #[test]
    fn every_offered_op_plans() {
        let samples = [
            "a.png", "a.gif", "a.mp3", "a.mp4", "a.pdf", "a.docx", "a.md", "a.rtf", "a.xlsx", "a.tsv", "a.pptx",
            "a.srt", "a.zip", "a.tar.gz", "a.rar",
        ];
        let scratch = Path::new("/tmp/scratch");
        for s in samples {
            let files = vec![PathBuf::from(format!("/nonexistent/{s}"))];
            for mode in [Mode::Convert, Mode::Tools] {
                for op in options(mode, &files) {
                    let jobs = plan(op, &files, None, scratch).unwrap_or_else(|e| panic!("{s} {op:?}: {e}"));
                    assert!(!jobs.is_empty() && !jobs[0].outputs.is_empty(), "{s} {op:?}");
                    assert!(jobs[0].outputs.iter().all(|o| o != &files[0]), "{s} {op:?} overwrites its input");
                }
            }
        }
    }
}

/// Runs every offered conversion and tool on real sample files, skipping ops whose programs are
/// missing. Slow (LibreOffice): `cargo test -- --ignored --nocapture`.
#[cfg(test)]
mod real {
    use super::*;
    use crate::catalog::{Mode, options};

    fn sh(program: &str, args: &[&str], cwd: &Path) {
        let ok = Command::new(program).args(args).current_dir(cwd).stdout(Stdio::null()).stderr(Stdio::null()).status();
        assert!(ok.is_ok_and(|s| s.success()), "making a sample with {program} {args:?} failed");
    }

    fn samples(dir: &Path) -> Vec<PathBuf> {
        let ff = ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"];
        sh(magick(), &["-size", "64x48", "gradient:orange-white", "photo.png"], dir);
        sh(magick(), &["-size", "64x48", "xc:orange", "anim.gif"], dir);
        sh("ffmpeg", &[&ff[..], &["sine=frequency=440:duration=1", "-ac", "2", "tone.wav"]].concat(), dir);
        sh(
            "ffmpeg",
            &[
                &ff[..],
                &[
                    "testsrc=size=128x96:rate=25:duration=1",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=duration=1",
                    "-shortest",
                    "-pix_fmt",
                    "yuv420p",
                    "clip.mp4",
                ],
            ]
            .concat(),
            dir,
        );
        std::fs::write(dir.join("notes.md"), "# Title\n\nSome *text* and a list:\n\n- one\n- two\n").unwrap();
        std::fs::write(dir.join("table.csv"), "a,b\n1,2\n3,4\n").unwrap();
        std::fs::write(dir.join("subs.srt"), "1\n00:00:01,000 --> 00:00:02,000\nHello\n").unwrap();
        sh("pandoc", &["notes.md", "-o", "letter.docx"], dir);
        sh("pandoc", &["notes.md", "-s", "-o", "letter.rtf"], dir);
        sh(magick(), &["photo.png", "photo.png", "photo.png", "doc.pdf"], dir);
        sh("ffmpeg", &["-hide_banner", "-loglevel", "error", "-i", "tone.wav", "tone.mp3"], dir);
        sh("zip", &["-q", "bundle.zip", "notes.md", "table.csv"], dir);
        sh("tar", &["-czf", "bundle.tar.gz", "notes.md"], dir);
        let slides = dir.join("deck.odp");
        let mut c = soffice(&dir.join("letter.docx"), "pdf", dir, None);
        c.args = c.args.into_iter().map(|a| if a == "pdf" { "odp".into() } else { a }).collect();
        let _ = run_cmd(&Cmd { args: c.args, ..c });
        let mut all: Vec<PathBuf> = [
            "photo.png",
            "anim.gif",
            "tone.wav",
            "tone.mp3",
            "clip.mp4",
            "notes.md",
            "table.csv",
            "subs.srt",
            "letter.docx",
            "letter.rtf",
            "doc.pdf",
            "bundle.zip",
            "bundle.tar.gz",
        ]
        .iter()
        .map(|n| dir.join(n))
        .collect();
        if slides.exists() {
            all.push(slides);
        }
        all
    }

    /// What a tool's output must look like on the 1-second, 64×48 and 3-page samples.
    #[derive(Clone, Copy, Debug)]
    enum Expect {
        /// Not checked beyond being non-empty.
        Anything,
        /// One media file this many seconds long.
        Seconds(f64),
        /// A folder of this many media parts, each this long.
        Parts(usize, f64),
        Pixels(u32, u32),
        Pages(u32),
        Rotation(u32),
        Channels(u32),
    }

    /// The settings each asking tool runs with here, and what each must produce.
    fn cases(tool: Tool) -> Vec<(Params, Expect)> {
        use params::SplitBy;
        match tool {
            Tool::Trim => vec![(Params::Trim { start: 0.25, end: 0.75 }, Expect::Seconds(0.5))],
            Tool::SplitMedia => vec![
                (Params::Split(SplitBy::Parts(2)), Expect::Parts(2, 0.5)),
                (Params::Split(SplitBy::Every(0.4)), Expect::Parts(3, 0.4)),
            ],
            Tool::Speed => vec![(Params::Speed(2.0), Expect::Seconds(0.5)), (Params::Speed(0.5), Expect::Seconds(2.0))],
            Tool::FadeAudio => vec![(Params::Fade { fade_in: 0.2, fade_out: 0.3 }, Expect::Seconds(1.0))],
            Tool::Thumbnail => vec![(Params::Thumbnail { at: 0.5 }, Expect::Pixels(128, 96))],
            Tool::CropImage => vec![
                (Params::Crop(Crop::Aspect(1, 1)), Expect::Pixels(48, 48)),
                (Params::Crop(Crop::Rect { x: 8, y: 8, w: 32, h: 16 }), Expect::Pixels(32, 16)),
            ],
            Tool::Resize => vec![
                (Params::Resize(Resize::Percent(50)), Expect::Pixels(32, 24)),
                (Params::Resize(Resize::Fit { width: 40, height: 40 }), Expect::Pixels(40, 30)),
            ],
            Tool::RotatePdf => vec![(Params::RotatePages(90), Expect::Rotation(90))],
            Tool::ExtractPages => vec![(Params::Pages { first: 2, last: 3 }, Expect::Pages(2))],
            _ => Vec::new(),
        }
    }

    fn near(what: &Path, got: Option<f64>, want: f64) -> Result<(), String> {
        // ffmpeg rounds to whole frames (0.1 s at 10 fps) and audio packets.
        match got {
            Some(g) if (g - want).abs() <= 0.12 => Ok(()),
            _ => Err(format!("{} lasts {got:?} s, wanted {want}", what.display())),
        }
    }

    fn check(outs: &[PathBuf], expect: Expect) -> Result<(), String> {
        let out = outs.first().ok_or("no output")?;
        match expect {
            Expect::Anything => Ok(()),
            Expect::Seconds(s) => near(out, media_duration(out), s),
            Expect::Parts(n, each) => {
                let mut parts: Vec<PathBuf> = std::fs::read_dir(out).unwrap().map(|e| e.unwrap().path()).collect();
                parts.sort();
                if parts.len() != n {
                    return Err(format!("{} parts, wanted {n}: {parts:?}", parts.len()));
                }
                // The last part of an uneven split holds what is left.
                parts[..n - 1].iter().try_for_each(|p| near(p, media_duration(p), each))
            }
            Expect::Pixels(w, h) => match image_size(out) {
                Some(got) if got == (w, h) => Ok(()),
                got => Err(format!("{} is {got:?}, wanted {w}x{h}", out.display())),
            },
            Expect::Pages(n) => match pdf_page_count(out) {
                Some(got) if got == n => Ok(()),
                got => Err(format!("{got:?} pages, wanted {n}")),
            },
            Expect::Rotation(r) => {
                let info = capture("pdfinfo", &[out.as_os_str()]).unwrap_or_default();
                let rot = info.lines().find_map(|l| l.strip_prefix("Page rot:")).map(str::trim);
                if rot == Some(r.to_string().as_str()) { Ok(()) } else { Err(format!("page rotation {rot:?}")) }
            }
            Expect::Channels(n) => {
                let args =
                    ["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=channels", "-of", "csv=p=0"]
                        .map(OsStr::new);
                let got = capture("ffprobe", &[&args[..], &[out.as_os_str()]].concat());
                if got.as_deref() == Some(n.to_string().as_str()) { Ok(()) } else { Err(format!("{got:?} channels")) }
            }
        }
    }

    #[test]
    #[ignore]
    fn every_op_on_real_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut failures = Vec::new();
        let mut ran = 0;
        for file in samples(dir.path()) {
            let files = vec![file.clone()];
            for op in [Mode::Convert, Mode::Tools].into_iter().flat_map(|m| options(m, &files)) {
                let name = format!("{} → {:?}", file.file_name().unwrap().to_string_lossy(), op);
                let missing = missing_programs(op, &files);
                if !missing.is_empty() {
                    println!("skip {name}: needs {}", missing.join(", "));
                    continue;
                }
                let runs: Vec<(Option<Params>, Expect)> = match op {
                    Op::Tool(Tool::MonoAudio) => vec![(None, Expect::Channels(1))],
                    Op::Tool(t) if t.needs_params() => cases(t).into_iter().map(|(p, e)| (Some(p), e)).collect(),
                    _ => vec![(None, Expect::Anything)],
                };
                assert!(!runs.is_empty(), "{name} asks for settings but has no test case");
                for (params, expect) in runs {
                    let name = match params {
                        Some(p) => format!("{name} {p:?}"),
                        None => name.clone(),
                    };
                    ran += 1;
                    match run(op, &files, params.as_ref(), None) {
                        Ok(outs)
                            if outs.iter().all(|o| {
                                o.is_dir() && std::fs::read_dir(o).unwrap().next().is_some()
                                    || o.metadata().is_ok_and(|m| m.len() > 0)
                            }) =>
                        {
                            match check(&outs, expect) {
                                Ok(()) => println!("ok   {name}"),
                                Err(e) => failures.push(format!("{name}: {e}")),
                            }
                        }
                        Ok(outs) => failures.push(format!("{name}: empty output {outs:?}")),
                        Err(e) => failures.push(format!("{name}: {e:#}")),
                    }
                }
            }
        }
        println!("{ran} ops ran, {} failed", failures.len());
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }
}

#[cfg(test)]
#[path = "security_tests.rs"]
mod security_tests;
