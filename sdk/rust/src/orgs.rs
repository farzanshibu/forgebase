//! Tenancy (`/api/v1`): organizations, projects and environments, members, invitations, teams,
//! roles and role sets, realm bindings and account suspensions. `client.orgs()`.
//!
//! The OpenAPI document types these responses as plain objects, so the wire types are written by
//! hand after the server's JSON (snake_case). Lists are plain arrays.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::Result;
use crate::http::{Req, Transport, seg};

/// A realm (platform super-admins).
#[derive(Debug, Clone, Deserialize)]
pub struct TenantRealm {
    /// Realm id.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
}

/// An organization.
#[derive(Debug, Clone, Deserialize)]
pub struct Organization {
    /// Organization id.
    pub id: String,
    /// Realm.
    #[serde(default)]
    pub realm_id: String,
    /// Unique per realm.
    #[serde(default)]
    pub slug: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
}

/// An environment of a project.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectEnvironment {
    /// The tenant id of the environment's data.
    pub id: String,
    /// Owning project.
    #[serde(default)]
    pub project_id: String,
    /// `development`, `staging`, `production`, ...
    #[serde(default)]
    pub name: String,
}

/// A project.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgProject {
    /// Project id.
    pub id: String,
    /// Realm.
    #[serde(default)]
    pub realm_id: String,
    /// Owning organization; `None` for a personal project.
    #[serde(default)]
    pub org_id: Option<String>,
    /// Principal key of the owner of a personal project.
    #[serde(default)]
    pub owner_user: Option<String>,
    /// Slug.
    #[serde(default)]
    pub slug: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Present on [`OrgProjectsApi::get`].
    #[serde(default)]
    pub environments: Vec<ProjectEnvironment>,
}

/// A membership. `scope_id` is the organization id.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgMember {
    /// Membership id.
    #[serde(default)]
    pub id: String,
    /// Organization id.
    #[serde(default)]
    pub scope_id: String,
    /// `user:<collection>/<id>` or `service:<id>`.
    pub principal: String,
    /// Role keys.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Role-set keys.
    #[serde(default)]
    pub role_sets: Vec<String>,
    /// `active` or `suspended`.
    #[serde(default)]
    pub status: String,
}

/// What [`OrgMembersApi::set`] answers.
#[derive(Debug, Clone, Deserialize)]
pub struct MemberAssignment {
    /// Organization id.
    #[serde(default)]
    pub org_id: String,
    /// Principal.
    #[serde(default)]
    pub principal: String,
    /// Role keys.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Role-set keys.
    #[serde(default)]
    pub role_sets: Vec<String>,
    /// Status.
    #[serde(default)]
    pub status: String,
}

/// A team.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgTeam {
    /// Team id.
    pub id: String,
    /// Organization.
    #[serde(default)]
    pub org_id: String,
    /// Parent team.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Unique per organization.
    #[serde(default)]
    pub key: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Roles every member (and every member of a sub-team) holds.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Denied permission patterns, inherited the same way.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Principal keys (not returned by create).
    #[serde(default)]
    pub members: Vec<String>,
}

/// A role.
#[derive(Debug, Clone, Deserialize)]
pub struct PolicyRole {
    /// Role id.
    #[serde(default)]
    pub id: String,
    /// Realm.
    #[serde(default)]
    pub realm_id: String,
    /// Owning organization; `None` for a realm role.
    #[serde(default)]
    pub org_id: Option<String>,
    /// Key.
    pub key: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Granted permission patterns (`records:*`, `projects:read`).
    #[serde(default)]
    pub allow: Vec<String>,
    /// Denied patterns; a deny wins.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Seeded roles are edited by super-admins only.
    #[serde(default)]
    pub system: bool,
    /// Whether members can be given the role.
    #[serde(default)]
    pub assignable: bool,
}

/// A named bundle of roles.
#[derive(Debug, Clone, Deserialize)]
pub struct PolicyRoleSet {
    /// Id.
    #[serde(default)]
    pub id: String,
    /// Realm.
    #[serde(default)]
    pub realm_id: String,
    /// Organization; `None` for a realm set.
    #[serde(default)]
    pub org_id: Option<String>,
    /// Key.
    pub key: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Role keys.
    #[serde(default)]
    pub roles: Vec<String>,
}

