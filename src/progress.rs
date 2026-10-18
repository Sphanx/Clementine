//! How far a running job has got, and how to stop it.
//!
//! [`Progress`] is what [`crate::backend::run_with`] reports while it works; [`Cancel`] is the
//! token that stops it. ffmpeg's `-progress` output is parsed here too, by pure functions, so the
//! maths can be tested without running anything.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Where a running op is: which job (one per file, usually), which step of it, and how far
/// that step has got when the program says so.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    /// 0-based index of the running job.
    pub job: usize,
    pub jobs: usize,
    /// 0-based index of the running step within the job; only steps that do work are counted.
    pub step: usize,
    pub steps: usize,
    /// How far the running step has got, `0.0..=1.0`; `None` when the program does not say.
    pub within: Option<f64>,
}

impl Progress {
    /// The whole op's fraction, when the running step reports one. Every job weighs the same,
    /// and so does every step within a job.
    pub fn overall(&self) -> Option<f64> {
        let within = self.within?;
        let jobs = self.jobs.max(1) as f64;
        let steps = self.steps.max(1) as f64;
        let job = (self.step as f64 + within.clamp(0.0, 1.0)) / steps;
        Some(((self.job as f64 + job) / jobs).clamp(0.0, 1.0))
    }

    /// `2 of 5 · 43%`, `43%`, `step 2 of 3` or empty: what the activity row says.
    pub fn label(&self) -> String {
        let count = if self.jobs > 1 {
            Some(format!("{} of {}", self.job + 1, self.jobs))
        } else if self.steps > 1 {
            Some(format!("step {} of {}", self.step + 1, self.steps))
        } else {
            None
        };
        let percent = self.overall().map(|f| format!("{}%", (f * 100.0).floor() as u32));
        match (count, percent) {
            (Some(c), Some(p)) => format!("{c} · {p}"),
            (Some(c), None) => c,
            (None, Some(p)) => p,
            (None, None) => String::new(),
        }
    }
}

/// The error a cancelled run returns; tell it from a failure with [`is_cancelled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Whether `e` means the run was cancelled rather than failed.
pub fn is_cancelled(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Cancelled>().is_some()
}

/// Stops a running op. Clones share the same state, so the UI keeps one and the worker another.
///
/// The worker registers each child's process group as it starts; [`Cancel::cancel`] sends that
/// group SIGTERM at once, and the worker follows up with SIGKILL if it has not gone after
/// [`KILL_AFTER`].
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<Inner>);

#[derive(Debug, Default)]
struct Inner {
    cancelled: AtomicBool,
    /// Process group of the running child; 0 when none.
    group: AtomicI32,
}

/// How long a cancelled child gets to exit on SIGTERM before it is killed.
pub const KILL_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the op to stop: no further step starts, and the running child is sent SIGTERM.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        let group = self.0.group.load(Ordering::SeqCst);
        if group > 0 {
            signal_group(group, libc::SIGTERM);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Records the running child's process group. A cancel that came in just before is
    /// delivered now, so no child slips through.
    pub(crate) fn started(&self, group: i32) {
        self.0.group.store(group, Ordering::SeqCst);
        if self.is_cancelled() {
            signal_group(group, libc::SIGTERM);
        }
    }

    /// The child has been reaped; nothing to signal any more.
    pub(crate) fn finished(&self) {
        self.0.group.store(0, Ordering::SeqCst);
    }
}

/// Sends `signal` to every process in group `group` (the child and anything it spawned, such as
/// LibreOffice's `soffice.bin`). A group that is already gone is not an error.
pub(crate) fn signal_group(group: i32, signal: libc::c_int) {
    if group > 0 {
        // SAFETY: kill(2) takes plain integers; a negative pid addresses a process group.
        unsafe {
            libc::kill(-group, signal);
        }
    }
}

/// One line of ffmpeg's `-progress` output that matters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FfLine {
    /// The output's timestamp so far, in seconds.
    Time(f64),
    /// ffmpeg finished writing.
    End,
}

/// Parses a `key=value` line of `ffmpeg -progress pipe:1`. Only the output time and the final
/// `progress=end` matter; `N/A` (before the first frame) and everything else is `None`.
/// `out_time_ms` is in microseconds too, despite its name (an old ffmpeg quirk).
pub fn parse_ffmpeg_line(line: &str) -> Option<FfLine> {
    let (key, value) = line.trim().split_once('=')?;
    match key.trim() {
        "out_time_us" | "out_time_ms" => {
            let us: i64 = value.trim().parse().ok()?;
            (us >= 0).then(|| FfLine::Time(us as f64 / 1_000_000.0))
        }
        "progress" if value.trim() == "end" => Some(FfLine::End),
        _ => None,
    }
}

/// How far `done` seconds of output is into `total`, `0.0..=1.0`.
pub fn time_fraction(done: f64, total: f64) -> Option<f64> {
    (total.is_finite() && total > 0.0 && done.is_finite()).then(|| (done / total).clamp(0.0, 1.0))
}

