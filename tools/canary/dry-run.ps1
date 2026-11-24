<#
.SYNOPSIS
  Daily maintainer dry run: checks that OV Refund still works with Mijn NS. Never submits anything.

.DESCRIPTION
  Runs a developer build of OV Refund (installed by tools\canary\install.ps1) on a fresh COPY of the
  local OV Refund data, with OVREFUND_DRY_RUN=1 (no notifications, no e-mail summary, no NS e-mails,
  no scheduled-task changes, full 1.5-year history fetch):
    1. History: logs in with the saved session and reads the full travel history (period filter).
    2. Claim form: opens NS's claim form for the most recent trip still in the claim window, fills it,
       ticks the declaration and runs the pre-submit check. "Verzenden" is never pressed.
    3. Arriva form: fills Arriva's claim form with synthetic demo data behind the network guard, checks the
       file field locally with the two demo attachments (nothing is uploaded: the guard blocks every upload),
       checks every field and that "Indienen" is enabled. Never pressed, no CAPTCHA.
  Steps 1 and 2 run as a separate developer instance: its own data folder, and so its own Credential
  Manager namespace, scheduled-task name and export folder. OVREFUND_SHARED_SECRETS=1 lets it READ the
  installed app's saved NS login, bank details and card numbers (to log in again and to fill NS's form);
  writing or deleting them is refused in that mode, so the real entries are never changed.
  The copy is deleted afterwards. After every run an e-mail goes to the project mailbox through the
  same relay as the in-app bug form (sender "OV Refund daily check", subject ends in OK or FAILED);
  it holds step names, counts and error texts only, no personal data. On failure also a Windows
  notification. A log of the last runs is kept in the canary folder. -NoMail skips the e-mail.
#>
param(
  [string]$Root = (Join-Path $env:LOCALAPPDATA 'OVRefund-canary'),
  [switch]$NoMail
)
$ErrorActionPreference = 'Stop'
$exe = Join-Path $Root 'ov-refund.exe'
$real = Join-Path $env:LOCALAPPDATA 'OVRefund'
$work = Join-Path $Root 'run'
$logFile = Join-Path $Root 'dry-run.log'
$steps = @()

function Log([string]$m) { Add-Content -Path $logFile -Value ("{0:yyyy-MM-dd HH:mm} {1}" -f (Get-Date), $m) -Encoding UTF8 }

function Toast([string]$title, [string]$text) {
  try {
    [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
    $xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
    $t = $xml.GetElementsByTagName('text')
    $t.Item(0).AppendChild($xml.CreateTextNode($title)) | Out-Null
    $t.Item(1).AppendChild($xml.CreateTextNode($text)) | Out-Null
    $app = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show([Windows.UI.Notifications.ToastNotification]::new($xml))
  } catch { Log "notification failed: $($_.Exception.Message)" }
}

function Send-Report([string]$subject, [string]$body) {
  # Same relay as the in-app bug form, sent from the app's own browser (sender name "OV Refund daily check").
  if ($NoMail) { return }
  $tmp = Join-Path $Root 'mail'
  try {
    New-Item -ItemType Directory -Force -Path (Join-Path $tmp 'data'), (Join-Path $tmp 'webview') | Out-Null
    $file = Join-Path $tmp 'report.txt'
    Set-Content -Path $file -Value $body -Encoding UTF8
    $psi = New-Object Diagnostics.ProcessStartInfo $exe
    $psi.Arguments = "relay-report `"$subject`" `"$file`""
    $psi.UseShellExecute = $false; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true; $psi.CreateNoWindow = $true
    $psi.EnvironmentVariables['OVREFUND_DATA_DIR'] = (Join-Path $tmp 'data')
    $psi.EnvironmentVariables['WEBVIEW2_USER_DATA_FOLDER'] = (Join-Path $tmp 'webview')
    $psi.EnvironmentVariables['OVREFUND_DRY_RUN'] = '1'
    $p = [Diagnostics.Process]::Start($psi)
    $o = $p.StandardOutput.ReadToEndAsync(); $e = $p.StandardError.ReadToEndAsync()
    if (-not $p.WaitForExit(120000)) { try { $p.Kill() } catch {}; Log 'e-mail failed: relay timed out' }
    elseif ($o.Result -match 'report sent') { Log "e-mail sent: $subject" }
    else { Log ("e-mail failed: " + (($o.Result + ' ' + $e.Result) -split "`r?`n" | Where-Object { $_ -match 'relay|error|Error' } | Select-Object -First 2) -join ' ') }
  } catch { Log "e-mail failed: $($_.Exception.Message)" }
  finally { foreach ($i in 1..5) { try { if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }; break } catch { Start-Sleep -Seconds 2 } } }
}

