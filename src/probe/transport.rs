//! Transport resolution (DESIGN §4, §5.3, §9).
//!
//! `direct` uses the ssh alias as written, `remote` maps it through
//! `remote_aliases`, and `auto` probes the alias's resolved host:port with an
//! 800 ms TCP connect before falling back to the remote alias. Whether a host
//! is *proxied* (and therefore belongs to the `parallel_remote` queue) is
//! decided separately from the transport mode, using `ssh -G`.

use std::collections::{BTreeMap, HashMap};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::{TransportMode, TransportUsed};

pub const AUTO_TCP_TIMEOUT: Duration = Duration::from_millis(800);
pub const SSH_G_CACHE_TTL: Duration = Duration::from_secs(60);
pub const HOP_LIMIT: usize = 6;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshConfig {
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub proxyjump: Option<String>,
    pub proxycommand: Option<String>,
}

fn meaningful(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("none"))
}

impl SshConfig {
    /// A host is proxied when `ssh -G` reports a real ProxyJump or
    /// ProxyCommand; `none` is treated as direct (review round 2).
    pub fn proxied(&self) -> bool {
        meaningful(&self.proxyjump).is_some() || meaningful(&self.proxycommand).is_some()
    }

    /// First jump host, stripped of `user@`, `:port`, and `[...]`.
    pub fn first_hop(&self) -> Option<String> {
        let jump = meaningful(&self.proxyjump)?;
        let first = jump.split(',').next()?.trim();
        Some(normalize_hop(first))
    }
}

fn normalize_hop(hop: &str) -> String {
    let without_user = hop.rsplit('@').next().unwrap_or(hop);
    let without_brackets = without_user.trim_start_matches('[');
    if let Some(end) = without_brackets.find(']') {
        return without_brackets[..end].to_string();
    }
    match without_brackets.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            host.to_string()
        }
        _ => without_brackets.to_string(),
    }
}

/// Parse `ssh -G <alias>` output (`key value` lines).
pub fn parse_ssh_g(output: &str, alias: &str) -> SshConfig {
    let mut cfg = SshConfig::default();
    for line in output.lines() {
        let Some((key, value)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        match key.to_ascii_lowercase().as_str() {
            "hostname" => cfg.hostname = non_empty(value),
            "port" => cfg.port = value.parse().ok(),
            "user" => cfg.user = non_empty(value),
            "proxyjump" => cfg.proxyjump = non_empty(value),
            "proxycommand" => cfg.proxycommand = non_empty(value),
            _ => {}
        }
    }
    if cfg.hostname.as_deref() == Some(alias) {
        // No ssh_config Host entry matched; ssh would use the literal name.
        // Keep it, the alias-presence check in doctor reports this fact.
    }
    cfg
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub trait SshQuery {
    fn ssh_g(&self, alias: &str) -> Option<SshConfig>;
}

#[derive(Clone, Debug)]
struct Cached {
    at: Instant,
    value: SshConfig,
}

/// 60 s in-process cache for the `auto` TCP decision, keyed by resolved
/// `host:port`. Without it, two presets containing the same alias each pay
/// another 800 ms probe (review CFG-13).
#[derive(Debug)]
pub struct TcpCache {
    ttl: Duration,
    entries: Mutex<HashMap<(String, u16), (Instant, bool)>>,
}

impl TcpCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Return the cached decision when fresh, otherwise run `probe` once and
    /// remember it.
    pub fn cached(&self, host: &str, port: u16, probe: &dyn Fn(&str, u16) -> bool) -> bool {
        let key = (host.to_string(), port);
        {
            let cache = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, value)) = cache.get(&key)
                && at.elapsed() < self.ttl
            {
                return *value;
            }
        }
        let value = probe(host, port);
        let mut cache = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(key, (Instant::now(), value));
        value
    }
}

/// Real `ssh -G` resolver with 60 s in-process caches for both the config
/// lookup and the `auto` TCP decision.
pub struct Resolver {
    binary: String,
    cache: Mutex<HashMap<String, Cached>>,
    tcp: TcpCache,
}

