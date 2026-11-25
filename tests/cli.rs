//! End-to-end CLI tests against `testdata/fake-ssh` (DESIGN §12).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use serde_json::Value;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("gpufree-test-{}-{}-{tag}", std::process::id(), n));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn fake_ssh() -> String {
    format!("{}/testdata/fake-ssh", env!("CARGO_MANIFEST_DIR"))
}

fn write_config(dir: &TempDir, total_timeout: &str, extra_settings: &str, extra: &str) -> PathBuf {
    let path = dir.file("config.toml");
    let text = format!(
        "[settings]\n\
         ssh_binary = {bin:?}\n\
         mux = false\n\
         sample_interval = \"100ms\"\n\
         connect_timeout = \"1s\"\n\
         total_timeout = {total:?}\n\
         {extra_settings}\n\
         {extra}\n",
        bin = fake_ssh(),
        total = total_timeout
    );
    fs::write(&path, text).unwrap();
    path
}

fn config(dir: &TempDir, extra: &str) -> PathBuf {
    write_config(dir, "5s", "", extra)
}

fn testdata(name: &str) -> String {
    format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Config using an arbitrary ssh binary (for the doctor failure fixtures).
fn config_with_binary(dir: &TempDir, binary: &str, extra: &str) -> PathBuf {
    write_raw(
        dir,
        "config.toml",
        &format!(
            "[settings]\n\
             ssh_binary = {binary:?}\n\
             mux = false\n\
             sample_interval = \"100ms\"\n\
             connect_timeout = \"1s\"\n\
             total_timeout = \"5s\"\n\
             {extra}\n"
        ),
    )
}

fn write_raw(dir: &TempDir, name: &str, text: &str) -> PathBuf {
    let path = dir.file(name);
    fs::write(&path, text).unwrap();
    path
}

fn gpufree(config: &Path) -> Command {
    let mut cmd = Command::cargo_bin("gpufree").unwrap();
    cmd.arg("--config").arg(config);
    cmd
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("failed to run gpufree")
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "invalid JSON ({err})\nstdout:\n{}\nstderr:\n{}",
            stdout(output),
            stderr(output)
        )
    })
}

fn host_keys_present(host: &Value) {
    for key in [
        "host",
        "resolved_alias",
        "transport_used",
        "status",
        "probed_at",
        "duration_ms",
        "hostname",
        "load1",
        "cpus",
        "fields_complete",
        "processes_complete",
        "note",
        "gpus",
        "error",
    ] {
        assert!(host.get(key).is_some(), "host key {key} missing: {host}");
    }
}

fn gpu_keys_present(gpu: &Value) {
    for key in [
        "index",
        "uuid",
        "name",
        "driver",
        "mig",
        "compute_mode",
        "memory_total_mb",
        "memory_used_mb",
        "memory_free_mb",
        "util_pct",
        "temp_c",
        "state",
        "reason",
        "processes",
    ] {
        assert!(gpu.get(key).is_some(), "GPU key {key} missing: {gpu}");
    }
}

// ---------------------------------------------------------------------------
// JSON, stdin script delivery, table output
// ---------------------------------------------------------------------------

