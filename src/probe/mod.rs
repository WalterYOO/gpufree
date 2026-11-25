//! Probe scheduling: fixed worker threads + `mpsc`, separate direct and
//! proxied queues, a per-first-hop soft limit, and generation cancellation
//! (DESIGN §5.3). No async runtime.

pub mod parse;
pub mod script;
pub mod ssh;
pub mod transport;

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;

use chrono::SecondsFormat;

use crate::config::{Settings, TransportUsed};
use crate::model::{ErrorKind, HostResult, HostStatus, ProbeErrorInfo, Process, State};
use crate::probe::ssh::Canceller;
use crate::probe::transport::{HOP_LIMIT, HostPlan, hop_key};

#[derive(Clone, Debug)]
pub struct JobSpec {
    pub index: usize,
    pub host: String,
    pub plan: HostPlan,
    pub prefix: Option<String>,
}

#[derive(Clone, Debug)]
pub struct HostMeta {
    pub host: String,
    pub resolved_alias: String,
    pub transport_used: TransportUsed,
}

#[derive(Debug)]
pub struct RunHandle {
    pub rx: Receiver<(usize, HostResult)>,
    pub canceller: Arc<Canceller>,
    pub metas: Vec<HostMeta>,
}

/// A per-first-hop soft limit: at most `limit` probes may target the same jump
/// host at once. Waiters are woken on release and on cancellation.
#[derive(Debug)]
pub struct HopLimiter {
    limit: usize,
    state: Mutex<HashMap<String, usize>>,
    cv: Condvar,
    canceller: Arc<Canceller>,
}

impl HopLimiter {
    pub fn new(limit: usize, canceller: Arc<Canceller>) -> Arc<Self> {
        let limiter = Arc::new(Self {
            limit,
            state: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
            canceller: Arc::clone(&canceller),
        });
        canceller.register_hop(&limiter);
        limiter
    }

    fn acquire(self: &Arc<Self>, key: &str) -> Option<HopPermit> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if self.canceller.is_cancelled() {
                return None;
            }
            let active = state.get(key).copied().unwrap_or(0);
            if active < self.limit {
                state.insert(key.to_string(), active + 1);
                return Some(HopPermit {
                    limiter: Arc::clone(self),
                    key: key.to_string(),
                });
            }
            state = self.cv.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    pub fn notify_all(&self) {
        self.cv.notify_all();
    }
}

struct HopPermit {
    limiter: Arc<HopLimiter>,
    key: String,
}

impl Drop for HopPermit {
    fn drop(&mut self) {
        {
            let mut state = self.limiter.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(active) = state.get_mut(&self.key) {
                *active = active.saturating_sub(1);
                if *active == 0 {
                    state.remove(&self.key);
                }
            }
        }
        self.limiter.cv.notify_all();
    }
}

/// Start a probe generation. Results arrive as `(job index, result)`; the
/// receiver closes once every worker has exited.
pub fn spawn(specs: Vec<JobSpec>, settings: Arc<Settings>, utc: bool) -> RunHandle {
    let canceller = Canceller::new();
    let (tx, rx) = mpsc::channel::<(usize, HostResult)>();
    let hop = HopLimiter::new(HOP_LIMIT, Arc::clone(&canceller));

    let metas: Vec<HostMeta> = specs
        .iter()
        .map(|spec| HostMeta {
            host: spec.host.clone(),
            resolved_alias: spec.plan.alias.clone(),
            transport_used: spec.plan.transport_used,
        })
        .collect();

    let mut direct = Vec::new();
    let mut proxied = Vec::new();
    for spec in specs {
        if spec.plan.proxied {
            proxied.push(spec);
        } else {
            direct.push(spec);
        }
    }

    spawn_pool(
        direct,
        settings.parallel,
        Arc::clone(&settings),
        Arc::clone(&canceller),
        Arc::clone(&hop),
        tx.clone(),
        utc,
    );
    spawn_pool(
        proxied,
        settings.parallel_remote,
        Arc::clone(&settings),
        Arc::clone(&canceller),
        hop,
        tx.clone(),
        utc,
    );
    drop(tx);

    RunHandle {
        rx,
        canceller,
        metas,
    }
}

