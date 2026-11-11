//! Channel namespaces (`rls-coverage.md` G17, FB-136).
//!
//! A caller with a tenant (a tenant-bound key, a selected
//! `X-ForgeBase-Tenant`, a function running for a tenant) lives in that
//! tenant's namespace: the channel `chat` it names is routed as
//! `t:<tenant>:chat`, so two tenants using the same name never meet. The
//! explicit form `t:<tenant>:chat` names the same channel; only that
//! tenant's callers and privileged callers may use it. Callers without a
//! tenant keep the plain names.
//!
//! `global:<name>` channels are shared by every tenant; any caller the
//! channel rule admits may subscribe, only privileged callers (the system,
//! administrators, admin service accounts) publish or join their presence.
//! The server's own channels ([`SYSTEM_PREFIXES`], job progress) are never
//! namespaced: their authorizer decides who reads them.
//!
//! Authorizers (channel rules, permission keys) see the name **without**
//! the tenant prefix ([`Channel::rule_name`]): rules are written once per
//! channel name and use `@request.tenant` to tell tenants apart.

use forgebase_data::AccessContext;

use crate::RealtimeError;
use crate::protocol::valid_channel;

/// Prefix of the channels every tenant shares.
pub const GLOBAL_PREFIX: &str = "global:";

/// Prefix of a tenant's channels: `t:<tenant>:<name>`.
pub const TENANT_PREFIX: &str = "t:";

/// Channels the server publishes on as the system for every tenant (job
/// progress, `forgebase-jobs`): never namespaced.
pub const SYSTEM_PREFIXES: [&str; 1] = ["jobs:"];

/// A channel as a caller named it and where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// The name the caller used (echoed in the messages it receives).
    pub name: String,
    /// The name channel rules and permission keys see: the tenant's own
    /// name for it, without `t:<tenant>:`.
    pub rule_name: String,
    /// The name messages and presence are routed by.
    pub routed: String,
    /// Whether the channel is shared by every tenant (`global:`).
    pub global: bool,
}

fn reserved(name: &str) -> bool {
    name.starts_with(GLOBAL_PREFIX) || name.starts_with(TENANT_PREFIX) || SYSTEM_PREFIXES.iter().any(|p| name.starts_with(p))
}

fn invalid(message: &str) -> RealtimeError {
    RealtimeError::invalid("invalid_channel", message)
}

/// Where `name` lives for `ctx`.
pub fn resolve(ctx: &AccessContext, name: &str) -> Result<Channel, RealtimeError> {
    if !valid_channel(name) {
        return Err(invalid("channel names are 1-128 characters of [A-Za-z0-9_.:-]"));
    }
    let plain = |routed: String, rule_name: &str| Channel {
        name: name.to_owned(),
        rule_name: rule_name.to_owned(),
        routed,
        global: name.starts_with(GLOBAL_PREFIX),
    };
    if name.starts_with(GLOBAL_PREFIX) || SYSTEM_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Ok(plain(name.to_owned(), name));
    }
    if let Some(rest) = name.strip_prefix(TENANT_PREFIX) {
        let Some((tenant, inner)) = rest.split_once(':').filter(|(t, i)| !t.is_empty() && !i.is_empty()) else {
            return Err(invalid("tenant channels are named t:<tenant>:<name>"));
        };
        if reserved(inner) {
            return Err(invalid("a tenant channel name cannot start with t:, global: or jobs:"));
        }
        if !ctx.is_privileged() && ctx.tenant.as_deref() != Some(tenant) {
            return Err(RealtimeError::Forbidden(format!(
                "tenant_mismatch: channel {name} belongs to another tenant"
            )));
        }
        return Ok(plain(name.to_owned(), inner));
    }
    Ok(match ctx.tenant.as_deref() {
        Some(t) => plain(format!("{TENANT_PREFIX}{t}:{name}"), name),
        None => plain(name.to_owned(), name),
    })
}

/// Refuses publishing (messages, presence) on a `global:` channel to
/// non-privileged callers.
pub fn check_publish(ctx: &AccessContext, channel: &Channel) -> Result<(), RealtimeError> {
    if channel.global && !ctx.is_privileged() {
        return Err(RealtimeError::Forbidden(format!(
            "global_channel: only administrators and the server publish on {}",
            channel.name
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(tenant: Option<&str>) -> AccessContext {
        let c = AccessContext::user("users", "u1");
        match tenant {
            Some(t) => c.with_tenant(t),
            None => c,
        }
    }

    #[test]
    fn tenant_callers_get_their_namespace() {
        let c = resolve(&user(Some("env1")), "chat").unwrap();
        assert_eq!((c.name.as_str(), c.rule_name.as_str(), c.routed.as_str()), ("chat", "chat", "t:env1:chat"));
        let c = resolve(&user(Some("env1")), "t:env1:chat").unwrap();
        assert_eq!((c.rule_name.as_str(), c.routed.as_str()), ("chat", "t:env1:chat"));
        let e = resolve(&user(Some("env1")), "t:env2:chat").unwrap_err();
        assert!(e.to_string().contains("tenant_mismatch"), "{e}");
        let e = resolve(&user(None), "t:env2:chat").unwrap_err();
        assert!(e.to_string().contains("tenant_mismatch"), "{e}");
        assert_eq!(resolve(&user(None), "chat").unwrap().routed, "chat");
    }

    #[test]
    fn global_system_and_privileged_names() {
        let c = resolve(&user(Some("env1")), "global:news").unwrap();
        assert!(c.global);
        assert_eq!(c.routed, "global:news");
        assert!(check_publish(&user(Some("env1")), &c).is_err());
        assert!(check_publish(&AccessContext::admin("a"), &c).is_ok());
        assert!(check_publish(&AccessContext::system(), &c).is_ok());
        assert_eq!(resolve(&user(Some("env1")), "jobs:j1").unwrap().routed, "jobs:j1");
        let admin = AccessContext::admin("a");
        assert_eq!(resolve(&admin, "t:env2:chat").unwrap().routed, "t:env2:chat");
        assert_eq!(resolve(&AccessContext::system().with_tenant("env3"), "chat").unwrap().routed, "t:env3:chat");
        for bad in ["t:env1", "t::x", "t:env1:global:x", "t:env1:t:env2:x", "t:env1:jobs:1"] {
            assert!(resolve(&user(Some("env1")), bad).is_err(), "{bad}");
        }
    }
}