#[test]
fn json_normal_probe_has_stable_shape() {
    let dir = TempDir::new("json");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "ok", "--json"]));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let value = json(&out);

    assert_eq!(value["schema"], "gpufree/v1");
    assert_eq!(value["preset"], Value::Null);
    assert_eq!(value["transport"], "direct");
    assert!(value["generated_at"].is_string());
    assert_eq!(value["summary"]["hosts_total"], 1);
    assert_eq!(value["summary"]["hosts_ok"], 1);
    assert_eq!(value["summary"]["hosts_error"], 0);
    assert_eq!(value["summary"]["hosts_shown"], 1);
    assert_eq!(value["summary"]["gpus_total"], 2);
    assert_eq!(value["summary"]["gpus_free"], 1);
    assert_eq!(value["summary"]["gpus_busy"], 1);
    assert_eq!(value["summary"]["gpus_unknown"], 0);

    let host = &value["hosts"][0];
    host_keys_present(host);
    assert_eq!(host["host"], "ok");
    assert_eq!(host["resolved_alias"], "ok");
    assert_eq!(host["transport_used"], "direct");
    assert_eq!(host["status"], "ok");
    assert_eq!(host["hostname"], "ok");
    assert_eq!(host["cpus"], 64);
    assert_eq!(host["fields_complete"], true);
    assert_eq!(host["processes_complete"], true);
    assert_eq!(host["note"], Value::Null);
    assert_eq!(host["error"], Value::Null);

    let free = &host["gpus"][0];
    let busy = &host["gpus"][1];
    gpu_keys_present(free);
    assert_eq!(free["state"], "free");
    assert_eq!(free["reason"], Value::Null);
    assert_eq!(free["processes"], serde_json::json!([]));
    assert_eq!(busy["state"], "busy");
    assert_eq!(busy["reason"], Value::Null);
    assert_eq!(busy["memory_used_mb"], 12288);

    let process = &busy["processes"][0];
    for key in ["gpu_uuid", "pid", "user", "process_name", "used_memory_mb"] {
        assert!(process.get(key).is_some(), "process key {key} missing");
    }
    assert_eq!(process["gpu_uuid"], "GPU-bbbb");
    assert_eq!(process["pid"], 12345);
    assert_eq!(process["user"], "alice");
    assert_eq!(process["process_name"], "python train.py");
    assert_eq!(process["used_memory_mb"], 12000);
}

#[test]
fn script_goes_over_stdin_and_args_avoid_n_and_separator() {
    let dir = TempDir::new("stdin");
    let cfg = config(&dir, "");
    let script_path = dir.file("script.sh");
    let args_path = dir.file("args.txt");
    let out = run(gpufree(&cfg)
        .args(["--host", "ok", "--json"])
        .env("FAKE_SSH_SCRIPT_OUT", &script_path)
        .env("FAKE_SSH_ARGS_OUT", &args_path));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));

    let script = fs::read_to_string(&script_path).unwrap();
    assert!(script.contains("@@GPU"), "script not delivered over stdin");
    assert!(script.contains("@@END"));

    let args = fs::read_to_string(&args_path).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert!(!args.contains(&"-n"), "ssh must never get -n: {args:?}");
    assert!(!args.contains(&"--"), "ssh must never get --: {args:?}");
    assert!(args.contains(&"-T"));
    assert!(args.contains(&"BatchMode=yes"));
    assert_eq!(&args[args.len() - 2..], ["sh", "-s"]);
}

#[test]
fn non_tty_defaults_to_plain_table() {
    let dir = TempDir::new("table");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "ok"]));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("HOST"));
    assert!(text.contains("ok"));
    assert!(text.contains("free"));
    assert!(
        !text.contains('\u{1b}'),
        "plain table must not contain ANSI"
    );
}

#[test]
fn explicit_format_table_bypasses_tui_and_color_always_wins() {
    let dir = TempDir::new("color");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg)
        .args(["--host", "ok", "--format", "table", "--color", "always"])
        .env("NO_COLOR", "1"));
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).contains("\u{1b}[32m"), "color=always must win");
}

#[test]
fn json_and_format_conflict_is_usage_error() {
    let dir = TempDir::new("conflict");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "ok", "--json", "--format", "table"]));
    assert_eq!(code(&out), 2);
}

// ---------------------------------------------------------------------------
// Error classification and edge outputs
// ---------------------------------------------------------------------------

#[test]
fn timeout_is_classified_and_kills_the_group() {
    let dir = TempDir::new("timeout");
    let cfg = write_config(&dir, "1s", "", "");
    let started = Instant::now();
    let out = run(gpufree(&cfg).args(["--host", "slow", "--json"]));
    assert_eq!(code(&out), 1);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout must bound the run"
    );
    let value = json(&out);
    assert_eq!(value["hosts"][0]["status"], "error");
    assert_eq!(value["hosts"][0]["error"]["kind"], "ssh_timeout");
    assert_eq!(value["hosts"][0]["error"]["retryable"], true);
    assert_eq!(value["hosts"][0]["gpus"], serde_json::json!([]));
}

