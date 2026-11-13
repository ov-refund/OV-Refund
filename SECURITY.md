# Security policy

Your data stays local. Passwords and bank details are stored in Windows Credential Manager. Bug reports go through Web3Forms, and only after you agree. Logging out keeps the account link. Forget everything erases it.

## Reporting

Please report vulnerabilities privately via GitHub Security Advisories ("Report a vulnerability"), not in a public issue. Never include real passwords, cookies, IBANs or card numbers in a report.

## Threat model and design

| Asset | Where it lives | Protection |
|---|---|---|
| NS e-mail + password (only if *Log in to NS automatically* is on) | Windows Credential Manager (`OVRefund/ns-login`) | DPAPI, per Windows user. You enter them under Settings → Account & NS. The app types them only into NS's own login form on `login.ns.nl`, once per run. Never logged, e-mailed or exported. Turning the option off, or *Log out of NS*, deletes them. |
| NS account e-mail and notification addresses | SQLite settings | Plain text. Removed by *Forget everything*. |
| NS session (cookies, incl. the 180-day `authToken`) | WebView2 profile `%LOCALAPPDATA%\nl.ovrefund.app\EBWebView`, plus a cookie snapshot `%LOCALAPPDATA%\OVRefund\ns-session.bin` | Chromium cookie encryption (DPAPI, per Windows user). The snapshot is encrypted with `CryptProtectData` (current user). *Log out of NS* clears both. |
| Full OV-chipkaart number(s) | Windows Credential Manager (`OVRefund/ov-card-<last4>`), saved automatically when Mijn NS shows the card | DPAPI at rest. Shown in full in the app sidebar. The database, logs, diagnostics and e-mails hold only the last 4 digits. |
| NS bearer token / API key | Inside the NS page only | The app never extracts, stores or replays it. |
| Bank name + IBAN | Windows Credential Manager (`OVRefund/ns-bank-details`) | DPAPI. Typed only into the NS form. The UI masks the IBAN by default and can show it in full. |
| SMTP / mail app password | Windows Credential Manager | DPAPI. Used to send summaries. With *Read NS e-mails* on, also used to read NS delay-refund mails via IMAP (TLS). The app searches by subject and keeps only mail from an `ns.nl` sender (`BODY.PEEK`, so mail stays unread). It accepts a mail only when your mail provider reports DKIM/DMARC **pass** for `ns.nl` in `Authentication-Results`, so a forged `From:` is ignored. It stores only the decision, reference, amount and travel date, never the mail content. |
| Travel history, claims ledger | SQLite `%LOCALAPPDATA%\OVRefund\ovrefund.sqlite` | Per-user profile. Cards are stored as the last 4 digits only. |

- **IPC isolation:** only the local `main` window has a Tauri capability. The `ns` window (www.ns.nl) has none, so NS page script cannot call Rust commands.
- **CSP** on the local UI: `default-src 'self'`. No remote scripts.
- **No direct API calls:** only NS's own web app calls NS endpoints. OV Refund reads the responses through the WebView2 DevTools-protocol API of its own embedded browser.
- **No bypassing:** login pages, CAPTCHAs, bot challenges and unexpected HTTP statuses stop the run (`login_required` / `blocked`). Runs never retry in a loop, and only one run executes at a time (DB lease).
- **Submission safety:** the app journals `SUBMITTING` before the click. A crash leaves `INDETERMINATE`, which is never retried automatically. Automatic mode pauses until you resolve uncertain claims.
- **Delay classification:** historical timetable evidence supporting an arrival delay of at least 30 minutes follows the normal claim path, including matched cancellations and missed connections. These trips can be submitted automatically only with the existing opt-in and submission gates. A late check-out alone, a later train taken by choice, an unconfirmed replacement, unsupported routes and unresolved duplicates do not qualify for automatic submission. NS assesses each request.
- **Redaction:** logs, audit details, e-mails and diagnostics are filtered for tokens, cookies, IBANs (any case), card numbers, NS ids (UUIDs) and e-mail addresses (`crates/ovrefund-core/src/redact.rs`, unit-tested). Diagnostics use a field **whitelist**: counts and status, no scheduler command or exact run times. Check the file before sharing it.
- **Automatic login** types the stored password only after checking, right before typing, that the page is `https://login.ns.nl` or `https://loginapi.ns.nl` (exact host match).
- **Uninstall with "delete data"** removes the data folder, the scheduled task and every Credential Manager entry in the `OVRefund` and `OVRefund-demo` namespaces (found by enumeration). Diagnostics are in `%LOCALAPPDATA%\OVRefund\diagnostics` and go with the data folder. Copies elsewhere remain.
- **Bug reports** go only through the Web3Forms relay. They contain your text, cleaned of IBANs, card and phone numbers and e-mail addresses. A summary without personal data is optional. Keep personal data out of your text. No e-mail address or other contact details are sent. Like any website, Web3Forms sees your IP address. The authors delete reports once the problem is solved. Want a reply? Use GitHub Issues: public, under your own account.
- **NS terms:** `docs/ns-terms-review.md` reviews the NS-account terms of use.
- **Release:** the installer has no Authenticode publisher signature. Verify it with the published SHA-256 checksums. In-app updates are signed and verified before they install. Release builds replace local build folder paths with neutral ones, and the program and installer are checked for private data before they are signed.