/// Roles bound to a principal at realm level.
#[derive(Debug, Clone, Deserialize)]
pub struct RealmBinding {
    /// Principal.
    pub principal: String,
    /// Role keys.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Role-set keys.
    #[serde(default)]
    pub role_sets: Vec<String>,
}

/// An invitation.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgInvitation {
    /// Invitation id.
    pub id: String,
    /// Organization.
    #[serde(default)]
    pub org_id: String,
    /// Invited address.
    #[serde(default)]
    pub email: String,
    /// Roles granted on accept.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Unix seconds.
    #[serde(default)]
    pub expires_at: i64,
    /// `pending`, `accepted` or `expired`. Only the list route sets it.
    #[serde(default)]
    pub status: Option<String>,
    /// Inviter.
    #[serde(default)]
    pub invited_by: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub created_at: Option<i64>,
    /// Unix seconds.
    #[serde(default)]
    pub accepted_at: Option<i64>,
    /// Who accepted.
    #[serde(default)]
    pub accepted_by: Option<String>,
}

/// A fresh invitation: `token` is shown once (the server also mails it when a mailer is configured).
#[derive(Debug, Clone, Deserialize)]
pub struct InvitationCreated {
    /// The invitation.
    #[serde(flatten)]
    pub invitation: OrgInvitation,
    /// The secret token.
    pub token: String,
}

/// Answer of [`OrgInvitationsApi::accept`].
#[derive(Debug, Clone, Deserialize)]
pub struct InvitationAccepted {
    /// Organization joined.
    #[serde(default)]
    pub org_id: String,
    /// Principal that joined.
    #[serde(default)]
    pub principal: String,
    /// Roles granted.
    #[serde(default)]
    pub roles: Vec<String>,
}

/// An active account suspension.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountSuspension {
    /// Principal.
    pub principal: String,
    /// Reason.
    #[serde(default)]
    pub reason: Option<String>,
    /// Who suspended.
    #[serde(default)]
    pub suspended_by: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub created_at: Option<i64>,
    /// Unix seconds; `None` until lifted.
    #[serde(default)]
    pub until: Option<i64>,
}

/// A new organization or project.
#[derive(Debug, Clone, Serialize)]
pub struct NewOrg {
    /// Unique per realm.
    pub slug: String,
    /// Display name.
    pub name: String,
}

impl NewOrg {
    /// An organization (or project) named `name`.
    #[must_use]
    pub fn new(slug: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            name: name.into(),
        }
    }
}

/// A role to create or replace.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NewRole {
    /// Key.
    pub key: String,
    /// Display name (default: the key).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Granted patterns.
    pub allow: Vec<String>,
    /// Denied patterns.
    pub deny: Vec<String>,
}

impl NewRole {
    /// A role with no permissions yet.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ..Self::default()
        }
    }

    /// Grants a permission pattern.
    #[must_use]
    pub fn allow(mut self, pattern: impl Into<String>) -> Self {
        self.allow.push(pattern.into());
        self
    }

    /// Denies a permission pattern.
    #[must_use]
    pub fn deny(mut self, pattern: impl Into<String>) -> Self {
        self.deny.push(pattern.into());
        self
    }
}

/// A role set to create or replace.
#[derive(Debug, Clone, Serialize)]
pub struct NewRoleSet {
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Role keys.
    pub roles: Vec<String>,
}

/// A principal with roles (members, realm bindings).
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemberInput {
    /// `user:<collection>/<id>` or `service:<id>`.
    pub principal: String,
    /// Role keys.
    pub roles: Vec<String>,
    /// Role-set keys.
    pub role_sets: Vec<String>,
}

impl MemberInput {
    /// `principal` with `roles`.
    #[must_use]
    pub fn new(principal: impl Into<String>, roles: &[&str]) -> Self {
        Self {
            principal: principal.into(),
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
            role_sets: Vec::new(),
        }
    }
}

/// A team to create.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NewTeam {
    /// Key (unique per organization).
    pub key: String,
    /// Display name.
    pub name: String,
    /// Parent team id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Roles.
    pub roles: Vec<String>,
    /// Denied patterns.
    pub deny: Vec<String>,
}

impl NewTeam {
    /// A top-level team.
    #[must_use]
    pub fn new(key: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            name: name.into(),
            ..Self::default()
        }
    }

    /// Puts it under a parent team.
    #[must_use]
    pub fn parent(mut self, id: impl Into<String>) -> Self {
        self.parent_id = Some(id.into());
        self
    }
}

