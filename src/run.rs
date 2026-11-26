//! Top-level orchestration: subcommands, config, source resolution, output
//! mode selection, and exit codes (DESIGN §3.1, §4, §9, A6/A8–A10/A17/A21/A22).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Read};
use std::sync::Arc;
use std::time::Instant;

use crate::cli::{Cli, Command, Format, PresetsAction};
use crate::config::{self, Config, ConfigError, InitOutcome, Settings, TransportMode};
use crate::model::{ErrorKind, HostResult, HostStatus, ProbeErrorInfo, Snapshot};
use crate::output;
use crate::probe::transport::{Resolver, SshQuery};
use crate::probe::{self, HostMeta, JobSpec};
use crate::tui::App;

pub fn run(cli: Cli) -> i32 {
    if cli.print_config && cli.command.is_some() {
        eprintln!("gpufree: --print-config cannot be combined with a subcommand");
        return 2;
    }
    match &cli.command {
        Some(Command::Init) => return cmd_init(&cli),
        Some(Command::Presets { action }) => return cmd_presets(&cli, action),
        Some(Command::Doctor { probe }) => return cmd_doctor(&cli, *probe),
        Some(Command::Completion(args)) => return crate::completion::run(args),
        None => {}
    }
    if cli.print_config {
        return cmd_print_config(&cli);
    }
    cmd_probe(cli)
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

fn cmd_init(cli: &Cli) -> i32 {
    match config::init(cli.config.as_deref()) {
        Ok(InitOutcome::Created(path)) => {
            crate::outln!("wrote {}", path.display());
            0
        }
        Ok(InitOutcome::Exists(path)) => {
            // A6: existing file is not an error, but the notice goes to stderr.
            eprintln!(
                "gpufree: config already exists at {}; leaving it unchanged",
                path.display()
            );
            0
        }
        Err(err) => {
            eprintln!("gpufree: {err}");
            2
        }
    }
}

fn cmd_presets(cli: &Cli, action: &PresetsAction) -> i32 {
    let loaded = match config::load(cli.config.as_deref()) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("gpufree: {err}");
            return match action {
                PresetsAction::Validate => validate_exit(&err),
                PresetsAction::List => 2,
            };
        }
    };
    match action {
        PresetsAction::List => {
            if loaded.config.presets.is_empty() {
                crate::outln!("no presets configured");
            } else {
                crate::outln!(
                    "{:<20} {:<8} {:>5}  DESCRIPTION",
                    "PRESET",
                    "TRANSPORT",
                    "HOSTS"
                );
                for (name, preset) in &loaded.config.presets {
                    crate::outln!(
                        "{:<20} {:<8} {:>5}  {}",
                        name,
                        preset.transport.to_string(),
                        preset.hosts.len(),
                        preset.description.as_deref().unwrap_or("")
                    );
                }
            }
            0
        }
        PresetsAction::Validate => {
            for (name, preset) in &loaded.config.presets {
                crate::outln!(
                    "preset {name}: ok ({} hosts, transport {})",
                    preset.hosts.len(),
                    preset.transport
                );
            }
            crate::outln!("configuration valid");
            0
        }
    }
}

/// A6: validation failures exit 1; TOML/IO/usage problems exit 2.
fn validate_exit(err: &ConfigError) -> i32 {
    match err {
        ConfigError::Validation { .. } => 1,
        _ => 2,
    }
}

fn cmd_doctor(cli: &Cli, probe: bool) -> i32 {
    let loaded = match config::load(cli.config.as_deref()) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("gpufree: {err}");
            return 2;
        }
    };
    crate::signal::install();
    // `--probe` builds the same ssh argv as a normal probe, so it must honour
    // `--no-mux` too (cmd_probe / cmd_print_config already do).
    let mut config: Config = loaded.config.clone();
    if cli.no_mux {
        config.settings.mux = false;
    }
    crate::doctor::run(&config, &loaded.path, loaded.present, probe, cli.utc)
}

