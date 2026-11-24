$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# The test root always lives under the runner's temp folder (the runner context isn't available in job-level env).
if (-not $env:UPDATE_TEST_ROOT -and $env:RUNNER_TEMP) { $env:UPDATE_TEST_ROOT = Join-Path $env:RUNNER_TEMP 'ovrefund-update-test' }
if ($env:GITHUB_ACTIONS -ne 'true' -or -not $env:UPDATE_TEST_ROOT -or -not $env:UPDATE_TEST_LOGS) {
    throw 'FAIL These destructive integration scripts run only on a disposable GitHub Actions runner.'
}
$root = [IO.Path]::GetFullPath($env:UPDATE_TEST_ROOT)
$expected = [IO.Path]::GetFullPath((Join-Path $env:RUNNER_TEMP 'ovrefund-update-test'))
if ($root -ne $expected) { throw 'FAIL Unexpected temporary directory' }
$logs = $env:UPDATE_TEST_LOGS
New-Item -ItemType Directory -Force $root, $logs | Out-Null
function Assert-Test([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "FAIL $Message" }
    Write-Host "PASS $Message"
}
function Wait-Test([scriptblock]$Condition, [string]$Message, [int]$Seconds = 120) {
    $until = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        if (& $Condition) { Write-Host "PASS $Message"; return }
        Start-Sleep -Milliseconds 750
    } while ([DateTime]::UtcNow -lt $until)
    throw "FAIL Timeout: $Message"
}
function Run-Process([string]$File, [string[]]$Arguments, [int]$Seconds = 180) {
    $p = Start-Process -FilePath $File -ArgumentList $Arguments -WindowStyle Hidden -PassThru
    try {
        if (-not $p.WaitForExit($Seconds * 1000)) {
            Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
            throw 'FAIL Process timed out'
        }
        Assert-Test ($p.ExitCode -eq 0) "Process completed: $([IO.Path]::GetFileName($File)) $($Arguments -join ' ')"
    } finally { $p.Dispose() }
}
function Stop-App {
    Get-Process -Name ov-refund -ErrorAction SilentlyContinue | Stop-Process -Force
    Wait-Test { @(Get-Process -Name ov-refund -ErrorAction SilentlyContinue).Count -eq 0 } 'App processes stopped'
}
function Write-Json([string]$Path, $Value) {
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
}
