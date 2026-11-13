-- ADR-0035: the alerting engine, notifiers and SLOs.
--
-- `_forgebase_alert_rules` holds the custom rules created through the API
-- (rules from `[[alerts.rules]]` and the built-in pack live in code); a
-- built-in rule switched on or off through the API has a row in
-- `_forgebase_alert_rule_settings`. `_forgebase_alerts` holds one row per
-- alert instance (rule + label set, `fingerprint`): open while `pending` or
-- `firing` (at most one open row per fingerprint), kept as history once
-- `resolved`; `_forgebase_alert_events` records each state change.
-- `_forgebase_alert_silences` mute notifications of matching alerts until
-- `ends_at`. `_forgebase_alert_notifiers` are the destinations (webhook,
-- Slack, email, PagerDuty, queue topic) with their secret sealed by the
-- keyring; `_forgebase_alert_groups` remembers what each notifier was last
-- told about each group (dedup, repeat interval); `_forgebase_alert_notifications`
-- is the delivery outbox with retries. `_forgebase_slos` defines service
-- level objectives; `_forgebase_slo_minutes` keeps their per-minute good and
-- total counts (pruned after the SLO window). Times are Unix milliseconds.
CREATE TABLE _forgebase_alert_rules (
    id          TEXT    NOT NULL PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE,
    expr        TEXT    NOT NULL,
    op          TEXT    NOT NULL CHECK (op IN ('>', '>=', '<', '<=', '==', '!=')),
    threshold   REAL    NOT NULL,
    for_ms      INTEGER NOT NULL DEFAULT 0,
    severity    TEXT    NOT NULL CHECK (severity IN ('info', 'warning', 'critical')),
    labels      TEXT    NOT NULL DEFAULT '{}',
    annotations TEXT    NOT NULL DEFAULT '{}',
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_by  TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;

CREATE TABLE _forgebase_alert_rule_settings (
    rule_id    TEXT    NOT NULL PRIMARY KEY,
    enabled    INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    updated_by TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE _forgebase_alerts (
    id               TEXT    NOT NULL PRIMARY KEY,
    fingerprint      TEXT    NOT NULL,
    rule_id          TEXT    NOT NULL,
    name             TEXT    NOT NULL,
    severity         TEXT    NOT NULL,
    state            TEXT    NOT NULL CHECK (state IN ('pending', 'firing', 'resolved')),
    labels           TEXT    NOT NULL DEFAULT '{}',
    annotations      TEXT    NOT NULL DEFAULT '{}',
    value            REAL,
    active_at        INTEGER NOT NULL,
    fired_at         INTEGER,
    resolved_at      INTEGER,
    last_eval_at     INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX _forgebase_alerts_open ON _forgebase_alerts (fingerprint) WHERE state IN ('pending', 'firing');
CREATE INDEX _forgebase_alerts_state ON _forgebase_alerts (state, active_at);
CREATE INDEX _forgebase_alerts_resolved ON _forgebase_alerts (resolved_at);

CREATE TABLE _forgebase_alert_events (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    alert_id TEXT    NOT NULL REFERENCES _forgebase_alerts (id) ON DELETE CASCADE,
    at       INTEGER NOT NULL,
    state    TEXT    NOT NULL CHECK (state IN ('pending', 'firing', 'resolved')),
    value    REAL
) STRICT;
CREATE INDEX _forgebase_alert_events_alert ON _forgebase_alert_events (alert_id, id);

CREATE TABLE _forgebase_alert_silences (
    id         TEXT    NOT NULL PRIMARY KEY,
    matchers   TEXT    NOT NULL,
    starts_at  INTEGER NOT NULL,
    ends_at    INTEGER NOT NULL,
    comment    TEXT    NOT NULL DEFAULT '',
    created_by TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX _forgebase_alert_silences_ends ON _forgebase_alert_silences (ends_at);

CREATE TABLE _forgebase_alert_notifiers (
    id              TEXT    NOT NULL PRIMARY KEY,
    name            TEXT    NOT NULL UNIQUE,
    kind            TEXT    NOT NULL CHECK (kind IN ('webhook', 'slack', 'email', 'pagerduty', 'queue')),
    config          TEXT    NOT NULL DEFAULT '{}',
    secret          BLOB,
    enabled         INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    send_resolved   INTEGER NOT NULL DEFAULT 1 CHECK (send_resolved IN (0, 1)),
    last_success_at INTEGER,
    last_failure_at INTEGER,
    last_error      TEXT,
    created_by      TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
) STRICT;

CREATE TABLE _forgebase_alert_groups (
    notifier_id  TEXT    NOT NULL REFERENCES _forgebase_alert_notifiers (id) ON DELETE CASCADE,
    group_key    TEXT    NOT NULL,
    firing       TEXT    NOT NULL DEFAULT '[]',
    last_sent_at INTEGER NOT NULL,
    PRIMARY KEY (notifier_id, group_key)
) STRICT;

CREATE TABLE _forgebase_alert_notifications (
    id              TEXT    NOT NULL PRIMARY KEY,
    notifier_id     TEXT    NOT NULL REFERENCES _forgebase_alert_notifiers (id) ON DELETE CASCADE,
    group_key       TEXT    NOT NULL,
    event           TEXT    NOT NULL CHECK (event IN ('firing', 'resolved', 'test')),
    status          TEXT    NOT NULL CHECK (status IN ('pending', 'sent', 'failed')),
    payload         TEXT    NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL,
    last_status     INTEGER,
    last_error      TEXT,
    sent_at         INTEGER,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
) STRICT;
CREATE INDEX _forgebase_alert_notifications_due ON _forgebase_alert_notifications (status, next_attempt_at);
CREATE INDEX _forgebase_alert_notifications_notifier ON _forgebase_alert_notifications (notifier_id, created_at);

CREATE TABLE _forgebase_slos (
    id                   TEXT    NOT NULL PRIMARY KEY,
    name                 TEXT    NOT NULL UNIQUE,
    description          TEXT    NOT NULL DEFAULT '',
    kind                 TEXT    NOT NULL CHECK (kind IN ('availability', 'latency')),
    objective            REAL    NOT NULL,
    window_ms            INTEGER NOT NULL,
    route_prefix         TEXT,
    route_regex          TEXT,
    methods              TEXT    NOT NULL DEFAULT '[]',
    latency_threshold_ms INTEGER,
    alerting             INTEGER NOT NULL DEFAULT 1 CHECK (alerting IN (0, 1)),
    labels               TEXT    NOT NULL DEFAULT '{}',
    created_by           TEXT    NOT NULL,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
) STRICT;

CREATE TABLE _forgebase_slo_minutes (
    slo_id TEXT    NOT NULL REFERENCES _forgebase_slos (id) ON DELETE CASCADE,
    minute INTEGER NOT NULL,
    total  INTEGER NOT NULL,
    good   INTEGER NOT NULL,
    PRIMARY KEY (slo_id, minute)
) STRICT;
