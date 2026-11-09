-- 0009_entitlements: plans, features, entitlements, quotas and usage metering
-- (FB-24, ADR-0010).
--
-- Entitlement is not quota: a feature check answers "is it in the subject's
-- plan", a quota check answers "is there usage left". Subjects are the billing
-- subjects of ADR-0008 §2: `org:<org id>` for an organization, otherwise the
-- caller's principal key (`user:<collection>/<id>`, `service:<id>`).
--
-- Usage events are immutable (triggers below); counters, reservations and
-- leases change only inside the transaction that records the usage, so a
-- quota is never over-granted. Times are Unix epoch milliseconds.

-- ---------------------------------------------------------------------------
-- Catalog
-- ---------------------------------------------------------------------------

-- What can be granted. `kind`: boolean (on/off), metered (counted per
-- period), gauge (a level such as storage bytes), concurrency (simultaneous
-- leases), rate (API requests per hour or day, enforced in the HTTP layer).
-- `permissions`: permission patterns this feature gates (pipeline step 4).
CREATE TABLE _forgebase_features (
    key         TEXT    NOT NULL PRIMARY KEY CHECK (length(key) BETWEEN 1 AND 64),
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    description TEXT    NOT NULL DEFAULT '',
    kind        TEXT    NOT NULL CHECK (kind IN ('boolean', 'metered', 'gauge', 'concurrency', 'rate')),
    unit        TEXT    NOT NULL DEFAULT '' CHECK (length(unit) <= 32),
    permissions TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(permissions)),
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;

-- At most one default plan: the plan of every subject without an entitled
-- subscription.
CREATE TABLE _forgebase_plans (
    id          TEXT    NOT NULL PRIMARY KEY,
    key         TEXT    NOT NULL UNIQUE CHECK (length(key) BETWEEN 1 AND 64),
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    description TEXT    NOT NULL DEFAULT '',
    is_default  INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    active      INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    trial_days  INTEGER NOT NULL DEFAULT 0 CHECK (trial_days >= 0),
    metadata    TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(metadata)),
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX _forgebase_plans_default ON _forgebase_plans (is_default) WHERE is_default = 1;

-- A feature in a plan. `limit_value` NULL = unlimited. `period`: none
-- (boolean, gauge, concurrency), hour, day, month, lifetime, rolling (then
-- `window_secs`). `trial_limit` replaces the limit while the subscription is
-- trialing (the trial allowance).
CREATE TABLE _forgebase_plan_features (
    plan_id     TEXT    NOT NULL REFERENCES _forgebase_plans(id) ON DELETE CASCADE,
    feature_key TEXT    NOT NULL REFERENCES _forgebase_features(key) ON DELETE CASCADE,
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    limit_value INTEGER CHECK (limit_value IS NULL OR limit_value >= 0),
    unit        TEXT    NOT NULL DEFAULT '',
    period      TEXT    NOT NULL DEFAULT 'none'
                CHECK (period IN ('none', 'hour', 'day', 'month', 'lifetime', 'rolling')),
    window_secs INTEGER CHECK (window_secs IS NULL OR window_secs >= 1),
    trial_limit INTEGER CHECK (trial_limit IS NULL OR trial_limit >= 0),
    PRIMARY KEY (plan_id, feature_key)
) STRICT;

