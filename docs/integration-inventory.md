# Integration inventory

Historical integration observations. This audit used no network. Personal account details are omitted.

Status legend: **WORKING** = observed live, end to end · **PARTIAL** = observed, with limits ·
**BLOCKED** = not possible within the rules · **UNKNOWN** = not yet observed.

## Matrix

| Capability | Status | Evidence / notes |
|---|---|---|
| Authentication (Mijn NS) | **PARTIAL** | User login via `login.ns.nl` (SAML/OAuth, `csrfToken`, Akamai/Imperva bot telemetry). Works interactively. **The session does not survive a browser restart**: after a full close/reopen, `/mijnns` redirects to `login.ns.nl/login`. The access token lives in `sessionStorage` (`mns-access-token`). The only "remember" option on the login page is *Onthoud mijn e-mailadres* (email only). |
| Journey retrieval | **WORKING** (in-session) | `GET gateway.apiportal.ns.nl/mijnns-transaction-api/couplings/{couplingId}/transactions?from&until` (also `?from&limit`), issued by the Mijn NS SPA with `Authorization` + `Ocp-Apim-Subscription-Key`. JSON: `transactions.transactions[]` with `type=JOURNEY`, `journey.departure/arrival.{timestamp(UTC), location.{name,uicCode}}`, `journey.carrier.{code,description}`, `product`, `subscription`, `travelClass`, `id` (e.g. `HB-ROR…`). |
| Official export | **PARTIAL** | Mijn NS *Reishistorie* offers **CSV** and **PDF** download for the selected period (not yet parsed). |
| Scheduled arrival source | **WORKING** (with publication lag) | Rijden de Treinen *train archive* (CC BY 4.0), monthly `https://opendata.rijdendetreinen.nl/public/services/services-YYYY-MM.csv.gz` (~33 MB), with planned times plus realized delays per stop. Stations (CC0) `stations-2023-09-nl.csv` map UIC → station code. Offline matching and DST regressions are covered by fixture tests. **Lag:** a month file appears on the 4th of the next month, so trips of the current month wait up to ~5 weeks. No permitted faster Rijden de Treinen source exists (see *Faster train data for recent trips*). NS API portal schedule APIs need a key and their historical support is **UNKNOWN**. |
| Claim form (journey-linked) | **WORKING** (inspected, not submitted) | Mijn NS route `#/reishistorie/geld-terug-bij-vertraging/{couplingId}/{transactionId}/{checkinUtc}`, reached from trip details → *Snel naar → Geld terug bij vertraging* (`[data-test=gtbv-claim-link]`). Fields: `checkIn`/`checkOut` (prefilled selects), `arrival` radio (reached NS destination yes/no), `departure` (time input, **scheduled departure**, required), `delay` radio (`30 t/m 59 min` / `60 minuten of meer`), `name`, `iban` (required), `declaration` checkbox "Ik heb alles naar waarheid ingevuld." (required), submit *Verzenden*. Settings `mijnns-claim-api/settings` → `gtbvClaimPeriod: min 24h, max 3 months`. |
| Claim form (public, no account) | **UNKNOWN** | `https://gtbv-formulier.ns.nl/ns/klantenservice-geld-terug-bij-vertraging` (Mendix app). Not needed while the Mijn NS route works. |
| Actual confirmed submission | **UNKNOWN** | Requires a genuinely eligible trip and explicit one-time user approval. Not done yet (see `verified-flow.md`). |
| Arriva claim form (HubSpot) | **PARTIAL** (filled and checked, not submitted) | `https://klantenservice.arriva.nl/geldterug`, an embedded HubSpot form. File uploads go to `…hsforms.com/submissions-files/v1/upload/files`; the final write is a POST whose path contains `/submit/` on `*.hsforms.com`/`*.hubspot.com`. Only a manual *Claimen* may let that one write through (and only a manual *Claimen* may upload). Google reCAPTCHA Enterprise (invisible); a puzzle, if shown, is solved by the user. The success response and reference format are **UNKNOWN**. |
| Claims status | **WORKING** (read) | `GET mijnns-claim-api/gtbv/claims` and `…/cico/claims`. UI *Mijn teruggaven* (`#/claims`), kept for 18 months. Only an empty list was observed. The populated record shape is unknown. |
| OV-chipkaart.nl personal history | **UNKNOWN** | Not needed: Mijn NS covers the coupled personal card. The anonymous-card lookup is not used for personal cards. |
| NS API portal (official APIs) | **UNKNOWN** | The catalogue is visible only to registered users, with free keys and per-product rate limits. No personal-journey or claim API is advertised. |

