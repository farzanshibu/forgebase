//! Changes to the authorization model (ADR-0008 §9). Every operation runs the
//! pipeline with its own permission first, writes in one transaction, then
//! reloads the snapshot (invalidating every cached permission).
//!
//! Escalation guard: nobody but a platform super-admin can grant (through a
//! membership, team, role definition, override, invitation or ACL entry) a
//! permission they do not hold themselves.

use std::collections::HashMap;

use forgebase_data::{AccessContext, GetOptions, Principal};
use forgebase_db::{Statement, TxMode, Value};
use forgebase_schema::generate_id;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::engine::{Check, home_realm, step_authentication};
use crate::perm::{self, PermissionSet};
use crate::pipeline::{Denial, ResourceRef, Step};
use crate::snapshot::{Effect, FieldRuleText, Snapshot, rule_spec};
use crate::{PolicyError, PolicyService, now};

/// Default environments of a new project.
pub const DEFAULT_ENVIRONMENTS: [&str; 3] = ["development", "staging", "production"];

/// Lifetime of an invitation.
pub const INVITATION_TTL_SECS: i64 = 7 * 24 * 3600;

fn check_slug(s: &str, what: &str) -> Result<(), PolicyError> {
    let ok = !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(PolicyError::invalid(
            "invalid_key",
            format!("{what} must be 1-64 of [a-z0-9_-]"),
        ))
    }
}

fn check_name(s: &str) -> Result<(), PolicyError> {
    if s.trim().is_empty() || s.len() > 100 {
        return Err(PolicyError::invalid("invalid_name", "names are 1-100 characters"));
    }
    Ok(())
}

fn check_patterns(list: &[String]) -> Result<(), PolicyError> {
    if list.len() > 200 {
        return Err(PolicyError::invalid("invalid_permission", "at most 200 patterns"));
    }
    list.iter()
        .try_for_each(|p| perm::validate(p).map_err(|m| PolicyError::invalid("invalid_permission", m)))
}

/// A principal key as accepted from clients.
pub fn check_principal(p: &str) -> Result<(), PolicyError> {
    let ok = match p.split_once(':') {
        Some(("user", rest)) => rest
            .split_once('/')
            .is_some_and(|(c, id)| !c.is_empty() && !id.is_empty()),
        Some(("service", id)) => !id.is_empty(),
        _ => false,
    };
    if ok && p.len() <= 200 {
        Ok(())
    } else {
        Err(PolicyError::invalid(
            "invalid_principal",
            "principals are user:<collection>/<id> or service:<id>",
        ))
    }
}

fn json_list(v: &[String]) -> Value {
    Value::Text(Json::from(v.to_vec()).to_string())
}

/// Whether a caller's permission set covers a pattern they want to grant.
fn covers(caller: &PermissionSet, pattern: &str) -> bool {
    caller.allow.iter().any(|a| perm::matches(a, pattern)) && !caller.deny.iter().any(|d| perm::matches(d, pattern))
}

/// Input of role definitions.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RoleInput {
    /// Key.
    pub key: String,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Granted patterns.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Denied patterns.
    #[serde(default)]
    pub deny: Vec<String>,
}

/// Input of collection rules.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PolicyInput {
    /// List rule (`null` locked, `""` everyone).
    #[serde(default)]
    pub list_rule: Option<String>,
    /// View rule.
    #[serde(default)]
    pub view_rule: Option<String>,
    /// Create rule.
    #[serde(default)]
    pub create_rule: Option<String>,
    /// Update rule.
    #[serde(default)]
    pub update_rule: Option<String>,
    /// Delete rule.
    #[serde(default)]
    pub delete_rule: Option<String>,
    /// Field rules.
    #[serde(default)]
    pub fields: HashMap<String, FieldRuleText>,
}

impl PolicyService {
    async fn write(&self, stmts: Vec<Statement>) -> Result<(), PolicyError> {
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        for s in &stmts {
            if let Err(e) = tx.execute(s).await {
                let _ = tx.rollback().await;
                return Err(match e {
                    forgebase_db::DbError::Constraint {
                        kind: forgebase_db::ConstraintKind::Unique,
                        ..
                    } => PolicyError::Conflict("an object with this key".into()),
                    other => other.into(),
                });
            }
        }
        tx.commit().await?;
        self.reload().await
    }

    fn require_super_admin(&self, ctx: &AccessContext, what: &str) -> Result<(), PolicyError> {
        if ctx.is_privileged() {
            return Ok(());
        }
        let d = Denial::at(Step::Permission, format!("{what} needs a platform super-admin"));
        self.audit().record(crate::pipeline::audit_record(
            ctx,
            &format!("platform:{what}"),
            None,
            &d,
        ));
        Err(PolicyError::Denied(d))
    }

    fn org_check(&self, ctx: &AccessContext, org: &str, permission: &str) -> Result<Snapshot2, PolicyError> {
        let snap = self.snapshot();
        let realm = snap
            .orgs
            .get(org)
            .map(|o| o.realm_id.clone())
            .ok_or_else(|| PolicyError::NotFound(format!("organization {org}")))?;
        if !ctx.is_privileged() && realm != ctx.realm_id {
            return Err(PolicyError::NotFound(format!("organization {org}")));
        }
        self.check(
            ctx,
            &Check {
                permission: permission.into(),
                org: Some(org.into()),
                ..Check::default()
            },
        )?;
        Ok(Snapshot2 { snap, realm })
    }

    /// The caller may grant every pattern in `patterns` (escalation guard).
    fn guard_grant(
        &self,
        ctx: &AccessContext,
        snap: &Snapshot,
        org: Option<&str>,
        patterns: &[String],
    ) -> Result<(), PolicyError> {
        if ctx.is_privileged() {
            return Ok(());
        }
        let eff = snap.effective(&ctx.principal_key(), &ctx.realm_id, org);
        match patterns.iter().find(|p| !covers(&eff.perms, p)) {
            None => Ok(()),
            Some(p) => {
                let d = Denial::at(Step::Permission, format!("cannot grant {p}: you do not hold it"));
                self.audit().record(crate::pipeline::audit_record(ctx, p, None, &d));
                Err(PolicyError::Denied(d))
            }
        }
    }

