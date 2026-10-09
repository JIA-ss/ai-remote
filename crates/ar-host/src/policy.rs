//! Policy: which calls run, which ask the owner first, which are refused.
//!
//! Rules from `ai-remote.toml` are evaluated in order and the first match wins; when none matches,
//! the tool's own default applies (observe/control tools run, admin tools ask). Tool annotations
//! from mounted servers are never trusted for this.

use ar_core::config::{Action, PolicyRule, glob_match};
use ar_net::Peer;

pub fn decide(rules: &[PolicyRule], tool: &str, peer: &Peer, default: Action) -> Action {
    for rule in rules {
        if glob_match(&rule.tool, tool) && applies(rule, peer) {
            return rule.action;
        }
    }
    default
}

/// Whether `rule` concerns `peer`: it names no caller, or names this one.
pub fn applies(rule: &PolicyRule, peer: &Peer) -> bool {
    rule.principal.as_ref().is_none_or(|p| {
        peer.name == *p || peer.principal.as_deref() == Some(p.as_str()) || peer.user_id.as_deref() == Some(p.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str) -> Peer {
        Peer { name: name.into(), principal: Some(format!("eid:{name}")), user_id: None, level: None, revoked: false }
    }

    fn rule(tool: &str, action: Action, principal: Option<&str>) -> PolicyRule {
        PolicyRule { tool: tool.into(), action, principal: principal.map(Into::into) }
    }

    #[test]
    fn first_match_wins_and_default_applies() {
        let rules = vec![rule("shell", Action::Deny, Some("bob")), rule("shell", Action::Allow, None)];
        assert_eq!(decide(&rules, "shell", &peer("bob"), Action::Ask), Action::Deny);
        assert_eq!(decide(&rules, "shell", &peer("alice"), Action::Ask), Action::Allow);
        assert_eq!(decide(&rules, "mouse", &peer("alice"), Action::Allow), Action::Allow);
    }

    #[test]
    fn principal_forms_match() {
        let rules = vec![rule("*", Action::Deny, Some("eid:bob"))];
        assert_eq!(decide(&rules, "anything", &peer("bob"), Action::Allow), Action::Deny);
    }
}
