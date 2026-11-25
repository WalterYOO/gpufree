//! TOML configuration: presets, probe settings, free-state thresholds, and
//! transport mode. See DESIGN §4. The file is only ever read here; `gpufree
//! init` writes it once and refuses to overwrite.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum TransportMode {
    #[default]
    Direct,
    Remote,
    Auto,
}

impl fmt::Display for TransportMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Direct => "direct",
            Self::Remote => "remote",
            Self::Auto => "auto",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportUsed {
    Direct,
    Remote,
}

impl fmt::Display for TransportUsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Direct => "direct",
            Self::Remote => "remote",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorMode {
    /// Whether ANSI color should be emitted, honouring `NO_COLOR`.
    pub fn enabled(self, stdout_is_tty: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => stdout_is_tty && std::env::var_os("NO_COLOR").is_none(),
        }
    }
}

// ---------------------------------------------------------------------------
// Duration parsing
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(untagged)]
enum DurationInput {
    Text(String),
    Seconds(u64),
}

/// Parse `"6s"`, `"800ms"`, `"1m"`, `"0"`, or a bare integer (seconds).
pub fn parse_duration(input: &str) -> Result<Duration, String> {
    let text = input.trim();
    if text.is_empty() {
        return Err("empty duration".to_string());
    }
    let (number, unit) = if let Some(rest) = text.strip_suffix("ms") {
        (rest, "ms")
    } else if let Some(rest) = text.strip_suffix('s') {
        (rest, "s")
    } else if let Some(rest) = text.strip_suffix('m') {
        (rest, "m")
    } else {
        (text, "s")
    };
    let value: u64 = number
        .trim()
        .parse()
        .map_err(|_| format!("invalid duration `{input}`"))?;
    Ok(match unit {
        "ms" => Duration::from_millis(value),
        "s" => Duration::from_secs(value),
        "m" => Duration::from_secs(value.saturating_mul(60)),
        _ => unreachable!(),
    })
}

fn de_duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let input = DurationInput::deserialize(deserializer)?;
    match input {
        DurationInput::Seconds(secs) => Ok(Duration::from_secs(secs)),
        DurationInput::Text(text) => parse_duration(&text).map_err(serde::de::Error::custom),
    }
}

pub fn format_duration(d: Duration) -> String {
    if d.is_zero() {
        "0s".to_string()
    } else if d.subsec_millis() > 0 && d.as_secs() == 0 {
        format!("{}ms", d.as_millis())
    } else if d.subsec_millis() > 0 {
        format!("{}.{:03}s", d.as_secs(), d.subsec_millis())
    } else {
        format!("{}s", d.as_secs())
    }
}

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreeThresholds {
    #[serde(default = "default_used_mem_max_mb")]
    pub used_mem_max_mb: u64,
    #[serde(default = "default_util_max_pct")]
    pub util_max_pct: u32,
}

fn default_used_mem_max_mb() -> u64 {
    512
}
fn default_util_max_pct() -> u32 {
    10
}

impl Default for FreeThresholds {
    fn default() -> Self {
        Self {
            used_mem_max_mb: default_used_mem_max_mb(),
            util_max_pct: default_util_max_pct(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default = "default_parallel")]
    pub parallel: usize,
    #[serde(default = "default_parallel_remote")]
    pub parallel_remote: usize,
    #[serde(default = "default_connect_timeout", deserialize_with = "de_duration")]
    pub connect_timeout: Duration,
    #[serde(default = "default_total_timeout", deserialize_with = "de_duration")]
    pub total_timeout: Duration,
    #[serde(default = "default_samples")]
    pub samples: u32,
    #[serde(default = "default_sample_interval", deserialize_with = "de_duration")]
    pub sample_interval: Duration,
    #[serde(default = "default_ssh_binary")]
    pub ssh_binary: String,
    #[serde(default = "default_true")]
    pub mux: bool,
    #[serde(default = "default_control_persist", deserialize_with = "de_duration")]
    pub control_persist: Duration,
    #[serde(default)]
    pub color: ColorMode,
    #[serde(default)]
    pub free: FreeThresholds,
}

fn default_parallel() -> usize {
    8
}
fn default_parallel_remote() -> usize {
    4
}
fn default_connect_timeout() -> Duration {
    Duration::from_secs(6)
}
fn default_total_timeout() -> Duration {
    Duration::from_secs(15)
}
fn default_samples() -> u32 {
    2
}
fn default_sample_interval() -> Duration {
    Duration::from_secs(1)
}
fn default_ssh_binary() -> String {
    "ssh".to_string()
}
fn default_true() -> bool {
    true
}
fn default_control_persist() -> Duration {
    Duration::from_secs(60)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            parallel: default_parallel(),
            parallel_remote: default_parallel_remote(),
            connect_timeout: default_connect_timeout(),
            total_timeout: default_total_timeout(),
            samples: default_samples(),
            sample_interval: default_sample_interval(),
            ssh_binary: default_ssh_binary(),
            mux: default_true(),
            control_persist: default_control_persist(),
            color: ColorMode::default(),
            free: FreeThresholds::default(),
        }
    }
}

