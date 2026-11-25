//! Regression: the TUI must exit when its pty disappears without SIGHUP.
//!
//! Closing the pty master leaves the slave permanently ready with `POLLHUP`
//! and no bytes. crossterm reports "no event" forever, and before the
//! `fd_hung_up` check the event loop spun at 100% CPU and leaked the process
//! (observed with tmux sessions and review harnesses that closed the pty but
//! did not signal the child).

#![cfg(unix)]

use std::ffi::CStr;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn open_pty() -> (OwnedFd, OwnedFd) {
    // SAFETY: the pty helpers take and return plain fds; every returned fd is
    // wrapped in `OwnedFd` exactly once and closed on drop.
    unsafe {
        let master_fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master_fd >= 0, "posix_openpt failed");
        // The child must not inherit the master, otherwise the pty never
        // hangs up and the test would measure the wrong thing.
        assert_eq!(libc::fcntl(master_fd, libc::F_SETFD, libc::FD_CLOEXEC), 0);
        let master = OwnedFd::from_raw_fd(master_fd);
        assert_eq!(libc::grantpt(master_fd), 0, "grantpt failed");
        assert_eq!(libc::unlockpt(master_fd), 0, "unlockpt failed");

        let name = CStr::from_ptr(libc::ptsname(master_fd));
        let slave_fd = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        assert!(slave_fd >= 0, "opening pty slave failed");
        let slave = OwnedFd::from_raw_fd(slave_fd);

        let winsize = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            libc::ioctl(master_fd, libc::TIOCSWINSZ, &winsize),
            0,
            "TIOCSWINSZ failed"
        );
        (master, slave)
    }
}

fn spawn_tui_on(slave: OwnedFd, config: &std::path::Path) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gpufree"));
    cmd.arg("--config")
        .arg(config)
        .arg("--host")
        .arg("a")
        .stdin(Stdio::from(slave.try_clone().expect("clone pty slave")))
        .stdout(Stdio::from(slave.try_clone().expect("clone pty slave")))
        .stderr(Stdio::from(slave));
    // `setsid` keeps the pty from becoming the child's controlling terminal,
    // so closing the master does not deliver SIGHUP. This is the exact case
    // that used to spin forever; SIGHUP-based cleanup is tested elsewhere.
    // SAFETY: `setsid` is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().expect("spawn gpufree")
}

#[test]
fn tui_exits_when_pty_master_disappears() {
    let config = std::env::temp_dir().join(format!("gpufree-hangup-{}.toml", std::process::id()));
    std::fs::write(
        &config,
        "[settings]\nssh_binary = \"/usr/bin/false\"\nparallel = 1\nparallel_remote = 1\n\
         connect_timeout = \"1s\"\ntotal_timeout = \"2s\"\n",
    )
    .expect("write temp config");

    let (master, slave) = open_pty();
    let mut child = spawn_tui_on(slave, &config);

    // Give the TUI time to enter raw mode and draw its first frame.
    std::thread::sleep(Duration::from_millis(1000));
    if let Some(status) = child.try_wait().expect("poll child") {
        panic!("TUI exited before the pty was closed: {status:?}");
    }

    // Closing the master is what a vanished terminal does.
    drop(master);

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("gpufree did not exit within 5s of the pty master closing");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let _ = std::fs::remove_file(&config);
    assert_eq!(
        status.code(),
        Some(0),
        "expected a clean exit after terminal loss, got {status:?}"
    );
}
