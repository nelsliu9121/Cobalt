# BOMTOON wallet balances and Account history

## Status

Approved design. Implementation has not started.

## Goal

Collect the signed-in BOMTOON user's coin and ticket information without exposing credentials or blocking reading. Show the aggregate Coin count in the Library and Recent top bars, and provide a dedicated Account page with Coin types, Ticket totals, and 90-day expiration rows. Ticket information is Account data; an episode does not directly consume a Ticket.

The balance summary must have one explicit refresh path so a future successful purchase or rental can re-query the displayed count without introducing polling or a second wallet implementation.

## Requirements

- Request the aggregate wallet summary when the app starts.
- Keep the library usable while the wallet summary loads or fails.
- Show only the aggregate coin count in Library and Recent top bars.
- Do not show tickets in Library or Recent top bars.
- Provide an Account action from the library surface.
- Refresh the wallet summary whenever Account opens.
- Load 90-day, expiry-sorted coin and ticket rows only while Account is open.
- Retain the complete aggregate ticket total and every valid ticket expiration row returned by the fixed 90-day Account query.
- Keep Ticket balances and expiration history in Account only; do not present Tickets as direct episode payment.
- Preserve cached summary data during a refresh and reject stale outcomes.
- Isolate wallet failures from library, episode, and reader behavior.
- Clear wallet state and invalidate active wallet work on sign-out.
- Expose one app-internal summary refresh operation that successful episode commerce can invoke.

## Non-goals

- Purchasing or renting episodes in this change.
- Spending, exchanging, charging, or otherwise mutating coins or tickets.
- Periodic wallet polling.
- Persisting balances across app runs.
- Adding a new SDK task, protocol message, managed-credential provider, or network-service aggregation layer.
- Claiming an authenticated browser capture or a directly observed response schema.
- Deriving a current permanent-versus-expiring balance allocation by subtracting history rows from the aggregate balance. The production bundle does not establish that history rows are unspent balances.

## Production-bundle evidence

The API contract is derived from BOMTOON's deployed Taiwan Next.js client, build `OwHQUV9KN_XCsgOkqJPqz`, inspected on 2026-08-30. No authenticated DevTools Network request or real response body was captured. The implementation must therefore parse defensively and keep every wallet failure non-destructive.

The deployed client provides these relevant routes and computations:

- `GET /api/balcony-api-v2/asset/user` supplies `data.coinBalance` and `data.ticketBalance`.
- The client computes aggregate coins as `coin + bonusCoin + freeCoin`.
- The client computes aggregate tickets as `ticket + bonusTicket + freeTicket`.
- `GET /api/balcony-api-v2/payment/charge` accepts `createdAt`, `sort`, `coinKind`, and an optional `coinStatusGroup`.
- The wallet page uses a 90-day `createdAt` window and supports `sort=EXPIRE` for `coinKind=COIN` and `coinKind=TICKET`.
- Later authenticated commerce evidence established that Tickets do not directly rent or purchase episodes. Episode rental uses Coin or a title-scoped Gift; permanent episode purchase uses Coin.

Evidence URLs:

- `https://www.bomtoon.tw/_next/static/OwHQUV9KN_XCsgOkqJPqz/_buildManifest.js`
- `https://www.bomtoon.tw/_next/static/chunks/71657-832ab2b1e86e77b5.js`
- `https://www.bomtoon.tw/_next/static/chunks/78380-89058720c21d9a72.js`
- `https://www.bomtoon.tw/_next/static/chunks/pages/my/asset/[asset]-3b8664093d94b359.js`
- `https://www.bomtoon.tw/_next/static/chunks/pages/detail/[alias]-21a7ecf7f1dd5c5b.js`

These are undocumented internal endpoints already used by the existing app's Balcony API integration. A future production-bundle change can invalidate the inferred schema; strict parsing and section-local failure are required.

## Domain model

### Wallet summary

The app stores the remote buckets rather than only a precomputed total:

```text
WalletSummary
  coins
    coin
    bonus_coin
    free_coin
  tickets
    ticket
    bonus_ticket
    free_ticket
```

Every amount is a bounded, non-negative integer. Aggregate accessors use checked addition:

```text
total_coins = coin + bonus_coin + free_coin
total_tickets = ticket + bonus_ticket + free_ticket
```

The top bar consumes only `total_coins`. Account consumes `total_tickets` and the named remote Ticket types without losing information. Episode commerce does not consume or present Ticket balance.

### Expiration rows

A wallet-history row retains only displayable, validated data:

```text
ExpirationRow
  kind: Coin | Ticket
  subtype
  quantity
  expires_at: optional timestamp
  description: optional bounded text
```