impl Resolver {
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            cache: Mutex::new(HashMap::new()),
            tcp: TcpCache::new(SSH_G_CACHE_TTL),
        }
    }

    pub fn binary(&self) -> &str {
        &self.binary
    }

    pub fn plan(
        &self,
        hosts: &[String],
        mode: TransportMode,
        aliases: &BTreeMap<String, String>,
    ) -> Result<Vec<HostPlan>, String> {
        plan_with_cache(
            hosts,
            mode,
            aliases,
            self,
            &|host, port| tcp_probe(host, port, AUTO_TCP_TIMEOUT),
            &self.tcp,
        )
    }
}

impl SshQuery for Resolver {
    fn ssh_g(&self, alias: &str) -> Option<SshConfig> {
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = cache.get(alias)
                && entry.at.elapsed() < SSH_G_CACHE_TTL
            {
                return Some(entry.value.clone());
            }
        }
        let output = Command::new(&self.binary)
            .arg("-G")
            .arg(alias)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let cfg = parse_ssh_g(&text, alias);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(
            alias.to_string(),
            Cached {
                at: Instant::now(),
                value: cfg.clone(),
            },
        );
        Some(cfg)
    }
}

fn tcp_probe(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, timeout).is_ok() {
            return true;
        }
    }
    false
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPlan {
    pub host: String,
    /// The ssh alias actually used for this host.
    pub alias: String,
    pub transport_used: TransportUsed,
    pub proxied: bool,
    pub first_hop: Option<String>,
}

/// Resolve plans, injecting both `ssh -G` and the TCP probe so the selection
/// logic is unit-testable.
pub fn plan_with(
    hosts: &[String],
    mode: TransportMode,
    aliases: &BTreeMap<String, String>,
    query: &dyn SshQuery,
    tcp: &dyn Fn(&str, u16) -> bool,
) -> Result<Vec<HostPlan>, String> {
    let mut plans = Vec::with_capacity(hosts.len());
    for host in hosts {
        let (alias, used) = match mode {
            TransportMode::Direct => (host.clone(), TransportUsed::Direct),
            TransportMode::Remote => {
                let alias = aliases.get(host).cloned().ok_or_else(|| {
                    format!(
                        "host {host:?}: transport = \"remote\" but no remote_aliases entry exists"
                    )
                })?;
                (alias, TransportUsed::Remote)
            }
            TransportMode::Auto => resolve_auto(host, aliases, query, tcp)?,
        };
        let cfg = query.ssh_g(&alias);
        let proxied = cfg.as_ref().is_some_and(SshConfig::proxied);
        let first_hop = cfg.as_ref().and_then(SshConfig::first_hop);
        plans.push(HostPlan {
            host: host.clone(),
            alias,
            transport_used: used,
            proxied,
            first_hop,
        });
    }
    Ok(plans)
}

/// Like [`plan_with`], but the injected TCP closure is wrapped in a 60 s
/// cache so repeated auto resolutions of the same host:port probe once.
pub fn plan_with_cache(
    hosts: &[String],
    mode: TransportMode,
    aliases: &BTreeMap<String, String>,
    query: &dyn SshQuery,
    tcp: &dyn Fn(&str, u16) -> bool,
    cache: &TcpCache,
) -> Result<Vec<HostPlan>, String> {
    plan_with(hosts, mode, aliases, query, &|host, port| {
        cache.cached(host, port, tcp)
    })
}

fn resolve_auto(
    host: &str,
    aliases: &BTreeMap<String, String>,
    query: &dyn SshQuery,
    tcp: &dyn Fn(&str, u16) -> bool,
) -> Result<(String, TransportUsed), String> {
    let cfg = query.ssh_g(host);
    let direct_ok = cfg.as_ref().is_some_and(|cfg| {
        let hostname = cfg.hostname.as_deref().unwrap_or(host);
        !hostname.is_empty() && tcp(hostname, cfg.port.unwrap_or(22))
    });
    if direct_ok {
        return Ok((host.to_string(), TransportUsed::Direct));
    }
    match aliases.get(host) {
        Some(alias) => Ok((alias.clone(), TransportUsed::Remote)),
        None => Err(format!(
            "host {host:?}: direct connection failed and no remote_aliases entry exists for transport = \"auto\""
        )),
    }
}

