<#
.SYNOPSIS
  One command for a new release: test, build + sign, virus-scan, push code to the private develop repo,
  stage the public code + website snapshot, push it after confirmation, publish the GitHub release
  at exactly that pushed commit, and prepare the (free) Microsoft false-positive submission.
  The public repository ov-refund/OV-Refund holds the code (one commit, no history), the website (site/)
  and the releases. The private develop repository (origin) keeps the full history.

  WARNING: once a release, tag or fork points at a commit with private data, force-pushing a clean
  snapshot does NOT undo the leak. Delete that release and tag on GitHub and contact GitHub Support
  to purge cached views of the old commit. Review the staged snapshot before you confirm the push.
.DESCRIPTION
  Steps (stops at the first problem; nothing public happens before step 7):
    1. checks      - clean working tree, GitHub CLI logged in, public repo clone present and pointing
                     (fetch and push URLs) at the release repository, origin (fetch and push URLs) is the
                     private develop repo (never the public repo)
    2. tests       - TypeScript, vitest, cargo test, cargo clippy
    3. build       - tools/release.ps1 -NoPublish (version bump, path-remapped build, private-data scan of
                     the program and installer, signed installer, latest.json, SHA256SUMS)
    4. scan        - Microsoft Defender scans the installer on this PC (no quarantine). If it is flagged,
                     the release stops and the Microsoft submission page is opened for you.
    5. confirm     - you type the version number to continue
    6. develop     - commit "Release vX", tag vX, push to the private develop repository (origin; guarded:
                     never the public repository), then tools/publish-public.ps1 stages the public snapshot
                     (private files removed, private-data check) on branch public-main of the public clone.
    7. public repo - you type PUSH: force-push public-main to main of the public repository (code + website;
                     the website is then published from site/ by .github/workflows/pages.yml). The pushed
                     commit is verified on GitHub. Also works for the first push to an empty repository.
    8. release     - GitHub release in the public repository with its tag at the pushed commit
                     (gh release create --target), files: installer, latest.json, SHA256SUMS
                     -> installed apps see the update from now on
    9. Microsoft   - opens the submission page, selects the installer in Explorer, copies its SHA-256.
                     Microsoft has no free upload API, so this last form is filled in by hand (~1 minute).
  One-time setup: `winget install GitHub.cli`, then `gh auth login` (log in as the ov-refund account).
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\tools\ship.ps1 -Version 0.2.0 -Notes "Sneller controleren; bugfixes / Faster checks; bug fixes"
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\tools\ship.ps1 -Version 0.2.0 -DryRun     # steps 1-4 only
#>
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [string]$Notes = "",
  [switch]$DryRun,      # stop after the scan; nothing is pushed or published
  [switch]$SkipTests,   # only for re-running after a fixed scan problem
  [switch]$SkipBuild    # reuse release\vX (e.g. after Microsoft cleared a false positive)
)
$ErrorActionPreference = 'Stop'
# Release notes are "Nederlands / English" (the app shows one half); checked before anything runs.
if ($Notes -and $Notes -notmatch ' / ') { throw "Notes must be bilingual: `"Nederlands / English`"." }
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:ProgramFiles\GitHub CLI;$env:Path"
$publicRepo = Join-Path (Split-Path -Parent $root) 'ov-refund-public'
$repo = [regex]::Match((Get-Content site\assets\config.js -Raw), "REPO = '([^']+)'").Groups[1].Value
$out = Join-Path $root "release\v$Version"
$setup = Join-Path $out 'OV-Refund-Setup.exe'
function Step($n, $text) { Write-Host ""; Write-Host "[$n] $text" -ForegroundColor Cyan }
function Run([string]$what, [scriptblock]$cmd, [switch]$Full) {
  $ErrorActionPreference = 'Continue'   # tools write progress to stderr; judge by exit code
  $lines = & $cmd 2>&1 | ForEach-Object { "$_" }
  if (-not $Full) { $lines = $lines | Select-Object -Last 4 }
  $lines | ForEach-Object { Write-Host "    $_" }
  $code = $LASTEXITCODE; $ErrorActionPreference = 'Stop'
  if ($code -ne 0) { throw "$what failed (exit $code)" }
}
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3" }
# The develop history stays private: refuse to push it if origin points at the public repository.
# Fetch URL plus every push URL (a pushurl can differ from the fetch URL).
function Get-OriginUrls([string]$dir) {
  $ErrorActionPreference = 'Continue'
  $urls = @(git -C $dir remote get-url origin 2>$null) + @(git -C $dir remote get-url --push --all origin 2>$null)
  $ErrorActionPreference = 'Stop'
  @($urls | Where-Object { $_ } | ForEach-Object { "$_".Trim() })
}
function Test-PublicUrl([string]$url) {
  $public = [regex]::Escape($repo)
  ($url -match "(^|[/:])$public(\.git)?/?$" -or $url -match '(^|[/:])ov-refund/OV-Refund(\.git)?/?$')
}
function Assert-PrivateOrigin {
  $urls = Get-OriginUrls $root
  if ($urls.Count -eq 0) { throw "origin is missing. The develop history goes only to the private develop repository." }
  foreach ($url in $urls) {
    if (Test-PublicUrl $url) { throw "origin ($url) is the public repository. The develop history goes only to the private develop repository." }
  }
  if ([IO.Path]::GetFullPath($root).TrimEnd('\') -eq [IO.Path]::GetFullPath($publicRepo).TrimEnd('\')) { throw "Run this from the private develop repository, not from the public clone." }
}
function Assert-PublicClone {
  $urls = Get-OriginUrls $publicRepo
  if ($urls.Count -eq 0) { throw "The public clone $publicRepo has no origin." }
  $want = [regex]::Escape($repo)
  foreach ($url in $urls) {
    if ($url -notmatch "^(https://github\.com/|git@github\.com:|ssh://git@github\.com/)$want(\.git)?/?$") { throw "origin of $publicRepo is not $repo ($url)." }
  }
}

# ---- 1. checks ----
Step 1 'Checks'
$ErrorActionPreference = 'Continue'
$dirty = git status --porcelain
$branch = git rev-parse --abbrev-ref HEAD
$ErrorActionPreference = 'Stop'
if ($dirty -and -not $SkipBuild) { throw "Commit or stash your changes first (working tree not clean)." }
if ($branch -ne 'main') { throw "Release from the main branch (now on $branch)." }
if (-not (Test-Path (Join-Path $publicRepo '.git'))) { throw "Public repository clone not found at $publicRepo" }
Assert-PrivateOrigin
Assert-PublicClone
if (-not $DryRun) {
  if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw "GitHub CLI missing: run 'winget install GitHub.cli' and then 'gh auth login'." }
  $ErrorActionPreference = 'Continue'; gh auth status 2>&1 | Out-Null; $ok = $LASTEXITCODE -eq 0; $ErrorActionPreference = 'Stop'
  if (-not $ok) { throw "GitHub CLI not logged in: run 'gh auth login' (as the ov-refund account)." }
  $ErrorActionPreference = 'Continue'; gh release view "v$Version" --repo $repo 2>&1 | Out-Null; $exists = $LASTEXITCODE -eq 0; $ErrorActionPreference = 'Stop'
  if ($exists) { throw "Release v$Version already exists on GitHub." }
}
Write-Host "    ok ($repo, version $Version)"

# ---- 2. tests ----
if (-not $SkipTests) {
  Step 2 'Tests'
  Run 'TypeScript' { npx tsc --noEmit }
  Run 'vitest' { npx vitest run }
  Run 'cargo test' { cargo test --workspace }
  Run 'clippy' { cargo clippy --workspace --all-targets -- -D warnings }
}

# ---- 3. build ----
if (-not $SkipBuild) {
  Step 3 'Build and sign'
  Run 'release build' { powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'release.ps1') -Version $Version -Notes $Notes -NoPublish }
}
if (-not (Test-Path $setup)) { throw "Installer missing: $setup (removed by antivirus? see Windows Security -> Protection history)" }
$hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()

# ---- 4. virus scan (local Microsoft Defender, report only) ----
Step 4 'Microsoft Defender scan'
function Open-Submission {
  Set-Clipboard -Value $hash
  Start-Process 'https://www.microsoft.com/en-us/wdsi/filesubmission'
  Start-Process explorer.exe -ArgumentList "/select,`"$setup`""
  Write-Host "    Submission page opened; installer selected in Explorer; SHA-256 copied to the clipboard." -ForegroundColor Yellow
  Write-Host "    Choose: Software developer -> Microsoft Defender Antivirus (Windows 10/11) -> upload the file"
  Write-Host "            -> 'Incorrectly detected as malware/malicious'. Detection name: as shown above (if any)."
}
$mp = Join-Path $env:ProgramFiles 'Windows Defender\MpCmdRun.exe'
if (Test-Path $mp) {
  $ErrorActionPreference = 'Continue'
  $scan = & $mp -Scan -ScanType 3 -File $setup -DisableRemediation 2>&1 | ForEach-Object { "$_" }
  $code = $LASTEXITCODE; $ErrorActionPreference = 'Stop'
  if ($code -eq 2) {
    $scan | Where-Object { $_ -match 'Threat|found' } | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
    Write-Host "    Defender flags the installer. Nothing has been published." -ForegroundColor Red
    Open-Submission
    Write-Host "    When Microsoft has cleared it (usually a few days; run 'Update-MpSignature'), continue with:"
    Write-Host "      powershell -ExecutionPolicy Bypass -File .\tools\ship.ps1 -Version $Version -SkipTests -SkipBuild"
    exit 1
  }
  if ($code -ne 0) { Write-Host "    Scan could not run (exit $code); check the installer by hand." -ForegroundColor Yellow } else { Write-Host "    clean" }
} else { Write-Host "    Defender command line not found; skipped." -ForegroundColor Yellow }
if ($DryRun) { Write-Host ""; Write-Host "Dry run finished. Release files: $out"; exit 0 }