/// Tenancy: `client.orgs()`.
#[derive(Clone)]
pub struct OrgsApi {
    pub(crate) t: Arc<Transport>,
}

impl OrgsApi {
    /// Realms.
    #[must_use]
    pub fn realms(&self) -> TenantRealmsApi {
        TenantRealmsApi { t: self.t.clone() }
    }
    /// Projects and environments.
    #[must_use]
    pub fn projects(&self) -> OrgProjectsApi {
        OrgProjectsApi { t: self.t.clone() }
    }
    /// Memberships.
    #[must_use]
    pub fn members(&self) -> OrgMembersApi {
        OrgMembersApi { t: self.t.clone() }
    }
    /// Invitations.
    #[must_use]
    pub fn invitations(&self) -> OrgInvitationsApi {
        OrgInvitationsApi { t: self.t.clone() }
    }
    /// Teams.
    #[must_use]
    pub fn teams(&self) -> OrgTeamsApi {
        OrgTeamsApi { t: self.t.clone() }
    }
    /// Roles.
    #[must_use]
    pub fn roles(&self) -> OrgRolesApi {
        OrgRolesApi { t: self.t.clone() }
    }
    /// Role sets.
    #[must_use]
    pub fn role_sets(&self) -> OrgRoleSetsApi {
        OrgRoleSetsApi { t: self.t.clone() }
    }
    /// Realm-level role bindings (super-admins).
    #[must_use]
    pub fn realm_bindings(&self) -> RealmBindingsApi {
        RealmBindingsApi { t: self.t.clone() }
    }
    /// Account suspensions (super-admins).
    #[must_use]
    pub fn suspensions(&self) -> AccountSuspensionsApi {
        AccountSuspensionsApi { t: self.t.clone() }
    }

    /// Organizations the caller is an active member of (every organization of the realm for super-admins).
    pub async fn list(&self) -> Result<Vec<Organization>> {
        self.t.json(Req::get("/api/v1/orgs")).await
    }

    /// One organization by id.
    pub async fn get(&self, org: &str) -> Result<Organization> {
        self.t.json(Req::get(format!("/api/v1/orgs/{}", seg(org)))).await
    }

    /// Creates an organization; the caller becomes its owner.
    pub async fn create(&self, input: &NewOrg) -> Result<Organization> {
        self.t.json(Req::post("/api/v1/orgs").json(input)?).await
    }

    /// Renames an organization.
    pub async fn rename(&self, org: &str, name: &str) -> Result<Organization> {
        self.t
            .json(Req::patch(format!("/api/v1/orgs/{}", seg(org))).json_raw(json!({"name": name})))
            .await
    }

    /// Deletes an organization with its projects, teams and memberships (tenant data rows stay).
    pub async fn delete(&self, org: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("/api/v1/orgs/{}", seg(org)))).await
    }
}

/// Realms: `client.orgs().realms()`.
#[derive(Clone)]
pub struct TenantRealmsApi {
    t: Arc<Transport>,
}

impl TenantRealmsApi {
    /// Every realm (platform super-admins).
    pub async fn list(&self) -> Result<Vec<TenantRealm>> {
        self.t.json(Req::get("/api/v1/realms")).await
    }

    /// Creates a realm (platform super-admins).
    pub async fn create(&self, id: &str, name: &str) -> Result<TenantRealm> {
        self.t
            .json(Req::post("/api/v1/realms").json_raw(json!({"id": id, "name": name})))
            .await
    }
}

/// Projects and environments: `client.orgs().projects()`.
#[derive(Clone)]
pub struct OrgProjectsApi {
    t: Arc<Transport>,
}

fn projects_path(org: Option<&str>) -> String {
    org.map_or_else(
        || "/api/v1/projects".to_owned(),
        |o| format!("/api/v1/orgs/{}/projects", seg(o)),
    )
}

impl OrgProjectsApi {
    /// Projects of an organization, or the caller's personal projects when `org` is `None`.
    pub async fn list(&self, org: Option<&str>) -> Result<Vec<OrgProject>> {
        self.t.json(Req::get(projects_path(org))).await
    }

