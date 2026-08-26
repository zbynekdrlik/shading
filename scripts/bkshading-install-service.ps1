<#
.SYNOPSIS
  issue 808 -- on-box installer for the bkshading SERVICE on the strih Windows PC.

.DESCRIPTION
  Places the service exe + config under a stable dir (C:\bkshading), registers a persistent
  Task Scheduler KEEP-ALIVE task, and verifies the operator panel port (8770) is Listening.

  Task Scheduler has no real Restart=on-failure for a long-lived process, so this uses the repo's
  proven keep-alive idiom (obs-self-heal-install.sh / avsync-keepalive.ps1, see
  .claude/rules/avsync-monitoring.md): ONE scheduled task, triggered AtLogOn PLUS a repetition every
  N minutes, whose action re-runs THIS installer in -KeepAlive mode -- an idempotent check that
  relaunches the service if (and only if) it is not already running. That gives boot-time start
  (via auto-logon) AND crash/hang recovery, without depending on Task Scheduler's weak RestartCount.

  Config handling: the config (bkshading.toml) is seeded from the shipped example ONLY IF it is
  absent -- a redeploy NEVER clobbers an operator-tuned config. The service config is a pure camera
  list + bind + [preview]; it holds NO OBS-WS password or any credential, so nothing secret is ever
  written here.

  DRY-RUN by default: the default (no -Execute) prints the install plan and changes NOTHING.
  -Execute performs the real install (copy files, register the task, start + verify). -KeepAlive is
  the per-tick pass the scheduled task runs. -Uninstall removes the task.

  Modes:
    (default)   DRY-RUN install plan -- prints what -Execute would do, changes nothing.
    -Execute    real install: copy exe (+ seed config if absent) into C:\bkshading, copy this
                installer into C:\bkshading, register the keep-alive task, start the service, verify
                port 8770 Listening.
    -KeepAlive  per-tick relaunch pass (the scheduled-task action): if the service is not running,
                start it (with -Execute) -- otherwise a no-op. Without -Execute it is a dry report.
    -Uninstall  unregister the scheduled task (and stop a running instance, best-effort).

  Pure ASCII by design: this file is scp'd to the box and PowerShell reads it in a non-UTF-8
  codepage, so a non-ASCII char in a string breaks parsing (see .claude/rules/recordings-retention.md).

  No pwsh runtime on dev1 CI, so this file is validated STATICALLY
  (tests/python/test_bkshading_deploy_service_808.py); the live install against strih is the
  supervisor's rig step (UNVERIFIED here).

.PARAMETER StageDir
  Where the deploy dropped the exe + config seed + this installer. Default: the dir this script sits
  in (so the deploy can upload all three to one dir and just run the installer).
.PARAMETER InstallDir
  Stable on-box install dir. Default C:\bkshading.
.PARAMETER Port
  The operator panel port to verify Listening. Default 8770 (== the service config default bind).
.PARAMETER TaskName
  The keep-alive scheduled-task name. Default bkshading-service.
.PARAMETER KeepAliveMinutes
  The keep-alive task repetition cadence in minutes. Default 5.
.PARAMETER Execute
  Perform the real install (default is a dry-run plan).
.PARAMETER KeepAlive
  Run the per-tick check-and-relaunch pass (the scheduled-task action).
.PARAMETER Uninstall
  Remove the scheduled task (and stop a running instance).
