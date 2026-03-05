pub mod dns_proxy;
pub mod firewall;

use regex::Regex;
use serde::{Deserialize, Serialize};

use hyperbox_core::NetworkMode;

pub use firewall::{
    CommandExecutor, CommandSpec, FirewallManager, RecordingExecutor, ShellExecutor, VmNetworkSpec,
    build_apply_plan, build_teardown_plan,
};
pub use dns_proxy::{DnsAllowlistProxy, ResolvedIp};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DomainPattern {
    Exact(String),
    WildcardSuffix(String),
}

impl DomainPattern {
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.is_empty() {
            return None;
        }

        if let Some(rest) = raw.strip_prefix("*.") {
            if rest.is_empty() {
                return None;
            }

            return Some(Self::WildcardSuffix(rest.to_ascii_lowercase()));
        }

        Some(Self::Exact(raw.to_ascii_lowercase()))
    }

    pub fn matches(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        match self {
            Self::Exact(exact) => &host == exact,
            Self::WildcardSuffix(suffix) => {
                host.ends_with(suffix)
                    && host
                        .strip_suffix(suffix)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowlist {
    patterns: Vec<DomainPattern>,
}

impl Allowlist {
    pub fn new(entries: &[String]) -> Self {
        let patterns = entries
            .iter()
            .filter_map(|entry| DomainPattern::parse(entry))
            .collect();
        Self { patterns }
    }

    pub fn allows(&self, domain: &str) -> bool {
        self.patterns.iter().any(|pattern| pattern.matches(domain))
    }
}

#[derive(Debug, Clone)]
pub struct NetworkPolicyEvaluator {
    allowlist: Option<Allowlist>,
    domain_re: Regex,
}

impl NetworkPolicyEvaluator {
    pub fn new(mode: &NetworkMode) -> Self {
        let allowlist = match mode {
            NetworkMode::Allowlist(entries) => Some(Allowlist::new(entries)),
            _ => None,
        };

        Self {
            allowlist,
            domain_re: Regex::new(r"(?i)^[a-z0-9.-]+$").expect("compile domain regex"),
        }
    }

    pub fn allows_domain(&self, mode: &NetworkMode, domain: &str) -> bool {
        if !self.domain_re.is_match(domain) {
            return false;
        }

        match mode {
            NetworkMode::None => false,
            NetworkMode::Full => true,
            NetworkMode::Allowlist(_) => self
                .allowlist
                .as_ref()
                .is_some_and(|allowlist| allowlist.allows(domain)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_requires_subdomain() {
        let pattern = DomainPattern::parse("*.github.com").expect("pattern");
        assert!(pattern.matches("api.github.com"));
        assert!(!pattern.matches("github.com"));
    }

    #[test]
    fn network_modes_behave() {
        let mode = NetworkMode::Allowlist(vec![
            "api.openai.com".to_string(),
            "*.github.com".to_string(),
        ]);
        let eval = NetworkPolicyEvaluator::new(&mode);
        assert!(eval.allows_domain(&mode, "api.openai.com"));
        assert!(eval.allows_domain(&mode, "gist.github.com"));
        assert!(!eval.allows_domain(&mode, "example.com"));
    }
}
