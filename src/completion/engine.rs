//! Runtime completion engine behind the hidden `gpufree __complete` command.
//!
//! The engine walks the live clap tree instead of maintaining a second copy of
//! the CLI grammar. Dynamic values (presets, ssh hosts) are read-only config
//! and `~/.ssh/config` metadata; completion never connects to a host, probes a
//! GPU, or writes a file. Any internal failure is silent by design: a broken
//! TAB press must not print a traceback into the user's terminal.

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, Command as ClapCommand, CommandFactory};

use crate::cli::Cli;

const MAX_CANDIDATES: usize = 1000;
const MAX_CANDIDATE_LEN: usize = 4096;
const HEADER: &str = "#gpufree-complete:v1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Plain,
    File,
    Dir,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::File => "file",
            Self::Dir => "dir",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub kind: Kind,
    pub candidates: Vec<String>,
}

fn plain(candidates: Vec<String>) -> Outcome {
    Outcome {
        kind: Kind::Plain,
        candidates,
    }
}

// ---------------------------------------------------------------------------
// Process entry point
// ---------------------------------------------------------------------------

/// Entry point for `gpufree __complete`. Always exits 0 and never requires a
/// successful clap parse, so a TAB press cannot produce a usage error.
pub fn complete_main_os(argv: Vec<OsString>) -> i32 {
    let debug = std::env::var_os("GPUFREE_COMPLETION_DEBUG")
        .map(|value| value == "1")
        .unwrap_or(false);
    let tokens: Vec<String> = argv
        .into_iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();

    let hook = std::panic::take_hook();
    if !debug {
        std::panic::set_hook(Box::new(|_| {}));
    }
    let outcome = std::panic::catch_unwind(move || run(tokens)).unwrap_or_default();
    std::panic::set_hook(hook);

    emit(&outcome);
    0
}

fn emit(outcome: &Outcome) {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let _ = writeln!(out, "{HEADER} kind={}", outcome.kind.as_str());
    for candidate in clean(&outcome.candidates) {
        let _ = writeln!(out, "{candidate}");
    }
    let _ = out.flush();
}

/// Drop empty, over-long, or control-character candidates; deduplicate while
/// preserving order; cap the result so one TAB cannot flood a terminal.
fn clean(candidates: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for candidate in candidates {
        if candidate.is_empty() || candidate.len() > MAX_CANDIDATE_LEN {
            continue;
        }
        if candidate
            .chars()
            .any(|ch| ch.is_control() || ch == '\u{2028}' || ch == '\u{2029}')
        {
            continue;
        }
        if !seen.insert(candidate.clone()) {
            continue;
        }
        out.push(candidate.clone());
        if out.len() >= MAX_CANDIDATES {
            break;
        }
    }
    out
}

#[derive(Default)]
struct RawArgs {
    line: Option<String>,
    point: Option<usize>,
    cword: Option<usize>,
    words: Vec<String>,
}

/// Parse the tiny internal protocol. Unknown tokens before `--` are ignored,
/// mirroring the reference implementation; adapters always pass `--`.
fn parse_raw(tokens: &[String]) -> RawArgs {
    let mut raw = RawArgs::default();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--" => {
                raw.words.extend(tokens[index + 1..].iter().cloned());
                break;
            }
            "--shell" => index += 2,
            "--line" => {
                raw.line = tokens.get(index + 1).cloned();
                index += 2;
            }
            "--point" => {
                raw.point = tokens.get(index + 1).and_then(|value| value.parse().ok());
                index += 2;
            }
            "--cword" => {
                raw.cword = tokens.get(index + 1).and_then(|value| value.parse().ok());
                index += 2;
            }
            _ => index += 1,
        }
    }
    raw
}

