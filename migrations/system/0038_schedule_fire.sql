-- FB-165 (ADR-0029 §1.7): schedules on pg_cron.
--
-- PostgreSQL's twin (migrations/postgres/0038_schedule_fire.sql) creates
-- `forgebase.fire_schedule(id)`, the function pg_cron calls to fire a
-- schedule. SQLite has no pg_cron: the Rust tick (JobService::
-- fire_due_schedules) fires every schedule, so there is nothing to create
-- here. This file exists so both backends carry the same version.
SELECT 1;
