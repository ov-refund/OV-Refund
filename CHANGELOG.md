# Changelog

## 0.1.5 — 2026-11-24

- Het reisbewijs voor Arriva lukt weer bij reizen waar Mijn NS een link "Beoordeel je reis" toont. Eerder weigerde OV Refund daar de schermafbeelding en stopte de claim (er werd niets verstuurd). / The travel proof for Arriva works again for trips where Mijn NS shows a "Beoordeel je reis" link. Before, OV Refund refused the screenshot there and the claim stopped (nothing was sent).

## 0.1.4 — 2026-11-24

- Beveiligingsupdate van de e-mailbibliotheek (lettre 0.11.24) en twee andere onderdelen (cookie, url). Dit dicht onder meer een lek waarbij iemand op het netwerk antwoorden van de mailserver kon vervalsen. / Security update of the e-mail library (lettre 0.11.24) and two other components (cookie, url). Among other things, this closes a hole where someone on the network could forge replies from the mail server.

## 0.1.3 — 2026-11-24

- Arriva-treinreizen claimen: OV Refund controleert Arriva-reizen uit je Mijn NS-reishistorie met de regels van Arriva. Je claimt zelf, per reis, met Claimen; Arriva-claims gaan nooit automatisch. Een echte Arriva-indiening is nog niet van begin tot eind geverifieerd. / Claim Arriva train trips: OV Refund checks Arriva trips from your Mijn NS travel history with Arriva's rules. You claim yourself, one trip at a time, with Claim; Arriva claims never go automatically. A real Arriva submission has not yet been verified end to end.
- Je ziet de bijlagen (reisbewijs uit Mijn NS en vertragingsbewijs van Rijden de Treinen) en een samenvatting voordat er iets verstuurd wordt. Een CAPTCHA-puzzel zie je alleen als Google erom vraagt. Bijlagen worden gewist zodra de uitkomst bekend is. / You see the attachments (travel proof from Mijn NS and delay proof from Rijden de Treinen) and a summary before anything is sent. You only see a CAPTCHA puzzle when Google asks for one. Attachments are deleted once the outcome is known.
- Het reisbewijs is een schermafbeelding van de echte Mijn NS-pagina; andere reizen en persoonsgegevens (zoals je e-mailadres en de kaartkeuze) zijn grijs afgedekt, en het bijschrift zegt dat. / The travel proof is a screenshot of the real Mijn NS page; other trips and personal details (such as your e-mail address and the card picker) are covered in grey, and the caption says so.
- In de demo doorloop je een Arriva-claim stap voor stap, zonder dat er iets wordt verzonden; een reis met een niet-ondersteunde vervoerder laat het label zien. / In the demo you go through an Arriva claim step by step without anything being sent; a trip with an unsupported carrier shows the label.
- Je naam en e-mailadres voor Arriva vul je in bij Jouw gegevens. / Enter your name and e-mail address for Arriva under Your details.
- De volledige reisgeschiedenis wordt weer goed opgehaald nu NS na het inloggen eerst een overzicht met alleen de laatste reizen toont. Een ingekorte lijst telt nooit als je hele geschiedenis. / The full travel history is imported correctly again now that NS first shows an overview with only the latest trips after login. A shortened list never counts as your whole history.
- Het NS-claimformulier wordt betrouwbaarder ingevuld: antwoorden en de verklaring worden na het klikken gecontroleerd en een mislukte stap wordt bij naam genoemd. / The NS claim form is filled in more reliably: answers and the declaration are checked after clicking, and a failed step is named.
- Reizen met OVpay (bankpas of OV-pas) worden nog niet ondersteund. Reizen met een andere vervoerder dan NS of Arriva staan als "Vervoerder nog niet ondersteund" en worden nooit geclaimd. / Trips with OVpay (bank card or OV-pas) are not supported yet. Trips with a carrier other than NS or Arriva show "Carrier not supported yet" and are never claimed.