fn run(tokens: Vec<String>) -> Outcome {
    let raw = parse_raw(&tokens);
    let (cword, words) = match &raw.line {
        Some(line) => {
            let mut limit = raw.point.unwrap_or(line.len()).min(line.len());
            while !line.is_char_boundary(limit) {
                limit -= 1;
            }
            let words = tokenize(&line[..limit]);
            let cword = words.len().saturating_sub(1);
            (cword, words)
        }
        None => {
            let cword = raw
                .cword
                .unwrap_or_else(|| raw.words.len().saturating_sub(1));
            (cword, raw.words)
        }
    };

    let (cword, words) = strip_program(cword, words);
    let cword = cword.min(words.len());
    let current = words.get(cword).cloned().unwrap_or_default();
    let prior = &words[..cword];
    let dynamic = DynamicValues::load(&words);
    let root = Cli::command();
    candidates(&root, prior, &current, &dynamic)
}

/// A small, non-expanding POSIX-ish tokenizer for `COMP_LINE`.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut last_was_escaped = false;
    let mut started = false;

    for ch in line.chars() {
        if escaped {
            buf.push(ch);
            escaped = false;
            last_was_escaped = true;
            started = true;
        } else {
            last_was_escaped = false;
        }
        if last_was_escaped {
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                buf.push(ch);
            }
        } else if ch == '\\' {
            escaped = true;
            started = true;
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            started = true;
        } else if ch.is_whitespace() {
            if started || !buf.is_empty() {
                words.push(std::mem::take(&mut buf));
                started = false;
            }
        } else {
            buf.push(ch);
            started = true;
        }
    }
    if escaped {
        buf.push('\\');
    }
    if started || !buf.is_empty() || quote.is_some() {
        words.push(buf);
    }
    if line.chars().last().is_some_and(|ch| ch.is_whitespace())
        && quote.is_none()
        && !escaped
        && !last_was_escaped
    {
        words.push(String::new());
    }
    words
}

fn strip_program(cword: usize, words: Vec<String>) -> (usize, Vec<String>) {
    let Some(first) = words.first() else {
        return (cword, words);
    };
    let name = Path::new(first)
        .file_name()
        .map(|part| part.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    if name == "gpufree" {
        let rest = words.into_iter().skip(1).collect();
        return (cword.saturating_sub(1), rest);
    }
    (cword, words)
}

// ---------------------------------------------------------------------------
// Dynamic values
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct DynamicValues {
    pub presets: Vec<String>,
    pub hosts: Vec<String>,
}

impl DynamicValues {
    /// Load preset names and host names for the current command line.
    /// Failures are silent: completion must still offer flags and subcommands.
    pub fn load(words: &[String]) -> Self {
        let config_path = flag_value(words, "--config").map(PathBuf::from);
        let loaded = crate::config::load(config_path.as_deref()).ok();

        let mut presets = Vec::new();
        let mut hosts = BTreeSet::new();
        if let Some(loaded) = &loaded {
            presets.extend(
                loaded
                    .config
                    .presets
                    .keys()
                    .filter(|name| is_safe_dynamic_candidate(name))
                    .cloned(),
            );
            for preset in loaded.config.presets.values() {
                for host in &preset.hosts {
                    if is_safe_dynamic_candidate(host) {
                        hosts.insert(host.clone());
                    }
                }
            }
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Some(home) = home {
            for host in ssh_hosts(&home.join(".ssh/config")) {
                if is_safe_dynamic_candidate(&host) {
                    hosts.insert(host);
                }
            }
        }
        Self {
            presets,
            hosts: hosts.into_iter().collect(),
        }
    }
}

/// Dynamic candidates come from user-writable data (`~/.ssh/config`, a
/// `--config` file), and the adapters insert plain candidates into the command
/// line without adding their own quoting: bash readline may wrap a match in
/// double quotes, but `"$(cmd)"` still runs `cmd` when the line is executed.
/// So a name that a shell could act on is dropped instead of offered. Letters
/// and digits from any script plus the punctuation found in real aliases and
/// preset names are kept; `$`, backticks, quotes, backslashes, globs,
/// `;|&()<>{}`, `=`, `~`, `#`, `!` and whitespace are not.
fn is_safe_dynamic_candidate(candidate: &str) -> bool {
    candidate.chars().all(|ch| {
        if ch.is_ascii() {
            ch.is_ascii_alphanumeric()
                || matches!(ch, '.' | '_' | '-' | '@' | ':' | '+' | '/' | '%')
        } else {
            !ch.is_control() && ch != '\u{2028}' && ch != '\u{2029}'
        }
    })
}

fn flag_value(words: &[String], flag: &str) -> Option<String> {
    let prefix = format!("{flag}=");
    for (index, word) in words.iter().enumerate() {
        if word == flag {
            return words.get(index + 1).cloned();
        }
        if let Some(value) = word.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
    }
    None
}

/// Aliases from `~/.ssh/config`. Wildcard and negated patterns are skipped
/// because completing them would be noise; `Include` is not followed.
fn ssh_hosts(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut hosts = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(keyword) = parts.next() else {
            continue;
        };
        // A `Match` block runs only until the next `Host` or `Match`
        // (ssh_config(5)), so a `Host` line always starts a real block and is
        // a usable alias.
        if !keyword.eq_ignore_ascii_case("host") {
            continue;
        }
        for token in parts {
            if token.starts_with('#') {
                break;
            }
            if token.starts_with('!') || token.contains('*') || token.contains('?') {
                continue;
            }
            hosts.push(token.to_string());
        }
    }
    hosts
}

// ---------------------------------------------------------------------------
// Grammar walking
// ---------------------------------------------------------------------------

struct Walk<'a> {
    cmd: &'a ClapCommand,
    used: HashSet<String>,
    expecting: Option<&'a Arg>,
    positionals: usize,
    positional_only: bool,
}