    /// Creates a project (with the default environments) in an organization, or in the caller's personal scope.
    pub async fn create(&self, input: &NewOrg, org: Option<&str>) -> Result<OrgProject> {
        self.t.json(Req::post(projects_path(org)).json(input)?).await
    }

    /// One project with its environments.
    pub async fn get(&self, project: &str) -> Result<OrgProject> {
        self.t
            .json(Req::get(format!("/api/v1/projects/{}", seg(project))))
            .await
    }

    /// Deletes a project and its environments.
    pub async fn delete(&self, project: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/projects/{}", seg(project))))
            .await
    }

    /// Adds an environment (`staging`, `qa`, ...).
    pub async fn create_environment(&self, project: &str, name: &str) -> Result<ProjectEnvironment> {
        self.t
            .json(Req::post(format!("/api/v1/projects/{}/environments", seg(project))).json_raw(json!({"name": name})))
            .await
    }
}

/// Organization memberships: `client.orgs().members()`.
#[derive(Clone)]
pub struct OrgMembersApi {
    t: Arc<Transport>,
}

impl OrgMembersApi {
    /// Members of an organization.
    pub async fn list(&self, org: &str) -> Result<Vec<OrgMember>> {
        self.t
            .json(Req::get(format!("/api/v1/orgs/{}/members", seg(org))))
            .await
    }

    /// Adds a member or replaces their roles. Roles must be assignable and within the caller's own permissions.
    pub async fn set(&self, org: &str, member: &MemberInput) -> Result<MemberAssignment> {
        self.t
            .json(Req::put(format!("/api/v1/orgs/{}/members", seg(org))).json(member)?)
            .await
    }

    /// Removes a member and their team memberships.
    pub async fn remove(&self, org: &str, principal: &str) -> Result<()> {
        self.t
            .unit(
                Req::post(format!("/api/v1/orgs/{}/members/remove", seg(org)))
                    .json_raw(json!({"principal": principal})),
            )
            .await
    }

    /// Suspends (`false`) or reactivates (`true`) a membership.
    pub async fn set_active(&self, org: &str, principal: &str, active: bool) -> Result<()> {
        self.t
            .unit(
                Req::post(format!("/api/v1/orgs/{}/members/status", seg(org)))
                    .json_raw(json!({"principal": principal, "active": active})),
            )
            .await
    }
}

/// Invitations: `client.orgs().invitations()`.
#[derive(Clone)]
pub struct OrgInvitationsApi {
    t: Arc<Transport>,
}

impl OrgInvitationsApi {
    /// Invitations of an organization, newest first. The token is never listed.
    pub async fn list(&self, org: &str) -> Result<Vec<OrgInvitation>> {
        self.t
            .json(Req::get(format!("/api/v1/orgs/{}/invitations", seg(org))))
            .await
    }

    /// Invites an email address. The returned `token` is shown once.
    pub async fn create(&self, org: &str, email: &str, roles: &[&str]) -> Result<InvitationCreated> {
        self.t
            .json(
                Req::post(format!("/api/v1/orgs/{}/invitations", seg(org)))
                    .json_raw(json!({"email": email, "roles": roles})),
            )
            .await
    }

    /// Accepts an invitation as the signed-in user (their account email must match the invited address). Single use.
    pub async fn accept(&self, token: &str) -> Result<InvitationAccepted> {
        self.t
            .json(Req::post("/api/v1/invitations/accept").json_raw(json!({"token": token})))
            .await
    }
}

/// Teams: `client.orgs().teams()`.
#[derive(Clone)]
pub struct OrgTeamsApi {
    t: Arc<Transport>,
}

impl OrgTeamsApi {
    /// Teams of an organization.
    pub async fn list(&self, org: &str) -> Result<Vec<OrgTeam>> {
        self.t.json(Req::get(format!("/api/v1/orgs/{}/teams", seg(org)))).await
    }

    /// Creates a team, optionally under a parent team.
    pub async fn create(&self, org: &str, team: &NewTeam) -> Result<OrgTeam> {
        self.t
            .json(Req::post(format!("/api/v1/orgs/{}/teams", seg(org))).json(team)?)
            .await
    }

