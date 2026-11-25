//! Shell completion (DESIGN §17).
//!
//! The shell adapters are deliberately tiny: all command/option knowledge
//! lives in the hidden `gpufree __complete` runtime, which walks the live
//! clap tree instead of duplicating the grammar. `gpufree completion` prints,
//! installs, uninstalls, or inspects those adapters.

pub mod detect;
pub mod engine;
pub mod install;
pub mod shells;

use std::path::PathBuf;

use serde_json::json;

use crate::cli::CompletionArgs;

/// `gpufree completion ...` entry point. Returns the process exit code.
pub fn run(args: &CompletionArgs) -> i32 {
    match run_inner(args) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("gpufree: {err}");
            2
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Print,
    Install,
    Uninstall,
    Status,
}

fn action_of(args: &CompletionArgs) -> Action {
    if args.install {
        Action::Install
    } else if args.uninstall {
        Action::Uninstall
    } else if args.status {
        Action::Status
    } else {
        Action::Print
    }
}

/// Explicit `--rc-file` wins; otherwise `GPUFREE_COMPLETION_RC`. An explicit
/// `--dir` asks for a file-only install, so a globally exported rc variable
/// must not silently turn it into an rc edit.
fn rc_file(args: &CompletionArgs) -> Option<PathBuf> {
    if let Some(path) = &args.rc_file {
        return Some(path.clone());
    }
    if args.dir.is_some() {
        return None;
    }
    std::env::var_os("GPUFREE_COMPLETION_RC")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn run_inner(args: &CompletionArgs) -> Result<(), String> {
    let action = action_of(args);
    let shell_arg = args.shell.as_deref().unwrap_or("auto").trim().to_string();
    match shell_arg.to_ascii_lowercase().as_str() {
        "install" | "uninstall" | "status" | "print" => {
            return Err(format!(
                "unknown shell {shell_arg:?}; --{0} is a flag: try 'gpufree completion --{0}'",
                shell_arg.to_ascii_lowercase()
            ));
        }
        _ => {}
    }

    match action {
        Action::Print => {
            if args.rc_file.is_some() {
                return Err(
                    "--rc-file is only valid with --install, --uninstall or --status".into(),
                );
            }
            if args.dir.is_some() {
                return Err("--dir is only valid with --install or --uninstall".into());
            }
            if args.dry_run {
                return Err("--dry-run is only valid with --install or --uninstall".into());
            }
            if args.follow_symlinks {
                return Err(
                    "--follow-symlinks is only valid with --install, --uninstall or --status"
                        .into(),
                );
            }
        }
        Action::Status => {
            if args.dry_run {
                return Err("--dry-run is only valid with --install or --uninstall".into());
            }
            if args.dir.is_some() {
                return Err("--dir is only valid with --install or --uninstall".into());
            }
        }
        Action::Install | Action::Uninstall => {
            if args.dir.is_some() && args.rc_file.is_some() {
                return Err(
                    "--dir writes only the completion file and cannot be combined with --rc-file"
                        .into(),
                );
            }
        }
    }

    let shell = detect::detect_shell(Some(&shell_arg))?;

    match action {
        Action::Print => {
            let script = shells::generate(&shell);
            if args.json {
                let payload = json!({
                    "ok": true,
                    "command": "completion",
                    "action": "print",
                    "shell": shell,
                    "marker_version": install::MARKER_VERSION,
                    "script": script,
                });
                println!("{payload}");
            } else {
                print!("{script}");
            }
        }
        Action::Install => {
            let env = install::Env::from_process()?;
            let options = install::Options {
                rc_file: rc_file(args),
                dir: args.dir.clone(),
                follow_symlinks: args.follow_symlinks,
                dry_run: args.dry_run,
            };
            let report = install::install(&shell, &options, &env)?;
            if args.json {
                println!("{}", install_json("install", &report));
            } else if !args.quiet {
                println!("{}", format_install(&report));
            }
        }
        Action::Uninstall => {
            let env = install::Env::from_process()?;
            let options = install::Options {
                rc_file: rc_file(args),
                dir: args.dir.clone(),
                follow_symlinks: args.follow_symlinks,
                dry_run: args.dry_run,
            };
            let report = install::uninstall(&shell, &options, &env)?;
            if args.json {
                println!("{}", install_json("uninstall", &report));
            } else if !args.quiet {
                println!("{}", format_uninstall(&report));
            }
        }
        Action::Status => {
            let env = install::Env::from_process()?;
            let options = install::Options {
                rc_file: rc_file(args),
                dir: None,
                follow_symlinks: args.follow_symlinks,
                dry_run: false,
            };
            let report = install::status(&shell, &options, &env)?;
            if args.json {
                let payload = json!({
                    "ok": true,
                    "command": "completion",
                    "action": "status",
                    "shell": report.shell,
                    "state": report.state,
                    "installed": report.installed,
                    "script_file": report.script_file.to_string_lossy(),
                    "script_state": report.script_state,
                    "rc_file": report.rc_file.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "rc_state": report.rc_state,
                    "message": report.message,
                });
                println!("{payload}");
            } else if !args.quiet {
                println!("{}", format_status(&report));
            }
        }
    }
    Ok(())
}

fn install_json(action: &str, report: &install::InstallReport) -> serde_json::Value {
    json!({
        "ok": true,
        "command": "completion",
        "action": action,
        "shell": report.shell,
        "script_file": report.script_file.to_string_lossy(),
        "resolved_script_file": report.resolved_script_file.to_string_lossy(),
        "rc_file": report.rc_file.as_ref().map(|p| p.to_string_lossy().to_string()),
        "directories": report.directories.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>(),
        "changed": report.changed,
        "script_changed": report.script_changed,
        "rc_changed": report.rc_changed,
        "script_removed": report.script_removed,
        "rc_removed": report.rc_removed,
        "directory_only": report.directory_only,
        "dry_run": report.dry_run,
        "marker_version": install::MARKER_VERSION,
        "warnings": report.warnings,
    })
}

fn format_install(report: &install::InstallReport) -> String {
    if !report.changed {
        return format!(
            "{} completion is already up to date in {}",
            report.shell,
            report.script_file.display()
        );
    }
    let prefix = if report.dry_run {
        "would install"
    } else {
        "installed"
    };
    let mut text = format!(
        "{prefix} {} completion in {}",
        report.shell,
        report.script_file.display()
    );
    if let Some(rc) = &report.rc_file {
        text.push_str(&format!(" (configured {})", rc.display()));
    }
    if report.directory_only {
        text.push_str("\nadd this directory to your shell's completion path manually");
    }
    for warning in &report.warnings {
        text.push_str(&format!("\nwarning: {warning}"));
    }
    text
}

fn format_uninstall(report: &install::InstallReport) -> String {
    if !report.changed {
        return format!(
            "no {} completion installation found; nothing to do",
            report.shell
        );
    }
    let prefix = if report.dry_run {
        "would remove"
    } else {
        "removed"
    };
    let mut text = format!("{prefix} {} completion", report.shell);
    if report.script_removed {
        text.push_str(&format!(" from {}", report.script_file.display()));
    }
    if report.rc_removed {
        text.push_str(" (cleaned rc block)");
    }
    text
}

fn format_status(report: &install::StatusReport) -> String {
    let mut text = format!(
        "{} completion: {}\nscript: {} ({})",
        report.shell,
        report.state,
        report.script_file.display(),
        report.script_state
    );
    if let Some(rc) = &report.rc_file {
        text.push_str(&format!("\nrc: {} ({})", rc.display(), report.rc_state));
    } else if report.rc_state != "not_required" {
        text.push_str(&format!("\nrc: {}", report.rc_state));
    }
    if let Some(message) = &report.message {
        text.push_str(&format!("\n{message}"));
    }
    text
}
