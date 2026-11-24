. "$PSScriptRoot/common.ps1"
Start-Transcript -Path (Join-Path $logs 'scenarios.log') | Out-Null
$server = $null
$data = Join-Path $env:LOCALAPPDATA 'OVRefund'
$profile = Join-Path $env:LOCALAPPDATA 'nl.ovrefund.app/EBWebView'
$webroot = Join-Path $root 'http'
$base = 'http://127.0.0.1:18761'
$marker = 'ci-update-marker-v1'
$db = Join-Path $data 'ovrefund.sqlite'
$script:cliRun = 0
function Publish-Update([string]$Version, [string]$Installer = 'OV-Refund-Setup.exe') {
    # Same static metadata shape and signature trimming as the release script.
    Write-Json (Join-Path $webroot 'latest.json') ([ordered]@{
        version = $Version
        notes = 'NL / EN'
        pub_date = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
        platforms = [ordered]@{
            'windows-x86_64' = [ordered]@{
                signature = (Get-Content (Join-Path $root 'setup-0.9.1.exe.sig') -Raw).Trim()
                url = "$base/$Installer"
            }
        }
    })
}
function Invoke-UpdateCli([string]$Command, [int]$Seconds = 150) {
    # Developer-build CLI that runs the app's own check_update / install_update commands
    # headless (same endpoint override, signature check and run-lock guard). One JSON line.
    $env:OVREFUND_UPDATE_URL = "$base/latest.json"
    $script:cliRun++
    $out = Join-Path $logs "updater-cli-$($script:cliRun)-$Command.log"
    $err = Join-Path $logs "updater-cli-$($script:cliRun)-$Command-stderr.log"
    $p = Start-Process -FilePath $script:exe -ArgumentList $Command -WindowStyle Hidden -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
    try {
        $null = $p.Handle # keeps the exit code available after exit
        if (-not $p.WaitForExit($Seconds * 1000)) {
            Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
            throw "FAIL Updater CLI $Command timed out"
        }
        $code = $p.ExitCode
    } finally { $p.Dispose() }
    $line = @(Get-Content -LiteralPath $out -ErrorAction SilentlyContinue | Where-Object { $_.Trim().StartsWith('{') }) | Select-Object -Last 1
    # Output contains only updater metadata or its error; never arbitrary app state.
    Write-Host "updater-cli $Command exit=$code $line"
    $json = $null
    if ($line) { try { $json = $line | ConvertFrom-Json } catch { $json = $null } }
    return [pscustomobject]@{ Code = $code; Json = $json }
}
function Has-Field($Json, [string]$Name) { $null -ne $Json -and $null -ne $Json.PSObject.Properties[$Name] }
function Call-Updater([string]$Mode, [string]$Version = '') {
    switch ($Mode) {
        'offered' {
            $r = Invoke-UpdateCli 'update-check'
            Assert-Test ($r.Code -eq 0 -and (Has-Field $r.Json 'available') -and $r.Json.available -eq $true -and $r.Json.configured -eq $true -and $r.Json.version -eq $Version) "Updater scenario offered: update $Version offered"
        }
        'none' {
            $r = Invoke-UpdateCli 'update-check'
            Assert-Test ($r.Code -eq 0 -and (Has-Field $r.Json 'available') -and $r.Json.available -eq $false) 'Updater scenario none: downgrade not offered'
        }
        'garbage' {
            $r = Invoke-UpdateCli 'update-check'
            # A clean "no update" or a clean reported check error; never a crash or an offer.
            $clean = ($r.Code -eq 0 -and (Has-Field $r.Json 'available') -and $r.Json.available -eq $false) -or
                ($r.Code -eq 1 -and (Has-Field $r.Json 'error') -and $r.Json.error -eq 'check')
            Assert-Test $clean 'Updater scenario garbage: malformed metadata handled cleanly'
        }
        'signature' {
            $r = Invoke-UpdateCli 'update-install'
            Assert-Test ($r.Code -eq 3 -and (Has-Field $r.Json 'error') -and $r.Json.error -eq 'signature') 'Updater scenario signature: tampered update rejected by signature verification'
        }
        'install' {
            # On success the updater starts the installer and exits this process with code 0
            # before printing; PowerShell then verifies the installed version and data.
            $r = Invoke-UpdateCli 'update-install' 300
            $ok = $r.Code -eq 0 -and -not (Has-Field $r.Json 'error')
            Assert-Test $ok 'Updater scenario install: download verified, installer started, CLI exited'
        }
        default { throw "FAIL Invalid updater test mode $Mode" }
    }
}
function Start-App {
    $env:OVREFUND_UPDATE_URL = "$base/latest.json"
    return Start-Process -FilePath $script:exe -WindowStyle Hidden -PassThru
}
function Test-Version([string]$Version) {
    try {
        if (-not (Test-Path -LiteralPath $script:exe)) { return $false }
        $info = [Diagnostics.FileVersionInfo]::GetVersionInfo($script:exe)
        return "$($info.FileMajorPart).$($info.FileMinorPart).$($info.FileBuildPart)" -eq $Version
    } catch { return $false } # The installer may be replacing the file right now.
}
function Check-Marker {
    & python -c "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); assert c.execute('PRAGMA integrity_check').fetchone()[0]=='ok'; assert c.execute('SELECT value FROM settings WHERE key=?',('ci_update_marker',)).fetchone()==(sys.argv[2],)" $db $marker
    Assert-Test ($LASTEXITCODE -eq 0) 'Migrated SQLite database and settings marker intact'
}
function App-Tasks {
    @(Get-ScheduledTask | Where-Object {
        $_.TaskName -like 'OVRefund*' -or $_.TaskName -like 'OV Refund*' -or
        @($_.Actions | Where-Object { $_.PSObject.Properties['Execute'] -and $_.Execute -eq $script:exe }).Count -gt 0
    })
}
function Check-NoCredentials {
    $entries = & cmdkey /list 2>&1
    Assert-Test ($LASTEXITCODE -eq 0) 'Credential enumeration succeeded'
    # Only inspect output in memory: unrelated credentials must never enter logs.
    Assert-Test (-not ($entries -match '\.OVRefund(?:-demo[^\s]*)?\s*$')) 'OV Refund credentials absent'
}
function Seed-Credentials {
    # Synthetic orphan entry exercises the same namespace enumerator as forget-all.
    $null = & cmdkey /generic:ci-update-fixture.OVRefund /user:ci-fixture /pass:ci-fixture
    Assert-Test ($LASTEXITCODE -eq 0) 'Synthetic OV Refund credential created'
    $entries = & cmdkey /list 2>&1
    Assert-Test ([bool]($entries -match 'ci-update-fixture\.OVRefund')) 'Synthetic credential visible'
}
function Wait-Installers {
    Wait-Test {
        @(Get-CimInstance Win32_Process | Where-Object {
            $_.Name -match '(?i)((setup|installer).*\.exe$|uninstall\.exe$|^Au_.*\.exe$)' -and
            ($_.ExecutablePath -like "$root\*" -or $_.ExecutablePath -like "$env:TEMP\*" -or $_.ExecutablePath -like "$script:installDir\*")
        }).Count -eq 0
    } 'Installer processes finished' 180
}
function Uninstall-App {
    $uninstaller = Join-Path $script:installDir 'uninstall.exe'
    Assert-Test (Test-Path -LiteralPath $uninstaller) 'Uninstaller exists'
    Run-Process $uninstaller @('/S')
    # NSIS can relaunch itself from TEMP: waiting for Start-Process alone is insufficient.
    Wait-Test { -not (Test-Path -LiteralPath $script:exe) } 'Installed executable removed' 180
    Wait-Installers
    Wait-Test { @(App-Tasks).Count -eq 0 } 'All OV Refund scheduled tasks removed'
    Wait-Test { -not (Test-Path -LiteralPath $profile) } 'Identifier WebView2 profile removed'
}
try {
    Remove-Item Env:OVREFUND_DATA_DIR -ErrorAction SilentlyContinue
    Assert-Test (-not (Test-Path $data)) 'Clean runner has no existing app data'
    Assert-Test (@(Get-Process -Name ov-refund -ErrorAction SilentlyContinue).Count -eq 0) 'Clean runner has no app process'
    $candidates = @("$env:LOCALAPPDATA/OV Refund/ov-refund.exe", "$env:LOCALAPPDATA/Programs/OV Refund/ov-refund.exe")
    Assert-Test (@($candidates | Where-Object { Test-Path -LiteralPath $_ }).Count -eq 0) 'Clean runner has no installed executable'
    Check-NoCredentials
    New-Item -ItemType Directory -Force $webroot | Out-Null
    Copy-Item (Join-Path $root 'setup-0.9.1.exe') (Join-Path $webroot 'OV-Refund-Setup.exe')
    Publish-Update '0.9.1'
    $server = Start-Process -FilePath (Get-Command python.exe).Source -ArgumentList @('-u', '-m', 'http.server', '18761', '--bind', '127.0.0.1', '--directory', "`"$webroot`"") -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $logs 'http-out.log') -RedirectStandardError (Join-Path $logs 'http-error.log')
    $server.Id | Set-Content (Join-Path $root 'server.pid')
    Wait-Test {
        try { (Invoke-WebRequest "$base/latest.json" -TimeoutSec 2).StatusCode -eq 200 } catch { $false }
    } 'Loopback metadata server ready' 30
    Run-Process (Join-Path $root 'setup-0.9.0.exe') @('/S')
    $found = @($candidates | Where-Object { Test-Path -LiteralPath $_ })
    Assert-Test ($found.Count -eq 1) 'Installed executable located'
    $script:exe = [IO.Path]::GetFullPath($found[0])
    $script:installDir = Split-Path $script:exe
    Assert-Test (Test-Version '0.9.0') 'Installed file version is 0.9.0'
    Run-Process $script:exe @('history')
    Assert-Test (Test-Path $db) 'History CLI created the real migrated database'
    & python -c "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('INSERT OR REPLACE INTO settings(key,value) VALUES(?,?)',('ci_update_marker',sys.argv[2])); c.commit()" $db $marker
    Assert-Test ($LASTEXITCODE -eq 0) 'Seeded SQLite settings marker'
    Run-Process $script:exe @('install-schedule', '14')
    Assert-Test (@(App-Tasks).Count -gt 0) 'Scheduled task created before update'
    Seed-Credentials
    Call-Updater 'offered' '0.9.1'
    Call-Updater 'install'
    Wait-Test { Test-Version '0.9.1' } 'Installed file version upgraded to 0.9.1' 180
    Wait-Installers
    # The installer restarts the app with the same arguments; that instance finds no newer
    # version and exits. Stop anything left before checking the database.
    Stop-App
    Check-Marker
    Assert-Test (@(App-Tasks).Count -gt 0) 'Scheduled task survived update'

    # Copy the signed installer, then flip a byte AFTER signing. Never execute this fixture.
    $tampered = [IO.File]::ReadAllBytes((Join-Path $webroot 'OV-Refund-Setup.exe'))
    $tampered[$tampered.Length - 1] = $tampered[$tampered.Length - 1] -bxor 1
    [IO.File]::WriteAllBytes((Join-Path $webroot 'tampered.exe'), $tampered)
    Publish-Update '0.9.2' 'tampered.exe'
    Call-Updater 'offered' '0.9.2'
    Call-Updater 'signature'
    Wait-Installers
    Assert-Test (Test-Version '0.9.1') 'Signature rejection left 0.9.1 installed'
    Publish-Update '0.9.0'
    Call-Updater 'none'
    [IO.File]::WriteAllText((Join-Path $webroot 'latest.json'), '{garbage')
    Call-Updater 'garbage'
    # A second successful check proves the updater still works after malformed metadata.
    Publish-Update '0.9.0'
    Call-Updater 'none'
    # The installed GUI still starts after the rejected updates (no CDP needed: liveness only).
    $app = Start-App
    Start-Sleep -Seconds 15
    $app.Refresh()
    Assert-Test (-not $app.HasExited) '0.9.1 GUI starts and stays alive after rejected updates'
    Stop-App
    Run-Process $script:exe @('history')
    Check-Marker

    # Seed the actual WebView2 profile path as well, so its removal assertion is
    # meaningful even if the GUI start above did not create the folder.
    New-Item -ItemType Directory -Force $profile | Out-Null
    [IO.File]::WriteAllText((Join-Path $profile 'ci-fixture'), 'synthetic profile fixture')
    Uninstall-App
    Assert-Test (Test-Path $data) 'Normal uninstall preserved real data directory'
    Check-Marker
    $entries = & cmdkey /list 2>&1
    Assert-Test ([bool]($entries -match 'ci-update-fixture\.OVRefund')) 'Normal uninstall preserved non-login credential'

    Run-Process (Join-Path $root 'setup-0.9.1.exe') @('/S')
    Assert-Test (Test-Version '0.9.1') 'Reinstalled 0.9.1'
    Run-Process $script:exe @('history')
    Check-Marker
    Run-Process $script:exe @('install-schedule', '14')
    Assert-Test (@(App-Tasks).Count -gt 0) 'Scheduled task recreated before delete-data test'
    New-Item -ItemType Directory -Force $profile | Out-Null
    [IO.File]::WriteAllText((Join-Path $profile 'ci-fixture'), 'synthetic profile fixture')
    # No hook-defined silent delete-data switch: emulate the checkbox's CLI exactly.
    $wipeExe = Join-Path $root 'ov-refund.exe'
    Copy-Item -LiteralPath $script:exe -Destination $wipeExe
    Uninstall-App
    # PREUNINSTALL's forget-login can recreate the DB. Emulate the checkbox's
    # final forget-all with the same binary after the silent uninstall finishes.
    Run-Process $wipeExe @('forget-all')
    Remove-Item -LiteralPath $wipeExe -Force
    Assert-Test (-not (Test-Path $data)) 'Delete-data uninstall left no real data directory'
    Check-NoCredentials
    Write-Host 'PASS All installed updater and removal scenarios'
} catch {
    Write-Host "FAIL $($_.Exception.Message)"
    # Diagnostics for a failed run (no personal data on a CI runner).
    try {
        Write-Host '--- processes'; Get-Process -Name ov-refund, msedgewebview2 -ErrorAction SilentlyContinue | Format-Table Id, ProcessName, StartTime -AutoSize | Out-String | Write-Host
        Write-Host '--- WebView2 runtime'
        foreach ($k in 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}') { if (Test-Path $k) { Write-Host "$k pv=$((Get-ItemProperty $k).pv)" } }
        Write-Host '--- updater CLI'; Get-ChildItem -LiteralPath $logs -Filter 'updater-cli-*.log' -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "$($_.Name): $((Get-Content -LiteralPath $_.FullName -Raw -ErrorAction SilentlyContinue))" }
        Write-Host '--- session'; Write-Host "SessionId=$((Get-Process -Id $PID).SessionId) Interactive=$([Environment]::UserInteractive)"
        $log = Join-Path $data 'ovrefund.log'; if (Test-Path $log) { Copy-Item $log (Join-Path $logs 'app.log'); Get-Content $log -Tail 40 | Write-Host }
    } catch { Write-Host "diagnostics failed: $($_.Exception.Message)" }
    throw
}
finally {
    Stop-App
    if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue }
    Stop-Transcript | Out-Null
}
