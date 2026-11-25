//! Golden JSON test for the `gpufree/v1` schema (DESIGN §7, §12).
#![recursion_limit = "512"]

use gpufree::config::{TransportMode, TransportUsed};
use gpufree::model::{
    ErrorKind, Gpu, HostResult, HostStatus, ProbeErrorInfo, Process, Snapshot, State, Summary,
};
use gpufree::output::json;
use serde_json::{Value, json};

fn gpu_free() -> Gpu {
    Gpu {
        index: Some(0),
        uuid: Some("GPU-0".into()),
        name: Some("NVIDIA RTX 4090".into()),
        driver: Some("570.86.15".into()),
        mig: false,
        compute_mode: Some("Default".into()),
        memory_total_mb: Some(24564),
        memory_used_mb: Some(0),
        memory_free_mb: Some(24564),
        util_pct: Some(0),
        temp_c: Some(33),
        state: State::Free,
        reason: None,
        processes: Some(vec![]),
    }
}

fn gpu_busy() -> Gpu {
    Gpu {
        index: Some(1),
        uuid: Some("GPU-3f0a".into()),
        name: Some("NVIDIA RTX 4090".into()),
        driver: Some("570.86.15".into()),
        mig: false,
        compute_mode: Some("Default".into()),
        memory_total_mb: Some(24564),
        memory_used_mb: Some(131072),
        memory_free_mb: Some(0),
        util_pct: Some(97),
        temp_c: Some(71),
        state: State::Busy,
        reason: None,
        processes: Some(vec![Process {
            gpu_uuid: "GPU-3f0a".into(),
            pid: Some(12345),
            user: Some("root".into()),
            process_name: "python train.py".into(),
            used_memory_mb: Some(131072),
        }]),
    }
}

fn snapshot() -> Snapshot {
    let ok_host = HostResult {
        host: "4090-1".into(),
        resolved_alias: "4090-1r".into(),
        transport_used: TransportUsed::Remote,
        status: HostStatus::Ok,
        probed_at: "2026-01-02T03:04:05+00:00".into(),
        duration_ms: 210,
        hostname: Some("gpu-node".into()),
        load1: Some(0.5),
        cpus: Some(64),
        fields_complete: true,
        processes_complete: true,
        note: None,
        gpus: vec![gpu_free(), gpu_busy()],
        error: None,
    };
    let error_host = HostResult {
        host: "nas".into(),
        resolved_alias: "nas".into(),
        transport_used: TransportUsed::Direct,
        status: HostStatus::Error,
        probed_at: "2026-01-02T03:04:05+00:00".into(),
        duration_ms: 30,
        hostname: None,
        load1: None,
        cpus: None,
        fields_complete: false,
        processes_complete: false,
        note: None,
        gpus: vec![],
        error: Some(ProbeErrorInfo::new(
            ErrorKind::OutputLimit,
            "remote output exceeded the 8 MiB limit",
        )),
    };
    let hosts = vec![ok_host, error_host];
    Snapshot {
        schema: "gpufree/v1".into(),
        generated_at: "2026-01-02T03:04:05+00:00".into(),
        preset: Some("company".into()),
        transport: TransportMode::Remote,
        duration_ms: 1234,
        summary: Summary::from_hosts(&hosts),
        hosts,
    }
}

fn assert_order(text: &str, keys: &[&str]) {
    let mut last = 0;
    for key in keys {
        let needle = format!("\"{key}\"");
        let pos = text[last..]
            .find(&needle)
            .unwrap_or_else(|| panic!("key {key} missing after byte {last}"))
            + last;
        assert!(
            pos >= last,
            "key {key} is out of order in the serialized document"
        );
        last = pos + needle.len();
    }
}