fn spawn_pool(
    jobs: Vec<JobSpec>,
    workers: usize,
    settings: Arc<Settings>,
    canceller: Arc<Canceller>,
    hop: Arc<HopLimiter>,
    results: Sender<(usize, HostResult)>,
    utc: bool,
) {
    let (job_tx, job_rx) = mpsc::channel::<JobSpec>();
    for job in jobs {
        let _ = job_tx.send(job);
    }
    drop(job_tx);
    let job_rx = Arc::new(Mutex::new(job_rx));

    for _ in 0..workers.max(1) {
        let job_rx = Arc::clone(&job_rx);
        let settings = Arc::clone(&settings);
        let canceller = Arc::clone(&canceller);
        let hop = Arc::clone(&hop);
        let results = results.clone();
        thread::spawn(move || {
            loop {
                let job = {
                    let rx = job_rx.lock().unwrap_or_else(|e| e.into_inner());
                    rx.recv()
                };
                let Ok(job) = job else { break };
                if canceller.is_cancelled() {
                    break;
                }
                let _permit = if job.plan.proxied {
                    match hop_key(&job.plan) {
                        Some(key) => match hop.acquire(&key) {
                            Some(permit) => Some(permit),
                            None => break,
                        },
                        None => None,
                    }
                } else {
                    None
                };

                let result = probe_guarded(&job, &settings, &canceller, utc);
                if results.send((job.index, result)).is_err() {
                    break;
                }
            }
        });
    }
}