/// A short label for the first hop, used as the per-hop concurrency key.
pub fn hop_key(plan: &HostPlan) -> Option<String> {
    if plan.proxied {
        plan.first_hop
            .clone()
            .or_else(|| Some(format!("proxycommand:{}", plan.alias)))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeQuery(HashMap<String, SshConfig>);

    impl FakeQuery {
        fn new(entries: &[(&str, &str, Option<&str>)]) -> Self {
            let mut map = HashMap::new();
            for (alias, hostname, jump) in entries {
                map.insert(
                    alias.to_string(),
                    SshConfig {
                        hostname: Some(hostname.to_string()),
                        port: Some(22),
                        user: None,
                        proxyjump: jump.map(str::to_string),
                        proxycommand: Some("none".into()),
                    },
                );
            }
            Self(map)
        }
    }

    impl SshQuery for FakeQuery {
        fn ssh_g(&self, alias: &str) -> Option<SshConfig> {
            self.0.get(alias).cloned()
        }
    }

    fn aliases(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_ssh_g_output() {
        let text = "\
host gpu06r
hostname 10.0.0.6
user root
port 2222
proxyjump ali
proxycommand none
";
        let cfg = parse_ssh_g(text, "gpu06r");
        assert_eq!(cfg.hostname.as_deref(), Some("10.0.0.6"));
        assert_eq!(cfg.port, Some(2222));
        assert_eq!(cfg.user.as_deref(), Some("root"));
        assert!(cfg.proxied());
        assert_eq!(cfg.first_hop().as_deref(), Some("ali"));
    }

    #[test]
    fn proxycommand_none_is_direct() {
        let cfg = SshConfig {
            proxycommand: Some("none".into()),
            proxyjump: Some("none".into()),
            ..SshConfig::default()
        };
        assert!(!cfg.proxied());
    }

    #[test]
    fn first_hop_strips_user_and_port() {
        let cfg = SshConfig {
            proxyjump: Some("alice@jump.example:2200,bob@second:22".into()),
            ..SshConfig::default()
        };
        assert_eq!(cfg.first_hop().as_deref(), Some("jump.example"));
    }

    #[test]
    fn direct_plan_uses_alias_verbatim() {
        let q = FakeQuery::new(&[("a", "a.example", None)]);
        let plans = plan_with(
            &["a".to_string()],
            TransportMode::Direct,
            &BTreeMap::new(),
            &q,
            &|_, _| false,
        )
        .unwrap();
        assert_eq!(plans[0].alias, "a");
        assert_eq!(plans[0].transport_used, TransportUsed::Direct);
        assert!(!plans[0].proxied);
    }

    #[test]
    fn remote_plan_requires_mapping() {
        let q = FakeQuery::new(&[]);
        let err = plan_with(
            &["a".to_string()],
            TransportMode::Remote,
            &BTreeMap::new(),
            &q,
            &|_, _| false,
        )
        .unwrap_err();
        assert!(err.contains("no remote_aliases entry"), "{err}");
    }

    #[test]
    fn remote_plan_marks_proxied_when_jump_present() {
        let q = FakeQuery::new(&[("ar", "a.example", Some("jump"))]);
        let plans = plan_with(
            &["a".to_string()],
            TransportMode::Remote,
            &aliases(&[("a", "ar")]),
            &q,
            &|_, _| false,
        )
        .unwrap();
        assert_eq!(plans[0].alias, "ar");
        assert_eq!(plans[0].transport_used, TransportUsed::Remote);
        assert!(plans[0].proxied);
        assert_eq!(plans[0].first_hop.as_deref(), Some("jump"));
        assert_eq!(hop_key(&plans[0]).as_deref(), Some("jump"));
    }

    #[test]
    fn auto_prefers_direct_when_tcp_connects() {
        let q = FakeQuery::new(&[("a", "10.0.0.1", None)]);
        let plans = plan_with(
            &["a".to_string()],
            TransportMode::Auto,
            &aliases(&[("a", "ar")]),
            &q,
            &|host, port| host == "10.0.0.1" && port == 22,
        )
        .unwrap();
        assert_eq!(plans[0].alias, "a");
        assert_eq!(plans[0].transport_used, TransportUsed::Direct);
    }

    #[test]
    fn auto_falls_back_to_remote_alias() {
        let q = FakeQuery::new(&[("a", "10.0.0.1", None), ("ar", "10.0.0.1", Some("jump"))]);
        let plans = plan_with(
            &["a".to_string()],
            TransportMode::Auto,
            &aliases(&[("a", "ar")]),
            &q,
            &|_, _| false,
        )
        .unwrap();
        assert_eq!(plans[0].alias, "ar");
        assert_eq!(plans[0].transport_used, TransportUsed::Remote);
        assert!(plans[0].proxied);
    }

    #[test]
    fn auto_without_mapping_is_an_error() {
        let q = FakeQuery::new(&[("a", "10.0.0.1", None)]);
        let err = plan_with(
            &["a".to_string()],
            TransportMode::Auto,
            &BTreeMap::new(),
            &q,
            &|_, _| false,
        )
        .unwrap_err();
        assert!(err.contains("no remote_aliases entry"), "{err}");
    }

    #[test]
    fn auto_tolerates_ssh_g_failure_if_alias_exists() {
        let q = FakeQuery::new(&[("ar", "10.0.0.1", Some("jump"))]);
        let plans = plan_with(
            &["a".to_string()],
            TransportMode::Auto,
            &aliases(&[("a", "ar")]),
            &q,
            &|_, _| false,
        )
        .unwrap();
        assert_eq!(plans[0].alias, "ar");
    }

    #[test]
    fn auto_tcp_decision_is_cached_across_resolutions() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let q = FakeQuery::new(&[("a", "10.0.0.1", None)]);
        let cache = TcpCache::new(SSH_G_CACHE_TTL);
        let probes = AtomicUsize::new(0);
        let tcp = |host: &str, port: u16| -> bool {
            probes.fetch_add(1, Ordering::SeqCst);
            host == "10.0.0.1" && port == 22
        };
        let hosts = ["a".to_string()];

        for _ in 0..2 {
            let plans = plan_with_cache(
                &hosts,
                TransportMode::Auto,
                &BTreeMap::new(),
                &q,
                &tcp,
                &cache,
            )
            .unwrap();
            assert_eq!(plans[0].transport_used, TransportUsed::Direct);
        }
        assert_eq!(
            probes.load(Ordering::SeqCst),
            1,
            "one TCP probe for two resolutions"
        );
    }

    #[test]
    fn auto_tcp_cache_keys_on_host_and_port() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let q = FakeQuery::new(&[("a", "10.0.0.1", None), ("b", "10.0.0.2", None)]);
        let cache = TcpCache::new(SSH_G_CACHE_TTL);
        let probes = AtomicUsize::new(0);
        let tcp = |_: &str, _: u16| -> bool {
            probes.fetch_add(1, Ordering::SeqCst);
            false
        };
        let _ = plan_with_cache(
            &["a".to_string(), "b".to_string()],
            TransportMode::Auto,
            &aliases(&[("a", "ar"), ("b", "br")]),
            &q,
            &tcp,
            &cache,
        )
        .unwrap();
        assert_eq!(
            probes.load(Ordering::SeqCst),
            2,
            "distinct endpoints probe separately"
        );
    }

    #[test]
    fn proxied_decision_is_independent_of_transport() {
        let q = FakeQuery::new(&[("a", "a.example", Some("jump"))]);
        let plans = plan_with(
            &["a".to_string()],
            TransportMode::Direct,
            &BTreeMap::new(),
            &q,
            &|_, _| true,
        )
        .unwrap();
        assert_eq!(plans[0].transport_used, TransportUsed::Direct);
        assert!(plans[0].proxied);
    }
}
