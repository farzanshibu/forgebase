# Windows smoke test of an extracted engine (the same checks as smoke.sh, over
# loopback TCP as forgebase-pgengine uses on Windows). pg_ctl starts the
# server with a restricted token, so it also runs from an administrator
# account (postgres.exe alone refuses one).
#
#   pwsh deploy/postgres-engine/smoke-windows.ps1 -Engine <dir> -Work <dir>
param([Parameter(Mandatory)][string]$Engine, [Parameter(Mandatory)][string]$Work, [int]$Port = 54329)
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true
$Bin = (Resolve-Path "$Engine\bin").Path
New-Item -ItemType Directory -Force -Path $Work | Out-Null
$Data = "$Work\data"; $Log = "$Work\primary.log"; $Standby = "$Work\standby"
$Port2 = $Port + 1
function Q([string]$Sql, [int]$P = $Port) {
  $r = & "$Bin\psql.exe" -X -q -A -t -v ON_ERROR_STOP=1 -h 127.0.0.1 -p $P -U postgres -d postgres -c $Sql
  if ($LASTEXITCODE) { throw "psql failed: $Sql" }
  return ($r -join "`n").Trim()
}
function Check([bool]$Ok, [string]$What) { if (-not $Ok) { throw "SMOKE FAIL: $What" } else { Write-Host "  ok: $What" } }

Write-Host "--- programs run"
foreach ($p in Get-ChildItem "$Bin\*.exe") { Write-Host "  $($p.Name): $(& $p.FullName --version)" }

Write-Host "--- initdb, start"
$env:TZ = "UTC"
& "$Bin\initdb.exe" -D $Data -U postgres --auth=trust --encoding=UTF8 --locale-provider=builtin `
  --builtin-locale=C.UTF-8 --locale=C --no-instructions | Out-Null
@"
listen_addresses = '127.0.0.1'
port = $Port
shared_preload_libraries = 'pg_stat_statements,pg_cron'
cron.database_name = 'postgres'
cron.use_background_workers = on
max_worker_processes = 16
wal_level = logical
max_wal_senders = 16
max_replication_slots = 16
max_prepared_transactions = 16
fsync = off
"@ | Add-Content "$Data\postgresql.conf"
& "$Bin\pg_ctl.exe" -D $Data -l $Log -w -t 60 start | Out-Null

try {
  Write-Host "--- extensions"
  foreach ($e in "vector", "pg_trgm", "pgmq", "pg_cron", "citext", "pgcrypto", "unaccent", "fuzzystrmatch", "btree_gin",
                 "btree_gist", "hstore", "ltree", "intarray", "tablefunc", "pg_stat_statements", "amcheck") {
    Q "CREATE EXTENSION `"$e`"" | Out-Null
  }
  Q "SELECT string_agg(extname || ' ' || extversion, ', ' ORDER BY extname) FROM pg_extension"
  Q "CREATE TABLE items (id bigint PRIMARY KEY, embedding vector(3)); INSERT INTO items VALUES (1, '[1,2,3]'), (2, '[4,5,6]'), (3, '[7,8,9]'); CREATE INDEX ON items USING hnsw (embedding vector_l2_ops)" | Out-Null
  Check ((Q "SET enable_seqscan = off; SELECT id FROM items ORDER BY embedding <-> '[3,1,2]' LIMIT 1") -eq "1") "vector HNSW query"
  Check ((Q "SELECT similarity('forgebase', 'forgebas') > 0.5") -eq "t") "pg_trgm"
  Check ((Q "SELECT encode(digest('x', 'sha256'), 'hex')").StartsWith("2d711642")) "pgcrypto"
  Q "SELECT pgmq.create('smoke')" | Out-Null
  Q "SELECT pgmq.send('smoke', '{`"a`": 1}')" | Out-Null
  Check ((Q "SELECT message->>'a' FROM pgmq.read('smoke', 30, 1)") -eq "1") "pgmq send/read"
  Q "CREATE TABLE cron_hits (at timestamptz)" | Out-Null
  Q "SELECT cron.schedule('smoke', '2 seconds', 'INSERT INTO cron_hits VALUES (now())')" | Out-Null
  $fired = $false
  for ($i = 0; $i -lt 70 -and -not $fired; $i++) { Start-Sleep 1; $fired = (Q "SELECT count(*) > 0 FROM cron_hits") -eq "t" }
  Check $fired "pg_cron job fired"
  Write-Host "  ssl_library = $(Q 'SHOW ssl_library')"
  Start-Sleep 5
  $pm = [int](Get-Content "$Data\postmaster.pid" -TotalCount 1)
  $procs = Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -eq $pm -or $_.ParentProcessId -eq $pm }
  $ws = ($procs | Measure-Object -Property WorkingSetSize -Sum).Sum / 1KB
  Write-Host "  idle working set: $([int]$ws) KiB in $($procs.Count) processes"
  & "$Bin\pg_dump.exe" -h 127.0.0.1 -p $Port -U postgres -d postgres -Fc -f "$Work\dump"
  Check ((& "$Bin\pg_restore.exe" -l "$Work\dump") -match "TABLE public items").Count -gt 0 "pg_dump/pg_restore"
  & "$Bin\pg_basebackup.exe" -h 127.0.0.1 -p $Port -U postgres -D $Standby -R -X stream -c fast
  & "$Bin\pg_verifybackup.exe" $Standby | Out-Null
  & "$Bin\pg_ctl.exe" -D $Standby -l "$Work\standby.log" -o "-p $Port2" -w -t 60 start | Out-Null
  Q "INSERT INTO items VALUES (4, '[1,1,1]')" | Out-Null
  $ok = $false
  for ($i = 0; $i -lt 30 -and -not $ok; $i++) { Start-Sleep 1; $ok = (Q "SELECT count(*) FROM items" $Port2) -eq "4" }
  Check $ok "standby replays"
  Check ((Q "SELECT pg_is_in_recovery()" $Port2) -eq "t") "standby is in recovery"
  & "$Bin\pg_ctl.exe" -D $Standby -m fast -w stop | Out-Null
} finally {
  & "$Bin\pg_ctl.exe" -D $Data -m fast -w stop | Out-Null
}
Write-Host "`nSMOKE OK"
