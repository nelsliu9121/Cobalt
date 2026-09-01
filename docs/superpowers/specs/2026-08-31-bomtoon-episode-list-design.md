# BOMTOON episode list redesign

## Status

The bearer detail correction is complete. Authenticated title loading and post-purchase reconciliation use the bounded content JSON endpoint, require `result == "SUCCESS"` and exact alias identity, and retain the existing metadata and episode access limits. Focused parser/API tests, the full Bomtoon suite, formatting, Clippy, authenticated browser simulation across multiple titles, stable episode navigation, and clean layout diagnostics pass. The browser proof was non-spending; no episode, quote, purchase, Coin, or Gift action was used. The runtime simulator remains the host build, SDK IPC, daemon, device-result, and frame-render gate because it has no post-start action channel or browser credential reuse.

## Goal

Redesign the signed-in comic episode view so it presents the comic's title, creators, and synopsis, then a paginated rich-row episode list. Each episode row shows its thumbnail, title, published date, and truthful readable-access status. Possession state comes from the bearer-authenticated title content response.

## Requirements

- Load the selected comic from `https://www.bomtoon.tw/api/balcony-api-v2/contents/{alias}?isNotLoginAdult=false&isPorch=false` with the managed BOMTOON access token.
- Require the JSON envelope's `result == "SUCCESS"`, require `data.alias` to equal the requested alias, and parse only allowlisted fields from `data`.
- Show the comic title, ordered creator names, and synopsis preview above the episode list on page 1.
- On pages after page 1, show only the comic title above the balances and episode rows.
- Show a `More` action on page 1 only, and only when the synopsis is longer than its preview.
- Open the full synopsis in a modal over the unchanged episode page.
- Paginate a long synopsis inside the modal and preserve all text.
- Render each episode as a rich row with a thumbnail lead, title, Taipei `YYYY-MM-DD` published date, and optional trailing access label.
- Use `purchaseStatus` and the existing access rules as the authority for possession, rental, free, and sample state.
- Show `Owned` for permanent possession, ceiling-rounded rental time for active rentals, `Free` for free episodes, and `Sample` for samples or previews.
- Leave the trailing status empty for an unowned episode. Tapping it continues to open the existing purchase options.
- Preserve existing title Gift and Coin balance presentation, purchase flow, reader flow, and episode navigation semantics.
- Show Gift failure, purchase rejection, and unresolved cross-account warnings in dismissible modals rather than reserving episode-list space.
- A Gift failure modal offers Retry and Close. After Close, tapping the episode re-enters the existing quote and Gift flow.
- Keep the pager at the bottom with its existing minimum touch-target height, but place its band flush with the panel edge so it does not also reserve the bottom screen margin.
- Fetch thumbnails only for the visible episode page and release or cancel episode thumbnail resources when they are no longer relevant.
- Fall back to the existing book glyph when a thumbnail is absent, unsafe, loading, or failed.
- Keep every screen and modal page within `CLARA_BW_METRICS`.

## Non-goals

- Changing purchase, quote, Gift, wallet, or reader semantics.
- Showing `Gifted` as an acquisition-origin label. The observed fields identify readable state and gift eligibility, not whether readable access originated from a gift.
- A tile grid, large episode cards, or a new `kobo-ui` control.
- Background thumbnail prefetch beyond the visible episode page.
- Persisting comic metadata or thumbnails across app launches.
- Parsing, storing, exposing, or logging account fields, access tokens, session cookies, or credentials.
- Changing app capabilities, network origins, workspace dependencies, protocol types, or Store metadata.

## Evidence

The design uses local captures as schema evidence. Raw HARs can contain account and credential material; implementation and tests must use only allowlisted business fields and sanitized synthetic fixtures.

The original implementation used `evidences/detail-365-html.har`. That historical response contained `script#__NEXT_DATA__`, `props.pageProps.ssrPersonalized == true`, and a personalized `ssrDetail`. The current authenticated route no longer preserves that contract: a live non-spending simulator trace returned `ssrPersonalized == false` while the response still contained `userData`, one episode, and a `purchaseStatus` field. The strict parser therefore produced the reported error for every title.

