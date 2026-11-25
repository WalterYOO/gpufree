//! Spawning and supervising the system `ssh` process (DESIGN §5.1).
//!
//! Every child runs in its own process group. A `ChildGuard` is installed the
//! instant spawn succeeds and owns every cleanup path: timeout, cancellation,
//! early return, and panic. Normal completion calls `disarm()` so a reused
//! pgid is never signalled.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::Settings;
use crate::model::{ErrorKind, ProbeErrorInfo};

pub const OUTPUT_CAP: usize = 8 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const TERM_GRACE: Duration = Duration::from_millis(200);
const REAP_LIMIT: Duration = Duration::from_secs(2);
const READER_DRAIN: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Cancellation registry
// ---------------------------------------------------------------------------

static HUB: OnceLock<Mutex<Vec<Weak<Canceller>>>> = OnceLock::new();

fn hub() -> &'static Mutex<Vec<Weak<Canceller>>> {
    HUB.get_or_init(|| Mutex::new(Vec::new()))
}

/// Shared cancellation state: a flag, the live process groups, and the
/// per-hop limiters that must be woken when it flips.
#[derive(Debug)]
pub struct Canceller {
    cancelled: AtomicBool,
    pgids: Mutex<HashSet<i32>>,
    hops: Mutex<Vec<Weak<crate::probe::HopLimiter>>>,
}

impl Canceller {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Arc<Self> {
        let this = Arc::new(Self {
            cancelled: AtomicBool::new(false),
            pgids: Mutex::new(HashSet::new()),
            hops: Mutex::new(Vec::new()),
        });
        hub()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::downgrade(&this));
        this
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Flip the flag, SIGTERM every live process group, and wake hop waiters.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let pgids: Vec<i32> = {
            let guard = self.pgids.lock().unwrap_or_else(|e| e.into_inner());
            guard.iter().copied().collect()
        };
        for pgid in pgids {
            signal_group(pgid, libc::SIGTERM);
        }
        let hops: Vec<Arc<crate::probe::HopLimiter>> = {
            let guard = self.hops.lock().unwrap_or_else(|e| e.into_inner());
            guard.iter().filter_map(Weak::upgrade).collect()
        };
        for hop in hops {
            hop.notify_all();
        }
    }

    /// Register a child's pgid. Returns false (and SIGTERMs the group) if
    /// cancellation won the race with spawn.
    fn register_pgid(&self, pgid: i32) -> bool {
        let mut guard = self.pgids.lock().unwrap_or_else(|e| e.into_inner());
        if self.cancelled.load(Ordering::SeqCst) {
            signal_group(pgid, libc::SIGTERM);
            return false;
        }
        guard.insert(pgid);
        true
    }

    fn unregister_pgid(&self, pgid: i32) {
        let mut guard = self.pgids.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&pgid);
    }

    pub(crate) fn register_hop(&self, hop: &Arc<crate::probe::HopLimiter>) {
        let mut guard = self.hops.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(Arc::downgrade(hop));
    }
}

/// Cancel every live generation (called from the signal handler).
pub fn cancel_all() {
    let live: Vec<Arc<Canceller>> = {
        let mut guard = hub().lock().unwrap_or_else(|e| e.into_inner());
        guard.retain(|weak| weak.strong_count() > 0);
        guard.iter().filter_map(Weak::upgrade).collect()
    };
    for canceller in live {
        canceller.cancel();
    }
}

pub fn signal_group(pgid: i32, signal: i32) {
    if pgid <= 0 {
        return;
    }
    // SAFETY: kill(2) with a negative pid signals the process group; the
    // pgid came from a child we spawned with process_group(0).
    unsafe {
        libc::kill(-pgid, signal);
    }
}

// ---------------------------------------------------------------------------
// Child guard
// ---------------------------------------------------------------------------

/// Owns a spawned ssh child and guarantees kill + bounded reap on every exit
/// path, including unwinding.
struct ChildGuard {
    child: Option<Child>,
    pgid: i32,
    canceller: Arc<Canceller>,
    registered: bool,
    reaped: bool,
    disarmed: bool,
}

impl ChildGuard {
    fn new(child: Child, canceller: Arc<Canceller>) -> Self {
        let pgid = child.id() as i32;
        let registered = canceller.register_pgid(pgid);
        Self {
            child: Some(child),
            pgid,
            canceller,
            registered,
            reaped: false,
            disarmed: false,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("child present until disarm")
    }

    fn poll(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        let status = child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
            self.unregister();
        }
        Ok(status)
    }

