//! Idempotent installation for shell completion.
//!
//! Completion scripts live in their own files (`_gpufree` for zsh, a
//! bash-completion drop-in for bash, or `fish/completions/gpufree.fish`).
//! Only when the shell cannot autoload that directory do we add a small
//! marker block to the user's rc file, and that block contains two setup
//! lines rather than the whole script. A malformed or duplicated marker
//! always fails closed: guessing how to repair an rc file is worse than
//! telling the user exactly which line is wrong.

use std::collections::VecDeque;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::shells;

pub const MARKER_VERSION: u32 = 1;
/// Ownership marker at the end of every installed completion file.
pub const SCRIPT_MARKER: &str = "# gpufree-generated completion file";

const BEGIN_PREFIX: &str = "# >>> gpufree completion";
const END_PREFIX: &str = "# <<< gpufree completion";

type Result<T> = std::result::Result<T, String>;

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

/// Filesystem locations used for planning. Captured once so tests can point
/// the whole installer at a temporary tree without mutating the process env.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub xdg_data: PathBuf,
    pub xdg_config: PathBuf,
    pub zdotdir: Option<PathBuf>,
    pub bash_completion_user_dir: Option<PathBuf>,
    pub zsh_custom: Option<PathBuf>,
    pub bash_completion_installed: bool,
}

const BASH_COMPLETION_MARKERS: [&str; 5] = [
    "/usr/share/bash-completion/bash_completion",
    "/etc/bash_completion",
    "/opt/homebrew/etc/profile.d/bash_completion.sh",
    "/usr/local/etc/profile.d/bash_completion.sh",
    "/usr/share/bash-completion/completions",
];

impl Env {
    pub fn from_process() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| "HOME is not set".to_string())?;
        let xdg_data = nonempty_env("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let xdg_config = nonempty_env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(Self {
            home,
            xdg_data,
            xdg_config,
            zdotdir: nonempty_env("ZDOTDIR").map(PathBuf::from),
            bash_completion_user_dir: nonempty_env("BASH_COMPLETION_USER_DIR").map(PathBuf::from),
            zsh_custom: nonempty_env("ZSH_CUSTOM").map(PathBuf::from),
            bash_completion_installed: BASH_COMPLETION_MARKERS
                .iter()
                .any(|marker| Path::new(marker).exists()),
        })
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

// ---------------------------------------------------------------------------
// Public options and reports
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub rc_file: Option<PathBuf>,
    pub dir: Option<PathBuf>,
    pub follow_symlinks: bool,
    pub dry_run: bool,
}

#[derive(Clone, Debug)]
pub struct InstallReport {
    pub shell: String,
    pub script_file: PathBuf,
    pub resolved_script_file: PathBuf,
    pub rc_file: Option<PathBuf>,
    pub directories: Vec<PathBuf>,
    pub changed: bool,
    pub script_changed: bool,
    pub rc_changed: bool,
    pub script_removed: bool,
    pub rc_removed: bool,
    pub directory_only: bool,
    pub dry_run: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StatusReport {
    pub shell: String,
    pub script_file: PathBuf,
    pub script_state: &'static str,
    pub rc_file: Option<PathBuf>,
    pub rc_state: &'static str,
    pub state: &'static str,
    pub installed: bool,
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn script_name(shell: &str) -> &'static str {
    match shell {
        "bash" => "gpufree",
        "zsh" => "_gpufree",
        "fish" => "gpufree.fish",
        _ => unreachable!("unsupported shell {shell:?}"),
    }
}

fn zsh_base(env: &Env) -> PathBuf {
    env.zdotdir.clone().unwrap_or_else(|| env.home.clone())
}

fn default_rc_path(shell: &str, env: &Env) -> PathBuf {
    match shell {
        "bash" => {
            let rc = env.home.join(".bashrc");
            if rc.exists() {
                return rc;
            }
            let profile = env.home.join(".bash_profile");
            if profile.exists() {
                return profile;
            }
            rc
        }
        "zsh" => zsh_base(env).join(".zshrc"),
        "fish" => env.xdg_config.join("fish/config.fish"),
        _ => unreachable!("unsupported shell {shell:?}"),
    }
}

fn rc_candidates(shell: &str, env: &Env) -> Vec<PathBuf> {
    match shell {
        "bash" => vec![env.home.join(".bashrc"), env.home.join(".bash_profile")],
        _ => vec![default_rc_path(shell, env)],
    }
}

fn oh_my_zsh_completions(env: &Env) -> Option<PathBuf> {
    if let Some(custom) = &env.zsh_custom {
        return Some(custom.join("completions"));
    }
    let omz = env.home.join(".oh-my-zsh");
    omz.is_dir().then(|| omz.join("custom/completions"))
}

fn script_plan(shell: &str, env: &Env) -> (PathBuf, bool) {
    if shell == "bash" {
        if let Some(user_dir) = &env.bash_completion_user_dir {
            return (user_dir.join("completions/gpufree"), false);
        }
        if env.bash_completion_installed {
            return (
                env.xdg_data.join("bash-completion/completions/gpufree"),
                false,
            );
        }
        return (env.xdg_data.join("gpufree/completion.bash"), true);
    }
    if shell == "zsh" {
        if let Some(dir) = oh_my_zsh_completions(env) {
            return (dir.join("_gpufree"), false);
        }
        if let Some(dir) = zsh_fpath_dirs(env)
            .into_iter()
            .find(|dir| writable_dir(dir))
        {
            return (dir.join("_gpufree"), false);
        }
        return (env.home.join(".zfunc/_gpufree"), true);
    }
    (env.xdg_config.join("fish/completions/gpufree.fish"), false)
}

fn zsh_config_files(env: &Env) -> Vec<PathBuf> {
    let base = zsh_base(env);
    [".zshrc", ".zshenv", ".zprofile", ".zlogin"]
        .iter()
        .map(|name| base.join(name))
        .collect()
}

fn zsh_fpath_dirs(env: &Env) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for config in zsh_config_files(env) {
        let Ok(text) = fs::read_to_string(&config) else {
            continue;
        };
        let Ok(scan) = scan(&text) else {
            continue;
        };
        for dir in fpath_dirs_in(&text, &config, &scan) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// Extract directories from `fpath=(...)` / `fpath+=(...)` lines, ignoring
/// text that only looks like configuration (comments, quotes, heredoc bodies,
/// arithmetic, and gpufree's own managed block).
fn fpath_dirs_in(text: &str, config: &Path, scan: &Scan) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            if let Some(index) = trimmed.find("fpath") {
                let position = offset + (line.len() - trimmed.len()) + index;
                if scan.is_code(position) {
                    fpath_dirs_on_line(trimmed[index..].trim_start(), config, &mut out);
                }
            }
        }
        offset += raw.len();
    }
    out
}

