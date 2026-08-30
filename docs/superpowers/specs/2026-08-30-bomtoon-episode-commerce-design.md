# BOMTOON episode commerce

## Status

Implementation complete. Automated gates pass. Limited non-spending simulator smoke passes. Remaining simulator and operator-attended release checks are pending.

## Goal

Allow a signed-in user to perform one attended BOMTOON episode transaction from the episode list:

- consume a title-scoped Gift to rent an episode;
- spend Coin to rent an episode;
- spend Coin to purchase permanent episode access.

The app must quote before spending, fail closed when affordability or eligibility is unknown, durably record an account-scoped unresolved mutation before POST, reconcile every accepted or uncertain mutation before allowing another attempt, and leave the user on the refreshed episode list after success.

`CONTEXT.md` is the canonical glossary. In particular, a Gift rents an episode, while a Ticket discounts a Coin top-up or redeems into Free Coin. Tickets are not direct episode payment.

## Requirements

- Support one episode transaction at a time.
- Fetch an authoritative quote before showing transaction actions.
- Re-quote the selected mode immediately before mutation.
- Present Gift rental, Coin rental, and Coin purchase as explicit action buttons rather than radio choices followed by a second confirmation.
- Disable paid actions unless the Spendable Coin Balance is known and sufficient.
- Disable Gift rental unless the title Gift balance is known, positive, and the quote allows Gift rental.
- Submit the exact observed purchase request through the existing managed credential.
- Never optimistically decrement Coin or Gift balances or promote episode access.
- Reconcile access plus the affected balance after every accepted or uncertain mutation.
- Never repeat a mutation whose outcome is uncertain.
- Persist an account-scoped unresolved-mutation marker atomically and wait for its save acknowledgement before POST.
- Load durable commerce safety state before enabling commerce after startup or sign-in.
- Clear an unresolved marker only after conclusive same-account reconciliation and an acknowledged atomic delete.
- Disable commerce while authentication, account scope, or connectivity is unknown.
- Preserve unresolved markers across offline periods, sign-out, credential expiry, app exit, and device restart.
- Bump the wire protocol to version 13 and the coordinated Cobalt workspace/runtime release to `0.4.0`; publish BOMTOON app version `0.5.0` with minimum Cobalt `0.4.0` so older runtimes reject installation rather than fail on an unknown task.
- Keep an active rental readable until BOMTOON removes `RENT`, even if the local clock reaches its expiry.
- Show rental time as ceiling-rounded whole hours and refresh elapsed rentals on interaction rather than with a timer.
- Keep active rentals read-only; do not offer permanent purchase while BOMTOON reports `RENT`.
- Show the available Gift count for the current comic title on its episode page.
- Remove direct Ticket presentation and applicability from episode commerce while retaining Account Ticket balances and history.
- Preserve current reader, pagination, account, and wallet behavior outside the specified commerce transitions.

## Non-goals

- Coin top-up or real-money payment.
- Ticket redemption into Free Coin.
- Ticket discounts for Coin top-up.
- Mileage exchange.
- Direct Ticket payment for an episode.
- Bulk episode selection or purchase.
- Purchasing an episode during an active rental.
- Background polling or countdown timers.
- Persisting quotes, ordinary navigation, or resolved transaction history across app launches. Only unresolved financial intent is durable.
- Automated tests that spend real Coin or Gifts.
- General credential access or token exposure. A narrow managed opaque-account-scope operation is required for restart safety.

## Evidence

The contract is based on the deployed Taiwan client and three attended captures made on 2026-08-30. Raw HARs are secret-bearing local evidence and are excluded from Git; this specification records only allowlisted business fields.

### Gift rental

The observed Gift flow used:

```text
GET /api/balcony-api-v2/gift/contents/detail?contentsId=<numeric title id>
GET /api/balcony-api-v2/contents/price/<title alias>/<episode alias>?purchaseType=POSSESSION
POST /api/balcony-api/purchase
```

The POST body was:

```json
{
  "id": 6800,
  "purchaseType": "RENT_GIFT",
  "isMobile": false
}
```

The mutation returned HTTP 201 and `result == "SUCCESS"`. The title Gift entry's `usedCount` increased by one, the wallet summary did not change, and refreshed episode detail reported `purchaseStatus == "RENT"`, `isRentGift == true`, and a populated `rentExpiredAt`.