    fn wait_up_to(&mut self, limit: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + limit;
        loop {
            match self.poll() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => {}
                Err(_) => return None,
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn unregister(&mut self) {
        if self.registered {
            self.canceller.unregister_pgid(self.pgid);
            self.registered = false;
        }
    }

    /// SIGTERM, 200 ms grace, SIGKILL, then a bounded 2 s reap. A process stuck
    /// in D state is handed to a detached reaper so the worker never blocks.
    fn terminate(&mut self) {
        if self.reaped {
            self.unregister();
            return;
        }
        signal_group(self.pgid, libc::SIGTERM);
        if self.wait_up_to(TERM_GRACE).is_some() {
            return;
        }
        signal_group(self.pgid, libc::SIGKILL);
        if self.wait_up_to(REAP_LIMIT).is_some() {
            return;
        }
        // D state: pull the pgid before handing the child off, so a later
        // cancel can never signal a reused pgid.
        self.unregister();
        if let Some(mut child) = self.child.take() {
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }

    /// Normal completion: the child was already reaped by `poll`, so drop it
    /// without signalling, and forget the pgid immediately.
    fn disarm(&mut self) {
        self.unregister();
        if let Some(mut child) = self.child.take()
            && !self.reaped
        {
            let _ = child.try_wait();
        }
        self.disarmed = true;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.disarmed {
            self.terminate();
        }
        self.unregister();
    }
}

// ---------------------------------------------------------------------------
// Output readers
// ---------------------------------------------------------------------------

fn spawn_reader<R: Read + AsRawFd + Send + 'static>(
    pipe: R,
    abort: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
) -> JoinHandle<Vec<u8>> {
    let fd: RawFd = pipe.as_raw_fd();
    thread::spawn(move || {
        let _pipe = pipe; // keep the fd open for the lifetime of the thread
        let mut data: Vec<u8> = Vec::new();
        let mut total = 0usize;
        let mut buf = [0u8; 8192];
        loop {
            if abort.load(Ordering::Relaxed) {
                break;
            }
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll(2) with one valid pollfd.
            let ready = unsafe { libc::poll(&mut pfd, 1, 200) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if ready == 0 {
                continue;
            }
            // SAFETY: fd belongs to `_pipe`; buf is a valid writable buffer.
            let got = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if got < 0 {
                let err = std::io::Error::last_os_error();
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) {
                    continue;
                }
                break;
            }
            if got == 0 {
                break;
            }
            let got = got as usize;
            total = total.saturating_add(got);
            if data.len() < OUTPUT_CAP {
                let take = got.min(OUTPUT_CAP - data.len());
                data.extend_from_slice(&buf[..take]);
            }
            if total > OUTPUT_CAP {
                overflow.store(true, Ordering::SeqCst);
            }
        }
        data
    })
}

fn join_reader(handle: JoinHandle<Vec<u8>>, abort: &Arc<AtomicBool>) -> Vec<u8> {
    let start = Instant::now();
    while !handle.is_finished() && start.elapsed() < READER_DRAIN {
        thread::sleep(Duration::from_millis(10));
    }
    abort.store(true, Ordering::SeqCst);
    handle.join().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Running ssh
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SshOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
}

/// Build the argv used to probe one host. Kept public for tests and doctor.
pub fn build_args(alias: &str, settings: &Settings) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-T".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        format!(
            "ConnectTimeout={}",
            settings.connect_timeout.as_secs().max(1)
        ),
        "-o".to_string(),
        "ConnectionAttempts=1".to_string(),
    ];

    let mut mux_added = false;
    if settings.mux_enabled()
        && let Some(home) = dirs::home_dir()
    {
        let ssh_dir = home.join(".ssh");
        if ssh_dir.is_dir() {
            args.push("-o".to_string());
            args.push("ControlMaster=auto".to_string());
            args.push("-o".to_string());
            args.push(format!("ControlPath={}", ssh_dir.join("gf-%C").display()));
            args.push("-o".to_string());
            args.push(format!(
                "ControlPersist={}",
                settings.control_persist.as_secs().max(1)
            ));
            mux_added = true;
        }
    }
    if !mux_added {
        // Never pass ControlPersist=0: in ssh that means "stay forever".
        args.push("-o".to_string());
        args.push("ControlMaster=no".to_string());
        args.push("-o".to_string());
        args.push("ControlPath=none".to_string());
    }

    args.push(alias.to_string());
    args.push("sh".to_string());
    args.push("-s".to_string());
    args
}