## Endpoint details (observed, Mijn NS SPA traffic)

| URL (host + path) | Auth | Method | Format | Use in app |
|---|---|---|---|---|
| `www.ns.nl/mijnns#/reishistorie` | session | UI | Angular SPA | driven in an embedded WebView2 |
| `gateway.apiportal.ns.nl/mijnns-card-coupling-api/v2/couplings/me` | bearer | GET | JSON `[{couplingId,type:"ovcp",tokens[{cardNumber,expirationDate}]}]` | observed passively → card fingerprint (last 4 digits only) |
| `gateway.apiportal.ns.nl/mijnns-transaction-api/couplings/{id}/transactions` | bearer | GET | JSON (see above) | observed passively → journeys |
| `gateway.apiportal.ns.nl/mijnns-claim-api/settings` | bearer | GET | JSON claim periods | policy window |
| `gateway.apiportal.ns.nl/mijnns-claim-api/gtbv/claims` | bearer | GET | JSON list | status reconciliation |
| `gateway.apiportal.ns.nl/mijnns-contract-api/v2/contracts/me` | bearer | GET | JSON (product code, price) | product shown in review |

**Access approach chosen:** the app never extracts, stores or replays the NS bearer token or subscription key.
It opens the official Mijn NS pages in an embedded system WebView2 with its own isolated profile, lets the user log in there, and *passively reads* the JSON responses the official page already receives (DevTools protocol `Network.getResponseBody` via WebView2's `CallDevToolsProtocolMethod`). Claims are entered into the official form fields and submitted by clicking the official *Verzenden* button. No private endpoint is called directly.

**Finding page elements (`src-tauri/src/ns_elements.rs`):** every element OV Refund uses on the claim form and the travel-history filter has an ordered list of strategies: (a) the id/selector observed above (`#arrival0`/`#arrival1`, `#departure`, `#delay0`/`#delay1`, `#name`, `#iban`, `#declaration`, `button[type=submit]`, `#filter-period-control`, `mns-transaction-filter nes-button` "Tonen", `[data-test=gtbv-claim-link]`), (b) the visible label or question text exactly as NS shows it ("Heb je je gewenste NS-eindstation bereikt?" → "Ja, ik heb mijn NS-eindstation bereikt" / "Nee, ik heb mijn NS-eindstation niet bereikt"; "Vertrektijd volgens dienstregeling"; "Vertraging" → "30 t/m 59 min" / "60 minuten of meer"; "Naam rekeninghouder"; "Bankrekeningnummer (IBAN)"; "Ik heb alles naar waarheid ingevuld."; "Verzenden"; "Tonen"; "Geld terug bij vertraging"; for the period filter the option "Afgelopen 1,5 jaar"), (c) ARIA role and accessible name. All strategies search open shadow roots. A strategy counts only with exactly one match; if two strategies each find one element but not the same one (for example swapped ids), nothing is used. The prefilled check-in/check-out selects have no fallback (their labels were not recorded). A fallback match is logged (element name and strategy only) and added to the form notes; it never relaxes a check: the prefilled times must match the trip, typed values are read back, the declaration must be ticked, the pre-submit form check must be clean (fields are "known" by the resolved element, not by id) and the button must be an enabled submit button of the form that holds the declaration. Otherwise the claim is *not sent*. The label and ARIA fallbacks were tested only against a fake DOM, not yet against the live NS page.

## Terms and permitted use (to review before public release)

- NS: *Voorwaarden geld terug bij vertraging* (PDF, metadata 2025-07-28). Claims must be complete and truthful, one per person per delay. The page says there is no refund under €2.30, when a delay was announced in advance, when another carrier caused it, or in force majeure.
- NS-account terms of use: <https://www.ns.nl/voorwaarden/gebruiksvoorwaarden-ns-account> (version of 18 Oct 2024). Reviewed 2026-11-13 in `docs/ns-terms-review.md`: no explicit ban on personal automated use, but art. 2.9(b) and 2.5 are a risk to put to NS.
- Rijden de Treinen archive: CC BY 4.0, attribution "Rijden de Treinen" is shown in the app and README.
- Bot protection (Akamai `akam/…` pixels, Imperva `incap_ses_*`) is present on login and ns.nl. The app never bypasses it, and any challenge stops the run and asks the user to log in.

## Arriva trains: refund rules

Read on 2026-11-20 from Arriva's terms page, one region at a time:
https://www.arriva.nl/vragen-contact/contact-met-arriva/een-vergoeding-aanvragen/?regio=<region>
(section "Geld terug bij vertraging in <region>" and "Indienen en afhandeltijd"). The page sits behind an
automatic browser check, so it was read in the app's guarded WebView2 window (normal page load, no
writes). The claim form's choices were read from https://klantenservice.arriva.nl/geldterug on the
same date. In code: `crates/ovrefund-core/src/carrier_policy.rs` (`ARRIVA`, `arriva_region`, `refund_cents`), used by
the eligibility check for real Arriva trips (`eligibility::assess_arriva`), the form filler and the demo.

