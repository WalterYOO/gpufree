//! Plain-text table for non-TTY output (DESIGN §3.2). No ANSI unless colour
//! is explicitly enabled and stdout is a terminal.

use crate::model::{HostResult, Snapshot, State};
use crate::output::sanitize::clean;

pub fn render(snapshot: &Snapshot, color: bool) -> String {
    let host_w = snapshot
        .hosts
        .iter()
        .map(|h| h.host.chars().count())
        .max()
        .unwrap_or(4)
        .clamp(4, 28);
    let name_w = snapshot
        .hosts
        .iter()
        .flat_map(|h| h.gpus.iter())
        .filter_map(|g| g.name.as_ref())
        .map(|n| n.chars().count())
        .max()
        .unwrap_or(5)
        .clamp(5, 24);

    let mut out = String::new();
    out.push_str(&format!(
        "{:<host_w$}  {:<4}  {:<name_w$}  {:<13}  {:>4}  {:>4}  {:<13}  {}\n",
        "HOST", "IDX", "MODEL", "FREE/TOTAL", "UTIL", "TEMP", "STATE", "USERS"
    ));
    out.push_str(&format!(
        "{}  {}  {}  {}  {}  {}  {}  {}\n",
        "-".repeat(host_w),
        "-".repeat(4),
        "-".repeat(name_w),
        "-".repeat(13),
        "-".repeat(4),
        "-".repeat(4),
        "-".repeat(13),
        "-".repeat(5)
    ));

    for host in &snapshot.hosts {
        if !host.is_ok() {
            let error = host.error.as_ref();
            let kind = error.map(|e| e.kind.as_str()).unwrap_or("error");
            let state = paint("error", "error", color);
            out.push_str(&format!(
                "{:<host_w$}  {:<4}  {:<name_w$}  {:<13}  {:>4}  {:>4}  {:<13}  {}\n",
                clean(&host.host),
                "!",
                format!("{kind} ({:.1}s)", host.duration_ms as f64 / 1000.0),
                "",
                "",
                "",
                state,
                ""
            ));
            continue;
        }
        if host.gpus.is_empty() {
            out.push_str(&format!(
                "{:<host_w$}  {:<4}  {:<name_w$}  {:<13}  {:>4}  {:>4}  {:<13}  {}\n",
                clean(&host.host),
                "-",
                "no GPU",
                "",
                "",
                "",
                paint("ok", "free", color),
                ""
            ));
            continue;
        }
        for gpu in &host.gpus {
            let idx = gpu
                .index
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".into());
            let name = crate::output::sanitize::clean_or(gpu.name.as_deref(), "—");
            let free_total = memory_pair(gpu);
            let util = gpu
                .util_pct
                .map(|u| format!("{u}%"))
                .unwrap_or_else(|| "—".into());
            let temp = gpu
                .temp_c
                .map(|t| format!("{t}°"))
                .unwrap_or_else(|| "—".into());
            let state_text = state_text(gpu.state, gpu.reason.as_deref());
            let state = paint(&state_text, gpu.state.as_str(), color);
            let users = users(host, gpu);
            out.push_str(&format!(
                "{:<host_w$}  {:<4}  {:<name_w$}  {:<13}  {:>4}  {:>4}  {:<13}  {}\n",
                clean(&host.host),
                idx,
                truncate(&name, name_w),
                free_total,
                util,
                temp,
                state,
                users
            ));
        }
    }
    out
}

fn state_text(state: State, reason: Option<&str>) -> String {
    match (state, reason) {
        (State::Unknown, Some("mig")) => "unknown (MIG)".to_string(),
        _ => state.as_str().to_string(),
    }
}

fn memory_pair(gpu: &crate::model::Gpu) -> String {
    match (gpu.memory_free_mb, gpu.memory_total_mb) {
        (Some(free), Some(total)) => {
            format!("{:.1}/{:.1}G", free as f64 / 1024.0, total as f64 / 1024.0)
        }
        _ => "—".to_string(),
    }
}

