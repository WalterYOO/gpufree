//! Conservative shell detection for `gpufree completion`.
//!
//! Explicit choice (positional or `GPUFREE_COMPLETION_SHELL`) wins, then
//! `$SHELL`, then the parent process chain, then the platform default. We
//! never guess that `sh` means bash: a wrong adapter fails silently.

use std::path::Path;
use std::process::Command;

pub const SUPPORTED_SHELLS: [&str; 3] = ["bash", "zsh", "fish"];
const PLANNED_SHELLS: [&str; 2] = ["powershell", "pwsh"];

fn supported_message() -> String {
    "supported shells: bash, zsh, fish".to_string()
}

fn basename(value: &str) -> String {
    let text = value.trim();
    if text.is_empty() {
        return String::new();
    }
    let name = Path::new(text)
        .file_name()
        .map(|part| part.to_string_lossy().into_owned())
        .unwrap_or_else(|| text.to_string())
        .to_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name).to_string();
    name.trim_start_matches('-').to_string()
}

/// Return a supported shell name or a usage error.
pub fn normalize_shell(value: &str) -> Result<String, String> {
    let name = basename(value);
    if SUPPORTED_SHELLS.contains(&name.as_str()) {
        return Ok(name);
    }
    if PLANNED_SHELLS.contains(&name.as_str()) {
        return Err(format!(
            "unsupported shell {name:?}; {} (PowerShell is planned but not implemented in this release)",
            supported_message()
        ));
    }
    Err(format!("unknown shell {value:?}; {}", supported_message()))
}

fn shell_from_env(name: &str) -> Option<&'static str> {
    SUPPORTED_SHELLS
        .iter()
        .copied()
        .find(|shell| *shell == name)
}

fn parent_shell() -> Option<String> {
    let mut pid = std::process::id().to_string();
    // `ps` reports the caller of this process, so start from the parent.
    let mut current = parent_pid(&pid)?;
    for _ in 0..4 {
        if current <= 1 {
            return None;
        }
        pid = current.to_string();
        let name = basename(&command_output(&["ps", "-p", &pid, "-o", "comm="])?);
        if let Some(shell) = shell_from_env(&name) {
            return Some(shell.to_string());
        }
        current = parent_pid(&pid)?;
    }
    None
}

fn parent_pid(pid: &str) -> Option<u32> {
    command_output(&["ps", "-p", pid, "-o", "ppid="])?
        .trim()
        .parse()
        .ok()
}

fn command_output(args: &[&str]) -> Option<String> {
    let output = Command::new(args[0]).args(&args[1..]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn platform_shell() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("zsh")
    } else if cfg!(target_os = "linux") {
        Some("bash")
    } else {
        None
    }
}

/// Resolve `auto`/`None` to one of the supported shells.
pub fn detect_shell(explicit: Option<&str>) -> Result<String, String> {
    if let Some(value) = explicit {
        if !value.trim().is_empty() && !value.trim().eq_ignore_ascii_case("auto") {
            return normalize_shell(value);
        }
    }

    if let Some(value) = std::env::var_os("GPUFREE_COMPLETION_SHELL") {
        let value = value.to_string_lossy();
        if !value.trim().is_empty() {
            // An explicit environment override must not fall back silently.
            return normalize_shell(&value);
        }
    }

    if let Some(value) = std::env::var_os("SHELL") {
        let value = value.to_string_lossy();
        if !value.trim().is_empty() {
            let name = basename(&value);
            if let Some(shell) = shell_from_env(&name) {
                return Ok(shell.to_string());
            }
            if PLANNED_SHELLS.contains(&name.as_str()) {
                return normalize_shell(&name);
            }
            // A real $SHELL the user chose (sh/dash/ksh/...) is authoritative:
            // falling back would install into the wrong file while claiming
            // success.
            return Err(format!(
                "cannot detect a supported shell (SHELL={value}); pass e.g. 'gpufree completion bash' ({})",
                supported_message()
            ));
        }
    }

    if let Some(shell) = parent_shell() {
        return Ok(shell);
    }
    if let Some(shell) = platform_shell() {
        return Ok(shell.to_string());
    }
    Err(format!(
        "cannot detect a supported shell; pass e.g. 'gpufree completion bash' or set GPUFREE_COMPLETION_SHELL ({})",
        supported_message()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths_and_login_names() {
        assert_eq!(normalize_shell("/bin/zsh").unwrap(), "zsh");
        assert_eq!(normalize_shell("-bash").unwrap(), "bash");
        assert_eq!(normalize_shell("FISH.EXE").unwrap(), "fish");
        assert!(normalize_shell("auto").unwrap_err().contains("unknown"));
    }

    #[test]
    fn rejects_unsupported_and_planned() {
        assert!(normalize_shell("sh").is_err());
        assert!(normalize_shell("pwsh").unwrap_err().contains("planned"));
    }

    #[test]
    fn explicit_choice_wins() {
        assert_eq!(detect_shell(Some("bash")).unwrap(), "bash");
        assert_eq!(detect_shell(Some("/usr/local/bin/fish")).unwrap(), "fish");
        assert!(detect_shell(Some("ksh")).is_err());
    }
}