#>
[CmdletBinding()]
param(
  [string]$StageDir          = '',
  [string]$InstallDir        = 'C:\bkshading',
  [int]$Port                 = 8770,
  [string]$TaskName          = 'bkshading-service',
  [int]$KeepAliveMinutes     = 5,
  [string]$ExeName           = 'bkshading.exe',
  [string]$ConfigName        = 'bkshading.toml',
  [string]$ConfigExampleName = 'bkshading.example.toml',
  [switch]$Execute,
  [switch]$KeepAlive,
  [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$ProgressPreference    = 'SilentlyContinue'

# `powershell -File` over Windows OpenSSH leaves $PSCommandPath / $PSScriptRoot EMPTY (the first
# live strih install died at bind time on a `Split-Path -Parent $PSCommandPath` param default —
# issue 808). Resolve self identity ssh-safe: prefer the automatic vars when present, fall back to
# the staged copy the orchestrator scp'd next to us.
if (-not $StageDir) {
  if ($PSScriptRoot)          { $StageDir = $PSScriptRoot }
  elseif ($PSCommandPath)     { $StageDir = Split-Path -Parent $PSCommandPath }
  else                        { $StageDir = $env:USERPROFILE }
}
$SelfName = if ($PSCommandPath) { Split-Path -Leaf $PSCommandPath } else { 'bkshading-install-service.ps1' }
$SelfPath = if ($PSCommandPath) { $PSCommandPath } else { Join-Path $StageDir $SelfName }

$ExePath     = Join-Path $InstallDir $ExeName
$ConfigPath  = Join-Path $InstallDir $ConfigName
$DeployedPs1 = Join-Path $InstallDir $SelfName
$LogFile     = Join-Path $InstallDir 'bkshading-service-keepalive.log'  # keep-alive decisions
$OutLog      = Join-Path $InstallDir 'bkshading-service.out.log'        # service stdout
$ErrLog      = Join-Path $InstallDir 'bkshading-service.err.log'        # service stderr

# Append a keep-alive decision line to the log (best-effort: the scheduled task runs -WindowStyle
# Hidden, so Write-Output alone would be discarded -- this is what makes $LogFile a REAL, readable
# record of every relaunch/no-op pass).
function Write-KeepAliveLog([string]$msg) {
  try { Add-Content -LiteralPath $LogFile -Value $msg -ErrorAction Stop } catch { }
}

# Match THIS install's running service by its EXACT exe path, never a bare process name -- several
# unrelated processes can share a name, and only the full ExecutablePath proves the C:\bkshading
# install is the one running (.claude/rules/avsync-monitoring.md gotcha #2). Used by the steady-state
# keep-alive pass, where the only legitimate instance is the deployed one.
function Test-ServiceRunning {
  $procs = Get-CimInstance Win32_Process -Filter "Name='$ExeName'" -ErrorAction SilentlyContinue |
    Where-Object { $_.ExecutablePath -eq $ExePath }
  return [bool]$procs
}

function Stop-ServiceInstances {
  Get-CimInstance Win32_Process -Filter "Name='$ExeName'" -ErrorAction SilentlyContinue |
    Where-Object { $_.ExecutablePath -eq $ExePath } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}

# Stop EVERY bkshading.exe regardless of path -- used ONCE at install time to migrate off a manual
# stage (e.g. an old C:\stage-bkshading instance, issue #1157) that would otherwise keep holding
# port 8770 after we replace the binary, producing a false-green port verify. The keep-alive pass
# never uses this (it must only ever act on the deployed exact-path instance).
function Stop-AllServiceByName {
  Get-CimInstance Win32_Process -Filter "Name='$ExeName'" -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}

# Launch the service detached, redirecting its stdout/stderr to log files so its own output is
# observable (comprehensive-logging). Pass --config only when the config file EXISTS: a MISSING
# config file is a hard error in the service (config.rs from_path), so with no config we launch bare
# -- which starts with an empty camera list (main.rs), never a crash-loop.
function Start-BkshadingService {
  $psArgs = @{
    FilePath               = $ExePath
    WindowStyle            = 'Hidden'
    RedirectStandardOutput = $OutLog
    RedirectStandardError  = $ErrLog
  }
  if (Test-Path -LiteralPath $ConfigPath) { $psArgs['ArgumentList'] = @('--config', $ConfigPath) }
  Start-Process @psArgs | Out-Null
}

# --- -KeepAlive: the per-tick check-and-relaunch pass (the scheduled-task action) ----------------
if ($KeepAlive) {
  $stamp = (Get-Date).ToString('s')
  if (Test-ServiceRunning) {
    $line = "$stamp keep-alive: bkshading service already running ($ExePath) - no-op"
  }
  elseif ($Execute) {
    Start-BkshadingService
    $line = "$stamp keep-alive: bkshading service was not running - relaunched"
  }
  else {
    $line = "$stamp keep-alive DRY-RUN: bkshading service not running - would relaunch (pass -Execute)"
  }
  Write-Output $line
  Write-KeepAliveLog $line
  exit 0
}

# --- -Uninstall: remove the scheduled task (+ stop a running instance) ---------------------------
if ($Uninstall) {
  $existing = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
  if ($existing) {
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    Write-Output "removed scheduled task: $TaskName"
  }
  else {
    Write-Output "scheduled task not present: $TaskName"
  }
  Stop-ServiceInstances
  Write-Output 'DONE: bkshading service uninstalled (task removed; the exe/config are left in place).'
  exit 0
}

# --- install plan (printed in BOTH dry-run and execute so the plan and the real run cannot diverge)
Write-Output '=== bkshading service install (issue 808) ==='
Write-Output ("  stage dir   : {0}" -f $StageDir)
Write-Output ("  install dir : {0}" -f $InstallDir)
Write-Output ("  exe         : {0}  (-> {1})" -f $ExeName, $ExePath)
Write-Output ("  config      : {0}  (seeded from {1} ONLY IF absent -- never clobbered)" -f $ConfigName, $ConfigExampleName)
Write-Output ("  task        : {0}  (keep-alive: AtLogOn + a repetition every {1} min)" -f $TaskName, $KeepAliveMinutes)
Write-Output ("  verify      : port {0} Listening after start" -f $Port)
Write-Output ("  mode        : {0}" -f ($(if ($Execute) { 'EXECUTE (installing)' } else { 'DRY-RUN (no changes)' })))
Write-Output ''

if (-not $Execute) {
  Write-Output 'DRY-RUN -- nothing changed. Re-run with -Execute to install (the live run is the supervisor rig step).'
  exit 0
}

# --- -Execute: the real install -----------------------------------------------------------------
if (-not (Test-Path -LiteralPath $InstallDir)) {
  New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
}

$stageExe = Join-Path $StageDir $ExeName
if (-not (Test-Path -LiteralPath $stageExe)) {
  Write-Error "staged service exe not found: $stageExe"
  exit 1
}
# A running exe cannot be overwritten on Windows -- stop ANY bkshading.exe first (by name), which
# also MIGRATES off a manual stage (e.g. an old C:\stage-bkshading instance, issue #1157) that would
# otherwise keep holding port 8770 and make the verify below a false green (issue 808 review).
Stop-AllServiceByName
Start-Sleep -Milliseconds 500
Copy-Item -LiteralPath $stageExe -Destination $ExePath -Force
Write-Output "deployed exe -> $ExePath"

# Copy THIS installer into the install dir so the keep-alive task runs the deployed copy, not the
# transient staging copy. Guard the self-copy for the case the installer is already run in-place from
# InstallDir (Copy-Item -Force onto itself would throw).
if ($SelfPath -ne $DeployedPs1) {
  Copy-Item -LiteralPath $SelfPath -Destination $DeployedPs1 -Force
}

# Seed the config ONLY IF the operator config is absent -- never clobber a tuned config.
$stageSeed = Join-Path $StageDir $ConfigExampleName
if (-not (Test-Path -LiteralPath $ConfigPath)) {
  if (Test-Path -LiteralPath $stageSeed) {
    Copy-Item -LiteralPath $stageSeed -Destination $ConfigPath -Force
    Write-Output "seeded config -> $ConfigPath (from $ConfigExampleName)"
  }
  else {
    Write-Output "WARNING: no config seed at $stageSeed and no existing $ConfigPath; the service will launch WITHOUT --config (an empty camera list) until you place $ConfigName"
  }
}
else {
  Write-Output "kept existing operator config (not clobbered): $ConfigPath"
}

# Register the keep-alive scheduled task: at logon PLUS a repetition every N minutes; the action
# re-runs THIS installer in -KeepAlive -Execute mode (relaunch-if-absent). Two triggers, mirroring
# install-strih-nic-selfheal.ps1's @($triggerRepeat, $triggerBoot) shape.
$argLine = ('-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "{0}" -KeepAlive -Execute -InstallDir "{1}" -Port {2} -TaskName {3} -ExeName {4} -ConfigName {5}' -f `
  $DeployedPs1, $InstallDir, $Port, $TaskName, $ExeName, $ConfigName)
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $argLine

$triggerRepeat = New-ScheduledTaskTrigger -Once -At (Get-Date) `
  -RepetitionInterval (New-TimeSpan -Minutes $KeepAliveMinutes) `
  -RepetitionDuration (New-TimeSpan -Days 3650)
$triggerLogon = New-ScheduledTaskTrigger -AtLogOn

$settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew `
  -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable `
  -ExecutionTimeLimit (New-TimeSpan -Minutes 5)

Register-ScheduledTask -TaskName $TaskName -Action $action `
  -Trigger @($triggerRepeat, $triggerLogon) -Settings $settings `
  -Description 'issue 808 bkshading service keep-alive (relaunch if not running; AtLogOn + repetition).' `
  -Force | Out-Null
Write-Output "registered keep-alive task '$TaskName' (AtLogOn + every $KeepAliveMinutes min)"

# Start it now and verify the panel port reaches a Listening state OWNED BY THE DEPLOYED EXE. A bare
# "something is Listening on 8770" check would false-green when a stale/foreign instance holds the
# port while our exe fails to bind (issue 808 review) -- so resolve the connection's owning process
# and require its path to be exactly $ExePath.
if (-not (Test-ServiceRunning)) { Start-BkshadingService }
Write-Output "waiting for port $Port to Listen (owned by $ExePath) ..."
$listening = $false
$foreignOwner = $null
for ($i = 0; $i -lt 15; $i++) {
  Start-Sleep -Seconds 1
  $conn = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($conn) {
    $owner = (Get-CimInstance Win32_Process -Filter "ProcessId=$($conn.OwningProcess)" -ErrorAction SilentlyContinue).ExecutablePath
    if ($owner -eq $ExePath) { $listening = $true; break }
    else { $foreignOwner = $owner }
  }
}
if ($listening) {
  Write-Output "OK: bkshading service Listening on port $Port owned by $ExePath (open http://<host>:$Port/ to confirm the panel)."
  Write-Output "  verify:   Get-ScheduledTask -TaskName $TaskName | Format-List"
  Write-Output "  keepalive log: $LogFile"
  exit 0
}
elseif ($foreignOwner) {
  Write-Error "port $Port is held by a DIFFERENT process ($foreignOwner), not the deployed $ExePath -- stop that instance (e.g. an old C:\stage-bkshading service) and re-run -Execute."
  exit 1
}
else {
  Write-Error "bkshading service did NOT reach a Listening state on port $Port within 15s -- check $ErrLog, or run '$ExePath --config `"$ConfigPath`"' manually to see the error."
  exit 1
}
