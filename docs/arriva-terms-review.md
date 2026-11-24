# Arriva terms review

Prepared from the app's code and from Arriva's pages and claim form as read on 2026-11-20 and 2026-11-23. This is
a plain reading by the project, **not legal advice**. Quotes below are copied from the pages as read on the date
given; nothing else is a quote. Arriva's general transport conditions have not been read (accepted by the
maintainer, see the conclusion).

## Sources

| Document | URL | Status |
|---|---|---|
| Een vergoeding aanvragen (refund rules per region) | <https://www.arriva.nl/vragen-contact/contact-met-arriva/een-vergoeding-aanvragen/> (`?regio=<region>`) | Regional rules read 2026-11-20, one region at a time, in the app's guarded window (the page sits behind an automatic browser check; normal page load, no writes). Rules in `docs/integration-inventory.md` and `carrier_policy.rs`. General text read 2026-11-23 by the maintainer in a browser (see below). |
| Geld terug bij vertraging (public claim form, HubSpot) | <https://klantenservice.arriva.nl/geldterug> | Fields and delay choices read 2026-11-20. Consent text read 2026-11-23 (see *Consent box*). No account needed. |
| Privacy statement (over.arriva.nl) | <https://over.arriva.nl/privacy> | Read 2026-11-23. |
| Disclaimer (over.arriva.nl) | <https://over.arriva.nl/disclaimer> | Read 2026-11-23. |
| Privacy statement (www.arriva.nl, "Laatst geüpdatet 22 mei 2026") | <https://www.arriva.nl/privacy/> | Read 2026-11-23 by the maintainer in a browser. |
| Disclaimer (www.arriva.nl) | <https://www.arriva.nl/disclaimer/> | Read 2026-11-23 by the maintainer in a browser. |
| Terms overview | <https://www.arriva.nl/voorwaarden-en-privacy/voorwaarden-overzicht/> | Read 2026-11-23 by the maintainer in a browser. The conditions it lists were not read. |
| General transport, subscription, OVpay and OV-pas conditions | linked from the terms overview | Not read (they concern travel, not website use). Accepted by the maintainer (2026-11-23). |

The www.arriva.nl pages return 403 to automated readers; the maintainer opened them in a browser and the texts
below come from that reading.

## What the read pages say

### Een vergoeding aanvragen (www.arriva.nl, read 2026-11-23)

- "Je verzoek tot restitutie dient binnen 3 maanden na de gebeurtenis bij ons ingediend te zijn. Voor de
  afhandeling van je verzoek hanteren wij een maximumtermijn van 7 tot 10 werkdagen. Om voor een vergoeding in
  aanmerking te komen, vul je het contactformulier in."
- Exclusions: "Er wordt geen geld terugbetaald of gecompenseerd als: Je reisrecht (een abonnement) op een OV-pas,
  betaalpas of OV-chipkaart staat en je daarmee niet bent in- of uitgecheckt; Er sprake is van overmacht.
  Bijvoorbeeld bij extreme weersomstandigheden of een stroomstoring."
- "De hoogte van je vergoeding bij een vertraging met de trein hangt af van je vervoerbewijs. Bij een
  NS-abonnement of -product gelden de spits- en daltijden van NS. De hoogte van de vergoeding is nooit meer dan
  de ritprijs."
- The regional rules sit behind a region picker; their text was not pasted on 2026-11-23. The rules in
  `carrier_policy.rs` come from the read of 2026-11-20.
- No clause about apps, automated submission, intermediaries or submitting on someone's behalf.

### Disclaimer (www.arriva.nl and over.arriva.nl, read 2026-11-23)

- Both pages have the same text: "Wij hebben op de inhoud van deze internetsite (alle teksten, beelden, geluiden
  en programmatuur) het auteursrecht. Van deze internetsite mag een bezoeker daarom niets kopiëren of veranderen."
- It also excludes liability. Nothing about automated use, bots, third parties or intermediaries.
- For this review: OV Refund copies no Arriva content. It fills the form with the user's own data, and its
  attachments are a Mijn NS screenshot and an open-data delay card.