#[test]
fn no_nvidia_smi_variants() {
    let dir = TempDir::new("nogpu");
    let cfg = config(&dir, "");

    let out = run(gpufree(&cfg).args(["--host", "nogpu1", "--json"]));
    assert_eq!(code(&out), 0, "no GPU is not an error");
    let value = json(&out);
    let host = &value["hosts"][0];
    assert_eq!(host["status"], "ok");
    assert_eq!(host["note"], "no_gpu");
    assert_eq!(host["gpus"], serde_json::json!([]));
    assert_eq!(host["fields_complete"], true);
    assert_eq!(host["processes_complete"], true);
    assert_eq!(host["hostname"], "nas");

    let out = run(gpufree(&cfg).args(["--host", "nogpu2", "--json"]));
    assert_eq!(code(&out), 1);
    let value = json(&out);
    let host = &value["hosts"][0];
    assert_eq!(host["status"], "error");
    assert_eq!(host["error"]["kind"], "nvidia_smi_missing");
    assert_eq!(host["error"]["retryable"], false);
}

#[test]
fn driver_error_has_no_unknown_gpu_rows() {
    let dir = TempDir::new("driver");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "drivererr", "--json"]));
    assert_eq!(code(&out), 1);
    let value = json(&out);
    let host = &value["hosts"][0];
    assert_eq!(host["status"], "error");
    assert_eq!(host["error"]["kind"], "nvidia_smi_failed");
    assert_eq!(host["error"]["retryable"], true);
    assert_eq!(host["gpus"], serde_json::json!([]));
    assert!(
        host["error"]["message"]
            .as_str()
            .unwrap()
            .contains("driver")
    );
}

#[test]
fn partial_fields_keep_the_gpu_free_but_incomplete() {
    let dir = TempDir::new("partial");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "partial", "--json"]));
    assert_eq!(code(&out), 0);
    let value = json(&out);
    let host = &value["hosts"][0];
    assert_eq!(host["fields_complete"], false);
    assert_eq!(host["processes_complete"], true);
    let gpu = &host["gpus"][0];
    assert_eq!(gpu["state"], "free");
    assert_eq!(gpu["driver"], Value::Null);
    assert_eq!(gpu["temp_c"], Value::Null);
    assert_eq!(gpu["compute_mode"], Value::Null);
}

#[test]
fn failed_process_query_makes_idle_gpu_unknown() {
    let dir = TempDir::new("noproc");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "noproc", "--json"]));
    assert_eq!(code(&out), 0);
    let value = json(&out);
    let host = &value["hosts"][0];
    assert_eq!(host["processes_complete"], false);
    let gpu = &host["gpus"][0];
    assert_eq!(gpu["state"], "unknown");
    assert_eq!(gpu["reason"], "no_process_data");
    assert_eq!(gpu["processes"], Value::Null);
}

#[test]
fn mig_is_unknown_and_never_free() {
    let dir = TempDir::new("mig");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "mig", "--json"]));
    assert_eq!(code(&out), 0);
    let value = json(&out);
    let gpu = &value["hosts"][0]["gpus"][0];
    assert_eq!(gpu["state"], "unknown");
    assert_eq!(gpu["reason"], "mig");
    assert_eq!(gpu["mig"], true);
    assert_eq!(value["summary"]["gpus_free"], 0);
    assert_eq!(value["summary"]["gpus_unknown"], 1);
}

#[test]
fn two_samples_take_the_worst_reading() {
    let dir = TempDir::new("twosample");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "twosample", "--json"]));
    assert_eq!(code(&out), 0);
    let value = json(&out);
    let gpu = &value["hosts"][0]["gpus"][0];
    assert_eq!(gpu["memory_used_mb"], 900);
    assert_eq!(gpu["util_pct"], 50);
    assert_eq!(gpu["state"], "busy");
}

#[test]
fn commas_in_names_are_preserved() {
    let dir = TempDir::new("comma");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "comma", "--json"]));
    assert_eq!(code(&out), 0);
    let value = json(&out);
    let host = &value["hosts"][0];
    assert_eq!(host["gpus"][0]["name"], "NVIDIA A100, 80GB");
    let process = &host["gpus"][0]["processes"][0];
    assert_eq!(process["process_name"], "python -c foo, bar");
    assert_eq!(process["used_memory_mb"], 2048);
    assert_eq!(process["user"], "carol");
}

