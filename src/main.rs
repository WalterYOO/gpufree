//! gpufree — check GPU availability across SSH hosts.
//!
//! The binary is deliberately thin: clap parsing and exit-code propagation.
//! All logic lives in the library so it can be unit-tested.

use clap::Parser;

fn main() {
    // `__complete` is an internal protocol, not a user-facing command: handle
    // it before clap so a bad TAB never prints a usage error, and so the
    // endpoint is invisible to `--help`.
    let mut argv = std::env::args_os();
    let _program = argv.next();
    if argv.next().is_some_and(|arg| arg == "__complete") {
        let rest: Vec<std::ffi::OsString> = argv.collect();
        std::process::exit(gpufree::completion::engine::complete_main_os(rest));
    }
    let cli = gpufree::cli::Cli::parse();
    // Flush stdout and turn a closed pipe into a quiet exit 0 instead of a
    // panic inside `println!`.
    let code = gpufree::run::run(cli);
    std::process::exit(gpufree::output::stream::finish(code));
}