fn cmd_print_config(cli: &Cli) -> i32 {
    let loaded = match config::load(cli.config.as_deref()) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("gpufree: {err}");
            return 2;
        }
    };
    let mut config: Config = loaded.config.clone();
    if cli.no_mux {
        config.settings.mux = false;
    }
    if let Some(color) = cli.color {
        config.settings.color = color;
    }

    crate::outln!("# effective configuration from {}", loaded.path.display());
    crate::outln!("# (contains no secrets; transport resolution shown below)");
    crate::out!("{}", config.to_toml());

    crate::outln!("\n[resolved]");
    let resolver = Resolver::new(config.settings.ssh_binary.clone());
    let mut failed = false;
    if config.presets.is_empty() {
        crate::outln!("# no presets configured");
    }
    for (name, preset) in &config.presets {
        let mode = cli.transport.unwrap_or(preset.transport);
        crate::outln!("# preset {name} (transport {mode})");
        match resolver.plan(&preset.hosts, mode, &preset.remote_aliases) {
            Ok(plans) => {
                for plan in plans {
                    let endpoint = resolver
                        .ssh_g(&plan.alias)
                        .map(|cfg| match (cfg.hostname, cfg.port) {
                            (Some(host), Some(port)) => format!("{host}:{port}"),
                            (Some(host), None) => host,
                            _ => "?".to_string(),
                        })
                        .unwrap_or_else(|| "?".to_string());
                    crate::outln!(
                        "#   {} -> {}  transport={} endpoint={} proxied={}",
                        plan.host,
                        plan.alias,
                        plan.transport_used,
                        endpoint,
                        plan.proxied
                    );
                }
            }
            Err(err) => {
                crate::outln!("#   ERROR: {err}");
                failed = true;
            }
        }
    }
    if failed { 2 } else { 0 }
}

// ---------------------------------------------------------------------------
// Probe mode
// ---------------------------------------------------------------------------

struct Source {
    hosts: Vec<String>,
    prefix: Option<String>,
    aliases: BTreeMap<String, String>,
    transport: TransportMode,
    preset: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Tui,
    Json,
    Table,
}

fn cmd_probe(cli: Cli) -> i32 {
    let loaded = match config::load(cli.config.as_deref()) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("gpufree: {err}");
            return 2;
        }
    };

    let mut settings = loaded.config.settings.clone();
    if cli.no_mux {
        settings.mux = false;
    }
    if let Some(color) = cli.color {
        settings.color = color;
    }

    let source = match resolve_source(&cli, &loaded) {
        Ok(source) => source,
        Err(message) => {
            eprintln!("gpufree: {message}");
            if message.starts_with("no host source") {
                print_examples(&loaded);
            }
            return 2;
        }
    };

    if let Some(host) = source.hosts.iter().find(|host| host.starts_with('-')) {
        eprintln!(
            "gpufree: refusing host {host:?}: host names must not start with '-' (option injection)"
        );
        return 2;
    }
    if source.hosts.is_empty() {
        eprintln!("gpufree: no hosts to probe");
        return 2;
    }
    if !probe::ssh::ssh_available(&settings.ssh_binary) {
        eprintln!(
            "gpufree: ssh binary {:?} is not executable; run `gpufree doctor`",
            settings.ssh_binary
        );
        return 2;
    }

    // A8: resolve every transport before starting any probe.
    let resolver = Resolver::new(settings.ssh_binary.clone());
    let plans = match resolver.plan(&source.hosts, source.transport, &source.aliases) {
        Ok(plans) => plans,
        Err(message) => {
            eprintln!("gpufree: {message}");
            return 2;
        }
    };
    let specs: Vec<JobSpec> = plans
        .into_iter()
        .enumerate()
        .map(|(index, plan)| JobSpec {
            index,
            host: plan.host.clone(),
            plan,
            prefix: source.prefix.clone(),
        })
        .collect();
    let metas: Vec<HostMeta> = specs
        .iter()
        .map(|spec| HostMeta {
            host: spec.host.clone(),
            resolved_alias: spec.plan.alias.clone(),
            transport_used: spec.plan.transport_used,
        })
        .collect();

    let output_mode = choose_output(&cli);
    crate::signal::install();
    let settings = Arc::new(settings);

    match output_mode {
        OutputMode::Tui => run_tui(
            specs,
            metas,
            settings,
            source.preset,
            source.transport,
            cli.free_only,
            cli.utc,
        ),
        OutputMode::Json | OutputMode::Table => run_batch(
            specs,
            metas,
            settings,
            source.preset,
            source.transport,
            output_mode,
            cli.free_only,
            cli.strict,
            cli.require_free,
            cli.utc,
        ),
    }
}

