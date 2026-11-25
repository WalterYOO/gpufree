//! End-to-end tests for the completion surface and the hidden `__complete`
//! endpoint. Everything runs against a temporary HOME/XDG tree.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use serde_json::Value;

struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "gpufree-cli-completion-{tag}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(path.join("home")).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(temp: &Temp) -> Command {
    let mut command = Command::cargo_bin("gpufree").unwrap();
    command
        .env("HOME", temp.path().join("home"))
        .env("XDG_DATA_HOME", temp.path().join("data"))
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env(
            "BASH_COMPLETION_USER_DIR",
            temp.path().join("bash-completion"),
        )
        .env("SHELL", "/bin/zsh")
        .env_remove("ZDOTDIR")
        .env_remove("ZSH_CUSTOM")
        .env_remove("GPUFREE_COMPLETION_SHELL")
        .env_remove("GPUFREE_COMPLETION_BIN");
    command
}

fn json_output(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("valid JSON on stdout")
}

#[test]
fn prints_adapters_for_every_shell() {
    let temp = Temp::new("print");
    for shell in ["bash", "zsh", "fish"] {
        let output = command(&temp).args(["completion", shell]).output().unwrap();
        assert!(output.status.success(), "{shell}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("__complete"), "{shell}");
        assert!(text.ends_with('\n'), "{shell}");
        assert!(!text.ends_with("\n\n"), "{shell}");
    }
}

#[test]
fn print_json_is_machine_readable() {
    let temp = Temp::new("print-json");
    let output = command(&temp)
        .args(["completion", "zsh", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload = json_output(&output);
    assert_eq!(payload["action"], "print");
    assert_eq!(payload["shell"], "zsh");
    assert!(
        payload["script"]
            .as_str()
            .unwrap()
            .contains("#compdef gpufree")
    );
}

#[test]
fn install_status_uninstall_round_trip() {
    let temp = Temp::new("roundtrip");
    let user_dir = temp.path().join("bash-completion");
    let rc = temp.path().join("home/.bashrc");

    let output = command(&temp)
        .args([
            "completion",
            "bash",
            "--install",
            "--rc-file",
            rc.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload = json_output(&output);
    assert_eq!(payload["action"], "install");
    assert_eq!(payload["changed"], true);
    let script = PathBuf::from(payload["script_file"].as_str().unwrap());
    assert_eq!(script, user_dir.join("completions/gpufree"));
    assert!(script.exists());
    assert!(
        fs::read_to_string(&rc)
            .unwrap()
            .contains("gpufree completion: bash")
    );

    // Idempotent.
    let output = command(&temp)
        .args([
            "completion",
            "bash",
            "--install",
            "--rc-file",
            rc.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(json_output(&output)["changed"], false);

    let output = command(&temp)
        .args(["completion", "bash", "--status", "--json"])
        .output()
        .unwrap();
    let payload = json_output(&output);
    assert_eq!(payload["state"], "current");
    assert_eq!(payload["installed"], true);

    let output = command(&temp)
        .args([
            "completion",
            "bash",
            "--uninstall",
            "--rc-file",
            rc.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    let payload = json_output(&output);
    assert_eq!(payload["script_removed"], true);
    assert_eq!(payload["rc_removed"], true);
    assert!(!script.exists());
    assert!(
        !fs::read_to_string(&rc)
            .unwrap()
            .contains("gpufree completion")
    );
}

#[test]
fn dry_run_does_not_write() {
    let temp = Temp::new("dry");
    let output = command(&temp)
        .args(["completion", "bash", "--install", "--dry-run"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!temp.path().join("bash-completion").exists());
    assert!(!temp.path().join("home/.bashrc").exists());
}

#[test]
fn human_output_is_quiet_when_asked() {
    let temp = Temp::new("quiet");
    let output = command(&temp)
        .args(["completion", "bash", "--install", "--quiet"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn rc_environment_variable_is_honoured_but_dir_suppresses_it() {
    let temp = Temp::new("rc-env");
    let rc = temp.path().join("custom-rc");
    let output = command(&temp)
        .env("GPUFREE_COMPLETION_RC", &rc)
        .args(["completion", "bash", "--install"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        fs::read_to_string(&rc)
            .unwrap()
            .contains("gpufree completion: bash")
    );
    command(&temp)
        .env("GPUFREE_COMPLETION_RC", &rc)
        .args(["completion", "bash", "--uninstall"])
        .assert()
        .success();
    assert!(
        !fs::read_to_string(&rc)
            .unwrap()
            .contains("gpufree completion")
    );

    let dir = temp.path().join("only-dir");
    command(&temp)
        .env("GPUFREE_COMPLETION_RC", &rc)
        .args([
            "completion",
            "bash",
            "--install",
            "--dir",
            dir.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(
        !fs::read_to_string(&rc)
            .unwrap()
            .contains("gpufree completion")
    );
}

#[test]
fn usage_errors_exit_two_with_a_hint() {
    let temp = Temp::new("usage");
    command(&temp)
        .args(["completion", "install"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--install is a flag"));
    command(&temp)
        .args(["completion", "ksh"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("supported shells"));
    command(&temp)
        .args(["completion", "bash", "--status", "--dry-run"])
        .assert()
        .code(2);
    command(&temp)
        .args(["completion", "bash", "--install", "--uninstall"])
        .assert()
        .code(2);

    // `--dir` means "write the completion file only"; combining it with an
    // explicit rc target would silently edit shell config (DESIGN §17.1).
    let dir = temp.path().join("only-dir");
    let rc = temp.path().join("custom-rc");
    command(&temp)
        .args([
            "completion",
            "bash",
            "--install",
            "--dir",
            dir.to_str().unwrap(),
            "--rc-file",
            rc.to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot be combined with --rc-file",
        ));
    assert!(!dir.exists());
    assert!(!rc.exists());
}

#[test]
fn hidden_endpoint_never_prints_usage_errors() {
    let temp = Temp::new("endpoint");
    let output = command(&temp)
        .args([
            "__complete",
            "--shell",
            "bash",
            "--line",
            "gpufree --format ",
            "--point",
            "18",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("#gpufree-complete:v1 kind=plain"));
    assert_eq!(lines.collect::<Vec<_>>(), ["table", "json"]);

    // Garbage in, nothing out, still exit 0.
    let output = command(&temp)
        .args(["__complete", "--point", "not-a-number", "--", "--host"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}

#[test]
fn hidden_endpoint_completes_presets_from_config() {
    let temp = Temp::new("presets");
    let config = temp.path().join("gpufree.toml");
    fs::write(
        &config,
        "[presets.company]\nhosts = [\"4090-1\"]\n[presets.lab]\nhosts = [\"gpu06\"]\n",
    )
    .unwrap();
    let line = format!("gpufree --config {} --preset ", config.display());
    let output = command(&temp)
        .args(["__complete", "--shell", "bash", "--line", &line])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let candidates: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(candidates, ["company", "lab"]);
}

#[test]
fn dynamic_candidates_never_carry_shell_syntax() {
    // `~/.ssh/config` and `--config` are data files: a candidate taken from
    // them must stay literal text on the command line (DESIGN §17.2).
    let temp = Temp::new("hostile");
    let ssh_dir = temp.path().join("home/.ssh");
    fs::create_dir_all(&ssh_dir).unwrap();
    fs::write(
        ssh_dir.join("config"),
        "Host ok-host\nHost $(touch${IFS}/tmp/gpufree-pwn)\nHost tick`id`\nHost semi;id\nHost pipe|id\n",
    )
    .unwrap();
    let config = temp.path().join("evil.toml");
    fs::write(
        &config,
        "[presets.\"ok-preset\"]\nhosts = [\"ok:2222\", \"$(boom)\", \"tick`id`\"]\n[presets.'$(pwn)']\nhosts = [\"h2\"]\n",
    )
    .unwrap();

    let line = format!("gpufree --config {} --preset ", config.display());
    let output = command(&temp)
        .args(["__complete", "--shell", "bash", "--line", &line])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let candidates: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(candidates, ["ok-preset"], "stdout: {text}");

    let line = format!("gpufree --config {} --host ", config.display());
    let output = command(&temp)
        .args(["__complete", "--shell", "bash", "--line", &line])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let candidates: Vec<&str> = text.lines().skip(1).collect();
    assert!(candidates.contains(&"ok-host"), "stdout: {text}");
    assert!(candidates.contains(&"ok:2222"), "stdout: {text}");
    for candidate in &candidates {
        assert!(
            !candidate
                .chars()
                .any(|ch| matches!(ch, '$' | '`' | ';' | '|' | '(' | ')')),
            "unsafe candidate {candidate:?} in {text}"
        );
    }

    // End to end through the bash adapter: a `$`-prefixed word must not pick
    // up the filtered names, so Tab leaves the command line unchanged.
    let bin = env!("CARGO_BIN_EXE_gpufree");
    let output = command(&temp)
        .args(["completion", "bash"])
        .output()
        .unwrap();
    let script = temp.path().join("gpufree.bash");
    fs::write(&script, output.stdout).unwrap();
    let line = format!("gpufree --config={} --preset $", config.display());
    let snippet = r#"source "$1"
COMP_WORDBREAKS=$' \t\n"'\''><=;|&(:'
COMP_LINE=$2
COMP_POINT=${#COMP_LINE}
COMPREPLY=()
_gpufree_completion
printf '%s\n' "${#COMPREPLY[@]}" "${COMPREPLY[@]}"
"#;
    for bash in ["/opt/homebrew/bin/bash", "/bin/bash"] {
        if !Path::new(bash).exists() {
            continue;
        }
        let run = Command::new(bash)
            .env("GPUFREE_COMPLETION_BIN", bin)
            .env("HOME", temp.path().join("home"))
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args([
                "--norc",
                "-c",
                snippet,
                "bash",
                script.to_str().unwrap(),
                &line,
            ])
            .output()
            .unwrap();
        assert!(run.status.success(), "{bash}");
        let stdout = String::from_utf8(run.stdout).unwrap();
        assert_eq!(stdout.trim_end(), "0", "{bash} stdout: {stdout}");
    }
}

#[test]
fn help_mentions_completion_but_not_the_hidden_endpoint() {
    let temp = Temp::new("help");
    let output = command(&temp).arg("--help").output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("completion"));
    assert!(!text.contains("__complete"));
}

#[test]
fn completion_help_documents_actions() {
    let temp = Temp::new("completion-help");
    command(&temp)
        .args(["completion", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--install"))
        .stdout(predicate::str::contains("--uninstall"))
        .stdout(predicate::str::contains("--status"))
        .stdout(predicate::str::contains("--rc-file"));
}

#[test]
fn bash_adapter_strips_equals_prefix_for_readline() {
    let temp = Temp::new("bash-eq");
    let bin = env!("CARGO_BIN_EXE_gpufree");
    let output = command(&temp)
        .args(["completion", "bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let script = temp.path().join("gpufree.bash");
    fs::write(&script, output.stdout).unwrap();

    let bash = ["/opt/homebrew/bin/bash", "/bin/bash"]
        .into_iter()
        .find(|path| Path::new(path).exists())
        .unwrap_or("bash");
    if Command::new(bash).arg("--version").output().is_err() {
        return;
    }

    let snippet = r#"source "$1"
COMP_WORDBREAKS=$' \t\n"'\''><=;|&(:'
COMP_LINE='gpufree --format=j'
COMP_POINT=${#COMP_LINE}
COMPREPLY=()
_gpufree_completion
printf 'format:%s\n' "${COMPREPLY[@]}"
COMP_LINE='gpufree --transport=a'
COMP_POINT=${#COMP_LINE}
COMPREPLY=()
_gpufree_completion
printf 'transport:%s\n' "${COMPREPLY[@]}"
COMP_LINE='gpufree --format '
COMP_POINT=${#COMP_LINE}
COMPREPLY=()
_gpufree_completion
printf 'space:%s\n' "${COMPREPLY[@]}"
"#;
    let run = Command::new(bash)
        .env("GPUFREE_COMPLETION_BIN", bin)
        .env("HOME", temp.path().join("home"))
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--norc", "-c", snippet, "bash", script.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "bash failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "format:json", "stdout: {stdout}");
    assert_eq!(lines[1], "transport:auto", "stdout: {stdout}");
    assert!(lines.contains(&"space:table"), "stdout: {stdout}");
    assert!(lines.contains(&"space:json"), "stdout: {stdout}");
}

#[test]
fn bash_adapter_handles_quoted_and_equals_values() {
    let temp = Temp::new("bash-quoted");
    let bin = env!("CARGO_BIN_EXE_gpufree");
    let output = command(&temp)
        .args(["completion", "bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let script = temp.path().join("gpufree.bash");
    fs::write(&script, output.stdout).unwrap();

    let bash = ["/opt/homebrew/bin/bash", "/bin/bash"]
        .into_iter()
        .find(|path| Path::new(path).exists())
        .unwrap_or("bash");
    if Command::new(bash).arg("--version").output().is_err() {
        return;
    }

    let base = temp.path().join("eq");
    fs::create_dir_all(base.join("a=b/sub")).unwrap();
    fs::write(base.join("a=b/uniqfile"), "").unwrap();
    let spaced = temp.path().join("sp ace");
    fs::create_dir_all(spaced.join("a=b")).unwrap();
    fs::write(spaced.join("a=b/spacefile"), "").unwrap();

    let snippet = r#"source "$1"
BASE=$2
SPACED=$3
COMP_WORDBREAKS=$' \t\n"'\''><=;|&(:'
probe() {
    COMP_LINE=$1
    COMP_POINT=${#COMP_LINE}
    COMPREPLY=()
    _gpufree_completion
    printf '%s\n' "${COMPREPLY[@]}"
}
probe "gpufree --config=\"$BASE/a=b/uniq"
printf '%s\n' ---
probe "gpufree --config=$BASE/a=b/uniq"
printf '%s\n' ---
probe "gpufree completion bash --status --dir=\"$BASE/a=b/su"
printf '%s\n' ---
probe "gpufree --config=\"$SPACED/a=b/space"
"#;
    let run = Command::new(bash)
        .env("GPUFREE_COMPLETION_BIN", bin)
        .env("HOME", temp.path().join("home"))
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args([
            "--norc",
            "-c",
            snippet,
            "bash",
            script.to_str().unwrap(),
            base.to_str().unwrap(),
            spaced.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "bash failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).unwrap();
    let sections: Vec<&str> = stdout.split("---\n").collect();
    assert_eq!(
        sections[0].trim_end(),
        base.join("a=b/uniqfile").display().to_string(),
        "stdout: {stdout}"
    );
    assert_eq!(sections[1].trim_end(), "b/uniqfile", "stdout: {stdout}");
    assert_eq!(
        sections[2].trim_end(),
        base.join("a=b/sub").display().to_string(),
        "stdout: {stdout}"
    );
    assert_eq!(
        sections[3].trim_end(),
        spaced.join("a=b/spacefile").display().to_string(),
        "stdout: {stdout}"
    );
}

#[test]
fn bash_adapter_splices_dequoted_completion_results() {
    // Inside a completion function bash returns dequoted filenames
    // (`sp\ ace/a` arrives as `sp ace/a`, `"my dir"` as `my dir`), while
    // readline replaces the raw span after the last unquoted, unescaped `=`.
    // Stub compgen so the splice math is tested without depending on
    // interactive quoting behavior.
    let temp = Temp::new("bash-splice");
    let bin = env!("CARGO_BIN_EXE_gpufree");
    let output = command(&temp)
        .args(["completion", "bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let script = temp.path().join("gpufree.bash");
    fs::write(&script, output.stdout).unwrap();

    let snippet = r#"source "$1"
COMP_WORDBREAKS=$' \t\n"'\''><=;|&(:'
probe() {
    # `fake` must not collide with _gpufree_completion's own locals, which
    # would shadow it inside the stubbed compgen call.
    local fake=$1 line=$2 label=$3
    compgen() { printf '%s\n' "$fake"; }
    COMP_LINE=$line
    COMP_POINT=${#COMP_LINE}
    COMPREPLY=()
    _gpufree_completion
    printf '%s:%s\n' "$label" "${COMPREPLY[*]}"
}
probe '/tmp/w/sp ace/a=b/spacefile' 'gpufree --config=/tmp/w/sp\ ace/a=b/space' escaped-space
probe '/tmp/w/x=y/plainfile' 'gpufree --config=/tmp/w/x\=y/pl' escaped-eq
probe '/tmp/w/my dir/a=b/spacefile' 'gpufree --config=/tmp/w/"my dir"/a=b/space' quoted-dir
probe '/tmp/w/lead/a=b/spacefile' 'gpufree --config="/tmp/w/lead"/a=b/space' leading-quote
probe '/tmp/w/plain/a=b/uniqfile' 'gpufree --config=/tmp/w/plain/a=b/uniq' plain
"#;

    let mut ran = 0;
    for bash in ["/opt/homebrew/bin/bash", "/bin/bash"] {
        if !Path::new(bash).exists() {
            continue;
        }
        let run = Command::new(bash)
            .env("GPUFREE_COMPLETION_BIN", bin)
            .env("HOME", temp.path().join("home"))
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--norc", "-c", snippet, "bash", script.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{bash} failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        let stdout = String::from_utf8(run.stdout).unwrap();
        let expected = [
            "escaped-space:b/spacefile",
            "escaped-eq:/tmp/w/x=y/plainfile",
            "quoted-dir:b/spacefile",
            "leading-quote:b/spacefile",
            "plain:b/uniqfile",
        ];
        assert_eq!(
            stdout.lines().collect::<Vec<_>>(),
            expected,
            "{bash} stdout: {stdout}"
        );
        ran += 1;
    }
    assert!(ran > 0, "no bash available");
}

#[test]
fn bash_adapter_normalizes_comp_point_to_bytes() {
    let temp = Temp::new("bash-point");
    let bin = env!("CARGO_BIN_EXE_gpufree");
    let output = command(&temp)
        .args(["completion", "bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let script = temp.path().join("gpufree.bash");
    fs::write(&script, output.stdout).unwrap();

    let base = temp.path().join("目录/a=b");
    fs::create_dir_all(&base).unwrap();
    fs::write(base.join("文件"), "").unwrap();

    // COMP_POINT is a byte offset on bash < 5 and a character index on
    // bash >= 5; the probe computes it the way the running shell really does,
    // then places the cursor mid-line after multibyte text.
    let snippet = r#"source "$1"
BASE=$2
point() {
    if (( BASH_VERSINFO[0] < 5 )); then
        COMP_POINT=$(LC_ALL=C; printf %s "${#1}")
    else
        COMP_POINT=${#1}
    fi
}
COMP_WORDBREAKS=$' \t\n"'\''><=;|&(:'
P1="gpufree --config=$BASE/文"
COMP_LINE="$P1 --quiet"
point "$P1"
COMPREPLY=()
_gpufree_completion
printf 'path:%s\n' "${COMPREPLY[@]}"
P2='gpufree --host 目录 --format=t'
COMP_LINE="$P2 --quiet"
point "$P2"
COMPREPLY=()
_gpufree_completion
printf 'enum:%s\n' "${COMPREPLY[@]}"
"#;

    let mut ran = 0;
    for bash in ["/opt/homebrew/bin/bash", "/bin/bash"] {
        if !Path::new(bash).exists() {
            continue;
        }
        let run = Command::new(bash)
            .env("GPUFREE_COMPLETION_BIN", bin)
            .env("HOME", temp.path().join("home"))
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("LANG", "en_US.UTF-8")
            .env("LC_ALL", "en_US.UTF-8")
            .args([
                "--norc",
                "-c",
                snippet,
                "bash",
                script.to_str().unwrap(),
                base.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{bash} failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        let stdout = String::from_utf8(run.stdout).unwrap();
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines[0], "path:b/文件", "{bash} stdout: {stdout}");
        assert_eq!(lines[1], "enum:table", "{bash} stdout: {stdout}");
        ran += 1;
    }
    assert!(ran > 0, "no bash available");
}
