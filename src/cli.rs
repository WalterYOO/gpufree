//! Command-line surface. Full flag set lands in M1; this stub keeps
//! `gpufree --help` / `--version` usable while the design is being implemented.

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "gpufree",
    version,
    about = "Check GPU availability across SSH hosts"
)]
pub struct Cli {
    /// Preset name from config.toml
    #[arg(long)]
    pub preset: Option<String>,
}
