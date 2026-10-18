//! Turns an [`Op`] on some files into external commands, and runs them.
//!
//! Nothing is converted in-process except subtitles to text: the heavy lifting
//! is ffmpeg, ImageMagick, pandoc, LibreOffice and poppler, all local. An op whose
//! programs are missing is still listed, but greyed out (see [`missing_programs`]).
//!
//! Outputs land next to the input, or in the folder chosen in Settings, under a name that is
//! never taken, so an input is never overwritten.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};

use crate::catalog::{Kind, Op, Tool, canonical, extension, kind_of};

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
    Move { from: PathBuf, to: PathBuf },
    SubtitleToText { from: PathBuf, to: PathBuf },
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

fn ffmpeg(input: &Path) -> Cmd {
    Cmd::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"]).arg(input)
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
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let profile = cache.join("clementine").join("libreoffice");
    let mut cmd = Cmd::new("soffice")
        .arg(format!("-env:UserInstallation=file://{}", profile.display()))
        .args(["--headless", "--norestore"]);
    if let Some(f) = infilter {
        cmd = cmd.arg(format!("--infilter={f}"));
    }
    cmd.arg("--convert-to").arg(convert_to).arg("--outdir").arg(outdir).arg(input)
}

static LO_LOCK: Mutex<()> = Mutex::new(());

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

fn extract(input: &Path, dest: &Path) -> Step {
    let ext = extension(input);
    let cmd = match ext.as_str() {
        "zip" => Cmd::new("unzip").arg("-q").arg(input).arg("-d").arg(dest),
        "rar" => Cmd::new("unrar").args(["x", "-o+", "-idq"]).arg(input).arg(format!("{}/", dest.display())),
        "7z" => Cmd::new("7z").args(["x", "-y", "-bd"]).arg(format!("-o{}", dest.display())).arg(input),
        "gz" => {
            let mut c = Cmd::new("gzip").args(["-dc"]).arg(input);
            c.stdout = Some(dest.join(stem(input)));
            c
        }
        _ => Cmd::new("tar").arg("-xf").arg(input).arg("-C").arg(dest),
    };
    Step::Run(cmd)
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
            let mut source = input.as_os_str().to_owned();
            source.push(frame);
            steps.push(Step::Run(Cmd::new(magick()).arg(source).arg("-auto-orient").arg(&out)));
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
            steps.push(extract(input, &unpacked));
            let pack = match target {
                "zip" => Cmd::new("zip").args(["-r", "-q"]).arg(&out).arg("."),
                "tar" => Cmd::new("tar").arg("-cf").arg(&out).arg("."),
                _ => Cmd::new("tar").arg("-czf").arg(&out).arg("."),
            };
            steps.push(Step::Run(Cmd { cwd: Some(unpacked), ..pack }));
        }
        Kind::Other => bail!("{} cannot be converted", input.display()),
    }
    Ok(Job { steps, outputs: vec![out] })
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
    if matches!(target, "html" | "epub") {
        cmd = cmd.arg("--standalone").arg("--metadata").arg(format!("title={}", stem(input)));
    }
    cmd.arg("-o").arg(out).arg(input)
}

fn tool_job(input: &Path, tool: Tool, names: &mut Names) -> Result<Job> {
    let dir = parent(input);
    let name = stem(input);
    let ext = extension(input);
    let mut steps = Vec::new();
    let im = |out: &Path, args: &[&str]| {
        Step::Run(Cmd::new(magick()).arg(input).arg("-auto-orient").args(args.iter().copied()).arg(out))
    };
    let out = match tool {
        Tool::CompressImage => {
            let out = names.claim(&dir, &format!("{name} (compressed)"), &ext);
            let extra: &[&str] = if ext == "png" { &["-define", "png:compression-level=9"] } else { &[] };
            steps.push(im(&out, &[&["-strip", "-quality", "75"], extra].concat()));
            out
        }
        Tool::ResizeHalf => {
            let out = names.claim(&dir, &format!("{name} (50%)"), &ext);
            steps.push(im(&out, &["-resize", "50%"]));
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
            steps.push(extract(input, &out));
            out
        }
        Tool::MergePdf | Tool::Zip => bail!("{tool:?} works on the whole selection"),
    };
    Ok(Job { steps, outputs: vec![out] })
}