# User folders (C:\Users\<name>\...) never go into the e-mail: the Windows user name is personal data.
function Redact-Paths([string]$s) {
  if (-not $s) { return $s }
  $s = $s -replace '(?i)[A-Z]:\\Users\\[^\\/\s"'']+', '<user folder>'
  $s -replace '(?i)/Users/[^/\s"'']+', '<user folder>'
}

# Only lines that describe the failure; trip details (stations, times) never leave the computer.
function FailureLines([string[]]$out) {
  $out | Where-Object { $_ -match '^(  form fill error|form-test failed|  problem|.*requested history period|.*failed:|.*not found|.*did not load|.*timed out after)' } |
    ForEach-Object { Redact-Paths $_.Trim() } | Select-Object -First 8
}

function Run-Step([string]$name, [string[]]$arguments, [int]$timeoutSec) {
  $psi = New-Object Diagnostics.ProcessStartInfo $exe
  $psi.Arguments = ($arguments -join ' ')
  $psi.UseShellExecute = $false; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true; $psi.CreateNoWindow = $true
  $psi.EnvironmentVariables['OVREFUND_DATA_DIR'] = (Join-Path $work 'data')
  $psi.EnvironmentVariables['WEBVIEW2_USER_DATA_FOLDER'] = (Join-Path $work 'webview')
  $psi.EnvironmentVariables['OVREFUND_DRY_RUN'] = '1'
  # Read-only use of the installed app's saved credentials (developer builds only; see the description).
  $psi.EnvironmentVariables['OVREFUND_SHARED_SECRETS'] = '1'
  $p = [Diagnostics.Process]::Start($psi)
  $stdout = $p.StandardOutput.ReadToEndAsync(); $stderr = $p.StandardError.ReadToEndAsync()
  if (-not $p.WaitForExit($timeoutSec * 1000)) { try { $p.Kill() } catch {}; return @{ ok = $false; lines = @("$name timed out after $timeoutSec s") } }
  $lines = ($stdout.Result + "`n" + $stderr.Result) -split "`r?`n" | Where-Object { $_ -and $_ -notmatch 'Chrome_WidgetWin' }
  @{ ok = ($p.ExitCode -eq 0); code = $p.ExitCode; lines = $lines }
}

