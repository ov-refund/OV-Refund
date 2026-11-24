# Verified flow

Engineering verification record. Historical observations are distinguished from offline fixture tests.
No network or live claims were used in the privacy audit. Personal journeys and account details are omitted.

**A real claim submission has NOT been verified yet, for either carrier (NS or Arriva).** Everything after
the final send (confirmation, reference, duplicate check after a real send) is covered by fixture tests only.

## The supported process

1. The user logs in to Mijn NS in the app's NS window (an isolated WebView2 profile, `https://*.ns.nl` only).
2. A check opens Mijn NS *Reishistorie* in a hidden WebView2 and passively reads the JSON the official page receives.
3. Every trip is matched against the Rijden de Treinen archive of the operated timetable for that day.
4. For a claimable trip the app opens the trip's own *Geld terug bij vertraging* form via travel history → trip row
   → *Snel naar → Geld terug bij vertraging*, fills it in, ticks the declaration and clicks *Verzenden* (only after
   the user approved, or with opt-in automatic submission).
5. Status is reconciled from *Mijn teruggaven* (the NS claim list) and, optionally, NS's decision e-mails.
   Manual submission refreshes the list before opening the form. An exact match stops submission.
   When the list is unavailable, the user must check Mijn teruggaven and confirm before continuing.
   Automatic submission requires a readable list.

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

## Arriva trains (manual claims)

1. Arriva train trips come from the same Mijn NS history (`journey.carrier`, code `ARR`/`12` or a name starting
   with *Arriva*) and are assessed with Arriva's rules (`carrier_policy::ARRIVA`, `eligibility::assess_arriva`):
   region from the route, minimum delay per region, 50%/100% bands, 3-month deadline, and the checkable
   exclusions (bus, another carrier on the trip, timetable adjusted in advance, later train by choice).
2. An Arriva trip is never more than *Beoordelen/Review*: it never enters automatic submission and never goes
   through the NS gateway (`pipeline::submit_claim_inner` refuses non-NS trips; `auto_queue` skips them).
3. Only when the user presses *Claimen* (`arriva_claim::make_preview`, then `arriva_claim::submit`), in this order:
   - local checks for claim status, claims filed elsewhere, Arriva's rules, deadline, fare, card, bank
     account and contact details. A saved expiry date applies only to the linked card it was saved for;
   - the travel proof (Mijn NS history screenshot + caption strip with the full card number) and the Rijden de
     Treinen delay card(s). The screenshot is strict: NS's list must be narrowed to the trip's day only, exactly
     one row must match (exact day heading, departure time and station), and the expanded row must be this trip
     (its transaction id in the page, else its check-out time and destination). Otherwise the claim stops with
     "Schermafbeelding van je reis lukte niet / Couldn't capture your trip"; a longer list is never sent. Every
     other trip row and every text outside the trip and its day heading that is not a generic NS label (account
     e-mail, names, card picker; also text at opacity 0, CSS-generated text, and custom elements that show
     nothing readable, likely closed shadow roots) are covered with grey "verborgen" blocks. Right before the
     screenshot the page is measured again; if something new is uncovered or the trip moved more than 2 px the
     mask is drawn once more, and if it still differs there is no screenshot. A visible frame (`iframe`), an
     open dialog or popover outside the trip, or two trips in one box also mean no screenshot (fail closed;
     fake-DOM test `travel_proof_mask_script_in_a_fake_dom`, not yet re-checked live);
   - the user previews both files (*Bijlagen*) and presses *Klopt, ga verder*. The next dialog shows the
     trip, date, delay band, amount and the last four digits of the bank account and card. It asks permission
     to tick Arriva's named consent box ("Ik ga akkoord") when required. A repeat attempt after an uncertain
     outcome also needs confirmation that the user checked their Arriva e-mail. Cancelling either dialog or
     being refused before the form opens discards the preview and triggers attachment cleanup;
   - a build or run that cannot send (dry run, test or developer build without permission) stops here and deletes
     the attachments: Arriva's form is never opened, so the user's attachments are never uploaded;
   - Arriva's HubSpot form at `klantenservice.arriva.nl/geldterug` is filled in a hidden, guarded WebView2
     window. It may upload the attachments separately or include them only in the final multipart request.
     The filled fields are read back and checked (also: no other required field
     left empty), the claim is recorded as *Submitting* together with its in-flight marker (one transaction),
     and *Indienen* is pressed. When Google asks for a CAPTCHA puzzle, the user solves it in the puzzle window.
     *Toon puzzel* brings that window back. A refusal after an attachment request tells the user that files may
     already have reached Arriva and that the final claim was not submitted.
