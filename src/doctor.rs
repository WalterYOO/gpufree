//! `gpufree doctor` (DESIGN §9, exit codes A6).
//!
//! Diagnoses config, ssh availability and resolution, per-preset transport,
//! and (with `--probe`) real nvidia-smi/compute-apps/MIG access. Returns
//! 0 when every check passes, 1 when any check fails. Config parse errors are
//! mapped to exit 2 by the caller before this runs.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use crate::config::{Config, TransportUsed};
use crate::model::HostResult;
use crate::probe::transport::{HostPlan, Resolver, SshConfig, SshQuery};
use crate::probe::{self, HostMeta, JobSpec};

pub fn run(config: &Config, path: &Path, present: bool, probe: bool, utc: bool) -> i32 {
    let mut failures = 0usize;

    println!("gpufree doctor");
    if present {
        println!("config: {} (loaded)", path.display());
    } else {
        println!(
            "config: {} (not found; using built-in defaults — run `gpufree init`)",
            path.display()
        );
    }
    println!("presets: {}", config.presets.len());

    let binary = config.settings.ssh_binary.clone();
    if probe::ssh::ssh_available(&binary) {
        println!("ssh binary: {binary} (executable)");
    } else {
        println!("ssh binary: {binary} (NOT executable)");
        failures += 1;
    }
    match Command::new(&binary).arg("-V").output() {
        Ok(output) if output.status.success() => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let line = text.lines().next().unwrap_or("(no output)");
            println!("ssh version: {line}");
        }
        Ok(output) => {
            // A26/A28: `ssh -V` exiting non-zero is a failed check, not a
            // version banner to print while reporting success.
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let line = text.lines().next().unwrap_or("(no output)");
            println!(
                "ssh version: FAILED (exit {}): {line}",
                output.status.code().unwrap_or(-1)
            );
            failures += 1;
        }
        Err(err) => {
            println!("ssh version: FAILED ({err})");
            failures += 1;
        }
    }

    let resolver = Resolver::new(binary.clone());
    let mut specs: Vec<JobSpec> = Vec::new();
    let mut index = 0usize;

    if config.presets.is_empty() {
        println!("\nno presets configured");
    }
    for (name, preset) in &config.presets {
        println!("\npreset {name} (transport {})", preset.transport);
        match resolver.plan(&preset.hosts, preset.transport, &preset.remote_aliases) {
            Ok(plans) => {
                for plan in plans {
                    if !print_plan(&resolver, &plan, name) {
                        failures += 1;
                    }
                    specs.push(JobSpec {
                        index,
                        host: plan.host.clone(),
                        plan,
                        prefix: preset.prefix.clone(),
                    });
                    index += 1;
                }
            }
            Err(err) => {
                println!("  ERROR: {err}");
                failures += 1;
            }
        }
    }

    if probe {
        println!("\nprobing {} host(s)…", specs.len());
        if !probe::ssh::ssh_available(&binary) {
            println!("--probe skipped: ssh binary is not executable");
            return 1;
        }
        let total = specs.len();
        let labels: Vec<HostMeta> = specs
            .iter()
            .map(|spec| HostMeta {
                host: spec.host.clone(),
                resolved_alias: spec.plan.alias.clone(),
                transport_used: spec.plan.transport_used,
            })
            .collect();
        let settings = Arc::new(config.settings.clone());
        let handle = probe::spawn(specs, settings, utc);
        let mut slots: Vec<Option<HostResult>> = (0..total).map(|_| None).collect();
        for (index, result) in handle.rx {
            if index < slots.len() {
                slots[index] = Some(result);
            }
        }
        let interrupted = handle.canceller.is_cancelled();
        for (index, meta) in labels.iter().enumerate() {
            match slots[index].take() {
                Some(result) => {
                    if !result.is_ok() {
                        failures += 1;
                    }
                    print_probe(meta, &result);
                }
                None => {
                    failures += 1;
                    println!("  {}: ERROR cancelled (no result)", meta.host);
                }
            }
        }
        if interrupted {
            failures += 1;
            println!("\nprobe was interrupted by a signal");
        }
    }

    println!();
    if failures == 0 {
        println!("doctor: all checks passed");
        0
    } else {
        println!("doctor: {failures} check(s) failed");
        1
    }
}