## Optional automatic login

Measured: NS's login does **not** survive a new browser process, and restoring cookies alone does not reliably log in. Automatic login is optional. It stores the NS credentials in Windows Credential Manager and fills them in only on the NS login page. Turning automatic login off deletes those credentials. The app also does this after repeated failures or an account mismatch. Enter the credentials again to turn it back on.

Safeguards:
- DPAPI-protected Windows Credential Manager only; never in the DB, logs, diagnostics or e-mails
- one automatic attempt per run, never a retry loop
- any security check or unexpected page stops the run and notifies you
- you can turn the option off at any time

## Removing your data

| Action | What is removed |
|---|---|
| **Log out of NS** (Settings → Erase data) | First the stored NS password and the session snapshot. The stored account e-mail stays linked to the ledger until Forget everything. Then, as far as possible: NS cookies, storage of `www.ns.nl`, `login.ns.nl` and `loginapi.ns.nl`, and all WebView2 profile browsing data (history, disk cache, cookies and saved logins). |
| **Erase local history** (Settings → Erase data; type WISSEN / ERASE) | Travel history, assessments, runs, unlinked mails, the log and the diagnostics files. Mails linked to kept claims and claimed-elsewhere imports remain. Claims that NS knows about (submitted, approved, paid, claimed elsewhere) are kept, so nothing can be submitted twice. Your own exports (backups, CSV) remain. Deleted rows are overwritten (`secure_delete`, `VACUUM`, WAL truncated). |
| **Forget everything** (Settings → Erase data → Forget everything; type ALLES WISSEN / ERASE ALL) | Every row in the database, every OV Refund credential (NS password, IBAN, card numbers, mail app password), the session snapshot, the NS cookies, the scheduled task and the log. The app then starts again at the setup wizard. |
| **Uninstall** | Always: the scheduled task, stored NS password, session snapshot and WebView2 profile (cookies, storage and history). With "Delete the application data" ticked, also `%LOCALAPPDATA%\OVRefund` and all OV Refund credentials. **Updates** remove nothing. |
| Not removed by the app | Copies you made outside the app’s export folders, and e-mails already in your mailbox. |

## NS login window

The visible login window has no address bar and loads only NS's own sites (`https://*.ns.nl`). It blocks and logs every other navigation. SmartScreen stays on. Automatic login types the stored password only if NS's own top-level login page (`https://login.ns.nl` or `https://loginapi.ns.nl`) is showing at that moment and the expected field has focus.

The NS login window may load third-party content. Windows/WebView2 SmartScreen may check URLs. The installer may download Microsoft WebView2. The optional e-mail summary with routes and dates goes through your own mail provider. Rijden de Treinen sees your IP address and which months you download.

## Developer-only switches

`--demo`, `OVREFUND_DATA_DIR` and `OVREFUND_DEMO_INSTANCE` work only in developer builds (`--features devtools`). Release builds and the installer ignore them.

## Release signing

Build-time code running as the signing user can read `%USERPROFILE%\.ovrefund-release`, including the key and the DPAPI-protected password. Removing secrets from the build environment does not prevent that. `tools/release.ps1 -AskPassphrase` asks for the password at signing time instead of reading the DPAPI file. The default stays the same. For higher assurance, build and sign with separate accounts or machines, and give only the signing account the key.

The updater verifies the artifact signature. The version and other metadata in `latest.json` are not signed separately. HTTPS protects them in transit. An artifact signature does not prove the advertised version and gives no separate rollback protection.