fn choose_output(cli: &Cli) -> OutputMode {
    if cli.json {
        return OutputMode::Json;
    }
    if let Some(format) = cli.format {
        return match format {
            Format::Json => OutputMode::Json,
            Format::Table => OutputMode::Table,
        };
    }
    // A9/A10: TUI only when neither format was requested and both ends are TTY.
    if std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
        OutputMode::Tui
    } else {
        OutputMode::Table
    }
}

fn resolve_source(cli: &Cli, loaded: &config::LoadedConfig) -> Result<Source, String> {
    if let Some(name) = &cli.preset {
        let preset = loaded.config.preset(name).ok_or_else(|| {
            format!(
                "preset {name:?} not found{}",
                if loaded.present {
                    format!("; see `gpufree presets list` ({})", loaded.path.display())
                } else {
                    format!(
                        "; no config file exists at {} — run `gpufree init`",
                        loaded.path.display()
                    )
                }
            )
        })?;
        return Ok(Source {
            hosts: preset.hosts.clone(),
            prefix: preset.prefix.clone(),
            aliases: preset.remote_aliases.clone(),
            transport: cli.transport.unwrap_or(preset.transport),
            preset: Some(name.clone()),
        });
    }

    let explicit_given = !cli.host.is_empty()
        || !cli.hosts.is_empty()
        || !cli.positional.is_empty()
        || cli.hosts_file.is_some();
    let mut raw: Vec<String> = Vec::new();
    raw.extend(cli.host.iter().cloned());
    raw.extend(cli.hosts.iter().cloned());
    raw.extend(cli.positional.iter().cloned());
    if let Some(path) = &cli.hosts_file {
        raw.extend(read_hosts_file(path)?);
    }

    if !explicit_given {
        if let Some(default) = &loaded.config.default_preset {
            let preset = loaded
                .config
                .preset(default)
                .ok_or_else(|| format!("default_preset {default:?} does not exist in [presets]"))?;
            return Ok(Source {
                hosts: preset.hosts.clone(),
                prefix: preset.prefix.clone(),
                aliases: preset.remote_aliases.clone(),
                transport: cli.transport.unwrap_or(preset.transport),
                preset: Some(default.clone()),
            });
        }
        return Err("no host source given".to_string());
    }

    let hosts = dedupe_hosts(raw);
    if hosts.is_empty() {
        return Err(format!(
            "no host source given: the supplied list{} was empty",
            if cli.hosts_file.is_some() {
                "/file"
            } else {
                ""
            }
        ));
    }
    Ok(Source {
        hosts,
        prefix: None,
        aliases: BTreeMap::new(),
        transport: cli.transport.unwrap_or(TransportMode::Direct),
        preset: None,
    })
}

fn dedupe_hosts(raw: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut hosts = Vec::new();
    for host in raw {
        let host = host.trim().to_string();
        if host.is_empty() {
            continue;
        }
        if seen.insert(host.clone()) {
            hosts.push(host);
        }
    }
    hosts
}