#[test]
fn huge_output_is_capped_with_output_limit() {
    let dir = TempDir::new("huge");
    let cfg = config(&dir, "");
    let started = Instant::now();
    let out = run(gpufree(&cfg).args(["--host", "huge", "--json"]));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "output cap must stop the probe promptly"
    );
    assert_eq!(code(&out), 1);
    let value = json(&out);
    let error = &value["hosts"][0]["error"];
    assert_eq!(error["kind"], "output_limit");
    assert_eq!(error["retryable"], false);
}

#[test]
fn truncated_script_is_a_parse_error() {
    let dir = TempDir::new("parsefail");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "parsefail", "--json"]));
    assert_eq!(code(&out), 1);
    let value = json(&out);
    assert_eq!(value["hosts"][0]["error"]["kind"], "parse_error");
    assert_eq!(value["hosts"][0]["error"]["retryable"], false);
}

#[test]
fn auth_and_hostkey_errors_are_not_retryable() {
    let dir = TempDir::new("auth");
    let cfg = config(&dir, "");
    for (alias, kind) in [
        ("authfail", "ssh_auth"),
        ("hostkey", "ssh_hostkey"),
        ("unreach", "ssh_timeout"),
    ] {
        let out = run(gpufree(&cfg).args(["--host", alias, "--json"]));
        assert_eq!(code(&out), 1, "{alias}");
        let value = json(&out);
        let error = &value["hosts"][0]["error"];
        assert_eq!(error["kind"], kind, "{alias}");
    }
}

// ---------------------------------------------------------------------------
// Review regressions: OBJ-1..OBJ-8 and the doctor ssh -V addendum
// ---------------------------------------------------------------------------

#[test]
fn real_early_exit_no_gpu_host_is_ok() {
    // OBJ-1/PRS-03: the real script prints @@NOGPU 1 and exits, with no @@END.
    let dir = TempDir::new("nogpureal");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "nogpureal", "--json"]));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let value = json(&out);
    let host = &value["hosts"][0];
    assert_eq!(host["status"], "ok");
    assert_eq!(host["note"], "no_gpu");
    assert_eq!(host["gpus"], serde_json::json!([]));
    assert_eq!(host["fields_complete"], true);
    assert_eq!(host["processes_complete"], true);
    assert_eq!(host["error"], Value::Null);
    assert_eq!(host["hostname"], "ong");
    assert_eq!(host["cpus"], 8);
}

#[test]
fn real_early_exit_no_gpu_two_is_nvidia_smi_missing() {
    // OBJ-1/PRS-04: @@NOGPU 2 exits early and is an error, not parse_error.
    let dir = TempDir::new("nogpureal2");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "nogpureal2", "--json"]));
    assert_eq!(code(&out), 1);
    let value = json(&out);
    let host = &value["hosts"][0];
    assert_eq!(host["status"], "error");
    assert_eq!(host["error"]["kind"], "nvidia_smi_missing");
    assert_eq!(host["error"]["retryable"], false);
    assert_eq!(host["gpus"], serde_json::json!([]));
}