# ---- 5. confirm ----
Step 5 'Go public?'
Write-Host "    This pushes the code to the private develop repository and stages the public code + website."
Write-Host "    You confirm the public push separately (step 7); the release follows right after it."
if ((Read-Host "    Type the version number ($Version) to continue") -ne $Version) { Write-Host "Stopped; nothing was published."; exit 1 }

# ---- 6. source repository ----
Step 6 'Push code (private develop repository)'
Assert-PrivateOrigin
$ErrorActionPreference = 'Continue'
git add Cargo.toml Cargo.lock package.json package-lock.json src-tauri/tauri.conf.json 2>&1 | Out-Null
git diff --cached --quiet; if ($LASTEXITCODE -ne 0) { git commit -q -m "Release v$Version" }
git tag -a "v$Version" -m "OV Refund $Version" 2>&1 | Out-Null
$ErrorActionPreference = 'Stop'
Run 'git push (develop)' { git push origin main --follow-tags }
Run 'publish-public' { powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'publish-public.ps1') -PublicRepo $publicRepo } -Full

# ---- 7. public repository (code + website) ----
Step 7 'Push the public code and website'
$ErrorActionPreference = 'Continue'
$staged = "$(git -C $publicRepo rev-parse --verify -q refs/heads/public-main)".Trim()
$ErrorActionPreference = 'Stop'
if (-not $staged) { throw "Branch public-main not found in $publicRepo after staging." }
Write-Host "    Staged commit $staged on public-main in $publicRepo (summary above)."
Write-Host "    Pushing replaces the public history of $repo with this one commit. A leak cannot be undone by a later push."
if ((Read-Host "    Type PUSH to publish it") -cne 'PUSH') {
  Write-Host "Stopped before the public push. No release was created; run ship.ps1 again (-SkipTests -SkipBuild) to continue."
  exit 1
}
Assert-PublicClone
Run 'git push (public)' { git -C $publicRepo push --force origin public-main:main }
$ErrorActionPreference = 'Continue'
$remote = "$(git -C $publicRepo ls-remote origin refs/heads/main)".Split("`t")[0].Trim()
$ErrorActionPreference = 'Stop'
if ($remote -ne $staged) { throw "main of $repo is $remote after the push, expected $staged. No release was created." }
Write-Host "    pushed: $staged"

# ---- 8. GitHub release (public), tagged at the pushed commit ----
Step 8 'Publish the release'
$ErrorActionPreference = 'Continue'
$tag = "$(git -C $publicRepo ls-remote origin "refs/tags/v$Version")".Split("`t")[0].Trim()
$ErrorActionPreference = 'Stop'
if ($tag -and $tag -ne $staged) { throw "Tag v$Version already exists in $repo at another commit ($tag). Delete it on GitHub first." }
$noteText = if ($Notes) { $Notes } else { "OV Refund $Version" }
Run 'gh release' { gh release create "v$Version" $setup (Join-Path $out 'latest.json') (Join-Path $out 'SHA256SUMS.txt') --repo $repo --target $staged --title "OV Refund $Version" --notes $noteText }
Write-Host "    https://github.com/$repo/releases/tag/v$Version"

# ---- 9. Microsoft ----
Step 9 'Microsoft false-positive submission (by hand, free)'
Open-Submission
Write-Host ""
Write-Host "Done: OV Refund $Version is live." -ForegroundColor Green