/// A21: one host per line, surrounding whitespace trimmed, blank lines and
/// lines whose first non-blank character is `#` ignored, CRLF and UTF-8 BOM
/// tolerated, no inline comments.
fn read_hosts_file(path: &str) -> Result<Vec<String>, String> {
    let text = if path == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|err| format!("cannot read hosts from stdin: {err}"))?;
        buffer
    } else {
        std::fs::read_to_string(path)
            .map_err(|err| format!("cannot read hosts file {path:?}: {err}"))?
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

fn print_examples(loaded: &config::LoadedConfig) {
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  gpufree --preset company");
    eprintln!("  gpufree --host 4090-1 --host gpu06");
    eprintln!("  gpufree --hosts 4090-1,gpu06,h20-5");
    eprintln!("  gpufree 4090-1 gpu06");
    eprintln!("  gpufree --hosts-file -");
    eprintln!();
    if loaded.present {
        eprintln!(
            "No default_preset is configured in {}.",
            loaded.path.display()
        );
    } else {
        eprintln!(
            "No config file at {} — create one with `gpufree init`.",
            loaded.path.display()
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn run_batch(
    specs: Vec<JobSpec>,
    metas: Vec<HostMeta>,
    settings: Arc<Settings>,
    preset: Option<String>,
    transport: TransportMode,
    mode: OutputMode,
    free_only: bool,
    strict: bool,
    require_free: bool,
    utc: bool,
) -> i32 {
    let total = specs.len();
    let started = Instant::now();
    let handle = probe::spawn(specs, Arc::clone(&settings), utc);

    let mut slots: Vec<Option<HostResult>> = (0..total).map(|_| None).collect();
    for (index, result) in handle.rx {
        if index < slots.len() {
            slots[index] = Some(result);
        }
    }
    if handle.canceller.is_cancelled() {
        // Signal-driven cancellation: stop without emitting a misleading
        // partial document.
        return 130;
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    let generated_at = probe::now_rfc3339(utc);
    let hosts: Vec<HostResult> = slots
        .into_iter()
        .enumerate()
        .map(|(index, slot)| slot.unwrap_or_else(|| missing_result(&metas[index], utc)))
        .collect();
    let mut snapshot = Snapshot::new(generated_at, preset, transport, duration_ms, hosts);
    if free_only {
        snapshot.apply_free_only();
    }

    let stdout_is_tty = std::io::stdout().is_terminal();
    match mode {
        OutputMode::Json => match output::json::render(&snapshot) {
            Ok(text) => crate::out!("{text}"),
            Err(err) => {
                eprintln!("gpufree: failed to serialize JSON: {err}");
                return 1;
            }
        },
        OutputMode::Table => {
            let color = settings.color.enabled(stdout_is_tty);
            crate::out!("{}", output::table::render(&snapshot, color));
        }
        OutputMode::Tui => unreachable!("TUI handled separately"),
    }
    snapshot.exit_code(strict, require_free)
}

fn missing_result(meta: &HostMeta, utc: bool) -> HostResult {
    HostResult {
        host: meta.host.clone(),
        resolved_alias: meta.resolved_alias.clone(),
        transport_used: meta.transport_used,
        status: HostStatus::Error,
        probed_at: probe::now_rfc3339(utc),
        duration_ms: 0,
        hostname: None,
        load1: None,
        cpus: None,
        fields_complete: false,
        processes_complete: false,
        note: None,
        gpus: Vec::new(),
        error: Some(ProbeErrorInfo::new(
            ErrorKind::Unknown,
            "probe did not return a result",
        )),
    }
}

fn run_tui(
    specs: Vec<JobSpec>,
    metas: Vec<HostMeta>,
    settings: Arc<Settings>,
    preset: Option<String>,
    transport: TransportMode,
    free_only: bool,
    utc: bool,
) -> i32 {
    let specs_for_spawn = specs.clone();
    let settings_for_spawn = Arc::clone(&settings);
    let spawner: crate::tui::SpawnFn = Box::new(move || {
        probe::spawn(
            specs_for_spawn.clone(),
            Arc::clone(&settings_for_spawn),
            utc,
        )
    });
    let color = settings.color.enabled(true);
    let mut app = App::new(metas, preset, transport, free_only, color, Some(spawner));
    app.refresh();
    match crate::tui::run(app) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("gpufree: terminal error: {err}");
            1
        }
    }
}