### Privacy statement (over.arriva.nl, read 2026-11-23)

- Contact-form data is used "Voor het behandelen van vragen en klachten met betrekking tot Arriva".
- The controller listed is Arriva Personenvervoer Nederland B.V.
- No processors are named for contact data. HubSpot and reCAPTCHA are not mentioned.
- Retention: "niet langer dan noodzakelijk".

### Privacy statement (www.arriva.nl, "Laatst geüpdatet 22 mei 2026", read 2026-11-23)

- Contact-form data: "Wanneer je contact hebt met Arriva, via telefoon, e-mail, chat, social media,
  contactformulier, …" and "Voor het behandelen van vragen en klachten met betrekking tot Arriva en voor het in
  behandeling nemen en beslechten van eventuele geschillen tussen jou en Arriva. Ook kan correspondentie worden
  gebruikt worden ter evaluatie en verbetering van de kwaliteit van dienstverlening alsmede ten behoeve van
  training, coaching en beoordelingsdoeleinden".
- Newsletters only with consent: "Wanneer je toestemming hebt gegeven voor het ontvangen van de nieuwsbrief en/of
  speciale aanbiedingen van Arriva. Je kunt je toestemming altijd intrekken door middel van de afmeldlink die je
  in iedere nieuwsbrief of e-mail aantreft."
- Outside the EEA: "Arriva kan persoonsgegevens delen met partijen die zowel binnen als buiten de Europese
  Economische Ruimte (EER) zijn gevestigd … Europese modelcontractbepalingen".
- Retention: "niet langer dan noodzakelijk voor het doel waarvoor ze zijn verzameld en nooit langer dan wettelijk
  is toegestaan".
- Controller contact: Arriva Personenvervoer Nederland B.V., via the data protection officer (Functionaris Gegevensbescherming) named in its privacy statement.
- HubSpot and reCAPTCHA are not named.

### Terms overview (www.arriva.nl, read 2026-11-23)

- Lists the subscription, OVpay, OV-pas and general transport conditions (not read; they concern travel, not
  website use).
- "Geld terug bij vertraging: Dien een geld terug verzoek in via het contactformulier wanneer je geld terug wilt
  vragen als je vertraging hebt opgelopen tijdens je reis."

## Purpose of the public form

Arriva's form is public and meant for travellers to ask for a delay refund for their own Arriva train trip.
OV Refund uses it for exactly that: one claim for one trip of the user, in the user's own name, with the user's own
details. It is not used for anyone else's trips and not for bus trips. Arriva assesses every claim and decides.

## NS Vrij subscriptions

Arriva's required fare field asks “Wat was de prijs van je rit (€)?” The form has no subscription option or
free-text box. Arriva's regional refund page says “Bij een NS-abonnement of -product gelden de spits- en
daltijden van NS. De hoogte van de vergoeding is nooit meer dan de ritprijs.”