A live non-spending probe of the existing bearer endpoint returned:

- root keys `result` and `data`;
- `result == "SUCCESS"`;
- allowlisted `data` fields including `id`, `alias`, `title`, `synopsis`, `creators`, and `episodes`;
- nine episodes for the selected title, with `purchaseStatus` present on all nine.

The probe logged only field names, counts, and the public result code. It did not log response values, account data, headers, cookies, or credentials. Raw authenticated responses remain local evidence and must not enter fixtures or source control.

The bearer endpoint is the new trust boundary because the runtime injects the managed access token and the server returns an explicit success envelope. The parser remains fail-closed on a missing or non-success result, an alias mismatch, malformed or oversized fields, and unknown access states. It must not fall back to public HTML or infer ownership from missing data.

## Screen design

The episode screen uses existing `ScreenBuilder` primitives.

```text
+------------------------------------------------+
| < Back                      Episodes           |
+------------------------------------------------+
| Comic title                                    |
| Creator A | Creator B                          |
| Synopsis preview, clamped to leave room for    |
| episode rows.                                  |
| [More]                                         |
| Coins 10 | Gifts 2                             |
+------------------------------------------------+
| [thumb] Episode 12                    Owned    |
|         2026-08-31                             |
| [thumb] Episode 11                    23 hrs   |
|         2026-08-24                             |
| [thumb] Episode 10                    Free     |
|         2026-08-17                             |
| [thumb] Episode 09                             |
|         2026-08-10                             |
+------------------------------------------------+
| < Previous                  1 / 8       Next > |
+------------------------------------------------+
```

The top bar says `Episodes`; the comic title appears once in the content header. On page 1, the content header also shows creators, the synopsis preview, and conditional `More`. On later pages, the content header contains only the comic title. Gift failure, purchase rejection, and unresolved cross-account warnings render as modal overlays and consume no episode-list space.

The implementation measures page 1 and later pages separately using the existing layout-issue mechanism. It selects the largest episode-row capacity, up to the existing six-row ceiling, that fits each header shape, balance line, rows, and pager. Pagination stores explicit episode ranges because page 1 and later pages can have different capacities. Navigation, page count, visible-thumbnail scheduling, and refresh anchoring use those same ranges so every episode appears exactly once without gaps or duplicates.

Bomtoon opts into an edge-aligned page-position band. The band retains `DisplayMetrics::page_position_band()` height and the existing Previous/Next hit targets, but its bottom edge is the panel bottom instead of the top of `screen_margin()`. Content ends at the top of that band. Other applications retain the existing inset pager.

Creator names retain remote order and are joined with ` | `. Empty creator names are rejected at the parser boundary. The page-1 synopsis preview preserves UTF-8 boundaries and indicates omitted text. `More` is absent when the full synopsis fits in the preview allocation and from every later page.

## Synopsis modal

`More` sets app-local modal state and rebuilds the current episode screen with a `ScreenBuilder::modal` overlay. The episode page, selection, and pagination state remain underneath it.

The full synopsis is split into UTF-8-safe modal pages. Pagination prefers paragraph or whitespace boundaries and preserves every source character. Candidate modal pages are measured against `CLARA_BW_METRICS`; a page is accepted only when its text plus controls fits.

Modal controls are deterministic:

- page 1 omits Previous;
- the final page omits Next;
- Close is always present;
- Close clears only synopsis-modal state and returns to the same episode page.

No synopsis modal action starts network work or changes commerce or reader state.

## Transient warning modals

Episode warnings share the existing modal overlay mechanism and never alter stored episode ranges. Modal priority is unresolved cross-account warning, then purchase rejection, then Gift failure. Page turns are suppressed while a warning modal is open.

- Closing the cross-account warning dismisses its presentation until the marker identity or selected comic changes. The unresolved marker remains authoritative and affected episode actions remain disabled.
- Closing a purchase-rejection warning clears only that presentation notice. Commerce reconciliation state is unchanged.
- A Gift failure modal offers `Retry` and `Close`. Retry invokes the existing title-Gift retry action. Close dismisses the warning without claiming the Gift state loaded. A later tap on the episode enters the existing quote/Gift flow and may present a new failure modal if loading fails again.