    /// Allow patterns of role (and role-set) keys usable in `org`; unknown
    /// or non-assignable roles are refused.
    fn role_patterns(
        snap: &Snapshot,
        realm: &str,
        org: Option<&str>,
        roles: &[String],
        sets: &[String],
    ) -> Result<Vec<String>, PolicyError> {
        let mut out = Vec::new();
        let mut keys = roles.to_vec();
        for s in sets {
            let set = snap
                .role_sets
                .iter()
                .find(|r| r.realm_id == realm && (r.org_id.as_deref() == org || r.org_id.is_none()) && r.key == *s)
                .ok_or_else(|| PolicyError::invalid("unknown_role_set", format!("no role set {s}")))?;
            keys.extend(set.roles.iter().cloned());
        }
        for k in keys {
            let role = snap
                .role(realm, org, &k)
                .ok_or_else(|| PolicyError::invalid("unknown_role", format!("no role {k}")))?;
            if !role.assignable {
                return Err(PolicyError::invalid(
                    "role_not_assignable",
                    format!("{k} is a platform capability, not an organization role"),
                ));
            }
            out.extend(role.allow.iter().cloned());
        }
        Ok(out)
    }

    // ----- realms -------------------------------------------------------

    /// Creates a realm (platform super-admins).
    pub async fn create_realm(&self, ctx: &AccessContext, id: &str, name: &str) -> Result<Json, PolicyError> {
        self.require_super_admin(ctx, "realms:create")?;
        check_slug(id, "realm id")?;
        check_name(name)?;
        self.write(vec![Statement::new(
            "INSERT INTO _forgebase_realms (id, name, settings, created_at) VALUES (?1, ?2, '{}', ?3)",
            vec![id.into(), name.into(), now().into()],
        )])
        .await?;
        Ok(json!({"id": id, "name": name}))
    }

    /// All realms (platform super-admins).
    pub fn list_realms(&self, ctx: &AccessContext) -> Result<Json, PolicyError> {
        self.require_super_admin(ctx, "realms:read")?;
        let mut v: Vec<_> = self.snapshot().realms.values().cloned().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(json!(v))
    }

    // ----- organizations -------------------------------------------------

