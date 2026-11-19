# Installed updater integration test

`update-test.yml` runs on `windows-latest` for pushes to `update-test` only,
or via **Actions > Update and removal test > Run workflow**. Manual dispatch
requires the workflow to be present on the default branch. It uses no repository
secrets and never publishes anything. Logs are uploaded even after failure.

Run these scripts only on a disposable Actions runner: they install/uninstall the
app and exercise real per-user data, scheduled tasks and Credential Manager.
Dependencies and runner setup are downloaded by Actions; the update scenarios
use a loopback Python HTTP server. Local authoring/validation requires no network.

Both NSIS builds use JSON config overrides, `createUpdaterArtifacts: true`,
`--no-sign`, and `devtools,custom-protocol,tauri/devtools`. Only the finished new
installer is signed, by invoking the installed CLI in a minimal child environment.
The random disposable key/password are never uploaded and are deleted after
signing, with an always-run cleanup as a fallback. Config overrides also enable
insecure updater transport solely for the loopback HTTP test.

`history` creates/migrates `%LOCALAPPDATA%/OVRefund/ovrefund.sqlite` without NS
traffic. A synthetic settings row, scheduled task, and generic credential prove
preservation and removal. No demo mode or real credentials are used. The real
identifier's `EBWebView` cleanup path is seeded, so removal is asserted against
an existing fixture.

Update scenarios use the developer-build CLI commands `update-check` and
`update-install` (feature `devtools`, absent from release builds). They run the
app's own `check_update` / `install_update` commands without a window: the same
`OVREFUND_UPDATE_URL` override, signature check and run-lock guard as the
in-app button, printing one JSON line (`update-install` exits 3 with
`{"error":"signature"}` when verification fails). A successful install starts
the installer and exits 0; the installer then restarts the app with the same
arguments, which finds no newer version and exits. Earlier CDP-based driving of
the GUI did not work: the app's own WebView2 browser arguments override
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, so no debugging port opened.

The hook has no custom silent delete-data switch. The test emulates the checkbox
branch by copying the installed executable into the temporary test directory,
silently uninstalling, then invoking that copy's `forget-all`. This reproduces
the final wipe after `forget-login` (which can recreate the database). It does
not test the graphical checkbox itself. The copy is deleted immediately.

Tauri's configured package version controls update comparisons and Windows file
resources. The app's diagnostic `current` field instead uses the unchanged Cargo
manifest version; assertions deliberately use installed file versions and offered
versions. Neither the manifest nor the tracked Tauri config is rewritten.

Offline validation cannot establish runner GUI availability, WebView2 setup,
NSIS timing, or execution of the complete install/update/uninstall chain. The
workflow fails with bounded waits if these assumptions do not hold. It proves
the two test builds on that runner, rather than every historical public binary.
