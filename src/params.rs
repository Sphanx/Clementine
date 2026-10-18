//! Settings for the tools that ask before they run, and the checks on them.
//!
//! Everything here is pure: what is known about the files ([`Facts`]) is probed
//! elsewhere (see `backend::facts`) and handed in.

use crate::catalog::Tool;

/// How [`Tool::SplitMedia`] cuts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SplitBy {
    /// This many parts of equal length.
    Parts(u32),
    /// A new part every this many seconds; the last one may be shorter.
    Every(f64),
}

/// How [`Tool::CropImage`] cuts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Crop {
    /// The largest centred box of this width : height ratio.
    Aspect(u32, u32),
    /// An exact rectangle, in pixels from the top-left corner.
    Rect { x: u32, y: u32, w: u32, h: u32 },
}

/// How [`Tool::Resize`] scales.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resize {
    Percent(u32),
    /// Fit inside this box, keeping the aspect ratio.
    Fit {
        width: u32,
        height: u32,
    },
}

/// The settings a tool asked for. Times are in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Params {
    Trim {
        start: f64,
        end: f64,
    },
    Split(SplitBy),
    Thumbnail {
        at: f64,
    },
    Speed(f64),
    Crop(Crop),
    Resize(Resize),
    Fade {
        fade_in: f64,
        fade_out: f64,
    },
    /// Degrees clockwise: 90, 180 or 270.
    RotatePages(u32),
    /// 1-based, inclusive.
    Pages {
        first: u32,
        last: u32,
    },
}

/// What is known about the selection when the dialog opens. Unknown values are `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Facts {
    /// The shortest duration in the selection.
    pub duration: Option<f64>,
    /// The first image's width and height.
    pub size: Option<(u32, u32)>,
    /// The fewest pages in the selection.
    pub pages: Option<u32>,
}

/// The speeds the Speed tool offers.
pub const SPEEDS: &[f64] = &[0.25, 0.5, 0.75, 1.25, 1.5, 2.0, 3.0, 4.0];

/// The aspect ratios the Crop tool offers.
pub const ASPECTS: &[(u32, u32)] = &[(1, 1), (4, 3), (3, 2), (16, 9), (9, 16), (4, 5)];

/// What the dialog starts with. `None` for a tool that asks nothing.
pub fn defaults(tool: Tool, facts: &Facts) -> Option<Params> {
    let duration = facts.duration.unwrap_or(0.0);
    Some(match tool {
        Tool::Trim => Params::Trim { start: 0.0, end: duration },
        Tool::SplitMedia => Params::Split(SplitBy::Parts(2)),
        Tool::Thumbnail => Params::Thumbnail { at: (duration / 2.0).min(5.0) },
        Tool::Speed => Params::Speed(2.0),
        Tool::CropImage => Params::Crop(Crop::Aspect(1, 1)),
        Tool::Resize => Params::Resize(Resize::Percent(50)),
        Tool::FadeAudio => {
            let d = if duration > 0.0 { (duration / 4.0).min(2.0) } else { 2.0 };
            Params::Fade { fade_in: d, fade_out: d }
        }
        Tool::RotatePdf => Params::RotatePages(90),
        Tool::ExtractPages => Params::Pages { first: 1, last: facts.pages.unwrap_or(1) },
        _ => return None,
    })
}

/// A start–end range inside a file of `duration` seconds (when known): `0 ≤ start < end ≤ duration`.
pub fn validate_range(start: f64, end: f64, duration: Option<f64>) -> Result<(), String> {
    if !start.is_finite() || !end.is_finite() {
        return Err("Enter a time like 1:30 or 90.5".into());
    }
    if start < 0.0 {
        return Err("The start cannot be before 0:00".into());
    }
    if start >= end {
        return Err("The start must come before the end".into());
    }
    // A probed duration is rounded; allow a millisecond of slack.
    if let Some(d) = duration
        && end > d + 0.001
    {
        return Err(format!("The end is past the file's length ({})", format_time(d)));
    }
    Ok(())
}