-- Add-ons bought on top of a plan, per quantity.
CREATE TABLE _forgebase_addons (
    id          TEXT    NOT NULL PRIMARY KEY,
    key         TEXT    NOT NULL UNIQUE CHECK (length(key) BETWEEN 1 AND 64),
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    description TEXT    NOT NULL DEFAULT '',
    active      INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;

-- What one unit of an add-on grants: enables the feature and adds
-- `limit_value` units to its limit (NULL: unlimited when the add-on is what
-- enables it, no change otherwise).
CREATE TABLE _forgebase_addon_features (
    addon_id    TEXT    NOT NULL REFERENCES _forgebase_addons(id) ON DELETE CASCADE,
    feature_key TEXT    NOT NULL REFERENCES _forgebase_features(key) ON DELETE CASCADE,
    limit_value INTEGER CHECK (limit_value IS NULL OR limit_value >= 0),
    period      TEXT    NOT NULL DEFAULT 'none'
                CHECK (period IN ('none', 'hour', 'day', 'month', 'lifetime', 'rolling')),
    window_secs INTEGER CHECK (window_secs IS NULL OR window_secs >= 1),
    PRIMARY KEY (addon_id, feature_key)
) STRICT;

-- Usage packs: one-off extra units of a metered feature, used once the
-- period's allowance is spent; `valid_days` NULL = no expiry.
CREATE TABLE _forgebase_usage_packs (
    id          TEXT    NOT NULL PRIMARY KEY,
    key         TEXT    NOT NULL UNIQUE CHECK (length(key) BETWEEN 1 AND 64),
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    feature_key TEXT    NOT NULL REFERENCES _forgebase_features(key) ON DELETE CASCADE,
    units       INTEGER NOT NULL CHECK (units >= 1),
    valid_days  INTEGER CHECK (valid_days IS NULL OR valid_days >= 1),
    active      INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at  INTEGER NOT NULL
) STRICT;

-- How a feature's limit is enforced. `scope`: subject (the org or the user
-- billed), user (each member of the subject), team, resource. `enforcement`:
-- hard (deny at the limit) or soft (allow and record overage, up to
-- `overage_pct` over the limit when set). Rollover carries unused units of a
-- calendar period into the next one: at most `rollover_pct` % of the limit
-- and at most `rollover_cap` units. `permissions`: permission patterns that
-- pipeline step 6 admits against this quota, `cost` units each.
CREATE TABLE _forgebase_quota_policies (
    feature_key    TEXT    NOT NULL PRIMARY KEY REFERENCES _forgebase_features(key) ON DELETE CASCADE,
    scope          TEXT    NOT NULL DEFAULT 'subject' CHECK (scope IN ('subject', 'user', 'team', 'resource')),
    enforcement    TEXT    NOT NULL DEFAULT 'hard' CHECK (enforcement IN ('hard', 'soft')),
    overage_pct    INTEGER CHECK (overage_pct IS NULL OR overage_pct >= 0),
    rollover_pct   INTEGER NOT NULL DEFAULT 0 CHECK (rollover_pct BETWEEN 0 AND 100),
    rollover_cap   INTEGER CHECK (rollover_cap IS NULL OR rollover_cap >= 0),
    cost           INTEGER NOT NULL DEFAULT 1 CHECK (cost >= 1),
    lease_ttl_secs INTEGER NOT NULL DEFAULT 300 CHECK (lease_ttl_secs >= 1),
    permissions    TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(permissions)),
    updated_at     INTEGER NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- Subjects (written by the billing sync seam, FB-70, and administrators)
-- ---------------------------------------------------------------------------

-- The current subscription of a subject. Entitled while active, past_due or
-- trialing before `trial_ends_at`; otherwise the default plan applies.
-- `cycle_anchor`: monthly quota windows start on this day and time of the
-- month (NULL: calendar months, UTC). `source_event_at` orders billing events
-- so an older event never overwrites a newer one.
CREATE TABLE _forgebase_subscriptions (
    subject              TEXT    NOT NULL PRIMARY KEY CHECK (length(subject) BETWEEN 1 AND 200),
    plan_id              TEXT    NOT NULL REFERENCES _forgebase_plans(id),
    status               TEXT    NOT NULL
                         CHECK (status IN ('active', 'trialing', 'past_due', 'paused', 'canceled', 'expired')),
    trial_ends_at        INTEGER,
    current_period_start INTEGER,
    current_period_end   INTEGER,
    cycle_anchor         INTEGER,
    cancel_at            INTEGER,
    external_ref         TEXT,
    source_event_at      INTEGER,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
) STRICT;
CREATE INDEX _forgebase_subscriptions_trials ON _forgebase_subscriptions (trial_ends_at) WHERE status = 'trialing';

CREATE TABLE _forgebase_subscription_addons (
    subject    TEXT    NOT NULL,
    addon_id   TEXT    NOT NULL REFERENCES _forgebase_addons(id) ON DELETE CASCADE,
    quantity   INTEGER NOT NULL CHECK (quantity >= 1),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (subject, addon_id)
) STRICT;

-- Billing events already applied (idempotent sync).
CREATE TABLE _forgebase_billing_sync_events (
    event_id   TEXT    NOT NULL PRIMARY KEY CHECK (length(event_id) BETWEEN 1 AND 256),
    subject    TEXT    NOT NULL,
    outcome    TEXT    NOT NULL CHECK (json_valid(outcome)),
    applied_at INTEGER NOT NULL
) STRICT;

