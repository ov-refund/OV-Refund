<#
.SYNOPSIS
  Shared private-data checks for tools/publish-public.ps1 (source snapshot) and tools/release.ps1
  (built app and installer). Dot-source this file: . (Join-Path $PSScriptRoot 'leak-scan.ps1')
.DESCRIPTION
  Checks, on the raw text and on a copy with common escapes decoded (&#64; &#x40; &commat; %40 \u0040):
    - private terms: %USERPROFILE%\.ovrefund-release\leak-terms.txt, one regular expression per line,
      lines starting with # are comments. Hits are reported by line number of that file only.
    - user folders: C:\Users\<name>, C:/Users/<name>, /c/Users/<name>, /home/<name>, /Users/<name>
      (placeholder names from tests are allowed).
    - e-mail addresses: only the exact test fixtures below and reserved example domains are allowed.
#>

$LeakRegexOptions = [Text.RegularExpressions.RegexOptions]::IgnoreCase -bor [Text.RegularExpressions.RegexOptions]::CultureInvariant

# Placeholder user names used by tests in this repository.
$LeakAllowedUsers = @('test', 'x', 'a', 'private-name', 'someone-with-a-long-name')

# Exact e-mail addresses that are test fixtures in this repository (NS senders, look-alike senders that must be
# rejected, provider examples for the mail setup) plus the public commit identity. Add new fixtures here by hand.
$LeakAllowedMails = @(
  'noreply@ns.nl', 'noreply.ns@ns.nl', 'x@mail.ns.nl',
  'noreply@evilns.nl', 'noreply@ns.nl.attacker.com',
  'name@domain.tld',
  'test-a@gmail.com', 'test-a@hotmail.com', 'test-a@outlook.be', 'test-a@outlook.com', 'test-a@planet.nl',
  'test-b@gmail.com', 'test-jan@gmail.com', 'test-jan@hotmail.com', 'test-jan@outlook.nl',
  'test-me@gmail.com', 'test-me@ziggo.nl', 'test-x@hotmail.nl', 'test-x@live.be', 'test-x@mylive.com',
  'test-x@outlook.com', 'test-x@outlook.company.nl', 'test-y@msn.com', 'test-z@hotmail.nl',
  'ov-refund@users.noreply.github.com',
  # Compiled into ov-refund.exe by dependencies: OpenSSL credits and a root certificate (webpki roots).
  'appro@openssl.org', 'info@e-szigno.hu'
)

$LeakUserPathRx = New-Object Text.RegularExpressions.Regex (
  '(?:\b[A-Za-z]:[\\/]+Users|(?<![A-Za-z0-9._~-])/(?:[A-Za-z]/)?Users|(?<![A-Za-z0-9._~-])/home)[\\/]+([^\\/\s"''<>|:*?]+)', $LeakRegexOptions)
$LeakMailRx = New-Object Text.RegularExpressions.Regex ('[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}', $LeakRegexOptions)
# Binary data (images, icons, executables) often contains short random look-alikes of addresses; there only
# lower-case domains with labels of 2+ characters count.
$LeakBinMailRx = New-Object Text.RegularExpressions.Regex ('[A-Za-z0-9._%+-]{2,}@[a-z0-9-]{2,}(\.[a-z0-9-]{2,})*\.[a-z]{2,}')
$LeakEscapeRx = New-Object Text.RegularExpressions.Regex ('&#(\d{1,7});|&#[xX]([0-9a-fA-F]{1,6});|\\u([0-9a-fA-F]{4})|%([0-9a-fA-F]{2})|&commat;')

function Read-LeakTerms([string]$Path) {
  if (-not (Test-Path -LiteralPath $Path)) { throw "Leak-term list missing ($Path). Refusing to continue." }
  $terms = New-Object System.Collections.Generic.List[object]
  $n = 0
  foreach ($line in [IO.File]::ReadAllLines($Path)) {
    $n++
    $t = $line.Trim()
    if (-not $t -or $t.StartsWith('#')) { continue }
    try { $rx = New-Object Text.RegularExpressions.Regex ($t, $LeakRegexOptions) }
    catch { throw "private term #$n is not a valid regular expression." }
    $terms.Add([pscustomobject]@{ N = $n; Rx = $rx })
  }
  if ($terms.Count -eq 0) { throw 'Leak-term list is empty. Refusing to continue.' }
  return ,$terms
}

function Test-LeakAllowedUser([string]$Name) {
  if ($Name -match '^[$%{\[]') { return $true }   # placeholders like $env:USERNAME, %USERNAME%, {user}
  return ($LeakAllowedUsers -contains $Name.ToLowerInvariant())
}

function Test-LeakAllowedMail([string]$Address) {
  $a = $Address.ToLowerInvariant()
  if ($LeakAllowedMails -contains $a) { return $true }
  $domain = $a.Split('@', 2)[1]
  if ($domain -match '\.(png|webp|jpe?g|gif|svg|ico)[a-z]?$') { return $true }   # asset names like icon@2x.png (in binaries + 1 byte)
  if ($domain -match '(^|\.)example\.(com|org|net|nl)$') { return $true }
  if ($domain -match '(^|\.)(example|invalid|test)$') { return $true }
  return $false
}

# Decodes &#64; &#x40; &commat; %40 \u0040 and similar. Control characters stay encoded, so line numbers do not move.
function ConvertFrom-LeakEscapes([string]$Text) {
  if (-not $LeakEscapeRx.IsMatch($Text)) { return $Text }
  return $LeakEscapeRx.Replace($Text, [Text.RegularExpressions.MatchEvaluator] {
      param($m)
      $code = -1
      if ($m.Value -eq '&commat;') { return '@' }
      if ($m.Groups[1].Success) { $code = [int]$m.Groups[1].Value }
      elseif ($m.Groups[2].Success) { $code = [Convert]::ToInt32($m.Groups[2].Value, 16) }
      elseif ($m.Groups[3].Success) { $code = [Convert]::ToInt32($m.Groups[3].Value, 16) }
      elseif ($m.Groups[4].Success) { $code = [Convert]::ToInt32($m.Groups[4].Value, 16) }
      if ($code -lt 32 -or $code -gt 0xFFFF -or ($code -ge 0xD800 -and $code -le 0xDFFF)) { return $m.Value }
      return [string][char]$code
    })
}

# Returns the hits in $Text as objects { Line; What }. Line is the 1-based line number for text,
# or the character offset when -Binary is given.
function Find-Leaks {
  param([string]$Text, $Terms, [switch]$Binary)
  $found = New-Object System.Collections.Generic.List[object]
  $variants = @($Text)
  $decoded = ConvertFrom-LeakEscapes $Text
  if ($decoded -cne $Text) { $variants += $decoded }
  foreach ($v in $variants) {
    $starts = $null
    if (-not $Binary) {
      $starts = New-Object System.Collections.Generic.List[int]
      $starts.Add(0)
      for ($i = $v.IndexOf("`n"); $i -ge 0; $i = $v.IndexOf("`n", $i + 1)) { $starts.Add($i + 1) }
    }
    $where = {
      param([int]$index)
      if ($Binary) { return $index }
      $k = $starts.BinarySearch($index)
      if ($k -lt 0) { $k = (-bnot $k) - 1 }
      return $k + 1
    }
    foreach ($t in $Terms) {
      foreach ($m in $t.Rx.Matches($v)) { $found.Add([pscustomobject]@{ Line = (& $where $m.Index); What = "private term #$($t.N)" }) }
    }
    foreach ($m in $LeakUserPathRx.Matches($v)) {
      if (-not (Test-LeakAllowedUser $m.Groups[1].Value)) { $found.Add([pscustomobject]@{ Line = (& $where $m.Index); What = 'user folder path' }) }
    }
    $mailRx = if ($Binary) { $LeakBinMailRx } else { $LeakMailRx }
    foreach ($m in $mailRx.Matches($v)) {
      if (-not (Test-LeakAllowedMail $m.Value)) { $found.Add([pscustomobject]@{ Line = (& $where $m.Index); What = 'e-mail address' }) }
    }
  }
  return ,$found
}

# Scans one built file as bytes (UTF-8 and UTF-16LE). Prints each hit (location and kind only) and returns the count.
function Test-BuiltFileLeaks([string]$Path, $Terms) {
  $bytes = [IO.File]::ReadAllBytes($Path)
  $name = Split-Path -Leaf $Path
  $seen = @{}
  $encodings = @(@('UTF-8', (New-Object Text.UTF8Encoding($false, $false))), @('UTF-16LE', [Text.Encoding]::Unicode))
  foreach ($e in $encodings) {
    foreach ($h in (Find-Leaks -Text $e[1].GetString($bytes) -Terms $Terms -Binary)) {
      $key = "${name} ($($e[0]), char $($h.Line)): $($h.What)"
      if (-not $seen.ContainsKey($key)) { $seen[$key] = $true; Write-Host "LEAK? $key" -ForegroundColor Red }
    }
  }
  return $seen.Count
}
