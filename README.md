# OV Refund

OV Refund is een lokale Windows-app voor vertraagde NS-reizen uit je eigen Mijn NS-reishistorie. Je bekijkt de claims en dient ze zelf in, of je laat OV Refund dat automatisch doen. Gratis en zonder commissie. NS beslist over je vergoeding. Niet verbonden aan NS, Translink of Rijden de Treinen.

OV Refund is a local Windows app for delayed NS trips in your own Mijn NS travel history. You review and submit claims yourself, or let OV Refund submit them automatically. Free and without commission. NS decides on your refund. Not affiliated with NS, Translink or Rijden de Treinen.

## Installeren / Install

Download `OV-Refund-Setup.exe` van de nieuwste release en open het (Windows 10/11, geen administratorrechten nodig). Wil je de download controleren? Vergelijk dan `certutil -hashfile OV-Refund-Setup.exe SHA256` met `SHA256SUMS.txt` bij de release. Toont SmartScreen een waarschuwing? Klik dan op *Meer informatie → Toch uitvoeren* als je wilt doorgaan. Updates in de app zijn digitaal ondertekend en worden gecontroleerd. Het installatiebestand heeft geen uitgevershandtekening.

Download `OV-Refund-Setup.exe` from the latest release and open it (Windows 10/11, no administrator rights needed). Want to check the download? Compare `certutil -hashfile OV-Refund-Setup.exe SHA256` with `SHA256SUMS.txt` in the release. Does SmartScreen show a warning? Click *More info → Run anyway* if you want to continue. In-app updates are digitally signed and verified. The installer has no publisher signature.

De wizard heeft deze stappen / The wizard has these steps:

1. **Welkom / Welcome**: lees de uitleg en vink de verklaring aan / read the notice and tick the declaration.
2. **Inloggen bij Mijn NS / Log in to Mijn NS**: klik op **Inloggen bij Mijn NS** en log in met je Mijn NS-account. Je persoonlijke OV-chipkaart moet daaraan gekoppeld zijn. Het NS-venster sluit vanzelf. / Click **Log in to Mijn NS** and log in with your Mijn NS account. Your personal OV-chipkaart must be linked to it. The NS window closes by itself.
3. **Gebruik je een claimdienst? / Do you use a claim service?**: kies Ja of Nee. Gebruik je een dienst? Zeg die dan op en voeg de reizen toe die al geclaimd zijn bij *Instellingen → Gegevens & back-up → Elders geclaimd*. Klik daarna op *Import klaar*. Tot dan blijft automatisch indienen gepauzeerd. / Choose Yes or No. Do you use a service? Cancel it and add the trips it already claimed under *Settings → Data & backup → Claimed elsewhere*. Then click *Import done*. Until then, automatic submission stays paused.
4. **Waar mag NS je geld op storten? / Where should NS pay you?**: vul de naam van de rekeninghouder en een geldig IBAN in / enter the account holder and a valid IBAN.
5. **Automatisch laten controleren? / Check automatically?**: OV Refund controleert elke 7 dagen. Een gemiste controle loopt de volgende keer dat je inlogt op Windows. Automatisch indienen staat standaard uit. / OV Refund checks every 7 days. A missed check runs the next time you sign in to Windows. Automatic submission is off by default.
6. **Mag OV Refund zelf inloggen? / May OV Refund log in by itself?**: deze stap zie je alleen bij automatische controles. Zonder automatisch inloggen krijg je een herinnering. / You only see this step with automatic checks. Without automatic login you get a reminder.
7. **Hoe wil je het horen? / How should we let you know?**: Windows-meldingen staan standaard aan. Een overzicht per e-mail via je eigen mailbox is optioneel. / Windows notifications are on by default. An e-mail summary through your own mailbox is optional.
8. **Klaar! / All set!**: klik op *Start eerste controle*. Ben je nog niet ingelogd? Klik dan op *Naar OV Refund*. / Click *Start first check*. Not logged in yet? Click *Go to OV Refund*.

*Volgende* blijft grijs tot de stap klaar is. Bij inloggen en je rekening kun je op *Later* klikken. Na Welkom kun je op *Instellen overslaan* klikken en de rest later instellen bij Instellingen.

*Next* stays grey until the step is done. At login and bank account you can click *Later*. After Welcome you can click *Skip setup* and set up the rest later under Settings.