### Coin rental

The observed quote reported `purchaseType == "RENT"`, `coinKind == "COIN"`, `rentCoin == 2`, and no Gift eligibility. The POST body selected `RENT` and `isMobile == false`. The HTTP 201 success receipt matched the title and episode aliases and reported aggregate `useCoin == 2`. The Spendable Coin Balance decreased by exactly two and viewer access became available.

### Coin purchase

The observed quote reported `purchaseType == "POSSESSION"`, `coinKind == "COIN"`, and `possessionCoin == permanentCoin == 3`. The POST body selected `POSSESSION` and `isMobile == false`. The HTTP 201 success receipt matched the aliases and reported aggregate `useCoin == 3`. The Spendable Coin Balance decreased by exactly three and viewer access became available.

`isRepeatPurchase` is not an access signal; the attended permanent purchase returned `true`. The paid captures did not include a refreshed title-detail response, so refreshed `RENT` and `POSSESSION` statuses remain required implementation and release checks.

## Architecture

Add `apps/bomtoon/src/commerce.rs` as an app-local deep module. `Task::Post` already supports the mutation itself. Restart-safe account isolation additionally requires a narrow managed-credential metadata operation that exposes an opaque account scope without exposing credential bytes or the provider's account identifier.

### Commerce ownership

`Commerce` owns:

- the selected numeric title ID, title alias, numeric episode ID, and episode alias;
- the active flow state;
- parsed quote choices and the quote generation they belong to;
- title-scoped Gift balance, refresh generation, task identity, cached value, and error state;
- enablement rules for Gift rental, Coin rental, and Coin purchase;
- receipt expectations and unresolved accepted outcomes;
- the current opaque account scope and durable unresolved-mutation marker;
- store-operation identity and acknowledgement state;
- presentation data for the quote and accepted-but-stale overlays.

The flow states are:

```text
LoadingSafetyState
Idle
Quoting
Choosing
Requoting
PersistingIntent
Mutating
Reconciling
ClearingIntent
AcceptedButStale
```

`Commerce` exposes narrow effects rather than owning navigation or SDK context:

```text
RequestAccountScope
RequestLoadMarker
RequestSaveMarker
RequestQuote
RequestMutation
RequestContentRefresh
RequestWalletRefresh
RequestGiftRefresh
RequestForgetMarker
ShowQuote
ShowError
ShowAcceptedButStale
Complete
```

The exact Rust types belong in the implementation plan, but effects must make illegal transitions unrepresentable. Only `Requoting` can request a marker save, only a matching successful save can enter `Mutating`, and no state with a durable marker can emit another mutation.

### Existing ownership

- `api.rs` constructs exact Gift, quote, purchase, content, and wallet tasks.
- `parse.rs` validates bounded response envelopes and converts them into domain data.
- `model.rs` owns episode identity, price, access, rental expiry, and wallet models.
- `main.rs` owns navigation, screen construction, SDK `Context`, task spawning, wallet state, reader state, and application of `Commerce` effects.
- Existing `WalletState` continues to own `/asset/user` and Account history refreshes.
- Managed credential storage owns stable account scoping and exposes only an opaque scope to `main.rs`.

The existing single foreground task slot carries quote, re-quote, mutation, and content reconciliation serially. Title Gift refresh has one independently generation-tagged task so it can overlap title content or wallet summary loading.

Account coin and Ticket history tasks are Account-only. Leaving Account cancels active history work before another view starts. On an episode page the maximum is one foreground task, one Gift task, and one wallet-summary task, remaining below the SDK-wide four-task ceiling. Entering Reader starts no commerce or Gift work.

### Managed account scope

An unresolved marker must never be reconciled against another BOMTOON account. The managed login path supplies the provider's stable account identifier only to credential storage. Credential storage derives a non-reversible 128-bit scope using a device-local secret and exposes only that opaque scope through a new typed SDK/protocol result. The app never receives the source identifier, credential, derivation key, or token.

The same BOMTOON account receives the same scope across token renewal, sign-out, and re-login on that device. Different accounts receive different scopes. Removing a credential does not remove the non-reversible scope mapping needed to recognize a later login.

