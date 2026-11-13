# Builds the Windows engine archive (x86_64-pc-windows-msvc): pinned sources ->
# OpenSSL, zlib, lz4, zstd (DLLs, next to the executables in bin\) ->
# PostgreSQL with meson + MSVC -> the contrib list, pgvector and pg_cron with
# their Makefile.win, pgmq's SQL files -> pruned tree -> .tar.zst archive.
#
#   pwsh deploy/postgres-engine/build-windows.ps1 -Work C:\engine -Out C:\engine\out
#
# Needs: Visual Studio 2022 with the C++ tools, Strawberry Perl (perl and
# pkg-config), Python 3.11+, CMake, win_flex/win_bison (winflexbison3), and
# meson + ninja (pip). The GitHub windows-2022 image has all but the last two.
# Windows psql has no line editing (no libedit for MSVC).
param(
  [string]$Target = "x86_64-pc-windows-msvc",
  [string]$Work = "$env:RUNNER_TEMP\engine",
  [string]$Out = "$Work\out"
)
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true
$Here = $PSScriptRoot
$Root = (Resolve-Path "$Here\..\..").Path
$env:FORGEBASE_PG_ENGINE_LOCK = "$Root\engine.lock"
function Lock([string]$Key) { (python "$Here\engine.py" get $Key).Trim() }
function Log([string]$Msg) { Write-Host "`n=== $Msg [$((Get-Date).ToUniversalTime().ToString('HH:mm:ss'))]" }
function Unpack([string]$Name) {
  $ver = Lock "sources.$Name.version"
  $file = Get-ChildItem "$Src\$Name-$ver.tar.*" | Select-Object -First 1
  $dir = if ($Name -eq "postgresql") { "postgresql-$ver" } else { try { Lock "sources.$Name.dir" } catch { "$Name-$ver" } }
  if (-not (Test-Path "$Build\$dir")) {
    Log "unpack $($file.Name)"
    # An extraction cut short must not leave a half-filled directory that the
    # Test-Path above would take for a finished one on the next run.
    # .bz2 goes through Python: tar.exe + an external bzip2 deadlocks on the runner.
    try {
      if ($file.Extension -eq ".bz2") { Run "untar $Name" 10 "python" @("$Here\untar.py", $file.FullName, $Build) }
      else { Run "tar $Name" 10 "tar" @("-C", $Build, "-xf", $file.FullName) }
    }
    catch { Remove-Item -Recurse -Force -ErrorAction SilentlyContinue "$Build\$dir"; throw }
  }
  return "$Build\$dir"
}

