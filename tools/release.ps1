<#
.SYNOPSIS
  Build, sign and publish a new OV Refund version.

.DESCRIPTION
  1. Sets the version everywhere (Cargo workspace, package.json, tauri.conf.json).
  2. Builds the NSIS installer, signed with the maintainer's updater key
     (%USERPROFILE%\.ovrefund-release\updater.key, password DPAPI-protected next to it).
     Rust paths are remapped for the build (%USERPROFILE%\.cargo -> C:\cargo, repository -> C:\src,
     %USERPROFILE% -> C:\home) so no Windows user name ends up in the program. Afterwards dist\, the program
     and the installer are scanned (tools/leak-scan.ps1: user folder paths, private terms, e-mail addresses);
     any hit stops the release before signing.
  3. Writes release\v<version>\ with OV-Refund-Setup.exe and latest.json (what the app's updater reads).
  4. Without -NoPublish, and only when the staged public snapshot (branch public-main of the public clone)
     has been pushed to main of the public repository: creates the GitHub release with its tag at exactly
     that pushed commit (gh release create --target) and uploads the files. Otherwise it prints what to do.
     tools/ship.ps1 runs the whole sequence (build -NoPublish, stage, push after confirmation, release).

  Users then get the update: the website's download button always points to the newest release,
  and installed apps show "Nieuwe versie beschikbaar - Nu bijwerken".

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\tools\release.ps1 -Version 0.2.0 -Notes "Sneller controleren; bugfixes / Faster checks; bug fixes"

  -Notes is "Nederlands / English": the app shows the half for its language (split on the first " / ").
  The installer is always uploaded as exactly OV-Refund-Setup.exe (the website's download link uses that name).
#>
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [string]$Notes = "",
  [switch]$NoPublish,
  [switch]$AskPassphrase,
  [string]$PublicRepo = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $PublicRepo) { $PublicRepo = Join-Path (Split-Path -Parent $root) 'ov-refund-public' }
Set-Location $root
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3" }
if ($Notes -and $Notes -notmatch ' / ') { throw "Notes must be bilingual: `"Nederlands / English`" (the app splits on ' / ')." }

# ---- repository (single source: site/assets/config.js) and the URL compiled into the app ----
$repo = [regex]::Match((Get-Content site\assets\config.js -Raw), "REPO = '([^']+)'").Groups[1].Value
if (-not $repo -or $repo -like 'OWNER/*') { throw "Set REPO in site/assets/config.js first (owner/name of the GitHub repository)." }
$expectedUrl = "https://github.com/$repo/releases/latest/download/latest.json"
$updaterRs = Get-Content src-tauri\src\updater.rs -Raw
if ($updaterRs -notmatch [regex]::Escape("pub const UPDATE_URL: &str = `"$expectedUrl`";")) {
  throw "src-tauri/src/updater.rs UPDATE_URL must be `"$expectedUrl`" (otherwise installed apps can't find this release)."
}

# UTF-8 without BOM (Windows PowerShell 5.1's -Encoding utf8 adds a BOM, which breaks package.json)
function Write-Utf8([string]$Path) { process { [IO.File]::WriteAllText((Join-Path $root $Path), $_, (New-Object Text.UTF8Encoding $false)) } }

# ---- version everywhere ----
(Get-Content Cargo.toml -Raw) -replace '(?m)^(version = )"\d+\.\d+\.\d+"', "`$1`"$Version`"" | Write-Utf8 Cargo.toml
(Get-Content package.json -Raw) -replace '"version": "\d+\.\d+\.\d+"', "`"version`": `"$Version`"" | Write-Utf8 package.json
(Get-Content src-tauri\tauri.conf.json -Raw) -replace '"version": "\d+\.\d+\.\d+"', "`"version`": `"$Version`"" | Write-Utf8 src-tauri\tauri.conf.json

# Build hooks and dependencies must never inherit updater signing secrets.
Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY, Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD, Env:TAURI_SIGNING_PRIVATE_KEY_PATH -ErrorAction SilentlyContinue
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$unsignedConfig = Join-Path $PSScriptRoot 'unsigned-bundle.json'
# Panic messages carry source paths (also of crates under %USERPROFILE%\.cargo); remap them so the user name
# never ships. rustc applies the LAST matching remap, so the general prefix comes first, the specific ones after.
# CARGO_ENCODED_RUSTFLAGS (0x1F-separated) keeps paths with spaces intact; it overrides RUSTFLAGS.
$userHome = [IO.Path]::GetFullPath($env:USERPROFILE).TrimEnd('\')
$remap = @(
  "--remap-path-prefix=$userHome=C:\home",
  "--remap-path-prefix=$userHome\.cargo=C:\cargo",
  "--remap-path-prefix=$([IO.Path]::GetFullPath($root).TrimEnd('\'))=C:\src"
)
if ($env:CARGO_HOME) { $remap += "--remap-path-prefix=$([IO.Path]::GetFullPath($env:CARGO_HOME).TrimEnd('\'))=C:\cargo" }
$savedFlags = @($env:RUSTFLAGS, $env:CARGO_ENCODED_RUSTFLAGS)
try {
  Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
  $env:CARGO_ENCODED_RUSTFLAGS = $remap -join [char]0x1F
  $ErrorActionPreference = 'Continue'
  npx --offline tauri build --bundles nsis --config $unsignedConfig 2>&1 | ForEach-Object { "$_" }
  $code = $LASTEXITCODE
  $ErrorActionPreference = 'Stop'
} finally {
  $env:RUSTFLAGS = $savedFlags[0]; $env:CARGO_ENCODED_RUSTFLAGS = $savedFlags[1]
}
if ($code -ne 0) { throw "build failed ($code)" }

# ---- release folder ----
$setup = Get-ChildItem "target\release\bundle\nsis\*_${Version}_x64-setup.exe" | Select-Object -First 1
if (-not $setup) { throw "installer for $Version not found" }

# ---- private-data scan of what ships (web assets, program, installer) ----
. (Join-Path $PSScriptRoot 'leak-scan.ps1')
$terms = Read-LeakTerms (Join-Path $env:USERPROFILE '.ovrefund-release\leak-terms.txt')
$leaks = 0
$maps = @(Get-ChildItem dist -Recurse -File -Filter *.map -ErrorAction SilentlyContinue)
if ($maps.Count -gt 0) { Write-Host "LEAK? dist contains $($maps.Count) source map(s)" -ForegroundColor Red; $leaks += $maps.Count }
foreach ($f in @(Get-ChildItem dist -Recurse -File -ErrorAction SilentlyContinue) + @(Get-Item target\release\ov-refund.exe) + @($setup)) {
  $leaks += Test-BuiltFileLeaks $f.FullName $terms
}
if ($leaks -gt 0) { throw "Private-data scan: $leaks hit(s) in the build. Nothing was signed or published." }
Write-Host "Private-data scan of dist, ov-refund.exe and the installer: no hits."
# Sign only the finished updater artifact, in a child with an explicit minimal environment.
# Invoke the installed CLI directly: no npm hooks, build scripts or dependency downloads.
$keyDir = Join-Path $env:USERPROFILE '.ovrefund-release'
$sign = New-Object System.Diagnostics.ProcessStartInfo
$sign.FileName = (Get-Command node.exe -ErrorAction Stop).Source
$sign.UseShellExecute = $false
$sign.CreateNoWindow = $true
$sign.WorkingDirectory = $root
$cli = Join-Path $root 'node_modules\@tauri-apps\cli\tauri.js'
$sign.Arguments = '"' + $cli + '" signer sign --app-version "' + $Version + '" "' + $setup.FullName + '"'
$sign.EnvironmentVariables.Clear()
foreach ($name in @('SystemRoot', 'WINDIR', 'TEMP', 'TMP')) {
  $value = [Environment]::GetEnvironmentVariable($name)
  if ($value) { $sign.EnvironmentVariables[$name] = $value }
}
$bstr = [IntPtr]::Zero
try {
  $sign.EnvironmentVariables['TAURI_SIGNING_PRIVATE_KEY'] = Get-Content (Join-Path $keyDir 'updater.key') -Raw
  # Same-user build code can still read this directory and decrypt DPAPI data.
  # Use a separate signing account/machine for higher assurance (SECURITY.md).
  $sec = if ($AskPassphrase) { Read-Host 'Signing key password' -AsSecureString } else { Get-Content (Join-Path $keyDir 'updater.key.password.dpapi') | ConvertTo-SecureString }
  $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($sec)
  $sign.EnvironmentVariables['TAURI_SIGNING_PRIVATE_KEY_PASSWORD'] = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr)
  $proc = [System.Diagnostics.Process]::Start($sign)
  $proc.WaitForExit()
  if ($proc.ExitCode -ne 0) { throw "signing failed ($($proc.ExitCode))" }
} finally {
  $sign.EnvironmentVariables.Clear()
  if ($bstr -ne [IntPtr]::Zero) { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
  $sec = $null
  if ($proc) { $proc.Dispose() }
}
$sig = Get-Content "$($setup.FullName).sig" -Raw
$out = Join-Path $root "release\v$Version"
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item $setup.FullName (Join-Path $out 'OV-Refund-Setup.exe') -Force
$latest = [ordered]@{
  version   = $Version
  notes     = $Notes
  pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
  platforms = [ordered]@{
    'windows-x86_64' = [ordered]@{ signature = $sig.Trim(); url = "https://github.com/$repo/releases/download/v$Version/OV-Refund-Setup.exe" }
  }
}
[IO.File]::WriteAllText((Join-Path $out 'latest.json'), ($latest | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding $false))
$hash = (Get-FileHash (Join-Path $out 'OV-Refund-Setup.exe') -Algorithm SHA256).Hash.ToLower()
"$hash  OV-Refund-Setup.exe" | Set-Content (Join-Path $out 'SHA256SUMS.txt') -Encoding ascii
Write-Host "Release files in $out"

# ---- antivirus false positives (free) ----
# Unsigned installers start without reputation, and Defender's machine-learning check sometimes flags them.
Write-Host ""
Write-Host "Before sharing: submit the installer to Microsoft as a false positive (free):"
Write-Host "  https://www.microsoft.com/en-us/wdsi/filesubmission"
Write-Host "  -> Software developer -> Microsoft Defender Antivirus (Windows 10/11)"
Write-Host "  -> upload OV-Refund-Setup.exe -> 'Incorrectly detected as malware/malicious'"
Write-Host "  SHA-256: $hash"
Write-Host "  If Defender already removed it: Windows Security -> Protection history -> Restore."

# ---- publish ----
# The release tag must point at the public code of this version: it is only created at the commit that is
# already pushed to main of the public repository (never before that push).
if ($NoPublish) { return }
$ErrorActionPreference = 'Continue'
$staged = if (Test-Path (Join-Path $PublicRepo '.git')) { "$(git -C $PublicRepo rev-parse --verify -q refs/heads/public-main)".Trim() } else { '' }
$remote = if ($staged) { "$(git -C $PublicRepo ls-remote origin refs/heads/main)".Split("`t")[0].Trim() } else { '' }
$ErrorActionPreference = 'Stop'
if (-not $staged -or $staged -ne $remote) {
  Write-Host ""
  Write-Host "Not published: the public code for v$Version is not on main of $repo yet." -ForegroundColor Yellow
  Write-Host "  Run tools/ship.ps1 (stages, pushes after confirmation, then creates the release), or stage with"
  Write-Host "  tools/publish-public.ps1, push public-main to main, and run this script again."
  return
}
if (Get-Command gh -ErrorAction SilentlyContinue) {
  gh release create "v$Version" (Join-Path $out 'OV-Refund-Setup.exe') (Join-Path $out 'latest.json') (Join-Path $out 'SHA256SUMS.txt') --repo $repo --target $staged --title "OV Refund $Version" --notes $(if ($Notes) { $Notes } else { "OV Refund $Version" })
  if ($LASTEXITCODE -ne 0) { throw "gh release create failed ($LASTEXITCODE)" }
  Write-Host "Published: https://github.com/$repo/releases/tag/v$Version"
} else {
  Write-Host ""
  Write-Host "GitHub CLI not found. Publish by hand:"
  Write-Host "  1. https://github.com/$repo/releases/new  -> tag v$Version with target commit $staged, title 'OV Refund $Version'"
  Write-Host "  2. Upload OV-Refund-Setup.exe, latest.json and SHA256SUMS.txt from $out"
  Write-Host "  3. Publish. Apps and the website pick it up immediately."
}