| Region (train concession) | Minimum delay | Pay-as-you-go (saldo) | Student travel product |
|---|---|---|---|
| Limburg (includes the Maaslijn Roermond - Venlo - Nijmegen) | 15 min | 15-59 min: 50% of the fare; 60+ min: 100% | EUR 2.32 / EUR 4.64 |
| Gelderland, Overijssel, Drenthe, Groningen, Fryslân | 30 min | 30-59 min: 50%; 60+ min: 100% | EUR 2.32 / EUR 4.64 |

- Subscriptions (fractions of the monthly fee, e.g. 1/50 and 1/25 of a month route pass) and NS products
  (NS rules apply) are listed on the page but not used by OV Refund (pay-as-you-go fares only).
- Noord-Brabant has no Arriva trains; its table covers buses and is not used.
- Claim period: "binnen 3 maanden na de gebeurtenis" (within 3 months after the trip). The app treats
  travel date + 3 calendar months as the last day.
- No refund: delay caused by another operator, longer travel time announced in advance (track works),
  subscription on a card without check-in/check-out, force majeure (extreme weather, power failure),
  bus delays.
- Claim form, page 2, "Hoeveel minuten vertraging heb je ervaren?": `15 - 30 minuten`, `30 - 59 minuten`,
  `60+ minuten`. The first two overlap at 30; following the terms (30-59 band) the app picks
  `30 - 59 minuten` for exactly 30 minutes. After `15 - 30 minuten` the form itself estimates 50%.
- An unknown region is not claimable in the app: the trip says "dien zelf in bij Arriva" (submit it yourself at
  Arriva). The same holds for a missing fare, a student travel product or an unknown full card number.
- The demo's claimable trip is Arriva train 732284 (Maaslijn, Limburg concession) on 2026-10-29,
  Venlo - Nijmegen, +24 min arrival delay at Nijmegen in Rijden de Treinen open data: band 15-59,
  form choice `15 - 30 minuten`, last claim day 29-01-2027.

## Arriva claims for real trips

- Region: from the route (`ARRIVA_STATIONS`): a station that belongs to one concession wins over a hub
  (Zwolle, Arnhem, Nijmegen, Groningen, Leeuwarden …), origin first. Unknown: not claimable in the app
  ("dien zelf in bij Arriva").
- Form data and where it comes from: region and delay band (computed), departure/arrival station, travel date and
  time, fare (Mijn NS), full OV-chipkaart number (Credential Manager, per linked card: `ov-card-<last4>-<hash of the coupling id>`), IBAN (bank details),
  first name, last name and e-mail address (sidebar *Jouw gegevens*, Credential Manager `arriva-contact`), card
  expiry date (optional, also under *Jouw gegevens*), consent box `LEGAL_CONSENT` (only ticked when the form
  requires it; the user agrees to it in the confirmation dialog). Attachments: `reisbewijs-mijn-ns.png` and
  `vertragingsbewijs-rijden-de-treinen.png`, made on *Claimen* only, shown to the user for confirmation
  (cancelling deletes them), kept in `<data>/arriva/claims/<id>/` only while the claim is in flight or its outcome
  is uncertain, deleted after a final outcome, a dismissal, expiry or a dry run, and erased by *Alles vergeten*.
  Uploaded to HubSpot only in the user's own Claim; a check, the demo, `arriva-form-test` and the canary never
  upload (the guard blocks it; the file field is checked locally).
