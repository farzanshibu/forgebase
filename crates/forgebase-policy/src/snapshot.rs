//! The in-memory authorization model (ADR-0008 §7).
//!
//! Every table of migration 0004 is loaded into one immutable [`Snapshot`].
//! Decisions read only the snapshot, so the data core's synchronous
//! `Authorizer` never touches the database. Every write of the policy service
//! loads a fresh snapshot and swaps it in: that swap is the permission-cache
//! invalidation (the effective-permission cache lives inside the snapshot and
//! dies with it), so no change to a role, membership, team, override, ACL
//! entry or suspension can be served stale by this process.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use forgebase_db::{Backend, Rows, Statement, Value};
use serde::{Deserialize, Serialize};

use crate::PolicyError;
use crate::perm::{self, PermissionSet};
use crate::rules::Rule;

/// A realm (top isolation level).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Realm {
    /// Id.
    pub id: String,
    /// Display name.
    pub name: String,
}

/// An organization.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Org {
    /// Id.
    pub id: String,
    /// Realm.
    pub realm_id: String,
    /// Unique per realm.
    pub slug: String,
    /// Display name.
    pub name: String,
}

/// A project, owned by an organization or (personal scope) one user.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Project {
    /// Id.
    pub id: String,
    /// Realm.
    pub realm_id: String,
    /// Owning organization.
    pub org_id: Option<String>,
    /// Owning user (principal key) of a personal project.
    pub owner_user: Option<String>,
    /// Unique per owner.
    pub slug: String,
    /// Display name.
    pub name: String,
}

/// A project environment: a tenant of the data core.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Environment {
    /// Id (the tenant id).
    pub id: String,
    /// Project.
    pub project_id: String,
    /// `development`, `staging`, `production`, ...
    pub name: String,
}

/// A role (realm scope when `org_id` is `None`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Role {
    /// Id.
    pub id: String,
    /// Realm.
    pub realm_id: String,
    /// Organization of an org-defined role.
    pub org_id: Option<String>,
    /// Key used in memberships.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Granted permission patterns.
    pub allow: Vec<String>,
    /// Denied permission patterns.
    pub deny: Vec<String>,
    /// Seeded.
    pub system: bool,
    /// `false` for the platform super-admin role: never granted by a membership.
    pub assignable: bool,
}

/// A named bundle of roles.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoleSet {
    /// Id.
    pub id: String,
    /// Realm.
    pub realm_id: String,
    /// Organization of an org-defined set.
    pub org_id: Option<String>,
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Role keys.
    pub roles: Vec<String>,
}

/// Roles of a principal in an organization, or at realm level.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Grantee {
    /// Id.
    pub id: String,
    /// Organization (memberships) or realm (realm bindings).
    pub scope_id: String,
    /// Principal key.
    pub principal: String,
    /// Role keys.
    pub roles: Vec<String>,
    /// Role-set keys.
    pub role_sets: Vec<String>,
    /// `active` or `suspended` (memberships).
    pub status: String,
}

/// A team (tree inside an organization).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Team {
    /// Id.
    pub id: String,
    /// Organization.
    pub org_id: String,
    /// Parent team.
    pub parent_id: Option<String>,
    /// Key, unique per organization.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Roles every member (and every member of a descendant team) holds.
    pub roles: Vec<String>,
    /// Denied patterns, inherited the same way.
    pub deny: Vec<String>,
    /// Member principal keys.
    pub members: Vec<String>,
}

/// Allow or deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Grants.
    Allow,
    /// Denies (wins).
    Deny,
}

impl Effect {
    fn parse(s: &str) -> Self {
        if s == "allow" { Effect::Allow } else { Effect::Deny }
    }

    /// Stored text.
    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Allow => "allow",
            Effect::Deny => "deny",
        }
    }
}

/// An explicit per-principal allow or deny.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Override {
    /// Id.
    pub id: String,
    /// Realm.
    pub realm_id: String,
    /// Organization (`None`: realm-wide).
    pub org_id: Option<String>,
    /// Principal key.
    pub principal: String,
    /// Pattern.
    pub permission: String,
    /// Effect.
    pub effect: Effect,
}