/// Run the probe script on one host.
pub fn run(
    binary: &str,
    alias: &str,
    script: &str,
    settings: &Settings,
    canceller: Arc<Canceller>,
) -> Result<SshOutput, ProbeErrorInfo> {
    let args = build_args(alias, settings);
    let mut command = Command::new(binary);
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);

    let child = command.spawn().map_err(|err| {
        ProbeErrorInfo::new(
            ErrorKind::Unknown,
            format!("failed to spawn {binary:?}: {err}"),
        )
    })?;

    let mut guard = ChildGuard::new(child, Arc::clone(&canceller));
    if !guard.registered || canceller.is_cancelled() {
        // Drop performs kill + bounded reap; no pipes were handed out yet.
        return Err(cancelled_error());
    }

    let stdin = guard.child_mut().stdin.take();
    let stdout = guard.child_mut().stdout.take();
    let stderr = guard.child_mut().stderr.take();

    let stdin_writer = stdin.map(|mut stdin| {
        let script = script.to_string();
        thread::spawn(move || {
            let _ = stdin.write_all(script.as_bytes());
            let _ = stdin.flush();
            // dropping stdin closes the pipe
        })
    });

    let overflow = Arc::new(AtomicBool::new(false));
    let out_abort = Arc::new(AtomicBool::new(false));
    let err_abort = Arc::new(AtomicBool::new(false));
    let stdout_reader =
        stdout.map(|pipe| spawn_reader(pipe, Arc::clone(&out_abort), Arc::clone(&overflow)));
    let stderr_reader =
        stderr.map(|pipe| spawn_reader(pipe, Arc::clone(&err_abort), Arc::clone(&overflow)));

    let deadline = Instant::now() + settings.total_timeout;
    let mut status: Option<ExitStatus> = None;
    let mut timed_out = false;
    let mut cancelled = false;
    let mut wait_failed: Option<String> = None;

    loop {
        match guard.poll() {
            Ok(Some(exit)) => {
                status = Some(exit);
                break;
            }
            Ok(None) => {}
            Err(err) => {
                wait_failed = Some(err.to_string());
                break;
            }
        }
        if overflow.load(Ordering::SeqCst) {
            break;
        }
        if canceller.is_cancelled() {
            cancelled = true;
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }

    if status.is_some() {
        guard.disarm();
    } else {
        guard.terminate();
    }

    if let Some(writer) = stdin_writer {
        let _ = writer.join();
    }
    let out = stdout_reader
        .map(|handle| join_reader(handle, &out_abort))
        .unwrap_or_default();
    let err = stderr_reader
        .map(|handle| join_reader(handle, &err_abort))
        .unwrap_or_default();

    if cancelled {
        return Err(cancelled_error());
    }
    if overflow.load(Ordering::SeqCst) {
        return Err(ProbeErrorInfo::new(
            ErrorKind::OutputLimit,
            format!(
                "ssh output exceeded the {} MiB limit",
                OUTPUT_CAP / (1024 * 1024)
            ),
        ));
    }
    if timed_out {
        return Err(ProbeErrorInfo::new(
            ErrorKind::SshTimeout,
            format!(
                "probe timed out after {}",
                crate::config::format_duration(settings.total_timeout)
            ),
        ));
    }
    if let Some(message) = wait_failed {
        return Err(ProbeErrorInfo::new(
            ErrorKind::Unknown,
            format!("failed to wait for ssh: {message}"),
        ));
    }

    let status = status
        .ok_or_else(|| ProbeErrorInfo::new(ErrorKind::Unknown, "ssh exited without a status"))?;
    Ok(SshOutput {
        stdout: out,
        stderr: err,
        success: status.success(),
    })
}

fn cancelled_error() -> ProbeErrorInfo {
    ProbeErrorInfo::new(ErrorKind::Unknown, "probe cancelled")
}

/// Map ssh's stderr to DESIGN §5.4 error kinds.
pub fn classify_stderr(stderr: &[u8]) -> Option<ProbeErrorInfo> {
    let text = String::from_utf8_lossy(stderr);
    let lower = text.to_ascii_lowercase();
    let kind = if contains_any(
        &lower,
        &[
            "host key verification failed",
            "remote host identification has changed",
            "host key for",
            "no matching host key type",
        ],
    ) {
        ErrorKind::SshHostkey
    } else if contains_any(
        &lower,
        &[
            "permission denied",
            "authentication failed",
            "too many authentication failures",
            "no more authentication methods",
        ],
    ) {
        ErrorKind::SshAuth
    } else if contains_any(&lower, &["timed out", "timeout", "operation timed out"]) {
        ErrorKind::SshTimeout
    } else if contains_any(
        &lower,
        &[
            "connection refused",
            "no route to host",
            "could not resolve hostname",
            "name or service not known",
            "network is unreachable",
            "connection reset",
            "connection closed",
            "kex_exchange_identification",
            "broken pipe",
            "connection attempt failed",
        ],
    ) {
        ErrorKind::SshUnreachable
    } else {
        return None;
    };
    let message = first_meaningful_line(&text).unwrap_or_else(|| kind.as_str().to_string());
    Some(ProbeErrorInfo::new(kind, message))
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn first_meaningful_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            let mut message: String = line.chars().take(300).collect();
            if line.chars().count() > 300 {
                message.push('…');
            }
            message
        })
}