/// How long the output of an ffmpeg call with `args` will be, in seconds, given the length of its
/// input from `probe`. Honours a seek before the input (`-ss`), a length (`-t`) and a speed change
/// (`setpts=PTS/f`, or an `atempo` chain for audio). `None` when it cannot tell, or when the call
/// writes single frames.
pub fn ffmpeg_output_seconds(args: &[OsString], probe: impl Fn(&Path) -> Option<f64>) -> Option<f64> {
    let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let value_of = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1));
    if args.iter().any(|a| a == "-frames:v") {
        return None;
    }
    let input_at = args.iter().position(|a| a == "-i")?;
    let input = args.get(input_at + 1)?;
    let start = args[..input_at]
        .iter()
        .position(|a| a == "-ss")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let len = match value_of("-t").and_then(|t| t.parse::<f64>().ok()) {
        Some(t) => t,
        None => probe(Path::new(input))? - start,
    };
    let speed = match args.iter().find_map(|a| a.strip_prefix("setpts=PTS/")) {
        Some(f) => f.parse::<f64>().ok()?,
        None => args
            .iter()
            .filter(|a| a.contains("atempo="))
            .flat_map(|a| a.split(',').filter_map(|s| s.trim().strip_prefix("atempo=")?.parse::<f64>().ok()))
            .product(),
    };
    let out = len / speed;
    (out.is_finite() && out > 0.0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_ffmpeg_progress_lines() {
        assert_eq!(parse_ffmpeg_line("out_time_us=2500000"), Some(FfLine::Time(2.5)));
        assert_eq!(parse_ffmpeg_line("out_time_ms=1000000\n"), Some(FfLine::Time(1.0)));
        assert_eq!(parse_ffmpeg_line("out_time_us=N/A"), None);
        assert_eq!(parse_ffmpeg_line("out_time_us=-9223372036854775807"), None);
        assert_eq!(parse_ffmpeg_line("out_time=00:00:02.500000"), None);
        assert_eq!(parse_ffmpeg_line("progress=continue"), None);
        assert_eq!(parse_ffmpeg_line("progress=end"), Some(FfLine::End));
        assert_eq!(parse_ffmpeg_line("frame=12"), None);
        assert_eq!(parse_ffmpeg_line("garbage"), None);
        assert_eq!(parse_ffmpeg_line(""), None);
    }

    #[test]
    fn time_fractions_are_clamped() {
        assert_eq!(time_fraction(5.0, 10.0), Some(0.5));
        assert_eq!(time_fraction(12.0, 10.0), Some(1.0));
        assert_eq!(time_fraction(1.0, 0.0), None);
        assert_eq!(time_fraction(1.0, f64::NAN), None);
    }

    #[test]
    fn output_length_follows_seek_cut_and_speed() {
        let probe = |_: &Path| Some(10.0);
        let plain = os(&["-hide_banner", "-i", "a.mp4", "-c:v", "libx264", "b.mp4"]);
        assert_eq!(ffmpeg_output_seconds(&plain, probe), Some(10.0));
        let seek = os(&["-ss", "4", "-i", "a.mp4", "b.mp4"]);
        assert_eq!(ffmpeg_output_seconds(&seek, probe), Some(6.0));
        let cut = os(&["-ss", "4", "-i", "a.mp4", "-t", "2.5", "b.mp4"]);
        assert_eq!(ffmpeg_output_seconds(&cut, probe), Some(2.5));
        let video = os(&["-i", "a.mp4", "-filter:v", "setpts=PTS/2", "-filter:a", "atempo=2", "b.mp4"]);
        assert_eq!(ffmpeg_output_seconds(&video, probe), Some(5.0));
        let audio = os(&["-i", "a.wav", "-filter:a", "atempo=0.5,atempo=0.5", "b.wav"]);
        assert_eq!(ffmpeg_output_seconds(&audio, probe), Some(40.0));
        let still = os(&["-ss", "1", "-i", "a.mp4", "-frames:v", "1", "b.jpg"]);
        assert_eq!(ffmpeg_output_seconds(&still, probe), None);
        assert_eq!(ffmpeg_output_seconds(&plain, |_: &Path| None), None);
    }

    #[test]
    fn overall_fraction_weighs_jobs_and_steps_evenly() {
        let p = |job, jobs, step, steps, within| Progress { job, jobs, step, steps, within };
        assert_eq!(p(0, 1, 0, 1, Some(0.5)).overall(), Some(0.5));
        assert_eq!(p(1, 4, 0, 1, Some(0.0)).overall(), Some(0.25));
        assert_eq!(p(1, 2, 1, 2, Some(0.5)).overall(), Some(0.875));
        assert_eq!(p(3, 4, 0, 1, Some(1.0)).overall(), Some(1.0));
        assert_eq!(p(0, 1, 0, 1, Some(7.0)).overall(), Some(1.0));
        assert_eq!(p(0, 2, 0, 3, None).overall(), None);
        // A job with no work steps must not divide by zero.
        assert_eq!(p(0, 0, 0, 0, Some(0.5)).overall(), Some(0.5));
    }

    #[test]
    fn labels_say_where_the_op_is() {
        let p = |job, jobs, step, steps, within| Progress { job, jobs, step, steps, within };
        assert_eq!(p(1, 5, 0, 1, Some(0.5)).label(), "2 of 5 · 30%");
        assert_eq!(p(0, 1, 0, 1, Some(0.437)).label(), "43%");
        assert_eq!(p(0, 1, 1, 3, None).label(), "step 2 of 3");
        assert_eq!(p(2, 3, 0, 2, None).label(), "3 of 3");
        assert_eq!(p(0, 1, 0, 1, None).label(), "");
    }

    #[test]
    fn cancel_is_shared_between_clones() {
        let c = Cancel::new();
        let worker = c.clone();
        assert!(!worker.is_cancelled());
        c.cancel();
        assert!(worker.is_cancelled());
        assert!(is_cancelled(&anyhow::Error::new(Cancelled)));
        assert!(!is_cancelled(&anyhow::anyhow!("ffmpeg failed")));
    }

    #[test]
    fn cancel_signals_a_child_registered_after_it() {
        use std::os::unix::process::CommandExt;
        let c = Cancel::new();
        c.cancel();
        let mut child = std::process::Command::new("sleep").arg("30").process_group(0).spawn().unwrap();
        let started = std::time::Instant::now();
        c.started(child.id() as i32);
        let status = child.wait().unwrap();
        c.finished();
        assert!(!status.success());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
