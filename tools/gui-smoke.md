# GUI smoke test

`tools/gui-smoke.mjs` drives the real app in **demo mode** over loopback CDP
(WebView2 `--remote-debugging-port`), like `tools/shots.mjs`. It needs Node 22+
(built-in WebSocket) and an existing **devtools-enabled** demo executable. No
packages or downloads are needed, it writes no files, and it refuses to act
until the app shows its DEMO banner. Nothing is sent to NS: the demo uses
example data and a simulated claim gateway.

Checks (each prints `PASS` or `FAIL`; any failure exits nonzero):

1. Setup: finishes the setup wizard with fictional data (automatic checks off,
   no automatic login, no mail), logs in (demo) and waits for trips after the
   first check.
2. Submit dialog: real mouse-wheel input (`Input.dispatchMouseEvent`,
   `mouseWheel`) over the backdrop and over the dialog; the dialog stays
   `position: fixed`, fully inside the viewport, with its header and close
   button visible and not covered.
3. Settings at 1040x720: the same, on the Account, Help and Erase data tabs.
4. Bug dialog: the same.
5. "Simuleer automatische controle": succeeds (an "Automatische controle: …"
   message, no "Er loopt al een controle" or error) and the saved mode is the
   same before and after.
6. ⋯ Meer: opens a row menu, presses a real Esc key; the menu closes and focus
   returns to its button.
7. Language toggle: `<html lang>` switches to the other language and back.
8. No console errors, log errors or uncaught exceptions during the run.

The viewport is 1040x720 throughout. Use a fresh demo start for every run.

From the repository root in PowerShell (adjust the executable path):

```powershell
$demoExe = (Resolve-Path '.\target\fastrelease\ov-refund.exe').Path
$env:PORT = '9337'
$env:OVREFUND_DEMO_INSTANCE = 'guismoke'   # alphanumeric, at most 16 characters
$smokeProfile = Join-Path ([IO.Path]::GetTempPath()) ('ovrefund-smoke-' + [guid]::NewGuid().ToString('N'))
$env:WEBVIEW2_USER_DATA_FOLDER = $smokeProfile
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9337'
$demoProcess = $null
try {
    $demoProcess = Start-Process -FilePath $demoExe -ArgumentList '--demo' -PassThru
    node tools/gui-smoke.mjs
    if ($LASTEXITCODE -ne 0) { Write-Warning 'GUI smoke test failed; see the FAIL lines above.' }
} finally {
    if ($demoProcess -and -not $demoProcess.HasExited) {
        Stop-Process -Id $demoProcess.Id -Force
        $demoProcess.WaitForExit()
    }
    # Remove only the temporary WebView2 profile and this demo instance's data folder.
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    foreach ($dir in @($smokeProfile, (Join-Path $tempRoot 'OVRefund-demo-guismoke'))) {
        $full = [IO.Path]::GetFullPath($dir)
        if (-not $full.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) { throw "Refusing to remove $full" }
        # WebView2 may briefly hold its profile after the app closes.
        for ($attempt = 0; $attempt -lt 20; $attempt++) {
            try {
                if (Test-Path -LiteralPath $full) { Remove-Item -LiteralPath $full -Recurse -Force -ErrorAction Stop }
                break
            } catch {
                if ($attempt -eq 19) { throw }
                Start-Sleep -Milliseconds 250
            }
        }
    }
    Remove-Item Env:WEBVIEW2_USER_DATA_FOLDER, Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, Env:OVREFUND_DEMO_INSTANCE, Env:PORT -ErrorAction SilentlyContinue
}
```

The window is left visible on purpose: real wheel input needs a laid-out,
hit-testable page. Keep other demo instances on this port closed. The demo
instance name gives a separate data folder under `%TEMP%` and a separate
credential namespace, so the normal demo and the real data folder are never
touched.

Offline syntax validation:

```powershell
node --check tools/gui-smoke.mjs
```

For a fresh isolated demo with automatic cleanup (including its synthetic bank credential):

```powershell
cargo build --locked --profile fastrelease --features custom-protocol,devtools -p ov-refund
powershell -NoProfile -ExecutionPolicy Bypass -File tools/run-demo-review.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File tools/run-demo-review.ps1 -AutoClaims
```

Build the frontend with `npm run build` first. The automatic-claim driver needs Node 22.13+
for `node:sqlite`. It refuses any fixture path outside its randomly named demo directory in
TEMP. Between idle runs it resets only that demo ledger, restoring its append-only trigger
inside the same transaction. It never opens the real ledger or logs into NS.

The ten automatic checks cover prior uncertainty, missing bank details, claim-service consent,
Stop during a pause, bank removal during a pause, consent withdrawal during a pause, a matching
claimed-elsewhere import during a pause, the 14-day Check now window, full scheduled submission
with duplicate prevention and review-only exclusions, and absence of external web requests or
console exceptions. Paused runs start through the actual simulation button. The GUI smoke test
also checks keyboard focus after preflight and the submit hint's full-width row.