fn takes_value(arg: &Arg) -> bool {
    matches!(arg.get_action(), ArgAction::Set | ArgAction::Append)
}

fn find_long<'a>(cmd: &'a ClapCommand, root: &'a ClapCommand, long: &str) -> Option<&'a Arg> {
    if let Some(arg) = cmd.get_arguments().find(|arg| arg.get_long() == Some(long)) {
        return Some(arg);
    }
    if std::ptr::eq(cmd, root) {
        return None;
    }
    root.get_arguments()
        .find(|arg| arg.get_long() == Some(long))
}

fn find_short<'a>(cmd: &'a ClapCommand, root: &'a ClapCommand, short: char) -> Option<&'a Arg> {
    if let Some(arg) = cmd
        .get_arguments()
        .find(|arg| arg.get_short() == Some(short))
    {
        return Some(arg);
    }
    if std::ptr::eq(cmd, root) {
        return None;
    }
    root.get_arguments()
        .find(|arg| arg.get_short() == Some(short))
}

fn find_option<'a>(cmd: &'a ClapCommand, root: &'a ClapCommand, token: &str) -> Option<&'a Arg> {
    if let Some(long) = token.strip_prefix("--") {
        return find_long(cmd, root, long);
    }
    let mut chars = token.chars();
    if chars.next() == Some('-') {
        if let Some(short) = chars.next() {
            if chars.next().is_none() {
                return find_short(cmd, root, short);
            }
        }
    }
    None
}

fn is_subcommand<'a>(cmd: &'a ClapCommand, token: &str) -> Option<&'a ClapCommand> {
    cmd.get_subcommands()
        .find(|sub| !sub.is_hide_set() && sub.get_name() == token)
}

