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
| Scheduled arrival source | **WORKING** (with publication lag) | Rijden de Treinen *train archive* (CC BY 4.0), monthly `https://opendata.rijdendetreinen.nl/public/services/services-YYYY-MM.csv.gz` (~33 MB), with planned times plus realized delays per stop. Stations (CC0) `stations-2023-09-nl.csv` map UIC → station code. Offline matching and DST regressions are covered by fixture tests. **Lag:** the current month is not published yet. NS API portal schedule APIs need a key and their historical support is **UNKNOWN**. |
| Claim form (journey-linked) | **WORKING** (inspected, not submitted) | Mijn NS route `#/reishistorie/geld-terug-bij-vertraging/{couplingId}/{transactionId}/{checkinUtc}`, reached from trip details → *Snel naar → Geld terug bij vertraging* (`[data-test=gtbv-claim-link]`). Fields: `checkIn`/`checkOut` (prefilled selects), `arrival` radio (reached NS destination yes/no), `departure` (time input, **scheduled departure**, required), `delay` radio (`30 t/m 59 min` / `60 minuten of meer`), `name`, `iban` (required), `declaration` checkbox "Ik heb alles naar waarheid ingevuld." (required), submit *Verzenden*. Settings `mijnns-claim-api/settings` → `gtbvClaimPeriod: min 24h, max 3 months`. |
| Claim form (public, no account) | **UNKNOWN** | `https://gtbv-formulier.ns.nl/ns/klantenservice-geld-terug-bij-vertraging` (Mendix app). Not needed while the Mijn NS route works. |
| Actual confirmed submission | **UNKNOWN** | Requires a genuinely eligible trip and explicit one-time user approval. Not done yet (see `verified-flow.md`). |
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

## Terms and permitted use (to review before public release)

- NS: *Voorwaarden geld terug bij vertraging* (PDF, metadata 2025-07-28). Claims must be complete and truthful, one per person per delay. The page says there is no refund under €2.30, when a delay was announced in advance, when another carrier caused it, or in force majeure.
- NS-account terms of use: <https://www.ns.nl/voorwaarden/gebruiksvoorwaarden-ns-account> (version of 18 Oct 2024). Reviewed 2026-11-13 in `docs/ns-terms-review.md`: no explicit ban on personal automated use, but art. 2.9(b) and 2.5 are a risk to put to NS.
- Rijden de Treinen archive: CC BY 4.0, attribution "Rijden de Treinen" is shown in the app and README.
- Bot protection (Akamai `akam/…` pixels, Imperva `incap_ses_*`) is present on login and ns.nl. The app never bypasses it, and any challenge stops the run and asks the user to log in.