#[test]
fn doctor_probe_treats_a_real_no_gpu_host_as_healthy() {
    // OBJ-1/DOC-07: this shape used to print parse_error and fail doctor.
    let dir = TempDir::new("doctornogpu");
    let cfg = config(&dir, "[presets.p]\nhosts = [\"nogpureal\"]\n");
    let out = run(gpufree(&cfg).args(["doctor", "--probe"]));
    assert_eq!(
        code(&out),
        0,
        "stdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
    let text = stdout(&out);
    assert!(text.contains("ok · no GPU"), "{text}");
    assert!(!text.contains("parse_error"), "{text}");
    assert!(text.contains("all checks passed"), "{text}");
}

#[test]
fn sub_second_sample_interval_reaches_the_script() {
    // OBJ-2/PRS-02: 500ms must render as `sleep 0.5`, not `sleep 1`.
    let dir = TempDir::new("interval");
    let cfg = write_raw(
        &dir,
        "config.toml",
        &format!(
            "[settings]\nssh_binary = {bin:?}\nmux = false\nsamples = 3\nsample_interval = \"500ms\"\nconnect_timeout = \"1s\"\ntotal_timeout = \"5s\"\n",
            bin = fake_ssh()
        ),
    );
    let script_path = dir.file("script.sh");
    let out = run(gpufree(&cfg)
        .args(["--host", "ok", "--json"])
        .env("FAKE_SSH_SCRIPT_OUT", &script_path));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let script = fs::read_to_string(&script_path).unwrap();
    assert!(script.contains("sleep 0.5"), "{script}");
    assert!(script.contains("done_samples\" -lt 3"));
}

#[test]
fn quoted_process_names_survive_the_round_trip() {
    // OBJ-3/PRS-15.
    let dir = TempDir::new("quoted");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "quoted", "--json"]));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let value = json(&out);
    let process = &value["hosts"][0]["gpus"][0]["processes"][0];
    assert_eq!(process["process_name"], "python train.py --name \"a,b\"");
    assert_eq!(process["used_memory_mb"], 2048);
    assert_eq!(process["user"], "dave");
}

#[test]
fn duplicate_gpu_section_is_a_parse_error() {
    // OBJ-4/PRS-12: both sections have rc=0, so this must not be
    // nvidia_smi_failed.
    let dir = TempDir::new("dup");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "dup", "--json"]));
    assert_eq!(code(&out), 1);
    let value = json(&out);
    let host = &value["hosts"][0];
    assert_eq!(host["status"], "error");
    assert_eq!(host["error"]["kind"], "parse_error");
    assert_eq!(host["error"]["retryable"], false);
    assert!(
        host["error"]["message"]
            .as_str()
            .unwrap()
            .contains("duplicate @@GPU")
    );
}

#[test]
fn doctor_fails_when_ssh_g_cannot_resolve_a_host() {
    // OBJ-5/DOC-03: a failed `ssh -G` is a failed check, not "?" and rc 0.
    let dir = TempDir::new("docg");
    let cfg = config_with_binary(
        &dir,
        &testdata("ssh-gfail"),
        "[presets.p]\nhosts = [\"nomap\"]\n",
    );
    let out = run(gpufree(&cfg).arg("doctor"));
    assert_eq!(code(&out), 1, "stdout: {}", stdout(&out));
    let text = stdout(&out);
    assert!(text.contains("ERROR: preset p host nomap"), "{text}");
    assert!(text.contains("ssh -G"), "{text}");
    assert!(text.contains("check(s) failed"), "{text}");
    assert!(!text.contains("all checks passed"), "{text}");
}

#[test]
fn doctor_fails_when_ssh_version_exits_nonzero() {
    // A28/DOC-04b addendum.
    let dir = TempDir::new("docv");
    let cfg = config_with_binary(&dir, &testdata("ssh-vfail"), "");
    let out = run(gpufree(&cfg).arg("doctor"));
    assert_eq!(code(&out), 1, "stdout: {}", stdout(&out));
    let text = stdout(&out);
    assert!(text.contains("ssh version: FAILED (exit 1)"), "{text}");
    assert!(text.contains("check(s) failed"), "{text}");
    assert!(!text.contains("all checks passed"), "{text}");
}

#[test]
fn table_output_never_contains_remote_control_sequences() {
    // OBJ-8/SEC-08: the GPU name carries ESC ] 0 ; PWNED BEL.
    let dir = TempDir::new("evil");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "evil"]));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains('\u{1b}'), "raw ESC leaked: {text:?}");
    assert!(!text.contains('\u{7}'), "raw BEL leaked: {text:?}");
    assert!(text.contains("PWNED"), "text should survive sanitization");

    // JSON keeps the raw value; serde escapes it instead (A26).
    let escaped = run(gpufree(&cfg).args(["--host", "evil", "--json"]));
    assert_eq!(code(&escaped), 0);
    let value = json(&escaped);
    let name = value["hosts"][0]["gpus"][0]["name"].as_str().unwrap();
    assert!(
        name.contains('\u{1b}') && name.contains('\u{7}'),
        "{name:?}"
    );
}

