param([switch]$AutoClaims, [switch]$HomeScreenshot, [ValidateSet('nl', 'en')][string]$Lang = 'nl', [ValidateSet('light', 'dark')][string]$Theme = 'light')
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\fastrelease\ov-refund.exe'
$instance = 'review' + [guid]::NewGuid().ToString('N').Substring(0, 8)
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
$profile = Join-Path $tempRoot ('ovrefund-review-' + $instance)
$data = Join-Path $tempRoot ('OVRefund-demo-' + $instance)
$listener = New-Object Net.Sockets.TcpListener([Net.IPAddress]::Loopback, 0)
$listener.Start()
$port = $listener.LocalEndpoint.Port
$listener.Stop()
$saved = @{}
foreach ($key in @('OVREFUND_DEMO_INSTANCE', 'WEBVIEW2_USER_DATA_FOLDER', 'WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS', 'PORT', 'DEMO_REVIEW_DB', 'LANG_CODE', 'THEME', 'OUTDIR')) {
    $saved[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
$demoProcess = $null
try {
    $env:OVREFUND_DEMO_INSTANCE = $instance
    $env:WEBVIEW2_USER_DATA_FOLDER = $profile
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$port"
    $env:PORT = "$port"
    $env:DEMO_REVIEW_DB = Join-Path $data 'ovrefund.sqlite'
    $demoProcess = Start-Process -FilePath $exe -ArgumentList '--demo' -WindowStyle Hidden -PassThru
    if ($HomeScreenshot) {
        $env:LANG_CODE = $Lang
        $env:THEME = $Theme
        $folder = if ($Theme -eq 'dark') { "$Lang-dark" } else { $Lang }
        $env:OUTDIR = Join-Path $root "site/assets/shots/$folder"
        node (Join-Path $PSScriptRoot 'shots.mjs') --home-only
    }
    elseif ($AutoClaims) { node (Join-Path $PSScriptRoot 'auto-claim-smoke.mjs') }
    else { node (Join-Path $PSScriptRoot 'gui-smoke.mjs') }
    $result = $LASTEXITCODE
    if ($result -ne 0 -and (Test-Path -LiteralPath (Join-Path $data 'ovrefund.log'))) {
        Get-Content -LiteralPath (Join-Path $data 'ovrefund.log') -Tail 15 | Write-Output
    }
} finally {
    if ($demoProcess -and -not $demoProcess.HasExited) { Stop-Process -Id $demoProcess.Id -Force; $demoProcess.WaitForExit() }
    # Only this randomly named sandbox, never another instance or the real app's data.
    foreach ($dir in @($profile, $data)) {
        $full = [IO.Path]::GetFullPath($dir)
        if (-not $full.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) { throw "Refusing cleanup outside temp: $full" }
        for ($attempt = 0; $attempt -lt 30; $attempt++) {
            try { if (Test-Path -LiteralPath $full) { Remove-Item -LiteralPath $full -Recurse -Force }; break }
            catch { if ($attempt -eq 29) { throw }; Start-Sleep -Milliseconds 250 }
        }
    }
    # Only the synthetic bank credential written by this instance.
    $targets = cmdkey /list | Select-String -Pattern "Target:.*\.OVRefund-demo-$instance$" | ForEach-Object { ($_.Line -split 'Target:', 2)[1].Trim() }
    foreach ($target in $targets) { cmdkey "/delete:$target" | Out-Null }
    foreach ($key in $saved.Keys) { [Environment]::SetEnvironmentVariable($key, $saved[$key], 'Process') }
}
exit $result
