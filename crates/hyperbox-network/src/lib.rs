pub mod dns_proxy;
pub mod firewall;

use hyperbox_core::{Allowlist, NetworkMode};
use regex::Regex;

pub use dns_proxy::{DnsAllowlistProxy, ResolvedIp};
pub use firewall::{
    CommandExecutor, CommandSpec, FirewallManager, RecordingExecutor, ShellExecutor, VmNetworkSpec,
    build_allowlist_population_plan, build_apply_plan, build_teardown_plan,
};

#[derive(Debug, Clone)]
pub struct NetworkPolicyEvaluator {
    allowlist: Option<Allowlist>,
    domain_re: Regex,
}

impl NetworkPolicyEvaluator {
    pub fn new(mode: &NetworkMode) -> Self {
        let allowlist = match mode {
            NetworkMode::Allowlist(entries) => Some(entries.clone()),
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
    use hyperbox_core::AllowlistEntry;

    #[test]
    fn wildcard_requires_subdomain() {
        let pattern = AllowlistEntry::parse("*.github.com").expect("pattern");
        assert!(pattern.matches("api.github.com"));
        assert!(!pattern.matches("github.com"));
    }

    #[test]
    fn network_modes_behave() {
        let mode = NetworkMode::Allowlist(
            Allowlist::parse(&["api.openai.com".to_string(), "*.github.com".to_string()])
                .expect("allowlist"),
        );
        let eval = NetworkPolicyEvaluator::new(&mode);
        assert!(eval.allows_domain(&mode, "api.openai.com"));
        assert!(eval.allows_domain(&mode, "gist.github.com"));
        assert!(!eval.allows_domain(&mode, "example.com"));
    }
}