New-Item -ItemType Directory -Force -Path $Root | Out-Null
$problems = @()
try {
  if (-not (Test-Path $exe)) { throw "developer build missing: $exe (run tools\canary\install.ps1)" }
  if (-not (Test-Path (Join-Path $real 'ovrefund.sqlite'))) { throw 'no OV Refund data on this computer' }
  if (-not (Test-Path (Join-Path $real 'ns-session.bin'))) { throw 'no saved NS login: log in to Mijn NS in OV Refund first' }
  if (Test-Path $work) { Remove-Item $work -Recurse -Force }
  New-Item -ItemType Directory -Force -Path (Join-Path $work 'data'), (Join-Path $work 'webview') | Out-Null
  Copy-Item (Join-Path $real 'ovrefund.sqlite'), (Join-Path $real 'ns-session.bin') (Join-Path $work 'data')

  # 1. History (full period) with the saved login.
  $h = Run-Step 'history' @('scan') 900
  $summary = $h.lines | Where-Object { $_ -match '^run #' } | Select-Object -Last 1
  $logText = Get-Content (Join-Path $work 'data\ovrefund.log') -Raw -ErrorAction SilentlyContinue
  if (-not $h.ok -or $summary -notmatch 'session ok' -or $h.lines -match '^\s+problem:' -or $logText -match 'requested history period') {
    $why = (FailureLines ($h.lines + ($logText -split "`r?`n"))) -join ' | '
    if (-not $why) { $why = "exit $($h.code); no clear error (session state: $(if ($summary -match 'session (\w+)') { $Matches[1] } else { 'unknown' }))" }
    $problems += "Step 1 (history): $why"
  }

  $range = if ($logText -match 'fetch range (\w+)') { $Matches[1] } else { '?' }
  # The full history must not shrink: compare with the last successful run (NS once served only 15
  # transactions while everything else looked fine).
  if (-not $problems -and $summary -match ': (\d+) journeys') {
    $now = [int]$Matches[1]
    $prev = Get-Content $logFile -ErrorAction SilentlyContinue | Where-Object { $_ -match 'history: range Max; (\d+) journeys' } |
      ForEach-Object { if ($_ -match 'history: range Max; (\d+) journeys') { [int]$Matches[1] } } | Where-Object { $_ -ge 20 } | Select-Object -Last 1
    if ($range -ne 'Max') { $problems += "Step 1 (history): the full history was not requested (range $range)" }
    elseif ($prev -and $now -lt [Math]::Floor($prev * 0.9)) { $problems += "Step 1 (history): only $now trips read, the last good run read $prev (NS may have changed the travel-history page)" }
  }
  $hl = "history: range {0}; {1}" -f $range, ($summary -replace '^run #\d+: ', '')
  Log $hl; $steps += $hl
  # 2. Claim form up to (not including) Verzenden.
  if (-not $problems) {
    $f = Run-Step 'claim form' @('form-test', 'auto', '--no-screenshot') 600
    if (-not $f.ok) { $problems += 'Step 2 (claim form): ' + ((FailureLines $f.lines) -join ' | ') }
    else { $cl = ("claim form: " + (($f.lines | Where-Object { $_ -match '^  ok: (Verklaring|Knop Verzenden|Ingevulde waarden|Formuliercontrole)|^form test passed' } | ForEach-Object { ($_ -replace '^  ok: ', '').Split('/')[-1].Trim() }) -join '; ')); Log $cl; $steps += $cl }
  }
  # 3. Arriva claim form with synthetic demo data and the two demo attachments, up to (not including) Indienen.
  #    Independent of the NS steps and of the user's data; the CAPTCHA is never triggered.
  $a = Run-Step 'arriva form' @('arriva-form-test') 300
  if (-not $a.ok) { $why = (FailureLines $a.lines) -join ' | '; if (-not $why) { $why = "exit $($a.code); no clear error" }; $problems += "Step 3 (Arriva form): $why" }
  else { $al = 'arriva form: ' + (($a.lines | Where-Object { $_ -match '^arriva form test passed' } | Select-Object -Last 1) -replace '^arriva form test passed: ', ''); Log $al; $steps += $al }
} catch {
  $problems += "Setup: $(Redact-Paths $_.Exception.Message)"
} finally {
  # The copy holds personal data: always remove it (WebView2 may need a moment to release files).
  foreach ($i in 1..10) { try { if (Test-Path $work) { Remove-Item $work -Recurse -Force }; break } catch { Start-Sleep -Seconds 3 } }
}

$when = '{0:yyyy-MM-dd HH:mm}' -f (Get-Date)
$done = if ($steps) { "`n`nSteps that worked:`n" + ($steps -join "`n") } else { '' }
if ($problems) {
  $text = $problems -join "`n"
  Log "FAILED`n$text"
  Toast 'OV Refund: dagelijkse test mislukt' ($problems[0].Substring(0, [Math]::Min(180, $problems[0].Length)))
  Send-Report 'OV Refund daily check: FAILED' ("The daily OV Refund dry run FAILED on $when. Nothing was submitted.`n`n$text$done`n`nDetails: dry-run.log in the canary folder on this computer.")
  $code = 1
} else {
  Log 'ok: history, NS claim form and Arriva form work (nothing was sent)'
  Send-Report 'OV Refund daily check: OK' ("The daily OV Refund dry run passed on $when. Nothing was submitted.$done")
  $code = 0
}
# Keep the log short.
$all = Get-Content $logFile -ErrorAction SilentlyContinue; if ($all.Count -gt 400) { $all | Select-Object -Last 300 | Set-Content $logFile -Encoding UTF8 }
exit $code
