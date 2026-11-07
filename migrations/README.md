# Migrations

`system/` holds ForgeBase's own schema migrations. They are embedded in the
binary at build time by `crates/forgebase-migrations` and applied at startup
(`forgebase serve`, `forgebase init`) or explicitly (`forgebase migrate`).

Rules:

1. File name `NNNN_snake_case_name.sql`; `NNNN` is the version, strictly increasing.
2. Register every file in `SYSTEM_MIGRATIONS` (`crates/forgebase-migrations/src/lib.rs`);
   a test fails if a file is not registered.
3. **Never edit a migration that has shipped.** Its SHA-256 checksum is stored in
   `_forgebase_migrations`; a changed file makes the server refuse to start.
   Fix forward with a new migration.
4. Each migration runs inside one `BEGIN IMMEDIATE` transaction together with its
   bookkeeping row, followed by `PRAGMA foreign_key_check`. Statements that cannot
   run in a transaction (`VACUUM`, `PRAGMA journal_mode`, ...) are not allowed.
5. Migrations are forward-only. Rollback = restore the automatic snapshot that
   is written to `<data_dir>/backups/` before pending migrations are applied to an
   existing database.