/// Run one probe with a panic boundary: a bug in parsing must not lose the
/// host or leave the TUI spinning forever (DESIGN §5.3).
fn probe_guarded(
    job: &JobSpec,
    settings: &Settings,
    canceller: &Arc<Canceller>,
    utc: bool,
) -> HostResult {
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        probe_one(job, settings, canceller, utc)
    }));
    match outcome {
        Ok(result) => result,
        Err(payload) => {
            let message = panic_message(payload);
            error_result(
                job,
                now_rfc3339(utc),
                0,
                ProbeErrorInfo::new(
                    ErrorKind::InternalError,
                    format!("probe thread panicked: {message}"),
                ),
            )
        }
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

fn probe_one(
    job: &JobSpec,
    settings: &Settings,
    canceller: &Arc<Canceller>,
    utc: bool,
) -> HostResult {
    let start = Instant::now();
    let probed_at = now_rfc3339(utc);
    let script = script::render_prefixed(
        job.prefix.as_deref(),
        settings.samples,
        settings.sample_interval,
    );

    let output = match ssh::run(
        &settings.ssh_binary,
        &job.plan.alias,
        &script,
        settings,
        Arc::clone(canceller),
    ) {
        Ok(output) => output,
        Err(info) => {
            return error_result(job, probed_at, start.elapsed().as_millis() as u64, info);
        }
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let report = parse::parse(&text);
    if let Err(message) = report.validate() {
        let info = ssh::classify_stderr(&output.stderr)
            .unwrap_or_else(|| ProbeErrorInfo::new(ErrorKind::ParseError, message));
        return error_result(job, probed_at, start.elapsed().as_millis() as u64, info);
    }

    build_result(job, &report, settings, probed_at, start.elapsed())
}

fn build_result(
    job: &JobSpec,
    report: &parse::Report,
    settings: &Settings,
    probed_at: String,
    elapsed: std::time::Duration,
) -> HostResult {
    let duration_ms = elapsed.as_millis() as u64;
    match report.nogpu {
        Some(0) => {}
        Some(1) => {
            return ok_result(
                job,
                probed_at,
                duration_ms,
                Harness::new(report),
                Vec::new(),
                Some("no_gpu".to_string()),
            );
        }
        Some(_) => {
            return error_result(
                job,
                probed_at,
                duration_ms,
                ProbeErrorInfo::new(
                    ErrorKind::NvidiaSmiMissing,
                    "no nvidia-smi on a host with /dev/nvidia0",
                ),
            );
        }
        None => {
            return error_result(
                job,
                probed_at,
                duration_ms,
                ProbeErrorInfo::new(ErrorKind::ParseError, "missing @@NOGPU section"),
            );
        }
    }

    if let Some((_rc, message)) = report.gpu_failure() {
        return error_result(
            job,
            probed_at,
            duration_ms,
            ProbeErrorInfo::new(ErrorKind::NvidiaSmiFailed, message),
        );
    }

    let rows = report.gpu_rows();
    let proc_ok = report.proc_rc == Some(0);
    // A20: a failed ps user lookup keeps the process list but marks it
    // incomplete; it must not turn a GPU unknown by itself.
    let ps_complete = report
        .proc_rows
        .iter()
        .all(|row| row.pid.is_some_and(|pid| report.ps.contains_key(&pid)));
    let processes_complete = proc_ok && ps_complete;

    let mut by_uuid: HashMap<String, Vec<Process>> = HashMap::new();
    for row in &report.proc_rows {
        let user = row.pid.and_then(|pid| report.ps.get(&pid)).cloned();
        by_uuid
            .entry(row.gpu_uuid.clone())
            .or_default()
            .push(Process {
                gpu_uuid: row.gpu_uuid.clone(),
                pid: row.pid,
                user,
                process_name: row.process_name.clone(),
                used_memory_mb: row.used_memory_mb,
            });
    }

    let mut gpus = Vec::with_capacity(rows.len());
    for row in rows {
        // A20: only a failed compute-apps query nulls the list; a ps-only
        // failure keeps the processes and merely clears their user field.
        let processes = if proc_ok {
            let procs = row
                .uuid
                .as_ref()
                .and_then(|uuid| by_uuid.get(uuid).cloned())
                .unwrap_or_default();
            Some(procs)
        } else {
            None
        };
        let mut gpu = crate::model::Gpu {
            index: row.index,
            uuid: row.uuid,
            name: row.name,
            driver: row.driver,
            mig: false,
            compute_mode: row.compute_mode,
            memory_total_mb: row.memory_total_mb,
            memory_used_mb: row.memory_used_mb,
            memory_free_mb: row.memory_free_mb,
            util_pct: row.util_pct,
            temp_c: row.temp_c,
            state: State::Unknown,
            reason: None,
            processes,
        };
        gpu.decide_state(
            report.mig,
            settings.free.used_mem_max_mb,
            settings.free.util_max_pct,
        );
        gpus.push(gpu);
    }

    let note = gpus.is_empty().then(|| "no_gpu".to_string());
    let fields_complete = gpus.iter().all(crate::model::Gpu::is_complete);
    let mut result = ok_result(
        job,
        probed_at,
        duration_ms,
        Harness::new(report),
        gpus,
        note,
    );
    result.fields_complete = fields_complete;
    result.processes_complete = processes_complete;
    result
}

struct Harness {
    hostname: Option<String>,
    load1: Option<f64>,
    cpus: Option<u32>,
}

impl Harness {
    fn new(report: &parse::Report) -> Self {
        Self {
            hostname: report.hostname.clone(),
            load1: report.load1,
            cpus: report.cpus,
        }
    }
}

fn base_result(job: &JobSpec, probed_at: String, duration_ms: u64) -> HostResult {
    HostResult {
        host: job.host.clone(),
        resolved_alias: job.plan.alias.clone(),
        transport_used: job.plan.transport_used,
        status: HostStatus::Ok,
        probed_at,
        duration_ms,
        hostname: None,
        load1: None,
        cpus: None,
        fields_complete: true,
        processes_complete: true,
        note: None,
        gpus: Vec::new(),
        error: None,
    }
}

fn ok_result(
    job: &JobSpec,
    probed_at: String,
    duration_ms: u64,
    harness: Harness,
    gpus: Vec<crate::model::Gpu>,
    note: Option<String>,
) -> HostResult {
    let mut result = base_result(job, probed_at, duration_ms);
    result.hostname = harness.hostname;
    result.load1 = harness.load1;
    result.cpus = harness.cpus;
    result.gpus = gpus;
    result.note = note;
    result
}

fn error_result(
    job: &JobSpec,
    probed_at: String,
    duration_ms: u64,
    info: ProbeErrorInfo,
) -> HostResult {
    let mut result = base_result(job, probed_at, duration_ms);
    result.status = HostStatus::Error;
    result.fields_complete = false;
    result.processes_complete = false;
    result.error = Some(info);
    result
}

pub fn now_rfc3339(utc: bool) -> String {
    if utc {
        chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false)
    } else {
        chrono::Local::now().to_rfc3339_opts(SecondsFormat::Secs, false)
    }
}

/// Collect a whole generation (JSON / table mode). Returns results in job
/// order, with cancelled hosts represented as error entries.
pub fn collect(handle: RunHandle, total: usize) -> (Vec<HostResult>, Arc<Canceller>, bool) {
    let mut slots: Vec<Option<HostResult>> = (0..total).map(|_| None).collect();
    for (index, result) in handle.rx {
        if index < slots.len() {
            slots[index] = Some(result);
        }
    }
    let interrupted = handle.canceller.is_cancelled();
    let results = slots.into_iter().flatten().collect();
    (results, handle.canceller, interrupted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TransportMode;

    fn job(host: &str, alias: &str) -> JobSpec {
        JobSpec {
            index: 0,
            host: host.to_string(),
            plan: HostPlan {
                host: host.to_string(),
                alias: alias.to_string(),
                transport_used: TransportUsed::Direct,
                proxied: false,
                first_hop: None,
            },
            prefix: None,
        }
    }

    fn report(text: &str) -> parse::Report {
        let report = parse::parse(text);
        report.validate().unwrap();
        report
    }

    fn settings() -> Settings {
        Settings::default()
    }

    const OK: &str = "\
@@HOST
node-1
1.5 0.5 0.1 1/100 999
32
@@NOGPU
0
@@MIG
0
@@GPU
0, GPU-a, RTX 4090, 24564, 1, 24563, 0, 33, Default, 570.86.15
1, GPU-b, RTX 4090, 24564, 12288, 12276, 87, 71, Default, 570.86.15
@@GPU_RC=0
@@PROC
GPU-b, 12345, python train.py, 12000
@@PROC_RC=0
@@PS
12345 alice
@@END
";

    #[test]
    fn builds_states_and_processes() {
        let r = report(OK);
        let result = build_result(
            &job("node-1", "node-1"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::from_millis(12),
        );
        assert_eq!(result.status, HostStatus::Ok);
        assert_eq!(result.hostname.as_deref(), Some("node-1"));
        assert_eq!(result.cpus, Some(32));
        assert_eq!(result.gpus.len(), 2);
        assert_eq!(result.gpus[0].state, State::Free);
        assert_eq!(result.gpus[1].state, State::Busy);
        assert_eq!(
            result.gpus[1].processes.as_ref().unwrap()[0]
                .user
                .as_deref(),
            Some("alice")
        );
        assert!(result.fields_complete);
        assert!(result.processes_complete);
        assert!(result.note.is_none());
    }

    #[test]
    fn no_gpu_is_ok_with_note() {
        let r = report("@@HOST\nnas\n@@NOGPU\n1\n@@END\n");
        let result = build_result(
            &job("nas", "nas"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::ZERO,
        );
        assert_eq!(result.status, HostStatus::Ok);
        assert!(result.gpus.is_empty());
        assert_eq!(result.note.as_deref(), Some("no_gpu"));
    }

    #[test]
    fn nogpu_two_is_nvidia_smi_missing() {
        let r = report("@@HOST\nh\n@@NOGPU\n2\n@@END\n");
        let result = build_result(
            &job("h", "h"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::ZERO,
        );
        assert_eq!(result.status, HostStatus::Error);
        assert_eq!(
            result.error.as_ref().unwrap().kind,
            ErrorKind::NvidiaSmiMissing
        );
        assert!(!result.error.as_ref().unwrap().retryable);
        assert!(result.gpus.is_empty());
    }

    // Real script shape: `@@NOGPU 1|2` then `exit 0`, with no END/GPU/PROC.
    const EARLY_NOGPU_ONE: &str = "@@HOST\nong\n0.1 0.1 0.1 1/1 1\n8\n@@NOGPU\n1\n";
    const EARLY_NOGPU_TWO: &str = "@@HOST\nbroken\n0.1 0.1 0.1 1/1 1\n8\n@@NOGPU\n2\n";

    #[test]
    fn real_early_exit_nogpu_one_builds_ok_no_gpu() {
        let r = report(EARLY_NOGPU_ONE);
        r.validate().expect("early-exit no-GPU report is complete");
        let result = build_result(
            &job("ong", "ong"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::ZERO,
        );
        assert_eq!(result.status, HostStatus::Ok);
        assert!(result.gpus.is_empty());
        assert_eq!(result.note.as_deref(), Some("no_gpu"));
        assert!(result.fields_complete);
        assert!(result.processes_complete);
        assert!(result.error.is_none());
        assert_eq!(result.hostname.as_deref(), Some("ong"));
    }

    #[test]
    fn real_early_exit_nogpu_two_builds_nvidia_smi_missing() {
        let r = report(EARLY_NOGPU_TWO);
        r.validate().expect("early-exit no-GPU report is complete");
        let result = build_result(
            &job("broken", "broken"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::ZERO,
        );
        assert_eq!(result.status, HostStatus::Error);
        assert_eq!(
            result.error.as_ref().unwrap().kind,
            ErrorKind::NvidiaSmiMissing
        );
        assert!(!result.error.as_ref().unwrap().retryable);
        assert!(result.gpus.is_empty());
        assert!(result.note.is_none());
    }

    #[test]
    fn driver_failure_is_nvidia_smi_failed() {
        let text = "\
@@HOST
h
@@NOGPU
0
@@MIG
0
@@GPU
driver exploded
@@GPU_RC=9
@@PROC
@@PROC_RC=0
@@END
";
        let r = report(text);
        let result = build_result(
            &job("h", "h"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::ZERO,
        );
        assert_eq!(
            result.error.as_ref().unwrap().kind,
            ErrorKind::NvidiaSmiFailed
        );
        assert!(result.error.as_ref().unwrap().retryable);
    }

    #[test]
    fn failed_process_query_makes_idle_gpus_unknown() {
        let text = "\
@@HOST
h
@@NOGPU
0
@@MIG
0
@@GPU
0, GPU-a, X, 24564, 1, 24563, 0, 33, Default, 1
@@GPU_RC=0
@@PROC
permission denied
@@PROC_RC=1
@@END
";
        let r = report(text);
        let result = build_result(
            &job("h", "h"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::ZERO,
        );
        assert!(!result.processes_complete);
        assert_eq!(result.gpus[0].state, State::Unknown);
        assert_eq!(result.gpus[0].reason.as_deref(), Some("no_process_data"));
        assert!(result.gpus[0].processes.is_none(), "null, not empty");
    }

    #[test]
    fn mig_marks_every_gpu_unknown() {
        let text = "\
@@HOST
h
@@NOGPU
0
@@MIG
1
@@GPU
0, GPU-a, X, 24564, 1, 24563, 0, 33, Default, 1
@@GPU_RC=0
@@PROC
@@PROC_RC=0
@@END
";
        let r = report(text);
        let result = build_result(
            &job("h", "h"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::ZERO,
        );
        assert_eq!(result.gpus[0].state, State::Unknown);
        assert_eq!(result.gpus[0].reason.as_deref(), Some("mig"));
        assert!(result.gpus[0].mig);
    }

    #[test]
    fn fallback_marks_fields_incomplete() {
        let text = "\
@@HOST
h
@@NOGPU
0
@@MIG
0
@@GPU
@@FALLBACK=1
0, GPU-a, X, 24564, 1, 24563, 0
@@GPU_RC=0
@@PROC
@@PROC_RC=0
@@END
";
        let r = report(text);
        let result = build_result(
            &job("h", "h"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::ZERO,
        );
        assert!(!result.fields_complete);
        assert_eq!(result.gpus[0].state, State::Free);
        assert_eq!(result.gpus[0].driver, None);
    }

    #[test]
    fn good_hosts_are_free_but_no_gpu_is_still_ok() {
        let r = report(OK);
        let result = build_result(
            &job("n", "n"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::ZERO,
        );
        assert_eq!(result.free_count(), 1);
    }

    #[test]
    fn hop_limiter_blocks_until_released() {
        let canceller = Canceller::new();
        let hop = HopLimiter::new(1, Arc::clone(&canceller));
        let first = hop.acquire("jump");
        assert!(first.is_some());
        drop(first);
        assert!(hop.acquire("jump").is_some());
    }

    #[test]
    fn cancelled_hop_acquire_returns_none() {
        let canceller = Canceller::new();
        let hop = HopLimiter::new(1, Arc::clone(&canceller));
        let _held = hop.acquire("jump").unwrap();
        canceller.cancel();
        // acquire would block forever without the cancel predicate.
        assert!(hop.acquire("jump").is_none());
    }

    #[test]
    fn ps_failure_keeps_processes_but_marks_incomplete() {
        let text = "\
@@HOST
h
@@NOGPU
0
@@MIG
0
@@GPU
0, GPU-a, X, 24564, 4096, 20468, 50, 33, Default, 1
@@GPU_RC=0
@@PROC
GPU-a, 999, python, 2048
@@PROC_RC=0
@@PS
@@END
";
        let r = report(text);
        let result = build_result(
            &job("h", "h"),
            &r,
            &settings(),
            "t".into(),
            std::time::Duration::ZERO,
        );
        assert!(result.fields_complete);
        assert!(!result.processes_complete, "ps lookup failed");
        let procs = result.gpus[0].processes.as_ref().expect("list retained");
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].user, None);
        assert_eq!(procs[0].gpu_uuid, "GPU-a");
        assert_eq!(result.gpus[0].state, State::Busy);
    }

    #[test]
    fn job_meta_round_trip() {
        let j = job("a", "ar");
        assert_eq!(j.plan.transport_used, TransportUsed::Direct);
        assert_eq!(TransportMode::Direct.to_string(), "direct");
    }
}
