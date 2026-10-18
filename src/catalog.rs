//! What a file is, and what can be done with it.
//!
//! Everything here is pure: it looks at file names only, never at the disk,
//! so the wheel can be filled while a drag is still in flight.

use std::path::{Path, PathBuf};

/// The family a file belongs to, decided by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Image,
    Audio,
    Video,
    Pdf,
    Document,
    Spreadsheet,
    Slides,
    Subtitle,
    Archive,
    Other,
}

/// Which wheel is open: Shift, Ctrl and Alt by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Convert,
    Tools,
    Actions,
}

impl Mode {
    pub fn title(self) -> &'static str {
        match self {
            Mode::Convert => "Convert",
            Mode::Tools => "Tools",
            Mode::Actions => "Actions",
        }
    }

    pub fn next(self) -> Mode {
        match self {
            Mode::Convert => Mode::Tools,
            Mode::Tools => Mode::Actions,
            Mode::Actions => Mode::Convert,
        }
    }
}

/// A file tool: changes the file but keeps (mostly) its format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    CompressImage,
    ResizeHalf,
    Rotate90,
    StripMetadata,
    CompressVideo,
    HalfResVideo,
    MuteVideo,
    CompressAudio,
    NormalizeAudio,
    CompressPdf,
    SplitPdf,
    MergePdf,
    Extract,
    Zip,
}

/// A file action: works on anything, folders included, and converts nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    CopyFiles,
    CopyPath,
    CopyTo,
    MoveTo,
    ShowInFiles,
    Terminal,
}

/// One slice of a wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Convert to the format with this (lower-case, canonical) extension.
    Convert(&'static str),
    Tool(Tool),
    Action(Action),
}

impl Op {
    pub fn label(&self) -> String {
        match self {
            Op::Convert(ext) => ext.to_uppercase(),
            Op::Tool(t) => match t {
                Tool::CompressImage | Tool::CompressVideo | Tool::CompressAudio | Tool::CompressPdf => "Compress",
                Tool::ResizeHalf => "Resize 50%",
                Tool::Rotate90 => "Rotate",
                Tool::StripMetadata => "Strip metadata",
                Tool::HalfResVideo => "Half size",
                Tool::MuteVideo => "Mute",
                Tool::NormalizeAudio => "Normalize",
                Tool::SplitPdf => "Split pages",
                Tool::MergePdf => "Merge",
                Tool::Extract => "Extract",
                Tool::Zip => "Zip",
            }
            .to_string(),
            Op::Action(a) => match a {
                Action::CopyFiles => "Copy",
                Action::CopyPath => "Copy path",
                Action::CopyTo => "Copy to…",
                Action::MoveTo => "Move to…",
                Action::ShowInFiles => "Show in Files",
                Action::Terminal => "Terminal",
            }
            .to_string(),
        }
    }
}

const IMAGE: &[&str] = &["jpg", "jpeg", "png", "webp", "heic", "heif", "tif", "tiff", "avif", "bmp", "gif", "svg"];
const AUDIO: &[&str] = &["mp3", "m4a", "wav", "flac", "ogg", "oga", "opus", "aif", "aiff", "wma", "aac"];
const VIDEO: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "mpg", "mpeg", "flv"];
const DOCUMENT: &[&str] = &["docx", "odt", "rtf", "md", "markdown", "txt", "html", "htm", "epub"];
const SPREADSHEET: &[&str] = &["xlsx", "xls", "ods", "csv", "tsv"];
const SLIDES: &[&str] = &["pptx", "ppt", "odp"];
const SUBTITLE: &[&str] = &["srt", "vtt"];
const ARCHIVE: &[&str] = &["zip", "tar", "gz", "tgz", "rar", "7z"];

