//! Fallible stdout writes (DESIGN §3.2, §10).
//!
//! Rust ignores `SIGPIPE`, so writing to a pipe whose reader went away
//! (`gpufree --json | head -1`) returns `EPIPE`; `println!`/`print!` panic on
//! any write error. All user-facing batch output (JSON, the table, doctor,
//! presets, init, completion summaries) goes through this module instead: the
//! first write error is remembered, later writes become no-ops, and [`finish`]
//! turns the outcome into an exit code — a broken pipe is a normal, quiet
//! exit 0; any other IO error is reported on stderr and exits 1.
//!
//! Deliberately not routed through the sink: the TUI (crossterm draws directly
//! and already treats EIO/EPIPE as the terminal vanishing), `completion
//! __complete` (its BufWriter ignores write errors), and clap's
//! `--help`/`--version` (clap itself ignores write errors).

use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::sync::{Mutex, OnceLock};

/// A writer that remembers (and reports) at most one write failure.
#[derive(Debug)]
pub struct Sink<W: Write> {
    writer: W,
    failure: Option<io::Error>,
}

impl<W: Write> Sink<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            failure: None,
        }
    }

    fn record(&mut self, result: io::Result<()>) {
        if self.failure.is_none()
            && let Err(err) = result
        {
            self.failure = Some(err);
        }
    }

    /// Write text unless an earlier write already failed.
    pub fn write_text(&mut self, text: &str) {
        if self.failure.is_some() {
            return;
        }
        let result = self.writer.write_all(text.as_bytes());
        self.record(result);
    }

    pub fn write_args(&mut self, args: fmt::Arguments<'_>) {
        if self.failure.is_some() {
            return;
        }
        let result = self.writer.write_fmt(args);
        self.record(result);
    }

    /// Flush buffered bytes; a failure here is remembered like any other.
    pub fn finish(&mut self) {
        if self.failure.is_some() {
            return;
        }
        let result = self.writer.flush();
        self.record(result);
    }

    pub fn failure(&self) -> Option<&io::Error> {
        self.failure.as_ref()
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

/// Whether the error means the consumer went away rather than a real write
/// failure. A closed pipe is `BrokenPipe`; a pty whose master disappeared
/// returns `EIO` (`is_terminal_gone` in the TUI makes the same call), which
/// only counts as "gone" when stdout really was a terminal — `EIO` from a
/// regular file is a genuine error.
fn consumer_gone(err: &io::Error, stdout_is_terminal: bool) -> bool {
    err.kind() == io::ErrorKind::BrokenPipe
        || (stdout_is_terminal && err.raw_os_error() == Some(libc::EIO))
}

/// A closed pipe means the consumer stopped reading: quiet exit 0. Any other
/// write error is real and must be reported (exit 1).
pub fn exit_for(code: i32, failure: Option<&io::Error>, stdout_is_terminal: bool) -> i32 {
    match failure {
        None => code,
        Some(err) if consumer_gone(err, stdout_is_terminal) => 0,
        Some(_) => 1,
    }
}

fn sink() -> &'static Mutex<Sink<io::Stdout>> {
    static SINK: OnceLock<Mutex<Sink<io::Stdout>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(Sink::new(io::stdout())))
}

pub fn write_text(text: &str) {
    sink()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .write_text(text);
}

pub fn write_args(args: fmt::Arguments<'_>) {
    sink()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .write_args(args);
}

/// Flush stdout and translate the stream state into the process exit code.
pub fn finish(code: i32) -> i32 {
    let terminal = io::stdout().is_terminal();
    let mut sink = sink().lock().unwrap_or_else(|e| e.into_inner());
    sink.finish();
    let failure = sink.failure();
    if let Some(err) = failure
        && !consumer_gone(err, terminal)
    {
        // Deliberately not `eprintln!`: stderr is often the same vanished
        // terminal, and a panic there would turn a clean exit into 101.
        let _ = writeln!(io::stderr(), "gpufree: failed to write to stdout: {err}");
    }
    exit_for(code, failure, terminal)
}

/// `println!` for stdout that never panics; use for every user-facing line.
/// Internal helper, not part of the public API.
#[doc(hidden)]
#[macro_export]
macro_rules! outln {
    () => {
        $crate::output::stream::write_text("\n")
    };
    ($($arg:tt)*) => {
        $crate::output::stream::write_args(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// `print!` for stdout that never panics. Internal helper, not public API.
#[doc(hidden)]
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::output::stream::write_args(format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Failing {
        kind: io::ErrorKind,
        calls: usize,
    }

    impl Failing {
        fn new(kind: io::ErrorKind) -> Self {
            Self { kind, calls: 0 }
        }
    }

    impl Write for Failing {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            Err(io::Error::new(self.kind, "boom"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FlushFails;

    impl Write for FlushFails {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
    }

    #[test]
    fn broken_pipe_is_recorded_once_and_stops_writing() {
        let mut sink = Sink::new(Failing::new(io::ErrorKind::BrokenPipe));
        sink.write_text("a");
        sink.write_text("b");
        sink.write_args(format_args!("c"));
        assert_eq!(
            sink.failure().map(io::Error::kind),
            Some(io::ErrorKind::BrokenPipe)
        );
        assert_eq!(sink.into_inner().calls, 1, "later writes are no-ops");
    }

    #[test]
    fn success_preserves_bytes_and_exit_code() {
        let mut sink = Sink::new(Vec::new());
        sink.write_text("hello");
        sink.write_args(format_args!(" {}", 1));
        sink.write_text("\n");
        sink.finish();
        assert!(sink.failure().is_none());
        assert_eq!(sink.into_inner(), b"hello 1\n");
    }

    #[test]
    fn flush_failure_is_recorded() {
        let mut sink = Sink::new(FlushFails);
        sink.write_text("data");
        assert!(sink.failure().is_none());
        sink.finish();
        assert_eq!(
            sink.failure().map(io::Error::kind),
            Some(io::ErrorKind::BrokenPipe)
        );
    }

    #[test]
    fn exit_for_classifies_broken_pipe_and_other_errors() {
        let pipe = io::Error::from(io::ErrorKind::BrokenPipe);
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(exit_for(3, None, false), 3, "no error keeps the code");
        assert_eq!(exit_for(3, Some(&pipe), false), 0, "broken pipe exits 0");
        assert_eq!(exit_for(0, Some(&denied), false), 1, "other errors exit 1");
        assert_eq!(exit_for(0, Some(&denied), true), 1, "tty does not hide it");
    }

    #[test]
    fn eio_is_only_quiet_on_a_terminal() {
        // A pty whose master disappeared reports EIO, like a real terminal
        // does; EIO from a file is a genuine error.
        let eio = io::Error::from_raw_os_error(libc::EIO);
        assert_eq!(exit_for(0, Some(&eio), true), 0, "vanished terminal");
        assert_eq!(exit_for(0, Some(&eio), false), 1, "EIO on a file");
    }
}