/// One resource ACL entry (`resource_permissions`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AclEntry {
    /// Id.
    pub id: String,
    /// Realm.
    pub realm_id: String,
    /// `project`, `environment`, `collection`, `record`, `file`, ...
    pub resource_type: String,
    /// Resource id (`<collection>/<id>` for records).
    pub resource_id: String,
    /// Principal key, `team:<id>` or `role:<key>`.
    pub grantee: String,
    /// Pattern.
    pub permission: String,
    /// Effect.
    pub effect: Effect,
}

/// A collection rule.
#[derive(Debug, Clone)]
pub enum RuleSpec {
    /// `""`: everyone.
    Public,
    /// An expression.
    Expr(Rule),
}

/// Read and write rules of one field (`None`: unrestricted).
#[derive(Debug, Clone, Default)]
pub struct FieldRule {
    /// Who may read the field.
    pub read: Option<RuleSpec>,
    /// Who may write the field.
    pub write: Option<RuleSpec>,
}

/// The rules of one collection. A missing rule locks the action.
#[derive(Debug, Clone, Default)]
pub struct CollectionPolicy {
    /// list, view, create, update, delete.
    pub rules: [Option<RuleSpec>; 5],
    /// Field rules by lower-case field name.
    pub fields: HashMap<String, FieldRule>,
}

/// Effective permissions of one principal in one scope.
#[derive(Debug, Clone, Default)]
pub struct Effective {
    /// Allow and deny patterns from every source.
    pub perms: PermissionSet,
    /// Role keys held.
    pub roles: BTreeSet<String>,
    /// Ids of the teams (and their ancestors) the principal is in.
    pub teams: BTreeSet<String>,
}

/// (principal, realm, org) of a cached effective permission set.
type CacheKey = (String, String, Option<String>);

/// The whole model, immutable once loaded.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub(crate) realms: HashMap<String, Realm>,
    pub(crate) orgs: HashMap<String, Org>,
    pub(crate) projects: HashMap<String, Project>,
    pub(crate) envs: HashMap<String, Environment>,
    pub(crate) roles: Vec<Role>,
    pub(crate) role_sets: Vec<RoleSet>,
    pub(crate) realm_bindings: HashMap<(String, String), Grantee>,
    pub(crate) memberships: HashMap<(String, String), Grantee>,
    pub(crate) teams: HashMap<String, Team>,
    pub(crate) team_of: HashMap<String, Vec<String>>,
    pub(crate) overrides: Vec<Override>,
    pub(crate) acl: HashMap<(String, String), Vec<AclEntry>>,
    pub(crate) suspended: HashMap<String, Option<i64>>,
    pub(crate) policies: HashMap<String, CollectionPolicy>,
    /// Engine resource rules (FB-134): (kind, name) → rule action → rule
    /// (`None`: locked).
    pub(crate) resource_rules: HashMap<(String, String), HashMap<String, Option<RuleSpec>>>,
    /// Home realms of identities (FB-25), shared with the auth service.
    pub(crate) realm_dir: Option<forgebase_auth::realm::RealmDirectory>,
    cache: Mutex<HashMap<CacheKey, Arc<Effective>>>,
}

fn s(r: &[Value], i: usize) -> String {
    r[i].as_str().unwrap_or_default().to_owned()
}

fn opt(r: &[Value], i: usize) -> Option<String> {
    r[i].as_str().map(str::to_owned)
}

fn list(r: &[Value], i: usize) -> Vec<String> {
    r[i].as_str()
        .and_then(|t| serde_json::from_str::<Vec<String>>(t).ok())
        .unwrap_or_default()
}

async fn rows(db: &dyn Backend, sql: &str) -> Result<Rows, PolicyError> {
    Ok(db.fetch_all(&Statement::raw(sql)).await?)
}