impl Settings {
    /// Effective mux flag: `control_persist = 0` disables multiplexing too,
    /// and ssh must never receive `ControlPersist=0` (it means "forever").
    pub fn mux_enabled(&self) -> bool {
        self.mux && !self.control_persist.is_zero()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub transport: TransportMode,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub remote_aliases: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub default_preset: Option<String>,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub presets: BTreeMap<String, Preset>,
}

// ---------------------------------------------------------------------------
// Errors and loading
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("cannot read config {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot parse config {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("invalid configuration in {}:\n{issues}", path.display())]
    Validation { path: PathBuf, issues: String },
    #[error("{0}")]
    Message(String),
}

#[derive(Clone, Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub path: PathBuf,
    pub present: bool,
}

/// `${XDG_CONFIG_HOME:-~/.config}/gpufree/config.toml`.
pub fn default_config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let base = PathBuf::from(xdg);
        if !base.as_os_str().is_empty() {
            return Some(base.join("gpufree").join("config.toml"));
        }
    }
    dirs::home_dir().map(|home| home.join(".config").join("gpufree").join("config.toml"))
}

/// Load the configuration. An explicitly requested file must exist; the
/// default path is optional and falls back to built-in defaults.
pub fn load(path_override: Option<&Path>) -> Result<LoadedConfig, ConfigError> {
    let explicit = path_override.is_some();
    let path = match path_override {
        Some(p) => p.to_path_buf(),
        None => default_config_path().ok_or_else(|| {
            ConfigError::Message("cannot determine config path (no home directory)".to_string())
        })?,
    };
    if !path.exists() {
        if explicit {
            return Err(ConfigError::NotFound(path));
        }
        return Ok(LoadedConfig {
            config: Config::default(),
            path,
            present: false,
        });
    }
    let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;
    let config = parse_str_at(&text, &path)?;
    Ok(LoadedConfig {
        config,
        path,
        present: true,
    })
}

