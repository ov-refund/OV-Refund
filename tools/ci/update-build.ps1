param([Parameter(Mandatory)][ValidateSet('Build', 'Sign')][string]$Phase)
. "$PSScriptRoot/common.ps1"
Start-Transcript -Path (Join-Path $logs "$Phase.log") | Out-Null
try {
    $cli = (Resolve-Path 'node_modules/@tauri-apps/cli/tauri.js').Path
    $node = (Get-Command node.exe).Source
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY, Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD, Env:TAURI_SIGNING_PRIVATE_KEY_PATH -ErrorAction SilentlyContinue
    if ($Phase -eq 'Build') {
        $password = [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
        [IO.File]::WriteAllText((Join-Path $root 'password'), $password)
        # The signer prints key material: discard ALL output, never put it in artifacts.
        & $node $cli signer generate --ci -w (Join-Path $root 'test.key') --password $password *> $null
        Assert-Test ($LASTEXITCODE -eq 0) 'Disposable signing key generated'
        $password = $null
        $pubkey = (Get-Content (Join-Path $root 'test.key.pub') -Raw).Trim()
        foreach ($version in @('0.9.0', '0.9.1')) {
            $config = Join-Path $root "config-$version.json"
            Write-Json $config @{
                version = $version
                bundle = @{ createUpdaterArtifacts = $true }
                plugins = @{ updater = @{ pubkey = $pubkey; dangerousInsecureTransportProtocol = $true } }
            }
            # --no-sign defers signing finished updater artifacts to the isolated step.
            # tauri/devtools also enables inspector support in a release-profile WebView.
            & $node $cli build --ci --no-sign --bundles nsis --features 'devtools,custom-protocol,tauri/devtools' --config $config
            Assert-Test ($LASTEXITCODE -eq 0) "Build $version"
            $setups = @(Get-ChildItem "target/release/bundle/nsis/*_${version}_x64-setup.exe")
            Assert-Test ($setups.Count -eq 1) "One NSIS installer for $version"
            Copy-Item -LiteralPath $setups[0].FullName -Destination (Join-Path $root "setup-$version.exe")
        }
        & git diff --exit-code -- Cargo.toml Cargo.lock package.json package-lock.json src-tauri
        Assert-Test ($LASTEXITCODE -eq 0) 'Build overrides left tracked application files unchanged'
    } else {
        $artifact = Join-Path $root 'setup-0.9.1.exe'
        $sign = [Diagnostics.ProcessStartInfo]::new()
        $sign.FileName = $node
        $sign.UseShellExecute = $false
        $sign.CreateNoWindow = $true
        $sign.RedirectStandardOutput = $true
        $sign.RedirectStandardError = $true
        foreach ($arg in @($cli, 'signer', 'sign', '--app-version', '0.9.1', $artifact)) { $sign.ArgumentList.Add($arg) }
        $sign.Environment.Clear()
        foreach ($name in @('SystemRoot', 'WINDIR', 'TEMP', 'TMP')) {
            $value = [Environment]::GetEnvironmentVariable($name)
            if ($value) { $sign.Environment[$name] = $value }
        }
        $p = $null
        try {
            $sign.Environment['TAURI_SIGNING_PRIVATE_KEY'] = Get-Content (Join-Path $root 'test.key') -Raw
            $sign.Environment['TAURI_SIGNING_PRIVATE_KEY_PASSWORD'] = Get-Content (Join-Path $root 'password') -Raw
            $p = [Diagnostics.Process]::Start($sign)
            $stdout = $p.StandardOutput.ReadToEndAsync()
            $stderr = $p.StandardError.ReadToEndAsync()
            if (-not $p.WaitForExit(120000)) { $p.Kill($true); throw 'FAIL Signer timeout' }
            $null = $stdout.GetAwaiter().GetResult()
            $null = $stderr.GetAwaiter().GetResult()
            Assert-Test ($p.ExitCode -eq 0 -and (Test-Path "$artifact.sig")) 'New installer signed separately'
        } finally {
            $sign.Environment.Clear()
            if ($p) { $p.Dispose() }
            Remove-Item -LiteralPath (Join-Path $root 'test.key'), (Join-Path $root 'password') -Force -ErrorAction SilentlyContinue
        }
    }
} catch { Write-Host "FAIL $($_.Exception.Message)"; throw }
finally { Stop-Transcript | Out-Null }