[NS's refund table](https://www.ns.nl/service-en-contact/geld-terug/vergoedingen-geld-terug-bij-vertraging)
(last changed 2025-12-10) gives these fractions of the monthly fee for 30–59 and 60+ minutes respectively.
Weekend Vrij uses 1/12 and 1/6, Dal Vrij uses 1/36 and 1/18, and Altijd Vrij uses 1/42 and 1/21.
For confirmed free trips, OV Refund divides the monthly fee by the 60+ divisor (6, 18 or 21). Arriva's
50% and 100% bands then reproduce the corresponding NS fractions, subject to cent rounding. NS's separate
€2.30 minimum payout does not enter the Arriva calculation. The app labels the fare as the subscription's
derived per-trip value. This use of the fare field is the maintainer's interpretation. Arriva does not prescribe
this calculation.

## What is automated, and what is not

| Step | Who | Notes |
|---|---|---|
| Finding Arriva trips and applying Arriva's rules | App | From the user's own Mijn NS history. Limburg (incl. Maaslijn) from 15 min, other Arriva train regions from 30 min; 50% / 100% bands; 3-month claim period. The app uses the recorded fare or the NS Vrij per-trip value described above. Travellers submit at Arriva themselves if the fare or per-trip value, the region or the full card number is unknown, or if they used a student travel product. |
| Starting a claim | **User** | Only the button *Claimen*, per trip. Arriva claims never enter automatic submission. |
| Making the attachments | App, locally | Mijn NS travel-history screenshot (other trips and account name blanked) with a caption strip holding the full card number, date and route; Rijden de Treinen delay card. |
| Approving the attachments and the claim | **User** | A confirmation with the trip summary that quotes the consent box's sentence; then the preview *Bijlagen* → *Klopt, ga verder*. Cancelling deletes the attachments. |
| Filling the form | App | Hidden WebView2 window with its own profile; every field is read back and checked before sending. |
| CAPTCHA (Google reCAPTCHA Enterprise, invisible) | Google / **user** | Never solved or bypassed by the app. If Google shows a puzzle, only the puzzle is shown and the user solves it. |
| Sending | App, once | One submit per claim. No retries; an uncertain outcome is never resent automatically. |

Network guard (CDP request interception in the Arriva window): navigation only to `klantenservice.arriva.nl`;
reads pass; writes are blocked except Google's reCAPTCHA calls, HubSpot's attachment upload in the user's own
claim, and exactly one HubSpot submit after all checks pass and the claim was recorded as *Submitting*. No other
write can leave the claim window, so only that one submit can make a claim "possibly sent". A check, the demo,
the form test and the maintainer's dry run never upload and never submit.

## Consent box

Arriva's form (read in the app's guarded window on 2026-11-23, nothing sent) shows, above a required checkbox
named `LEGAL_CONSENT.subscription_type_3667673841`, the sentence:

> Wij gebruiken deze persoonsgegevens uitsluitend voor de in onze privacyverklaring omschreven doeleinden.

The checkbox label is "Ik ga akkoord.*", followed by "Voor meer informatie lees je hier ons privacybeleid."

OV Refund ticks the box on the user's behalf only when the form requires it. The confirmation dialog quotes
Arriva's sentence and names the box, and the user must tick "Ik ga akkoord dat OV Refund namens mij “Ik ga
akkoord” aanvinkt op het formulier van Arriva" before anything is made or sent.

**Caveat (not confirmed; accepted by the maintainer, 2026-11-23):** in HubSpot, a `subscription_type_*` legal-consent field is normally a
communication subscription consent. Ticking it might therefore also subscribe the user to an Arriva
communication type. The form's text does not say so, and the www.arriva.nl privacy statement says newsletters
need consent and carry an unsubscribe link. Whether this box does that is not known.

## Data sent to Arriva (via HubSpot)

First and last name, e-mail address, IBAN, full OV-chipkaart number, card expiry date (only if the user entered it
and the form asks for it), departure and arrival station, travel date and time, fare, delay band, region, the two
attachments, and the consent box when required. Nothing else is sent. Arriva's site, HubSpot and Google reCAPTCHA see the user's IP address.
Attachments are deleted locally once the outcome is final.

## Review conclusion and open points

- **Known:** none of the read pages restricts submitting the public contact form with tools, or by the traveller
  using an app for their own claim. The user starts and approves each claim; one claim per trip; the app never
  bypasses the CAPTCHA.
- **Accepted by the maintainer (2026-11-23):** whether the consent box (`subscription_type`) also subscribes the
  user to communications is not confirmed (see the caveat above). Not a release blocker.
- **Accepted by the maintainer (2026-11-23):** Arriva's general transport conditions (and the subscription, OVpay
  and OV-pas conditions) have not been read. Not a release blocker.
- **OPEN (the only open point):** no real Arriva claim has been submitted yet; HubSpot's success response and Arriva's reference format
  are unknown (see `docs/verified-flow.md`).
- **Recommended before wide promotion:** ask Arriva customer service whether personal, user-started submission of
  its public form by a local tool is acceptable, and keep the answer on file. Re-check the rules page on each
  release.