    /// Deletes a team and its sub-teams. `team` is the team id.
    pub async fn delete(&self, team: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("/api/v1/teams/{}", seg(team)))).await
    }

    /// Adds an organization member to a team.
    pub async fn add_member(&self, team: &str, principal: &str) -> Result<()> {
        self.t
            .unit(Req::post(format!("/api/v1/teams/{}/members", seg(team))).json_raw(json!({"principal": principal})))
            .await
    }

    /// Removes a member from a team.
    pub async fn remove_member(&self, team: &str, principal: &str) -> Result<()> {
        self.t
            .unit(
                Req::post(format!("/api/v1/teams/{}/members/remove", seg(team)))
                    .json_raw(json!({"principal": principal})),
            )
            .await
    }
}

/// Roles of an organization, or of the realm when `org` is `None`: `client.orgs().roles()`.
#[derive(Clone)]
pub struct OrgRolesApi {
    t: Arc<Transport>,
}

fn roles_path(org: Option<&str>) -> String {
    org.map_or_else(
        || "/api/v1/roles".to_owned(),
        |o| format!("/api/v1/orgs/{}/roles", seg(o)),
    )
}

impl OrgRolesApi {
    /// Roles usable in the organization (its own and the realm's), or the realm's roles.
    pub async fn list(&self, org: Option<&str>) -> Result<Vec<PolicyRole>> {
        self.t.json(Req::get(roles_path(org))).await
    }

    /// Creates or replaces a role. Realm roles need a super-admin; org roles stay within the caller's own permissions.
    pub async fn put(&self, role: &NewRole, org: Option<&str>) -> Result<PolicyRole> {
        self.t.json(Req::put(roles_path(org)).json(role)?).await
    }

    /// Deletes a custom role (seeded roles cannot be deleted).
    pub async fn delete(&self, key: &str, org: Option<&str>) -> Result<()> {
        self.t
            .unit(Req::delete(format!("{}/{}", roles_path(org), seg(key))))
            .await
    }
}

/// Role sets (named bundles of roles): `client.orgs().role_sets()`.
#[derive(Clone)]
pub struct OrgRoleSetsApi {
    t: Arc<Transport>,
}

fn role_sets_path(org: Option<&str>) -> String {
    org.map_or_else(
        || "/api/v1/role-sets".to_owned(),
        |o| format!("/api/v1/orgs/{}/role-sets", seg(o)),
    )
}

impl OrgRoleSetsApi {
    /// Role sets of the organization, or the realm's.
    pub async fn list(&self, org: Option<&str>) -> Result<Vec<PolicyRoleSet>> {
        self.t.json(Req::get(role_sets_path(org))).await
    }

    /// Creates or replaces a role set.
    pub async fn put(&self, set: &NewRoleSet, org: Option<&str>) -> Result<PolicyRoleSet> {
        self.t.json(Req::put(role_sets_path(org)).json(set)?).await
    }
}

/// Realm-level role bindings (super-admins): `client.orgs().realm_bindings()`.
#[derive(Clone)]
pub struct RealmBindingsApi {
    t: Arc<Transport>,
}

impl RealmBindingsApi {
    /// Every binding.
    pub async fn list(&self) -> Result<Vec<RealmBinding>> {
        self.t.json(Req::get("/api/v1/realm-bindings")).await
    }

    /// Binds realm roles to a principal, replacing the earlier binding.
    pub async fn put(&self, binding: &MemberInput) -> Result<RealmBinding> {
        self.t.json(Req::put("/api/v1/realm-bindings").json(binding)?).await
    }
}

/// Account suspensions (super-admins): `client.orgs().suspensions()`.
#[derive(Clone)]
pub struct AccountSuspensionsApi {
    t: Arc<Transport>,
}

impl AccountSuspensionsApi {
    /// Active suspensions.
    pub async fn list(&self) -> Result<Vec<AccountSuspension>> {
        self.t.json(Req::get("/api/v1/suspensions")).await
    }

    /// Suspends an account everywhere, until `until` (Unix seconds) or until lifted.
    pub async fn suspend(&self, principal: &str, reason: Option<&str>, until: Option<i64>) -> Result<()> {
        self.t
            .unit(
                Req::post("/api/v1/suspensions")
                    .json_value(json!({"principal": principal, "reason": reason, "until": until})),
            )
            .await
    }

    /// Lifts a suspension.
    pub async fn lift(&self, principal: &str) -> Result<()> {
        self.t
            .unit(Req::post("/api/v1/suspensions/lift").json_raw(json!({"principal": principal})))
            .await
    }
}