OVpay, anonieme kaarten, NS Zakelijk, e-tickets, losse kaartjes, andere vervoerders en internationale reizen worden niet ondersteund. De eerste controle haalt treintijden maand voor maand op: ongeveer **35 MB per maand met reizen**, meestal 3 of 4 maanden. Je ziet de resultaten per maand. Reizen uit deze maand wachten tot de treintijden gepubliceerd zijn, meestal begin volgende maand. De claimtermijn is 3 maanden.

OVpay, anonymous cards, NS Zakelijk, e-tickets, single tickets, other carriers and international trips are not supported. The first check fetches train times month by month: about **35 MB per month with trips**, usually 3 or 4 months. Results appear per month. Trips from this month wait until the train times are published, usually early next month. The claim window is 3 months.

## Reizen en instellingen / Trips and settings

De balk bovenaan heeft één knop voor de volgende stap. Tabs: *Alle reizen, Te claimen, Beoordelen, Geclaimd, Verlopen*. Periodes: *7 dagen, 30 dagen, Jaar, Alles*. Gele reizen kun je claimen. Ze tonen het bedrag en de knop *Bekijken en indienen*. Vertragingen staan in rood. Bij *Beoordelen* kun je ook op *Niet claimen* klikken. *Controleer nu* toont tijdens een controle de voortgang en de huidige maand in dezelfde knop.

The bar at the top has one button for the next step. Tabs: *All trips, Claimable, Review, Claimed, Expired*. Periods: *7 days, 30 days, Year, All*. Yellow trips are claimable. They show the amount and the *Review and submit* button. Delays appear in red. Under *Review* you can also click *Don't claim*. During a check, *Check now* shows progress and the current month inside the same button.

Bij indienen beantwoord je de vraag *Heb je je reis tot … afgemaakt?*. Vink daarna de verklaring aan: de gegevens kloppen en de reis is niet eerder geclaimd. Antwoord je Nee? Dien de claim dan zelf in via Mijn NS. *Eerst proberen (verstuurt niets)* test het formulier. *Indienen bij NS* verstuurt de claim.

To submit, answer the question *Did you complete your trip to …?*. Then tick the declaration: the details are correct and the trip has not been claimed before. Did you answer No? Then submit the claim yourself via Mijn NS. *Try first (sends nothing)* tests the form. *Submit to NS* sends the claim.

Automatisch indienen werkt pas als je de vraag over claimdiensten hebt beantwoord. Een automatische controle dient elke reis die in aanmerking komt in, één voor één met een korte pauze. Staat automatisch indienen aan? Dan dient *Controleer nu* ook claims in waarvan de termijn binnen 14 dagen afloopt. Vlak voor een termijn afloopt, plant OV Refund één extra controle op de achtergrond. Het e-mailoverzicht blijft uit tot je een app-wachtwoord invult.

Automatic submission only works once you have answered the claim-service question. An automatic check submits every qualifying trip, one by one with a short pause. Is automatic submission on? Then *Check now* also submits claims whose deadline is within 14 days. Shortly before a deadline, OV Refund plans one extra check in the background. The e-mail summary stays off until you enter an app password.

Naast de reizen staat *Jouw gegevens*, met de knop *Instellingen* eronder. Tabs in Instellingen: *Account & NS, Automatisch, Meldingen, Gegevens & back-up, Hulp, Gegevens wissen*. Een back-up maak en zet je terug onder *Gegevens & back-up*. *Alles vergeten* staat onder *Gegevens wissen*. Een back-up bevat reizen en claims. Wachtwoorden, IBAN, kaartnummer en e-mailinstellingen zitten er niet in.

Beside your trips is *Your details*, with the *Settings* button below it. Settings tabs: *Account & NS, Automatic, Notifications, Data & backup, Help, Erase data*. Make and restore backups under *Data & backup*. *Forget everything* is under *Erase data*. A backup contains trips and claims. It holds no passwords, IBAN, card number or e-mail settings.

## Privacy en problemen / Privacy and problems

Je reizen en claims staan op je eigen computer. Er is geen tracking en je hebt geen account bij ons. Wachtwoorden en bankgegevens staan versleuteld in Windows Referentiebeheer. De database bewaart alleen de laatste vier cijfers van je kaart. Zet je automatisch inloggen aan? Dan vult OV Refund je inloggegevens alleen in op de inlogpagina van NS. Uitloggen wist je NS-wachtwoord en de sessie, maar bewaart de koppeling met je account. *Alles vergeten* wist ook die koppeling.