-- Usage packs a subject holds; `remaining` is spent oldest expiry first.
CREATE TABLE _forgebase_pack_grants (
    id              TEXT    NOT NULL PRIMARY KEY,
    subject         TEXT    NOT NULL,
    pack_id         TEXT    REFERENCES _forgebase_usage_packs(id) ON DELETE SET NULL,
    feature_key     TEXT    NOT NULL REFERENCES _forgebase_features(key) ON DELETE CASCADE,
    units           INTEGER NOT NULL CHECK (units >= 1),
    remaining       INTEGER NOT NULL CHECK (remaining >= 0 AND remaining <= units),
    expires_at      INTEGER,
    idempotency_key TEXT    CHECK (idempotency_key IS NULL OR length(idempotency_key) BETWEEN 1 AND 256),
    created_by      TEXT    NOT NULL,
    created_at      INTEGER NOT NULL
) STRICT;
CREATE INDEX _forgebase_pack_grants_live ON _forgebase_pack_grants (feature_key, subject, expires_at) WHERE remaining > 0;
CREATE UNIQUE INDEX _forgebase_pack_grants_idem ON _forgebase_pack_grants (subject, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

-- Individual exceptions. `subject` NULL with a `member`: that user in every
-- subject; `member` NULL: the whole subject; both: that member inside that
-- subject. `effect` deny always wins; allow enables the feature and, with
-- `limit_value` set, replaces its limit.
CREATE TABLE _forgebase_entitlement_overrides (
    id          TEXT    NOT NULL PRIMARY KEY,
    subject     TEXT,
    member      TEXT,
    feature_key TEXT    NOT NULL REFERENCES _forgebase_features(key) ON DELETE CASCADE,
    effect      TEXT    NOT NULL CHECK (effect IN ('allow', 'deny')),
    limit_value INTEGER CHECK (limit_value IS NULL OR limit_value >= 0),
    period      TEXT    CHECK (period IS NULL OR period IN ('none', 'hour', 'day', 'month', 'lifetime', 'rolling')),
    window_secs INTEGER CHECK (window_secs IS NULL OR window_secs >= 1),
    expires_at  INTEGER,
    reason      TEXT    NOT NULL DEFAULT '',
    created_by  TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    CHECK (subject IS NOT NULL OR member IS NOT NULL)
) STRICT;
CREATE INDEX _forgebase_entitlement_overrides_subject ON _forgebase_entitlement_overrides (subject, feature_key);

-- Enterprise contract limits: replace the plan's grant of a feature for a
-- subject (`limit_value` NULL = unlimited).
CREATE TABLE _forgebase_custom_limits (
    subject     TEXT    NOT NULL,
    feature_key TEXT    NOT NULL REFERENCES _forgebase_features(key) ON DELETE CASCADE,
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    limit_value INTEGER CHECK (limit_value IS NULL OR limit_value >= 0),
    period      TEXT    CHECK (period IS NULL OR period IN ('none', 'hour', 'day', 'month', 'lifetime', 'rolling')),
    window_secs INTEGER CHECK (window_secs IS NULL OR window_secs >= 1),
    reason      TEXT    NOT NULL DEFAULT '',
    created_by  TEXT    NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (subject, feature_key)
) STRICT;

-- ---------------------------------------------------------------------------
-- Usage
-- ---------------------------------------------------------------------------

-- One row per (feature, subject, scope key, window). Calendar periods and
-- lifetime (window_start 0) and gauges (window_start 0, `used` = level).
-- `limit_value` is the limit in force when the row last changed (rollover
-- reads it); `carried_in` units rolled over from the previous window;
-- `alert_level` the highest alert threshold already raised.
CREATE TABLE _forgebase_usage_counters (
    feature_key  TEXT    NOT NULL,
    subject      TEXT    NOT NULL,
    scope_key    TEXT    NOT NULL DEFAULT '',
    window_start INTEGER NOT NULL,
    window_end   INTEGER NOT NULL,
    used         INTEGER NOT NULL DEFAULT 0,
    reserved     INTEGER NOT NULL DEFAULT 0 CHECK (reserved >= 0),
    carried_in   INTEGER NOT NULL DEFAULT 0 CHECK (carried_in >= 0),
    limit_value  INTEGER,
    alert_level  INTEGER NOT NULL DEFAULT 0,
    updated_at   INTEGER NOT NULL,
    PRIMARY KEY (feature_key, subject, scope_key, window_start)
) STRICT;
CREATE INDEX _forgebase_usage_counters_end ON _forgebase_usage_counters (window_end);
CREATE INDEX _forgebase_usage_counters_subject ON _forgebase_usage_counters (subject, feature_key);

-- Held reservations count against the quota until committed, released or
-- expired. `pack_units` came from usage packs (`packs`: [[grant id, units]]),
-- `overage_units` beyond the limit of a soft quota.
CREATE TABLE _forgebase_usage_reservations (
    id              TEXT    NOT NULL PRIMARY KEY,
    feature_key     TEXT    NOT NULL,
    subject         TEXT    NOT NULL,
    scope_key       TEXT    NOT NULL DEFAULT '',
    window_start    INTEGER NOT NULL,
    units           INTEGER NOT NULL CHECK (units >= 1),
    pack_units      INTEGER NOT NULL DEFAULT 0 CHECK (pack_units >= 0),
    overage_units   INTEGER NOT NULL DEFAULT 0 CHECK (overage_units >= 0),
    packs           TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(packs)),
    status          TEXT    NOT NULL CHECK (status IN ('held', 'committed', 'released', 'expired')),
    committed_units INTEGER,
    expires_at      INTEGER NOT NULL,
    idempotency_key TEXT    NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 256),
    actor           TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    settled_at      INTEGER,
    UNIQUE (subject, idempotency_key)
) STRICT;
CREATE INDEX _forgebase_usage_reservations_held ON _forgebase_usage_reservations (expires_at) WHERE status = 'held';
CREATE INDEX _forgebase_usage_reservations_key ON _forgebase_usage_reservations (feature_key, subject, scope_key)
    WHERE status = 'held';