fn fpath_dirs_on_line(line: &str, config: &Path, out: &mut Vec<PathBuf>) {
    let Some(rest) = line.strip_prefix("fpath") else {
        return;
    };
    let mut rest = rest.trim_start();
    if let Some(stripped) = rest.strip_prefix('+') {
        rest = stripped.trim_start();
    }
    let Some(rest) = rest.strip_prefix('=') else {
        return;
    };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix('(') else {
        return;
    };
    let Some(end) = rest.find(')') else {
        return;
    };
    for token in rest[..end].split_whitespace() {
        let token = token.trim_matches(|c| c == '\'' || c == '"');
        if token.is_empty() || token.contains('$') {
            continue;
        }
        let expanded = if let Some(tail) = token.strip_prefix("~/") {
            dirs_home().join(tail)
        } else if token == "~" {
            dirs_home()
        } else {
            PathBuf::from(token)
        };
        let path = if expanded.is_absolute() {
            expanded
        } else {
            config
                .parent()
                .map(|parent| parent.join(&expanded))
                .unwrap_or(expanded)
        };
        out.push(path);
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

#[cfg(unix)]
fn writable_dir(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let check = |p: &Path| match CString::new(p.as_os_str().as_bytes()) {
        Ok(c) => unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) == 0 },
        Err(_) => false,
    };
    if path.exists() {
        path.is_dir() && check(path)
    } else {
        path.parent().is_some_and(check)
    }
}

#[cfg(not(unix))]
fn writable_dir(path: &Path) -> bool {
    path.is_dir()
}

fn ensure_writable(path: &Path, verb: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if writable_dir(parent) {
        return Ok(());
    }
    Err(format!(
        "cannot {verb} {}: {} is not writable",
        path.display(),
        parent.display()
    ))
}

// ---------------------------------------------------------------------------
// Targets and atomic writes
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Target {
    requested: PathBuf,
    path: PathBuf,
}

fn resolve_target(requested: &Path, follow_symlinks: bool, label: &str) -> Result<Target> {
    let symlink = fs::symlink_metadata(requested)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false);
    if symlink && !follow_symlinks {
        return Err(format!(
            "refusing to write through symlink {}; pass --follow-symlinks to update its target",
            requested.display()
        ));
    }
    let path = if symlink {
        fs::canonicalize(requested)
            .map_err(|err| format!("cannot resolve symlink {}: {err}", requested.display()))?
    } else {
        requested.to_path_buf()
    };
    if let Ok(meta) = fs::metadata(&path) {
        if !meta.is_file() {
            return Err(format!("{label} is not a regular file: {}", path.display()));
        }
    }
    Ok(Target {
        requested: requested.to_path_buf(),
        path,
    })
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(data) => Ok(Some(data)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("cannot read {}: {err}", path.display())),
    }
}

fn read_text_optional(path: &Path) -> Result<String> {
    let Some(data) = read_optional(path)? else {
        return Ok(String::new());
    };
    String::from_utf8(data)
        .map_err(|_| format!("{} is not valid UTF-8; refusing to edit it", path.display()))
}

#[cfg(unix)]
fn mode_for(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o7777)
        .unwrap_or(0o644)
}

#[cfg(not(unix))]
fn mode_for(_path: &Path) -> u32 {
    0o644
}

