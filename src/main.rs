//! gpufree — check GPU availability across SSH hosts.
//!
//! The binary is deliberately thin: clap parsing and exit-code propagation.
//! All logic lives in the library so it can be unit-tested.

use clap::Parser;

fn main() {
    let cli = gpufree::cli::Cli::parse();
    std::process::exit(gpufree::run::run(cli));
}