fn context<'a>(root: &'a ClapCommand, prior: &[String]) -> Walk<'a> {
    let mut cmd = root;
    let mut used = HashSet::new();
    let mut expecting: Option<&Arg> = None;
    let mut positionals = 0;
    let mut positional_only = false;
    let mut index = 0;
    while index < prior.len() {
        let token = &prior[index];
        if positional_only {
            positionals += 1;
            index += 1;
            continue;
        }
        if token == "--" {
            positional_only = true;
            index += 1;
            continue;
        }
        if token.starts_with("--") && token.contains('=') {
            if let Some(name) = token[2..].split('=').next() {
                if let Some(arg) = find_long(cmd, root, name) {
                    used.insert(arg.get_id().to_string());
                }
            }
            index += 1;
            continue;
        }
        if token.starts_with('-') && token != "-" {
            if let Some(arg) = find_option(cmd, root, token) {
                used.insert(arg.get_id().to_string());
                if takes_value(arg) {
                    if index + 1 < prior.len() {
                        index += 2;
                    } else {
                        expecting = Some(arg);
                        index += 1;
                    }
                } else {
                    index += 1;
                }
                continue;
            }
            index += 1;
            continue;
        }
        if let Some(sub) = is_subcommand(cmd, token) {
            cmd = sub;
            positionals = 0;
            expecting = None;
            index += 1;
            continue;
        }
        positionals += 1;
        index += 1;
    }
    Walk {
        cmd,
        used,
        expecting,
        positionals,
        positional_only,
    }
}

fn positional_arg(cmd: &ClapCommand, index: usize) -> Option<&Arg> {
    let positions: Vec<&Arg> = cmd
        .get_positionals()
        .filter(|arg| !arg.is_hide_set())
        .collect();
    if positions.is_empty() {
        return None;
    }
    if index < positions.len() {
        return Some(positions[index]);
    }
    let last = *positions.last()?;
    let repeatable = last
        .get_num_args()
        .map(|range| range.max_values() > 1)
        .unwrap_or(false);
    repeatable.then_some(last)
}

fn add_option(
    arg: &Arg,
    current: &str,
    used: &HashSet<String>,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    if arg.is_positional() || arg.is_hide_set() {
        return;
    }
    let id = arg.get_id().to_string();
    let repeatable = matches!(arg.get_action(), ArgAction::Append | ArgAction::Count);
    if used.contains(&id) && !repeatable {
        return;
    }
    if let Some(long) = arg.get_long() {
        let option = format!("--{long}");
        if option.starts_with(current) && seen.insert(option.clone()) {
            out.push(option);
        }
    }
    if let Some(short) = arg.get_short() {
        let option = format!("-{short}");
        if option.starts_with(current) && seen.insert(option.clone()) {
            out.push(option);
        }
    }
}

fn option_candidates(
    cmd: &ClapCommand,
    root: &ClapCommand,
    current: &str,
    used: &HashSet<String>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for arg in cmd.get_arguments() {
        add_option(arg, current, used, &mut seen, &mut out);
    }
    if !std::ptr::eq(cmd, root) {
        // Only global options are valid after a subcommand; offering the
        // top-level host/format flags under `doctor` would be a lie.
        for arg in root.get_arguments().filter(|arg| arg.is_global_set()) {
            add_option(arg, current, used, &mut seen, &mut out);
        }
    }
    if "--help".starts_with(current) && seen.insert("--help".to_string()) {
        out.push("--help".to_string());
    }
    // `--version` exists only on the root command; subcommands reject it.
    if std::ptr::eq(cmd, root)
        && "--version".starts_with(current)
        && seen.insert("--version".to_string())
    {
        out.push("--version".to_string());
    }
    out
}

fn possible_values(arg: &Arg, current: &str) -> Option<Vec<String>> {
    let values: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_string())
        .collect();
    if values.is_empty() {
        return None;
    }
    Some(
        values
            .into_iter()
            .filter(|value| value.starts_with(current))
            .collect(),
    )
}

fn comma_candidates(hosts: &[String], current: &str) -> Outcome {
    if let Some(index) = current.rfind(',') {
        let prefix = &current[..=index];
        let tail = &current[index + 1..];
        return plain(
            hosts
                .iter()
                .filter(|host| host.starts_with(tail))
                .map(|host| format!("{prefix}{host}"))
                .collect(),
        );
    }
    plain(
        hosts
            .iter()
            .filter(|host| host.starts_with(current))
            .cloned()
            .collect(),
    )
}

