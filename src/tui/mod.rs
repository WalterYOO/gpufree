//! TUI state machine and event loop (DESIGN §8).
//!
//! Rendering lives in [`ui`]; this module owns selection, filtering, sorting,
//! the detail page, spinner state, and generation-based refresh/cancel. The
//! pure parts are unit-tested without a terminal.

pub mod filter;
pub mod ui;

use std::time::Duration;

#[cfg(unix)]
use std::os::fd::AsRawFd;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::config::TransportMode;
use crate::model::HostResult;
use crate::probe::{HostMeta, RunHandle};
use filter::{SortMode, StatusFilter, tokenize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Gpu { host: usize, gpu: usize },
    Error { host: usize },
    NoGpu { host: usize },
    Pending { host: usize },
}

impl RowKind {
    pub fn host(&self) -> usize {
        match *self {
            RowKind::Gpu { host, .. }
            | RowKind::Error { host }
            | RowKind::NoGpu { host }
            | RowKind::Pending { host } => host,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListRow {
    pub host: usize,
    pub kind: RowKind,
    pub group_start: bool,
    pub label: String,
    pub free_total: Option<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub meta: HostMeta,
    pub result: Option<HostResult>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Help,
    Detail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetailView {
    pub host: usize,
    pub gpu: Option<usize>,
    pub scroll: usize,
}

pub type SpawnFn = Box<dyn Fn() -> RunHandle>;

pub struct App {
    pub entries: Vec<Entry>,
    pub mode: Mode,
    pub query: String,
    pub status: StatusFilter,
    pub sort: SortMode,
    pub selected: usize,
    pub offset: usize,
    pub detail: Option<DetailView>,
    pub free_only: bool,
    pub color: bool,
    pub preset: Option<String>,
    pub transport: TransportMode,
    pub quit: bool,
    pub frame: u64,
    pub generation: u64,
    handle: Option<RunHandle>,
    spawner: Option<SpawnFn>,
}

impl App {
    pub fn new(
        metas: Vec<HostMeta>,
        preset: Option<String>,
        transport: TransportMode,
        free_only: bool,
        color: bool,
        spawner: Option<SpawnFn>,
    ) -> Self {
        Self {
            entries: metas
                .into_iter()
                .map(|meta| Entry { meta, result: None })
                .collect(),
            mode: Mode::Normal,
            query: String::new(),
            status: StatusFilter::All,
            sort: SortMode::Host,
            selected: 0,
            offset: 0,
            detail: None,
            free_only,
            color,
            preset,
            transport,
            quit: false,
            frame: 0,
            generation: 0,
            handle: None,
            spawner,
        }
    }

    /// Start a new probe generation, cancelling the previous one. Stale
    /// results are unreachable because the old receiver is dropped.
    pub fn refresh(&mut self) {
        if let Some(handle) = &self.handle {
            handle.canceller.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
        for entry in &mut self.entries {
            entry.result = None;
        }
        self.handle = self.spawner.as_ref().map(|spawn| spawn());
        self.mode = Mode::Normal;
        self.detail = None;
        self.selected = 0;
        self.offset = 0;
    }

    pub fn cancel_current(&mut self) {
        if let Some(handle) = &self.handle {
            handle.canceller.cancel();
        }
    }

    pub fn quit(&mut self) {
        self.cancel_current();
        self.quit = true;
    }

    pub fn drain(&mut self) {
        let mut incoming = Vec::new();
        if let Some(handle) = &self.handle {
            while let Ok(item) = handle.rx.try_recv() {
                incoming.push(item);
            }
        }
        for (index, result) in incoming {
            if let Some(entry) = self.entries.get_mut(index)
                && entry.result.is_none()
            {
                entry.result = Some(result);
            }
        }
    }

    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn done_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.result.is_some())
            .count()
    }

    pub fn total_free(&self) -> usize {
        self.entries
            .iter()
            .filter_map(|entry| entry.result.as_ref())
            .map(HostResult::free_count)
            .sum()
    }

    pub fn free_total(&self, host: usize) -> Option<(usize, usize)> {
        let result = self.entries.get(host)?.result.as_ref()?;
        if !result.is_ok() {
            return None;
        }
        Some((result.free_count(), result.gpus.len()))
    }

    fn tokens(&self) -> Vec<String> {
        tokenize(&self.query)
    }

    fn host_matches(&self, host: usize, tokens: &[String], with_error: bool) -> bool {
        if tokens.is_empty() {
            return true;
        }
        let entry = &self.entries[host];
        let mut fields = vec![entry.meta.host.as_str()];
        if with_error && let Some(error) = entry.result.as_ref().and_then(|r| r.error.as_ref()) {
            fields.push(error.kind.as_str());
            fields.push(error.message.as_str());
        }
        filter::matches(tokens, &fields)
    }

    fn gpu_matches(&self, host: usize, gpu: usize, tokens: &[String]) -> bool {
        if tokens.is_empty() {
            return true;
        }
        let entry = &self.entries[host];
        let Some(result) = &entry.result else {
            return false;
        };
        let Some(gpu) = result.gpus.get(gpu) else {
            return false;
        };
        let users = result.gpu_users(gpu);
        let mut fields = vec![entry.meta.host.as_str()];
        if let Some(name) = &gpu.name {
            fields.push(name.as_str());
        }
        for user in &users {
            fields.push(user.as_str());
        }
        if let Some(processes) = &gpu.processes {
            for process in processes {
                fields.push(process.process_name.as_str());
            }
        }
        filter::matches(tokens, &fields)
    }

    /// Row kinds for the current data set. With `apply_filters == false` only
    /// `free_only` is applied, which is the `total` side of the counter
    /// (DESIGN §8.3: error/no_gpu/pending rows included).
    fn row_kinds(&self, apply_filters: bool) -> Vec<RowKind> {
        let tokens = if apply_filters {
            self.tokens()
        } else {
            Vec::new()
        };
        let mut rows: Vec<RowKind> = Vec::new();

        for (host, entry) in self.entries.iter().enumerate() {
            if self.free_only
                && let Some(result) = &entry.result
                && !(result.is_ok() && result.free_count() > 0)
            {
                continue;
            }
            match &entry.result {
                None => {
                    if apply_filters
                        && (self.status != StatusFilter::All
                            || !self.host_matches(host, &tokens, false))
                    {
                        continue;
                    }
                    rows.push(RowKind::Pending { host });
                }
                Some(result) if !result.is_ok() => {
                    if apply_filters
                        && (!matches!(self.status, StatusFilter::All | StatusFilter::Error)
                            || !self.host_matches(host, &tokens, true))
                    {
                        continue;
                    }
                    rows.push(RowKind::Error { host });
                }
                Some(result) if result.gpus.is_empty() => {
                    if apply_filters
                        && (self.status != StatusFilter::All
                            || !self.host_matches(host, &tokens, false))
                    {
                        continue;
                    }
                    rows.push(RowKind::NoGpu { host });
                }
                Some(result) => {
                    for (gpu, info) in result.gpus.iter().enumerate() {
                        if apply_filters
                            && (!self.status.matches_state(info.state)
                                || !self.gpu_matches(host, gpu, &tokens))
                        {
                            continue;
                        }
                        rows.push(RowKind::Gpu { host, gpu });
                    }
                }
            }
        }
        rows
    }

    /// Rows after text and status filters (the `matched` side of the counter).
    pub fn matched_rows(&self) -> usize {
        self.row_kinds(true).len()
    }

    /// Rows before text/status filters but after `free_only` (the `total`).
    pub fn total_rows(&self) -> usize {
        self.row_kinds(false).len()
    }

    /// Build the rows for the current filter/sort, with HOST group labels.
    pub fn visible_rows(&self) -> Vec<ListRow> {
        let mut rows: Vec<ListRow> = self
            .row_kinds(true)
            .into_iter()
            .map(|kind| {
                let host = kind.host();
                self.row(host, kind)
            })
            .collect();

        if self.sort == SortMode::FreeMem {
            let (mut gpus, others): (Vec<ListRow>, Vec<ListRow>) = rows
                .into_iter()
                .partition(|row| matches!(row.kind, RowKind::Gpu { .. }));
            gpus.sort_by_key(|row| std::cmp::Reverse(self.row_free_mem(row)));
            rows = gpus;
            rows.extend(others);
        }

        // HOST label on the first row of each contiguous group; counts always
        // come from the host's full GPU list.
        let mut previous: Option<usize> = None;
        for row in &mut rows {
            let host_name = &self.entries[row.host].meta.host;
            if previous != Some(row.host) {
                row.group_start = true;
                row.label = match self.free_total(row.host) {
                    Some((free, total)) if matches!(row.kind, RowKind::Gpu { .. }) => {
                        format!("{host_name} {free}/{total}")
                    }
                    _ => host_name.clone(),
                };
                previous = Some(row.host);
            } else {
                row.group_start = false;
                row.label.clear();
            }
        }
        rows
    }

    fn row(&self, host: usize, kind: RowKind) -> ListRow {
        let free_total = if matches!(kind, RowKind::Gpu { .. }) {
            self.free_total(host)
        } else {
            None
        };
        ListRow {
            host,
            kind,
            group_start: false,
            label: String::new(),
            free_total,
        }
    }

    fn row_free_mem(&self, row: &ListRow) -> Option<u64> {
        let RowKind::Gpu { host, gpu } = row.kind else {
            return None;
        };
        self.entries
            .get(host)?
            .result
            .as_ref()?
            .gpus
            .get(gpu)?
            .memory_free_mb
    }

    /// Number of hosts with at least one visible row.
    pub fn matched_hosts(&self, rows: &[ListRow]) -> usize {
        let mut seen = std::collections::BTreeSet::new();
        for row in rows {
            seen.insert(row.host);
        }
        seen.len()
    }

    pub fn ensure_visible(&mut self, height: usize) {
        let len = self.visible_rows().len();
        if len == 0 {
            self.selected = 0;
            self.offset = 0;
            return;
        }
        if self.selected >= len {
            self.selected = len - 1;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if height > 0 && self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.visible_rows().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, len as isize - 1) as usize;
    }

    fn open_detail(&mut self) {
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.selected) else {
            return;
        };
        let detail = match row.kind {
            RowKind::Pending { .. } => return,
            RowKind::Gpu { host, gpu } => DetailView {
                host,
                gpu: Some(gpu),
                scroll: 0,
            },
            RowKind::Error { host } | RowKind::NoGpu { host } => DetailView {
                host,
                gpu: None,
                scroll: 0,
            },
        };
        self.detail = Some(detail);
        self.mode = Mode::Detail;
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.quit();
            return;
        }
        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Filter => self.handle_filter(key),
            Mode::Help => self.handle_help(key),
            Mode::Detail => self.handle_detail(key),
        }
    }