fn atomic_write(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("cannot determine parent directory of {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "gpufree".to_string());
    let temp = parent.join(format!(".{name}.{}.{stamp}.tmp", std::process::id()));

    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|err| format!("cannot create {}: {err}", temp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(mode))
                .map_err(|err| format!("cannot set mode on {}: {err}", temp.display()))?;
        }
        file.write_all(data)
            .map_err(|err| format!("cannot write {}: {err}", temp.display()))?;
        file.sync_all()
            .map_err(|err| format!("cannot flush {}: {err}", temp.display()))?;
        drop(file);
        fs::rename(&temp, path)
            .map_err(|err| format!("cannot replace {}: {err}", path.display()))?;
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

// ---------------------------------------------------------------------------
// Marker blocks
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct Block {
    shell: String,
    version: u32,
    start: usize,
    end: usize,
}

fn parse_marker(line: &str, begin: bool) -> Result<Option<(String, u32)>> {
    let (prefix, suffix) = if begin {
        (BEGIN_PREFIX, ">>>")
    } else {
        (END_PREFIX, "<<<")
    };
    let Some(rest) = line.strip_prefix(prefix) else {
        return Ok(None);
    };
    let malformed = || format!("malformed completion marker: {line:?}");
    let rest = rest.strip_prefix(':').ok_or_else(malformed)?.trim();
    let rest = rest.strip_suffix(suffix).ok_or_else(malformed)?.trim();
    let (shell, version) = rest.rsplit_once(' ').ok_or_else(malformed)?;
    if !super::detect::SUPPORTED_SHELLS.contains(&shell) {
        return Err(malformed());
    }
    let version = version
        .strip_prefix('v')
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(malformed)?;
    Ok(Some((shell.to_string(), version)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
}

/// Result of lexing an rc file. `blocks` holds the managed marker blocks found
/// in real shell code; `skipped` holds byte ranges that are not code (comments,
/// quoted strings, heredoc bodies, arithmetic expansions), so the fpath scanner
/// can reach the same conclusions as the block parser.
#[derive(Debug, Default)]
struct Scan {
    blocks: Vec<Block>,
    skipped: Vec<(usize, usize)>,
}

impl Scan {
    fn is_code(&self, position: usize) -> bool {
        !self
            .skipped
            .iter()
            .any(|&(start, end)| start <= position && position < end)
            && !self
                .blocks
                .iter()
                .any(|block| block.start <= position && position < block.end)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    Single,
    Double,
    /// `$'...'`: backslash escapes the next character, including `'`.
    AnsiC,
}

/// Parse the word following `<<`/`<<-` at `start` (the byte offset of the first
/// `<`). Dynamic delimiters (`<<$VAR`, ``<<`cmd` ``) cannot be located reliably,
/// so they fail closed rather than risking edits inside a heredoc body.
fn heredoc_at(line: &str, start: usize) -> Result<(Option<Heredoc>, usize)> {
    let bytes = line.as_bytes();
    let mut index = start + 2;
    let mut strip_tabs = false;
    if bytes.get(index) == Some(&b'-') {
        strip_tabs = true;
        index += 1;
    }
    while matches!(bytes.get(index), Some(b' ' | b'\t')) {
        index += 1;
    }
    let word_start = index;
    let mut delimiter = String::new();
    let mut quote: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == active {
                quote = None;
                index += 1;
            } else if active == b'"' && byte == b'\\' {
                index += 1;
                let Some(next) = line[index..].chars().next() else {
                    break;
                };
                delimiter.push(next);
                index += next.len_utf8();
            } else if active == b'"' && (byte == b'$' || byte == b'`') {
                return Err(dynamic_delimiter());
            } else {
                let ch = line[index..].chars().next().expect("char boundary");
                delimiter.push(ch);
                index += ch.len_utf8();
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => {
                quote = Some(byte);
                index += 1;
            }
            b'\\' => {
                index += 1;
                let Some(next) = line[index..].chars().next() else {
                    break;
                };
                delimiter.push(next);
                index += next.len_utf8();
            }
            b'$' | b'`' => return Err(dynamic_delimiter()),
            _ if byte.is_ascii_whitespace() || b";|&()<>".contains(&byte) => break,
            _ => {
                let ch = line[index..].chars().next().expect("char boundary");
                delimiter.push(ch);
                index += ch.len_utf8();
            }
        }
    }
    if index == word_start {
        return Ok((None, index));
    }
    if quote.is_some() {
        return Err(
            "unterminated quote in a heredoc delimiter; refusing to edit this file".to_string(),
        );
    }
    Ok((
        Some(Heredoc {
            delimiter,
            strip_tabs,
        }),
        index,
    ))
}

fn dynamic_delimiter() -> String {
    "cannot parse a heredoc with a dynamic delimiter; refusing to edit this file".to_string()
}

/// Lex the file well enough to tell managed marker lines from marker-looking
/// user content. The lexer is deliberately conservative: anything it cannot
/// model (a dynamic heredoc delimiter) is an error rather than a guess.
fn scan(text: &str) -> Result<Scan> {
    let mut out = Scan::default();
    let mut open: Option<(String, u32, usize)> = None;
    let mut pending: VecDeque<Heredoc> = VecDeque::new();
    let mut in_body = false;
    let mut quote: Option<Quote> = None;
    let mut quote_start = 0usize;
    let mut arithmetic = 0usize;
    let mut arithmetic_start = 0usize;
    // `${...}` is a word/pattern context, not arithmetic or a heredoc: `((`
    // and `<<` inside it are literal (e.g. `${var%%((*)}`).
    let mut parameter = 0usize;
    let mut parameter_start = 0usize;
    let mut parameter_quote: Option<u8> = None;
    let mut parameter_escaped = false;
    let mut parameter_parens = 0usize;
    let mut parameter_backtick = false;
    let mut word_start = true;
    let mut offset = 0usize;

    for raw in text.split_inclusive('\n') {
        let line_end = offset + raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);

        if in_body {
            let heredoc = pending.front().expect("body implies a pending heredoc");
            let candidate = if heredoc.strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            out.skipped.push((offset, line_end));
            if candidate == heredoc.delimiter {
                pending.pop_front();
                if pending.is_empty() {
                    in_body = false;
                }
            }
            offset = line_end;
            word_start = true;
            continue;
        }

        // A marker is a comment line, so it only counts when the line starts a
        // shell word in normal code context.
        if quote.is_none() && arithmetic == 0 && parameter == 0 && word_start {
            if let Some((shell, version)) = parse_marker(line, true)? {
                if open.is_some() {
                    return Err("nested gpufree completion BEGIN marker".to_string());
                }
                open = Some((shell, version, offset));
                offset = line_end;
                word_start = true;
                continue;
            }
            if let Some((shell, version)) = parse_marker(line, false)? {
                let Some((open_shell, open_version, start)) = open.take() else {
                    return Err("gpufree completion END marker without BEGIN".to_string());
                };
                if open_shell != shell || open_version != version {
                    return Err(format!(
                        "gpufree completion markers disagree: {open_shell} v{open_version} vs {shell} v{version}"
                    ));
                }
                out.blocks.push(Block {
                    shell,
                    version,
                    start,
                    end: line_end,
                });
                offset = line_end;
                word_start = true;
                continue;
            }
        }

        let bytes = raw.as_bytes();
        let mut index = 0usize;
        let mut escaped = false;
        let mut continued = false;
        while index < bytes.len() {
            let byte = bytes[index];
            let abs = offset + index;

            if parameter > 0 {
                if parameter_escaped {
                    parameter_escaped = false;
                    index += 1;
                    continue;
                }
                if let Some(active) = parameter_quote {
                    if active == b'"' && byte == b'\\' {
                        parameter_escaped = true;
                    } else if byte == active {
                        parameter_quote = None;
                    }
                    index += 1;
                    continue;
                }
                match byte {
                    b'\\' => {
                        parameter_escaped = true;
                        index += 1;
                    }
                    b'\'' | b'"' => {
                        parameter_quote = Some(byte);
                        index += 1;
                    }
                    b'`' => {
                        parameter_backtick = !parameter_backtick;
                        index += 1;
                    }
                    b'$' if !parameter_backtick && bytes.get(index + 1) == Some(&b'{') => {
                        parameter += 1;
                        index += 2;
                    }
                    b'$' if !parameter_backtick && bytes.get(index + 1) == Some(&b'(') => {
                        parameter_parens += 1;
                        index += 2;
                    }
                    b'(' if !parameter_backtick && parameter_parens > 0 => {
                        parameter_parens += 1;
                        index += 1;
                    }
                    b')' if !parameter_backtick && parameter_parens > 0 => {
                        parameter_parens -= 1;
                        index += 1;
                    }
                    b'}' if !parameter_backtick && parameter_parens == 0 => {
                        parameter -= 1;
                        index += 1;
                        if parameter == 0 {
                            out.skipped.push((parameter_start, abs + 1));
                            word_start = false;
                        }
                    }
                    _ => index += 1,
                }
                continue;
            }

            if arithmetic > 0 {
                match byte {
                    b'(' => arithmetic += 1,
                    b')' => {
                        arithmetic -= 1;
                        if arithmetic == 0 {
                            out.skipped.push((arithmetic_start, abs + 1));
                        }
                    }
                    _ => {}
                }
                index += 1;
                continue;
            }

            if let Some(active) = quote {
                if escaped {
                    escaped = false;
                    index += 1;
                    continue;
                }
                match (active, byte) {
                    (Quote::Double | Quote::AnsiC, b'\\') => {
                        escaped = true;
                        index += 1;
                    }
                    (Quote::Single, b'\'') | (Quote::Double, b'"') | (Quote::AnsiC, b'\'') => {
                        quote = None;
                        out.skipped.push((quote_start, abs + 1));
                        index += 1;
                        word_start = false;
                    }
                    _ => index += 1,
                }
                continue;
            }

            if escaped {
                escaped = false;
                if byte == b'\n' {
                    // Backslash-newline removes the newline; the next line is
                    // still inside the same word.
                    continued = true;
                }
                word_start = false;
                index += 1;
                continue;
            }

            match byte {
                b'\\' => {
                    escaped = true;
                    index += 1;
                    word_start = false;
                }
                b'\'' => {
                    quote = Some(Quote::Single);
                    quote_start = abs;
                    index += 1;
                    word_start = false;
                }
                b'"' => {
                    quote = Some(Quote::Double);
                    quote_start = abs;
                    index += 1;
                    word_start = false;
                }
                b'$' if bytes.get(index + 1) == Some(&b'{') => {
                    parameter = 1;
                    parameter_start = abs;
                    index += 2;
                    word_start = false;
                }
                b'$' if bytes.get(index + 1) == Some(&b'\'') => {
                    quote = Some(Quote::AnsiC);
                    quote_start = abs;
                    index += 2;
                    word_start = false;
                }
                b'$' if bytes.get(index + 1) == Some(&b'"') => {
                    quote = Some(Quote::Double);
                    quote_start = abs;
                    index += 2;
                    word_start = false;
                }
                b'#' if word_start => {
                    out.skipped.push((abs, line_end));
                    word_start = true;
                    break;
                }
                // `((...))` is arithmetic only at a command word start or as
                // `$((...))`; mid-word `((` (e.g. `p=foo((bar`) is literal.
                b'(' if bytes.get(index + 1) == Some(&b'(')
                    && (word_start || (index > 0 && bytes[index - 1] == b'$')) =>
                {
                    arithmetic = 2;
                    arithmetic_start = abs;
                    index += 2;
                    word_start = true;
                }
                b'<' if bytes.get(index + 1) == Some(&b'<') => {
                    if bytes.get(index + 2) == Some(&b'<') {
                        // Here-string, not a heredoc.
                        index += 3;
                    } else {
                        let (heredoc, next) = heredoc_at(raw, index)?;
                        if let Some(heredoc) = heredoc {
                            pending.push_back(heredoc);
                        }
                        index = next;
                    }
                    word_start = false;
                }
                b' ' | b'\t' | b'\r' | b'\n' | b';' | b'|' | b'&' | b'(' | b')' | b'<' | b'>' => {
                    word_start = true;
                    index += 1;
                }
                _ => {
                    word_start = false;
                    index += 1;
                }
            }
        }

        // The heredoc body starts after the newline that ends the command. A
        // backslash-newline, an open quote, or open arithmetic continues it.
        if !continued && quote.is_none() && arithmetic == 0 && !pending.is_empty() {
            in_body = true;
        }
        offset = line_end;
    }

    if quote.is_some() {
        out.skipped.push((quote_start, text.len()));
    }
    if arithmetic > 0 {
        out.skipped.push((arithmetic_start, text.len()));
    }
    if parameter > 0 {
        // An unterminated expansion makes every following line unparseable, so
        // do not guess which markers are real.
        return Err(
            "unterminated ${...} parameter expansion; refusing to edit this file".to_string(),
        );
    }
    if in_body && !pending.is_empty() {
        return Err("unterminated heredoc; refusing to edit this file".to_string());
    }
    if let Some((shell, version, _)) = open {
        return Err(format!(
            "gpufree completion BEGIN marker for {shell} v{version} has no END"
        ));
    }
    Ok(out)
}

fn parse_blocks(text: &str) -> Result<Vec<Block>> {
    Ok(scan(text)?.blocks)
}

/// Remove the one managed block for `shell`, failing closed on duplicates or
/// blocks written by a newer gpufree.
fn strip_shell_block(text: &str, shell: &str) -> Result<(String, Option<Block>)> {
    let blocks = parse_blocks(text)?;
    let mut found: Option<Block> = None;
    for block in &blocks {
        if block.shell != shell {
            continue;
        }
        if found.is_some() {
            return Err(format!(
                "duplicate gpufree completion blocks for {shell}; remove one manually"
            ));
        }
        if block.version > MARKER_VERSION {
            return Err(format!(
                "unsupported gpufree completion marker v{}; upgrade gpufree or remove the block manually",
                block.version
            ));
        }
        found = Some(block.clone());
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for block in &blocks {
        if block.shell != shell {
            continue;
        }
        out.push_str(&text[cursor..block.start]);
        cursor = block.end;
    }
    out.push_str(&text[cursor..]);
    Ok((out, found))
}

fn render_block(shell: &str, script: &Path) -> String {
    let quoted = shell_quote(&script.to_string_lossy());
    let body = match shell {
        "zsh" => {
            let dir = script.parent().unwrap_or_else(|| Path::new(""));
            let dir = shell_quote(&dir.to_string_lossy());
            format!("fpath=({dir} $fpath)\nautoload -Uz compinit && compinit -i\n")
        }
        "fish" => format!("source {quoted}\n"),
        _ => format!("if [ -r {quoted} ]; then . {quoted}; fi\n"),
    };
    format!(
        "{BEGIN_PREFIX}: {shell} v{MARKER_VERSION} >>>\n{body}{END_PREFIX}: {shell} v{MARKER_VERSION} <<<\n"
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------------------
// Script payload
// ---------------------------------------------------------------------------

fn script_payload(shell: &str) -> Vec<u8> {
    let mut text = shells::generate(shell);
    text.push_str(&format!(
        "{SCRIPT_MARKER}; remove with: gpufree completion --uninstall {shell}\n"
    ));
    text.into_bytes()
}

fn is_managed_script(data: &[u8]) -> bool {
    data.windows(SCRIPT_MARKER.len())
        .any(|window| window == SCRIPT_MARKER.as_bytes())
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

struct Plan {
    script: Target,
    rc: Option<Target>,
    directory_only: bool,
    warnings: Vec<String>,
}

fn build_plan(shell: &str, options: &Options, env: &Env, read_only: bool) -> Result<Plan> {
    let follow = options.follow_symlinks || read_only;
    let (script_path, needs_rc) = match &options.dir {
        Some(dir) => (dir.join(script_name(shell)), false),
        None => script_plan(shell, env),
    };
    let script = resolve_target(&script_path, follow, "completion file")?;

    let rc_requested = options.rc_file.clone().or_else(|| {
        if needs_rc {
            Some(default_rc_path(shell, env))
        } else {
            None
        }
    });
    let rc = match rc_requested {
        Some(path) => Some(resolve_target(&path, follow, "rc file")?),
        None => None,
    };

    let directory_only = options.dir.is_some() && rc.is_none();
    Ok(Plan {
        script,
        rc,
        directory_only,
        warnings: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Public actions
// ---------------------------------------------------------------------------

pub fn install(shell: &str, options: &Options, env: &Env) -> Result<InstallReport> {
    let plan = build_plan(shell, options, env, false)?;

    let old_script = read_optional(&plan.script.path)?;
    if let Some(data) = &old_script {
        if !is_managed_script(data) {
            return Err(format!(
                "completion file already exists and was not created by gpufree: {}",
                plan.script.path.display()
            ));
        }
    }
    let new_script = script_payload(shell);
    let script_changed = old_script.as_deref() != Some(new_script.as_slice());

    let mut rc_changed = false;
    let mut new_rc: Option<(PathBuf, String)> = None;
    if let Some(rc) = &plan.rc {
        let old_text = read_text_optional(&rc.path)?;
        let (mut text, _) = strip_shell_block(&old_text, shell)?;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&render_block(shell, &plan.script.path));
        if text != old_text {
            rc_changed = true;
            new_rc = Some((rc.path.clone(), text));
        }
    }

    if !options.dry_run {
        if script_changed {
            atomic_write(&plan.script.path, &new_script, mode_for(&plan.script.path))?;
        }
        if let Some((path, text)) = &new_rc {
            atomic_write(path, text.as_bytes(), mode_for(path))?;
        }
    }

    Ok(InstallReport {
        shell: shell.to_string(),
        script_file: plan.script.requested.clone(),
        resolved_script_file: plan.script.path.clone(),
        rc_file: plan.rc.as_ref().map(|rc| rc.requested.clone()),
        directories: plan
            .script
            .path
            .parent()
            .map(|parent| vec![parent.to_path_buf()])
            .unwrap_or_default(),
        changed: script_changed || rc_changed,
        script_changed,
        rc_changed,
        script_removed: false,
        rc_removed: false,
        directory_only: plan.directory_only,
        dry_run: options.dry_run,
        warnings: plan.warnings,
    })
}

pub fn uninstall(shell: &str, options: &Options, env: &Env) -> Result<InstallReport> {
    let plan = build_plan(shell, options, env, false)?;

    // Removing the symlink target would leave a dangling dotfile symlink, so
    // let the user decide what a symlinked completion file should mean.
    let script_is_symlink = fs::symlink_metadata(&plan.script.requested)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false);
    if script_is_symlink {
        return Err(format!(
            "refusing to remove symlinked completion file {}; remove it manually",
            plan.script.requested.display()
        ));
    }

    // A `--dir` install never touched shell config unless --rc-file was also
    // given, so uninstalling it must not go hunting in rc files.
    let rc_paths: Vec<PathBuf> = match &options.rc_file {
        Some(path) => vec![path.clone()],
        None if options.dir.is_some() => Vec::new(),
        None => {
            let mut paths = rc_candidates(shell, env);
            if let Some(rc) = &plan.rc {
                paths.insert(0, rc.path.clone());
            }
            let mut unique = Vec::new();
            for path in paths {
                if !unique.contains(&path) {
                    unique.push(path);
                }
            }
            unique
        }
    };

    // Validate every rc before removing anything, so a malformed block cannot
    // leave the install half-removed.
    let mut edits: Vec<(PathBuf, PathBuf, String)> = Vec::new();
    for requested in &rc_paths {
        let symlink = fs::symlink_metadata(requested)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false);
        if symlink && !options.follow_symlinks {
            continue;
        }
        let path = if symlink {
            match fs::canonicalize(requested) {
                Ok(path) => path,
                Err(_) => continue,
            }
        } else {
            requested.clone()
        };
        let text = read_text_optional(&path)?;
        if text.is_empty() {
            continue;
        }
        let (new_text, found) = strip_shell_block(&text, shell)?;
        if found.is_some() {
            edits.push((requested.clone(), path, new_text));
        }
    }

    // Validate the script before touching anything, but remove it only after
    // the rc edits succeed: a failed rc write must not leave an rc block
    // pointing at a file that no longer exists.
    let script_present = match read_optional(&plan.script.path)? {
        Some(data) => {
            if !is_managed_script(&data) {
                return Err(format!(
                    "refusing to remove {}: it was not created by gpufree",
                    plan.script.path.display()
                ));
            }
            true
        }
        None => false,
    };

    // Both files are edited by rename, so a read-only directory would fail
    // halfway through. Check every target up front and mutate nothing on error.
    if !options.dry_run {
        for (_, path, _) in &edits {
            ensure_writable(path, "write")?;
        }
        if script_present {
            ensure_writable(&plan.script.path, "remove")?;
        }
    }

    let mut rc_removed = false;
    let mut first_rc: Option<PathBuf> = options.rc_file.clone();
    for (requested, path, new_text) in edits {
        if !options.dry_run {
            atomic_write(&path, new_text.as_bytes(), mode_for(&path))?;
        }
        rc_removed = true;
        first_rc.get_or_insert(requested);
    }

    let mut script_removed = false;
    if script_present {
        if !options.dry_run {
            fs::remove_file(&plan.script.path)
                .map_err(|err| format!("cannot remove {}: {err}", plan.script.path.display()))?;
        }
        script_removed = true;
    }

    Ok(InstallReport {
        shell: shell.to_string(),
        script_file: plan.script.requested.clone(),
        resolved_script_file: plan.script.path.clone(),
        rc_file: first_rc,
        directories: Vec::new(),
        changed: script_removed || rc_removed,
        script_changed: false,
        rc_changed: false,
        script_removed,
        rc_removed,
        directory_only: plan.directory_only,
        dry_run: options.dry_run,
        warnings: Vec::new(),
    })
}

pub fn status(shell: &str, options: &Options, env: &Env) -> Result<StatusReport> {
    let plan = build_plan(shell, options, env, true)?;
    let expected = script_payload(shell);

    let (script_state, installed_script) = match read_optional(&plan.script.path)? {
        None => ("missing", false),
        Some(data) if !is_managed_script(&data) => ("foreign", false),
        Some(data) if data == expected => ("current", true),
        Some(_) => ("stale", false),
    };

    let mut rc_message: Option<String> = None;
    let (rc_state, rc_ok) = match &plan.rc {
        None => ("not_required", true),
        Some(rc) => {
            if fs::metadata(&rc.path).is_err() {
                ("missing", false)
            } else {
                let text = read_text_optional(&rc.path)?;
                let blocks = parse_blocks(&text)?;
                let same: Vec<&Block> =
                    blocks.iter().filter(|block| block.shell == shell).collect();
                if same.len() > 1 {
                    // install/uninstall fail closed on duplicates; status must
                    // not call the same state healthy.
                    rc_message = Some(format!(
                        "duplicate gpufree completion blocks for {shell} in {}",
                        rc.path.display()
                    ));
                    ("malformed", false)
                } else {
                    match same.first().copied() {
                        None => ("missing", false),
                        Some(block) if block.version > MARKER_VERSION => {
                            rc_message = Some(format!(
                                "gpufree completion block for {shell} in {} is v{}; this gpufree supports v{MARKER_VERSION}",
                                rc.path.display(),
                                block.version
                            ));
                            ("unsupported_version", false)
                        }
                        Some(block) => {
                            let expected = render_block(shell, &plan.script.path);
                            let content = &text[block.start..block.end];
                            if content == expected {
                                ("current", true)
                            } else {
                                ("stale", false)
                            }
                        }
                    }
                }
            }
        }
    };

    // A broken or newer rc block is the more actionable problem: without these
    // arms a missing script would report `not_installed` and hide it.
    let state = if script_state == "foreign" {
        "foreign"
    } else if rc_state == "malformed" {
        "malformed"
    } else if rc_state == "unsupported_version" {
        "unsupported_version"
    } else if script_state == "missing" {
        "not_installed"
    } else if script_state == "stale" || !rc_ok {
        "stale"
    } else {
        "current"
    };
    let message = rc_message.or_else(|| match script_state {
        "foreign" => Some(format!(
            "completion file was not created by gpufree: {}",
            plan.script.path.display()
        )),
        _ => None,
    });

    Ok(StatusReport {
        shell: shell.to_string(),
        script_file: plan.script.requested.clone(),
        script_state,
        rc_file: plan.rc.as_ref().map(|rc| rc.requested.clone()),
        rc_state,
        state,
        installed: installed_script && rc_ok,
        message,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "gpufree-completion-{tag}-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn env_at(root: &Path) -> Env {
        Env {
            home: root.join("home"),
            xdg_data: root.join("data"),
            xdg_config: root.join("config"),
            zdotdir: None,
            bash_completion_user_dir: None,
            zsh_custom: None,
            bash_completion_installed: false,
        }
    }

    fn options() -> Options {
        Options::default()
    }

    #[test]
    fn bash_install_writes_script_and_rc_block() {
        let temp = Temp::new("bash-install");
        let env = env_at(&temp.0);
        let report = install("bash", &options(), &env).unwrap();
        assert!(report.changed);
        assert!(report.rc_changed);
        let script = env.xdg_data.join("gpufree/completion.bash");
        assert!(script.exists());
        assert_eq!(report.script_file, script);
        let rc = std::fs::read_to_string(env.home.join(".bashrc")).unwrap();
        assert!(rc.contains("# >>> gpufree completion: bash v1 >>>"));
        assert!(rc.contains("completion.bash"));

        let again = install("bash", &options(), &env).unwrap();
        assert!(!again.changed);
        let rc = std::fs::read_to_string(env.home.join(".bashrc")).unwrap();
        assert_eq!(rc.matches("# >>> gpufree completion: bash").count(), 1);

        let status = status("bash", &options(), &env).unwrap();
        assert_eq!(status.state, "current");
        assert!(status.installed);
    }

    #[test]
    fn bash_completion_dir_install_skips_rc() {
        let temp = Temp::new("bash-comp");
        let mut env = env_at(&temp.0);
        env.bash_completion_installed = true;
        let report = install("bash", &options(), &env).unwrap();
        assert!(report.script_changed);
        assert!(report.rc_file.is_none());
        assert!(!env.home.join(".bashrc").exists());
        let status = status("bash", &options(), &env).unwrap();
        assert_eq!(status.rc_state, "not_required");
        assert_eq!(status.state, "current");
    }

    #[test]
    fn zsh_prefers_oh_my_zsh() {
        let temp = Temp::new("zsh-omz");
        let env = env_at(&temp.0);
        fs::create_dir_all(env.home.join(".oh-my-zsh")).unwrap();
        let report = install("zsh", &options(), &env).unwrap();
        assert!(
            report
                .script_file
                .ends_with(".oh-my-zsh/custom/completions/_gpufree")
        );
        assert!(report.rc_file.is_none());
    }

    #[test]
    fn zsh_without_autoload_dir_adds_fpath_block() {
        let temp = Temp::new("zsh-rc");
        let env = env_at(&temp.0);
        let report = install("zsh", &options(), &env).unwrap();
        assert!(report.script_file.ends_with(".zfunc/_gpufree"));
        let rc = std::fs::read_to_string(env.home.join(".zshrc")).unwrap();
        assert!(rc.contains("fpath=("));
        assert!(rc.contains("compinit"));
    }

    #[test]
    fn fish_never_touches_rc() {
        let temp = Temp::new("fish");
        let env = env_at(&temp.0);
        let report = install("fish", &options(), &env).unwrap();
        assert!(!report.rc_changed);
        assert!(report.rc_file.is_none());
        assert!(
            env.xdg_config
                .join("fish/completions/gpufree.fish")
                .exists()
        );
    }

    #[test]
    fn uninstall_removes_script_and_block() {
        let temp = Temp::new("uninstall");
        let env = env_at(&temp.0);
        install("bash", &options(), &env).unwrap();
        let report = uninstall("bash", &options(), &env).unwrap();
        assert!(report.changed);
        assert!(report.script_removed);
        assert!(report.rc_removed);
        assert!(!env.xdg_data.join("gpufree/completion.bash").exists());
        let rc = std::fs::read_to_string(env.home.join(".bashrc")).unwrap();
        assert!(!rc.contains("gpufree completion"));
        let again = uninstall("bash", &options(), &env).unwrap();
        assert!(!again.changed);
    }

    #[test]
    fn dir_install_writes_only_the_file() {
        let temp = Temp::new("dir");
        let env = env_at(&temp.0);
        let dir = temp.0.join("custom");
        let options = Options {
            dir: Some(dir.clone()),
            ..Options::default()
        };
        let report = install("bash", &options, &env).unwrap();
        assert!(report.directory_only);
        assert!(report.rc_file.is_none());
        assert!(dir.join("gpufree").exists());
        assert!(!env.home.join(".bashrc").exists());
        let report = uninstall("bash", &options, &env).unwrap();
        assert!(report.script_removed);
        assert!(!dir.join("gpufree").exists());
    }

    #[test]
    fn refuses_foreign_script() {
        let temp = Temp::new("foreign");
        let env = env_at(&temp.0);
        let script = env.xdg_data.join("gpufree/completion.bash");
        fs::create_dir_all(script.parent().unwrap()).unwrap();
        fs::write(&script, "# mine\n").unwrap();
        assert!(
            install("bash", &options(), &env)
                .unwrap_err()
                .contains("not created")
        );
        assert!(
            uninstall("bash", &options(), &env)
                .unwrap_err()
                .contains("not created")
        );
        assert_eq!(status("bash", &options(), &env).unwrap().state, "foreign");
    }

    #[test]
    fn malformed_marker_fails_closed() {
        let temp = Temp::new("malformed");
        let env = env_at(&temp.0);
        fs::create_dir_all(env.home.clone()).unwrap();
        fs::write(
            env.home.join(".bashrc"),
            "# >>> gpufree completion: bash v1 >>>\nsource x\n",
        )
        .unwrap();
        assert!(
            install("bash", &options(), &env)
                .unwrap_err()
                .contains("no END")
        );
    }

    #[test]
    fn duplicate_blocks_fail_closed() {
        let temp = Temp::new("dup");
        let env = env_at(&temp.0);
        fs::create_dir_all(env.home.clone()).unwrap();
        let block =
            "# >>> gpufree completion: bash v1 >>>\nx\n# <<< gpufree completion: bash v1 <<<\n";
        fs::write(env.home.join(".bashrc"), format!("{block}{block}")).unwrap();
        assert!(
            install("bash", &options(), &env)
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_rc_is_refused_without_flag() {
        let temp = Temp::new("symlink");
        let env = env_at(&temp.0);
        fs::create_dir_all(env.home.clone()).unwrap();
        let real = env.home.join("dotfiles-bashrc");
        fs::write(&real, "# real\n").unwrap();
        std::os::unix::fs::symlink(&real, env.home.join(".bashrc")).unwrap();
        let err = install("bash", &options(), &env).unwrap_err();
        assert!(err.contains("symlink"));
        let follow = Options {
            follow_symlinks: true,
            ..Options::default()
        };
        install("bash", &follow, &env).unwrap();
        assert!(
            fs::read_to_string(&real)
                .unwrap()
                .contains("gpufree completion")
        );
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_refuses_symlinked_script() {
        let temp = Temp::new("unlink-script");
        let env = env_at(&temp.0);
        let script = env.xdg_data.join("gpufree/completion.bash");
        fs::create_dir_all(script.parent().unwrap()).unwrap();
        let real = temp.0.join("real-completion.bash");
        fs::write(
            &real,
            format!("{SCRIPT_MARKER}; remove with: gpufree completion --uninstall bash\n"),
        )
        .unwrap();
        std::os::unix::fs::symlink(&real, &script).unwrap();
        let options = Options {
            follow_symlinks: true,
            ..Options::default()
        };
        let err = uninstall("bash", &options, &env).unwrap_err();
        assert!(err.contains("symlinked completion file"));
        assert!(real.exists());
    }

    fn first_block(text: &str) -> String {
        let start = text.find("# >>> gpufree completion").unwrap();
        let end = text[start..].find("# <<< gpufree completion").unwrap() + start;
        let end = text[end..].find('\n').unwrap() + end + 1;
        text[start..end].to_string()
    }

    #[test]
    fn status_flags_duplicate_blocks() {
        let temp = Temp::new("status-dup");
        let env = env_at(&temp.0);
        install("bash", &options(), &env).unwrap();
        let rc = env.home.join(".bashrc");
        let text = fs::read_to_string(&rc).unwrap();
        fs::write(&rc, format!("{text}{}", first_block(&text))).unwrap();
        let status = status("bash", &options(), &env).unwrap();
        assert_eq!(status.rc_state, "malformed");
        assert_eq!(status.state, "malformed");
        assert!(!status.installed);
        assert!(status.message.unwrap().contains("duplicate"));
    }

    #[test]
    fn status_reports_rc_problems_even_without_a_script() {
        let temp = Temp::new("status-rc-only");
        let env = env_at(&temp.0);
        fs::create_dir_all(env.home.clone()).unwrap();
        let rc = env.home.join(".bashrc");
        let block =
            "# >>> gpufree completion: bash v1 >>>\nbody\n# <<< gpufree completion: bash v1 <<<\n";
        fs::write(&rc, format!("{block}{block}")).unwrap();
        let report = status("bash", &options(), &env).unwrap();
        assert_eq!(report.script_state, "missing");
        assert_eq!(report.rc_state, "malformed");
        assert_eq!(report.state, "malformed");
        assert!(!report.installed);

        let v99 = "# >>> gpufree completion: bash v99 >>>\nbody\n# <<< gpufree completion: bash v99 <<<\n";
        fs::write(&rc, v99).unwrap();
        let report = status("bash", &options(), &env).unwrap();
        assert_eq!(report.script_state, "missing");
        assert_eq!(report.rc_state, "unsupported_version");
        assert_eq!(report.state, "unsupported_version");
        assert!(!report.installed);
        assert!(report.message.unwrap().contains("v99"));

        // A plain empty rc still means not installed.
        fs::write(&rc, "echo hi\n").unwrap();
        let report = status("bash", &options(), &env).unwrap();
        assert_eq!(report.state, "not_installed");
    }

    #[test]
    fn heredoc_marker_text_is_not_a_managed_block() {
        let temp = Temp::new("heredoc");
        let env = env_at(&temp.0);
        fs::create_dir_all(env.home.clone()).unwrap();
        let rc = env.home.join(".bashrc");
        fs::write(
            &rc,
            "# my rc\ncat <<'INNER'\n# >>> gpufree completion: bash v1 >>>\nuser text that must survive\n# <<< gpufree completion: bash v1 <<<\nINNER\necho after\n",
        )
        .unwrap();

        let report = install("bash", &options(), &env).unwrap();
        assert!(report.changed);
        let text = fs::read_to_string(&rc).unwrap();
        assert!(text.contains("user text that must survive"));
        assert!(text.contains("INNER"));
        assert_eq!(text.matches("# >>> gpufree completion").count(), 2);

        uninstall("bash", &options(), &env).unwrap();
        let text = fs::read_to_string(&rc).unwrap();
        assert!(text.contains("user text that must survive"));
        assert_eq!(text.matches("# >>> gpufree completion").count(), 1);
    }

    fn heredoc(delimiter: &str, strip_tabs: bool) -> Heredoc {
        Heredoc {
            delimiter: delimiter.to_string(),
            strip_tabs,
        }
    }

    #[test]
    fn heredoc_detector_handles_quotes_and_here_strings() {
        let parse = |line: &str| heredoc_at(line, line.find("<<").unwrap()).unwrap().0;
        assert_eq!(parse("cat <<'EOF'"), Some(heredoc("EOF", false)));
        assert_eq!(parse("cat <<-\"END\""), Some(heredoc("END", true)));
        assert_eq!(parse("cat <<EO\"F\""), Some(heredoc("EOF", false)));
        assert_eq!(parse("cat <<< \"text\""), None);
        assert!(heredoc_at("cat <<$VAR", 4).is_err());
        // Quoted occurrences are ignored by the lexer, not by heredoc_at.
        assert!(scan("echo '<< not a heredoc'\n").unwrap().blocks.is_empty());
    }

    #[test]
    fn markers_inside_quoted_or_heredoc_text_are_not_blocks() {
        let marker = "# >>> gpufree completion: bash v1 >>>\nuser text\n# <<< gpufree completion: bash v1 <<<\n";

        let simple = format!("cat <<'INNER'\n{marker}INNER\necho after\n");
        assert!(parse_blocks(&simple).unwrap().is_empty());

        // `#` is literal mid-word, so this starts a heredoc.
        let mid_word = format!("echo foo#bar <<'INNER'\n{marker}INNER\necho after\n");
        assert!(parse_blocks(&mid_word).unwrap().is_empty());

        // A backslash-newline keeps a quoted string open across lines.
        let continued = format!("echo \"hello \\\nworld\" <<'INNER'\n{marker}INNER\n");
        assert!(parse_blocks(&continued).unwrap().is_empty());

        // Inside a multi-line single-quoted string.
        let quoted = format!("echo 'start\n{marker}end'\n");
        assert!(parse_blocks(&quoted).unwrap().is_empty());

        // `$'...'` with an escaped quote stays open across the marker lines.
        let ansi = format!("echo $'it\\'s fine\n{marker}'\n");
        assert!(parse_blocks(&ansi).unwrap().is_empty());
    }

    #[test]
    fn arithmetic_shifts_are_not_heredocs() {
        let text = "HISTSIZE=$((1 << 16))\nlet x=$((2 << 3))\n";
        assert!(scan(text).unwrap().blocks.is_empty());

        let with_block = format!(
            "{text}# >>> gpufree completion: bash v1 >>>\nbody\n# <<< gpufree completion: bash v1 <<<\n"
        );
        assert_eq!(parse_blocks(&with_block).unwrap().len(), 1);
    }

    #[test]
    fn parameter_expansion_is_not_arithmetic() {
        // `((` and `<<` are literal inside `${...}`; both are legal patterns.
        let text = "x=${y%%((*)}\np=foo((bar\nz=${w%%<<*}\nn=${a:-${b:-((}}}\n";
        assert!(scan(text).unwrap().blocks.is_empty());

        let with_block = format!(
            "{text}# >>> gpufree completion: bash v1 >>>\nbody\n# <<< gpufree completion: bash v1 <<<\n"
        );
        assert_eq!(parse_blocks(&with_block).unwrap().len(), 1);
    }

    #[test]
    fn parameter_expansion_install_is_idempotent() {
        let temp = Temp::new("param-install");
        let env = env_at(&temp.0);
        fs::create_dir_all(env.home.clone()).unwrap();
        let rc = env.home.join(".bashrc");
        fs::write(&rc, "x=${y%%((*)}\necho \"$x\"\n").unwrap();

        install("bash", &options(), &env).unwrap();
        install("bash", &options(), &env).unwrap();
        let text = fs::read_to_string(&rc).unwrap();
        assert_eq!(text.matches("# >>> gpufree completion").count(), 1);
        assert_eq!(status("bash", &options(), &env).unwrap().state, "current");

        uninstall("bash", &options(), &env).unwrap();
        let text = fs::read_to_string(&rc).unwrap();
        assert!(!text.contains("gpufree completion"), "{text}");
        assert!(!env.xdg_data.join("gpufree/completion.bash").exists());
    }

    #[test]
    fn parameter_expansion_in_zsh_rc_keeps_the_block_visible() {
        let temp = Temp::new("param-zsh");
        let env = env_at(&temp.0);
        fs::create_dir_all(env.home.clone()).unwrap();
        let rc = env.home.join(".zshrc");
        fs::write(&rc, "p=foo((bar\nx=${p%%((*)}\n").unwrap();

        install("zsh", &options(), &env).unwrap();
        let status = status("zsh", &options(), &env).unwrap();
        assert_eq!(status.state, "current");
        assert_eq!(status.rc_file.as_deref(), Some(rc.as_path()));

        uninstall("zsh", &options(), &env).unwrap();
        let text = fs::read_to_string(&rc).unwrap();
        assert!(!text.contains("gpufree completion"), "{text}");
    }

    #[test]
    fn dynamic_heredoc_delimiters_fail_closed() {
        let err = parse_blocks("cat <<$VAR\n").unwrap_err();
        assert!(err.contains("dynamic"), "{err}");
        let err = parse_blocks("cat <<`cmd`\n").unwrap_err();
        assert!(err.contains("dynamic"), "{err}");
    }

    #[test]
    fn unterminated_parameter_expansion_fails_closed() {
        // Without a closing `}` the rest of the file cannot be classified, so
        // markers after it must not be treated as either managed or user text.
        let text = "x=${a\n# >>> gpufree completion: bash v1 >>>\nbody\n# <<< gpufree completion: bash v1 <<<\n";
        let err = parse_blocks(text).unwrap_err();
        assert!(err.contains("unterminated ${"), "{err}");
        // A mirror line with the expansion closed is still parsed normally.
        let closed = "x=${a}\n# >>> gpufree completion: bash v1 >>>\nbody\n# <<< gpufree completion: bash v1 <<<\n";
        assert_eq!(parse_blocks(closed).unwrap().len(), 1);
    }

    #[test]
    fn arithmetic_shift_install_is_idempotent() {
        let temp = Temp::new("arith-install");
        let env = env_at(&temp.0);
        fs::create_dir_all(env.home.clone()).unwrap();
        let rc = env.home.join(".bashrc");
        fs::write(&rc, "HISTSIZE=$((1 << 16))\n").unwrap();

        install("bash", &options(), &env).unwrap();
        install("bash", &options(), &env).unwrap();
        let text = fs::read_to_string(&rc).unwrap();
        assert_eq!(text.matches("# >>> gpufree completion").count(), 1);
        assert_eq!(status("bash", &options(), &env).unwrap().state, "current");

        uninstall("bash", &options(), &env).unwrap();
        let text = fs::read_to_string(&rc).unwrap();
        assert!(!text.contains("gpufree completion"));
        assert!(!env.xdg_data.join("gpufree/completion.bash").exists());
    }

    #[test]
    fn arithmetic_shift_in_zsh_rc_keeps_the_block_visible() {
        let temp = Temp::new("arith-zsh");
        let env = env_at(&temp.0);
        fs::create_dir_all(env.home.clone()).unwrap();
        let rc = env.home.join(".zshrc");
        fs::write(&rc, "HISTSIZE=$((1 << 16))\n").unwrap();

        install("zsh", &options(), &env).unwrap();
        let status = status("zsh", &options(), &env).unwrap();
        assert_eq!(status.state, "current");
        assert_eq!(status.rc_file.as_deref(), Some(rc.as_path()));

        uninstall("zsh", &options(), &env).unwrap();
        let text = fs::read_to_string(&rc).unwrap();
        assert!(!text.contains("gpufree completion"), "{text}");
    }

    #[test]
    fn fpath_scan_ignores_quoted_and_heredoc_examples() {
        let text = "cat <<'DOC'\nfpath=(/tmp/not-real-fpath $fpath)\nDOC\necho \"fpath=(/tmp/not-real-fpath $fpath)\"\nfpath+=(~/.zsh/completions)\n";
        let scan = scan(text).unwrap();
        let dirs = fpath_dirs_in(text, Path::new("/home/u/.zshrc"), &scan);
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with(".zsh/completions"));
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_keeps_script_when_rc_write_fails() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temp = Temp::new("uninstall-order");
        let env = env_at(&temp.0);
        install("bash", &options(), &env).unwrap();
        let script = env.xdg_data.join("gpufree/completion.bash");
        let mode = fs::metadata(&env.home).unwrap().permissions().mode();
        fs::set_permissions(&env.home, fs::Permissions::from_mode(0o555)).unwrap();
        let result = uninstall("bash", &options(), &env);
        fs::set_permissions(&env.home, fs::Permissions::from_mode(mode)).unwrap();

        assert!(result.is_err());
        assert!(script.exists(), "script must survive a failed rc write");
        let rc = fs::read_to_string(env.home.join(".bashrc")).unwrap();
        assert!(rc.contains("# >>> gpufree completion: bash"));
    }

    #[cfg(unix)]
    #[test]
    fn status_reads_through_a_symlinked_rc() {
        let temp = Temp::new("status-symlink");
        let env = env_at(&temp.0);
        install("bash", &options(), &env).unwrap();
        let rc = env.home.join(".bashrc");
        let real = env.home.join("dotfiles-bashrc");
        fs::rename(&rc, &real).unwrap();
        std::os::unix::fs::symlink(&real, &rc).unwrap();
        let status = status("bash", &options(), &env).unwrap();
        assert_eq!(status.state, "current");
    }

    #[test]
    fn dry_run_writes_nothing() {
        let temp = Temp::new("dry");
        let env = env_at(&temp.0);
        let options = Options {
            dry_run: true,
            ..Options::default()
        };
        let report = install("bash", &options, &env).unwrap();
        assert!(report.changed);
        assert!(!env.xdg_data.join("gpufree/completion.bash").exists());
        assert!(!env.home.join(".bashrc").exists());
    }

    #[test]
    fn stale_script_is_reported() {
        let temp = Temp::new("stale");
        let env = env_at(&temp.0);
        install("bash", &options(), &env).unwrap();
        let script = env.xdg_data.join("gpufree/completion.bash");
        let mut text = fs::read_to_string(&script).unwrap();
        text.push_str("# drift\n");
        fs::write(&script, text).unwrap();
        let status = status("bash", &options(), &env).unwrap();
        assert_eq!(status.script_state, "stale");
        assert_eq!(status.state, "stale");
    }

    #[test]
    fn rc_block_is_found_from_zdotdir() {
        let temp = Temp::new("zdotdir");
        let mut env = env_at(&temp.0);
        env.zdotdir = Some(temp.0.join("zdot"));
        fs::create_dir_all(env.zdotdir.as_ref().unwrap()).unwrap();
        install("zsh", &options(), &env).unwrap();
        assert!(temp.0.join("zdot/.zshrc").exists());
    }

    #[test]
    fn fpath_scan_expands_home_and_relative_paths() {
        let text = "fpath+=(~/.zsh/completions ./local \"$fpath\")\n";
        let scan = scan(text).unwrap();
        let dirs = fpath_dirs_in(text, Path::new("/home/u/.zshrc"), &scan);
        assert_eq!(
            dirs[0],
            Path::new(&std::env::var("HOME").unwrap()).join(".zsh/completions")
        );
        assert_eq!(dirs[1], PathBuf::from("/home/u/local"));
        assert_eq!(dirs.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_keeps_rc_when_script_dir_is_unwritable() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temp = Temp::new("uninstall-order2");
        let env = env_at(&temp.0);
        install("bash", &options(), &env).unwrap();
        let dir = env.xdg_data.join("gpufree");
        let mode = fs::metadata(&dir).unwrap().permissions().mode();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        let result = uninstall("bash", &options(), &env);
        fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();

        assert!(result.is_err());
        assert!(dir.join("completion.bash").exists());
        let rc = fs::read_to_string(env.home.join(".bashrc")).unwrap();
        assert!(rc.contains("# >>> gpufree completion: bash"), "{rc}");
    }
}