fn users(host: &HostResult, gpu: &crate::model::Gpu) -> String {
    if gpu.processes.is_none() {
        return "?".to_string();
    }
    let users: Vec<String> = host.gpu_users(gpu).iter().map(|u| clean(u)).collect();
    if users.is_empty() {
        "—".to_string()
    } else {
        users.join(",")
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn paint(text: &str, state: &str, color: bool) -> String {
    if !color {
        return text.to_string();
    }
    let code = match state {
        "free" => "32",
        "busy" => "90",
        "unknown" => "33",
        "error" => "31",
        _ => return text.to_string(),
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{TransportMode, TransportUsed};
    use crate::model::{
        ErrorKind, Gpu, HostResult, HostStatus, ProbeErrorInfo, Snapshot, State, Summary,
    };

    fn gpu(state: State) -> Gpu {
        Gpu {
            index: Some(0),
            uuid: Some("GPU-a".into()),
            name: Some("RTX 4090".into()),
            driver: Some("570".into()),
            mig: false,
            compute_mode: Some("Default".into()),
            memory_total_mb: Some(24576),
            memory_used_mb: Some(12288),
            memory_free_mb: Some(12288),
            util_pct: Some(0),
            temp_c: Some(33),
            state,
            reason: None,
            processes: Some(vec![]),
        }
    }

    fn host(name: &str, gpus: Vec<Gpu>) -> HostResult {
        HostResult {
            host: name.into(),
            resolved_alias: name.into(),
            transport_used: TransportUsed::Direct,
            status: HostStatus::Ok,
            probed_at: "t".into(),
            duration_ms: 100,
            hostname: Some(name.into()),
            load1: Some(0.1),
            cpus: Some(8),
            fields_complete: true,
            processes_complete: true,
            gpus,
            note: None,
            error: None,
        }
    }

    fn snapshot(hosts: Vec<HostResult>) -> Snapshot {
        Snapshot {
            schema: "gpufree/v1".into(),
            generated_at: "t".into(),
            preset: Some("p".into()),
            transport: TransportMode::Direct,
            duration_ms: 1,
            summary: Summary::from_hosts(&hosts),
            hosts,
        }
    }

    #[test]
    fn renders_gpu_rows_and_errors() {
        let mut error = host("3090-0", vec![]);
        error.status = HostStatus::Error;
        error.error = Some(ProbeErrorInfo::new(ErrorKind::SshTimeout, "timed out"));
        error.gpus.clear();
        let snap = snapshot(vec![host("4090-1", vec![gpu(State::Free)]), error]);
        let text = render(&snap, false);
        assert!(text.contains("HOST"));
        assert!(text.contains("4090-1"));
        assert!(text.contains("12.0/24.0G"));
        assert!(text.contains("free"));
        assert!(text.contains("ssh_timeout"));
        assert!(
            !text.contains("\x1b["),
            "plain output must not contain ANSI"
        );
    }

    #[test]
    fn no_gpu_row_is_rendered() {
        let mut nas = host("nas", vec![]);
        nas.note = Some("no_gpu".into());
        let text = render(&snapshot(vec![nas]), false);
        assert!(text.contains("no GPU"));
    }

    #[test]
    fn remote_control_sequences_never_reach_the_terminal() {
        // SEC-08/A26: an OSC title sequence and a BEL in remote strings must
        // be replaced before rendering.
        let mut g = gpu(State::Free);
        g.name = Some("EVIL\u{1b}]0;PWNED\u{7}GPU".into());
        g.processes = Some(vec![crate::model::Process {
            gpu_uuid: "GPU-a".into(),
            pid: Some(7),
            user: Some("bob\u{7}".into()),
            process_name: "python\u{1b}[31m".into(),
            used_memory_mb: Some(1),
        }]);
        let mut evil_host = host("h\u{1b}]2;x\u{7}", vec![g]);
        evil_host.hostname = Some("h".into());
        let text = render(&snapshot(vec![evil_host]), false);
        assert!(!text.contains('\u{1b}'), "raw ESC leaked: {text:?}");
        assert!(!text.contains('\u{7}'), "raw BEL leaked: {text:?}");
        assert!(text.contains("PWNED"), "text should survive, sanitized");
    }

    #[test]
    fn color_is_opt_in() {
        let snap = snapshot(vec![host("a", vec![gpu(State::Free)])]);
        assert!(!render(&snap, false).contains("\x1b["));
        assert!(render(&snap, true).contains("\x1b[32m"));
    }
}