    fn handle_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit(),
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Char('f') => {
                self.status = self.status.next();
                self.offset = 0;
            }
            KeyCode::Char('s') => {
                self.sort = self.sort.toggled();
                self.selected = 0;
                self.offset = 0;
            }
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Enter => self.open_detail(),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            _ => {}
        }
    }

    fn handle_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self.query.is_empty() {
                    self.mode = Mode::Normal;
                } else {
                    self.query.clear();
                }
                self.selected = 0;
                self.offset = 0;
            }
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
                self.offset = 0;
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.query.push(c);
                self.selected = 0;
                self.offset = 0;
            }
            _ => {}
        }
    }

    fn handle_help(&mut self, key: KeyEvent) {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter
        ) {
            self.mode = Mode::Normal;
        }
    }

    fn handle_detail(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_detail(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_detail(-1),
            KeyCode::PageDown => self.scroll_detail(10),
            KeyCode::PageUp => self.scroll_detail(-10),
            _ => {}
        }
    }

    fn scroll_detail(&mut self, delta: isize) {
        if let Some(detail) = &mut self.detail {
            detail.scroll = (detail.scroll as isize + delta).max(0) as usize;
        }
    }

    pub fn spinner(&self) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        FRAMES[(self.frame as usize / 2) % FRAMES.len()]
    }

    #[cfg(test)]
    fn apply_result(&mut self, host: usize, result: HostResult) {
        self.entries[host].result = Some(result);
    }
}