#[test]
fn golden_document_matches_exactly() {
    let text = json::render(&snapshot()).unwrap();
    assert!(
        text.ends_with("}\n"),
        "JSON output must end with one newline"
    );

    let actual: Value = serde_json::from_str(&text).unwrap();
    let expected = json!({
        "schema": "gpufree/v1",
        "generated_at": "2026-01-02T03:04:05+00:00",
        "preset": "company",
        "transport": "remote",
        "duration_ms": 1234,
        "summary": {
            "hosts_total": 2,
            "hosts_ok": 1,
            "hosts_error": 1,
            "hosts_shown": 2,
            "gpus_total": 2,
            "gpus_free": 1,
            "gpus_busy": 1,
            "gpus_unknown": 0
        },
        "hosts": [
            {
                "host": "4090-1",
                "resolved_alias": "4090-1r",
                "transport_used": "remote",
                "status": "ok",
                "probed_at": "2026-01-02T03:04:05+00:00",
                "duration_ms": 210,
                "hostname": "gpu-node",
                "load1": 0.5,
                "cpus": 64,
                "fields_complete": true,
                "processes_complete": true,
                "note": null,
                "gpus": [
                    {
                        "index": 0,
                        "uuid": "GPU-0",
                        "name": "NVIDIA RTX 4090",
                        "driver": "570.86.15",
                        "mig": false,
                        "compute_mode": "Default",
                        "memory_total_mb": 24564,
                        "memory_used_mb": 0,
                        "memory_free_mb": 24564,
                        "util_pct": 0,
                        "temp_c": 33,
                        "state": "free",
                        "reason": null,
                        "processes": []
                    },
                    {
                        "index": 1,
                        "uuid": "GPU-3f0a",
                        "name": "NVIDIA RTX 4090",
                        "driver": "570.86.15",
                        "mig": false,
                        "compute_mode": "Default",
                        "memory_total_mb": 24564,
                        "memory_used_mb": 131072,
                        "memory_free_mb": 0,
                        "util_pct": 97,
                        "temp_c": 71,
                        "state": "busy",
                        "reason": null,
                        "processes": [
                            {
                                "gpu_uuid": "GPU-3f0a",
                                "pid": 12345,
                                "user": "root",
                                "process_name": "python train.py",
                                "used_memory_mb": 131072
                            }
                        ]
                    }
                ],
                "error": null
            },
            {
                "host": "nas",
                "resolved_alias": "nas",
                "transport_used": "direct",
                "status": "error",
                "probed_at": "2026-01-02T03:04:05+00:00",
                "duration_ms": 30,
                "hostname": null,
                "load1": null,
                "cpus": null,
                "fields_complete": false,
                "processes_complete": false,
                "note": null,
                "gpus": [],
                "error": {
                    "kind": "output_limit",
                    "message": "remote output exceeded the 8 MiB limit",
                    "retryable": false
                }
            }
        ]
    });

    assert_eq!(actual, expected);
}

#[test]
fn golden_document_key_order_is_stable() {
    let text = json::render(&snapshot()).unwrap();

    assert_order(
        &text,
        &[
            "schema",
            "generated_at",
            "preset",
            "transport",
            "duration_ms",
            "summary",
            "hosts",
        ],
    );
    assert_order(
        &text,
        &[
            "hosts_total",
            "hosts_ok",
            "hosts_error",
            "hosts_shown",
            "gpus_total",
            "gpus_free",
            "gpus_busy",
            "gpus_unknown",
        ],
    );
    assert_order(
        &text,
        &[
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
        ],
    );
    assert_order(
        &text,
        &[
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
        ],
    );
    assert_order(
        &text,
        &["gpu_uuid", "pid", "user", "process_name", "used_memory_mb"],
    );
}

#[test]
fn null_process_list_and_process_users_are_explicit() {
    // A GPU whose process query failed serializes `processes: null`, and a
    // process with an unresolved owner serializes `user: null`.
    let mut gpu = gpu_busy();
    gpu.processes = None;
    let host = HostResult {
        host: "h".into(),
        resolved_alias: "h".into(),
        transport_used: TransportUsed::Direct,
        status: HostStatus::Ok,
        probed_at: "t".into(),
        duration_ms: 1,
        hostname: None,
        load1: None,
        cpus: None,
        fields_complete: true,
        processes_complete: false,
        note: None,
        gpus: vec![gpu],
        error: None,
    };
    let mut other = gpu_free();
    other.processes = Some(vec![Process {
        gpu_uuid: "GPU-0".into(),
        pid: None,
        user: None,
        process_name: "[Not Supported]".into(),
        used_memory_mb: None,
    }]);
    let mut host2 = host.clone();
    host2.host = "h2".into();
    host2.resolved_alias = "h2".into();
    host2.gpus = vec![other];

    let hosts = vec![host, host2];
    let snap = Snapshot {
        schema: "gpufree/v1".into(),
        generated_at: "t".into(),
        preset: None,
        transport: TransportMode::Direct,
        duration_ms: 1,
        summary: Summary::from_hosts(&hosts),
        hosts,
    };
    let value: Value = serde_json::from_str(&json::render(&snap).unwrap()).unwrap();

    assert!(value["hosts"][0]["gpus"][0]["processes"].is_null());
    let process = &value["hosts"][1]["gpus"][0]["processes"][0];
    assert!(process["pid"].is_null());
    assert!(process["user"].is_null());
    assert!(process["used_memory_mb"].is_null());
    assert_eq!(process["process_name"], "[Not Supported]");
    assert!(value["preset"].is_null());
}
