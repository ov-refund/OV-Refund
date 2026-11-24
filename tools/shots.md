# Website screenshots

From the repository root, use Node 22+ (built-in WebSocket) and an existing
**devtools-enabled** demo executable. No packages or downloads are needed.
The script connects only to loopback CDP and expects a freshly started demo
at Welcome. It requires the app's DEMO banner before acting.

PowerShell wrapper for all four variants (adjust the executable path):

```powershell
$demoExe = (Resolve-Path '.\target\fastrelease\ov-refund.exe').Path
foreach ($shotsLang in @('nl', 'en')) {
    foreach ($shotsTheme in @('light', 'dark')) {
        $env:LANG_CODE = $shotsLang
        $env:THEME = $shotsTheme
        $env:PORT = '9336'
        $env:OUTDIR = "site/assets/shots/$shotsLang$(if ($shotsTheme -eq 'dark') { '-dark' })"
        $env:OVREFUND_DEMO_INSTANCE = "shots${shotsLang}${shotsTheme}"
        $shotsProfile = Join-Path ([IO.Path]::GetTempPath()) ('ovrefund-shots-' + [guid]::NewGuid().ToString('N'))
        $env:WEBVIEW2_USER_DATA_FOLDER = $shotsProfile
        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9336'
        $demoProcess = $null
        try {
            $demoProcess = Start-Process -FilePath $demoExe -ArgumentList '--demo' -WindowStyle Hidden -PassThru
            node tools/shots.mjs
            if ($LASTEXITCODE -ne 0) { throw 'Screenshot capture failed; see the named selector above.' }
        } finally {
            if ($demoProcess -and -not $demoProcess.HasExited) {
                Stop-Process -Id $demoProcess.Id -Force
                $demoProcess.WaitForExit()
            }
            # Verify the absolute target before recursive deletion.
            $shotsTempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
            $shotsResolvedProfile = [IO.Path]::GetFullPath($shotsProfile)
            if (-not $shotsResolvedProfile.StartsWith($shotsTempRoot, [StringComparison]::OrdinalIgnoreCase) -or
                [IO.Path]::GetFileName($shotsResolvedProfile) -notlike 'ovrefund-shots-*') {
                throw 'Refusing to remove a profile outside the temporary shots directory.'
            }
            # WebView2 may briefly hold its profile after the app closes.
            for ($attempt = 0; $attempt -lt 20; $attempt++) {
                try {
                    if (Test-Path -LiteralPath $shotsResolvedProfile) {
                        Remove-Item -LiteralPath $shotsResolvedProfile -Recurse -Force -ErrorAction Stop
                    }
                    break
                } catch {
                    if ($attempt -eq 19) { throw }
                    Start-Sleep -Milliseconds 250
                }
            }
            Remove-Item Env:WEBVIEW2_USER_DATA_FOLDER, Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, Env:OVREFUND_DEMO_INSTANCE -ErrorAction SilentlyContinue
        }
    }
}
```

For a single variant, use `@('nl')` and `@('light')` in the loops (or `en` /
`dark`). If the maintainer already started that demo with the environment
above, the capture command is simply `node tools/shots.mjs` with `PORT`,
`LANG_CODE`, `THEME`, and `OUTDIR` set in that shell.

Run sequentially. The current `src-tauri/src/demo.rs` accepts only alphanumeric
instance names (at most 16 characters), so the requested `shots-nl-light` style
names currently fall back to the default demo namespace. Keep other demo
instances closed; if namespace isolation is needed, use `shotsnllight`,
`shotsnldark`, `shotsenlight`, or `shotsendark` instead. Each demo start resets
its demo database. The wrapper removes its temporary WebView2 profile; the
capture script creates no temporary files.

Each invocation prints twelve saved paths: `01-welcome` through `12-progress`
under `site/assets/shots/nl`, `nl-dark`, `en`, or `en-dark`. Wizard panels are
cropped in a 1280x1000 viewport; main views use 1280x820; dialogs are cropped
with a 24px margin. WebP quality is 88, capture scale is 1.5. The progress shot
crops the top bar and polls for the real month text/yellow arc immediately
after clicking Check now (normally 300-800ms); missed frames retry up to three
checks and then fail with the selector. A failure exits nonzero; restart the
demo before rerunning the complete flow.

Only fictional people, the example IBAN `NL91ABNA0417164300`, and reserved
example.com mailboxes are used. Displayed demo/help addresses are replaced
before capture. The optional notification mail section is expanded for its
shot, then closed to continue: example.com has no supported SMTP preset.
No mail test or claim submission is triggered.

Yellow rings are baked into the images. Labels remain website HTML:
`shots.mjs` writes `site/assets/shots/hints.js` directly, merging the existing
JSON object into `export default { "nl": { "01-welcome": {x,y,side,text} }, ... };`.
Coordinates are percentages of the captured clip. Other languages and shot
names are preserved; recaptured shots without annotations lose their old hint.
Both theme folders merge into this common parent file. `site/assets/site.js`
looks up `HINTS[lang][name]`, without a theme key; the last captured theme sets
that language's shared positions. There is no hints.json conversion step.
Keep this shared hints.js with all four sets of generated images.

Offline syntax validation:

```powershell
node --check tools/shots.mjs
```

The homepage uses focused `00-home.webp` captures assembled from real demo totals and four
journeys, with one claim expanded to show its amount, calculation and review action. It is labelled as a simplified
example, and NS's assessment caveat is retained. To refresh its four variants with isolated profiles and automatic credential/data cleanup:

```powershell
foreach ($lang in @('nl', 'en')) {
    foreach ($theme in @('light', 'dark')) {
        powershell -NoProfile -ExecutionPolicy Bypass -File tools/run-demo-review.ps1 -HomeScreenshot -Lang $lang -Theme $theme
        if ($LASTEXITCODE -ne 0) { throw 'Homepage capture failed' }
    }
}
```

Build the frontend and the `fastrelease` demo first as described in `gui-smoke.md`.
`shots.mjs --home-only` skips writing the instruction images. Screenshot preparation disables
Windows notifications and never sends mail or claims.