- Arriva-teksten per regio: vanaf 15 minuten in Limburg (ook de Maaslijn), anders vanaf 30 minuten; 50% of 100% van de ritprijs; Arriva beslist. Is de regio, de ritprijs of je volledige kaartnummer onbekend, of reisde je met een studentenreisproduct? Dan dien je zelf in bij Arriva. / Arriva texts per region: from 15 minutes in Limburg (including the Maaslijn), otherwise from 30 minutes; 50% or 100% of the fare; Arriva decides. If the region, the fare or your full card number is unknown, or you travelled with a student travel product, submit the claim yourself at Arriva.
- Controles maken geen contact meer met Arriva. / Checks no longer contact Arriva.
- Een reis die op treintijden wacht, laat zien wanneer die verwacht worden: Rijden de Treinen publiceert de treintijden van een maand rond de 4e van de volgende maand. / A trip waiting for train times shows when they are expected: Rijden de Treinen publishes a month’s train times around the 4th of the next month.
- Automatisch indienen bij NS wordt niet meer geblokkeerd door een Arriva-claim die nog loopt. / Automatic NS submission is no longer blocked by an Arriva claim in progress.
- Kan OV Refund je NS-claimlijst niet lezen, dan dient de app niets automatisch in. / If OV Refund can't read your NS claims list, it submits nothing automatically.
- De geschatte vergoeding voor Dal Vrij en Weekend Vrij houdt rekening met feestdagen. / The estimated refund for Dal Vrij and Weekend Vrij takes public holidays into account.
- Een update die niet te lezen is, meldt de app als mislukte updatecontrole in plaats van "geen update". / An update file that can't be read is reported as a failed update check instead of "no update".
- Bij het terugzetten van een back-up wordt het bestand eerst gecontroleerd. / Restoring a backup checks the file first.
- Een vervaldatum van je OV-chipkaart in het verleden kun je opslaan (je reisde misschien met een kaart die inmiddels verlopen is); je ziet dan een opmerking. / A past OV-chipkaart expiry date can be saved (you may have travelled with a card that has expired since); you then see a note.
- Stopt een Arriva-claim voordat het formulier opent (er loopt al een controle), dan blijft de reis niet op "Bezig…" staan en worden de bijlagen meteen gewist. / If an Arriva claim stops before the form opens (a check is already running), the trip no longer stays "In progress…" and the attachments are deleted at once.
- Het afdekken in het reisbewijs is strenger: vlak voor de schermafbeelding wordt opnieuw gecontroleerd, en bij een ingesloten venster of open dialoog op de pagina wordt er geen schermafbeelding gemaakt. / Covering in the travel proof is stricter: it is checked again right before the screenshot, and no screenshot is taken when the page has an embedded frame or an open dialog.
- Bevestigen bij Arriva laat de zin van Arriva's toestemmingsvakje zien. / Confirming at Arriva shows the sentence of Arriva's consent box.
- Een gratis Arriva-reis met Weekend Vrij, Dal Vrij of Altijd Vrij kun je claimen in de app zodra OV Refund de ritwaarde volgens de NS-regel kan berekenen. / You can claim a free Arriva trip with Weekend Vrij, Dal Vrij or Altijd Vrij in the app once OV Refund can calculate the per-trip value using the NS rule.
- Lokale geschiedenis wissen zegt dat de gegevens uit NS-mails bewaard blijven als bescherming tegen dubbel indienen. / Erase local history says that the details from NS e-mails are kept as protection against submitting twice.
- Foutmeldingen bij de Arriva-gegevens staan direct bij het veld. / Errors in the Arriva details form are shown next to the field.
- Diverse verbeteringen in teksten en toegankelijkheid. / Various wording and accessibility fixes.

## 0.1.2 — 2026-11-19

- De volledige reisgeschiedenis (1,5 jaar) wordt weer opgehaald. Mijn NS schrijft de begindatum nu anders, waardoor OV Refund alleen de laatste week las. / The full travel history (1.5 years) is imported again. Mijn NS now writes the start date differently, which made OV Refund read only the last week.
- Een onvolledige reisgeschiedenis wordt niet meer als compleet gezien; na de update haalt OV Refund de hele periode één keer opnieuw op. / An incomplete travel history is no longer treated as complete; after the update OV Refund reads the full period once more.

## 0.1.1 — 2026-11-14

- Eerste publieke Windows-versie: reizen ophalen, vertragingen herkennen, claims bekijken en optioneel automatisch indienen. / First public Windows release: import trips, detect delays, review claims and optionally submit automatically.
- Vertragingen van minstens 30 minuten met passende historische treingegevens zijn te claimen, ook bij uitval of een gemiste overstap. / Delays of at least 30 minutes supported by historical train data are claimable, including cancellations and missed connections.
- Duidelijkere bediening, veilig stoppen tijdens controles en opnieuw proberen vóór een claimtermijn verloopt. / Clearer controls, safe stopping during checks and retries before claim deadlines.
- Vernieuwde website met Nederlands, Engels, licht en donker thema. / Updated website in Dutch and English, with light and dark themes.

- Strengere controle op dubbele claims en e-mailherkomst. / Stricter duplicate-claim and e-mail authentication checks.
- Voorzichtiger koppelen van treinen bij vertraging. / More conservative train matching for delayed trips.
- Minder gegevens in diagnostiek en duidelijkere privacyuitleg. / Less data in diagnostics and clearer privacy information.
- Verbeterde formulieren, vensters en mobiele website. / Improved forms, dialogs and mobile website.

Een echte indiening bij NS is nog niet van begin tot eind geverifieerd. NS beslist over elke aanvraag. Het installatiebestand heeft geen uitgevershandtekening; updates in de app worden cryptografisch gecontroleerd.

A real NS submission has not yet been verified end to end. NS decides on each request. The installer has no publisher signature; in-app updates are cryptographically verified.