/// Plans every job for `op`. `scratch` is a private folder for intermediate files.
pub fn plan(op: Op, files: &[PathBuf], scratch: &Path) -> Result<Vec<Job>> {
    plan_in(op, files, scratch, None)
}

/// Like [`plan`], with every output placed in `dest` instead of next to its input.
pub fn plan_in(op: Op, files: &[PathBuf], scratch: &Path, dest: Option<&Path>) -> Result<Vec<Job>> {
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
        Op::Tool(tool) => files.iter().map(|f| tool_job(f, tool, &mut names)).collect(),
        Op::Action(_) => bail!("actions are handled by the window"),
    }
}

/// The programs `op` would call that are not installed. Empty means it can run.
pub fn missing_programs(op: Op, files: &[PathBuf]) -> Vec<String> {
    let Ok(jobs) = plan(op, files, Path::new("/nonexistent-scratch")) else { return Vec::new() };
    let mut missing: Vec<String> = jobs
        .iter()
        .flat_map(|j| &j.steps)
        .filter_map(|s| match s {
            Step::Run(c) if !has_program(&c.program) => Some(c.program.clone()),
            _ => None,
        })
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

fn run_cmd(cmd: &Cmd) -> Result<()> {
    let _lo = (cmd.program == "soffice").then(|| LO_LOCK.lock().unwrap_or_else(|e| e.into_inner()));
    let mut c = Command::new(&cmd.program);
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
    }
}

/// Runs `op` on `files` and returns what it produced. Blocks: call it off the UI thread.
/// A job that fails leaves nothing behind; earlier, successful jobs are kept.
/// Results go to `dest` (created if needed) or, when `None`, next to each input.
pub fn run(op: Op, files: &[PathBuf], dest: Option<&Path>) -> Result<Vec<PathBuf>> {
    let scratch = tempfile::Builder::new().prefix("clementine-").tempdir()?;
    if let Some(dest) = dest {
        std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    }
    let mut produced = Vec::new();
    for job in plan_in(op, files, scratch.path(), dest)? {
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
        let jobs = plan_in(Op::Convert("jpg"), &files, Path::new("/tmp/scratch"), Some(&out)).unwrap();
        let outputs: Vec<_> = jobs.iter().flat_map(|j| j.outputs.clone()).collect();
        assert_eq!(outputs, vec![out.join("a (2).jpg"), out.join("b.jpg")]);
        // Tools and whole-selection ops follow the same rule.
        let zip = plan_in(Op::Tool(Tool::Zip), &files, Path::new("/tmp/scratch"), Some(&out)).unwrap();
        assert_eq!(zip[0].outputs, vec![out.join("Archive.zip")]);
        // Without a folder, nothing changes.
        let jobs = plan(Op::Convert("jpg"), &files, Path::new("/tmp/scratch")).unwrap();
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
                    let jobs = plan(op, &files, scratch).unwrap_or_else(|e| panic!("{s} {op:?}: {e}"));
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
        sh("ffmpeg", &[&ff[..], &["sine=frequency=440:duration=1", "tone.wav"]].concat(), dir);
        sh(
            "ffmpeg",
            &[
                &ff[..],
                &[
                    "testsrc=size=128x96:rate=10:duration=1",
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
        sh(magick(), &["photo.png", "doc.pdf"], dir);
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
                ran += 1;
                match run(op, &files, None) {
                    Ok(outs)
                        if outs.iter().all(|o| {
                            o.is_dir() && std::fs::read_dir(o).unwrap().next().is_some()
                                || o.metadata().is_ok_and(|m| m.len() > 0)
                        }) =>
                    {
                        println!("ok   {name}")
                    }
                    Ok(outs) => failures.push(format!("{name}: empty output {outs:?}")),
                    Err(e) => failures.push(format!("{name}: {e:#}")),
                }
            }
        }
        println!("{ran} ops ran, {} failed", failures.len());
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }
}