# Runs a program with stdin closed and a time limit. A step that hangs prints
# the process tree it is stuck in and fails after $Minutes, instead of holding
# the job until its 120-minute cap (engine-pg18.6-r1 hung silently for 111
# minutes between "PostgreSQL (meson)" and the job cancel).
$EmptyIn = Join-Path ([IO.Path]::GetTempPath()) "forgebase-empty-stdin.txt"
Set-Content -Path $EmptyIn -Value "" -NoNewline
function Run([string]$Label, [int]$Minutes, [string]$Exe, [string[]]$Arguments) {
  $quoted = $Arguments | ForEach-Object { if ($_ -match '\s') { "`"$_`"" } else { $_ } }
  Write-Host "  > $Label (limit $Minutes min): $Exe $($quoted -join ' ')"
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe -ArgumentList $quoted -NoNewWindow -PassThru -RedirectStandardInput $EmptyIn
  $null = $p.Handle   # keep the handle so ExitCode is readable after exit
  if (-not $p.WaitForExit($Minutes * 60000)) {
    Write-Host "::error::$Label did not finish in $Minutes minutes; process tree:"
    $all = Get-CimInstance Win32_Process
    $tree = @{ [int]$p.Id = $true }
    do { $grew = $false; foreach ($q in $all) { if ($tree[[int]$q.ParentProcessId] -and -not $tree[[int]$q.ProcessId]) { $tree[[int]$q.ProcessId] = $true; $grew = $true } } } while ($grew)
    $all | Where-Object { $tree[[int]$_.ProcessId] } |
      Select-Object ProcessId, ParentProcessId, Name, @{ n = "Cmd"; e = { "$($_.CommandLine)".Substring(0, [Math]::Min(200, "$($_.CommandLine)".Length)) } } | Format-Table -AutoSize | Out-String -Width 300 | Write-Host
    taskkill /T /F /PID $p.Id | Out-Null
    throw "$Label timed out after $Minutes minutes"
  }
  Write-Host "  < $Label finished in $([int]$sw.Elapsed.TotalSeconds) s, exit $($p.ExitCode)"
  if ($p.ExitCode -ne 0) { throw "$Label failed with exit code $($p.ExitCode)" }
}

$PgVer = Lock "engine.postgres"
$BuildNo = Lock "engine.build"
$Programs = (Lock "contents.programs") -split " "
$Contrib = (Lock "contents.contrib") -split " "
$Src = if ($env:FB_ENGINE_SOURCES) { $env:FB_ENGINE_SOURCES } else { "$Work\src" }
$Build = "$Work\build"
$Deps = "$Work\deps"
$Prefix = "$Work\stage\forgebase-postgres"   # contains "postgres": modules in lib\, data in share\
$Engine = "$Work\engine"
New-Item -ItemType Directory -Force -Path $Src, $Build, $Deps, $Out | Out-Null

Log "MSVC environment"
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
Import-Module "$vs\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
Enter-VsDevShell -VsInstallPath $vs -DevCmdArguments "-arch=x64 -host_arch=x64" -SkipAutomaticLocation | Out-Null
(Get-Command cl.exe).Source

Log "fetch and verify sources"
python "$Here\engine.py" fetch --dest $Src --os windows

if (-not ((Test-Path "$Deps\lib\libssl.lib") -and (Test-Path "$Deps\lib\libcrypto.lib") -and
          (Test-Path "$Deps\bin\libssl-3-x64.dll") -and (Test-Path "$Deps\bin\libcrypto-3-x64.dll"))) {
  Log "OpenSSL (DLLs; no-asm, so no NASM)"
  $d = Unpack "openssl"
  Push-Location $d
  perl Configure VC-WIN64A shared no-asm no-module no-tests no-docs "--prefix=$Deps" "--openssldir=$Deps\ssl"
  nmake /nologo build_libs | Out-Null
  nmake /nologo install_dev | Out-Null
  Pop-Location
}

function CMakeDep([string]$Name, [string]$SubDir, [string[]]$Flags) {
  $d = Unpack $Name
  cmake -S "$d\$SubDir" -B "$d\_b" -G Ninja -DCMAKE_BUILD_TYPE=Release "-DCMAKE_INSTALL_PREFIX=$Deps" @Flags | Out-Null
  cmake --build "$d\_b" | Out-Null
  cmake --install "$d\_b" | Out-Null
}
if (-not (Test-Path "$Deps\lib\zlib.lib") -and -not (Test-Path "$Deps\lib\z.lib")) {
  Log "zlib"; CMakeDep "zlib" "." @("-DZLIB_BUILD_TESTING=OFF", "-DZLIB_BUILD_STATIC=OFF")
}
if (-not (Test-Path "$Deps\lib\lz4.lib")) {
  Log "lz4"; CMakeDep "lz4" "build\cmake" @("-DBUILD_SHARED_LIBS=ON", "-DBUILD_STATIC_LIBS=OFF", "-DLZ4_BUILD_CLI=OFF")
}
if (-not (Test-Path "$Deps\bin\zstd.exe")) {
  Log "zstd (DLL and the CLI used for packing)"
  CMakeDep "zstd" "build\cmake" @("-DZSTD_BUILD_SHARED=ON", "-DZSTD_BUILD_STATIC=ON", "-DZSTD_BUILD_PROGRAMS=ON", "-DZSTD_BUILD_TESTS=OFF")
}
Get-ChildItem "$Deps\bin\*.dll", "$Deps\lib\*.lib" | ForEach-Object { "  $($_.Name)" }

# The pack step runs this zstd (engine.py pack --zstd). engine-pg18.6-r3 reached
# the pack step with no $Deps\bin\zstd.exe (FileNotFoundError) although the zstd
# step had run. Find it wherever CMake left it, or say what is there.
if (-not (Test-Path "$Deps\bin\zstd.exe")) {
  $found = Get-ChildItem -Path $Deps, $Build -Recurse -Filter zstd.exe -ErrorAction SilentlyContinue | Select-Object -First 1
  if (-not $found) {
    Write-Host "::error::zstd.exe not found; zstd-related files:"
    Get-ChildItem -Path $Deps, $Build -Recurse -ErrorAction SilentlyContinue | Where-Object { $_.Name -match '^zstd' -and -not $_.PSIsContainer -and $_.Extension -match 'exe|dll|lib' } | ForEach-Object { "  $($_.FullName)" }
    throw "the zstd CLI was not built"
  }
  Write-Host "  zstd.exe was not installed; using $($found.FullName)"
  New-Item -ItemType Directory -Force -Path "$Deps\bin" | Out-Null
  Copy-Item $found.FullName "$Deps\bin\zstd.exe"
}
& "$Deps\bin\zstd.exe" --version

Log "PostgreSQL $PgVer (meson)"
$pg = Unpack "postgresql"
$env:PKG_CONFIG_PATH = "$Deps\lib\pkgconfig"
$env:CMAKE_PREFIX_PATH = $Deps
$env:OPENSSL_ROOT_DIR = $Deps
$env:PATH = "$Deps\bin;$env:PATH"
Write-Host "  tools: $((Get-Command meson).Source), $((Get-Command ninja).Source), $((Get-Command tar).Source)"
meson --version
if (-not (Test-Path "$pg\_b\build.ninja")) {
  if (Test-Path "$pg\_b") { Remove-Item -Recurse -Force "$pg\_b" }   # a setup that was cut short
  Run "meson setup" 15 "meson" @("setup", "$pg\_b", $pg, "--prefix=$Prefix", "--buildtype=release", "--auto-features=disabled",
    "--wrap-mode=nodownload", "-Dssl=openssl", "-Dzlib=enabled", "-Dlz4=enabled", "-Dzstd=enabled",
    "-Dextra_include_dirs=$Deps\include", "-Dextra_lib_dirs=$Deps\lib")
}
Run "meson compile" 60 "meson" @("compile", "-C", "$pg\_b")
if (Test-Path "$Work\stage") { Remove-Item -Recurse -Force "$Work\stage" }
meson install -C "$pg\_b" --quiet
$Shipped = @{}
# Every installed file, minus contrib modules not in contents.contrib.
$installed = meson introspect "$pg\_b" --installed | ConvertFrom-Json
$contribRoot = (Resolve-Path "$pg").Path + "\contrib\"
$contribBuild = (Resolve-Path "$pg\_b").Path + "\contrib\"
foreach ($p in $installed.PSObject.Properties) {
  # meson may print either slash; compare normalised full paths.
  $from = [IO.Path]::GetFullPath($p.Name)
  $to = [IO.Path]::GetFullPath($p.Value)
  foreach ($root in @($contribRoot, $contribBuild)) {
    if ($from.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
      $c = $from.Substring($root.Length).Split("\")[0]
      if ($Contrib -notcontains $c) { $Shipped[$to] = $false }
    }
  }
}
Write-Host "  contrib files left out: $($Shipped.Count)"

Log "extensions: pgvector, pg_cron (Makefile.win), pgmq (SQL)"
$v = Unpack "pgvector"
Push-Location $v; nmake /nologo /F Makefile.win "PGROOT=$Prefix" OPTFLAGS= ; nmake /nologo /F Makefile.win "PGROOT=$Prefix" install; Pop-Location
$c = Unpack "pg_cron"
Push-Location $c
# Makefile.win links libintl.lib (an NLS build); this engine has no NLS.
$cronLibs = "`"$Prefix\lib\postgres.lib`" `"$Prefix\lib\libpq.lib`" ws2_32.lib"
nmake /nologo /F Makefile.win "PGROOT=$Prefix" "LIBS=$cronLibs"
nmake /nologo /F Makefile.win "PGROOT=$Prefix" "LIBS=$cronLibs" install
Pop-Location
$q = (Unpack "pgmq") + "\pgmq-extension"
$qv = Lock "sources.pgmq.version"
Copy-Item "$q\pgmq.control" "$Prefix\share\extension\"
Copy-Item "$q\sql\pgmq.sql" "$Prefix\share\extension\pgmq--$qv.sql"
Get-ChildItem "$q\sql\pgmq--*--*.sql" | Copy-Item -Destination "$Prefix\share\extension\"

Log "assemble the engine tree"
if (Test-Path $Engine) { Remove-Item -Recurse -Force $Engine }
New-Item -ItemType Directory -Force -Path "$Engine\bin", "$Engine\lib", "$Engine\share" | Out-Null
foreach ($p in $Programs) { Copy-Item "$Prefix\bin\$p.exe" "$Engine\bin\" }
Get-ChildItem "$Prefix\bin\*.dll" | Where-Object { $_.Name -notmatch '^lib(ecpg|pgtypes)' } | Copy-Item -Destination "$Engine\bin\"
Get-ChildItem "$Deps\bin\*.dll" | Copy-Item -Destination "$Engine\bin\"
Get-ChildItem "$Prefix\lib\*.dll" | Where-Object { $Shipped[$_.FullName] -ne $false } | Copy-Item -Destination "$Engine\lib\"
Copy-Item -Recurse "$Prefix\share\*" "$Engine\share\"
foreach ($k in $Shipped.Keys) {
  $rel = $k.Substring($Prefix.Length + 1)
  if ($rel.StartsWith("share\")) { Remove-Item -Force -ErrorAction SilentlyContinue "$Engine\$rel" }
}
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue "$Engine\share\doc", "$Engine\share\man"
python "$Here\engine.py" licences --os windows --into "$Engine\share\licenses"

Log "check: every DLL an executable or module imports is in bin\ or the OS"
$have = @{}; Get-ChildItem "$Engine\bin\*.dll" | ForEach-Object { $have[$_.Name.ToLower()] = $true }
$bad = 0
foreach ($f in Get-ChildItem "$Engine\bin\*.exe", "$Engine\bin\*.dll", "$Engine\lib\*.dll") {
  $deps = dumpbin /nologo /dependents $f.FullName | Select-String '^\s+\S+\.dll$' | ForEach-Object { $_.Line.Trim().ToLower() }
  foreach ($d in $deps) {
    if ($have[$d] -or $d -eq "postgres.exe" -or $d -match '^(api-ms-|ext-ms-|kernel32|advapi32|user32|ws2_32|secur32|crypt32|shell32|ole32|wldap32|bcrypt|ntdll|vcruntime|msvcp|ucrtbase|shlwapi|netapi32|userenv|iphlpapi|dnsapi)') { continue }
    Write-Host "  $($f.Name) imports $d"; $bad = 1
  }
}
if ($bad) { throw "missing DLLs" }

Log "pack"
$name = "forgebase-pg$PgVer-$BuildNo-$Target.tar.zst"
python "$Here\engine.py" pack $Engine "$Out\$name" --postgres $PgVer --build $BuildNo --target $Target `
  --zstd "$Deps\bin\zstd.exe" `
  --feature "pgvector=$(Lock sources.pgvector.version)" --feature "pg_cron=$(Lock sources.pg_cron.version)" `
  --feature "pgmq=$qv" --feature "pg_trgm=$PgVer" --feature "graph_cypher=false"
Get-Item "$Out\$name" | Format-List Name, Length