    /// Creates an organization in the caller's realm; the caller (a user)
    /// becomes its owner.
    pub async fn create_org(&self, ctx: &AccessContext, slug: &str, name: &str) -> Result<Json, PolicyError> {
        let snap = self.snapshot();
        if !ctx.is_privileged() {
            step_authentication(&snap, ctx, false).map_err(|d| self.deny(ctx, "orgs:create", d))?;
            if !matches!(ctx.principal, Principal::User { .. }) {
                return Err(self.deny(
                    ctx,
                    "orgs:create",
                    Denial::at(Step::Permission, "only users create organizations"),
                ));
            }
            if ctx.realm_id != home_realm(&snap, ctx) {
                return Err(self.deny(
                    ctx,
                    "orgs:create",
                    Denial::at(Step::Tenant, "cross-realm access is denied"),
                ));
            }
        }
        if !snap.realms.contains_key(&ctx.realm_id) {
            return Err(PolicyError::NotFound(format!("realm {}", ctx.realm_id)));
        }
        check_slug(slug, "slug")?;
        check_name(name)?;
        let id = generate_id();
        let mut stmts = vec![Statement::new(
            "INSERT INTO _forgebase_orgs (id, realm_id, slug, name, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            vec![
                id.clone().into(),
                ctx.realm_id.clone().into(),
                slug.into(),
                name.into(),
                ctx.actor().into(),
                now().into(),
            ],
        )];
        if matches!(ctx.principal, Principal::User { .. } | Principal::Service { .. }) {
            stmts.push(member_stmt(&id, &ctx.principal_key(), &["owner".to_owned()], &[]));
        }
        self.write(stmts).await?;
        Ok(json!({"id": id, "realm_id": ctx.realm_id, "slug": slug, "name": name}))
    }

    fn deny(&self, ctx: &AccessContext, perm: &str, d: Denial) -> PolicyError {
        self.audit().record(crate::pipeline::audit_record(ctx, perm, None, &d));
        PolicyError::Denied(d)
    }

    /// Organizations the caller is an active member of (all of the realm
    /// for super-admins).
    pub fn list_orgs(&self, ctx: &AccessContext) -> Result<Json, PolicyError> {
        let snap = self.snapshot();
        let key = ctx.principal_key();
        let mut v: Vec<_> = snap
            .orgs
            .values()
            .filter(|o| o.realm_id == ctx.realm_id)
            .filter(|o| ctx.is_privileged() || snap.membership(&o.id, &key).is_some_and(|m| m.status == "active"))
            .cloned()
            .collect();
        v.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(json!(v))
    }

    /// One organization (`org:read`).
    pub fn get_org(&self, ctx: &AccessContext, org: &str) -> Result<Json, PolicyError> {
        let s = self.org_check(ctx, org, "org:read")?;
        Ok(json!(s.snap.orgs.get(org)))
    }

    /// Renames an organization (`org:update`).
    pub async fn update_org(&self, ctx: &AccessContext, org: &str, name: &str) -> Result<Json, PolicyError> {
        self.org_check(ctx, org, "org:update")?;
        check_name(name)?;
        self.write(vec![Statement::new(
            "UPDATE _forgebase_orgs SET name = ?1 WHERE id = ?2",
            vec![name.into(), org.into()],
        )])
        .await?;
        self.get_org(ctx, org)
    }

    /// Deletes an organization with its projects, teams and memberships
    /// (`org:delete`; tenant data rows are not deleted).
    pub async fn delete_org(&self, ctx: &AccessContext, org: &str) -> Result<(), PolicyError> {
        self.org_check(ctx, org, "org:delete")?;
        self.write(vec![Statement::new(
            "DELETE FROM _forgebase_orgs WHERE id = ?1",
            vec![org.into()],
        )])
        .await
    }

    // ----- projects and environments --------------------------------------

    /// Creates a project in an organization (`projects:create`) or, without
    /// one, in the caller's personal scope, with the default environments.
    pub async fn create_project(
        &self,
        ctx: &AccessContext,
        org: Option<&str>,
        slug: &str,
        name: &str,
    ) -> Result<Json, PolicyError> {
        check_slug(slug, "slug")?;
        check_name(name)?;
        let (org_id, owner) = match org {
            Some(o) => {
                self.org_check(ctx, o, "projects:create")?;
                (Some(o.to_owned()), None)
            }
            None => {
                let snap = self.snapshot();
                step_authentication(&snap, ctx, false).map_err(|d| self.deny(ctx, "projects:create", d))?;
                if !matches!(ctx.principal, Principal::User { .. }) {
                    return Err(PolicyError::invalid("org_required", "only users have a personal scope"));
                }
                (None, Some(ctx.principal_key()))
            }
        };
        let id = generate_id();
        let mut stmts = vec![Statement::new(
            "INSERT INTO _forgebase_projects (id, realm_id, org_id, owner_user, slug, name, settings, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '{}', ?7)",
            vec![
                id.clone().into(),
                ctx.realm_id.clone().into(),
                org_id.clone().into(),
                owner.clone().into(),
                slug.into(),
                name.into(),
                now().into(),
            ],
        )];
        let mut envs = Vec::new();
        for e in DEFAULT_ENVIRONMENTS {
            let eid = generate_id();
            stmts.push(env_stmt(&eid, &id, e));
            envs.push(json!({"id": eid, "name": e}));
        }
        self.write(stmts).await?;
        Ok(json!({"id": id, "org_id": org_id, "owner_user": owner, "slug": slug, "name": name, "environments": envs}))
    }

    fn project_check(&self, ctx: &AccessContext, project: &str, permission: &str) -> Result<(), PolicyError> {
        let snap = self.snapshot();
        let p = snap
            .projects
            .get(project)
            .filter(|p| ctx.is_privileged() || p.realm_id == ctx.realm_id)
            .ok_or_else(|| PolicyError::NotFound(format!("project {project}")))?;
        self.check(
            ctx,
            &Check {
                permission: permission.into(),
                org: p.org_id.clone(),
                resource: Some(ResourceRef::new("project", project)),
                owner: p.owner_user.clone(),
                allow_anonymous: false,
            },
        )
        .map_err(|e| match e {
            // Callers without any access cannot tell a project exists.
            PolicyError::Denied(d) if d.step == Step::Tenant => PolicyError::NotFound(format!("project {project}")),
            other => other,
        })
    }

    /// One project with its environments (`projects:read`).
    pub fn get_project(&self, ctx: &AccessContext, project: &str) -> Result<Json, PolicyError> {
        self.project_check(ctx, project, "projects:read")?;
        let snap = self.snapshot();
        let mut envs: Vec<_> = snap
            .envs
            .values()
            .filter(|e| e.project_id == project)
            .cloned()
            .collect();
        envs.sort_by(|a, b| a.name.cmp(&b.name));
        let mut v = json!(snap.projects.get(project));
        v["environments"] = json!(envs);
        Ok(v)
    }

    /// Projects of an organization (`projects:read`) or the caller's
    /// personal projects.
    pub fn list_projects(&self, ctx: &AccessContext, org: Option<&str>) -> Result<Json, PolicyError> {
        let snap = match org {
            Some(o) => self.org_check(ctx, o, "projects:read")?.snap,
            None => self.snapshot(),
        };
        let me = ctx.principal_key();
        let mut v: Vec<_> = snap
            .projects
            .values()
            .filter(|p| match org {
                Some(o) => p.org_id.as_deref() == Some(o),
                None => p.owner_user.as_deref() == Some(me.as_str()),
            })
            .cloned()
            .collect();
        v.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(json!(v))
    }

    /// Deletes a project and its environments (`projects:delete`).
    pub async fn delete_project(&self, ctx: &AccessContext, project: &str) -> Result<(), PolicyError> {
        self.project_check(ctx, project, "projects:delete")?;
        self.write(vec![Statement::new(
            "DELETE FROM _forgebase_projects WHERE id = ?1",
            vec![project.into()],
        )])
        .await
    }

    /// Adds an environment (`projects:update`).
    pub async fn create_environment(
        &self,
        ctx: &AccessContext,
        project: &str,
        name: &str,
    ) -> Result<Json, PolicyError> {
        self.project_check(ctx, project, "projects:update")?;
        check_slug(name, "environment name")?;
        let id = generate_id();
        self.write(vec![env_stmt(&id, project, name)]).await?;
        Ok(json!({"id": id, "project_id": project, "name": name}))
    }

    // ----- memberships ------------------------------------------------------

    /// Adds or changes a member (`users:update`); roles must be assignable and
    /// within the caller's own permissions.
    pub async fn set_member(
        &self,
        ctx: &AccessContext,
        org: &str,
        principal: &str,
        roles: &[String],
        role_sets: &[String],
    ) -> Result<Json, PolicyError> {
        check_principal(principal)?;
        let s = self.org_check(ctx, org, "users:update")?;
        let patterns = Self::role_patterns(&s.snap, &s.realm, Some(org), roles, role_sets)?;
        self.guard_grant(ctx, &s.snap, Some(org), &patterns)?;
        self.write(vec![
            Statement::new(
                "DELETE FROM _forgebase_memberships WHERE org_id = ?1 AND principal = ?2",
                vec![org.into(), principal.into()],
            ),
            member_stmt(org, principal, roles, role_sets),
        ])
        .await?;
        Ok(json!({"org_id": org, "principal": principal, "roles": roles, "role_sets": role_sets, "status": "active"}))
    }

    /// Suspends or reactivates a membership (`users:update`).
    pub async fn set_member_status(
        &self,
        ctx: &AccessContext,
        org: &str,
        principal: &str,
        active: bool,
    ) -> Result<(), PolicyError> {
        self.org_check(ctx, org, "users:update")?;
        self.write(vec![Statement::new(
            "UPDATE _forgebase_memberships SET status = ?1 WHERE org_id = ?2 AND principal = ?3",
            vec![
                if active { "active" } else { "suspended" }.into(),
                org.into(),
                principal.into(),
            ],
        )])
        .await
    }

    /// Removes a member and their team memberships (`users:delete`).
    pub async fn remove_member(&self, ctx: &AccessContext, org: &str, principal: &str) -> Result<(), PolicyError> {
        self.org_check(ctx, org, "users:delete")?;
        self.write(vec![
            Statement::new(
                "DELETE FROM _forgebase_memberships WHERE org_id = ?1 AND principal = ?2",
                vec![org.into(), principal.into()],
            ),
            Statement::new(
                "DELETE FROM _forgebase_team_members WHERE principal = ?2 AND team_id IN (SELECT id FROM _forgebase_teams WHERE org_id = ?1)",
                vec![org.into(), principal.into()],
            ),
        ])
        .await
    }

    /// Members of an organization (`users:read`).
    pub fn list_members(&self, ctx: &AccessContext, org: &str) -> Result<Json, PolicyError> {
        let s = self.org_check(ctx, org, "users:read")?;
        let mut v: Vec<_> = s
            .snap
            .memberships
            .values()
            .filter(|m| m.scope_id == org)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.principal.cmp(&b.principal));
        Ok(json!(v))
    }

    // ----- teams --------------------------------------------------------------

    /// Creates a team (`teams:create`), optionally under a parent team.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_team(
        &self,
        ctx: &AccessContext,
        org: &str,
        key: &str,
        name: &str,
        parent: Option<&str>,
        roles: &[String],
        deny: &[String],
    ) -> Result<Json, PolicyError> {
        let s = self.org_check(ctx, org, "teams:create")?;
        check_slug(key, "team key")?;
        check_name(name)?;
        check_patterns(deny)?;
        if let Some(p) = parent
            && s.snap.teams.get(p).is_none_or(|t| t.org_id != org)
        {
            return Err(PolicyError::invalid(
                "invalid_parent",
                "the parent team is not in this organization",
            ));
        }
        let patterns = Self::role_patterns(&s.snap, &s.realm, Some(org), roles, &[])?;
        self.guard_grant(ctx, &s.snap, Some(org), &patterns)?;
        let id = generate_id();
        self.write(vec![Statement::new(
            "INSERT INTO _forgebase_teams (id, org_id, parent_id, key, name, roles, deny, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            vec![
                id.clone().into(),
                org.into(),
                parent.into(),
                key.into(),
                name.into(),
                json_list(roles),
                json_list(deny),
                now().into(),
            ],
        )])
        .await?;
        Ok(
            json!({"id": id, "org_id": org, "parent_id": parent, "key": key, "name": name, "roles": roles, "deny": deny}),
        )
    }

    fn team_org(&self, team: &str) -> Result<String, PolicyError> {
        self.snapshot()
            .teams
            .get(team)
            .map(|t| t.org_id.clone())
            .ok_or_else(|| PolicyError::NotFound(format!("team {team}")))
    }

    /// Deletes a team and its sub-teams (`teams:delete`).
    pub async fn delete_team(&self, ctx: &AccessContext, team: &str) -> Result<(), PolicyError> {
        let org = self.team_org(team)?;
        self.org_check(ctx, &org, "teams:delete")?;
        self.write(vec![Statement::new(
            "DELETE FROM _forgebase_teams WHERE id = ?1",
            vec![team.into()],
        )])
        .await
    }

    /// Adds a member of the organization to a team (`teams:members:manage`).
    pub async fn add_team_member(&self, ctx: &AccessContext, team: &str, principal: &str) -> Result<(), PolicyError> {
        let org = self.team_org(team)?;
        let s = self.org_check(ctx, &org, "teams:members:manage")?;
        if s.snap.membership(&org, principal).is_none() {
            return Err(PolicyError::invalid(
                "not_a_member",
                "add the principal to the organization first",
            ));
        }
        let mut patterns = Vec::new();
        for t in s.snap.team_chain(team) {
            patterns.extend(Self::role_patterns(&s.snap, &s.realm, Some(&org), &t.roles, &[])?);
        }
        self.guard_grant(ctx, &s.snap, Some(&org), &patterns)?;
        self.write(vec![Statement::new(
            "INSERT OR IGNORE INTO _forgebase_team_members (team_id, principal, created_at) VALUES (?1, ?2, ?3)",
            vec![team.into(), principal.into(), now().into()],
        )])
        .await
    }

    /// Removes a team member (`teams:members:manage`).
    pub async fn remove_team_member(
        &self,
        ctx: &AccessContext,
        team: &str,
        principal: &str,
    ) -> Result<(), PolicyError> {
        let org = self.team_org(team)?;
        self.org_check(ctx, &org, "teams:members:manage")?;
        self.write(vec![Statement::new(
            "DELETE FROM _forgebase_team_members WHERE team_id = ?1 AND principal = ?2",
            vec![team.into(), principal.into()],
        )])
        .await
    }

    /// Teams of an organization (`teams:read`).
    pub fn list_teams(&self, ctx: &AccessContext, org: &str) -> Result<Json, PolicyError> {
        let s = self.org_check(ctx, org, "teams:read")?;
        let mut v: Vec<_> = s.snap.teams.values().filter(|t| t.org_id == org).cloned().collect();
        v.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(json!(v))
    }

    // ----- roles, role sets, bindings, overrides ------------------------------

    /// Creates or replaces a role: at org scope (`roles:manage`, within the
    /// caller's own permissions) or, with `org = None`, at realm scope
    /// (super-admins). Seeded roles can be edited only by super-admins.
    pub async fn put_role(
        &self,
        ctx: &AccessContext,
        org: Option<&str>,
        input: &RoleInput,
    ) -> Result<Json, PolicyError> {
        check_slug(&input.key, "role key")?;
        check_patterns(&input.allow)?;
        check_patterns(&input.deny)?;
        let (snap, realm) = match org {
            Some(o) => {
                let s = self.org_check(ctx, o, "roles:manage")?;
                self.guard_grant(ctx, &s.snap, Some(o), &input.allow)?;
                (s.snap, s.realm)
            }
            None => {
                self.require_super_admin(ctx, "roles:manage")?;
                (self.snapshot(), ctx.realm_id.clone())
            }
        };
        let existing = snap
            .roles
            .iter()
            .find(|r| r.realm_id == realm && r.org_id.as_deref() == org && r.key == input.key)
            .cloned();
        if existing.as_ref().is_some_and(|r| r.system) && !ctx.is_privileged() {
            return Err(PolicyError::invalid(
                "system_role",
                "seeded roles are changed by super-admins only",
            ));
        }
        if existing.as_ref().is_some_and(|r| !r.assignable) {
            return Err(PolicyError::invalid(
                "system_role",
                "the super-admin role cannot be edited",
            ));
        }
        let id = existing.as_ref().map_or_else(generate_id, |r| r.id.clone());
        let name = input.name.clone().unwrap_or_else(|| input.key.clone());
        self.write(vec![
            Statement::new("DELETE FROM _forgebase_roles WHERE id = ?1", vec![id.clone().into()]),
            Statement::new(
                "INSERT INTO _forgebase_roles (id, realm_id, org_id, key, name, description, allow, deny, system, assignable, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10)",
                vec![
                    id.clone().into(),
                    realm.into(),
                    org.into(),
                    input.key.clone().into(),
                    name.into(),
                    input.description.clone().into(),
                    json_list(&input.allow),
                    json_list(&input.deny),
                    existing.as_ref().is_some_and(|r| r.system).into(),
                    now().into(),
                ],
            ),
        ])
        .await?;
        Ok(json!(self.snapshot().roles.iter().find(|r| r.id == id)))
    }

    /// Deletes a custom role.
    pub async fn delete_role(&self, ctx: &AccessContext, org: Option<&str>, key: &str) -> Result<(), PolicyError> {
        match org {
            Some(o) => drop(self.org_check(ctx, o, "roles:manage")?),
            None => self.require_super_admin(ctx, "roles:manage")?,
        }
        let snap = self.snapshot();
        let role = snap
            .roles
            .iter()
            .find(|r| r.realm_id == ctx.realm_id && r.org_id.as_deref() == org && r.key == key)
            .ok_or_else(|| PolicyError::NotFound(format!("role {key}")))?;
        if role.system {
            return Err(PolicyError::invalid("system_role", "seeded roles cannot be deleted"));
        }
        self.write(vec![Statement::new(
            "DELETE FROM _forgebase_roles WHERE id = ?1",
            vec![role.id.clone().into()],
        )])
        .await
    }

    /// Roles usable in an organization (`roles:read`) or the realm's roles.
    pub fn list_roles(&self, ctx: &AccessContext, org: Option<&str>) -> Result<Json, PolicyError> {
        let snap = match org {
            Some(o) => self.org_check(ctx, o, "roles:read")?.snap,
            None => self.snapshot(),
        };
        let v: Vec<_> = snap
            .roles
            .iter()
            .filter(|r| r.realm_id == ctx.realm_id && (r.org_id.is_none() || r.org_id.as_deref() == org))
            .cloned()
            .collect();
        Ok(json!(v))
    }

    /// Creates or replaces a role set (org: `roles:manage`; realm: super-admin).
    pub async fn put_role_set(
        &self,
        ctx: &AccessContext,
        org: Option<&str>,
        key: &str,
        name: &str,
        roles: &[String],
    ) -> Result<Json, PolicyError> {
        check_slug(key, "role set key")?;
        let (snap, realm) = match org {
            Some(o) => {
                let s = self.org_check(ctx, o, "roles:manage")?;
                (s.snap, s.realm)
            }
            None => {
                self.require_super_admin(ctx, "roles:manage")?;
                (self.snapshot(), ctx.realm_id.clone())
            }
        };
        let patterns = Self::role_patterns(&snap, &realm, org, roles, &[])?;
        self.guard_grant(ctx, &snap, org, &patterns)?;
        let id = generate_id();
        self.write(vec![
            Statement::new(
                "DELETE FROM _forgebase_role_sets WHERE realm_id = ?1 AND coalesce(org_id, '') = coalesce(?2, '') AND key = ?3",
                vec![realm.clone().into(), org.into(), key.into()],
            ),
            Statement::new(
                "INSERT INTO _forgebase_role_sets (id, realm_id, org_id, key, name, roles, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                vec![id.clone().into(), realm.into(), org.into(), key.into(), name.into(), json_list(roles), now().into()],
            ),
        ])
        .await?;
        Ok(json!({"id": id, "org_id": org, "key": key, "name": name, "roles": roles}))
    }

    /// Binds realm-level roles to a principal (super-admins).
    pub async fn put_realm_binding(
        &self,
        ctx: &AccessContext,
        principal: &str,
        roles: &[String],
        role_sets: &[String],
    ) -> Result<Json, PolicyError> {
        self.require_super_admin(ctx, "realm_bindings:manage")?;
        check_principal(principal)?;
        Self::role_patterns(&self.snapshot(), &ctx.realm_id, None, roles, role_sets)?;
        self.write(vec![
            Statement::new(
                "DELETE FROM _forgebase_realm_bindings WHERE realm_id = ?1 AND principal = ?2",
                vec![ctx.realm_id.clone().into(), principal.into()],
            ),
            Statement::new(
                "INSERT INTO _forgebase_realm_bindings (id, realm_id, principal, roles, role_sets, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                vec![
                    generate_id().into(),
                    ctx.realm_id.clone().into(),
                    principal.into(),
                    json_list(roles),
                    json_list(role_sets),
                    now().into(),
                ],
            ),
        ])
        .await?;
        Ok(json!({"realm_id": ctx.realm_id, "principal": principal, "roles": roles, "role_sets": role_sets}))
    }

    /// Adds an explicit allow or deny for one principal: org-wide
    /// (`roles:manage`; allows within the caller's permissions) or
    /// realm-wide (super-admins).
    pub async fn add_override(
        &self,
        ctx: &AccessContext,
        org: Option<&str>,
        principal: &str,
        permission: &str,
        effect: Effect,
    ) -> Result<Json, PolicyError> {
        check_principal(principal)?;
        check_patterns(&[permission.to_owned()])?;
        match org {
            Some(o) => {
                let s = self.org_check(ctx, o, "roles:manage")?;
                if effect == Effect::Allow {
                    self.guard_grant(ctx, &s.snap, Some(o), &[permission.to_owned()])?;
                }
            }
            None => self.require_super_admin(ctx, "overrides:manage")?,
        }
        let id = generate_id();
        self.write(vec![Statement::new(
            "INSERT INTO _forgebase_permission_overrides (id, realm_id, org_id, principal, permission, effect, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            vec![
                id.clone().into(),
                ctx.realm_id.clone().into(),
                org.into(),
                principal.into(),
                permission.into(),
                effect.as_str().into(),
                now().into(),
            ],
        )])
        .await?;
        Ok(json!({"id": id, "org_id": org, "principal": principal, "permission": permission, "effect": effect}))
    }

    /// Removes an override (same permission as adding it).
    pub async fn delete_override(&self, ctx: &AccessContext, id: &str) -> Result<(), PolicyError> {
        let snap = self.snapshot();
        let ov = snap
            .overrides
            .iter()
            .find(|o| o.id == id && o.realm_id == ctx.realm_id)
            .ok_or_else(|| PolicyError::NotFound(format!("override {id}")))?;
        match &ov.org_id {
            Some(o) => drop(self.org_check(ctx, o, "roles:manage")?),
            None => self.require_super_admin(ctx, "overrides:manage")?,
        }
        self.write(vec![Statement::new(
            "DELETE FROM _forgebase_permission_overrides WHERE id = ?1",
            vec![id.into()],
        )])
        .await
    }

    // ----- resource ACL ------------------------------------------------------------

    /// The organization (or personal owner) a resource belongs to, for ACL
    /// management.
    fn resource_scope(
        &self,
        ctx: &AccessContext,
        rtype: &str,
        rid: &str,
    ) -> Result<(Option<String>, Option<String>), PolicyError> {
        let snap = self.snapshot();
        let project = match rtype {
            "project" => snap.projects.get(rid),
            "environment" => snap.tenant(rid).map(|(_, p)| p),
            _ => None,
        };
        Ok(match project {
            Some(p) if p.realm_id == ctx.realm_id || ctx.is_privileged() => (p.org_id.clone(), p.owner_user.clone()),
            Some(_) => return Err(PolicyError::NotFound(format!("{rtype} {rid}"))),
            None if matches!(rtype, "project" | "environment") => {
                return Err(PolicyError::NotFound(format!("{rtype} {rid}")));
            }
            None => (ctx.org_id.clone(), None),
        })
    }

    /// Grants or denies `permission` on one resource to a grantee
    /// (`acl:manage` in the owning organization; personal owners for their
    /// own projects). Allow entries need the caller to hold the permission.
    pub async fn put_acl(
        &self,
        ctx: &AccessContext,
        rtype: &str,
        rid: &str,
        grantee: &str,
        permission: &str,
        effect: Effect,
    ) -> Result<Json, PolicyError> {
        check_slug(rtype, "resource type")?;
        if rid.is_empty() || rid.len() > 200 {
            return Err(PolicyError::invalid(
                "invalid_resource",
                "resource ids are 1-200 characters",
            ));
        }
        if !(grantee.starts_with("team:") || grantee.starts_with("role:")) {
            check_principal(grantee)?;
        }
        check_patterns(&[permission.to_owned()])?;
        let (org, owner) = self.resource_scope(ctx, rtype, rid)?;
        if !ctx.is_privileged() {
            let check = Check {
                permission: "acl:manage".into(),
                org: org.clone(),
                resource: None,
                owner: owner.clone(),
                allow_anonymous: false,
            };
            if org.is_none() && owner.is_none() {
                return Err(self.deny(
                    ctx,
                    "acl:manage",
                    Denial::at(Step::Tenant, "select an organization (X-ForgeBase-Org)"),
                ));
            }
            self.check(ctx, &check)?;
            if effect == Effect::Allow && owner.as_deref() != Some(ctx.principal_key().as_str()) {
                self.guard_grant(ctx, &self.snapshot(), org.as_deref(), &[permission.to_owned()])?;
            }
        }
        let id = generate_id();
        self.write(vec![Statement::new(
            "INSERT INTO _forgebase_resource_permissions (id, realm_id, resource_type, resource_id, grantee, permission, effect, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            vec![
                id.clone().into(),
                ctx.realm_id.clone().into(),
                rtype.into(),
                rid.into(),
                grantee.into(),
                permission.into(),
                effect.as_str().into(),
                ctx.actor().into(),
                now().into(),
            ],
        )])
        .await?;
        Ok(
            json!({"id": id, "resource_type": rtype, "resource_id": rid, "grantee": grantee, "permission": permission, "effect": effect}),
        )
    }

    /// Removes an ACL entry (same permission as adding it).
    pub async fn delete_acl(&self, ctx: &AccessContext, id: &str) -> Result<(), PolicyError> {
        let snap = self.snapshot();
        let e = snap
            .acl
            .values()
            .flatten()
            .find(|e| e.id == id && e.realm_id == ctx.realm_id)
            .cloned()
            .ok_or_else(|| PolicyError::NotFound(format!("ACL entry {id}")))?;
        if !ctx.is_privileged() {
            let (org, owner) = self.resource_scope(ctx, &e.resource_type, &e.resource_id)?;
            self.check(
                ctx,
                &Check {
                    permission: "acl:manage".into(),
                    org,
                    owner,
                    ..Check::default()
                },
            )?;
        }
        self.write(vec![Statement::new(
            "DELETE FROM _forgebase_resource_permissions WHERE id = ?1",
            vec![id.into()],
        )])
        .await
    }

    /// ACL entries of one resource (`acl:manage`).
    pub fn list_acl(&self, ctx: &AccessContext, rtype: &str, rid: &str) -> Result<Json, PolicyError> {
        let (org, owner) = self.resource_scope(ctx, rtype, rid)?;
        if !ctx.is_privileged() {
            self.check(
                ctx,
                &Check {
                    permission: "acl:manage".into(),
                    org,
                    owner,
                    ..Check::default()
                },
            )?;
        }
        let snap = self.snapshot();
        let v: Vec<_> = snap
            .acl
            .get(&(rtype.to_owned(), rid.to_ascii_lowercase()))
            .into_iter()
            .flatten()
            .filter(|e| e.realm_id == ctx.realm_id)
            .cloned()
            .collect();
        Ok(json!(v))
    }

    // ----- suspension ----------------------------------------------------------------

    /// Suspends an account everywhere (super-admins). `until`: Unix seconds.
    pub async fn suspend(
        &self,
        ctx: &AccessContext,
        principal: &str,
        reason: &str,
        until: Option<i64>,
    ) -> Result<(), PolicyError> {
        self.require_super_admin(ctx, "suspensions:manage")?;
        check_principal(principal)?;
        self.write(vec![Statement::new(
            "INSERT OR REPLACE INTO _forgebase_suspensions (principal, reason, suspended_by, suspended_at, until_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            vec![principal.into(), reason.into(), ctx.actor().into(), now().into(), until.into()],
        )])
        .await
    }

    /// Lifts a suspension (super-admins).
    pub async fn unsuspend(&self, ctx: &AccessContext, principal: &str) -> Result<(), PolicyError> {
        self.require_super_admin(ctx, "suspensions:manage")?;
        self.write(vec![Statement::new(
            "DELETE FROM _forgebase_suspensions WHERE principal = ?1",
            vec![principal.into()],
        )])
        .await
    }

    // ----- collection rules --------------------------------------------------------

    /// Sets the rules of a collection (super-admins). Every rule is parsed and
    /// compiled against the collection's schema; field rules must not read
    /// records.
    pub async fn put_policy(
        &self,
        ctx: &AccessContext,
        collection: &str,
        input: &PolicyInput,
    ) -> Result<Json, PolicyError> {
        self.require_super_admin(ctx, "policies:manage")?;
        let data = self.data()?;
        let coll = data.get_collection(&AccessContext::system(), collection).await?;
        let rules = [
            &input.list_rule,
            &input.view_rule,
            &input.create_rule,
            &input.update_rule,
            &input.delete_rule,
        ];
        for r in rules.iter().copied().flatten() {
            if let Some(crate::snapshot::RuleSpec::Expr(rule)) = rule_spec(Some(r))? {
                data.validate_predicate(&coll, &rule.sample(&Json::String("x".into())).into_predicate())
                    .map_err(|e| PolicyError::invalid("invalid_rule", e.to_string()))?;
            }
        }
        for (name, f) in &input.fields {
            if coll.field(name).is_none() {
                return Err(PolicyError::invalid(
                    "unknown_field",
                    format!("{} has no field {name}", coll.name),
                ));
            }
            for r in [&f.read, &f.write].into_iter().flatten() {
                if let Some(crate::snapshot::RuleSpec::Expr(rule)) = rule_spec(Some(r))?
                    && rule.reads_records()
                {
                    return Err(PolicyError::invalid(
                        "invalid_rule",
                        format!("field rule of {name} may use only @request values"),
                    ));
                }
            }
        }
        self.write(vec![
            Statement::new(
                "DELETE FROM _forgebase_collection_policies WHERE collection_id = ?1",
                vec![coll.id.clone().into()],
            ),
            Statement::new(
                "INSERT INTO _forgebase_collection_policies (collection_id, list_rule, view_rule, create_rule, update_rule, delete_rule, fields, updated_by, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                vec![
                    coll.id.clone().into(),
                    input.list_rule.clone().into(),
                    input.view_rule.clone().into(),
                    input.create_rule.clone().into(),
                    input.update_rule.clone().into(),
                    input.delete_rule.clone().into(),
                    serde_json::to_string(&input.fields)?.into(),
                    ctx.actor().into(),
                    now().into(),
                ],
            ),
        ])
        .await?;
        self.get_policy(ctx, collection).await
    }

    /// The rules of a collection (super-admins).
    pub async fn get_policy(&self, ctx: &AccessContext, collection: &str) -> Result<Json, PolicyError> {
        self.require_super_admin(ctx, "policies:read")?;
        let coll = self
            .data()?
            .get_collection(&AccessContext::system(), collection)
            .await?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT list_rule, view_rule, create_rule, update_rule, delete_rule, fields FROM _forgebase_collection_policies WHERE collection_id = ?1",
                vec![coll.id.clone().into()],
            ))
            .await?;
        let r = rows.rows.first();
        let col = |i: usize| r.and_then(|r| r[i].as_str()).map(str::to_owned);
        let fields: Json = col(5)
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| json!({}));
        Ok(json!({
            "collection": coll.name, "list_rule": col(0), "view_rule": col(1), "create_rule": col(2),
            "update_rule": col(3), "delete_rule": col(4), "fields": fields,
        }))
    }

    /// Removes the rules of a collection (back to locked; super-admins).
    pub async fn delete_policy(&self, ctx: &AccessContext, collection: &str) -> Result<(), PolicyError> {
        self.require_super_admin(ctx, "policies:manage")?;
        let coll = self
            .data()?
            .get_collection(&AccessContext::system(), collection)
            .await?;
        self.write(vec![Statement::new(
            "DELETE FROM _forgebase_collection_policies WHERE collection_id = ?1",
            vec![coll.id.into()],
        )])
        .await
    }

    // ----- invitations ------------------------------------------------------------------

    /// Invites an email address to an organization (`users:invite`; roles
    /// within the caller's permissions). Returns the one-time token, which is
    /// also mailed when a mailer is attached; only its SHA-256 is stored.
    pub async fn invite(
        &self,
        ctx: &AccessContext,
        org: &str,
        email: &str,
        roles: &[String],
    ) -> Result<(Json, String), PolicyError> {
        let s = self.org_check(ctx, org, "users:invite")?;
        let email = email.trim().to_ascii_lowercase();
        if !(3..=254).contains(&email.len()) || !email.contains('@') {
            return Err(PolicyError::invalid(
                "invalid_email",
                "a valid email address is required",
            ));
        }
        let patterns = Self::role_patterns(&s.snap, &s.realm, Some(org), roles, &[])?;
        self.guard_grant(ctx, &s.snap, Some(org), &patterns)?;
        let token = forgebase_auth::crypto::random_token(32).map_err(|e| PolicyError::Internal(e.to_string()))?;
        let id = generate_id();
        let expires = now() + INVITATION_TTL_SECS;
        self.write(vec![Statement::new(
            "INSERT INTO _forgebase_invitations (id, org_id, email, roles, token_hash, invited_by, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            vec![
                id.clone().into(),
                org.into(),
                email.clone().into(),
                json_list(roles),
                token_hash(&token).into(),
                ctx.actor().into(),
                now().into(),
                expires.into(),
            ],
        )])
        .await?;
        Ok((
            json!({"id": id, "org_id": org, "email": email, "roles": roles, "expires_at": expires}),
            token,
        ))
    }

    /// Accepts an invitation: the caller must be a user whose verified-or-not
    /// auth record email equals the invited address. Single use.
    pub async fn accept_invitation(&self, ctx: &AccessContext, token: &str) -> Result<Json, PolicyError> {
        let Principal::User { collection, id } = &ctx.principal else {
            return Err(self.deny(
                ctx,
                "invitations:accept",
                Denial::at(Step::Authentication, "authentication required"),
            ));
        };
        step_authentication(&self.snapshot(), ctx, false).map_err(|d| self.deny(ctx, "invitations:accept", d))?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT id, org_id, email, roles FROM _forgebase_invitations
                 WHERE token_hash = ?1 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > ?2",
                vec![token_hash(token).into(), now().into()],
            ))
            .await?;
        let r = rows
            .rows
            .first()
            .ok_or_else(|| PolicyError::NotFound("invitation".into()))?;
        let (inv, org, email) = (
            r[0].as_str().unwrap_or_default().to_owned(),
            r[1].as_str().unwrap_or_default().to_owned(),
            r[2].as_str().unwrap_or_default().to_owned(),
        );
        let roles: Vec<String> = r[3]
            .as_str()
            .and_then(|t| serde_json::from_str(t).ok())
            .unwrap_or_default();
        let me = self
            .data()?
            .get_record(&AccessContext::system(), collection, id, &GetOptions::default())
            .await?;
        if me.get("email").and_then(Json::as_str).map(str::to_ascii_lowercase) != Some(email.clone()) {
            return Err(PolicyError::NotFound("invitation".into()));
        }
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let claimed = tx
            .execute(&Statement::new(
                "UPDATE _forgebase_invitations SET accepted_at = ?1, accepted_by = ?2 WHERE id = ?3 AND accepted_at IS NULL",
                vec![now().into(), ctx.principal_key().into(), inv.clone().into()],
            ))
            .await?;
        if claimed.rows_affected != 1 {
            let _ = tx.rollback().await;
            return Err(PolicyError::NotFound("invitation".into()));
        }
        tx.execute(&Statement::new(
            "DELETE FROM _forgebase_memberships WHERE org_id = ?1 AND principal = ?2",
            vec![org.clone().into(), ctx.principal_key().into()],
        ))
        .await?;
        tx.execute(&member_stmt(&org, &ctx.principal_key(), &roles, &[]))
            .await?;
        tx.commit().await?;
        self.reload().await?;
        Ok(json!({"org_id": org, "principal": ctx.principal_key(), "roles": roles}))
    }

    // ----- introspection ------------------------------------------------------------------

    /// The caller's effective roles and permissions in an org (or realm level).
    pub fn my_permissions(&self, ctx: &AccessContext, org: Option<&str>) -> Result<Json, PolicyError> {
        let snap = self.snapshot();
        if let Some(o) = org
            && !ctx.is_privileged()
        {
            crate::engine::step_tenant(&snap, ctx, Some(o)).map_err(PolicyError::Denied)?;
        }
        let eff = snap.effective(&ctx.principal_key(), &ctx.realm_id, org);
        Ok(json!({
            "principal": ctx.principal_key(), "realm_id": ctx.realm_id, "org_id": org,
            "super_admin": ctx.is_privileged(),
            "suspended": snap.is_suspended(&ctx.principal_key(), now()),
            "roles": eff.roles, "allow": eff.perms.allow, "deny": eff.perms.deny, "teams": eff.teams,
        }))
    }

    // ----- read endpoints of the admin studio (ADR-0014 §3) -----------------------

    /// Role sets usable in the org (`roles:read`), or the realm's.
    pub fn list_role_sets(&self, ctx: &AccessContext, org: Option<&str>) -> Result<Json, PolicyError> {
        let snap = match org {
            Some(o) => self.org_check(ctx, o, "roles:read")?.snap,
            None => self.snapshot(),
        };
        let v: Vec<_> = snap
            .role_sets
            .iter()
            .filter(|r| r.realm_id == ctx.realm_id && (r.org_id.is_none() || r.org_id.as_deref() == org))
            .cloned()
            .collect();
        Ok(json!(v))
    }

    /// Realm role bindings (super-admins).
    pub fn list_realm_bindings(&self, ctx: &AccessContext) -> Result<Json, PolicyError> {
        self.require_super_admin(ctx, "realm_bindings:read")?;
        let mut v: Vec<_> = self
            .snapshot()
            .realm_bindings
            .values()
            .filter(|g| g.scope_id == ctx.realm_id)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.principal.cmp(&b.principal));
        Ok(json!(v))
    }

    /// Explicit allow/deny overrides of an org (`roles:read`) or of the
    /// realm (super-admins).
    pub fn list_overrides(&self, ctx: &AccessContext, org: Option<&str>) -> Result<Json, PolicyError> {
        let snap = match org {
            Some(o) => self.org_check(ctx, o, "roles:read")?.snap,
            None => {
                self.require_super_admin(ctx, "overrides:read")?;
                self.snapshot()
            }
        };
        let v: Vec<_> = snap
            .overrides
            .iter()
            .filter(|o| o.realm_id == ctx.realm_id && o.org_id.as_deref() == org)
            .cloned()
            .collect();
        Ok(json!(v))
    }

    /// Active account suspensions (super-admins).
    pub async fn list_suspensions(&self, ctx: &AccessContext) -> Result<Json, PolicyError> {
        self.require_super_admin(ctx, "suspensions:read")?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT principal, reason, suspended_by, suspended_at, until_at FROM _forgebase_suspensions
                 WHERE until_at IS NULL OR until_at > ?1 ORDER BY suspended_at DESC",
                vec![now().into()],
            ))
            .await?;
        Ok(Json::Array(
            rows.rows
                .iter()
                .map(|r| {
                    json!({
                        "principal": r[0].as_str(), "reason": r[1].as_str(), "suspended_by": r[2].as_str(),
                        "created_at": r[3].as_i64(), "until": r[4].as_i64(),
                    })
                })
                .collect(),
        ))
    }

    /// Invitations of an org, newest first (`users:read`); never the token.
    pub async fn list_invitations(&self, ctx: &AccessContext, org: &str) -> Result<Json, PolicyError> {
        self.org_check(ctx, org, "users:read")?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT id, email, roles, invited_by, created_at, expires_at, accepted_at, accepted_by
                 FROM _forgebase_invitations WHERE org_id = ?1 ORDER BY created_at DESC LIMIT 500",
                vec![org.into()],
            ))
            .await?;
        let t = now();
        Ok(Json::Array(
            rows.rows
                .iter()
                .map(|r| {
                    let accepted = r[6].as_i64();
                    let expires = r[5].as_i64().unwrap_or(0);
                    let status = if accepted.is_some() {
                        "accepted"
                    } else if expires <= t {
                        "expired"
                    } else {
                        "pending"
                    };
                    json!({
                        "id": r[0].as_str(), "org_id": org, "email": r[1].as_str(),
                        "roles": r[2].as_str().and_then(|s| serde_json::from_str::<Json>(s).ok()).unwrap_or(json!([])),
                        "invited_by": r[3].as_str(), "created_at": r[4].as_i64(), "expires_at": expires,
                        "accepted_at": accepted, "accepted_by": r[7].as_str(), "status": status,
                    })
                })
                .collect(),
        ))
    }

    /// The context of `principal` (`user:<collection>/<id>` or `service:<id>`)
    /// for dry runs by a super-admin ("would this user be allowed?").
    pub fn context_for(&self, ctx: &AccessContext, principal: &str) -> Result<AccessContext, PolicyError> {
        self.require_super_admin(ctx, "authz:check_as")?;
        check_principal(principal)?;
        let mut c = match principal.split_once(':') {
            Some(("user", rest)) => {
                let (coll, id) = rest.split_once('/').unwrap_or_default();
                AccessContext::user(coll, id)
            }
            Some((_, id)) => AccessContext::service(id, false),
            None => AccessContext::anonymous(),
        }
        .with_realm(ctx.realm_id.clone());
        if let Some(o) = &ctx.org_id {
            c = c.with_org(o.clone());
        }
        let eff = self
            .snapshot()
            .effective(&c.principal_key(), &c.realm_id, c.org_id.as_deref());
        c.roles = eff.roles.iter().cloned().collect();
        Ok(c)
    }

    /// Recent denials (super-admins).
    pub async fn audit_log(&self, ctx: &AccessContext, limit: u32) -> Result<Json, PolicyError> {
        self.require_super_admin(ctx, "audit:read")?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT at, realm_id, principal, tenant, org_id, permission, resource, step, reason, request_id, impersonator
                 FROM _forgebase_authz_audit ORDER BY id DESC LIMIT ?1",
                vec![i64::from(limit.clamp(1, 1000)).into()],
            ))
            .await?;
        let names = [
            "at",
            "realm_id",
            "principal",
            "tenant",
            "org_id",
            "permission",
            "resource",
            "step",
            "reason",
            "request_id",
            "impersonator",
        ];
        Ok(Json::Array(
            rows.rows
                .iter()
                .map(|r| {
                    Json::Object(
                        names
                            .iter()
                            .zip(r)
                            .map(|(k, v)| {
                                let j = match v {
                                    Value::Integer(i) => json!(i),
                                    Value::Text(t) => json!(t),
                                    _ => Json::Null,
                                };
                                ((*k).to_owned(), j)
                            })
                            .collect(),
                    )
                })
                .collect(),
        ))
    }
}

/// A snapshot together with the realm of the org it was checked for.
struct Snapshot2 {
    snap: std::sync::Arc<Snapshot>,
    realm: String,
}

fn member_stmt(org: &str, principal: &str, roles: &[String], sets: &[String]) -> Statement {
    Statement::new(
        "INSERT INTO _forgebase_memberships (id, org_id, principal, roles, role_sets, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6)",
        vec![
            generate_id().into(),
            org.into(),
            principal.into(),
            json_list(roles),
            json_list(sets),
            now().into(),
        ],
    )
}

fn env_stmt(id: &str, project: &str, name: &str) -> Statement {
    Statement::new(
        "INSERT INTO _forgebase_environments (id, project_id, name, created_at) VALUES (?1, ?2, ?3, ?4)",
        vec![id.into(), project.into(), name.into(), now().into()],
    )
}

/// Stored form of an invitation token (32 random bytes: SHA-256 suffices).
fn token_hash(token: &str) -> String {
    let d = Sha256::digest(token.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}
