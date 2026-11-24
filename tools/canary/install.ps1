<#
.SYNOPSIS
  Install (or update) the maintainer's daily OV Refund dry run on this computer.

.DESCRIPTION
  Copies a developer build of OV Refund to %LOCALAPPDATA%\OVRefund-canary and registers a Windows
  scheduled task that runs tools\canary\dry-run.ps1 twice a day, at 08:00 and 17:00 (also catches up
  after the computer was off). Other times: -At '07:30','18:00'. Build the developer version first:
    npm run build
    cargo build --profile fastrelease -p ov-refund --features custom-protocol,devtools
  Every run e-mails an OK/FAILED report through the bug-report relay (no set-up needed).
  Remove again with: install.ps1 -Remove
#>
param(
  [string]$Exe = (Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'target\fastrelease\ov-refund.exe'),
  [string[]]$At = @('08:00', '17:00'),
  [switch]$Remove
)
$ErrorActionPreference = 'Stop'
$task = 'OV Refund dry run'
$root = Join-Path $env:LOCALAPPDATA 'OVRefund-canary'
if ($Remove) {
  schtasks /Delete /TN $task /F 2>$null | Out-Null
  if (Test-Path $root) { Remove-Item $root -Recurse -Force }
  Write-Host 'Dry run removed.'
  exit 0
}
if (-not (Test-Path $Exe)) { throw "Developer build not found: $Exe" }
New-Item -ItemType Directory -Force -Path $root | Out-Null
Copy-Item $Exe (Join-Path $root 'ov-refund.exe') -Force
Copy-Item (Join-Path $PSScriptRoot 'dry-run.ps1') (Join-Path $root 'dry-run.ps1') -Force
$script = Join-Path $root 'dry-run.ps1'
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$script`""
$trigger = @($At | ForEach-Object { New-ScheduledTaskTrigger -Daily -At $_ })
$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 45) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Limited
Register-ScheduledTask -TaskName $task -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force | Out-Null
Write-Host "Installed: '$task' runs daily at $($At -join ' and ') (or as soon as possible after). Log: $(Join-Path $root 'dry-run.log')"
Write-Host "Run it now: Start-ScheduledTask -TaskName '$task'"
