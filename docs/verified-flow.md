# Verified flow

Engineering verification record. Historical observations are distinguished from offline fixture tests.
No network or live claims were used in the privacy audit. Personal journeys and account details are omitted.

## The supported process

1. The user logs in to Mijn NS in the app's NS window (an isolated WebView2 profile, `https://*.ns.nl` only).
2. A check opens Mijn NS *Reishistorie* in a hidden WebView2 and passively reads the JSON the official page receives.
3. Every trip is matched against the Rijden de Treinen archive of the operated timetable for that day.
4. For a claimable trip the app opens the trip's own *Geld terug bij vertraging* form via travel history → trip row
   → *Snel naar → Geld terug bij vertraging*, fills it in, ticks the declaration and clicks *Verzenden* (only after
   the user approved, or with opt-in automatic submission).
5. Status is reconciled from *Mijn teruggaven* (the NS claim list) and, optionally, NS's decision e-mails.

## Status per step

| Step | Status | Evidence |
|---|---|---|
| Interactive login to Mijn NS in the app | **VERIFIED LIVE** | Login observed on 2026-11-12 in the app's NS window. |
| Automatic login in a fresh hidden process | **VERIFIED LIVE previously** | `ns_bridge::try_auto_login`. Not re-checked live in this audit. |
| Session reuse across processes without a password | **BLOCKED** | NS rejected a restored cookie jar, even after 5 min. See [SECURITY.md](../SECURITY.md), Optional automatic login. |
| Travel history read (full + incremental) | **VERIFIED LIVE previously** | Parser: `ns_parse.rs`, with a synthetic transaction fixture. |
| Delay evidence from the operated timetable | **VERIFIED OFFLINE** | `archive.rs`, `matcher.rs`, `eligibility.rs`, with fixture and DST tests. |
| Claim list (*Mijn teruggaven*) read | **VERIFIED LIVE, empty only** | `mijnns-claim-api/gtbv/claims` returned an empty list. The populated record shape is **NOT VERIFIED**. The parser fails closed: an unreadable list counts as "unavailable", never as "no claims". |
| Claim form opened and filled, without sending | **VERIFIED LIVE previously** | `form_test_run` checked the route and prefilled times. It stops before the declaration and sending. |
| Pressing *Verzenden* and reading NS's confirmation | **NOT VERIFIED** | No live submission yet. The confirmation shape, the NS reference format and the populated claim list are unobserved. Without an NS reference or confirmation text a send is recorded as *Onzeker* (uncertain), never as submitted. |
| "Did not reach final destination" path of the form | **NOT VERIFIED** | Not observed. The app does not fill it in and tells the user to submit via Mijn NS. |
| NS decision e-mails | **VERIFIED OFFLINE** | Synthetic samples based on the structure of NS mails: `sample_paid_mail_uses_application_number_not_kenmerk`, `sample_rejection_mail`, `sample_received_mail_is_not_a_decision`. |
| Refund amount for a Vrij-type subscription trip | **VERIFIED OFFLINE** | `weekend_vrij_uses_the_trips_own_monthly_fee` tests fictional fees and cent rounding. Published fallback prices are separate. |
| Scheduled unattended check (Task Scheduler) | **VERIFIED LIVE previously** | A hidden check completed. Offline tests cover the task XML and per-user naming. |
| Unattended submission | **NOT VERIFIED** | Implemented and covered by fixture tests (`crates/ovrefund-core/tests/ledger.rs`). Before a first verified submission it can only be turned on with an explicit "unverified" override. |
| Windows notifications | Implemented, live test pending | – |
| E-mail summary (SMTP) | Verified with the demo instance | Demo SMTP check. |

## What "working" means today

- Working: login, reading travel history, delay detection, filling the official form up to (not including) the
  declaration and *Verzenden*, reading an empty claim list, interpreting NS mails, scheduled hidden checks.
- Not yet working as proven: an actual submission and everything after it (NS confirmation, populated claim list,
  restart/duplicate check after a real submission, an unattended real submission). The product must
  not be described as a working automated refund tool until a user-approved live submission has been verified.

## Remaining live verification

1. One user-approved submission of a clearly eligible trip through the app's own gateway.
2. Record the confirmation page shape and the NS reference. Check the populated `gtbv/claims` record.
3. Restart the app and run a check: no duplicate, status reconciled.
4. One unattended scheduled run that submits a second eligible trip without input (only with the user's opt-in).