fn value_candidates(arg: &Arg, current: &str, dynamic: &DynamicValues) -> Outcome {
    if let Some(values) = possible_values(arg, current) {
        return plain(values);
    }
    match arg.get_id().as_str() {
        "preset" => plain(
            dynamic
                .presets
                .iter()
                .filter(|name| name.starts_with(current))
                .cloned()
                .collect(),
        ),
        "host" | "positional" => plain(
            dynamic
                .hosts
                .iter()
                .filter(|host| host.starts_with(current))
                .cloned()
                .collect(),
        ),
        "hosts" => comma_candidates(&dynamic.hosts, current),
        "config" | "hosts_file" | "rc_file" => Outcome {
            kind: Kind::File,
            candidates: Vec::new(),
        },
        "dir" => Outcome {
            kind: Kind::Dir,
            candidates: Vec::new(),
        },
        "shell" => plain(
            ["auto", "bash", "zsh", "fish"]
                .into_iter()
                .filter(|shell| shell.starts_with(current))
                .map(str::to_string)
                .collect(),
        ),
        _ => Outcome::default(),
    }
}

/// Compute candidates for `prior` tokens and the token being completed.
pub fn candidates(
    root: &ClapCommand,
    prior: &[String],
    current: &str,
    dynamic: &DynamicValues,
) -> Outcome {
    let walk = context(root, prior);

    if let Some(arg) = walk.expecting {
        return value_candidates(arg, current, dynamic);
    }

    if !walk.positional_only && current.starts_with('-') {
        if current.starts_with("--") && current.contains('=') {
            let index = current.find('=').unwrap();
            let (option, value) = (&current[..index], &current[index + 1..]);
            if let Some(arg) = find_long(walk.cmd, root, &option[2..]) {
                let outcome = value_candidates(arg, value, dynamic);
                if outcome.kind == Kind::Plain {
                    return plain(
                        outcome
                            .candidates
                            .iter()
                            .map(|candidate| format!("{option}={candidate}"))
                            .collect(),
                    );
                }
                // File/dir hints survive the `--opt=` form; each adapter
                // knows how to complete only the value part.
                return outcome;
            }
            return Outcome::default();
        }
        return plain(option_candidates(walk.cmd, root, current, &walk.used));
    }

    let mut out = Vec::new();
    if !walk.positional_only && walk.positionals == 0 {
        for sub in walk.cmd.get_subcommands() {
            if sub.is_hide_set() {
                continue;
            }
            if sub.get_name().starts_with(current) {
                out.push(sub.get_name().to_string());
            }
        }
    }

    if let Some(arg) = positional_arg(walk.cmd, walk.positionals) {
        let outcome = value_candidates(arg, current, dynamic);
        if outcome.kind == Kind::Plain {
            out.extend(outcome.candidates);
        } else if out.is_empty() {
            return outcome;
        }
    }

    if out.is_empty() && current.is_empty() && !walk.positional_only {
        out = option_candidates(walk.cmd, root, "", &walk.used);
    }
    plain(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dynamics() -> DynamicValues {
        DynamicValues {
            presets: vec!["company".into(), "lab".into()],
            hosts: vec!["4090-1".into(), "gpu06".into(), "h20-5".into()],
        }
    }

    fn complete(prior: &[&str], current: &str) -> Outcome {
        let root = Cli::command();
        let prior: Vec<String> = prior.iter().map(|value| value.to_string()).collect();
        candidates(&root, &prior, current, &dynamics())
    }

    #[test]
    fn tokenizer_handles_quotes_escapes_and_trailing_space() {
        assert_eq!(tokenize("gpufree --host a"), ["gpufree", "--host", "a"]);
        assert_eq!(
            tokenize("gpufree --host a "),
            ["gpufree", "--host", "a", ""]
        );
        assert_eq!(tokenize("gpufree 'a b'"), ["gpufree", "a b"]);
        assert_eq!(tokenize("gpufree \"a b"), ["gpufree", "a b"]);
        assert_eq!(tokenize("gpufree a\\ b"), ["gpufree", "a b"]);
        assert_eq!(tokenize(""), Vec::<String>::new());
    }

    #[test]
    fn preset_names_are_dynamic() {
        let outcome = complete(&["--preset"], "");
        assert_eq!(outcome.kind, Kind::Plain);
        assert_eq!(outcome.candidates, ["company", "lab"]);
        assert_eq!(complete(&["--preset"], "co").candidates, ["company"]);
    }

    #[test]
    fn host_flags_and_positionals_use_ssh_aliases() {
        assert_eq!(
            complete(&["--host"], "").candidates,
            ["4090-1", "gpu06", "h20-5"]
        );
        assert_eq!(complete(&["--host"], "gp").candidates, ["gpu06"]);
        assert_eq!(
            complete(&["4090-1"], "").candidates,
            ["4090-1", "gpu06", "h20-5"]
        );
    }

    #[test]
    fn comma_host_list_completes_the_last_item() {
        assert_eq!(
            complete(&["--hosts"], "4090-1,").candidates,
            ["4090-1,4090-1", "4090-1,gpu06", "4090-1,h20-5"]
        );
        assert_eq!(
            complete(&["--hosts"], "4090-1,g").candidates,
            ["4090-1,gpu06"]
        );
    }

    #[test]
    fn enum_values_come_from_clap() {
        assert_eq!(complete(&["--format"], "").candidates, ["table", "json"]);
        assert_eq!(
            complete(&["--transport"], "").candidates,
            ["direct", "remote", "auto"]
        );
        assert_eq!(complete(&["--color"], "n").candidates, ["never"]);
    }

    #[test]
    fn value_options_receive_file_and_dir_hints() {
        assert_eq!(complete(&["--config"], "/tm").kind, Kind::File);
        assert_eq!(complete(&["--hosts-file"], "").kind, Kind::File);
        assert_eq!(complete(&["completion", "--dir"], "").kind, Kind::Dir);
        assert_eq!(complete(&["completion", "--rc-file"], "").kind, Kind::File);
    }

    #[test]
    fn equals_form_completes_the_value() {
        assert_eq!(complete(&[], "--format=j").candidates, ["--format=json"]);
        assert_eq!(complete(&[], "--preset=c").candidates, ["--preset=company"]);
        assert_eq!(complete(&[], "--config=/tm").kind, Kind::File);
        assert_eq!(complete(&[], "--hosts-file=/tm").kind, Kind::File);
        assert_eq!(complete(&["completion"], "--dir=/tm").kind, Kind::Dir);
    }

    #[test]
    fn options_hide_once_used_but_repeatable_flags_stay() {
        let outcome = complete(&["--json"], "--");
        assert!(!outcome.candidates.contains(&"--json".to_string()));
        assert!(outcome.candidates.contains(&"--format".to_string()));
        let repeatable = complete(&["--host", "a"], "--host").candidates;
        assert!(repeatable.contains(&"--host".to_string()));
        assert!(!repeatable.contains(&"--json".to_string()));
    }

    #[test]
    fn subcommands_and_completion_values_are_offered() {
        assert_eq!(
            complete(&[], "c")
                .candidates
                .iter()
                .filter(|candidate| *candidate == "completion")
                .count(),
            1
        );
        assert_eq!(complete(&["presets"], "").candidates, ["list", "validate"]);
        assert_eq!(
            complete(&["completion"], "").candidates,
            ["auto", "bash", "zsh", "fish"]
        );
    }

    #[test]
    fn flags_of_the_selected_subcommand_are_offered() {
        let outcome = complete(&["doctor"], "--");
        assert!(outcome.candidates.contains(&"--probe".to_string()));
        assert!(outcome.candidates.contains(&"--help".to_string()));
        assert!(!outcome.candidates.contains(&"--version".to_string()));
        assert!(
            complete(&[], "--")
                .candidates
                .contains(&"--version".to_string())
        );
    }

    #[test]
    fn double_dash_stops_option_parsing() {
        let outcome = complete(&["--"], "--format");
        assert_eq!(outcome.candidates, Vec::<String>::new());
    }

    #[test]
    fn program_name_is_stripped() {
        assert_eq!(
            strip_program(2, vec!["gpufree".into(), "--host".into(), "".into()]),
            (1, vec!["--host".to_string(), String::new()])
        );
        assert_eq!(
            strip_program(1, vec!["/usr/local/bin/gpufree".into(), "doctor".into()]),
            (0, vec!["doctor".to_string()])
        );
        assert_eq!(
            strip_program(1, vec!["other".into(), "doctor".into()]),
            (1, vec!["other".to_string(), "doctor".to_string()])
        );
    }

    #[test]
    fn ssh_config_parser_skips_patterns() {
        let dir = std::env::temp_dir().join(format!(
            "gpufree-completion-ssh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config");
        std::fs::write(
            &path,
            "Host 4090-1 gpu06\n  HostName x\nHost *.lab !bad\nHost=skipme\nMatch host *.example\n  Host inside-match\nHost h20-5 # comment\n",
        )
        .unwrap();
        let hosts = ssh_hosts(&path);
        // `Host inside-match` ends the preceding Match block and is a real alias.
        assert_eq!(hosts, ["4090-1", "gpu06", "inside-match", "h20-5"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dynamic_candidates_reject_shell_syntax() {
        for good in [
            "4090-1",
            "gpu06",
            "host.example.com",
            "user@host:2222",
            "中文主机",
            "a+b/c%",
            "under_score",
        ] {
            assert!(is_safe_dynamic_candidate(good), "should keep {good:?}");
        }
        for bad in [
            "$(touch x)",
            "`id`",
            "a;b",
            "a&b",
            "a|b",
            "a>b",
            "a<b",
            "a=b",
            "a b",
            "a\tb",
            "'x'",
            "\"x\"",
            "a\\b",
            "*.lab",
            "a?b",
            "[x]",
            "a~b",
            "#x",
            "!x",
            "a{b}",
            "a(b)",
        ] {
            assert!(!is_safe_dynamic_candidate(bad), "should drop {bad:?}");
        }
    }

    #[test]
    fn raw_protocol_reads_line_and_cword() {
        let tokens: Vec<String> = [
            "--shell",
            "bash",
            "--line",
            "gpufree --host ",
            "--point",
            "17",
        ]
        .iter()
        .map(|value| value.to_string())
        .collect();
        let raw = parse_raw(&tokens);
        assert_eq!(raw.line.as_deref(), Some("gpufree --host "));
        assert_eq!(raw.point, Some(17));
        let outcome = run(tokens);
        // The environment's ssh config is real here, so assert the contract
        // (host-shaped candidates) rather than an exact list.
        assert_eq!(outcome.kind, Kind::Plain);
        assert!(
            outcome
                .candidates
                .iter()
                .all(|candidate| !candidate.starts_with('-'))
        );
    }

    #[test]
    fn raw_protocol_reads_words_after_double_dash() {
        let tokens: Vec<String> = [
            "--shell", "fish", "--cword", "2", "--", "gpufree", "--preset", "",
        ]
        .iter()
        .map(|value| value.to_string())
        .collect();
        let outcome = run(tokens);
        // `--preset` must be treated as an option awaiting a value; the real
        // config may contribute extra names.
        assert!(
            !outcome
                .candidates
                .iter()
                .any(|candidate| candidate == "--preset")
        );
        assert!(
            !outcome
                .candidates
                .iter()
                .any(|candidate| candidate.starts_with("--"))
        );
    }

    #[test]
    fn cleanup_drops_control_characters_and_duplicates() {
        let cleaned = clean(&[
            "a".into(),
            "a".into(),
            "".into(),
            "bad\nvalue".into(),
            "ok".into(),
        ]);
        assert_eq!(cleaned, ["a", "ok"]);
    }
}