/// Parse + validate, labelling errors with `path` for display.
pub fn parse_str_at(text: &str, path: &Path) -> Result<Config, ConfigError> {
    let config: Config = toml::from_str(text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    let config = config.normalized();
    let issues = config.validate();
    if issues.is_empty() {
        Ok(config)
    } else {
        Err(ConfigError::Validation {
            path: path.to_path_buf(),
            issues: issues.join("\n  - "),
        })
    }
}

impl Config {
    pub fn parse_str(text: &str) -> Result<Config, ConfigError> {
        parse_str_at(text, Path::new("<config>"))
    }

    pub fn preset(&self, name: &str) -> Option<&Preset> {
        self.presets.get(name)
    }

    /// A17: duplicate hosts are removed in input order, first occurrence wins.
    fn normalized(mut self) -> Self {
        for preset in self.presets.values_mut() {
            let mut seen = std::collections::BTreeSet::new();
            preset
                .hosts
                .retain(|host| seen.insert(host.trim().to_string()));
            for host in &mut preset.hosts {
                *host = host.trim().to_string();
            }
        }
        self
    }

    /// Readable validation messages; empty means valid.
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();
        let s = &self.settings;

        if !(1..=16).contains(&s.parallel) {
            issues.push("settings.parallel must be between 1 and 16".to_string());
        }
        if !(1..=16).contains(&s.parallel_remote) {
            issues.push("settings.parallel_remote must be between 1 and 16".to_string());
        }
        if s.parallel + s.parallel_remote > 16 {
            issues.push(format!(
                "settings.parallel + settings.parallel_remote = {} exceeds the hard limit of 16 worker threads",
                s.parallel + s.parallel_remote
            ));
        }
        if !(1..=10).contains(&s.samples) {
            issues.push("settings.samples must be between 1 and 10".to_string());
        }
        if s.sample_interval < Duration::from_millis(100)
            || s.sample_interval > Duration::from_secs(10)
        {
            issues.push("settings.sample_interval must be between 100ms and 10s".to_string());
        }
        if s.connect_timeout.is_zero() {
            issues.push("settings.connect_timeout must be greater than 0".to_string());
        }
        if s.total_timeout.is_zero() {
            issues.push("settings.total_timeout must be greater than 0".to_string());
        }
        if s.total_timeout < s.connect_timeout {
            issues.push(format!(
                "settings.total_timeout ({}) is shorter than settings.connect_timeout ({})",
                format_duration(s.total_timeout),
                format_duration(s.connect_timeout)
            ));
        }
        if s.ssh_binary.trim().is_empty() {
            issues.push("settings.ssh_binary must not be empty".to_string());
        }
        if s.free.util_max_pct > 100 {
            issues.push("settings.free.util_max_pct must be between 0 and 100".to_string());
        }

        if let Some(default) = &self.default_preset
            && !self.presets.contains_key(default)
        {
            issues.push(format!(
                "default_preset = {default:?} does not exist in [presets]"
            ));
        }

        for (name, preset) in &self.presets {
            if name.trim().is_empty() {
                issues.push("preset names must not be empty".to_string());
            }
            if preset.hosts.is_empty() {
                issues.push(format!("presets.{name}.hosts must not be empty"));
            }
            for host in &preset.hosts {
                if host.trim().is_empty() {
                    issues.push(format!("presets.{name}.hosts contains an empty host name"));
                } else if host.chars().any(char::is_whitespace) {
                    issues.push(format!(
                        "presets.{name}.hosts entry {host:?} contains whitespace"
                    ));
                }
                if host.starts_with('-') {
                    issues.push(format!(
                        "presets.{name}.hosts entry {host:?} must not start with '-' (option injection)"
                    ));
                }
            }
            for (host, alias) in &preset.remote_aliases {
                if alias.trim().is_empty() {
                    issues.push(format!(
                        "presets.{name}.remote_aliases.{host} must not be empty"
                    ));
                }
                if alias.starts_with('-') {
                    issues.push(format!(
                        "presets.{name}.remote_aliases.{host} = {alias:?} must not start with '-' (option injection)"
                    ));
                }
            }
            if preset.transport == TransportMode::Remote {
                for host in &preset.hosts {
                    if !preset.remote_aliases.contains_key(host) {
                        issues.push(format!(
                            "presets.{name}: transport = \"remote\" requires remote_aliases.{host}"
                        ));
                    }
                }
            }
            if let Some(prefix) = &preset.prefix
                && prefix.contains('\0')
            {
                issues.push(format!("presets.{name}.prefix contains a NUL byte"));
            }
        }

        issues
    }

    /// Effective configuration as TOML, with defaults filled in.
    pub fn to_toml(&self) -> String {
        use fmt::Write as _;
        let mut out = String::new();
        let s = &self.settings;
        match &self.default_preset {
            Some(p) => {
                let _ = writeln!(out, "default_preset = {}", toml_string(p));
            }
            None => out.push_str("# default_preset = \"company\"\n"),
        }
        out.push_str("\n[settings]\n");
        let _ = writeln!(out, "parallel = {}", s.parallel);
        let _ = writeln!(out, "parallel_remote = {}", s.parallel_remote);
        let _ = writeln!(
            out,
            "connect_timeout = {}",
            toml_string(&format_duration(s.connect_timeout))
        );
        let _ = writeln!(
            out,
            "total_timeout = {}",
            toml_string(&format_duration(s.total_timeout))
        );
        let _ = writeln!(out, "samples = {}", s.samples);
        let _ = writeln!(
            out,
            "sample_interval = {}",
            toml_string(&format_duration(s.sample_interval))
        );
        let _ = writeln!(out, "ssh_binary = {}", toml_string(&s.ssh_binary));
        let _ = writeln!(out, "mux = {}", s.mux);
        let _ = writeln!(
            out,
            "control_persist = {}",
            toml_string(&format_duration(s.control_persist))
        );
        let color = match s.color {
            ColorMode::Auto => "auto",
            ColorMode::Always => "always",
            ColorMode::Never => "never",
        };
        let _ = writeln!(out, "color = {}", toml_string(color));
        out.push_str("\n[settings.free]\n");
        let _ = writeln!(out, "used_mem_max_mb = {}", s.free.used_mem_max_mb);
        let _ = writeln!(out, "util_max_pct = {}", s.free.util_max_pct);

        for (name, preset) in &self.presets {
            let _ = writeln!(out, "\n[presets.{}]", toml_key(name));
            if let Some(desc) = &preset.description {
                let _ = writeln!(out, "description = {}", toml_string(desc));
            }
            let _ = writeln!(
                out,
                "transport = {}",
                toml_string(&preset.transport.to_string())
            );
            let hosts: Vec<String> = preset.hosts.iter().map(|h| toml_string(h)).collect();
            let _ = writeln!(out, "hosts = [{}]", hosts.join(", "));
            if let Some(prefix) = &preset.prefix {
                let _ = writeln!(out, "prefix = {}", toml_string(prefix));
            }
            if !preset.remote_aliases.is_empty() {
                let _ = writeln!(out, "\n[presets.{}.remote_aliases]", toml_key(name));
                for (host, alias) in &preset.remote_aliases {
                    let _ = writeln!(out, "{} = {}", toml_key(host), toml_string(alias));
                }
            }
        }
        out
    }
}