/// Checks `params` against what is known of the files. The message is shown in the dialog.
pub fn validate(params: &Params, facts: &Facts) -> Result<(), String> {
    let duration = facts.duration;
    match *params {
        Params::Trim { start, end } => validate_range(start, end, duration),
        Params::Split(SplitBy::Parts(n)) => {
            if !(2..=100).contains(&n) {
                return Err("Split into 2 to 100 parts".into());
            }
            match duration {
                Some(d) if d / f64::from(n) < 0.1 => Err("The parts would be shorter than 0.1 s".into()),
                _ => Ok(()),
            }
        }
        Params::Split(SplitBy::Every(s)) => {
            if !(s.is_finite() && s >= 0.1) {
                return Err("A part must be at least 0.1 s long".into());
            }
            match duration {
                Some(d) if s >= d => Err(format!("The file is only {} long", format_time(d))),
                Some(d) if (d / s).ceil() > 100.0 => Err("That makes more than 100 parts".into()),
                _ => Ok(()),
            }
        }
        Params::Thumbnail { at } => {
            if !at.is_finite() || at < 0.0 {
                return Err("Enter a time like 1:30 or 90.5".into());
            }
            match duration {
                Some(d) if at >= d => Err(format!("The time is past the file's length ({})", format_time(d))),
                _ => Ok(()),
            }
        }
        Params::Speed(f) if (0.25..=4.0).contains(&f) && f != 1.0 => Ok(()),
        Params::Speed(_) => Err("Choose a speed between 0.25× and 4×".into()),
        Params::Crop(Crop::Aspect(a, b)) if a > 0 && b > 0 => Ok(()),
        Params::Crop(Crop::Aspect(..)) => Err("Both sides of the ratio must be above 0".into()),
        Params::Crop(Crop::Rect { x, y, w, h }) => {
            if w == 0 || h == 0 {
                return Err("The width and height must be above 0".into());
            }
            match facts.size {
                Some((iw, ih))
                    if u64::from(x) + u64::from(w) > u64::from(iw) || u64::from(y) + u64::from(h) > u64::from(ih) =>
                {
                    Err(format!("The rectangle goes past the image ({iw} × {ih})"))
                }
                _ => Ok(()),
            }
        }
        Params::Resize(Resize::Percent(p)) if (1..=1000).contains(&p) && p != 100 => Ok(()),
        Params::Resize(Resize::Percent(_)) => Err("Choose a percentage from 1 to 1000, other than 100".into()),
        Params::Resize(Resize::Fit { width, height }) if width > 0 && height > 0 => Ok(()),
        Params::Resize(Resize::Fit { .. }) => Err("The width and height must be above 0".into()),
        Params::Fade { fade_in, fade_out } => {
            if !(fade_in.is_finite() && fade_out.is_finite()) || fade_in < 0.0 || fade_out < 0.0 {
                return Err("Fades cannot be negative".into());
            }
            if fade_in == 0.0 && fade_out == 0.0 {
                return Err("Set a fade in, a fade out, or both".into());
            }
            match duration {
                Some(d) if fade_in + fade_out > d + 0.001 => {
                    Err(format!("The fades are longer than the file ({})", format_time(d)))
                }
                _ => Ok(()),
            }
        }
        Params::RotatePages(90 | 180 | 270) => Ok(()),
        Params::RotatePages(_) => Err("Rotate by 90, 180 or 270 degrees".into()),
        Params::Pages { first, last } => {
            if first == 0 || first > last {
                return Err("The first page must be at least 1 and not after the last".into());
            }
            match facts.pages {
                Some(n) if last > n => Err(format!("The document has only {n} pages")),
                _ => Ok(()),
            }
        }
    }
}

