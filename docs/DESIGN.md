# UI design brief

**Direction:** a statement/ledger, sober like a bank statement or a station departure board. It is a utility the user glances at, not a marketing page.

## Layout (one page, no tabs)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ OV Refund        ● Verbonden met Mijn NS · bijgewerkt dd mmm uu:mm  [Controleer nu] │
├───────────────┬──────────────────────────────────────────────────────────────┤
│ ACCOUNT       │ Uitbetaald €x,xx │ In behandeling €x,xx │ Te claimen €x,xx │ … │
│ OV-chipkaart  │──────────────────────────────────────────────────────────────│
│ •••• nnnn     │ [Week|Maand|Jaar|Alles]   Status: [Alles ▾]          nn reizen │
│ Rekening      │──────────────────────────────────────────────────────────────│
│ •••• nnnn     │ Datum   Traject           In–uit   Vertraging  Vergoeding Status │
│ E-mail        │ dd mmm  Station A→B        uu:mm–uu:mm  +nn   € x,xx   Te claimen ›│
│ …             │   ↳ expanded row: planned vs taken trains, reasons, actions   │
│ AUTOMATISCH   │ …                                                             │
│ [x] Controleer│                                                               │
│   elke [7] d  │                                                               │
└───────────────┴──────────────────────────────────────────────────────────────┘
```

## Rules

- **Type:** `Segoe UI Variable`, `Segoe UI`, system-ui. Use 13 px for the base size, 12 px uppercase small-caps for labels, and `font-variant-numeric: tabular-nums` for all times and amounts.
- **Colour:** a warm off-white page (`#f7f6f3`), an ink text colour (`#1c1d1f`), and grey only for secondary text. There is **one** accent, NS-like deep blue `#00307a`, used for the primary button and focus states only. Status colours are applied to *text*, never to filled badges. There is a full dark theme as well.
- **Structure:** use hairline rules (`1px` borders) and whitespace. No drop shadows, no rounded "cards", no gradients, no glow. The maximum radius is 4 px, on buttons and inputs.
- **No** emoji, decorative icons, stat tiles with big icons, purple, or Inter.
- **Data first:** the journey ledger is the page. Claims are rows in that ledger, and expanding a row shows evidence and actions inline, not in a modal (except the final submit confirmation).
- **Money:** amounts are right-aligned and use Dutch formatting (`€ x,xx`). An estimate is shown as `≈ € x,xx` with an explanation on hover. An unknown amount is shown as `–`, never invented.
- **Language:** Dutch first, with an English toggle in the footer.