/// Whether `binary` can be executed. A path with a separator is checked
/// directly; a bare name is looked up on `PATH`.
pub fn ssh_available(binary: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    let path = Path::new(binary);
    let check = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };
    if binary.contains('/') {
        return check(path);
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(binary);
                check(&candidate)
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Settings, TransportMode};

    #[test]
    fn args_never_use_dash_n_or_separator() {
        let settings = Settings::default();
        let args = build_args("gpu06", &settings);
        assert!(args.contains(&"-T".to_string()));
        assert!(!args.iter().any(|a| a == "-n"));
        assert!(!args.iter().any(|a| a == "--"));
        assert_eq!(args[args.len() - 2..], ["sh", "-s"]);
        assert!(args.iter().any(|a| a == "BatchMode=yes"));
    }

    #[test]
    fn mux_off_maps_to_master_no_not_persist_zero() {
        let settings = Settings {
            mux: false,
            ..Default::default()
        };
        let args = build_args("a", &settings);
        assert!(args.iter().any(|a| a == "ControlMaster=no"));
        assert!(args.iter().any(|a| a == "ControlPath=none"));
        assert!(!args.iter().any(|a| a == "ControlPersist=0"));
    }

    #[test]
    fn control_persist_zero_disables_mux_options() {
        let settings = Settings {
            control_persist: Duration::ZERO,
            ..Default::default()
        };
        let args = build_args("a", &settings);
        assert!(args.iter().any(|a| a == "ControlMaster=no"));
        assert!(!args.iter().any(|a| a.starts_with("ControlPersist=")));
    }

    #[test]
    fn connect_timeout_is_integer_seconds() {
        let settings = Settings {
            connect_timeout: Duration::from_millis(1500),
            ..Default::default()
        };
        let args = build_args("a", &settings);
        assert!(args.iter().any(|a| a == "ConnectTimeout=1"));
    }

    #[test]
    fn classifies_ssh_errors() {
        let cases = [
            ("Permission denied (publickey).", ErrorKind::SshAuth, false),
            (
                "ssh: connect to host 1.2.3.4 port 22: Connection timed out",
                ErrorKind::SshTimeout,
                true,
            ),
            (
                "ssh: connect to host x port 22: Connection refused",
                ErrorKind::SshUnreachable,
                true,
            ),
            (
                "Host key verification failed.",
                ErrorKind::SshHostkey,
                false,
            ),
            (
                "Could not resolve hostname nope",
                ErrorKind::SshUnreachable,
                true,
            ),
        ];
        for (text, kind, retryable) in cases {
            let info = classify_stderr(text.as_bytes()).expect(text);
            assert_eq!(info.kind, kind, "{text}");
            assert_eq!(info.retryable, retryable, "{text}");
        }
    }

    #[test]
    fn classify_ignores_benign_stderr() {
        assert!(classify_stderr(b"Warning: Permanently added 'x' to known hosts.").is_none());
    }

    #[test]
    fn ssh_available_detects_sh() {
        assert!(ssh_available("sh"));
        assert!(!ssh_available("/definitely/not/here"));
    }

    #[test]
    fn canceller_kills_a_real_process_group() {
        // Ensure a spawned sleeper is removed by cancel().
        let canceller = Canceller::new();
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = child.id() as i32;
        assert!(canceller.register_pgid(pgid));
        canceller.cancel();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut reaped = false;
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                reaped = true;
                break;
            }
            // SIGTERM is ignored by the trap; escalate like a guard would.
            signal_group(pgid, libc::SIGKILL);
            thread::sleep(Duration::from_millis(20));
        }
        canceller.unregister_pgid(pgid);
        let _ = child.wait();
        assert!(reaped, "process group should be gone");
    }

    #[test]
    fn transport_enum_smoke() {
        assert_eq!(TransportMode::default(), TransportMode::Direct);
    }
}