/// Reads `90`, `90.5`, `1:30`, `1:30.25` or `1:02:03` as seconds.
pub fn parse_time(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut total = 0.0;
    for (i, part) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        let ok = !part.is_empty() && part.chars().all(|c| c.is_ascii_digit() || (last && c == '.'));
        if !ok {
            return None;
        }
        let v: f64 = part.parse().ok()?;
        // Minutes and seconds after a colon stay below 60.
        if i > 0 && v >= 60.0 {
            return None;
        }
        total = total * 60.0 + v;
    }
    Some(total)
}

/// `75.5` → `1:15.5`, `3725` → `1:02:05`. [`parse_time`] reads it back.
pub fn format_time(seconds: f64) -> String {
    let centis = (seconds.max(0.0) * 100.0).round() as u64;
    let (whole, frac) = (centis / 100, centis % 100);
    let (h, m, s) = (whole / 3600, whole / 60 % 60, whole % 60);
    let mut out = if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") };
    if frac > 0 {
        let f = format!("{frac:02}");
        out.push('.');
        out.push_str(f.trim_end_matches('0'));
    }
    out
}

/// Where each part of a `duration`-second file starts and how long it lasts.
pub fn split_points(duration: f64, by: SplitBy) -> Vec<(f64, f64)> {
    match by {
        SplitBy::Parts(n) => {
            let n = n.max(1);
            let len = duration / f64::from(n);
            (0..n).map(|i| (f64::from(i) * len, len)).collect()
        }
        SplitBy::Every(len) => {
            let len = len.max(0.1);
            let mut out = Vec::new();
            let mut start = 0.0;
            // A sliver under 10 ms at the end is rounding, not a part.
            while duration - start > 0.01 {
                out.push((start, len.min(duration - start)));
                start += len;
            }
            out
        }
    }
}

/// ffmpeg's `atempo` takes 0.5–2.0 per stage in older versions, so larger changes are chained.
pub fn atempo_chain(factor: f64) -> String {
    let mut stages = Vec::new();
    let mut f = factor;
    while f > 2.0 {
        stages.push(2.0);
        f /= 2.0;
    }
    while f < 0.5 {
        stages.push(0.5);
        f /= 0.5;
    }
    stages.push(f);
    stages.iter().map(|s| format!("atempo={s}")).collect::<Vec<_>>().join(",")
}

/// `2` → `2x`, `0.5` → `0.5x`: for output names and dialog labels.
pub fn speed_name(factor: f64) -> String {
    format!("{}x", trim_number(factor))
}

