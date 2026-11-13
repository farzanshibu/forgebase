# Smoke-tests the standard Windows build (embedded PostgreSQL) under a
# restricted token: postgres.exe refuses to run with administrator rights,
# and GitHub's Windows runner account is a full administrator.
#
#   pwsh deploy/smoke-windows-restricted.ps1 -Exe <forgebase.exe> -Work <dir> [-Direct]
#
# `runas /trustlevel:0x20000` starts a process with a basic-user token
# (Administrators deny-only, as pg_ctl does for postgres). runas neither waits
# nor passes output, so a batch file writes a log and a done marker. -Direct
# runs the same batch in this process (for a non-administrator shell).
param(
  [Parameter(Mandatory)][string]$Exe,
  [Parameter(Mandatory)][string]$Work,
  [int]$TimeoutSeconds = 600,
  [int]$Port = 8090,
  [switch]$Direct
)
$ErrorActionPreference = "Stop"
$Exe = (Resolve-Path $Exe).Path
if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
New-Item -ItemType Directory -Force -Path $Work | Out-Null
$Work = (Resolve-Path $Work).Path
if ("$Exe$Work" -match '[\s&^%!"]') { throw "paths must not contain spaces or cmd metacharacters: $Exe, $Work" }
$log = "$Work\smoke.log"; $done = "$Work\done.txt"; $bat = "$Work\smoke.cmd"

# Each step appends to the log; the first failure records its name and stops.
@"
@echo off
setlocal
cd /d $Work
echo --- token: Administrators must be deny-only or absent>> "$log"
whoami /groups | findstr /i /c:"Administrators" /c:"Mandatory Label" >> "$log" 2>&1
echo --- init>> "$log"
"$Exe" init smoke --no-admin >> "$log" 2>&1 || (echo init> "$done" & exit /b 1)
cd smoke
echo --- status>> "$log"
"$Exe" status >> "$log" 2>&1 || (echo status> "$done" & exit /b 1)
echo --- doctor (informational)>> "$log"
"$Exe" doctor >> "$log" 2>&1
echo --- db status --json>> "$log"
"$Exe" db status --json > "$Work\db.json" 2>> "$log" || (echo db-status> "$done" & exit /b 1)
type "$Work\db.json" >> "$log"
rem serve until /health/ready, kill it hard (leaving its postmaster), then
rem serve again: the second start must stop the leftover engine itself.
for %%R in (1 2) do (
  echo --- serve, run %%R>> "$log"
  start "" /b cmd /c ""$Exe" serve > "$Work\serve%%R.log" 2>&1"
  set ready=
  for /l %%i in (1,1,90) do if not defined ready (
    curl -s -f -m 2 http://127.0.0.1:$Port/health/ready > "$Work\ready%%R.json" 2>nul && set ready=1
    if not defined ready ping -n 2 127.0.0.1 > nul
  )
  if not defined ready (type "$Work\serve%%R.log" >> "$log" & echo serve-%%R> "$done" & exit /b 1)
  type "$Work\ready%%R.json" >> "$log"
  echo.>> "$log"
  taskkill /F /IM forgebase.exe > nul 2>&1
  ping -n 3 127.0.0.1 > nul
)
findstr /c:"left running by an earlier ForgeBase" "$Work\serve2.log" >> "$log" || (echo stale-recovery> "$done" & exit /b 1)
for /d %%E in (forgebase-data\postgres\engine\*) do "%%E\bin\pg_ctl.exe" stop -D forgebase-data\postgres\data -m fast -w >> "$log" 2>&1
echo ok> "$done"
"@ | Set-Content -Encoding ascii $bat

if ($Direct) {
  cmd /c $bat
} else {
  runas /trustlevel:0x20000 $bat
  if ($LASTEXITCODE) { throw "runas failed with exit code $LASTEXITCODE" }
}
$until = (Get-Date).AddSeconds($TimeoutSeconds)
while (-not (Test-Path $done) -and (Get-Date) -lt $until) { Start-Sleep 2 }
Start-Sleep 1
if (Test-Path $log) { Get-Content $log | Write-Host }
if (-not (Test-Path $done)) {
  Get-CimInstance Win32_Process | Where-Object { $_.Name -in "forgebase.exe", "postgres.exe", "cmd.exe" } |
    Format-Table ProcessId, ParentProcessId, Name, CommandLine -AutoSize | Out-String -Width 300 | Write-Host
  throw "the smoke batch did not finish in $TimeoutSeconds s"
}
$result = (Get-Content $done -Raw).Trim()
if ($result -ne "ok") { throw "SMOKE FAIL at: $result" }
if (-not (Select-String -Quiet -Path "$Work\db.json" -Pattern '"postgres"')) { throw "SMOKE FAIL: db status does not report postgres" }
Write-Host "`nSMOKE OK (standard Windows build, restricted token)"