/// A collection's rules from their stored text (list, view, create,
/// update, delete; field rules as JSON). A rule that does not parse locks
/// its action; a field rule that does not parse hides / freezes the field.
pub fn policy_from_text(rules: [Option<&str>; 5], fields_json: &str) -> CollectionPolicy {
    let mut p = CollectionPolicy::default();
    for (slot, text) in p.rules.iter_mut().zip(rules) {
        *slot = rule_spec(text).unwrap_or(None);
    }
    let fields: HashMap<String, FieldRuleText> = serde_json::from_str(fields_json).unwrap_or_default();
    for (name, f) in fields {
        let locked = || Some(RuleSpec::Expr(Rule::parse("1 = 0").expect("constant rule")));
        p.fields.insert(
            name.to_ascii_lowercase(),
            FieldRule {
                read: f
                    .read
                    .as_deref()
                    .map_or(Ok(None), |t| rule_spec(Some(t)))
                    .unwrap_or_else(|_| locked()),
                write: f
                    .write
                    .as_deref()
                    .map_or(Ok(None), |t| rule_spec(Some(t)))
                    .unwrap_or_else(|_| locked()),
            },
        );
    }
    p
}

/// Parses a stored or submitted collection rule: `None` locked, `""` public.
pub fn rule_spec(text: Option<&str>) -> Result<Option<RuleSpec>, PolicyError> {
    match text {
        None => Ok(None),
        Some(t) if t.trim().is_empty() => Ok(Some(RuleSpec::Public)),
        Some(t) => Ok(Some(RuleSpec::Expr(Rule::parse(t)?))),
    }
}

