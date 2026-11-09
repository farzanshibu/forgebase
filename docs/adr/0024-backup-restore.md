# ADR-0024: Backup, restore, snapshots, export/import and `doctor`

- Status: accepted
- Date: 2026-11-10
- Work package: FB-83 (MDF-41)
- Scope: `crates/forgebase-backup`, `Database::snapshot_to`, CLI `backup`/`restore`/`export`/`import`/`doctor`,
  `/api/v1/system/backups`.
- Builds on: ADR-0001 (data directory layout, single writer, lock file), ADR-0018 (content-addressed blobs and
  the reconciler), ADR-0012 (indexes are derived data).

## Decisions

1. **A backup is a directory**, not an archive: `MANIFEST.json`, `forgebase.db`, `storage/…`, `indexes/…`,
   optionally `auth/master.key`. The manifest lists every file with size and SHA-256 and is written last.
   The directory is built as `<dest>.partial` and renamed, so a crash never yields a complete-looking backup.
   No new dependencies: tar/zstd can wrap the directory outside ForgeBase.
2. **Online consistency comes from SQLite.** The database is copied with `VACUUM INTO` on the writer connection
   of the running server, or on a read-only handle opened by the CLI (so `forgebase backup` works against a
   running server without the lock). Blobs are copied afterwards. They are content-addressed and immutable, so a
   blob created after the snapshot is only an orphan in the copy and the reconciler removes it; a blob deleted
   in between shows up as missing in `verify`/`doctor`. S3-stored objects are not copied.
3. **The master key stays out of backups by default** (`--include-master-key` opts in). The manifest keeps a
   16-hex-character fingerprint; restore refuses a different key and refuses to continue without one unless
   `--master-key FILE` or `--allow-missing-key` says so.
4. **Restore is offline and reversible.** It holds the data-directory lock, verifies, stages and re-hashes,
   then swaps with a journal (`.restore-journal.json`). The replaced data is moved to
   `backups/pre-restore-<time>/`, never deleted. A crash before the commit mark is rolled back by the next
   restore or `restore --recover`; the database file is moved in last. Restore is not exposed over HTTP because
   it replaces files the running server has open.
5. **Snapshots** are backups under `backups/snapshots/<UTC time>` with retention (`keep_last` always kept,
   `max_age_days` for the rest). A restore argument that is a time picks the newest snapshot at or before it
   (point-in-time restore at snapshot granularity). Taking snapshots on a schedule is left to cron or a systemd
   timer; the REST endpoint and the CLI are the triggers.
6. **Export/import is a logical bundle**: schema JSON and NDJSON per collection with checksums, relation targets
   first. Collection, field and record ids are preserved; timestamps are reassigned by the data service. Auth
   collections export their schema only. Import needs the server stopped because it changes DDL the server's
   catalog caches in memory. The configuration exports as TOML and is validated by the real config loader.
7. **`doctor` stays read-only** and gains checks for the master key, interrupted restores, snapshots (`--deep`
   verifies the newest), staging leftovers, vector index files, WAL size and blob/metadata consistency.

## Consequences

- No migration, no new third-party dependency. `Database::snapshot_to` and `forgebase-backup` are add-only.
- Backup size equals data size (no incremental or compressed backups yet).
- Point-in-time recovery between snapshots (WAL archiving) is not provided.
