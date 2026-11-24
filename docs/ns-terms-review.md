# NS terms review

Reviewed 2026-11-13 from public pages only (no login). This is a plain reading by the project, **not legal advice**.
Quotes are in the original Dutch, shortened to the relevant words.

## Sources

| Document | URL | Version |
|---|---|---|
| Gebruiksvoorwaarden NS-account (covers Mijn NS) | <https://www.ns.nl/voorwaarden/gebruiksvoorwaarden-ns-account> | "van kracht vanaf 18 oktober 2024" (last changed 18 Oct 2024) |
| Geld terug bij vertraging (service page) | <https://www.ns.nl/service-en-contact/geld-terug/geld-terug-bij-vertraging> | read 2026-11-13. The "Voorwaarden" link on the page was a placeholder (`#`) at that time |
| Public claim form (no account) | <https://gtbv-formulier.ns.nl/ns/klantenservice-geld-terug-bij-vertraging> | linked from the service page for bank card/OVpay, e-tickets, cards without Mijn NS |

The old link in `docs/integration-inventory.md` (`/gebruiksvoorwaarden-ns-account` at the site root) now returns 404.

## What the NS-account terms say

| Art. | Quote / summary | Relevance to OV Refund |
|---|---|---|
| 1.1 | Access to the online environments is via "de website van NS of NS-apps". | Describes how access normally works. No explicit ban on other software. OV Refund loads NS's own website in WebView2. |
| 2.3 | "De inloggegevens van jouw NS-account dien je geheim te houden en mag je niet met derden delen." and "Het is niet toegestaan het NS-account van een ander te gebruiken of anderen toegang te geven tot jouw NS-account." | The password stays on the user's own PC (Windows Credential Manager) and is typed only on login.ns.nl. No third party, including the authors, receives it. A user must **not** run OV Refund for someone else's account. The FAQ says so. |
| 2.5 | NS may restrict or deny access "geheel of gedeeltelijk, tijdelijk of blijvend" on suspected misuse or breach. | Real risk for users: NS can block an account it considers misused. Mentioned in the disclaimer. |
| 2.8 | "Commercieel gebruik van de (reis)gegevens die worden vertoond in het NS-account is niet toegestaan." | OV Refund is free, has no commission and no server. The user uses their own data for their own claim. Paid claim services are the more obvious target of this clause. |
| 2.9(b) | Prohibits developing or using "software, hardware of andere middelen ... om onrechtmatige toegang te krijgen" to accounts, including to search ("doorzoeken") or copy ("kopiëren") account data. | **Main point of attention.** The wording targets *unlawful/unauthorised* access. OV Refund only uses the user's own, authorised session in NS's own website and never bypasses login or bot checks. Whether NS would see automated reading of one's own travel history as covered by 2.9(b) is not settled by the text. |
| 2.9(c) | Prohibits monitoring the "beschikbaarheid, prestaties en/of functionaliteit" of accounts. | Not what the app does (one check per interval), but very frequent polling could look like it. Keep the 7-day default and one run at a time. |
| 2.9(f) | Prohibits use for commercial or non-personal purposes. | Fits the personal, free use case. Would be a problem for any hosted or paid variant. |
| 4.3 | Copying or reproducing the account "en de content ervan" needs NS's prior written permission. | The app stores the user's own trips locally for the user's own claims. A strict reading could cover this. Low practical risk, but worth noting. |

Not addressed anywhere in the account terms: automated or scripted access as such, robots/scraping by name,
third-party apps or tools, password managers or storing the password, or claiming via apps or claim services.

## Geld terug bij vertraging page

- Claims up to 3 months after the travel date. No refund under EUR 2.30. E-tickets/mobile tickets only from 60 minutes.
- Personal OV-chipkaart with Mijn NS: the claim starts from the trip in travel history (the route OV Refund uses).
- Bank card / OVpay, e-tickets, cards without Mijn NS: the public online form. NS-Business Card: Mijn NS Zakelijk.
  International: NS International.
- Nothing on the page about claims filed by apps or third parties, or by a person other than the traveller.

## Review conclusion and open questions

- **No explicit prohibition** of using one's own Mijn NS account through a personal tool was found, and no clause
  about claiming via apps or claim services.
- **Not a clear blocker, but a real risk:** art. 2.9(b) (software to search/copy account data, qualified by
  "onrechtmatige toegang") and art. 2.5 (NS may block accounts) leave room for NS to object. Mitigations already in
  place: own account only, password never leaves the PC, no bypassing of login/bot checks, low frequency, free and
  non-commercial, user-approved submissions by default.
- **Recommended before wide promotion:** ask NS (customer service or privacy/legal contact) whether personal
  automated use of Mijn NS travel history and the delay form is acceptable, and keep the answer on file. Re-check
  these terms on each release. They can change with four weeks' notice.
- Website and README wording now say: the user acts in their own name, should follow NS's terms, OV Refund is not
  affiliated with or approved by NS, and automation is at the user's own risk.