- A changed form (missing or unreadable field, another required field left empty, a form error, no submit
  button) stops before the final write, and the message names the field.

## Rijden de Treinen download time

`ovrefund-core` stores the time of every month-file download as setting `archive_retrieved:<YYYY-MM>`.
The demo downloads the month file again when it builds the delay proof and prints that time (local
time, plus the server's Last-Modified date) on the card. If the download fails the card says
"download mislukt / download failed" and shows the bundled copy of the record.

## Full OV-chipkaart number

The full number comes from Mijn NS's card-coupling response (`/mijnns-card-coupling-api/`,
`tokens[].cardNumber`) and is kept only in Windows Credential Manager, per linked card
(`ov-card-<last4>-<short hash of the coupling id>`, so two cards ending in the same digits never share an entry;
an older `ov-card-<last4>` entry is still read when no other linked card ends in those digits). The travel
proof adds it, with the travel date and route, in a caption strip under the travel-history screenshot (in which
every other trip row and the account name / card picker are covered with grey "verborgen" blocks). Whether a Mijn NS page
shows the full number visibly (for a second, real screenshot) has not been checked with a live login.

## Faster train data for recent trips (researched 2026-11-24: BLOCKED)

Goal: assess trips of the current month within a day or two instead of waiting for the month file.
Matching needs, per travel day, every planned service calling at the trip's stations (for 1-transfer
routing and the later-train logic) plus their realized arrival times. Observed on 2026-11-24 with plain
GETs (generic User-Agent, a handful of requests, no personal data):

| Source | What it offers | Permitted for the app? |
|---|---|---|
| `opendata.rijdendetreinen.nl/public/services/` (directory listing) | Only yearly files (2019-2022) and monthly files from 2023-01 up to `services-2026-10.csv.gz`. Since 2025-04 each month is published on the 4th of the next month (~02:00-03:00). No daily or per-train files. `/public/` holds only `disruptions/`, `services/`, `stations/`, `tariff-distances/`. | Yes (robots.txt on that host: `Disallow:` empty; CC BY 4.0). Already used. |
| `www.rijdendetreinen.nl/en/open-data/train-archive` | Dataset page: "The data is updated yearly. For media outlets and professional users, it's possible to get a more up-to-date version of this dataset or a custom selection. Please contact us." | A more recent version is offered only on request. |
| Website train archive, per station and day: `/en/train-archive/<YYYY-MM-DD>/<station-slug>` (NL `/treinarchief/...`); per train `/vertrektijden/treinrit/...`, `/en/departures/details/...` | Exactly the data needed (all services at a station on a day, with delays), as server-rendered HTML; no public JSON API was found on the landing page (only the site's own bundles under `/dist/assets/`). | **No.** `www.rijdendetreinen.nl/robots.txt` has, for `User-agent: *`, `Disallow: /treinarchief/`, `/en/train-archive/`, `/vertrektijden/treinrit/`, `/en/departures/details/`, `/treinarchief?`, `/en/train-archive?`. The about page: website and software are copyrighted, "Zonder toestemming kopiëren van (delen) hiervan is niet toegestaan", public data only via the open-data pages. These pages were therefore not fetched. |
| GoTrain (RDT's open-source backend, GPL v3) | A REST API over the live NDOV feeds, run by yourself. | Live data only (would need a server collecting all day plus an NDOV feed subscription); not a fit for a desktop app that checks once a day. |
| NS API portal (Reisinformatie API) | Departures/arrivals/journey per train. | Needs a personal key; whether it serves days in the past is **UNKNOWN** (not observed). |

Conclusion: no permitted source gives the planned services plus actual arrivals of a recent day, so
no faster source was built. The current-month trips keep waiting for the month file. Options for the
owner: ask Rijden de Treinen (via the feedback address on its website) for permission or a more up-to-date feed,
or register for an NS API key and check whether its trip data covers past days.

Note: the delay proof card attached to claims does **not** visit the Rijden de Treinen website; it is
rendered locally from the month file (`src-tauri/src/arriva.rs`, `delay_cards`, `archive::month_rows`).
