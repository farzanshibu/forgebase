# Requirements addendum: authorization, entitlements, quotas and billing

_Landed 2026-11-07 from the human. Binding for FB-21, FB-23, FB-24, FB-30, FB-41, FB-70, FB-71 and FB-80. Where this conflicts with docs/SPEC.md, this file wins for the areas it covers._

## A. Human instruction (verbatim)

> update the billing studio - subscription and onetime purchase
> usage based
> per month
> like proper billing application like
>
> authz auth - feature
>
> (reference document below)
>
> include

## B. ForgeBase decisions taken from it (god, 2026-11-07)

1. **Tenancy model: both B2C and B2B (the reference's third model).** A user can hold a personal plan and also belong to several organizations, each with its own paid plan. Every check names the subject it bills against: the user or the active organization.
2. **ForgeBase is the identity provider.** The reference is written for Clerk. In ForgeBase, Clerk's part (identity, sessions, organizations, membership) is `forgebase-auth` (FB-20) plus the tenancy model (FB-21). Nothing calls Clerk.
3. **Storage mapping.** The reference recommends PostgreSQL + Redis. ForgeBase keeps authoritative policies, entitlements, usage events and counters in its own database (SQLite now, Postgres behind the Backend trait). The built-in KV engine (FB-31) may serve as a fast counter and cache layer, never as the source of truth. Quota reservation must be atomic in the database transaction.
4. **Evaluation pipeline (fixed order, every protected operation, server side, any failure denies):** 1 authentication → 2 tenant/organization resolution → 3 role and permission → 4 plan entitlement (feature) → 5 resource access (ACL / record rules) → 6 usage and quota. The decision and the failed step are audit-logged.
5. **Effective permission priority:** 1 platform super-admin (a separately protected platform capability, never an org role) → 2 explicit user deny → 3 resource-specific restrictions → 4 org membership and role permissions → 5 org plan entitlements → 6 user subscription entitlements → 7 user quota overrides → 8 quota availability. Explicit deny beats any grant.
6. **Entitlement is not quota.** A feature check answers "is it in the plan"; a quota check answers "is there usage left". Both are required.
7. **Billing is a proper billing application:** subscriptions (monthly and yearly), one-time purchases, usage-based (metered) pricing, trials, add-ons, usage packs, overage, proration, coupons, invoices, refunds, dunning, provider webhooks (Stripe, Razorpay), all synchronising entitlements idempotently. Still off by default (SPEC).

## C. Work package mapping

| Area from the reference | Package |
|---|---|
| Default roles (Super Admin, Owner, Admin, Manager, Developer, Billing Manager, Analyst, Member, Viewer, Guest), permission namespace `resource:action` and `org:<feature>:<permission>`, custom roles, role sets, team hierarchy, permission inheritance, explicit allow/deny, resource ACL (`resource_permissions`), platform super-admin, account suspension, permission cache invalidation, evaluation pipeline with hook points for steps 4 and 6 | FB-21 |
| Plans, features, plan_features, add-ons, trials, user and org entitlements, user_overrides, quota_policies, usage_events (immutable, idempotency key), usage_counters, atomic quota reservation and release, concurrency limits (leases), quota periods (hourly, daily, monthly, lifetime, storage), per user / per team / per resource scopes, soft and hard limits, alerts at 70 / 90 / 100 %, rollover, usage packs, enterprise custom limits, plan-based API rate limits, quota resets via jobs, `has({permission})`, `has({feature})`, quota check/consume API | FB-24 (new) |
| Subscriptions, one-time purchases, usage-based billing from FB-24 usage events, monthly/yearly cycles, proration, trials, coupons, invoices, refunds/credit notes, dunning, payment provider trait with Stripe and Razorpay, signed idempotent webhooks, entitlement sync | FB-70 |
| Billing Studio as a proper billing app | FB-71 |
| Roles and permission matrix editor, custom roles, role sets, team hierarchy, resource ACL, overrides, suspension, controlled and audited admin impersonation | FB-23 |
| Storage quota enforcement | FB-30 consumes FB-24 |
| SDK `has()` / quota helpers | FB-41 |
| Bypass tests for every step of the pipeline | FB-80 |

## D. Reference document supplied by the human (verbatim)

# Clerk: Complete Permission, Roles, Plans & Usage Limit Architecture

If you're building a SaaS application with Clerk, you need more than basic authentication and role-based access control (RBAC).

You need a complete authorization system that handles:

1. User permissions
2. Organization/team permissions
3. Roles and custom roles
4. Subscription plans
5. Feature access
6. Feature usage limits and quotas
7. Resource-level permissions
8. API and server-side authorization
9. Billing and entitlement synchronization
10. Admin and super-admin access

The recommended approach is to use Clerk for identity and organization membership, with a dedicated authorization and entitlement layer in your backend.

## 1. Complete authorization scope

Use four independent authorization dimensions.

User-level access

Individual user identity, role, permissions, subscription and personal limits.

Example: User can create 5 projects per month.

Organization / Team access

Team membership, team roles, shared resources, permissions and team quotas.

Example: Team can create 100 projects per month.

Plan-level entitlements

Subscription plan, included features, limits, add-ons and billing status.

Example: Pro plan includes AI generation and 10 GB storage.

Resource-level access

Access to specific projects, documents, folders, files and records.

Example: A user can edit only projects they own or have been assigned.

## 2. Roles and permissions

A suggested permission matrix:

| Role            | Permissions                            |
| --------------- | -------------------------------------- |
| Super Admin     | Everything                             |
| Owner           | Full organization control              |
| Admin           | Manage members, settings and resources |
| Manager         | Manage assigned teams and projects     |
| Developer       | API, integrations and development      |
| Billing Manager | Plans, invoices and payments           |
| Analyst         | Read analytics and reports             |
| Member          | Standard resource access               |
| Viewer          | Read-only                              |
| Guest           | Limited shared-resource access         |

Use permission-based checks rather than relying entirely on role names.

### Permission naming convention

Use a consistent namespace:

```
users:read
users:create
users:update
users:delete
users:invite

teams:read
teams:create
teams:update
teams:delete
teams:members:manage

projects:read
projects:create
projects:update
projects:delete
projects:share

files:read
files:upload
files:download
files:delete

billing:read
billing:manage

analytics:read
analytics:export

api_keys:read
api_keys:create
api_keys:revoke

ai:generate
ai:manage
```

For Clerk Organizations, custom permission keys follow this format:

```
org:<feature>:<permission>

org:projects:create
org:projects:delete
org:files:upload
org:billing:manage
```

Important: Clerk custom organization permissions are tied to Features. Clerk's built-in system permissions are not included in session claims, so don't assume they can be checked with the same server-side `has()` permission check.

## 3. Subscription plans and feature entitlements

Example SaaS plans:

| Feature              | Free | Starter | Pro      | Enterprise |
| -------------------- | ---- | ------- | -------- | ---------- |
| Projects             | 3    | 20      | 100      | Unlimited  |
| Team members         | 1    | 5       | 25       | Unlimited  |
| Storage              | 1 GB | 10 GB   | 100 GB   | Custom     |
| API requests/month   | 1K   | 10K     | 100K     | Custom     |
| AI generations/month | 10   | 100     | 1K       | Custom     |
| Custom domains       | No   | No      | Yes      | Yes        |
| Audit logs           | No   | Basic   | Advanced | Advanced   |
| SSO                  | No   | No      | Optional | Yes        |
| Webhooks             | No   | Yes     | Yes      | Yes        |

These are illustrative limits, not Clerk's default plans.

### Feature-based entitlement checks

Prefer checking individual features instead of hardcoding plan names.

For example:

```
const canUseAI = has({ feature: "ai_generation" });
const canUseAPI = has({ feature: "api_access" });
const canUseAnalytics = has({ feature: "advanced_analytics" });
```

Clerk supports checking Plans and Features for users and active Organizations. With both Organizations and Billing enabled, organization custom permission checks also depend on the corresponding Feature being included in the active organization plan.

## 4. Usage limits and quota management

This is particularly important: having permission to use a feature does not mean unlimited usage of that feature.

You need two separate checks:

- Entitlement: Is this feature available to the user/team?
- Quota: Does the user/team have remaining usage?

Example:

AI Generation — Team Usage: 750 / 1,000 (Available: 750 used, 250 remaining)

| Usage        | Limit          |
| ------------ | -------------- |
| Storage      | 75 / 100 GB    |
| API calls    | 8,200 / 10,000 |
| Projects     | 15 / 20        |
| Team members | 4 / 5          |

### Quota types to support

| Quota        | Example              |
| ------------ | -------------------- |
| Monthly      | 1,000 AI generations |
| Daily        | 100 API requests     |
| Hourly       | 500 requests         |
| Concurrent   | 5 running jobs       |
| Lifetime     | 10 free exports      |
| Storage      | 100 GB               |
| Per user     | 100 generations      |
| Per team     | 1,000 generations    |
| Per resource | 10 collaborators     |

Also support:

- Soft limits and hard limits.
- Usage warnings at 70%, 90% and 100%.
- Overage billing.
- Additional usage packs.
- Trial allowances.
- Rollover quotas.
- Custom enterprise limits.

Important: Clerk handles identity, roles, permissions and subscription-based feature access. For reliable usage metering, counters, concurrency limits and overage calculations, implement a backend quota service backed by your database or Redis.

## 5. Recommended authorization evaluation

Every protected operation should follow this sequence:

Incoming API request

1. Authentication: Is the user authenticated?
2. Tenant / Organization: Which organization owns this resource?
3. Role and permissions: Does the user have the required permission?
4. Subscription entitlement: Does their plan include this feature?
5. Resource access: Can they access this specific record?
6. Usage and quota: Is sufficient quota available?

Allow operation

Any failed check should deny the operation. Perform these checks server-side, not just in frontend components.

## 6. Suggested database architecture

Even when using Clerk, maintain your own application database for resource authorization and usage tracking.

I recommend PostgreSQL + Redis.

### Core database tables

| Table                | Responsibility                     |
| -------------------- | ---------------------------------- |
| users                | Application user profiles          |
| organizations        | Tenant/workspace records           |
| memberships          | User-to-team relationships         |
| roles                | Custom application roles           |
| permissions          | Permission definitions             |
| role_permissions     | Role-permission mapping            |
| subscriptions        | Subscription state                 |
| plans                | Plan definitions                   |
| features             | Feature definitions                |
| plan_features        | Feature entitlements               |
| user_overrides       | Individual exceptions              |
| usage_counters       | Usage tracking                     |
| usage_events         | Immutable usage records            |
| quota_policies       | Limits and quota rules             |
| resource_permissions | Record-level access                |
| api_keys             | API authentication                 |
| audit_logs           | Authorization and activity history |

### Example entitlement structure

```
{
  "organization_id": "org_123",
  "plan": "pro",
  "subscription_status": "active",
  "features": {
    "ai_generation": {
      "enabled": true,
      "limit": 1000,
      "period": "monthly"
    },
    "storage": {
      "enabled": true,
      "limit": 107374182400,
      "unit": "bytes"
    },
    "api_access": {
      "enabled": true,
      "limit": 100000,
      "period": "monthly"
    }
  }
}
```

### Effective permission resolution

Use this priority model:

1. Platform super-admin privileges.
2. Explicit user deny overrides.
3. Resource-specific restrictions.
4. Organization membership and role permissions.
5. Organization plan entitlements.
6. User-specific subscription entitlements.
7. User-specific quota overrides.
8. Usage quota availability.

Be careful: super-admin access should be a separately protected platform-level capability, not a normal Organization role. Explicit deny rules should normally override ordinary grants.

## 7. Recommended architecture for your Next.js stack

Next.js 16 Application (Frontend · Server Actions · API Routes) → Authorization Service (Authentication, RBAC / ABAC, Entitlements, Quota Engine, Resource ACL, Audit Logging) → Clerk (Auth · Organizations · Roles · Billing), PostgreSQL (Policies · Usage · Entitlements · ACL), Redis (Counters · Rate limits · Cache), Background Jobs (Usage aggregation · Billing · Reset).

### Example Next.js permission check

```
import { auth } from "@clerk/nextjs/server";

export async function requirePermission(
  permission: string
) {
  const { userId, has, orgId } = await auth();

  if (!userId) {
    throw new Error("UNAUTHENTICATED");
  }

  if (!orgId) {
    throw new Error("ORGANIZATION_REQUIRED");
  }

  if (!has({ permission })) {
    throw new Error("FORBIDDEN");
  }

  return { userId, orgId };
}
```

For a real application, extend this with database-backed resource ownership, subscription state and atomic quota reservation. Don't trust frontend-supplied user IDs, organization IDs or usage counters.

## 8. Full scope checklist

Identity & authentication: User authentication · Sessions and devices · MFA and reverification · User metadata · Account suspension

Organization & teams: Multi-tenant organizations · Team membership · Invitations · Custom roles · Role sets · Team hierarchy

Authorization: RBAC · ABAC · Resource-level ACL · Permission inheritance · Explicit allow/deny · Platform super-admin · API authorization

Plans & billing: Free / Pro / Enterprise plans · User subscriptions · Organization subscriptions · Feature entitlements · Add-ons · Trials · Subscription lifecycle

Usage & quotas: Monthly and daily limits · Per-user quotas · Per-team quotas · Storage limits · API rate limits · Concurrency limits · Overage billing · Usage alerts · Quota resets

Security & operations: Audit logs · Permission cache invalidation · Webhook synchronization · Idempotent usage tracking · Atomic quota enforcement · Admin impersonation controls · Security monitoring

## 9. Official Clerk documentation

- Organizations – Roles and Permissions: https://clerk.com/docs/guides/organizations/control-access/roles-and-permissions
- Authorization Checks: https://clerk.com/docs/guides/secure/authorization-checks
- Features and Entitlements: https://clerk.com/docs/guides/secure/features
- Organizations and Multi-tenancy: https://clerk.com/docs/guides/organizations/overview
- Organization Role Sets: https://clerk.com/docs/guides/organizations/control-access/role-sets

## My recommendation

For a serious multi-tenant SaaS, build a centralized Authorization & Entitlement Engine rather than scattering Clerk `has()` calls throughout your codebase.

Keep Clerk responsible for identity and membership, PostgreSQL responsible for authoritative policies and quotas, and Redis responsible for fast atomic usage enforcement.

One important architecture decision: Are you building this for:

- Individual users with personal subscriptions (B2C)?
- Teams/organizations with shared subscriptions (B2B)?
- Both, with users able to have personal plans and also belong to multiple paid teams?

I recommend the third model if you're building a general-purpose SaaS platform.

## E. Second addendum from the human, 2026-11-07 (verbatim)

| Area | Included capabilities |
|---|---|
| Multi-tenant identity | Isolated realms, email/password and one-time-code login, phone-number login with SMS codes, Google, GitHub, Apple, Facebook, WeChat, Discord, and LDAP login, passkeys, TOTP 2FA, an OpenID Connect layer, Cloudflare Turnstile bot protection |
| Authorization and apps | Realm-level RBAC, organizations with their own roles and membership, Client Apps, API keys, OAuth 2.0, device authorization, cross-app SSO |
| Billing and payments | Stripe, Creem, and WeChat Pay, App Store / Google Play in-app purchases, subscriptions, one-time purchases, multi-currency pricing, invoices, payment-to-entitlement mapping, payment and credit-consumption statistics |
| Credits and usage | Prepaid balances, top-ups, refunds, expiry, per-user ledgers, grants, rolling quota windows |
| Admin and operations | Users, roles, billing, credits, apps, tenant settings, audit trails, account lifecycle management |
| Product customization | Custom domains, white-label branding, transactional email, versioned legal agreements, API docs, SDKs |

## F. Decisions and mapping for section E (god, 2026-11-07)

1. **Realm** is the top isolation level: realm → organizations → projects/teams. A realm is a separate user pool (the same email can exist in two realms), with its own login methods, providers, branding, apps and realm-level roles. Existing data lives in realm `default`.
2. **OpenID Connect layer** means ForgeBase is also an identity provider for other apps: Client Apps, OAuth 2.0 authorization server, device authorization (RFC 8628), cross-app single sign-on inside a realm.
3. **Credits** are an append-only ledger (every grant, top-up, consumption, refund and expiry is a row; balances are derived and checked), separate from quotas (FB-24) but able to pay for usage.

| Capability | Package |
|---|---|
| Realm as top tenant level in the data model, AccessContext and realm-level RBAC | FB-21 (told) |
| Facebook, WeChat, Discord login (with Google, GitHub, Apple, generic OIDC), passkeys | FB-22 |
| Realm-scoped auth tables, phone-number login with SMS codes (provider trait), LDAP login, Cloudflare Turnstile on signup/login/reset/OTP, account lifecycle (suspend, deactivate, delete, anonymise, export) | FB-25 (new) |
| Client Apps, OAuth 2.0 authorization server (code + PKCE, client credentials, refresh, introspection, revocation), device authorization, OIDC discovery/JWKS/userinfo/ID tokens, consent, cross-app SSO | FB-26 (new) |
| Rolling (sliding) quota windows | FB-24 |
| Multi-currency pricing, payment-to-entitlement mapping, payment statistics | FB-70 |
| Creem, WeChat Pay, App Store and Google Play in-app purchases (server-side verification and server notifications) | FB-72 (new) |
| Prepaid balances, top-ups, refunds, expiry, per-user and per-org ledgers, grants, credit consumption and statistics | FB-73 (new) |
| Custom domains, white-label branding and hosted login pages, transactional email (SMTP + templates), versioned legal agreements with acceptance records, hosted API docs | FB-74 (new) |
| Admin: users, roles, audit trails, account lifecycle | FB-23 |
| Admin: billing, credits, payment and credit statistics | FB-71 |
| Admin: realms, apps, identity providers, tenant settings, branding, domains, email templates, legal agreements | FB-27 (new) |
| SDKs | FB-41, FB-82 |