Your trips and claims stay on your own computer. There is no tracking and no account with us. Passwords and bank details are encrypted in Windows Credential Manager. The database stores only the last four digits of your card. If you turn on automatic login, OV Refund enters your login details only on the NS login page. Logging out erases your NS password and the session, but keeps the link to your account. *Forget everything* erases that link too.

Rijden de Treinen ziet je IP-adres en welke maanden je downloadt. De app gebruikt NS, Rijden de Treinen voor dienstregelingsdata en GitHub voor updates. E-mail is optioneel en gaat via je eigen mailprovider (SMTP). Met *NS-mails lezen* leest de app ook de vergoedingsmails van NS (IMAP). Het NS-inlogvenster kan inhoud van derden laden. Windows/WebView2 SmartScreen kan URL’s controleren. Het installatieprogramma kan WebView2 downloaden. Hotmail, Outlook.com en Live laten apps zoals OV Refund geen mail meer versturen. Gebruik Windows-meldingen of een ander adres, zoals Gmail.

Rijden de Treinen can see your IP address and which months you download. The app uses NS, Rijden de Treinen for timetable data and GitHub for updates. E-mail is optional and goes through your own mail provider (SMTP). With *Read NS e-mails* the app also reads NS refund e-mails (IMAP). The NS login window may load third-party content. Windows/WebView2 SmartScreen may check URLs. The installer may download WebView2. Hotmail, Outlook.com and Live no longer let apps like OV Refund send mail. Use Windows notifications or another address, such as Gmail.

Een probleem? Klik rechtsonder op *Bug melden*. Je schrijft maximaal **500 tekens** en ziet vooraf wat er meegaat. De melding gaat pas via Web3Forms als je toestemming geeft. Technische gegevens zonder persoonsgegevens kun je meesturen. De app haalt IBAN-, kaart- en telefoonnummers en e-mailadressen uit je tekst. Zet er toch geen persoonsgegevens in. Er gaan geen contactgegevens mee. Web3Forms ziet wel je IP-adres. De makers wissen meldingen zodra het probleem is opgelost. Wil je antwoord? Gebruik dan GitHub Issues: openbaar, onder je eigen account.

A problem? Click *Report a bug* at the bottom right. You write at most **500 characters** and see what is included before sending. The report goes through Web3Forms only after you agree. You can add technical details without personal data. The app removes IBANs, card and phone numbers and e-mail addresses from your text. Still, keep personal data out of it. No contact details are sent. Web3Forms does see your IP address. The authors delete reports once the problem is solved. Want a reply? Use GitHub Issues: public, under your own account.

Zie [Hulp](site/hulp.html), [Disclaimer](site/disclaimer.html) en [SECURITY.md](SECURITY.md). De makers zijn niet aansprakelijk voor het gebruik van de app of de gevolgen daarvan.

See [Help](site/hulp.html), [Disclaimer](site/disclaimer.html) and [SECURITY.md](SECURITY.md). The authors are not liable for the use of the app or its consequences.

## Verification and development

See [docs/verified-flow.md](docs/verified-flow.md) for what has been verified and how. Trip import has been verified live. A real end-to-end submission has not. Automatic submission is opt-in and pauses on uncertain results.

Build requirements: Rust stable (MSVC), Node 20+, WebView2 (Windows 10/11).

```powershell
npm ci
cargo test --workspace
npx vitest run
npx tauri build --bundles nsis
```

This repository holds the source code, the website (`site/`, published to GitHub Pages by `.github/workflows/pages.yml`) and the releases. Maintainers release with `tools/ship.ps1`: it builds and checks the installer, stages the code with `tools/publish-public.ps1` as a single commit (which checks for private data), pushes it after confirmation and then creates the release at that commit.

Command line: `ov-refund --run-once`, `scan`, `history`, `diagnose`, `install-schedule [days]`, `remove-schedule`.

MIT (see `LICENSE`). Train data: [Rijden de Treinen](https://www.rijdendetreinen.nl/open-data), [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) (filtered, not otherwise modified). Station list: Rijden de Treinen, CC0.