4. Network guard: CDP request interception. Reads (GET/HEAD/OPTIONS) go to any https host, as the page loads
   them; writes only to Google's CAPTCHA, and HubSpot's file upload only in the user's own Claim when the build
   may send (`claim_upload_allowed`). A check never opens Arriva's form; the demo, `arriva-form-test` and the
   canary never upload (the guard blocks every upload outside the user's own Claim; the file field is checked
   locally). A separate upload may include either reviewed file or both. Each file has one retry, and
   preflights do not use an upload permit. For the final write the guard allows one POST to the observed
   EU multipart route with both reviewed filenames, after the ledger records *Submitting* and the guard is
   armed. The offline harness covers inline multipart, batch and per-file uploads. WebView2's multipart
   metadata and the separate-upload behaviour still need a live check. `may_arm` refuses in the demo, dry runs
   (`OVREFUND_DRY_RUN`), devtools tests with a scratch data folder, and devtools builds without
   `OVREFUND_ARRIVA_ALLOW_SUBMIT=1` (tests `final_write_is_never_armed_in_dry_runs_demo_or_tests`,
   `the_users_attachments_are_never_uploaded_by_a_build_that_cannot_send`, `claim_flow_order_record_before_arming`).
5. Outcome: a completed 2xx response with a clear acknowledgement, an empty body, or JSON without a failure
   indicator is *Submitted*. An unreleased final write is *Failed*. A released write with a missing,
   interrupted, or negative result is *Onzeker/Uncertain* and asks the user to check their Arriva e-mail.
   *Submitting*, *Submitted* and *Uncertain* block a repeat claim. Only the user can resolve *Uncertain*
   and approve a new attempt. The ledger clears the in-flight marker with the outcome.
6. Attachments are stored per claim with a marker of the trip they were made for; *Geschiedenis wissen* removes
   those of every removed claim, and the app never shows another trip's files under a reused claim id. They are
   deleted after a final outcome (submitted, not sent, rejected), on *Niet claimen* / *Al elders geclaimd*, on
   expiry, after a dry run and when the preview is cancelled; they are kept only while the claim is in flight or
   its outcome is uncertain. *Alles vergeten* removes them with the Arriva window profile (`<data>/arriva/work`).
7. Not claimable in the app ("dien zelf in bij Arriva"): an unknown region, a missing fare, a student travel
   product, an NS subscription without a fare (NS's rules decide the amount), or an unknown full card number. A
   card expiry date in the past is accepted (the trip may have been made on a card that has expired since). Trips with a carrier other than NS or Arriva are labelled
   *Vervoerder nog niet ondersteund* and never claimed. OVpay is not supported.

| Arriva step | Status | Evidence |
|---|---|---|
| Arriva rules (regions, bands, deadline) | **VERIFIED OFFLINE** | Terms read 2026-11-20 (see integration inventory); `carrier_policy` and `eligibility` tests. |
| Station → region table | **NOT VERIFIED LIVE** | Built from Arriva's concession lines; the same in both directions; stations in different regions take the strictest rule; a trip with no known station has no region and is not claimable in the app ("dien zelf in bij Arriva"). |
| Strict travel proof (day filter, row check) | **NOT VERIFIED LIVE** | Whether Mijn NS shows the transaction id, or the check-out time and destination, in the expanded row is unobserved; if neither matches, the claim stops before Arriva's form. |
| Form filled and checked, without sending | **VERIFIED with the demo and `arriva-form-test`** | Synthetic data; the guard blocked the final write and every upload (the file field is checked locally). Region choices other than Limburg not seen live. |
| Required fields for a real user (card expiry, consent) | **NOT VERIFIED** | The form is read at claim time; a required field the app cannot fill stops the claim with a message. |
| Pressing *Indienen*, HubSpot success response and reference | **NOT VERIFIED** | No real Arriva claim has been sent. Without a clear success it is recorded as *Uncertain*. |

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