/// Print one host's resolution; returns `false` when `ssh -G` failed, which
/// the caller counts as a doctor failure (DOC-03/A6).
fn print_plan(resolver: &Resolver, plan: &HostPlan, preset: &str) -> bool {
    let Some(cfg) = resolver.ssh_g(&plan.alias) else {
        println!(
            "  ERROR: preset {preset} host {}: `ssh -G {}` failed (alias not resolvable)",
            plan.host, plan.alias
        );
        return false;
    };
    let endpoint = endpoint(&cfg);
    let via = if plan.proxied {
        format!(
            " · proxied via {}",
            plan.first_hop.as_deref().unwrap_or("ProxyCommand")
        )
    } else {
        String::new()
    };
    println!(
        "  {:<20} -> {:<24} {:<7} {}{}",
        plan.host, plan.alias, plan.transport_used, endpoint, via
    );
    if cfg.hostname.as_deref() == Some(plan.alias.as_str()) {
        println!(
            "       note: no ssh_config Host entry for {:?}; resolved literally",
            plan.alias
        );
    }
    true
}

fn endpoint(cfg: &SshConfig) -> String {
    match (&cfg.hostname, cfg.port) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.clone(),
        _ => "?".to_string(),
    }
}

fn print_probe(meta: &HostMeta, result: &HostResult) {
    let transport = match meta.transport_used {
        TransportUsed::Direct => "direct",
        TransportUsed::Remote => "remote",
    };
    if !result.is_ok() {
        let error = result.error.as_ref();
        let kind = error.map(|e| e.kind.as_str()).unwrap_or("unknown");
        let message =
            crate::output::sanitize::clean(error.map(|e| e.message.as_str()).unwrap_or(""));
        let retry = error.map(|e| e.retryable).unwrap_or(false);
        println!(
            "  {} [{}]: ERROR {kind} (retryable={retry}) {message}",
            meta.host, transport
        );
        if error.map(|e| e.kind) == Some(crate::model::ErrorKind::SshAuth) {
            println!("       needs password/2FA; BatchMode=yes cannot proceed");
        }
        return;
    }
    if result.gpus.is_empty() {
        println!("  {} [{}]: ok · no GPU", meta.host, transport);
        return;
    }
    let mut drivers: Vec<String> = result
        .gpus
        .iter()
        .filter_map(|gpu| gpu.driver.as_deref())
        .map(crate::output::sanitize::clean)
        .collect();
    drivers.sort();
    drivers.dedup();
    let drivers = if drivers.is_empty() {
        "driver —".to_string()
    } else {
        format!("driver {}", drivers.join(","))
    };
    let mig = result.gpus.iter().any(|gpu| gpu.mig);
    let apps = if result.processes_complete {
        "compute-apps ok"
    } else {
        "compute-apps INCOMPLETE (permission?)"
    };
    println!(
        "  {} [{}]: ok · {} GPUs · {} · MIG {} · {}",
        meta.host,
        transport,
        result.gpus.len(),
        drivers,
        if mig { "yes" } else { "no" },
        apps
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_formats_host_and_port() {
        let cfg = SshConfig {
            hostname: Some("10.0.0.1".into()),
            port: Some(2222),
            ..SshConfig::default()
        };
        assert_eq!(endpoint(&cfg), "10.0.0.1:2222");
        assert_eq!(endpoint(&SshConfig::default()), "?");
    }

    #[test]
    fn transport_labels_are_stable() {
        assert_eq!(crate::config::TransportMode::Direct.to_string(), "direct");
        assert_eq!(TransportUsed::Remote.to_string(), "remote");
    }
}