No warning modal changes balances, ownership, account scope, purchase markers, or episode ranges.

## Data model

`ContentDetail` gains:

- ordered creator names;
- the full synopsis.

`Episode` gains:

- the `openedAt` timestamp in milliseconds;
- an optional validated `COMMON` thumbnail URL.

Existing identity, pricing, rental expiry, Gift eligibility, and `PurchaseState` fields remain authoritative for commerce and reader decisions.

Remote collections and strings remain explicitly bounded. The implementation plan must assign limits consistent with current title, creator, episode, URL, and response ceilings. Parsing must reject wrong types, invalid aliases, invalid timestamps, duplicate or ambiguous required thumbnail fields, and oversized required data.

## Request and parsing flow

Opening a comic keeps the existing foreground-task ownership and selected-alias state, but replaces the obsolete authenticated HTML detail request with the bearer-authenticated JSON content request.

The request contract is:

```text
GET https://www.bomtoon.tw/api/balcony-api-v2/contents/{validated alias}?isNotLoginAdult=false&isPorch=false
Credential: managed bearer `bomtoon-access-token`
Maximum response: 512 KiB
Accept: application/json
Accept-Language: existing zh-TW preference
x-balcony-id: BOMTOON_TW
x-balcony-timezone: Asia/Taipei
x-platform: MOBILE_IOS
```

The detail parser:

1. validates the expected alias before parsing;
2. decodes and parses the bounded JSON response;
3. requires root `result == "SUCCESS"`;
4. requires `data.alias` to equal the expected alias;
5. parses only the allowlisted comic and episode fields from `data`;
6. rejects missing, malformed, oversized, ambiguous, or hostile values through the existing typed parser boundary.

The authenticated HTML detail constructor and `__NEXT_DATA__` parser are removed after every caller and test migrates. Initial detail loading and post-purchase reconciliation use the same bearer JSON request and parser. No compatibility parser, public fallback, cookie request, or `ssrPersonalized` exception remains.

## Access status mapping

The existing closed access model remains the source for actions. Presentation maps it as follows:

```text
PurchaseState::Owned       -> Owned
PurchaseState::Rented      -> current ceiling-rounded rental hours
PurchaseState::Free        -> Free
PurchaseState::Sample      -> Sample
PurchaseState::NotOwned    -> empty trailing label
PurchaseState::Other(code) -> existing bounded remote-status label
```

A rented gift appears as rented access because the capture does not prove acquisition origin. A possession gift appears as owned access for the same reason. `isRentGift` remains Gift eligibility and must not be relabeled as acquisition history.

## Thumbnail ownership

The existing public-cover cache lifecycle is extended to own both shelf covers and episode thumbnails rather than introducing a second downloader.

- Safe episode URLs must use the observed BOMTOON image host and `/tw/ep_thumbnail/` path family.
- The parser selects only thumbnail type `COMMON`.
- Visible URL calculation includes the current episode page when `View::Episodes` is active.
- Entering the episode view preempts irrelevant shelf-cover fetches.
- Changing episode pages updates the visible working set before scheduling new work.
- A stale completion cannot install a picture for an old page, title, account generation, or signed-out state.
- Leaving Episodes, signing out, losing the credential, suspension, and app exit cancel pending episode thumbnail work and drop pictures no longer retained by a visible public shelf.
- Decode, upload, URL, or fetch failure changes only that row to the book-glyph fallback.

The SDK-wide task cap remains authoritative. Foreground detail or commerce work must not be starved by thumbnail tasks.

## Failure behavior

- Missing or non-success result, alias mismatch, malformed JSON, wrong required types, invalid dates, and oversized required values fail the foreground detail load and use the existing problem/retry surface.
- Unknown purchase status remains explicit through `PurchaseState::Other`; it is never converted to unowned access.
- Missing or unsafe thumbnail data degrades to the book glyph without failing otherwise valid detail.
- A failed thumbnail can retry only through the existing visible-image scheduling behavior; it does not create a page-level retry action.
- A failed post-purchase detail refresh preserves the existing accepted-but-stale commerce safety behavior. The redesign must not weaken unresolved-mutation handling.
- Synopsis modal construction is local and deterministic. If no measured page can fit one valid text unit, the modal shows the smallest valid unit plus its navigation controls rather than dropping text.