-- Immutable usage records (FB-70 bills from them; `seq` is its cursor).
-- `units` = in-window units + `pack_units`; `overage_units` is part of the
-- in-window units. Gauge adjustments may be negative.
CREATE TABLE _forgebase_usage_events (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    id              TEXT    NOT NULL UNIQUE,
    at              INTEGER NOT NULL,
    subject         TEXT    NOT NULL,
    feature_key     TEXT    NOT NULL,
    scope_key       TEXT    NOT NULL DEFAULT '',
    units           INTEGER NOT NULL,
    pack_units      INTEGER NOT NULL DEFAULT 0,
    overage_units   INTEGER NOT NULL DEFAULT 0,
    window_start    INTEGER NOT NULL,
    source          TEXT    NOT NULL CHECK (source IN ('consume', 'reservation', 'gauge')),
    reservation_id  TEXT,
    idempotency_key TEXT    NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 300),
    actor           TEXT    NOT NULL,
    request_id      TEXT,
    metadata        TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(metadata)),
    result          TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(result)),
    UNIQUE (subject, idempotency_key)
) STRICT;
CREATE INDEX _forgebase_usage_events_window ON _forgebase_usage_events (feature_key, subject, scope_key, at);
CREATE INDEX _forgebase_usage_events_subject ON _forgebase_usage_events (subject, seq);

CREATE TRIGGER _forgebase_usage_events_no_update BEFORE UPDATE ON _forgebase_usage_events
BEGIN
    SELECT RAISE(ABORT, 'usage events are immutable');
END;
CREATE TRIGGER _forgebase_usage_events_no_delete BEFORE DELETE ON _forgebase_usage_events
BEGIN
    SELECT RAISE(ABORT, 'usage events are immutable');
END;

-- Concurrency leases: active while `released_at` is NULL and `expires_at` is
-- in the future.
CREATE TABLE _forgebase_quota_leases (
    id              TEXT    NOT NULL PRIMARY KEY,
    feature_key     TEXT    NOT NULL,
    subject         TEXT    NOT NULL,
    scope_key       TEXT    NOT NULL DEFAULT '',
    expires_at      INTEGER NOT NULL,
    idempotency_key TEXT    NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 256),
    actor           TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    released_at     INTEGER,
    UNIQUE (subject, idempotency_key)
) STRICT;
CREATE INDEX _forgebase_quota_leases_active ON _forgebase_quota_leases (feature_key, subject, scope_key, expires_at)
    WHERE released_at IS NULL;

-- Usage alerts at 70 / 90 / 100 % of a window's allowance, raised once per
-- threshold and window (`alert_level` of the counter) and delivered by the
-- `entitlements.alert` job (`delivered_at`).
CREATE TABLE _forgebase_usage_alerts (
    id           TEXT    NOT NULL PRIMARY KEY,
    at           INTEGER NOT NULL,
    subject      TEXT    NOT NULL,
    feature_key  TEXT    NOT NULL,
    scope_key    TEXT    NOT NULL DEFAULT '',
    window_start INTEGER NOT NULL,
    threshold    INTEGER NOT NULL CHECK (threshold IN (70, 90, 100)),
    used         INTEGER NOT NULL,
    limit_value  INTEGER NOT NULL,
    delivered_at INTEGER
) STRICT;
CREATE INDEX _forgebase_usage_alerts_subject ON _forgebase_usage_alerts (subject, at);
CREATE INDEX _forgebase_usage_alerts_pending ON _forgebase_usage_alerts (at) WHERE delivered_at IS NULL;

-- Append-only audit of catalog and subject changes, alert deliveries and
-- period resets.
CREATE TABLE _forgebase_entitlement_audit (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    at      INTEGER NOT NULL,
    actor   TEXT    NOT NULL,
    action  TEXT    NOT NULL,
    subject TEXT,
    detail  TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(detail))
) STRICT;
CREATE INDEX _forgebase_entitlement_audit_subject ON _forgebase_entitlement_audit (subject, id);

CREATE TRIGGER _forgebase_entitlement_audit_no_update BEFORE UPDATE ON _forgebase_entitlement_audit
BEGIN
    SELECT RAISE(ABORT, 'the entitlement audit log is append-only');
END;
CREATE TRIGGER _forgebase_entitlement_audit_no_delete BEFORE DELETE ON _forgebase_entitlement_audit
BEGIN
    SELECT RAISE(ABORT, 'the entitlement audit log is append-only');
END;
