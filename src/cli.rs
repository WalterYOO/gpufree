//! Command-line surface (DESIGN §3). Input sources are mutually exclusive via
//! a clap `ArgGroup`; subcommands cover doctor, presets, and init.

use std::path::PathBuf;

use clap::{ArgGroup, Parser, Subcommand, ValueEnum};

use crate::config::{ColorMode, TransportMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
}

#[derive(Debug, Parser)]
#[command(
    name = "gpufree",
    version,
    about = "Check GPU availability across SSH hosts",
    group = ArgGroup::new("source")
        .multiple(false)
        .args(["preset", "host", "hosts", "hosts_file", "positional"])
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Config file path (default: ${XDG_CONFIG_HOME:-~/.config}/gpufree/config.toml)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Preset name from config.toml
    #[arg(long, value_name = "NAME")]
    pub preset: Option<String>,

    /// Host (ssh alias); repeatable
    #[arg(long = "host", value_name = "HOST")]
    pub host: Vec<String>,

    /// Comma-separated host list
    #[arg(long = "hosts", value_name = "HOSTS", value_delimiter = ',')]
    pub hosts: Vec<String>,

    /// Read hosts from a file, one per line; `-` reads stdin
    #[arg(long = "hosts-file", value_name = "PATH")]
    pub hosts_file: Option<String>,

    /// Host (ssh alias) as a positional argument; repeatable
    #[arg(value_name = "HOST")]
    pub positional: Vec<String>,

    /// Output JSON (alias for --format json)
    #[arg(long, conflicts_with = "format")]
    pub json: bool,

    /// Output format
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub format: Option<Format>,

    /// Keep only hosts with at least one free GPU
    #[arg(long)]
    pub free_only: bool,

    /// Exit with code 3 when no GPU is free
    #[arg(long)]
    pub require_free: bool,

    /// Exit with code 1 if any host fails
    #[arg(long)]
    pub strict: bool,

    /// Override the configured transport mode
    #[arg(long, value_enum, value_name = "MODE")]
    pub transport: Option<TransportMode>,

    /// Print the effective configuration (with transport resolution)
    #[arg(long)]
    pub print_config: bool,

    /// Colour output: auto | always | never
    #[arg(long, value_enum, value_name = "WHEN")]
    pub color: Option<ColorMode>,

    /// Render timestamps in UTC
    #[arg(long)]
    pub utc: bool,

    /// Disable ssh ControlMaster multiplexing
    #[arg(long)]
    pub no_mux: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check configuration, ssh, and (optionally) probe hosts
    Doctor {
        /// Actually connect and inspect nvidia-smi
        #[arg(long)]
        probe: bool,
    },
    /// Inspect configured presets
    Presets {
        #[command(subcommand)]
        action: PresetsAction,
    },
    /// Create the config file if it does not exist yet
    Init,
}

#[derive(Debug, Subcommand)]
pub enum PresetsAction {
    /// List presets
    List,
    /// Validate presets
    Validate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("gpufree").chain(args.iter().copied()))
    }

    #[test]
    fn accepts_each_input_source() {
        assert_eq!(
            parse(&["--preset", "company"]).unwrap().preset.as_deref(),
            Some("company")
        );
        assert_eq!(
            parse(&["--host", "a", "--host", "b"]).unwrap().host,
            ["a", "b"]
        );
        assert_eq!(parse(&["--hosts", "a,b"]).unwrap().hosts, ["a", "b"]);
        assert_eq!(parse(&["a", "b"]).unwrap().positional, ["a", "b"]);
        assert_eq!(
            parse(&["--hosts-file", "-"]).unwrap().hosts_file.as_deref(),
            Some("-")
        );
    }

    #[test]
    fn rejects_two_sources() {
        assert!(parse(&["--preset", "p", "--host", "a"]).is_err());
        assert!(parse(&["--host", "a", "b"]).is_err());
        assert!(parse(&["--hosts", "a", "--hosts-file", "x"]).is_err());
    }

    #[test]
    fn json_and_format_conflict() {
        assert!(parse(&["--json", "--format", "table"]).is_err());
        assert!(parse(&["--json", "--format", "json"]).is_err());
    }

    #[test]
    fn parses_numbers_and_subcommands() {
        assert_eq!(
            parse(&["--transport", "auto", "--color", "never"])
                .unwrap()
                .transport,
            Some(TransportMode::Auto)
        );
        assert!(matches!(
            parse(&["doctor", "--probe"]).unwrap().command,
            Some(Command::Doctor { probe: true })
        ));
        assert!(matches!(
            parse(&["presets", "list"]).unwrap().command,
            Some(Command::Presets {
                action: PresetsAction::List
            })
        ));
        assert!(matches!(
            parse(&["init"]).unwrap().command,
            Some(Command::Init)
        ));
    }

    #[test]
    fn unknown_format_is_a_usage_error() {
        assert!(parse(&["--format", "yaml"]).is_err());
    }
}
