. "$PSScriptRoot/common.ps1"
Start-Transcript -Path (Join-Path $logs 'cleanup.log') | Out-Null
try {
    # Remove signing material first, even if a failed installer left files locked.
    Remove-Item -LiteralPath (Join-Path $root 'test.key'), (Join-Path $root 'password') -Force -ErrorAction SilentlyContinue
    Stop-App
    $pidFile = Join-Path $root 'server.pid'
    if (Test-Path -LiteralPath $pidFile) {
        $serverPid = [int](Get-Content $pidFile -Raw)
        $serverProcess = Get-CimInstance Win32_Process -Filter "ProcessId = $serverPid"
        if ($serverProcess -and $serverProcess.CommandLine -like '*http.server*18761*') {
            Stop-Process -Id $serverPid -Force -ErrorAction SilentlyContinue
        }
    }
    # Keep only explicitly named logs. Never upload keys, binaries, profiles, or DBs.
    # common.ps1 validated this exact resolved runner-temporary directory.
    Wait-Test {
        if (Test-Path -LiteralPath $root) {
            Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
        }
        -not (Test-Path -LiteralPath $root)
    } 'Temporary directory released and removed' 60
    Assert-Test (-not (Test-Path -LiteralPath $root)) 'Temporary files and disposable key removed'
} catch { Write-Host "FAIL $($_.Exception.Message)"; throw }
finally { Stop-Transcript | Out-Null }