/// The lower-case extension that matters for this file. `a.tar.gz` gives `tar.gz`.
pub fn extension(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if name.ends_with(".tar.gz") {
        return "tar.gz".into();
    }
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Folds aliases together so `photo.jpeg` is not offered a conversion to JPG.
pub fn canonical(ext: &str) -> &str {
    match ext {
        "jpeg" => "jpg",
        "tif" => "tiff",
        "heif" => "heic",
        "aif" => "aiff",
        "oga" => "ogg",
        "markdown" => "md",
        "htm" => "html",
        "tgz" => "tar.gz",
        other => other,
    }
}

pub fn kind_of(path: &Path) -> Kind {
    if path.is_dir() {
        return Kind::Other;
    }
    let ext = extension(path);
    let ext = ext.as_str();
    let is = |list: &[&str]| list.contains(&ext);
    if ext == "pdf" {
        Kind::Pdf
    } else if ext == "tar.gz" || is(ARCHIVE) {
        Kind::Archive
    } else if is(IMAGE) {
        Kind::Image
    } else if is(AUDIO) {
        Kind::Audio
    } else if is(VIDEO) {
        Kind::Video
    } else if is(DOCUMENT) {
        Kind::Document
    } else if is(SPREADSHEET) {
        Kind::Spreadsheet
    } else if is(SLIDES) {
        Kind::Slides
    } else if is(SUBTITLE) {
        Kind::Subtitle
    } else {
        Kind::Other
    }
}

/// The formats a file of this kind can be converted to, in wheel order.
pub fn targets(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Image => &["jpg", "png", "webp", "avif", "heic", "tiff", "bmp", "pdf"],
        Kind::Audio => &["mp3", "m4a", "wav", "flac", "ogg", "opus", "aiff"],
        Kind::Video => &["mp4", "mov", "mkv", "webm", "avi", "gif", "mp3"],
        Kind::Pdf => &["png", "jpg", "txt", "docx"],
        Kind::Document => &["docx", "odt", "pdf", "md", "txt", "html", "epub"],
        Kind::Spreadsheet => &["xlsx", "ods", "csv", "tsv", "pdf"],
        Kind::Slides => &["pdf", "pptx", "odp", "png", "jpg"],
        Kind::Subtitle => &["srt", "vtt", "txt"],
        Kind::Archive => &["zip", "tar", "tar.gz"],
        Kind::Other => &[],
    }
}

/// The kind every file in the selection shares, or `None` for a mixed or empty one.
pub fn common_kind(files: &[PathBuf]) -> Option<Kind> {
    let first = kind_of(files.first()?);
    files.iter().all(|f| kind_of(f) == first).then_some(first)
}

/// Every slice of the wheel for this mode and selection.
pub fn options(mode: Mode, files: &[PathBuf]) -> Vec<Op> {
    match mode {
        Mode::Convert => {
            let Some(kind) = common_kind(files) else { return Vec::new() };
            // A target is offered unless every file is already in it.
            targets(kind)
                .iter()
                .filter(|t| !files.iter().all(|f| canonical(&extension(f)) == **t))
                .map(|t| Op::Convert(t))
                .collect()
        }
        Mode::Tools => {
            let mut ops: Vec<Op> = match common_kind(files) {
                Some(Kind::Image) => vec![Tool::CompressImage, Tool::ResizeHalf, Tool::Rotate90, Tool::StripMetadata],
                Some(Kind::Video) => vec![Tool::CompressVideo, Tool::HalfResVideo, Tool::MuteVideo],
                Some(Kind::Audio) => vec![Tool::CompressAudio, Tool::NormalizeAudio],
                Some(Kind::Pdf) if files.len() > 1 => vec![Tool::MergePdf, Tool::CompressPdf, Tool::SplitPdf],
                Some(Kind::Pdf) => vec![Tool::CompressPdf, Tool::SplitPdf],
                Some(Kind::Archive) => vec![Tool::Extract],
                _ => Vec::new(),
            }
            .into_iter()
            .map(Op::Tool)
            .collect();
            if !files.is_empty() {
                ops.push(Op::Tool(Tool::Zip));
            }
            ops
        }
        Mode::Actions => {
            [Action::CopyFiles, Action::CopyPath, Action::CopyTo, Action::MoveTo, Action::ShowInFiles, Action::Terminal]
                .into_iter()
                .map(Op::Action)
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(format!("/nonexistent/{s}"))
    }

    #[test]
    fn kinds_follow_extensions() {
        assert_eq!(kind_of(&p("a.JPEG")), Kind::Image);
        assert_eq!(kind_of(&p("a.tar.gz")), Kind::Archive);
        assert_eq!(kind_of(&p("a.pdf")), Kind::Pdf);
        assert_eq!(kind_of(&p("a.srt")), Kind::Subtitle);
        assert_eq!(kind_of(&p("a")), Kind::Other);
    }

    #[test]
    fn own_format_is_not_offered() {
        let ops = options(Mode::Convert, &[p("a.jpeg")]);
        assert!(!ops.contains(&Op::Convert("jpg")));
        assert!(ops.contains(&Op::Convert("png")));
    }

    #[test]
    fn mixed_selection_converts_nothing_but_keeps_actions() {
        let files = [p("a.png"), p("b.mp3")];
        assert!(options(Mode::Convert, &files).is_empty());
        assert_eq!(options(Mode::Tools, &files), vec![Op::Tool(Tool::Zip)]);
        assert_eq!(options(Mode::Actions, &files).len(), 6);
    }

    #[test]
    fn merge_needs_two_pdfs() {
        assert!(!options(Mode::Tools, &[p("a.pdf")]).contains(&Op::Tool(Tool::MergePdf)));
        assert!(options(Mode::Tools, &[p("a.pdf"), p("b.pdf")]).contains(&Op::Tool(Tool::MergePdf)));
    }
}