// ---------------------------------------------------------------------------
// Terminal lifecycle
// ---------------------------------------------------------------------------

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }
}

fn init_terminal() -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    // Ratatui's init installs its own restore hook; ours is in TerminalGuard.
    Ok(())
}

/// True when `poll(2)` reports hangup/error on `fd`.
#[cfg(unix)]
fn hangup_revents(revents: libc::c_short) -> bool {
    revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// Outcome of waiting for terminal input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputWait {
    Ready,
    Timeout,
    Hangup,
}

/// Wait up to `timeout` for readable input on stdin, or for the terminal to
/// disappear.
///
/// The TUI deliberately does not use `crossterm::event::poll` as its wait:
/// crossterm 0.29's mio event source reads the tty in a loop that treats
/// `Ok(0)` (EOF) as "no event yet" and never exits. When the pty master goes
/// away (terminal window closed, tmux session killed, ssh dropped), the slave
/// stays readable at EOF, so `event::poll` never returns — the process leaks
/// at 100% CPU with probes and ssh children still alive. Polling here exposes
/// `POLLHUP`, which lets us cancel and exit.
#[cfg(unix)]
fn wait_for_input(timeout: Duration) -> std::io::Result<InputWait> {
    poll_fd(std::io::stdin().as_raw_fd(), timeout)
}