A legacy credential without scope metadata remains usable for existing read-only behavior, but commerce is disabled with a re-login instruction. Missing, denied, malformed, or stale scope results never fall back to a global or guessed account identity.

### Durable unresolved marker

`AppStore` stores one atomic value under a versioned commerce key:

```text
UnresolvedMutationV1
  account_scope: 128-bit opaque value
  title_id
  title_alias
  episode_id
  episode_alias
  purchase_type: RENT_GIFT | RENT | POSSESSION
  quoted_price
  pre_mutation_spendable_coin: optional
  pre_mutation_title_gifts: optional
```

The marker contains exactly one affected balance snapshot: Coin for paid actions or title Gifts for Gift rental. It contains no token, raw account identifier, title, episode title, receipt body, or user-entered text.

On startup, `LoadingSafetyState` requests both the current managed account scope and the marker before commerce can become `Idle`. Library and already-authorized reading may initialize independently, but no quote or POST action is exposed until authentication, scope, connectivity, and marker handling are settled.

## Domain model

### Content and episode identity

```text
ContentDetail
  id: numeric title id
  alias: title alias
  episodes: bounded list

Episode
  id: numeric episode id
  alias: episode alias
  title
  access
  rent_coin
  purchase_coin
  rent_expires_at
  gift_eligible
```

Both ID forms are required. The Gift endpoint and mutation body use numeric IDs; quote URLs and receipt matching use aliases.

### Episode access

```text
EpisodeAccess
  NotOwned
  Rented
  Owned
  Sample
  Free
  Other(remote value)
```

Classification rules:

- exact `POSSESSION` becomes `Owned`;
- exact `RENT` becomes `Rented`;
- existing sample and free signals retain their current behavior when no paid entitlement exists;
- unknown non-empty status becomes `Other` and is not made readable;
- `Owned`, `Rented`, `Sample`, and `Free` are readable.

A valid `rentExpiredAt` is retained as a checked millisecond timestamp. Remaining time is:

```text
ceil(max(rent_expires_at - now, 0) / 1 hour)
```

A positive fractional hour displays as `1 hr`; two days display as `48 hrs`. A `RENT` response without a usable expiry remains server-readable but displays `Rented` without an invented duration and reconciles before opening.

Local time never changes `Rented` to `NotOwned`. At `0 hrs`, interaction requests fresh detail before opening. If BOMTOON still reports `RENT`, the reader opens; if it removes access, the row returns to not owned. A failed refresh preserves cached server-granted readability but does not open until status can be reconciled.

### Spendable Coin Balance

```text
spendable = coin + bonus_coin + free_coin
```

All components are bounded non-negative integers and use checked addition. BOMTOON chooses bucket consumption; the user cannot select a bucket.

In a purchase receipt, `useCoin` is the aggregate amount spent. `useGoldCoin`, `useBonusCoin`, and `useFreeCoin` are an optional breakdown and must not be added to `useCoin` a second time. When all breakdown fields are present, their checked sum must equal `useCoin`.

### Title-scoped Gift balance

Gift inventory is title-specific, not part of `/asset/user`:

```text
GET /api/balcony-api-v2/gift/contents/detail?contentsId=<numeric title id>
```

For each `receivedGifts` entry, retain it only when:

- `giftType == "RENT"`;
- `isReceived == true`;
- `issuedCount` and `usedCount` are bounded non-negative integers;
- `usedCount <= issuedCount`.

Available Gifts are the checked sum of `issuedCount - usedCount` across retained entries. `receivableGifts` are not spendable and do not contribute. Any invalid retained entry or arithmetic overflow fails the Gift section rather than inventing a balance.

Gift balance refreshes on title open and after Gift mutation. A failed refresh disables only Gift action; it does not disable Coin actions or reading. The UI never decrements Gift balance locally.

### Quote

The initial not-owned-row action requests:

```text
GET /api/balcony-api-v2/contents/price/<title alias>/<episode alias>
  ?purchaseType=POSSESSION
```

The response retains only bounded business fields required by the modal:

- numeric title and episode IDs;
- title and episode aliases;
- `isAvailable`;
- `coinKind`;
- bounded non-negative `rentCoin` and `possessionCoin`;
- optional `permanentCoin`, which must equal `possessionCoin` when present or disable only permanent purchase;
- `isRentGift` and `isPossessionGift`.

