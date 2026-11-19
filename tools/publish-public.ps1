<#
.SYNOPSIS
  Stage the committed source tree as ONE fresh commit (no history) on branch public-main in the local
  clone of the public repository ov-refund/OV-Refund. Pushing is a separate, deliberate step.
.DESCRIPTION
  The public repository holds the code, the website (site/, published by .github/workflows/pages.yml)
  and the GitHub Releases. The develop repository (origin of this clone) keeps the full history.

  WARNING: once a release, tag or fork points at a commit with private data, force-pushing a clean
  snapshot does NOT undo the leak. Delete that release and tag on GitHub and contact GitHub Support
  to purge cached views of the old commit. Check the staged snapshot before every push.

  Steps (stops at the first problem; nothing is pushed unless -Push is given):
    1. checks   - on main, clean working tree, leak-term and exclude lists present, this repository is
                  not the public clone, the public clone (fetch and push URLs) is ov-refund/OV-Refund
    2. export   - `git archive HEAD` into a fresh temporary folder (the committed tree, not the working tree)
    3. private  - removes every path in %USERPROFILE%\.ovrefund-release\public-exclude.txt (required; one entry
                  per line, # comments, * and ? wildcards; an entry without / matches a name at any depth,
                  a trailing / means a folder, .. and absolute paths are refused). Unused entries are reported.
    4. scan     - every file (text and binary), every file path and the commit message, against
                  %USERPROFILE%\.ovrefund-release\leak-terms.txt (one regex per line, # comments; required) plus
                  checks for user folder paths and e-mail addresses (tools/leak-scan.ps1). Hits are printed as
                  path:line only (private terms by number). Any hit stops the script with exit code 1;
                  nothing is staged and the branches public-main and public-test are deleted.
    5. stage    - in the public clone: (re)creates the orphan branch public-main, removes every other file
                  (also ignored ones), copies the exported tree in, checks that exactly the scanned files are
                  staged, and commits it as "OV Refund <ov-refund@users.noreply.github.com>" dated today
                  00:00 UTC. The local main branch of the public clone is not changed.
    6. push     - only with -Push: git push --force origin public-main:main. Otherwise the command is printed.
  The temporary folder is always deleted.
.PARAMETER Push
  Force-push public-main to main of the public repository after staging. Replaces its history.
  tools/ship.ps1 pushes itself (after asking) so it can create the release at the pushed commit.
.PARAMETER Message
  Commit message. Default: "OV Refund <version>" (version from Cargo.toml).
.PARAMETER AllowDirty
  For testing the script only: exports the working tree (tracked and untracked, not ignored files)
  instead of HEAD, skips the clean-tree check and stages on branch public-test (never public-main)
  with the message "TEST - not for publishing". Cannot be combined with -Push.
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\publish-public.ps1
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\publish-public.ps1 -Push
#>
param(
  [switch]$Push,
  [string]$Message = '',
  [switch]$AllowDirty,
  [string]$PublicRepo = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $PublicRepo) { $PublicRepo = Join-Path (Split-Path -Parent $root) 'ov-refund-public' }
Set-Location $root
. (Join-Path $PSScriptRoot 'leak-scan.ps1')

$PublicIdentityName = 'OV Refund'
$PublicIdentityMail = 'ov-refund@users.noreply.github.com'
$ReleaseDir = Join-Path $env:USERPROFILE '.ovrefund-release'
$TermsFile = Join-Path $ReleaseDir 'leak-terms.txt'
$ExcludeFile = Join-Path $ReleaseDir 'public-exclude.txt'
$PublicUrlPattern = '^(https://github\.com/|git@github\.com:|ssh://git@github\.com/)ov-refund/OV-Refund(\.git)?/?$'
$Branch = if ($AllowDirty) { 'public-test' } else { 'public-main' }

function Invoke-Git {
  # git writes progress to stderr; judge by exit code only.
  $ErrorActionPreference = 'Continue'
  $out = & git @args
  $code = $LASTEXITCODE
  $ErrorActionPreference = 'Stop'
  if ($code -ne 0) { throw "git $($args -join ' ') failed (exit $code)" }
  $out
}
function Get-FullPath([string]$p) { [IO.Path]::GetFullPath($p).TrimEnd('\', '/') }
function Get-RemoteUrls([string]$repo) {
  # Fetch URL plus every push URL (a pushurl can differ from the fetch URL).
  @(Invoke-Git -C $repo remote get-url origin) + @(Invoke-Git -C $repo remote get-url --push --all origin)
}
function Test-BranchExists([string]$name) {
  $ErrorActionPreference = 'Continue'
  & git -C $PublicRepo show-ref --verify --quiet "refs/heads/$name"
  $ok = $LASTEXITCODE -eq 0
  $ErrorActionPreference = 'Stop'
  $ok
}
# Back to main in the public clone (also when main is still unborn, as in a clone of an empty repository).
function Restore-PublicMain {
  $ErrorActionPreference = 'Continue'
  if (Test-BranchExists 'main') { & git -C $PublicRepo checkout -q -f main }
  else { & git -C $PublicRepo symbolic-ref HEAD refs/heads/main; & git -C $PublicRepo read-tree --empty }
  & git -C $PublicRepo clean -fdxq
  $ErrorActionPreference = 'Stop'
}
function Remove-StagedBranches {
  $ErrorActionPreference = 'Continue'
  $current = & git -C $PublicRepo symbolic-ref --short -q HEAD
  $ErrorActionPreference = 'Stop'
  if ($current -eq 'public-main' -or $current -eq 'public-test') { Restore-PublicMain }
  foreach ($b in @('public-main', 'public-test')) {
    if (Test-BranchExists $b) { Invoke-Git -C $PublicRepo branch -q -D $b | Out-Null; Write-Host "Deleted branch $b in the public clone." -ForegroundColor Yellow }
  }
}

# ---- 1. checks ----
if ($Push -and $AllowDirty) { throw '-AllowDirty is for testing only and cannot be combined with -Push.' }
$prevOutputEncoding = [Console]::OutputEncoding
try { [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false) } catch { }   # read git's UTF-8 path names correctly
$branchNow = Invoke-Git rev-parse --abbrev-ref HEAD
if ($branchNow -ne 'main') { throw "Publish from the main branch (now on $branchNow)." }
if (-not $AllowDirty -and (Invoke-Git status --porcelain)) { throw 'Commit or stash your changes first (working tree not clean).' }
if (-not (Test-Path -LiteralPath $TermsFile)) { throw "Leak-term list missing ($TermsFile). Refusing to continue." }
if (-not (Test-Path -LiteralPath $ExcludeFile)) { throw "Exclude list missing ($ExcludeFile). Refusing to continue." }
if (-not (Test-Path -LiteralPath (Join-Path $PublicRepo '.git'))) { throw "No git repository at $PublicRepo" }
$PublicRepo = Get-FullPath $PublicRepo
$rootTop = Get-FullPath ((Invoke-Git rev-parse --show-toplevel) -replace '/', '\')
$publicTop = Get-FullPath ((Invoke-Git -C $PublicRepo rev-parse --show-toplevel) -replace '/', '\')
if ($publicTop -ne $PublicRepo) { throw "$PublicRepo is not the top folder of its git repository ($publicTop)." }
if ($rootTop -eq $publicTop) { throw 'This script runs from the private develop repository, not from the public clone.' }
$ErrorActionPreference = 'Continue'
$ownUrls = @(& git remote get-url origin 2>$null) + @(& git remote get-url --push --all origin 2>$null)
$ErrorActionPreference = 'Stop'
foreach ($u in $ownUrls) { if ("$u" -match $PublicUrlPattern) { throw "origin of this repository is the public repository ($u). Run this from the private develop repository." } }
foreach ($u in (Get-RemoteUrls $PublicRepo)) { if ("$u" -notmatch $PublicUrlPattern) { throw "origin of $PublicRepo is not ov-refund/OV-Refund ($u)." } }
if (Invoke-Git -C $PublicRepo status --porcelain) { throw "The public clone has uncommitted changes: $PublicRepo" }

# Exclude list: validated before anything is exported.
$excludes = @()
foreach ($line in [IO.File]::ReadAllLines($ExcludeFile)) {
  $e = $line.Trim()
  if (-not $e -or $e.StartsWith('#')) { continue }
  $e = $e -replace '\\', '/'
  if ($e -match '^/' -or $e -match '^[A-Za-z]:' -or $e -match '(^|/)\.\.(/|$)') { throw "Exclude list entry #$($excludes.Count + 1) is absolute or contains '..': $e" }
  $dirOnly = $e.EndsWith('/')
  $e = $e.TrimEnd('/')
  if (-not $e) { throw 'Exclude list contains an empty folder entry.' }
  $rx = '^' + ([regex]::Escape($e) -replace '\\\*\\\*', '.*' -replace '\\\*', '[^/]*' -replace '\\\?', '[^/]') + '$'
  $excludes += [pscustomobject]@{ Text = $e; DirOnly = $dirOnly; Anchored = $e.Contains('/'); Rx = (New-Object Text.RegularExpressions.Regex ($rx, $LeakRegexOptions)); Used = 0 }
}
if ($excludes.Count -eq 0) { throw "Exclude list has no entries ($ExcludeFile). Refusing to continue." }
function Test-Excluded([string]$rel) {
  $segments = $rel.Split('/')
  $hit = $false
  foreach ($x in $excludes) {
    $match = $false
    if ($x.Anchored) {
      $prefix = ''
      for ($i = 0; $i -lt $segments.Count -and -not $match; $i++) {
        $prefix = if ($i -eq 0) { $segments[0] } else { "$prefix/$($segments[$i])" }
        $isDir = $i -lt $segments.Count - 1
        if ((-not $x.DirOnly -or $isDir) -and $x.Rx.IsMatch($prefix)) { $match = $true }
      }
    } else {
      for ($i = 0; $i -lt $segments.Count -and -not $match; $i++) {
        $isDir = $i -lt $segments.Count - 1
        if ((-not $x.DirOnly -or $isDir) -and $x.Rx.IsMatch($segments[$i])) { $match = $true }
      }
    }
    if ($match) { $x.Used++; $hit = $true }
  }
  $hit
}

if (-not $Message) {
  $cargo = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw
  $version = [regex]::Match($cargo, '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"').Groups[1].Value
  if (-not $version) { $version = [regex]::Match($cargo, '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"').Groups[1].Value }
  if (-not $version) { $version = (Get-Content -LiteralPath (Join-Path $root 'src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json).version }
  if (-not $version) { throw 'Version not found in Cargo.toml or src-tauri/tauri.conf.json; pass -Message.' }
  $Message = "OV Refund $version"
}
if ($AllowDirty) { $Message = 'TEST ' + [char]0x2014 + ' not for publishing' }

$temp = Join-Path ([IO.Path]::GetTempPath()) ('ovrefund-public-' + [guid]::NewGuid().ToString('N'))
$tree = Join-Path $temp 'tree'
$failed = $false
$savedDates = @($env:GIT_AUTHOR_DATE, $env:GIT_COMMITTER_DATE)
try {
  # ---- 2. export ----
  New-Item -ItemType Directory -Path $tree -Force | Out-Null
  if ($AllowDirty) {
    Write-Host "Exporting the WORKING TREE (-AllowDirty, testing only; stages on $Branch)." -ForegroundColor Yellow
    $files = (Invoke-Git -c core.quotepath=off ls-files -co --exclude-standard) | Sort-Object -Unique
    foreach ($f in $files) {
      $src = Join-Path $root $f
      if (-not (Test-Path -LiteralPath $src -PathType Leaf)) { continue }  # deleted in the working tree
      $dst = Join-Path $tree $f
      New-Item -ItemType Directory -Path (Split-Path -Parent $dst) -Force | Out-Null
      Copy-Item -LiteralPath $src -Destination $dst -Force
    }
  } else {
    $tar = Join-Path $temp 'head.tar'
    Invoke-Git -c core.autocrlf=false archive --format=tar -o $tar HEAD | Out-Null
    & (Join-Path $env:SystemRoot 'System32\tar.exe') -xf $tar -C $tree
    if ($LASTEXITCODE -ne 0) { throw "tar failed (exit $LASTEXITCODE)" }
    Remove-Item -LiteralPath $tar -Force
  }

  # ---- 3. private files ----
  $removed = 0
  foreach ($file in @(Get-ChildItem -LiteralPath $tree -Recurse -File -Force)) {
    $rel = $file.FullName.Substring($tree.Length + 1) -replace '\\', '/'
    if (Test-Excluded $rel) { Remove-Item -LiteralPath $file.FullName -Force; $removed++ }
  }
  # Folders left empty by the removal are not committed by git, but keep the tree tidy.
  Get-ChildItem -LiteralPath $tree -Recurse -Directory -Force | Sort-Object { $_.FullName.Length } -Descending | ForEach-Object {
    if (-not (Get-ChildItem -LiteralPath $_.FullName -Force)) { Remove-Item -LiteralPath $_.FullName -Force }
  }
  Write-Host "Private files removed: $removed"
  $k = 0
  foreach ($x in $excludes) { $k++; if ($x.Used -eq 0) { Write-Host "Note: exclude entry #$k matched nothing." -ForegroundColor Yellow } }

  # ---- 4. scan ----
  $terms = Read-LeakTerms $TermsFile
  $hits = New-Object System.Collections.Generic.List[string]
  $seen = @{}
  function Add-Hits([string]$where, $found) {
    foreach ($h in $found) {
      $key = if ($where -match '^\(') { "$where $($h.What)" } else { "${where}:$($h.Line) $($h.What)" }
      if (-not $seen.ContainsKey($key)) { $seen[$key] = $true; $hits.Add($key) }
    }
  }
  $utf8 = New-Object Text.UTF8Encoding($false, $false)   # lenient: invalid bytes become U+FFFD
  $all = @(Get-ChildItem -LiteralPath $tree -Recurse -File -Force)
  $scanned = New-Object System.Collections.Generic.List[string]
  Add-Hits '(commit message)' (Find-Leaks -Text $Message -Terms $terms)
  foreach ($file in $all) {
    $rel = $file.FullName.Substring($tree.Length + 1) -replace '\\', '/'
    $scanned.Add($rel)
    Add-Hits "(path of file #$($scanned.Count))" (Find-Leaks -Text $rel -Terms $terms)
    $bytes = [IO.File]::ReadAllBytes($file.FullName)
    $binary = [Array]::IndexOf($bytes, [byte]0, 0, [Math]::Min($bytes.Length, 8000)) -ge 0
    if ($binary) {
      Add-Hits "$rel (UTF-8 char offset)" (Find-Leaks -Text $utf8.GetString($bytes) -Terms $terms -Binary)
      Add-Hits "$rel (UTF-16 char offset)" (Find-Leaks -Text ([Text.Encoding]::Unicode.GetString($bytes)) -Terms $terms -Binary)
    } else {
      Add-Hits $rel (Find-Leaks -Text $utf8.GetString($bytes).Replace("`r", '') -Terms $terms)
    }
  }
  if ($hits.Count -gt 0) {
    $hits | ForEach-Object { Write-Host "LEAK? $_" -ForegroundColor Red }
    Write-Host "Scan: $($all.Count) files, $($hits.Count) hit(s). Nothing was staged." -ForegroundColor Red
    $failed = $true
    Remove-StagedBranches
  } else {
    Write-Host "Scan: $($all.Count) files, no hits."
  }

  # ---- 5. stage in the public clone ----
  if (-not $failed) {
  Push-Location $PublicRepo
  try {
    # Re-check right before changing anything: the clean below must only ever run inside the public clone.
    if ((Get-FullPath ((Invoke-Git rev-parse --show-toplevel) -replace '/', '\')) -ne $PublicRepo) { throw 'Not inside the public clone.' }
    foreach ($u in (Get-RemoteUrls $PublicRepo)) { if ("$u" -notmatch $PublicUrlPattern) { throw "origin of $PublicRepo is not ov-refund/OV-Refund ($u)." } }
    $ErrorActionPreference = 'Continue'
    $current = & git symbolic-ref --short -q HEAD
    $ErrorActionPreference = 'Stop'
    if ($current -eq $Branch) { Restore-PublicMain }
    if (Test-BranchExists $Branch) { Invoke-Git branch -q -D $Branch | Out-Null }
    Invoke-Git checkout -q --orphan $Branch | Out-Null
    try {
      Invoke-Git rm -r -f -q --ignore-unmatch . | Out-Null
      Invoke-Git clean -fdxq | Out-Null   # also ignored files: only the scanned tree may be committed
      & robocopy $tree $PublicRepo /E /XD .git /NFL /NDL /NJH /NJS /NP | Out-Null
      if ($LASTEXITCODE -ge 8) { throw "copy failed (robocopy exit $LASTEXITCODE)" }
      # -f: the exported tree is exactly what is committed, even if a tracked file matches .gitignore.
      Invoke-Git -c core.autocrlf=false add -A -f | Out-Null
      $staged = @(((Invoke-Git -c core.quotepath=off ls-files -z) -join '').Split([char]0) | Where-Object { $_ })
      $want = [string[]]@($scanned); [Array]::Sort($want, [StringComparer]::Ordinal)
      $have = [string[]]$staged; [Array]::Sort($have, [StringComparer]::Ordinal)
      if ($want.Count -ne $have.Count -or (($want -join "`n") -cne ($have -join "`n"))) {
        throw "Staged files differ from the scanned files ($($have.Count) staged, $($want.Count) scanned)."
      }
      # Date only, in UTC: no time of day or time zone in the public history.
      $day = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd') + 'T00:00:00Z'
      $env:GIT_AUTHOR_DATE = $day; $env:GIT_COMMITTER_DATE = $day
      $msgFile = Join-Path $temp 'message.txt'
      [IO.File]::WriteAllText($msgFile, $Message, (New-Object Text.UTF8Encoding $false))
      Invoke-Git -c core.autocrlf=false -c "user.name=$PublicIdentityName" -c "user.email=$PublicIdentityMail" commit -q -F $msgFile | Out-Null
    } catch {
      Write-Host 'Staging failed; restoring the public clone to main.' -ForegroundColor Red
      Restore-PublicMain
      $ErrorActionPreference = 'Continue'; & git branch -q -D $Branch; $ErrorActionPreference = 'Stop'
      throw
    }
    $hash = Invoke-Git rev-parse HEAD
    Write-Host ''
    Write-Host "Staged in $PublicRepo on branch $Branch (one commit, no history)." -ForegroundColor Green
    Write-Host "  commit : $hash"
    Write-Host "  message: $Message"
    Write-Host "  files  : $($staged.Count) (same as scanned)"
    Write-Host '  layout :'
    Invoke-Git ls-tree --name-only HEAD | ForEach-Object { Write-Host "    $_" }

    # ---- 6. push ----
    if ($AllowDirty) {
      Write-Host ''
      Write-Host "TEST staging only. Branch $Branch must never be published; go back with: git -C `"$PublicRepo`" checkout main" -ForegroundColor Yellow
    } elseif ($Push) {
      Write-Host ''
      Write-Host 'Pushing: git push --force origin public-main:main' -ForegroundColor Yellow
      Invoke-Git push --force origin public-main:main | Out-Null
      Write-Host 'Pushed. The website is published from site/ by .github/workflows/pages.yml (Settings -> Pages -> Source: GitHub Actions).'
    } else {
      Write-Host ''
      Write-Host 'Nothing was pushed. Review, then publish with (replaces the public history):'
      Write-Host "  git -C `"$PublicRepo`" push --force origin public-main:main"
      Write-Host "Back to the old state: git -C `"$PublicRepo`" checkout main"
    }
  } finally { Pop-Location }
  }
} finally {
  $env:GIT_AUTHOR_DATE = $savedDates[0]; $env:GIT_COMMITTER_DATE = $savedDates[1]
  try { [Console]::OutputEncoding = $prevOutputEncoding } catch { }
  if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Recurse -Force }
}
if ($failed) { exit 1 }
