//! Progress for long phases, readable by people and by agents.
//!
//! On a terminal it is an indicatif bar (or a spinner when the total is
//! unknown). Anywhere else, such as a pipe read by an agent, it prints plain
//! lines to stderr: one per tenth of a known total, or one every 15 seconds
//! when the total is unknown, and a final line. Messages printed through it
//! never tear the bar.

use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How often a plain-lines progress with no known total reports.
const UNKNOWN_TOTAL_EVERY: Duration = Duration::from_secs(15);
/// Plain-lines progress stays quiet for phases shorter than this; the
/// summary line still prints.
const QUIET_START: Duration = Duration::from_secs(2);

enum Mode {
    Bar(ProgressBar),
    Lines,
    Quiet,
}

pub struct Progress {
    mode: Mode,
    label: String,
    total: Option<u64>,
    done: AtomicU64,
    started: Instant,
    /// Lines mode: the last tenth reported, and when the last line went out.
    last_tenth: AtomicU64,
    last_line: Mutex<Instant>,
    message: Mutex<String>,
}

impl Progress {
    /// `total` is the number of steps when known. `quiet` prints nothing but
    /// the messages passed to [`Progress::println`].
    pub fn new(label: &str, total: Option<u64>, quiet: bool) -> Self {
        let mode = if quiet {
            Mode::Quiet
        } else if std::io::stderr().is_terminal() {
            let bar = match total {
                Some(n) => {
                    let bar = ProgressBar::new(n);
                    bar.set_style(
                        ProgressStyle::with_template(
                            "{prefix} {bar:40.cyan/blue} {pos}/{len} {msg} elapsed {elapsed_precise} eta {eta}",
                        )
                        .unwrap_or_else(|_| ProgressStyle::default_bar()),
                    );
                    bar
                }
                None => {
                    let bar = ProgressBar::new_spinner();
                    bar.set_style(
                        ProgressStyle::with_template("{prefix} {spinner} {pos} {msg} {elapsed}")
                            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                    );
                    bar.enable_steady_tick(Duration::from_millis(200));
                    bar
                }
            };
            bar.set_prefix(label.to_string());
            Mode::Bar(bar)
        } else {
            Mode::Lines
        };
        let now = Instant::now();
        Self {
            mode,
            label: label.to_string(),
            total,
            done: AtomicU64::new(0),
            started: now,
            last_tenth: AtomicU64::new(0),
            last_line: Mutex::new(now),
            message: Mutex::new(String::new()),
        }
    }

    pub fn inc(&self, n: u64) {
        let done = self.done.fetch_add(n, Ordering::SeqCst) + n;
        match &self.mode {
            Mode::Bar(bar) => bar.inc(n),
            Mode::Quiet => {}
            Mode::Lines => {
                let due = match self.total {
                    Some(total) if total > 0 => {
                        let tenth = done * 10 / total;
                        tenth > self.last_tenth.fetch_max(tenth, Ordering::SeqCst) && done < total
                    }
                    _ => {
                        let mut last = self.last_line.lock().unwrap_or_else(|e| e.into_inner());
                        if last.elapsed() >= UNKNOWN_TOTAL_EVERY {
                            *last = Instant::now();
                            true
                        } else {
                            false
                        }
                    }
                };
                if due && self.started.elapsed() >= QUIET_START {
                    eprintln!("{}", self.status_line(done));
                }
            }
        }
    }

    /// A short note on what is happening now, such as the crate compiling.
    pub fn set_message(&self, msg: &str) {
        if let Mode::Bar(bar) = &self.mode {
            bar.set_message(msg.to_string());
        }
        *self.message.lock().unwrap_or_else(|e| e.into_inner()) = msg.to_string();
    }

    /// A line of output that must not tear the bar.
    pub fn println(&self, line: &str) {
        match &self.mode {
            Mode::Bar(bar) => bar.println(line),
            Mode::Lines | Mode::Quiet => eprintln!("{line}"),
        }
    }

    /// End the progress with a summary line.
    pub fn finish(&self, summary: &str) {
        match &self.mode {
            Mode::Bar(bar) => {
                bar.finish_and_clear();
                eprintln!("{summary}");
            }
            Mode::Lines => eprintln!("{summary}"),
            Mode::Quiet => {}
        }
    }

    pub fn done(&self) -> u64 {
        self.done.load(Ordering::SeqCst)
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn status_line(&self, done: u64) -> String {
        let secs = self.started.elapsed().as_secs();
        let msg = self
            .message
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let msg = if msg.is_empty() {
            String::new()
        } else {
            format!(", {msg}")
        };
        match self.total {
            Some(total) => format!(
                "{}: {done}/{total} ({}%, {secs}s){msg}",
                self.label,
                done * 100 / total.max(1)
            ),
            None => format!("{}: {done} so far ({secs}s){msg}", self.label),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_mode_reports_each_tenth_once() {
        let p = Progress {
            mode: Mode::Lines,
            label: "recording".into(),
            total: Some(20),
            done: AtomicU64::new(0),
            started: Instant::now(),
            last_tenth: AtomicU64::new(0),
            last_line: Mutex::new(Instant::now()),
            message: Mutex::new(String::new()),
        };
        let mut due = 0;
        for _ in 0..20 {
            let before = p.last_tenth.load(Ordering::SeqCst);
            p.inc(1);
            if p.last_tenth.load(Ordering::SeqCst) > before && p.done() < 20 {
                due += 1;
            }
        }
        // Tenths 1 through 9 each report once; the end is the summary's job.
        assert_eq!(due, 9);
        assert_eq!(p.status_line(10), "recording: 10/20 (50%, 0s)");
    }
}
