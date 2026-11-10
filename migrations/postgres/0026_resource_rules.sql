-- PostgreSQL twin of migrations/system/0026_resource_rules.sql (FB-134,
-- ADR-0028 addendum "FB-134"): rules of named engine resources. One row per
-- (kind, name, action); `name` '*' holds the rules of every resource of the
-- kind that has no rules of its own; `rule` NULL locks the action, '' opens it
-- to everyone.
CREATE TABLE _forgebase_resource_rules (
    kind       text   NOT NULL,
    name       text   NOT NULL,
    action     text   NOT NULL,
    rule       text,
    updated_by text   NOT NULL,
    updated_at bigint NOT NULL,
    PRIMARY KEY (kind, name, action)
);