#[cfg(unix)]
fn poll_fd(fd: std::os::fd::RawFd, timeout: Duration) -> std::io::Result<InputWait> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    loop {
        // SAFETY: `pfd` is a valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut pfd, 1, millis) };
        if ready < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if ready == 0 {
            return Ok(InputWait::Timeout);
        }
        if hangup_revents(pfd.revents) {
            return Ok(InputWait::Hangup);
        }
        return Ok(InputWait::Ready);
    }
}

#[cfg(not(unix))]
fn wait_for_input(_timeout: Duration) -> std::io::Result<InputWait> {
    Ok(InputWait::Timeout)
}

/// Errors a vanished terminal produces on write. They mean "the pty is gone",
/// not "gpufree is broken", so the TUI exits cleanly instead of reporting a
/// terminal error nobody can see.
fn is_terminal_gone(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
    ) || err.raw_os_error() == Some(libc::EIO)
}

/// Run the TUI until `q`, ctrl-c, a termination signal, or the terminal
/// disappearing. Returns `130` on a signal, `0` otherwise.
pub fn run(mut app: App) -> std::io::Result<i32> {
    init_terminal()?;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
    let _guard = TerminalGuard;

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        previous_hook(info);
    }));

    loop {
        if crate::signal::interrupted() {
            app.cancel_current();
            break;
        }

        app.drain();
        if let Err(err) = terminal.draw(|frame| ui::draw(frame, &mut app)) {
            if is_terminal_gone(&err) {
                app.cancel_current();
                break;
            }
            return Err(err);
        }

        if app.quit {
            break;
        }

        match wait_for_input(Duration::from_millis(50))? {
            InputWait::Hangup => {
                app.cancel_current();
                break;
            }
            InputWait::Timeout => {}
            InputWait::Ready => {
                // Our poll said readable, so crossterm cannot block here.
                if event::poll(Duration::ZERO)?
                    && let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press
                {
                    app.handle_key(key);
                }
                // Drain already-queued keys so a paste does not lag a frame.
                while event::poll(Duration::ZERO)? {
                    match event::read()? {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            app.handle_key(key);
                        }
                        Event::Resize(_, _) => {}
                        _ => {}
                    }
                }
            }
        }
        app.tick();
    }

    Ok(if crate::signal::interrupted() { 130 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TransportUsed;
    use crate::model::{ErrorKind, Gpu, HostResult, HostStatus, ProbeErrorInfo, Process, State};

    #[cfg(unix)]
    #[test]
    fn hangup_revents_recognizes_hup_err_nval() {
        assert!(hangup_revents(libc::POLLHUP));
        assert!(hangup_revents(libc::POLLIN | libc::POLLHUP));
        assert!(hangup_revents(libc::POLLERR));
        assert!(hangup_revents(libc::POLLNVAL));
        assert!(!hangup_revents(libc::POLLIN));
        assert!(!hangup_revents(0));
    }

    #[cfg(unix)]
    #[test]
    fn poll_fd_tracks_open_and_closed_pipe_writer() {
        use std::os::fd::FromRawFd;

        let mut fds = [0; 2];
        // SAFETY: `fds` has room for two descriptors.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: both fds came from `pipe` and are owned here.
        let read = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let write = unsafe { std::fs::File::from_raw_fd(fds[1]) };

        assert_eq!(
            poll_fd(read.as_raw_fd(), Duration::from_millis(10)).unwrap(),
            InputWait::Timeout,
            "an open pipe with no data must time out"
        );
        drop(write);
        assert_eq!(
            poll_fd(read.as_raw_fd(), Duration::from_millis(10)).unwrap(),
            InputWait::Hangup,
            "a closed writer must report POLLHUP"
        );
    }

    fn meta(host: &str) -> HostMeta {
        HostMeta {
            host: host.to_string(),
            resolved_alias: host.to_string(),
            transport_used: TransportUsed::Direct,
        }
    }

    fn gpu(index: u32, name: &str, state: State, free: u64, used: u64) -> Gpu {
        Gpu {
            index: Some(index),
            uuid: Some(format!("GPU-{index}")),
            name: Some(name.to_string()),
            driver: Some("570".into()),
            mig: false,
            compute_mode: Some("Default".into()),
            memory_total_mb: Some(24564),
            memory_used_mb: Some(used),
            memory_free_mb: Some(free),
            util_pct: Some(0),
            temp_c: Some(30),
            state,
            reason: None,
            processes: Some(vec![]),
        }
    }

    fn host_ok(name: &str, gpus: Vec<Gpu>) -> HostResult {
        HostResult {
            host: name.into(),
            resolved_alias: name.into(),
            transport_used: TransportUsed::Direct,
            status: HostStatus::Ok,
            probed_at: "t".into(),
            duration_ms: 10,
            hostname: Some(name.into()),
            load1: Some(0.1),
            cpus: Some(8),
            fields_complete: true,
            processes_complete: true,
            gpus,
            note: None,
            error: None,
        }
    }

    fn host_err(name: &str) -> HostResult {
        HostResult {
            host: name.into(),
            resolved_alias: name.into(),
            transport_used: TransportUsed::Remote,
            status: HostStatus::Error,
            probed_at: "t".into(),
            duration_ms: 6000,
            hostname: None,
            load1: None,
            cpus: None,
            fields_complete: false,
            processes_complete: false,
            gpus: vec![],
            note: None,
            error: Some(ProbeErrorInfo::new(ErrorKind::SshTimeout, "timed out")),
        }
    }

    fn app(hosts: &[(&str, HostResult)]) -> App {
        let metas: Vec<HostMeta> = hosts.iter().map(|(name, _)| meta(name)).collect();
        let mut app = App::new(
            metas,
            Some("test".into()),
            TransportMode::Direct,
            false,
            false,
            None,
        );
        for (index, (_, result)) in hosts.iter().enumerate() {
            app.apply_result(index, result.clone());
        }
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn slash_enters_filter_and_esc_clears_then_exits() {
        let mut app = app(&[("a", host_ok("a", vec![gpu(0, "A100", State::Free, 1, 0)]))]);
        assert_eq!(app.mode, Mode::Normal);
        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.mode, Mode::Filter);
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Char('1')));
        assert_eq!(app.query, "a1");
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.query, "");
        assert_eq!(app.mode, Mode::Filter, "first esc only clears");
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn enter_commits_filter_and_keeps_it() {
        let mut app = app(&[("a", host_ok("a", vec![gpu(0, "A100", State::Free, 1, 0)]))]);
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.query, "a");
    }

    #[test]
    fn q_in_filter_types_and_q_in_normal_quits() {
        let mut app = app(&[]);
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('q')));
        assert_eq!(app.query, "q");
        assert!(!app.quit);
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.quit);
    }

    #[test]
    fn help_overlay_opens_and_closes() {
        let mut app = app(&[]);
        app.handle_key(key(KeyCode::Char('?')));
        assert_eq!(app.mode, Mode::Help);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn status_cycles_and_sort_toggles() {
        let mut app = app(&[]);
        app.handle_key(key(KeyCode::Char('f')));
        assert_eq!(app.status, StatusFilter::Free);
        app.handle_key(key(KeyCode::Char('s')));
        assert_eq!(app.sort, SortMode::FreeMem);
        app.handle_key(key(KeyCode::Char('s')));
        assert_eq!(app.sort, SortMode::Host);
    }

    #[test]
    fn host_label_shows_free_over_total_and_is_unaffected_by_filter() {
        let mut app = app(&[
            (
                "gpu06",
                host_ok(
                    "gpu06",
                    vec![
                        gpu(0, "H20", State::Free, 100, 0),
                        gpu(1, "H20", State::Busy, 0, 2000),
                        gpu(2, "H20", State::Unknown, 50, 0),
                    ],
                ),
            ),
            ("nas", host_ok("nas", vec![])),
        ]);
        let rows = app.visible_rows();
        // gpu06: 1 free of 3 (unknown counts in total).
        assert_eq!(rows[0].label, "gpu06 1/3");
        assert!(rows[1].label.is_empty());
        assert!(rows[2].label.is_empty());
        assert_eq!(rows[3].label, "nas");

        app.status = StatusFilter::Free;
        let rows = app.visible_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "gpu06 1/3", "counts ignore the filter");

        app.status = StatusFilter::All;
        app.query = "h20".into();
        let rows = app.visible_rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].label, "gpu06 1/3", "counts ignore text filter");
    }

    fn counter_app() -> App {
        app(&[
            (
                "gpu06",
                host_ok(
                    "gpu06",
                    vec![
                        gpu(0, "H20", State::Free, 100, 0),
                        gpu(1, "A100", State::Busy, 0, 2000),
                    ],
                ),
            ),
            ("bad", host_err("bad")),
        ])
    }

    #[test]
    fn counter_matched_follows_the_text_filter_total_does_not() {
        let mut app = counter_app();
        assert_eq!((app.matched_rows(), app.total_rows()), (3, 3));
        app.query = "a100".into();
        assert_eq!((app.matched_rows(), app.total_rows()), (1, 3));
        app.query = "nope".into();
        assert_eq!((app.matched_rows(), app.total_rows()), (0, 3));
    }

    #[test]
    fn counter_matched_follows_the_status_filter() {
        let mut app = counter_app();
        app.status = StatusFilter::Free;
        assert_eq!((app.matched_rows(), app.total_rows()), (1, 3));
        app.status = StatusFilter::Error;
        assert_eq!((app.matched_rows(), app.total_rows()), (1, 3));
        app.status = StatusFilter::Unknown;
        assert_eq!((app.matched_rows(), app.total_rows()), (0, 3));
    }

    #[test]
    fn counter_uses_the_intersection_of_text_and_status() {
        let mut app = counter_app();
        app.query = "a100".into();
        app.status = StatusFilter::Busy;
        assert_eq!((app.matched_rows(), app.total_rows()), (1, 3));
        app.status = StatusFilter::Free;
        assert_eq!(
            (app.matched_rows(), app.total_rows()),
            (0, 3),
            "text and status intersect"
        );
        app.query = "gpu06".into();
        app.status = StatusFilter::Free;
        assert_eq!((app.matched_rows(), app.total_rows()), (1, 3));
    }

    #[test]
    fn counter_is_independent_of_scrolling() {
        let mut app = counter_app();
        app.status = StatusFilter::All;
        let before = (app.matched_rows(), app.total_rows());
        app.selected = 2;
        app.offset = 2;
        assert_eq!((app.matched_rows(), app.total_rows()), before);
        app.ensure_visible(1);
        assert_eq!((app.matched_rows(), app.total_rows()), before);
    }

    #[test]
    fn counter_total_respects_free_only() {
        let mut app = counter_app();
        app.free_only = true;
        // The error host and the busy-only... gpu06 still has one free GPU.
        assert_eq!((app.matched_rows(), app.total_rows()), (2, 2));
    }

    #[test]
    fn text_filter_matches_users_and_processes() {
        let mut g = gpu(0, "H20", State::Busy, 1, 1000);
        g.processes = Some(vec![Process {
            gpu_uuid: "GPU-0".into(),
            pid: Some(42),
            user: Some("alice".into()),
            process_name: "python train.py".into(),
            used_memory_mb: Some(1000),
        }]);
        let mut app = app(&[("gpu06", host_ok("gpu06", vec![g]))]);
        app.query = "alice".into();
        assert_eq!(app.visible_rows().len(), 1);
        app.query = "py tr".into();
        assert_eq!(app.visible_rows().len(), 1);
        app.query = "bob".into();
        assert!(app.visible_rows().is_empty());
    }

    #[test]
    fn free_mem_sort_orders_descending_and_puts_errors_last() {
        let mut app = app(&[
            (
                "a",
                host_ok(
                    "a",
                    vec![
                        gpu(0, "X", State::Free, 10, 0),
                        gpu(1, "X", State::Free, 100, 0),
                    ],
                ),
            ),
            ("bad", host_err("bad")),
        ]);
        app.sort = SortMode::FreeMem;
        let rows = app.visible_rows();
        assert_eq!(rows[0].free_total, Some((2, 2)));
        assert_eq!(rows[0].label, "a 2/2");
        match rows[0].kind {
            RowKind::Gpu { gpu, .. } => assert_eq!(gpu, 1, "largest free memory first"),
            _ => panic!("expected GPU row"),
        }
        assert!(matches!(rows.last().unwrap().kind, RowKind::Error { .. }));
        assert_eq!(rows.last().unwrap().label, "bad");
    }

    #[test]
    fn free_only_hides_busy_and_error_hosts_but_keeps_pending() {
        let mut app = app(&[
            (
                "free",
                host_ok("free", vec![gpu(0, "X", State::Free, 10, 0)]),
            ),
            (
                "busy",
                host_ok("busy", vec![gpu(0, "X", State::Busy, 0, 1000)]),
            ),
            ("bad", host_err("bad")),
            ("pending", host_ok("pending", vec![])),
        ]);
        app.entries[3].result = None;
        app.free_only = true;
        let rows = app.visible_rows();
        let hosts: Vec<usize> = rows.iter().map(|r| r.host).collect();
        assert_eq!(hosts, vec![0, 3]);
    }

    #[test]
    fn error_rows_only_show_for_all_or_error_status() {
        let mut app = app(&[("bad", host_err("bad"))]);
        assert_eq!(app.visible_rows().len(), 1);
        app.status = StatusFilter::Error;
        assert_eq!(app.visible_rows().len(), 1);
        app.status = StatusFilter::Free;
        assert!(app.visible_rows().is_empty());
    }

    #[test]
    fn enter_opens_gpu_detail_and_esc_returns() {
        let mut app = app(&[(
            "gpu06",
            host_ok(
                "gpu06",
                vec![
                    gpu(0, "H20", State::Free, 1, 0),
                    gpu(1, "H20", State::Busy, 1, 100),
                ],
            ),
        )]);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Detail);
        assert_eq!(
            app.detail,
            Some(DetailView {
                host: 0,
                gpu: Some(0),
                scroll: 0
            })
        );
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Char('j')));
        assert!(app.detail.unwrap().scroll >= 2);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn enter_on_error_and_no_gpu_opens_detail() {
        let mut app = app(&[("bad", host_err("bad"))]);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Detail);
        assert_eq!(app.detail.unwrap().gpu, None);
        app.handle_key(key(KeyCode::Char('q')));
        assert_eq!(app.mode, Mode::Normal);
        assert!(!app.quit);
    }

    #[test]
    fn pending_rows_cannot_open_detail() {
        let mut app = app(&[("a", host_ok("a", vec![]))]);
        app.entries[0].result = None;
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn ctrl_c_quits_from_every_mode() {
        let mut app = app(&[]);
        app.mode = Mode::Filter;
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn refresh_resets_results_and_bumps_generation() {
        let mut app = app(&[("a", host_ok("a", vec![gpu(0, "X", State::Free, 1, 0)]))]);
        app.selected = 0;
        app.handle_key(key(KeyCode::Char('r')));
        assert_eq!(app.generation, 1);
        assert!(app.entries[0].result.is_none());
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn selection_clamps_to_visible_rows() {
        let mut app = app(&[("a", host_ok("a", vec![gpu(0, "X", State::Free, 1, 0)]))]);
        app.selected = 99;
        app.ensure_visible(10);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn spinner_advances() {
        let mut app = app(&[]);
        let first = app.spinner();
        app.tick();
        app.tick();
        assert_ne!(first, app.spinner());
    }
}