impl Snapshot {
    /// Loads everything.
    pub async fn load(db: &dyn Backend) -> Result<Self, PolicyError> {
        let mut snap = Snapshot::default();
        for r in rows(db, "SELECT id, name FROM _forgebase_realms").await?.rows {
            snap.realms.insert(
                s(&r, 0),
                Realm {
                    id: s(&r, 0),
                    name: s(&r, 1),
                },
            );
        }
        for r in rows(db, "SELECT id, realm_id, slug, name FROM _forgebase_orgs")
            .await?
            .rows
        {
            snap.orgs.insert(
                s(&r, 0),
                Org {
                    id: s(&r, 0),
                    realm_id: s(&r, 1),
                    slug: s(&r, 2),
                    name: s(&r, 3),
                },
            );
        }
        for r in rows(
            db,
            "SELECT id, realm_id, org_id, owner_user, slug, name FROM _forgebase_projects",
        )
        .await?
        .rows
        {
            snap.projects.insert(
                s(&r, 0),
                Project {
                    id: s(&r, 0),
                    realm_id: s(&r, 1),
                    org_id: opt(&r, 2),
                    owner_user: opt(&r, 3),
                    slug: s(&r, 4),
                    name: s(&r, 5),
                },
            );
        }
        for r in rows(db, "SELECT id, project_id, name FROM _forgebase_environments")
            .await?
            .rows
        {
            snap.envs.insert(
                s(&r, 0),
                Environment {
                    id: s(&r, 0),
                    project_id: s(&r, 1),
                    name: s(&r, 2),
                },
            );
        }
        for r in rows(
            db,
            "SELECT id, realm_id, org_id, key, name, description, allow, deny, system, assignable FROM _forgebase_roles ORDER BY key",
        )
        .await?
        .rows
        {
            snap.roles.push(Role {
                id: s(&r, 0),
                realm_id: s(&r, 1),
                org_id: opt(&r, 2),
                key: s(&r, 3),
                name: s(&r, 4),
                description: s(&r, 5),
                allow: list(&r, 6),
                deny: list(&r, 7),
                system: r[8].as_i64() == Some(1),
                assignable: r[9].as_i64() == Some(1),
            });
        }
        for r in rows(
            db,
            "SELECT id, realm_id, org_id, key, name, roles FROM _forgebase_role_sets",
        )
        .await?
        .rows
        {
            snap.role_sets.push(RoleSet {
                id: s(&r, 0),
                realm_id: s(&r, 1),
                org_id: opt(&r, 2),
                key: s(&r, 3),
                name: s(&r, 4),
                roles: list(&r, 5),
            });
        }
        for r in rows(
            db,
            "SELECT id, realm_id, principal, roles, role_sets FROM _forgebase_realm_bindings",
        )
        .await?
        .rows
        {
            let g = Grantee {
                id: s(&r, 0),
                scope_id: s(&r, 1),
                principal: s(&r, 2),
                roles: list(&r, 3),
                role_sets: list(&r, 4),
                status: "active".into(),
            };
            snap.realm_bindings.insert((g.scope_id.clone(), g.principal.clone()), g);
        }
        for r in rows(
            db,
            "SELECT id, org_id, principal, roles, role_sets, status FROM _forgebase_memberships",
        )
        .await?
        .rows
        {
            let g = Grantee {
                id: s(&r, 0),
                scope_id: s(&r, 1),
                principal: s(&r, 2),
                roles: list(&r, 3),
                role_sets: list(&r, 4),
                status: s(&r, 5),
            };
            snap.memberships.insert((g.scope_id.clone(), g.principal.clone()), g);
        }
        for r in rows(
            db,
            "SELECT id, org_id, parent_id, key, name, roles, deny FROM _forgebase_teams",
        )
        .await?
        .rows
        {
            snap.teams.insert(
                s(&r, 0),
                Team {
                    id: s(&r, 0),
                    org_id: s(&r, 1),
                    parent_id: opt(&r, 2),
                    key: s(&r, 3),
                    name: s(&r, 4),
                    roles: list(&r, 5),
                    deny: list(&r, 6),
                    members: Vec::new(),
                },
            );
        }
        for r in rows(
            db,
            "SELECT team_id, principal FROM _forgebase_team_members ORDER BY principal",
        )
        .await?
        .rows
        {
            if let Some(t) = snap.teams.get_mut(&s(&r, 0)) {
                t.members.push(s(&r, 1));
            }
            snap.team_of.entry(s(&r, 1)).or_default().push(s(&r, 0));
        }
        for r in rows(
            db,
            "SELECT id, realm_id, org_id, principal, permission, effect FROM _forgebase_permission_overrides",
        )
        .await?
        .rows
        {
            snap.overrides.push(Override {
                id: s(&r, 0),
                realm_id: s(&r, 1),
                org_id: opt(&r, 2),
                principal: s(&r, 3),
                permission: s(&r, 4),
                effect: Effect::parse(&s(&r, 5)),
            });
        }
        for r in rows(
            db,
            "SELECT id, realm_id, resource_type, resource_id, grantee, permission, effect FROM _forgebase_resource_permissions",
        )
        .await?
        .rows
        {
            let e = AclEntry {
                id: s(&r, 0),
                realm_id: s(&r, 1),
                resource_type: s(&r, 2),
                resource_id: s(&r, 3),
                grantee: s(&r, 4),
                permission: s(&r, 5),
                effect: Effect::parse(&s(&r, 6)),
            };
            snap.acl
                .entry((e.resource_type.clone(), e.resource_id.to_ascii_lowercase()))
                .or_default()
                .push(e);
        }
        for r in rows(db, "SELECT principal, until_at FROM _forgebase_suspensions")
            .await?
            .rows
        {
            snap.suspended.insert(s(&r, 0), r[1].as_i64());
        }
        for r in rows(
            db,
            "SELECT collection_id, list_rule, view_rule, create_rule, update_rule, delete_rule, fields FROM _forgebase_collection_policies",
        )
        .await?
        .rows
        {
            let rules = [1, 2, 3, 4, 5].map(|i| r[i].as_str());
            let p = policy_from_text(rules, r[6].as_str().unwrap_or("{}"));
            snap.policies.insert(s(&r, 0), p);
        }
        for r in rows(db, "SELECT kind, name, action, rule FROM _forgebase_resource_rules")
            .await?
            .rows
        {
            // A stored rule that no longer parses locks the action (and does
            // not fall back to an alias or the `*` rules).
            let spec = rule_spec(r[3].as_str()).unwrap_or(None);
            snap.resource_rules
                .entry((s(&r, 0), s(&r, 1)))
                .or_default()
                .insert(s(&r, 2), spec);
        }
        Ok(snap)
    }

    /// The resource rule for `op` on a resource (FB-134): the rules of the
    /// named resource if it has any, else those saved for the name `*`; in
    /// that set the rule of the exact operation, else of its alias (`read` /
    /// `write`). `None`: no rule (locked); `Some(None)` would be an explicit
    /// lock and is flattened to `None` too.
    pub fn resource_rule(&self, kind: &str, name: &str, op: forgebase_data::ResourceOp) -> Option<&RuleSpec> {
        let set = self
            .resource_rules
            .get(&(kind.to_owned(), name.to_owned()))
            .or_else(|| self.resource_rules.get(&(kind.to_owned(), "*".to_owned())))?;
        set.get(op.as_str()).or_else(|| set.get(op.alias()))?.as_ref()
    }