// ---------------------------------------------------------------------------
// Summary, filtering and exit codes
// ---------------------------------------------------------------------------

#[test]
fn free_only_filters_hosts_but_not_summary_or_exit_code() {
    let dir = TempDir::new("freeonly");
    let cfg = config(&dir, "");
    let out =
        run(gpufree(&cfg).args(["--host", "free", "--host", "busy", "--free-only", "--json"]));
    assert_eq!(code(&out), 0);
    let value = json(&out);
    assert_eq!(value["hosts"].as_array().unwrap().len(), 1);
    assert_eq!(value["hosts"][0]["host"], "free");
    assert_eq!(value["summary"]["hosts_shown"], 1);
    assert_eq!(value["summary"]["hosts_total"], 2);
    assert_eq!(value["summary"]["gpus_total"], 2);
    assert_eq!(value["summary"]["gpus_free"], 1);

    let out = run(gpufree(&cfg).args([
        "--host",
        "free",
        "--host",
        "busy",
        "--free-only",
        "--require-free",
        "--json",
    ]));
    assert_eq!(code(&out), 0);
}

#[test]
fn require_free_exits_three_only_when_some_host_succeeded() {
    let dir = TempDir::new("require");
    let cfg = config(&dir, "");

    let out = run(gpufree(&cfg).args(["--host", "busy", "--require-free", "--json"]));
    assert_eq!(code(&out), 3);
    assert_eq!(json(&out)["summary"]["gpus_free"], 0);

    // All failed: 1 wins over 3 (A4 / §3.1).
    let out = run(gpufree(&cfg).args(["--host", "authfail", "--require-free", "--json"]));
    assert_eq!(code(&out), 1);
}

#[test]
fn strict_only_changes_the_exit_code() {
    let dir = TempDir::new("strict");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "ok", "--host", "authfail", "--json"]));
    assert_eq!(code(&out), 0, "one success is enough without --strict");
    let out = run(gpufree(&cfg).args(["--host", "ok", "--host", "authfail", "--strict", "--json"]));
    assert_eq!(code(&out), 1);
}

#[test]
fn all_failed_is_one() {
    let dir = TempDir::new("allfail");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "authfail", "--host", "hostkey", "--json"]));
    assert_eq!(code(&out), 1);
    assert_eq!(json(&out)["summary"]["hosts_ok"], 0);
}

// ---------------------------------------------------------------------------
// Sources, config, dedupe, option injection
// ---------------------------------------------------------------------------

#[test]
fn no_source_prints_examples_and_exits_two() {
    let dir = TempDir::new("nosource");
    let cfg = config(&dir, "");
    let out = run(&mut gpufree(&cfg));
    assert_eq!(code(&out), 2);
    let err = stderr(&out);
    assert!(err.contains("Examples"), "{err}");
    assert!(err.contains("--preset company"), "{err}");
}

#[test]
fn default_preset_is_used_when_no_source_is_given() {
    let dir = TempDir::new("default");
    let cfg = write_raw(
        &dir,
        "config.toml",
        &format!(
            "default_preset = \"p\"\n\n[settings]\nssh_binary = {bin:?}\nmux = false\nsample_interval = \"100ms\"\nconnect_timeout = \"1s\"\ntotal_timeout = \"5s\"\n\n[presets.p]\nhosts = [\"free\"]\n",
            bin = fake_ssh()
        ),
    );
    let out = run(gpufree(&cfg).arg("--json"));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let value = json(&out);
    assert_eq!(value["preset"], "p");
    assert_eq!(value["hosts"][0]["host"], "free");
}

#[test]
fn unknown_preset_is_a_config_error() {
    let dir = TempDir::new("badpreset");
    let cfg = config(&dir, "[presets.p]\nhosts = [\"ok\"]\n");
    let out = run(gpufree(&cfg).args(["--preset", "nope", "--json"]));
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("not found"));
}

#[test]
fn invalid_config_exits_two() {
    let dir = TempDir::new("badconfig");
    let cfg = write_raw(&dir, "config.toml", "[settings]\nsamples = 0\n");
    let out = run(gpufree(&cfg).args(["--host", "ok", "--json"]));
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("samples"), "{}", stderr(&out));
}

