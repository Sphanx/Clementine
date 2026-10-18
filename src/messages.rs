//! Turns a converter's raw error into a sentence a person can act on.
//!
//! The raw text (usually the last lines of a program's stderr) is still shown in the
//! tooltip; this only picks the headline. Everything here is pure.

/// Maps a raw error (`format!("{e:#}")` of a backend error) to a friendly headline.
/// Unknown errors keep the first non-empty line of the raw text.
pub fn headline(raw: &str) -> String {
    let low = raw.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| low.contains(n));

    if has(&["no decode delegate", "no encode delegate", "nodecodedelegate", "noencodedelegate"]) {
        return match delegate_format(raw) {
            Some(f) => format!(
                "ImageMagick on this computer cannot handle {f} files. Install its {f} support \
                 (for HEIC and AVIF, the libheif plugin) and try again."
            ),
            None => "ImageMagick on this computer cannot handle this image format.".into(),
        };
    }
    if has(&[
        "incorrect password",
        "invalid password",
        "requires a password",
        "password required",
        "password-protected",
        "password protected",
        "file is encrypted",
        "encrypted pdf",
    ]) {
        if low.contains("password-protected archive") {
            return "This archive is password-protected. Extract it with its password in your archive \
                    manager, then try again."
                .into();
        }
        return "This PDF is password-protected. Remove the password in a PDF viewer, then try again.".into();
    }
    if has(&["unknown encoder", "encoder not found", "unknown decoder", "decoder not found", "unsupported codec"])
        || (low.contains("codec") && has(&["not supported", "unsupported", "not found"]))
    {
        return "The audio or video codec is not supported by the ffmpeg on this computer.".into();
    }
    if has(&[
        "invalid data found when processing input",
        "moov atom not found",
        "improper image header",
        "corrupt",
        "damaged",
        "couldn't find trailer",
        "may not be a pdf",
        "not a zip file",
        "end-of-central-directory",
        "unexpected end of",
        "premature end of",
        "truncated",
        "is not a valid",
    ]) {
        return "The file looks damaged or is not what its name says, so it could not be read.".into();
    }
    if has(&["could not start"]) {
        return "A program this needs could not be started. Is it installed?".into();
    }
    if has(&["no space left"]) {
        return "The disk is full.".into();
    }
    if has(&["permission denied", "read-only file system"]) {
        return "Susen is not allowed to write there. Pick another output folder in Settings.".into();
    }
    let first = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Something went wrong");
    first.to_string()
}

/// The format named in ImageMagick's "no decode delegate for this image format `HEIC'".
fn delegate_format(raw: &str) -> Option<String> {
    let start = raw.find('`').or_else(|| raw.find('\''))? + 1;
    let rest = &raw[start..];
    let end = rest.find(['\'', '`'])?;
    let f = rest[..end].trim();
    (!f.is_empty() && f.len() <= 8 && f.chars().all(|c| c.is_ascii_alphanumeric())).then(|| f.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heic_delegate() {
        let raw = "magick failed (exit status: 1): magick: no decode delegate for this image format \
                   `HEIC' @ error/constitute.c/ReadImage/746.";
        let h = headline(raw);
        assert!(h.contains("HEIC"), "{h}");
        assert!(h.contains("libheif"), "{h}");
    }

    #[test]
    fn avif_encode_delegate() {
        let raw = "convert: no encode delegate for this image format `AVIF' @ error/constitute.c/WriteImage/1412.";
        assert!(headline(raw).contains("AVIF"));
    }

    #[test]
    fn password_pdf() {
        for raw in [
            "pdftoppm failed (exit status: 1): Command Line Error: Incorrect password",
            "qpdf failed (exit status: 2): a.pdf: invalid password",
            "gs failed: This file requires a password for access.",
        ] {
            assert!(headline(raw).contains("password-protected"), "{raw}");
        }
    }

    #[test]
    fn password_detected_up_front() {
        let pdf = "locked.pdf is a password-protected PDF; Susen cannot open it without the password";
        assert!(headline(pdf).starts_with("This PDF is password-protected"), "{}", headline(pdf));
        let zip = "locked.zip is a password-protected archive; Susen cannot open it without the password";
        assert!(headline(zip).starts_with("This archive is password-protected"), "{}", headline(zip));
    }

    #[test]
    fn codec() {
        for raw in [
            "ffmpeg failed (exit status: 1): Unknown encoder 'libx265'",
            "ffmpeg failed: Decoder (codec hevc) not found for input stream #0:0",
            "ffmpeg failed: codec not currently supported in container",
        ] {
            assert!(headline(raw).contains("codec"), "{raw}");
        }
    }

    #[test]
    fn corrupt() {
        for raw in [
            "ffmpeg failed (exit status: 1): a.mp4: Invalid data found when processing input",
            "ffmpeg failed: moov atom not found",
            "magick failed: improper image header `x.png'",
            "pdftoppm failed: Syntax Error: Couldn't find trailer dictionary",
            "unzip failed: End-of-central-directory signature not found.",
        ] {
            assert!(headline(raw).contains("damaged"), "{raw}");
        }
    }

    #[test]
    fn missing_program_and_unknown() {
        assert!(headline("could not start ffmpeg: No such file or directory (os error 2)").contains("installed"));
        assert_eq!(headline("\n  something odd\nmore"), "something odd");
        assert_eq!(headline(""), "Something went wrong");
    }

    #[test]
    fn delegate_name_is_bounded() {
        assert_eq!(delegate_format("format `HEIC' @"), Some("HEIC".into()));
        assert_eq!(delegate_format("no quotes"), None);
        assert_eq!(delegate_format("format `not a format at all'"), None);
    }
}