/// A number without a trailing `.0`, for ffmpeg arguments and names.
pub fn trim_number(v: f64) -> String {
    let s = format!("{:.3}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(duration: f64) -> Facts {
        Facts { duration: Some(duration), ..Facts::default() }
    }

    #[test]
    fn ranges_must_be_ordered_and_inside_the_file() {
        assert!(validate_range(0.0, 10.0, Some(10.0)).is_ok());
        assert!(validate_range(2.5, 3.0, None).is_ok());
        assert!(validate_range(5.0, 5.0, Some(10.0)).is_err(), "empty range");
        assert!(validate_range(6.0, 5.0, Some(10.0)).is_err(), "reversed");
        assert!(validate_range(-1.0, 5.0, Some(10.0)).is_err(), "negative start");
        assert!(validate_range(0.0, 10.5, Some(10.0)).is_err(), "past the end");
        assert!(validate_range(0.0, 10.0005, Some(10.0)).is_ok(), "rounding slack");
        assert!(validate_range(f64::NAN, 5.0, Some(10.0)).is_err());
    }

    #[test]
    fn every_tool_validates_its_own_defaults() {
        let f = Facts { duration: Some(30.0), size: Some((640, 480)), pages: Some(4) };
        for tool in [
            Tool::Trim,
            Tool::SplitMedia,
            Tool::Thumbnail,
            Tool::Speed,
            Tool::CropImage,
            Tool::Resize,
            Tool::FadeAudio,
            Tool::RotatePdf,
            Tool::ExtractPages,
        ] {
            assert!(tool.needs_params());
            let p = defaults(tool, &f).unwrap();
            assert_eq!(validate(&p, &f), Ok(()), "{tool:?} {p:?}");
        }
        assert_eq!(defaults(Tool::MonoAudio, &f), None);
    }

    #[test]
    fn other_params_are_checked() {
        let f = facts(10.0);
        assert!(validate(&Params::Split(SplitBy::Parts(1)), &f).is_err());
        assert!(validate(&Params::Split(SplitBy::Every(10.0)), &f).is_err());
        assert!(validate(&Params::Split(SplitBy::Every(3.0)), &f).is_ok());
        assert!(validate(&Params::Thumbnail { at: 10.0 }, &f).is_err());
        assert!(validate(&Params::Speed(1.0), &f).is_err());
        assert!(validate(&Params::Speed(8.0), &f).is_err());
        assert!(validate(&Params::Fade { fade_in: 6.0, fade_out: 6.0 }, &f).is_err());
        assert!(validate(&Params::Fade { fade_in: 0.0, fade_out: 0.0 }, &f).is_err());
        let img = Facts { size: Some((100, 50)), ..Facts::default() };
        assert!(validate(&Params::Crop(Crop::Rect { x: 50, y: 0, w: 50, h: 50 }), &img).is_ok());
        assert!(validate(&Params::Crop(Crop::Rect { x: 51, y: 0, w: 50, h: 50 }), &img).is_err());
        assert!(validate(&Params::Crop(Crop::Rect { x: 0, y: 0, w: 0, h: 5 }), &img).is_err());
        let pdf = Facts { pages: Some(3), ..Facts::default() };
        assert!(validate(&Params::Pages { first: 2, last: 3 }, &pdf).is_ok());
        assert!(validate(&Params::Pages { first: 2, last: 4 }, &pdf).is_err());
        assert!(validate(&Params::Pages { first: 0, last: 1 }, &pdf).is_err());
        assert!(validate(&Params::RotatePages(45), &pdf).is_err());
    }

    #[test]
    fn times_parse_and_print() {
        assert_eq!(parse_time("90"), Some(90.0));
        assert_eq!(parse_time(" 1:30.5 "), Some(90.5));
        assert_eq!(parse_time("1:02:03"), Some(3723.0));
        assert_eq!(parse_time("1:75"), None);
        assert_eq!(parse_time("1.5:30"), None);
        assert_eq!(parse_time("abc"), None);
        assert_eq!(parse_time(""), None);
        assert_eq!(format_time(75.5), "1:15.5");
        assert_eq!(format_time(3725.0), "1:02:05");
        assert_eq!(format_time(0.25), "0:00.25");
        for t in [0.0, 1.0, 59.99, 61.5, 3599.0, 7322.25] {
            assert_eq!(parse_time(&format_time(t)), Some(t));
        }
    }

    #[test]
    fn splits_cover_the_whole_file() {
        assert_eq!(split_points(9.0, SplitBy::Parts(3)), vec![(0.0, 3.0), (3.0, 3.0), (6.0, 3.0)]);
        assert_eq!(split_points(10.0, SplitBy::Every(4.0)), vec![(0.0, 4.0), (4.0, 4.0), (8.0, 2.0)]);
        assert_eq!(split_points(8.0, SplitBy::Every(4.0)).len(), 2);
    }

    #[test]
    fn tempo_is_chained_within_range() {
        assert_eq!(atempo_chain(2.0), "atempo=2");
        assert_eq!(atempo_chain(4.0), "atempo=2,atempo=2");
        assert_eq!(atempo_chain(0.25), "atempo=0.5,atempo=0.5");
        assert_eq!(atempo_chain(0.75), "atempo=0.75");
        assert_eq!(speed_name(0.5), "0.5x");
        assert_eq!(speed_name(2.0), "2x");
    }
}