A positive future timestamp produces an `Expires YYYY-MM-DD` row. A missing or zero timestamp produces a `No expiry` row. A timestamp at or before the current date produces an `Expired YYYY-MM-DD` row. These are grant/history classifications, not claims about how the current aggregate balance is allocated.

Coin and ticket histories remain separate result sections so either can succeed independently. Each history response is capped at 512 KiB, 256 remote entries, and 256 retained display rows; each retained description is capped at 256 UTF-8 bytes. The summary response is capped at 64 KiB. Amounts must fit `usize`, aggregate addition is checked, and millisecond timestamps must fit `i64`.

### Episode-commerce boundary

The wallet parser retains aggregate Ticket buckets for Account presentation and history only. `Episode` does not retain Ticket applicability or Ticket quantity. Gift and Coin episode commerce is specified separately in `2026-08-30-bomtoon-episode-commerce-design.md`.

## API tasks

### Summary

`api::asset_summary()` creates one bearer-authenticated JSON fetch:

```text
GET https://www.bomtoon.tw/api/balcony-api-v2/asset/user
```

It uses the existing `bomtoon-access-token`, Balcony headers, language header, credential separation, host policy, and a 64 KiB response-byte ceiling. Credentials never enter the URL, normal headers, application state, parser errors, or logs.

The parser requires the established `result == "SUCCESS"` envelope and expected `data.coinBalance` and `data.ticketBalance` objects. Missing, negative, overflowing, or wrongly typed amounts fail the summary as a unit.

### Expiration history

Account starts two independent authenticated fetches:

```text
GET /api/balcony-api-v2/payment/charge
  ?createdAt=<Asia/Taipei timestamp 90 days before request>
  &sort=EXPIRE
  &coinKind=COIN

GET /api/balcony-api-v2/payment/charge
  ?createdAt=<Asia/Taipei timestamp 90 days before request>
  &sort=EXPIRE
  &coinKind=TICKET
```

The timestamp calculation follows the deployed client. Query construction is typed and fixed; user data cannot add parameters. Both requests use the same credential and Balcony header rules as the summary.

History parsing accepts only the success envelope, at most 256 array entries, and at most 256 flattened display rows from a response no larger than 512 KiB. It extracts supported quantity and expiration fields, rejects descriptions longer than 256 UTF-8 bytes, discards zero-quantity components, and preserves the server's `sort=EXPIRE` order. Malformed required data fails only the affected section. History never supplies or implies an unspent balance.

## Task and refresh state

Wallet work is independent of the existing single foreground `task`/`pending` pair used by library, recent, content, and logout flows. It uses a task-ID-keyed registry with explicit purposes:

- summary;
- coin expiration history;
- ticket expiration history.

Each task records a wallet generation. Outcomes first remove their exact registry entry, then mutate state only when their generation is current. Unknown, cancelled, and prior-generation outcomes are no-ops.

At most three wallet tasks are active: one summary, one coin history, and one ticket history. Wallet work never starts while the reader owns background task capacity, and opening Account is unavailable from the Reader view. Together with the app's single non-reader foreground request, the design stays within the SDK-wide four-task limit.

### Summary refresh seam

The episode-commerce success handler calls this same operation after an accepted Coin rental or purchase.

The operation is re-entrant:

1. If no summary request is active, increment the summary generation and start one.
2. If a summary request is active, mark one follow-up refresh as requested instead of spawning duplicates.
3. When the active request settles, start exactly one follow-up request when marked.
4. Keep cached summary data visible throughout.
5. Allow only the newest generation to replace the cache.

This coalesces bursts without losing a refresh requested during an active fetch. It introduces no timer or polling. The seam refreshes the complete `/asset/user` summary, so future coin or ticket spending updates both cached aggregates consistently.

### Account detail refresh

Opening Account immediately renders cached summary and history state, starts the summary refresh, and starts fresh coin and ticket history generations. Reopening Account invalidates older detail generations. Leaving Account does not block reading; late results may update the cache but cannot change the current view or replace newer generations.

## UI

### Library and Recent

The top-bar title is compact:

- loaded: `Library · Coins 120` or `Recent · Coins 120`;
- initial loading: `Library · Coins…`;
- no cached value after failure: `Library · Coins unavailable`;
- cached value after a failed re-query: keep `Library · Coins 120`; Account exposes the stale/error state and retry action.

The top bar never displays tickets. Balance loading and failure never replace library content with a loading or error screen.

An `Account` action opens the dedicated page. Existing shelf selection, pagination, comic actions, sign-out, and Back ownership otherwise remain unchanged.

### Account

The Account page owns Back and returns to the exact prior shelf and page. It contains:

- aggregate coin total;
- the remote coin buckets with clear labels;
- aggregate ticket total;
- the remote ticket buckets with clear labels;
- paginated 90-day coin history rows classified as `Expires`, `Expired`, or `No expiry`;
- paginated 90-day ticket history rows with quantity and expiration status.

The UI does not relabel a remote bucket as a guaranteed current permanent balance. `No expiry` is attached only to a history component whose response contains no positive expiry timestamp.

Empty history sections say `No coin expiration records` or `No ticket expiration records`. A loading section leaves already cached content visible. A failed section says `Unavailable`; `Retry balances` refreshes the summary and only failed detail sections. Successful sections are retained.

### Comic/Episodes

Comic and episode pages do not display Ticket balance, Ticket applicability, or Ticket quantity. A Ticket may discount a Coin top-up or redeem into Free Coin, but neither operation is episode commerce.

The episode-commerce design separately adds title-scoped Gift balance and Coin/Gift transaction actions. Account remains the sole surface for aggregate Ticket balances, buckets, and history.

## Failure and account behavior

- Initial summary failure leaves reading available and shows `Coins unavailable`.
- A later summary failure preserves cached values and records them as stale on Account.
- Coin-history failure affects only the coin-history section.
- Ticket-history failure affects only the ticket-history section.
- Parser errors use bounded, credential-free messages.
- `NoCredential` and `Unauthorized` from any wallet task use the existing SignedOut and Expired account transitions, clear account-owned data, cancel wallet tasks, and invalidate generations.
- Transient wallet transport or format failures never clear library, episode, or reader state.
- Sign-out cancels wallet tasks before clearing summary and history caches.
- App exit releases all wallet task state.

## Security and resource bounds

All wallet requests are read-only GETs to the existing BOMTOON allowlisted origin. No purchase, rent, charge, exchange, or mutation endpoint is introduced.

The summary response ceiling is 64 KiB. Each history response ceiling is 512 KiB, with at most 256 remote entries, 256 retained display rows, and 256 UTF-8 bytes per retained description. Parsers apply these limits before retaining display models. Pagination borrows bounded rows instead of cloning the complete response for each page. The summary cache contains six integers plus task/error metadata; the two history caches contain at most 512 display rows in total.

Credentials remain runtime-managed through `Credential::bearer("bomtoon-access-token")`. The app never receives token bytes and never calls `/api/auth/session`.

## Verification

### API construction

- Summary uses the exact `/asset/user` URL, managed bearer credential, JSON accept header, Balcony headers, zero offset, and 64 KiB ceiling.
- Coin and ticket history use the exact `/payment/charge` route and fixed 90-day `createdAt`, `EXPIRE`, and `coinKind` parameters.
- Credentials never enter URLs or regular headers.
- History query construction cannot be influenced by remote strings or UI actions.

### Parsing

- Valid summary buckets produce checked aggregate coin and ticket totals.
- Missing envelopes, non-success results, missing buckets, negative values, wrong types, and checked-addition overflow fail closed.
- Valid history produces stable, bounded expiration rows.
- Zero quantities are omitted.
- Missing, zero, future, and expired timestamps receive the exact display classification.
- Invalid timestamps, oversized arrays, oversized text, and malformed required fields fail only their section.
- Episode parsing does not infer direct Ticket applicability from `coinKind`.
- Unknown coin kinds do not affect readability.

### Application behavior

- Startup spawns library and summary requests independently.
- Summary loading and failure do not suppress library actions.
- Library and Recent top bars show only aggregate coins.
- Opening Account renders cached data and starts summary plus detail refreshes.
- Account partial successes remain visible when another section fails.
- Back restores the prior shelf and page.
- Account retains aggregate Ticket balances, buckets, and history.
- Comic and episode pages show no direct Ticket-payment UI.
- A summary refresh requested during an active request coalesces into exactly one follow-up.
- Prior-generation outcomes cannot overwrite newer summary or detail data.
- Cached summary survives transient refresh failure.
- The app-internal post-purchase/rent refresh seam exercises the same summary path without implementing a purchase.
- Sign-out and credential failures clear wallet data and invalidate late outcomes.

### Layout and simulator evidence

Account, Library, Recent, and Comic/Episodes layouts receive checks with `CLARA_BW_METRICS`, including maximum formatted counts, unavailable labels, empty histories, and a full history page. The completed implementation is exercised in the browser simulator and runtime simulator. No physical Kobo connection is part of verification.

## Implementation scope

Expected app-local files:

- `apps/bomtoon/src/api.rs`
- `apps/bomtoon/src/model.rs`
- `apps/bomtoon/src/parse.rs`
- `apps/bomtoon/src/main.rs`

No dependency, workspace member, catalog, protocol, SDK, runtime, CLI, credential-provider, or device-policy change is planned.