#[test]
fn repeated_hosts_are_deduplicated() {
    let dir = TempDir::new("dedupe");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host", "ok", "--host", "ok", "--json"]));
    assert_eq!(code(&out), 0);
    assert_eq!(json(&out)["summary"]["hosts_total"], 1);
}

#[test]
fn leading_dash_hosts_are_rejected_before_ssh() {
    let dir = TempDir::new("injection");
    let cfg = config(&dir, "");
    let out = run(gpufree(&cfg).args(["--host=-oProxyCommand=evil", "--json"]));
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("option injection"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn hosts_file_handles_bom_crlf_comments_and_blank_lines() {
    let dir = TempDir::new("hostsfile");
    let cfg = config(&dir, "");
    let path = dir.file("hosts.txt");
    let mut bytes = Vec::from("\u{feff}".as_bytes());
    bytes.extend_from_slice(b"ok\r\n# comment\r\n\r\n  free  \r\n");
    fs::write(&path, bytes).unwrap();

    let out = run(gpufree(&cfg).args(["--hosts-file", path.to_str().unwrap(), "--json"]));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let value = json(&out);
    assert_eq!(value["summary"]["hosts_total"], 2);
    assert_eq!(value["hosts"][0]["host"], "ok");
    assert_eq!(value["hosts"][1]["host"], "free");
}

#[test]
fn hosts_file_dash_reads_stdin() {
    let dir = TempDir::new("hostsstdin");
    let cfg = config(&dir, "");
    let mut cmd = gpufree(&cfg);
    cmd.args(["--hosts-file", "-", "--json"]);
    cmd.write_stdin("ok\nfree\n");
    let out = run(&mut cmd);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(json(&out)["summary"]["hosts_total"], 2);
}

#[test]
fn remote_transport_uses_the_preset_alias() {
    let dir = TempDir::new("remote");
    let cfg = config(
        &dir,
        "[presets.p]\ntransport = \"remote\"\nhosts = [\"ok\"]\n\n[presets.p.remote_aliases]\nok = \"ok-remote\"\n",
    );
    let out = run(gpufree(&cfg).args(["--preset", "p", "--json"]));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let value = json(&out);
    let host = &value["hosts"][0];
    assert_eq!(host["transport_used"], "remote");
    assert_eq!(host["resolved_alias"], "ok-remote");
    assert_eq!(value["transport"], "remote");
}

#[test]
fn remote_transport_without_mapping_aborts_before_probing() {
    let dir = TempDir::new("nomap");
    let cfg = config(&dir, "");
    let pidfile = dir.file("pids");
    let out = run(gpufree(&cfg)
        .args(["--host", "ok", "--transport", "remote", "--json"])
        .env("FAKE_SSH_PIDFILE", &pidfile));
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("no remote_aliases entry"),
        "{}",
        stderr(&out)
    );
    assert!(!pidfile.exists(), "no probe may start");
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

#[test]
fn init_creates_once_and_never_overwrites() {
    let dir = TempDir::new("init");
    let path = dir.file("gpufree/config.toml");

    let out = run(gpufree(&path).arg("init"));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(path.exists());
    let first = fs::read_to_string(&path).unwrap();
    assert!(first.contains("[settings]"));

    let out = run(gpufree(&path).arg("init"));
    assert_eq!(code(&out), 0, "existing config is not an error (A6)");
    assert!(stderr(&out).contains("already exists"));
    assert_eq!(fs::read_to_string(&path).unwrap(), first);

    // The generated template must be valid.
    let out = run(gpufree(&path).args(["presets", "validate"]));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
}

#[test]
fn presets_list_and_validate_exit_codes() {
    let dir = TempDir::new("presets");
    let cfg = config(&dir, "[presets.p]\nhosts = [\"ok\"]\n");
    let out = run(gpufree(&cfg).args(["presets", "list"]));
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).contains("p"));

    let out = run(gpufree(&cfg).args(["presets", "validate"]));
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).contains("configuration valid"));

    // Content invalid → 1.
    let invalid = write_raw(
        &dir,
        "invalid.toml",
        "[settings]\nssh_binary = \"ssh\"\n\n[presets.p]\ntransport = \"remote\"\nhosts = [\"a\"]\n",
    );
    let out = run(gpufree(&invalid).args(["presets", "validate"]));
    assert_eq!(code(&out), 1, "stderr: {}", stderr(&out));

    // Malformed TOML → 2.
    let malformed = write_raw(&dir, "malformed.toml", "this is not toml [[[");
    let out = run(gpufree(&malformed).args(["presets", "validate"]));
    assert_eq!(code(&out), 2);
}