Only exact `coinKind == "COIN"` permits Coin actions. Gift rental additionally requires exact `isRentGift == true`. `isPossessionGift` does not create an action because permanent Gift purchase is outside the defined Gift domain.

Selecting Gift or Coin rental re-quotes with `purchaseType=RENT`. Selecting Coin purchase re-quotes with `purchaseType=POSSESSION`. If price, eligibility, identity, or availability changed, Commerce replaces the modal with current data and does not mutate. Otherwise it proceeds directly to POST without a second confirmation.

### Mutation

```text
POST /api/balcony-api/purchase
Content-Type: application/json

{
  "id": <numeric episode id>,
  "purchaseType": "RENT_GIFT" | "RENT" | "POSSESSION",
  "isMobile": false
}
```

The request uses `Credential::bearer("bomtoon-access-token")`, existing Balcony headers, and the title-detail `x-referer`. Browser-only cookies, `Origin`, and captured authorization values are not copied into app code.

A success receipt requires:

- HTTP success and `result == "SUCCESS"`;
- exact effective purchase type;
- matching title and episode aliases;
- bounded Coin-use fields when present.

`isRepeatPurchase` and timestamps are not access proof. A success receipt means accepted, not reconciled.

## Interaction design

### Episode page

The title summary displays current cached balances independently:

```text
Coins 15 · Gifts 5
```

Loading and failure states are explicit and section-local. A cached value remains visible through refresh; its error state is available to retry. Unknown Coin balance disables only Coin actions. Unknown Gift balance disables only Gift action.

Episode rows expose:

- `Owned` → `Read`;
- `Rented` with expiry → `Read · N hrs`;
- `Rented` without usable expiry → `Read · Rented`;
- `Sample` or `Free` → `Read`;
- `NotOwned` → `View options`;
- `Other` → disabled remote-status label.

No episode row or title header displays Ticket quantity or Ticket applicability. Account continues to display aggregate Ticket balances, buckets, and history.

### Quote modal

The modal uses explicit buttons:

```text
Use Gift
Rent · N coins
Buy · N coins
Cancel
```

There are no radio buttons and no second confirmation. Unavailable actions remain disabled with a concrete reason such as `Gift balance unavailable`, `No Gifts for this title`, `Coin balance unavailable`, or `Need N coins`.

Coin actions require a known Spendable Coin Balance greater than or equal to the fresh quoted price. A confirmed Gift action remains available when Coin balance is unknown. Remote `isAvailable == true` suppresses mutation and requests content reconciliation instead.

### Success

After verified reconciliation, the modal closes and the app remains on the same episode list and page. The refreshed row and affected balance are visible. Permanent purchase also invalidates the cached library owned-episode count before back navigation.

An active `Rented` row opens the reader and exposes no purchase action until BOMTOON removes `RENT`. Rental hours recompute on user interaction and screen rebuilding; no timer or polling task exists.

## Mutation safety and reconciliation

Before POST, Cancel returns to `Idle`. A confirmed action first enters `PersistingIntent`, atomically saves `UnresolvedMutationV1`, and waits for the matching `StoreResult::Saved`. A denied, mismatched, cancelled, or unacknowledged save emits no POST. Only the acknowledged marker permits `Mutating`.

After the marker is durable, app/device exit and power loss are safe: restart reloads the marker and cannot expose another spend action. Commerce navigation remains pinned while the app is present, but app/device exit is never blocked.

### Explicit rejection

A structurally valid backend response that explicitly rejects the transaction shows a bounded safe message, clears the submitted quote, and permits a new quote. It never changes local access or balances.

### Accepted response

A success receipt records the expected access and affected balance, then starts authoritative reconciliation:

- Gift rental: refresh content detail and title Gift balance;
- Coin rental: refresh content detail and wallet summary;
- Coin purchase: refresh content detail and wallet summary.

Expected results are:

- Gift rental: `RENT` and available Gift delta of one;
- Coin rental: `RENT` and Spendable Coin Balance delta equal to aggregate receipt `useCoin`;
- Coin purchase: `POSSESSION` and the same receipt-backed balance rule.