## State transitions

```text
Main shelf
  -> open comic
  -> bearer-authenticated detail JSON pending
  -> content detail parsed
  -> Episodes page 1

Episodes
  -> More
  -> Synopsis modal page 1
  -> modal Previous/Next
  -> modal Close
  -> same Episodes page

Episodes
  -> episode page Previous/Next
  -> page 1 keeps title, creators, and synopsis; page 2+ keeps the title only

Episodes unowned row
  -> existing quote and purchase flow
  -> existing reconciliation
  -> bearer-authenticated detail JSON refresh
  -> same episode page with refreshed access label
```

All stale-result identity and generation checks continue to bind results to the selected title and account.

## Files

The regression correction is limited to four existing files:

1. `apps/bomtoon/src/api.rs`
2. `apps/bomtoon/src/parse.rs`
3. `apps/bomtoon/src/main.rs`
4. this design specification

No new source module, dependency, capability, origin, credential, protocol type, Store metadata, or SDK change is required. The existing BOMTOON bearer policy already permits the exact content API origin and path.

## Verification

### API and parser contracts

- Exact authenticated content URL, managed bearer credential, JSON acceptance, existing balcony headers, and 512 KiB ceiling.
- Minimal sanitized JSON fixture matching the observed `result/data` envelope.
- Successful response parses title, ordered creators, synopsis, episode identity, title, `openedAt`, `COMMON` thumbnail, and `purchaseStatus`.
- Missing or non-success result, wrong alias, wrong types, invalid timestamp, oversized fields, duplicate ambiguous thumbnail, hostile image host/path, and unknown purchase status remain fail-closed.
- HTML and `__NEXT_DATA__` input are rejected; tests contain no copied account data, email address, access token, cookie, or other credential material.

### Screen and behavior contracts

- Page 1 shows title, creators, synopsis preview, and conditional `More`.
- Pages after page 1 show only the comic title above balances and episode rows.
- Existing balances remain visible; Gift failure, purchase rejection, and cross-account warnings appear in the specified modal priority.
- Rows show thumbnail or fallback, title, exact Taipei date, and the approved optional status.
- Unowned rows have empty trailing status but remain actionable unless the unresolved cross-account marker disables them.
- Owned, rented, free, and sample rows retain existing reader behavior.
- Modal pagination preserves the complete synopsis, fits every page, and closes to the same episode page.
- Episode pagination uses separately measured first-page and later-page capacities, explicit non-overlapping ranges, and stops at the final page.
- Bomtoon uses the opt-in edge pager; its touch targets retain the shared minimum height while the band sits flush with the panel bottom.
- Only visible episode thumbnails request work; stale completions do not install; leaving the view cancels and drops resources.
- Post-purchase reconciliation updates the row through the bearer JSON parser while preserving commerce safety state.

### Gates and smoke checks

Run:

```sh
cargo test -p kobo-bomtoon
cargo test -p kobo-policy
cargo fmt --all -- --check
cargo clippy -p kobo-bomtoon --all-targets --all-features -- -D warnings
cargo clippy -p kobo-policy --all-targets --all-features -- -D warnings
```

Exercise the changed flow in the browser simulator:

- open a comic with a long synopsis;
- open, page, and close the synopsis modal;
- page through episodes;
- observe loaded and fallback thumbnails;
- open an owned episode;
- open purchase options for an unowned episode without spending;
- confirm the selected comic and episode page remain stable after each return.

Run `cargo run -p kobo-cli -- run --sim --app bomtoon` as the runtime build, IPC, daemon, device-result, and frame-render gate. This runtime simulator is deliberately one-shot: it has no post-start action channel and starts with a fresh credential root, so it cannot repeat the authenticated interactive browser flow.

Simulator evidence must use `CLARA_BW_METRICS`. No automated or unattended smoke check may spend Coin or consume a Gift.