#[test]
fn doctor_passes_with_a_working_ssh_and_probes() {
    let dir = TempDir::new("doctor");
    let cfg = config(&dir, "[presets.p]\nhosts = [\"ok\"]\n");
    let out = run(gpufree(&cfg).arg("doctor"));
    assert_eq!(
        code(&out),
        0,
        "stdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
    let text = stdout(&out);
    assert!(text.contains("ssh version: OpenSSH_9.9p1 Fake"));
    assert!(text.contains("preset p"));
    assert!(text.contains("all checks passed"));

    let out = run(gpufree(&cfg).args(["doctor", "--probe"]));
    assert_eq!(code(&out), 0, "stdout: {}", stdout(&out));
    assert!(stdout(&out).contains("ok · 2 GPUs"));
}

#[test]
fn doctor_flags_batchmode_incompatible_hosts() {
    let dir = TempDir::new("doctorauth");
    let cfg = config(&dir, "[presets.p]\nhosts = [\"authfail\"]\n");
    let out = run(gpufree(&cfg).args(["doctor", "--probe"]));
    assert_eq!(code(&out), 1);
    let text = stdout(&out);
    assert!(text.contains("ssh_auth"), "{text}");
    assert!(text.contains("needs password/2FA"), "{text}");
}

#[test]
fn print_config_shows_transport_resolution() {
    let dir = TempDir::new("printconfig");
    let cfg = config(
        &dir,
        "[presets.p]\ntransport = \"remote\"\nhosts = [\"ok\"]\n\n[presets.p.remote_aliases]\nok = \"ok-remote\"\n",
    );
    let out = run(gpufree(&cfg).arg("--print-config"));
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("[resolved]"));
    assert!(text.contains("transport=remote"), "{text}");
    assert!(text.contains("-> ok-remote"), "{text}");
    assert!(text.contains("proxied=true"), "{text}");
    assert!(text.contains("no secrets"));
}

// ---------------------------------------------------------------------------
// SIGINT cleanup
// ---------------------------------------------------------------------------

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn pid_alive(pid: &str) -> bool {
    StdCommand::new("kill")
        .arg("-0")
        .arg(pid)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[test]
fn sigint_leaves_no_orphan_ssh_processes() {
    let dir = TempDir::new("sigint");
    let cfg = write_config(&dir, "30s", "", "");
    let pidfile = dir.file("pids");

    let mut child = StdCommand::new(env!("CARGO_BIN_EXE_gpufree"))
        .arg("--config")
        .arg(&cfg)
        .args(["--host", "slow", "--json"])
        .env("FAKE_SSH_PIDFILE", &pidfile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn gpufree");

    let found = wait_until(Duration::from_secs(5), || {
        fs::read_to_string(&pidfile)
            .map(|text| text.split_whitespace().count() == 2)
            .unwrap_or(false)
    });
    assert!(found, "fake ssh never recorded its pid");
    let pids = fs::read_to_string(&pidfile).unwrap();
    let pids: Vec<&str> = pids.split_whitespace().collect();

    let status = StdCommand::new("kill")
        .arg("-INT")
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success());

    let exited = wait_until(Duration::from_secs(5), || {
        child.try_wait().map(|s| s.is_some()).unwrap_or(false)
    });
    assert!(exited, "gpufree did not exit after SIGINT");
    let exit = child.wait().unwrap();
    assert_eq!(exit.code(), Some(130), "SIGINT should exit 130");

    let gone = wait_until(Duration::from_secs(5), || {
        !pids.iter().any(|pid| pid_alive(pid))
    });
    assert!(gone, "orphan ssh/sleep processes remain: {pids:?}");
}