No refreshed value is synthesized from the receipt. If another session changed a balance concurrently, the refreshed server value remains authoritative; the discrepancy is reported safely and the episode is never made eligible for a duplicate POST.

### Unknown outcome

POST transport failure, cancellation, oversized or malformed response, mismatched receipt, or any response that cannot prove explicit rejection is an unknown outcome. Commerce refreshes content, wallet, and title Gift balance before allowing any new transaction.

If same-account refreshed content proves the intended entitlement, the mutation is treated as accepted and can never be repeated. If same-account refreshed content proves no entitlement and the affected balance exactly matches the marker's pre-mutation snapshot, the flow can conclude that no mutation committed. Any contradictory, cross-account, offline, or incomplete result enters `AcceptedButStale`.

### Accepted but stale

If an accepted mutation cannot obtain every required authoritative refresh:

- show `Accepted, refresh needed`;
- retain selected identity and expected outcome;
- disable every spend action for that episode;
- expose only `Refresh status`;
- never emit another POST.

Refresh status retries only unresolved GET requests. `AcceptedButStale` is backed by the durable marker and therefore survives app exit, sign-out, credential expiry, and restart.

## Error, account, and task behavior

- Gift, quote, and receipt responses are each capped at 64 KiB.
- Gift parsing accepts at most 64 `receivedGifts` and 64 `receivableGifts` entries.
- Retained aliases are capped at 128 UTF-8 bytes, retained titles at 512 UTF-8 bytes, and safe backend result codes at 128 UTF-8 bytes.
- Counts, prices, receipt usage, and checked aggregates must fit `usize`; timestamps must fit `i64`.
- Existing content and wallet parsers retain their current response and collection bounds.
- Unknown `coinKind`, purchase type, or purchase status fails closed.
- Gift underflow or overflow fails only Gift state.
- Quote or Gift GET failure never implies a mutation.
- `401`, `403`, `NoCredential`, and `Unauthorized` use existing account transitions, clear volatile account-owned commerce data, cancel tasks, and invalidate generations without deleting an unresolved marker.
- Foreground outcomes must match the exact task and expected purpose.
- Gift outcomes must match both task ID and current title generation.
- Store outcomes must match the exact versioned marker key and expected save/load/forget operation.
- Unknown, cancelled, or prior-generation GET outcomes are no-ops except for their bounded visible retry state.
- Task-capacity failure preserves previous authoritative data and exposes retry.
- Sign-out cancels commerce and Gift tasks and clears volatile quote/receipt state, but preserves the durable marker and its account scope.

### Authentication and connectivity matrix

| Authentication | Connectivity | Marker | Behavior |
|---|---|---|---|
| Authenticated, matching scope | Online | None | Commerce enabled after fresh balances and quote |
| Authenticated, matching scope | Online | Present | Reconcile only; POST disabled |
| Authenticated, different scope | Online | Present | Reading allowed; all commerce locked; marker is not queried against the wrong account |
| Authenticated | Offline | Any | Already-loaded reading may continue; quote, Gift refresh, and POST disabled; marker retained |
| Signed out or no credential | Any | Any | Login surface; no commerce; marker retained |
| Credential expired | Any | Any | Expired-login surface; no commerce; marker retained |
| Authentication or scope unknown | Any | Any | Commerce hidden or disabled until resolved |

Cold-start offline state uses the existing network failure surface and `Join Wi-Fi`; it cannot manufacture account state or clear a marker. A disconnect before POST leaves the acknowledged marker for later reconciliation. A disconnect after POST is an unknown outcome and follows the same path.

Signing out with an unresolved marker is allowed. Re-login to the same account resumes reconciliation. Login to another account permits reading but keeps commerce locked with an instruction to restore the originating account.

### Clearing the marker

Conclusive accepted entitlement, conclusive explicit rejection, or same-account proof of no entitlement plus unchanged affected balance enters `ClearingIntent`. Commerce atomically forgets the marker and waits for matching `StoreResult::Forgotten`. A denied, mismatched, cancelled, or unacknowledged forget leaves commerce locked. Only acknowledged deletion can return to `Idle`.

## Security and privacy