    /// Whether the principal is suspended at `now` (Unix seconds).
    pub fn is_suspended(&self, principal: &str, now: i64) -> bool {
        self.suspended
            .get(principal)
            .is_some_and(|until| until.is_none_or(|u| u > now))
    }

    /// The role `key` usable in `org` of `realm`: the org's own role first,
    /// then the realm's.
    pub fn role(&self, realm: &str, org: Option<&str>, key: &str) -> Option<&Role> {
        let lower = key.to_ascii_lowercase();
        let find = |o: Option<&str>| {
            self.roles
                .iter()
                .find(|r| r.realm_id == realm && r.org_id.as_deref() == o && r.key.eq_ignore_ascii_case(&lower))
        };
        org.and_then(|o| find(Some(o))).or_else(|| find(None))
    }

    fn role_set(&self, realm: &str, org: Option<&str>, key: &str) -> Option<&RoleSet> {
        let find = |o: Option<&str>| {
            self.role_sets
                .iter()
                .find(|r| r.realm_id == realm && r.org_id.as_deref() == o && r.key.eq_ignore_ascii_case(key))
        };
        org.and_then(|o| find(Some(o))).or_else(|| find(None))
    }

    /// Ancestors of a team, itself first (cycle-safe).
    pub fn team_chain(&self, team: &str) -> Vec<&Team> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = self.teams.get(team);
        while let Some(t) = cur {
            if !seen.insert(t.id.clone()) || out.len() > 32 {
                break;
            }
            out.push(t);
            cur = t.parent_id.as_ref().and_then(|p| self.teams.get(p));
        }
        out
    }

    /// The active membership of a principal in an org.
    pub fn membership(&self, org: &str, principal: &str) -> Option<&Grantee> {
        self.memberships.get(&(org.to_owned(), principal.to_owned()))
    }

    fn add_roles(&self, eff: &mut Effective, realm: &str, org: Option<&str>, roles: &[String], sets: &[String]) {
        let mut keys: Vec<String> = roles.to_vec();
        for set in sets {
            if let Some(rs) = self.role_set(realm, org, set) {
                keys.extend(rs.roles.iter().cloned());
            }
        }
        for k in keys {
            // Unknown and non-assignable (super-admin) roles grant nothing.
            if let Some(role) = self.role(realm, org, &k).filter(|r| r.assignable) {
                eff.roles.insert(role.key.clone());
                eff.perms.allow.extend(role.allow.iter().cloned());
                eff.perms.deny.extend(role.deny.iter().cloned());
            }
        }
    }

    /// Effective permissions of `principal` in `realm`, inside `org` when
    /// given: realm bindings, org membership (active only), teams with their
    /// ancestors, then overrides. Cached until the snapshot is replaced.
    pub fn effective(&self, principal: &str, realm: &str, org: Option<&str>) -> Arc<Effective> {
        let key = (principal.to_owned(), realm.to_owned(), org.map(str::to_owned));
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(e) = cache.get(&key) {
            return e.clone();
        }
        let mut eff = Effective::default();
        if let Some(b) = self.realm_bindings.get(&(realm.to_owned(), principal.to_owned())) {
            self.add_roles(&mut eff, realm, None, &b.roles, &b.role_sets);
        }
        if let Some(o) = org.filter(|o| self.orgs.get(*o).is_some_and(|x| x.realm_id == realm))
            && let Some(m) = self.membership(o, principal).filter(|m| m.status == "active")
        {
            self.add_roles(&mut eff, realm, Some(o), &m.roles, &m.role_sets);
            for team in self.team_of.get(principal).into_iter().flatten() {
                for t in self.team_chain(team).into_iter().filter(|t| t.org_id == o) {
                    eff.teams.insert(t.id.clone());
                    self.add_roles(&mut eff, realm, Some(o), &t.roles, &[]);
                    eff.perms.deny.extend(t.deny.iter().cloned());
                }
            }
        }
        for ov in self.overrides.iter().filter(|ov| {
            ov.principal == principal && ov.realm_id == realm && ov.org_id.as_deref().is_none_or(|x| Some(x) == org)
        }) {
            match ov.effect {
                Effect::Allow => eff.perms.allow.insert(ov.permission.clone()),
                Effect::Deny => eff.perms.deny.insert(ov.permission.clone()),
            };
        }
        let eff = Arc::new(eff);
        cache.insert(key, eff.clone());
        eff
    }

    /// Whether an ACL grantee string names this caller.
    pub fn grantee_matches(&self, grantee: &str, principal: &str, eff: &Effective) -> bool {
        if let Some(team) = grantee.strip_prefix("team:") {
            eff.teams.contains(team)
        } else if let Some(role) = grantee.strip_prefix("role:") {
            eff.roles.iter().any(|r| r.eq_ignore_ascii_case(role))
        } else {
            grantee == principal
        }
    }

    /// The ACL verdict for `key` on one resource: deny wins.
    pub fn acl(&self, realm: &str, rtype: &str, rid: &str, key: &str, principal: &str, eff: &Effective) -> perm::Grant {
        let mut granted = false;
        for e in self
            .acl
            .get(&(rtype.to_owned(), rid.to_ascii_lowercase()))
            .into_iter()
            .flatten()
            .filter(|e| e.realm_id == realm && perm::matches(&e.permission, key))
        {
            if self.grantee_matches(&e.grantee, principal, eff) {
                match e.effect {
                    Effect::Deny => return perm::Grant::Denied,
                    Effect::Allow => granted = true,
                }
            }
        }
        if granted {
            perm::Grant::Granted
        } else {
            perm::Grant::None
        }
    }

    /// Record ids of `collection` with an ACL entry for `key` naming this
    /// caller: (allowed, denied).
    pub fn record_acl(
        &self,
        realm: &str,
        collection: &str,
        key: &str,
        principal: &str,
        eff: &Effective,
    ) -> (Vec<String>, Vec<String>) {
        let prefix = format!("{}/", collection.to_ascii_lowercase());
        let (mut allow, mut deny) = (Vec::new(), Vec::new());
        for ((t, id), entries) in &self.acl {
            let Some(rec) = id.strip_prefix(&prefix).filter(|_| t == "record") else {
                continue;
            };
            for e in entries {
                if e.realm_id == realm
                    && perm::matches(&e.permission, key)
                    && self.grantee_matches(&e.grantee, principal, eff)
                {
                    // Ids are stored as given; keep the original case.
                    let original = e.resource_id[prefix.len()..].to_owned();
                    debug_assert_eq!(original.to_ascii_lowercase(), rec);
                    match e.effect {
                        Effect::Allow => allow.push(original),
                        Effect::Deny => deny.push(original),
                    }
                }
            }
        }
        (allow, deny)
    }

    /// The project and owning scope of a tenant (environment).
    pub fn tenant(&self, env: &str) -> Option<(&Environment, &Project)> {
        let e = self.envs.get(env)?;
        Some((e, self.projects.get(&e.project_id)?))
    }

    /// The realm of a billing subject (FB-136): `org:<id>` → the
    /// organization's realm (`None` when unknown); `user:<collection>/<id>`
    /// and `service:<id>` → the identity's home realm; anything else `None`.
    pub fn realm_of_subject(&self, subject: &str) -> Option<String> {
        if let Some(org) = subject.strip_prefix("org:") {
            return self.orgs.get(org).map(|o| o.realm_id.clone());
        }
        let dir = self.realm_dir.as_ref();
        if let Some(rest) = subject.strip_prefix("user:") {
            let (collection, id) = rest.split_once('/')?;
            if collection.is_empty() || id.is_empty() {
                return None;
            }
            return Some(dir.map_or_else(
                || forgebase_data::DEFAULT_REALM.to_owned(),
                |d| d.realm_of_collection(collection),
            ));
        }
        if let Some(id) = subject.strip_prefix("service:").filter(|id| !id.is_empty()) {
            return Some(dir.map_or_else(
                || forgebase_data::DEFAULT_REALM.to_owned(),
                |d| d.realm_of_service_account(id),
            ));
        }
        None
    }
}

/// Field rules as stored (`{"salary": {"read": "...", "write": "..."}}`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldRuleText {
    /// Read rule (`None`: unrestricted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<String>,
    /// Write rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<String>,
}