fn toml_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn toml_key(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        value.to_string()
    } else {
        toml_string(value)
    }
}

pub const INIT_TEMPLATE: &str = r#"# gpufree configuration
# Default path: ${XDG_CONFIG_HOME:-~/.config}/gpufree/config.toml
# Generate once with `gpufree init`; gpufree never overwrites this file.

# default_preset = "company"

[settings]
parallel        = 8
parallel_remote = 4
connect_timeout = "6s"
total_timeout   = "15s"
samples         = 2
sample_interval = "1s"
ssh_binary      = "ssh"
mux             = true
control_persist = "60s"
color           = "auto"

[settings.free]
used_mem_max_mb = 512
util_max_pct    = 10

# Example preset. Uncomment and edit the hosts to match your ssh config.
# [presets.company]
# description = "Company LAN"
# transport   = "direct"
# hosts       = ["4090-1", "h20-5"]
#
# [presets.company.remote_aliases]
# 4090-1 = "4090-1r"
# h20-5  = "h20-5-remote"
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitOutcome {
    Created(PathBuf),
    Exists(PathBuf),
}

/// Write the starter config. Never overwrites an existing file; an existing
/// file is reported as unchanged (exit code 0, notice on stderr).
pub fn init(path: Option<&Path>) -> Result<InitOutcome, ConfigError> {
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => default_config_path().ok_or_else(|| {
            ConfigError::Message("cannot determine config path (no home directory)".to_string())
        })?,
    };
    if path.exists() {
        return Ok(InitOutcome::Exists(path));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Read {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(&path, INIT_TEMPLATE).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;
    // The template must always parse; catch drift immediately.
    let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;
    parse_str_at(&text, &path)?;
    Ok(InitOutcome::Created(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> &'static str {
        include_str!("../config.example.toml")
    }

    #[test]
    fn example_config_parses_and_validates() {
        let config = Config::parse_str(fixture()).expect("example config must parse");
        assert_eq!(config.default_preset.as_deref(), Some("company"));
        assert_eq!(config.settings.parallel, 8);
        assert_eq!(config.settings.total_timeout, Duration::from_secs(15));
        assert_eq!(config.presets.len(), 5);
        assert_eq!(config.presets["company"].transport, TransportMode::Direct);
        assert_eq!(
            config.presets["company"].remote_aliases["4090-1"],
            "4090-1r"
        );
    }

    #[test]
    fn duration_forms() {
        assert_eq!(parse_duration("6s").unwrap(), Duration::from_secs(6));
        assert_eq!(parse_duration("800ms").unwrap(), Duration::from_millis(800));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert_eq!(parse_duration("7").unwrap(), Duration::from_secs(7));
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn init_template_parses() {
        let config = Config::parse_str(INIT_TEMPLATE).expect("init template must parse");
        assert_eq!(config.settings.parallel, 8);
        assert!(config.presets.is_empty());
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = Config::parse_str("[settings]\nparalel = 4\n").unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn rejects_zero_parallel() {
        let err = Config::parse_str("[settings]\nparallel = 0\n").unwrap_err();
        assert!(err.to_string().contains("between 1 and 16"), "{err}");
    }

    #[test]
    fn rejects_thread_overcommit() {
        let err =
            Config::parse_str("[settings]\nparallel = 12\nparallel_remote = 8\n").unwrap_err();
        assert!(err.to_string().contains("hard limit of 16"), "{err}");
    }

    #[test]
    fn rejects_remote_without_aliases() {
        let text = r#"
[presets.p]
transport = "remote"
hosts = ["a", "b"]

[presets.p.remote_aliases]
a = "ar"
"#;
        let err = Config::parse_str(text).unwrap_err();
        assert!(err.to_string().contains("remote_aliases.b"), "{err}");
    }

    #[test]
    fn accepts_remote_with_complete_aliases() {
        let text = r#"
[presets.p]
transport = "remote"
hosts = ["a"]

[presets.p.remote_aliases]
a = "ar"
"#;
        let config = Config::parse_str(text).unwrap();
        assert_eq!(config.presets["p"].transport, TransportMode::Remote);
    }

    #[test]
    fn rejects_missing_default_preset() {
        let err = Config::parse_str("default_preset = \"nope\"\n").unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn dedupes_hosts_first_occurrence_wins() {
        let text = r#"
[presets.p]
hosts = ["a", "b", "a"]
"#;
        let config = Config::parse_str(text).unwrap();
        assert_eq!(config.presets["p"].hosts, ["a", "b"]);
    }

    #[test]
    fn rejects_option_injection_hosts_and_aliases() {
        let text = r#"
[presets.p]
transport = "remote"
hosts = ["a"]

[presets.p.remote_aliases]
a = "-oProxyCommand=evil"
"#;
        let err = Config::parse_str(text).unwrap_err();
        assert!(err.to_string().contains("option injection"), "{err}");

        let text = r#"
[presets.p]
hosts = ["-oProxyCommand=evil"]
"#;
        let err = Config::parse_str(text).unwrap_err();
        assert!(err.to_string().contains("option injection"), "{err}");
    }

    #[test]
    fn enforces_sample_bounds() {
        for text in [
            "[settings]\nsamples = 0\n",
            "[settings]\nsamples = 11\n",
            "[settings]\nsample_interval = \"50ms\"\n",
            "[settings]\nsample_interval = \"11s\"\n",
        ] {
            assert!(Config::parse_str(text).is_err(), "{text}");
        }
        let ok = Config::parse_str("[settings]\nsamples = 10\nsample_interval = \"10s\"\n");
        assert!(ok.is_ok());
    }

    #[test]
    fn enforces_parallel_bounds_without_clamping() {
        assert!(Config::parse_str("[settings]\nparallel = 17\n").is_err());
        assert!(Config::parse_str("[settings]\nparallel_remote = 0\n").is_err());
    }

    #[test]
    fn total_shorter_than_connect_is_rejected() {
        let text = r#"
[settings]
connect_timeout = "6s"
total_timeout = "2s"
"#;
        let err = Config::parse_str(text).unwrap_err();
        assert!(err.to_string().contains("shorter than"), "{err}");
    }

    #[test]
    fn control_persist_zero_disables_mux() {
        let mut s = Settings::default();
        assert!(s.mux_enabled());
        s.control_persist = Duration::ZERO;
        assert!(!s.mux_enabled());
        s.control_persist = Duration::from_secs(60);
        s.mux = false;
        assert!(!s.mux_enabled());
    }

    #[test]
    fn to_toml_round_trips() {
        let config = Config::parse_str(fixture()).unwrap();
        let rendered = config.to_toml();
        let reparsed = Config::parse_str(&rendered).expect("rendered TOML must parse");
        assert_eq!(reparsed.settings, config.settings);
        assert_eq!(reparsed.presets.len(), config.presets.len());
        assert_eq!(
            reparsed.presets["company"].hosts,
            config.presets["company"].hosts
        );
    }

    #[test]
    fn config_path_uses_xdg() {
        // Safe here: this test only reads the env var; we restore it below.
        // (Tests run in the same process, so avoid mutating process env.)
        let path = default_config_path().expect("home");
        assert!(path.ends_with("gpufree/config.toml"));
    }
}