- Credentials remain runtime-managed; the app never receives token bytes, raw provider account identity, or the device-local scope derivation key.
- No token, cookie, authorization header, complete HAR body, captured session response, raw user identifier, or reversible account identifier enters source, fixtures, documentation, app storage, errors, or logs.
- Raw evidence remains outside the feature worktree under ignored `/evidences/` paths.
- Before every commit containing this design or its implementation, staged paths are checked to ensure no `evidences/` file is present.
- Mutation errors report only bounded operation context and safe backend result codes, never raw request or response bodies.
- Request bodies contain only numeric episode ID, enumerated purchase type, and `isMobile=false`.
- All requests use the existing BOMTOON origin and `network` capability.
- The shared auth change is limited to provisioning and reading a stable opaque account scope. The BOMTOON credential allowlist expands only to exact wallet/Gift/quote GET shapes and exact purchase POST method/path; no capability, device-policy, origin, or general credential-reading API is added.

## Verification

### API construction

- Gift request uses the exact title Gift route and numeric title ID.
- Initial quote uses the aliases plus fixed `purchaseType=POSSESSION`.
- Re-quotes use fixed `RENT` or `POSSESSION` according to the selected action.
- POST uses the exact purchase route, JSON content type, managed bearer credential, Balcony headers, title-detail referer, response ceiling, and observed `isMobile=false` body.
- Remote strings cannot add query parameters, change the origin, or select an unsupported purchase type.
- Credential policy permits BOMTOON bearer GET only for the existing library/content/image routes plus exact asset-summary, fixed-shape expiration-history, numeric title-Gift, and enumerated quote URLs. It permits POST only to `/api/balcony-api/purchase` with no query or fragment. Wrong methods, origins, paths, aliases, IDs, query keys, query values, and suffixes remain denied.
- Protocol version 13 documents the new task tag and rejects v12 peers through the existing handshake/version checks.
- `apps/catalog.json` advertises BOMTOON `0.5.0`, minimum Cobalt `0.4.0`, with the existing `network` capability.

### Parsing and domain tests

- Gift sum accepts multiple received rental entries and ignores unreceived or non-rental entries.
- Gift underflow, overflow, wrong types, oversized input, and malformed envelopes fail only Gift state.
- Quote parses Gift rental, Coin rental, and Coin purchase choices.
- Unknown `coinKind`, missing identity, negative price, overflow, or mismatched aliases disables mutation.
- Receipt matching covers every supported purchase type and rejects mismatched identities.
- Receipt aggregate and optional breakdown are checked without double-counting `useCoin`.
- `POSSESSION` and `RENT` map to readable access; unknown statuses remain unreadable.
- Whole-hour display covers exact hours, fractional hours, two days, elapsed time, and missing expiry.

### Commerce transition tests

- Unknown or insufficient Coin balance cannot emit paid mutation.
- Unknown or zero Gift balance cannot emit Gift mutation.
- Confirmed Gift remains actionable when Coin balance is unknown.
- Changed re-quote replaces the modal and does not POST.
- Explicit rejection permits only a fresh quote.
- Every ambiguous POST outcome reconciles before retry.
- Accepted-but-stale never emits another POST.
- A matching atomic marker-save acknowledgement is required before every POST.
- App exit or power loss after marker save reloads into reconciliation-only state.
- Marker deletion must be acknowledged before commerce returns to `Idle`.
- Signed-out, expired, disconnected, unknown-scope, same-account, and different-account states follow the exact matrix.
- A different account can read but cannot reconcile, clear, or bypass the originating account's marker.
- Stale foreground tasks and Gift generations cannot change current state.
- Active rental exposes no permanent-purchase action.
- `0 hrs` reconciles before opening Reader.
- Verified permanent purchase invalidates the cached library owned count.

### Layout and runtime

`CLARA_BW_METRICS` layout checks cover maximum formatted balances, Gift loading/failure, quote actions and disabled reasons, `N hrs`, `Rented`, and accepted-but-stale recovery.

The completed UI is exercised in both browser and runtime simulators. Checks cover quote cancellation, insufficient/unknown balance, changed quote, success staying on the same episode page, rental reading, and status refresh. Simulator tests never submit a real mutation.

### Attended backend release checks

On an attended disposable/low-cost account state:

