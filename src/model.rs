//! Data model shared by JSON and the TUI: hosts, GPUs, processes, snapshot,
//! summary, and the three-state free/busy/unknown decision from DESIGN §6.

use serde::Serialize;

use crate::config::{TransportMode, TransportUsed};

pub const SCHEMA: &str = "gpufree/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Free,
    Busy,
    Unknown,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Busy => "busy",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HostStatus {
    Ok,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    SshTimeout,
    SshUnreachable,
    SshAuth,
    SshHostkey,
    NvidiaSmiMissing,
    NvidiaSmiFailed,
    ParseError,
    OutputLimit,
    InternalError,
    Unknown,
}

impl ErrorKind {
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::SshTimeout | Self::SshUnreachable | Self::NvidiaSmiFailed | Self::InternalError
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SshTimeout => "ssh_timeout",
            Self::SshUnreachable => "ssh_unreachable",
            Self::SshAuth => "ssh_auth",
            Self::SshHostkey => "ssh_hostkey",
            Self::NvidiaSmiMissing => "nvidia_smi_missing",
            Self::NvidiaSmiFailed => "nvidia_smi_failed",
            Self::ParseError => "parse_error",
            Self::OutputLimit => "output_limit",
            Self::InternalError => "internal_error",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProbeErrorInfo {
    pub kind: ErrorKind,
    pub message: String,
    pub retryable: bool,
}

impl ProbeErrorInfo {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable: kind.retryable(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Process {
    pub gpu_uuid: String,
    pub pid: Option<u32>,
    pub user: Option<String>,
    pub process_name: String,
    pub used_memory_mb: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Gpu {
    pub index: Option<u32>,
    pub uuid: Option<String>,
    pub name: Option<String>,
    pub driver: Option<String>,
    pub mig: bool,
    pub compute_mode: Option<String>,
    pub memory_total_mb: Option<u64>,
    pub memory_used_mb: Option<u64>,
    pub memory_free_mb: Option<u64>,
    pub util_pct: Option<u32>,
    pub temp_c: Option<u32>,
    pub state: State,
    pub reason: Option<String>,
    pub processes: Option<Vec<Process>>,
}

impl Gpu {
    /// All fields that DESIGN §6 calls critical for a free/busy decision.
    pub fn has_critical_fields(&self) -> bool {
        self.index.is_some()
            && self.uuid.is_some()
            && self.memory_total_mb.is_some()
            && self.memory_used_mb.is_some()
            && self.memory_free_mb.is_some()
            && self.util_pct.is_some()
    }

    /// Critical fields plus the tier-2 fields reported in JSON.
    pub fn is_complete(&self) -> bool {
        self.has_critical_fields()
            && self.temp_c.is_some()
            && self.compute_mode.is_some()
            && self.driver.is_some()
    }

    /// DESIGN §6 state decision for one GPU.
    ///
    /// `processes == None` means the compute-process query failed; `Some([])`
    /// means it succeeded and there are no compute processes.
    pub fn decide_state(&mut self, mig: bool, used_mem_max_mb: u64, util_max_pct: u32) {
        self.mig = mig;
        self.reason = None;
        self.state = State::Unknown;

        if mig {
            self.reason = Some("mig".to_string());
            return;
        }
        if self
            .compute_mode
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("prohibited"))
        {
            self.reason = Some("compute_prohibited".to_string());
            return;
        }
        if !self.has_critical_fields() {
            self.reason = Some("missing_fields".to_string());
            return;
        }

        let used = self.memory_used_mb.unwrap_or(0);
        let util = self.util_pct.unwrap_or(0);
        // Unknown reasons win over busy (A3): a failed process query at low
        // load cannot prove the card is free.
        if self.processes.is_none() && used <= used_mem_max_mb && util <= util_max_pct {
            self.reason = Some("no_process_data".to_string());
            return;
        }
        let has_procs = self
            .processes
            .as_ref()
            .is_some_and(|procs| !procs.is_empty());
        if has_procs || used > used_mem_max_mb || util > util_max_pct {
            self.state = State::Busy;
            return;
        }
        self.state = State::Free;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HostResult {
    pub host: String,
    pub resolved_alias: String,
    pub transport_used: TransportUsed,
    pub status: HostStatus,
    pub probed_at: String,
    pub duration_ms: u64,
    pub hostname: Option<String>,
    pub load1: Option<f64>,
    pub cpus: Option<u32>,
    pub fields_complete: bool,
    pub processes_complete: bool,
    pub note: Option<String>,
    pub gpus: Vec<Gpu>,
    pub error: Option<ProbeErrorInfo>,
}

impl HostResult {
    pub fn is_ok(&self) -> bool {
        self.status == HostStatus::Ok
    }

    pub fn free_count(&self) -> usize {
        self.gpus.iter().filter(|g| g.state == State::Free).count()
    }

    pub fn busy_count(&self) -> usize {
        self.gpus.iter().filter(|g| g.state == State::Busy).count()
    }

    pub fn unknown_count(&self) -> usize {
        self.gpus
            .iter()
            .filter(|g| g.state == State::Unknown)
            .count()
    }

    pub fn gpu_users(&self, gpu: &Gpu) -> Vec<String> {
        let mut users: Vec<String> = gpu
            .processes
            .as_ref()
            .map(|procs| procs.iter().filter_map(|p| p.user.clone()).collect())
            .unwrap_or_default();
        users.sort();
        users.dedup();
        users
    }

    /// Compute processes of the whole host, tagged with the owning GPU index.
    pub fn all_processes(&self) -> Vec<(Option<u32>, &Process)> {
        let mut out = Vec::new();
        for gpu in &self.gpus {
            if let Some(procs) = &gpu.processes {
                for proc_ in procs {
                    out.push((gpu.index, proc_));
                }
            }
        }
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub hosts_total: usize,
    pub hosts_ok: usize,
    pub hosts_error: usize,
    pub hosts_shown: usize,
    pub gpus_total: usize,
    pub gpus_free: usize,
    pub gpus_busy: usize,
    pub gpus_unknown: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Snapshot {
    pub schema: String,
    pub generated_at: String,
    pub preset: Option<String>,
    pub transport: TransportMode,
    pub duration_ms: u64,
    pub summary: Summary,
    pub hosts: Vec<HostResult>,
}

impl Snapshot {
    pub fn new(
        generated_at: String,
        preset: Option<String>,
        transport: TransportMode,
        duration_ms: u64,
        hosts: Vec<HostResult>,
    ) -> Self {
        let summary = Summary::from_hosts(&hosts);
        Self {
            schema: SCHEMA.to_string(),
            generated_at,
            preset,
            transport,
            duration_ms,
            summary,
            hosts,
        }
    }

    /// DESIGN §3.1: all failed → 1; `--strict` and any failure → 1; at least
    /// one ok and `--require-free` with no free GPU → 3; otherwise 0.
    pub fn exit_code(&self, strict: bool, require_free: bool) -> i32 {
        if self.summary.hosts_ok == 0 {
            return 1;
        }
        if strict && self.summary.hosts_error > 0 {
            return 1;
        }
        if require_free && self.summary.gpus_free == 0 {
            return 3;
        }
        0
    }

    /// Keep only hosts with at least one free GPU. Summary GPU counters stay
    /// over all ok hosts (DESIGN §7); only `hosts_shown` changes.
    pub fn apply_free_only(&mut self) {
        self.hosts.retain(|h| h.is_ok() && h.free_count() > 0);
        self.summary.hosts_shown = self.hosts.len();
    }

    pub fn recompute_shown(&mut self) {
        self.summary.hosts_shown = self.hosts.len();
    }
}

impl Summary {
    pub fn from_hosts(hosts: &[HostResult]) -> Self {
        let mut s = Self {
            hosts_total: hosts.len(),
            hosts_shown: hosts.len(),
            ..Self::default()
        };
        for host in hosts {
            if host.is_ok() {
                s.hosts_ok += 1;
                s.gpus_total += host.gpus.len();
                s.gpus_free += host.free_count();
                s.gpus_busy += host.busy_count();
                s.gpus_unknown += host.unknown_count();
            } else {
                s.hosts_error += 1;
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(used: u64, util: u32, procs: Option<Vec<Process>>) -> Gpu {
        Gpu {
            index: Some(0),
            uuid: Some("GPU-0".into()),
            name: Some("Test".into()),
            driver: Some("570.0".into()),
            mig: false,
            compute_mode: Some("Default".into()),
            memory_total_mb: Some(24576),
            memory_used_mb: Some(used),
            memory_free_mb: Some(24576 - used),
            util_pct: Some(util),
            temp_c: Some(30),
            state: State::Unknown,
            reason: None,
            processes: procs,
        }
    }

    fn proc_() -> Process {
        Process {
            gpu_uuid: "GPU-0".into(),
            pid: Some(1),
            user: Some("alice".into()),
            process_name: "python".into(),
            used_memory_mb: Some(100),
        }
    }

    #[test]
    fn free_at_thresholds_is_free() {
        let mut g = gpu(512, 10, Some(vec![]));
        g.decide_state(false, 512, 10);
        assert_eq!(g.state, State::Free);
    }

    #[test]
    fn used_above_threshold_is_busy() {
        let mut g = gpu(513, 0, Some(vec![]));
        g.decide_state(false, 512, 10);
        assert_eq!(g.state, State::Busy);
    }

    #[test]
    fn util_above_threshold_is_busy() {
        let mut g = gpu(0, 11, Some(vec![]));
        g.decide_state(false, 512, 10);
        assert_eq!(g.state, State::Busy);
    }

    #[test]
    fn process_makes_busy_even_when_idle() {
        let mut g = gpu(1, 0, Some(vec![proc_()]));
        g.decide_state(false, 512, 10);
        assert_eq!(g.state, State::Busy);
    }

    #[test]
    fn missing_process_data_with_low_usage_is_unknown() {
        let mut g = gpu(1, 0, None);
        g.decide_state(false, 512, 10);
        assert_eq!(g.state, State::Unknown);
        assert_eq!(g.reason.as_deref(), Some("no_process_data"));
    }

    #[test]
    fn missing_process_data_with_high_usage_is_busy() {
        let mut g = gpu(1024, 0, None);
        g.decide_state(false, 512, 10);
        assert_eq!(g.state, State::Busy);
    }

    #[test]
    fn mig_is_unknown() {
        let mut g = gpu(0, 0, Some(vec![]));
        g.decide_state(true, 512, 10);
        assert_eq!(g.state, State::Unknown);
        assert_eq!(g.reason.as_deref(), Some("mig"));
        assert!(g.mig);
    }

    #[test]
    fn prohibited_is_unknown() {
        let mut g = gpu(0, 0, Some(vec![]));
        g.compute_mode = Some("Prohibited".into());
        g.decide_state(false, 512, 10);
        assert_eq!(g.state, State::Unknown);
        assert_eq!(g.reason.as_deref(), Some("compute_prohibited"));
    }

    #[test]
    fn missing_critical_field_is_unknown() {
        let mut g = gpu(0, 0, Some(vec![]));
        g.memory_free_mb = None;
        g.decide_state(false, 512, 10);
        assert_eq!(g.state, State::Unknown);
        assert_eq!(g.reason.as_deref(), Some("missing_fields"));
    }

    #[test]
    fn summary_counts_only_ok_hosts() {
        let mut ok = ready_host("a", vec![gpu(0, 0, Some(vec![]))]);
        ok.gpus[0].decide_state(false, 512, 10);
        let err = HostResult {
            host: "b".into(),
            resolved_alias: "b".into(),
            transport_used: TransportUsed::Direct,
            status: HostStatus::Error,
            probed_at: "t".into(),
            duration_ms: 1,
            hostname: None,
            load1: None,
            cpus: None,
            fields_complete: false,
            processes_complete: false,
            note: None,
            gpus: vec![gpu(0, 0, None)],
            error: Some(ProbeErrorInfo::new(ErrorKind::SshAuth, "denied")),
        };
        let s = Summary::from_hosts(&[ok, err]);
        assert_eq!(s.hosts_total, 2);
        assert_eq!(s.hosts_ok, 1);
        assert_eq!(s.hosts_error, 1);
        assert_eq!(s.gpus_total, 1);
        assert_eq!(s.gpus_free, 1);
    }

    #[test]
    fn exit_code_precedence() {
        let mut snap = Snapshot::new(
            "t".into(),
            None,
            TransportMode::Direct,
            0,
            vec![error_host("a", ErrorKind::SshAuth)],
        );
        assert_eq!(
            snap.exit_code(false, true),
            1,
            "all failed beats require-free"
        );

        snap.hosts.push(ready_host("b", vec![]));
        snap.summary = Summary::from_hosts(&snap.hosts);
        assert_eq!(snap.exit_code(false, true), 3);
        assert_eq!(snap.exit_code(true, true), 1, "strict failure wins over 3");
        assert_eq!(snap.exit_code(false, false), 0);
    }

    #[test]
    fn free_only_filters_hosts_but_not_gpu_counts() {
        let mut free = ready_host("free", vec![gpu(0, 0, Some(vec![]))]);
        free.gpus[0].decide_state(false, 512, 10);
        let mut busy = ready_host("busy", vec![gpu(4096, 90, Some(vec![]))]);
        busy.gpus[0].decide_state(false, 512, 10);
        let mut snap = Snapshot::new("t".into(), None, TransportMode::Direct, 0, vec![free, busy]);
        assert_eq!(snap.summary.gpus_free, 1);
        snap.apply_free_only();
        assert_eq!(snap.hosts.len(), 1);
        assert_eq!(snap.summary.hosts_shown, 1);
        assert_eq!(snap.summary.gpus_total, 2);
        assert_eq!(snap.summary.gpus_free, 1);
    }

    pub fn ready_host(host: &str, gpus: Vec<Gpu>) -> HostResult {
        HostResult {
            host: host.into(),
            resolved_alias: host.into(),
            transport_used: TransportUsed::Direct,
            status: HostStatus::Ok,
            probed_at: "2026-11-25T14:03:06+08:00".into(),
            duration_ms: 10,
            hostname: Some(host.into()),
            load1: Some(1.0),
            cpus: Some(8),
            fields_complete: true,
            processes_complete: true,
            note: None,
            gpus,
            error: None,
        }
    }

    pub fn error_host(host: &str, kind: ErrorKind) -> HostResult {
        HostResult {
            host: host.into(),
            resolved_alias: host.into(),
            transport_used: TransportUsed::Direct,
            status: HostStatus::Error,
            probed_at: "2026-11-25T14:03:06+08:00".into(),
            duration_ms: 10,
            hostname: None,
            load1: None,
            cpus: None,
            fields_complete: false,
            processes_complete: false,
            note: None,
            gpus: vec![],
            error: Some(ProbeErrorInfo::new(kind, "boom")),
        }
    }
}