1. Gift rental produces Gift delta `-1`, refreshed `RENT`, expiry display, and readable access.
2. Coin rental produces the exact receipt-backed Spendable Coin Balance delta, refreshed `RENT`, expiry display, and readable access.
3. Coin purchase produces the exact receipt-backed balance delta, refreshed `POSSESSION`, and readable access.
4. A controlled refresh failure after an accepted mutation reaches `Accepted, refresh needed` and cannot issue another POST.
5. Restart after an accepted POST reloads the marker, performs same-account reconciliation, and cannot issue another POST.
6. Sign-out, offline startup, expired credentials, and a different-account login all preserve the marker and keep commerce disabled.

Record only sanitized business fields. Revoke capture sessions afterward. No live secret or raw HAR is committed.

### Repository gates

- `cargo test -p kobo-bomtoon`
- `cargo fmt --all --check`
- `cargo test --workspace --all-targets --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- browser simulator evidence
- runtime simulator evidence
- staged-path check proving no `evidences/` path is included

### Implementation evidence

Verified on 2026-08-31:

- `cargo fmt --all --check` exited zero.
- `cargo test --workspace --all-targets --all-features` passed 2,671 tests across 55 suites, with 2 ignored and no failures.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` exited zero.
- Focused commerce, managed-account, protocol, catalog, runtime-compatibility, and BOMTOON package tests all passed before the workspace gates.
- The browser simulator rendered the Clara BW Featured surface, switched among public destinations, and exercised an offline title open. The offline surface displayed `Join Wi-Fi` guidance and a retry action; no live mutation was submitted.
- The runtime simulator connected the BOMTOON application, rendered the signed-out `Sign in` surface, wrote a frame, and exited zero. Its one-frame runner did not provide interactive runtime navigation.
- Existing revoked attended captures establish Gift delta `-1` with refreshed `RENT`, Coin rental delta `-2`, and Coin purchase delta `-3`. They do not establish refreshed paid `RENT`/`POSSESSION` or the post-implementation restart/failure scenarios below.
- `git ls-files evidences` and the staged evidence-path checks produced no output.

Still pending in non-spending simulators:

- interactive browser quote, cancel, and disabled-action scenarios;
- runtime action, Back, progress, and store-callback scenarios;
- interruption, restart, no-second-POST, and account-isolation scenarios.

Still operator-attended and pending:

- fresh paid `RENT` and `POSSESSION` refresh checks;
- one accepted live refresh-failure check proving no repost.

## Documentation and clean cutover

- Keep `CONTEXT.md` as the canonical wallet and episode-access glossary.
- Correct the earlier wallet design so Ticket balances/history remain Account-only and no episode directly consumes a Ticket.
- Remove obsolete episode Ticket fields, parsing, rendering, tests, and comments.
- Reuse the existing wallet-summary refresh seam after Coin mutations.
- Add no compatibility aliases or deprecated Ticket-commerce path.

## Implementation scope

Expected app-local files:

- `apps/bomtoon/src/api.rs`
- `apps/bomtoon/src/commerce.rs` — new
- `apps/bomtoon/src/model.rs`
- `apps/bomtoon/src/parse.rs`
- `apps/bomtoon/src/main.rs`
- focused existing test modules beside those files
- `CONTEXT.md`
- this specification and the corrected wallet specification
- `.gitignore`

Expected narrow account-scope integration files:

- BOMTOON session bootstrap and scope hashing in `kobo-net`;
- managed token metadata, durable scope-key storage, and scope resolution in `kobo-policy`;
- typed `CredentialScope` task encoding in `kobo-protocol` and execution in the task runner;
- focused provider, protocol, and managed-credential tests.
- exact BOMTOON credential method/URL policy in `kobo-net`;
- focused allowlist tests for every accepted and denied route shape.
- protocol version-history and compatibility tests;
- BOMTOON catalog version `0.5.0` and minimum Cobalt `0.4.0`.
- workspace package version `0.4.0`, internal path-version constraints, and regenerated `Cargo.lock`;
- runtime app-store compatibility tests proving the built runtime accepts the BOMTOON minimum.

No dependency, workspace member, capability, origin, or device-policy change is planned. The coordinated Cobalt `0.4.0` release, protocol v13, BOMTOON catalog compatibility bump, policy/runtime, network-provider, SDK-visible task, and exact credential-allowlist changes are limited to account scoping and the approved commerce routes.
