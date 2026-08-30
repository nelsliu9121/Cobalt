# BOMTOON public Featured shelf and bottom navigation

## Status

Approved design. Implementation has not started.

This specification records the completed grilling session and incorporates the advisor requirement that daily refresh use an actual device-local calendar day. The fixed UTC+8 `taipei_day` helper in `apps/bomtoon/src/main.rs` is not valid for this feature.

## Goal

Turn the BOMTOON app's main surface into a cover-led comic shelf with a fixed bottom strip containing three destinations in this order:

1. Featured
2. Recent
3. Library

Featured is the default and remains usable without a BOMTOON credential. Recent, Library, Account, comic details, episodes, and reading retain their authenticated behavior. A public homepage or cover failure must never block access to already available protected content.

## Confirmed product decisions

- Use the SDK's fixed-bottom `nav_bar` as the three-destination tab bar. Do not use the in-flow `tabs()` control.
- Keep the comic shelf horizontally centered in the main region between the top bar and bottom bar. Normal content flows from the top; the complete shelf block is not vertically centered.
- Open Featured by default.
- Show the public Featured feed while signed out.
- Show a top-bar `Sign in` action while signed out.
- A signed-out tap on a Featured comic, Recent, or Library opens the existing sign-in instruction screen.
- Successful sign-in returns to Featured and does not resume the prior comic or protected destination.
- While signed in, every main destination shows the coin balance as a tappable top-bar action. That action opens Account. There is no separate Account entry on the main shelf.
- Move Sign out inside Account.
- Successful sign-out clears protected state, returns to Featured, and retains the public feed and its session cover cache.
- Page-position restoration is deferred. Destination switches, Account returns, comic returns, and post-login returns start the destination at page 1. Remove the current Recent/Library page-restoration behavior in this clean cutover.

## Scope

### Featured

The public homepage payload supplies several editorial lists but no fields literally named Featured or Recommended. Map them as follows:

- Featured: the first three banner entries, in source order, whose link target is a comic.
- Recommended: concatenate `newest`, `weekDay`, and `onlyBom` in that order.
- Deduplicate Recommended by exact comic alias. The first occurrence wins.
- Preserve a comic's separate placement when it appears in both Featured and Recommended.
- Always use the public/non-adult artwork variant, whether signed out or signed in.

Featured uses comic cover artwork, not the wide promotional banner image. Resolve banner aliases against the three homepage comic lists first. If one of the selected banner comics is absent from those lists, obtain its public title and cover metadata from its public detail page. Failure to enrich one banner leaves that tile usable with the validated alias and a glyph placeholder; it does not replace the whole feed with an error.

### Recent

Keep the current authenticated Recent data and selection behavior, but render six compact artwork-led rows per page. Each row contains:

- a cover or glyph placeholder;
- the comic title;
- the episode title as its subtitle.

### Library

Keep the current authenticated Library data and selection behavior, but render six compact artwork-led rows per page. Each row contains:

- a cover or glyph placeholder;
- the comic title;
- the owned/total episode count as its subtitle.

### Comic and reader behavior

A signed-in comic tap from any destination uses the existing authenticated content request and episode screen. Readable and free episodes remain tappable; locked episodes remain informational. This change adds no purchase, rental, QR, browser handoff, or public episode-detail path.

Back from Account or a comic returns to the originating main destination at page 1. The origin destination is retained; its page position is not.

## Non-goals

- Continuous pixel scrolling or a new scrolling container.
- Page-position restoration.
- Persistent homepage or image caching.
- Personalized recommendations. The site has no personalized feed for these lists.
- Adult artwork after sign-in.
- Commerce or purchase handoff.
- Cover-led redesign of episode rows.
- A new shared bottom-tab primitive. `nav_bar` already provides the required pinned destination bar.
- Reusing Taipei time as device-local time.
- Guessing UTC when the device's timezone cannot be read.

## Live homepage evidence

The public Taiwan homepage was inspected on 2026-08-30:

- `GET https://www.bomtoon.tw/comic/main`
- Next.js build: `OwHQUV9KN_XCsgOkqJPqz`
- Public `pageProps.main` fields:
  - `banners`: 27 entries;
  - `newest`: 30 comics;
  - `weekDay`: 30 comics;
  - `onlyBom`: 30 comics.

The inspected payload was approximately 127 KiB. Homepage entries carry aliases, titles, maturity flags, and one or more thumbnail variants. Banner entries carry link targets and aliases but do not reliably carry a comic title or portrait cover. At least one of the current first three comic-target banners is absent from the three comic lists, so banner enrichment needs the bounded public-detail fallback described above.

These are undocumented production contracts. Parsing must be bounded, strict at structural boundaries, and tolerant of an individual missing thumbnail. A future site deployment can change the build ID without changing the stable `/comic/main` route, so the app must not hard-code the observed build ID or a `_next/data` URL.

## Data model

Add one public comic representation shared by homepage and cover presentation:

```text
ShelfComic
  alias: validated bounded string
  title: validated bounded string
  cover_url: optional validated image URL
```

Protected models retain their existing fields and gain only the thumbnail URL required by the confirmed UI:

```text
Comic
  alias
  title
  owned_episodes
  total_episodes
  cover_url: optional

RecentEntry
  content_alias
  content_title
  episode_alias
  episode_title
  cover_url: optional
```

Featured state is independent from protected account state:

```text
FeaturedState
  status: Unloaded | Loading | Ready | Failed
  featured: up to 3 ShelfComic values
  recommended: ordered deduplicated ShelfComic values
  loaded_local_day: optional device-local day key
  stale_warning: optional bounded message
  page: zero-based page index
```

The session image cache is keyed by validated URL, not by section position or alias, so the same artwork downloaded for multiple placements is decoded once. Public feed state and public pictures survive sign-out. Protected Recent, Library, Account, episode, and reader state do not.

## Public homepage parsing

Fetch `/comic/main` without a credential and with a fixed response ceiling. Extract only the exact `script#__NEXT_DATA__` JSON payload from the bounded HTML response. Require the expected `props.pageProps.main` object and bounded arrays.

Parser rules:

- Cap `banners`, `newest`, `weekDay`, and `onlyBom` at their explicit implementation limits before allocation.
- Validate aliases as the same safe path-segment form accepted by existing BOMTOON content URLs.
- Bound every retained title and URL before allocation.
- Accept only HTTPS image URLs on approved BOMTOON image hosts and paths.
- Prefer the public/non-adult thumbnail variant appropriate to the surface.
- Missing artwork yields `None`; it does not discard an otherwise valid comic.
- A malformed required alias or title discards that individual list entry.
- A missing or structurally invalid required homepage section fails the initial Featured metadata load as a unit. A daily refresh failure preserves the old ready feed.
- Recommended deduplication uses the exact validated alias and retains the first occurrence in `newest → weekDay → onlyBom` order.
- Featured filtering accepts only banner targets that explicitly identify comics; event, shop, gift, pick, and empty targets never consume one of the three slots.

For a selected banner alias absent from the homepage comic lists, start a bounded, unauthenticated public-detail metadata request. Parse only the public title and cover metadata. Do not parse or expose public episode rows. At most three banner-detail lookups can exist for one homepage generation.

## Network and credential policy

Public tasks use no credential:

- homepage HTML;
- public comic-detail metadata used only to enrich a selected banner;
- cover images.

Authenticated Balcony tasks keep the existing `bomtoon-access-token` policy:

- Recent;
- Library;
- Account and wallet;
- comic content and episodes;
- reader manifests and pages.

The network policy must explicitly allow the exact public homepage, public detail metadata, and approved image-host paths without allowing credentials on them. Existing tests that deny bearer credentials to detail HTML and Next.js data remain authoritative. No credential enters a public URL, public header, parser error, cache key, or log.

## Main navigation and screen states

The fixed bottom `nav_bar` always contains exactly:

```text
Featured | Recent | Library
```

The active destination is selected. The bar remains actionable while the current destination's metadata or covers are loading or failed. Main-destination errors never become a global blocking screen.

### Featured page 1

Page 1 contains:

1. `Featured` section label;
2. one horizontally centered row of up to three portrait cover tiles;
3. `Recommended` section label;
4. up to six compact Recommended cover rows;
5. page position when more Recommended rows exist;
6. the fixed bottom destination bar.

Clara BW layout safety has precedence over the requested first-fold density. If three Featured tiles plus six Recommended rows do not fit with valid tap targets, page 1 shows the largest fixed count that passes layout diagnostics. Continuation pages still show six Recommended rows.

### Featured continuation pages

Continuation pages contain only the `Recommended` label and the next six Recommended rows. Featured tiles do not repeat.

### Recent and Library

Each page contains up to six compact cover rows, a page position when needed, and the fixed bottom destination bar. Remote pagination retains the current behavior: fetch the next bounded server page only when the user advances beyond locally loaded items.

### Page controls

Use the SDK page-turn gesture/control and page-position indicator. Do not render Previous/Next buttons. Page calculations are deterministic and checked. A destination selection resets its page to zero under the confirmed no-restoration rule.

## Authentication and Account flow

Startup begins the public homepage request and the existing lightweight account/wallet probe independently. The public shelf does not wait for the probe.

- `NoCredential`: show `Sign in`; retain public state.
- `Unauthorized`: show the existing expired-sign-in guidance; retain public state.
- authenticated summary loading: show a tappable `Coins…` action.
- authenticated summary ready: show a tappable `Coins N` action.
- authenticated summary unavailable: show a tappable `Coins unavailable` action; Account owns retry details.

Recent and Library lazy-load on their first authenticated selection. Selecting either while signed out opens the sign-in instructions and does not start its protected fetch. Successful external login plus Retry returns to Featured page 1.

Account records only its origin destination. Back returns to that destination at page 1. Sign out remains an explicit Account action. Successful sign-out cancels and clears protected work, invalidates protected outcomes, preserves public Featured state and public pictures, and selects Featured page 1.

## Progressive cover loading

Every shelf is usable before artwork arrives:

1. Render titles and metadata immediately with `Glyph::Book` placeholders.
2. Request only artwork visible on the current page.
3. Bound concurrent public image/detail work to available SDK task capacity.
4. Install each successful picture without changing shelf order or geometry.
5. Retain completed pictures for the current app session.
6. On a destination or page change, reprioritize visible work; invisible work may be cancelled when task capacity is needed.
7. An individual image failure leaves its glyph placeholder and does not show a destination-level error.

No artwork is persisted to device storage. A new app process fetches metadata and visible covers again.

## Device-local daily refresh

### Problem

The existing `taipei_day(timestamp_ms)` in `apps/bomtoon/src/main.rs` deliberately applies a fixed UTC+8 offset for BOMTOON wallet-date presentation. It cannot implement the confirmed device-local homepage refresh. Reusing it would silently refresh according to Taipei midnight when the Kobo is configured for another timezone.

`crates/kobod/src/device.rs` has a private `local_offset_seconds()` for the daemon clock. It reads the firmware's `TZ` environment variable, but it is not available to applications and currently falls back to UTC for missing or malformed input. Copying that helper into BOMTOON would create a second timezone convention and would turn invalid configuration into a plausible but wrong day.

### Crate choice

Use `jiff` 0.2 for timezone and civil-date manipulation. Jiff 0.2.35 has an MSRV of Rust 1.70, below this workspace's Rust 1.85.1 requirement. Its system-timezone implementation:

- reads `TZ` when present;
- accepts IANA identifiers, TZif paths, and POSIX timezone rules;
- otherwise detects Unix `/etc/localtime`;
- applies historical and daylight-saving transitions when converting an instant;
- exposes `TimeZone::try_system()` so unavailable timezone data is an error rather than an implicit UTC value.

Use Jiff with the minimal Unix-capable features needed for `std`, system-timezone discovery, and the system zoneinfo database. Do not bundle the complete timezone database into the Kobo binary unless target verification proves the firmware lacks both a usable `TZ` value and TZif data. The firmware's POSIX `TZ` value is sufficient without a bundled database.

Jiff is preferable here to:

- a copied fixed-offset parser, which would duplicate daemon policy and mishandle IANA zones or daylight-saving rules;
- `chrono::Local`, which can obtain local time but provides a less explicit fallible system-timezone boundary for this SDK contract;
- `time`'s `local-offset` feature, which exposes an offset rather than the timezone and transition model needed for deterministic boundary tests.

The SDK must not expose Jiff types as application API. Jiff remains the implementation detail behind a small copyable SDK value:

```text
LocalDay
  year
  month
  day

device_local_day() -> Option<LocalDay>
```

`LocalDay` supports equality and ordering but no timezone arithmetic. The SDK converts `Timestamp::now()` through `TimeZone::try_system()` and copies the resulting civil year, month, and day. A private pure helper accepts a timestamp and `TimeZone` for deterministic tests.

If system-timezone discovery fails or the clock is invalid, `device_local_day()` returns `None`. It must not default to Taipei, UTC, a compiled zone, or a last-known offset. The existing wallet-specific `taipei_day` remains for wallet semantics and is not renamed or reused.

### Refresh state machine

Featured stores an optional observed `LocalDay` baseline independently from feed readiness.

- A successful initial Featured load records `device_local_day()` when it is available. An unavailable observation leaves the baseline unknown.
- On app resume and on entry to Featured, observe `device_local_day()` again.
- Current `None`: keep the feed and baseline unchanged. Do not infer a date.
- Baseline `None` plus current `Some(day)`: record `Some(day)` as the baseline without refreshing. This is the first trustworthy observation, not evidence that a day was crossed.
- Baseline `Some(day)` plus the same current day: do nothing.
- Baseline `Some(old)` plus current `Some(new)` where they differ: start exactly one Featured metadata refresh targeted at `new`.
- Repeated observations for the active target coalesce.
- If a different trustworthy day is observed while a refresh is active, retain it as the desired day and re-evaluate once the active request settles. Do not lose a timezone or midnight transition.
- A successful refresh atomically replaces Featured and Recommended, records its target day unless a newer desired day exists, clears stale warnings, resets Featured to page 1, and reprioritizes visible covers.
- A failed refresh keeps the previous feed and completed pictures, leaves the last successful baseline unchanged, records a nonblocking stale warning, and exposes Retry.
- Retry uses the same refresh operation. It does not create a second refresh path.
- A system clock moving backward or a timezone change can produce a different trustworthy `LocalDay` and therefore one refresh. This is intentional: the device's local calendar context changed.

This baseline transition is required: if initial loading sees no timezone but a later observation succeeds, recording that first `Some(day)` without a request enables subsequent day changes to refresh. Leaving the baseline as `None` would disable automatic refresh forever.

### Required boundary tests

The SDK local-calendar facility must be implemented and verified before Featured daily refresh is wired. Tests use injected Jiff timestamps and timezones and cover:

- the instant immediately before and exactly at local midnight;
- positive, negative, and non-hour fixed offsets;
- a POSIX timezone rule with spring-forward and fall-back transitions;
- an IANA/TZif timezone when the test environment supplies the database;
- two timezones mapping one instant to different civil dates;
- unavailable system-timezone discovery returning `None`, never UTC;
- invalid or out-of-range system time returning `None`;
- dates around the Unix epoch without unchecked arithmetic.

BOMTOON tests separately prove:

- same-day observations do not refresh;
- `None → Some(day)` establishes the baseline without refreshing;
- a later differing `Some(day)` refreshes exactly once;
- current `None` never replaces a known baseline;
- a newer day observed during an active refresh is not lost;
- unknown local day never substitutes Taipei or UTC;
- refresh failure retains stale content and the last successful baseline with a warning.

## Loading, failure, and retry behavior

- Initial Featured metadata loading is local to Featured. Bottom navigation and the top bar remain usable.
- Initial Featured metadata failure shows a local explanation and Retry while Recent and Library remain selectable.
- A daily refresh leaves the old shelf visible. Failure adds `Could not refresh · Showing previous feed` and Retry.
- An empty successful Featured list is an empty state, not a network error.
- Missing Recommended entries do not remove valid Featured entries, and vice versa after the structural envelope has passed.
- Recent and Library loading/failure states remain independent from one another and from Featured.
- Switching destinations does not cancel metadata already requested for that destination; its outcome populates its cache without changing the selected destination.
- Stale task outcomes are rejected by generation before mutating state or the visible screen.

## Task ownership and bounds

Public homepage, banner enrichment, protected shelves, wallet work, and pictures require explicit task purposes rather than overloading the existing single `Pending` enum with image state.

Every task records its generation and purpose. Outcomes remove their exact registry entry first and mutate state only when still current. Unknown, cancelled, and prior-generation outcomes are no-ops.

Maintain the SDK-wide task limit. Main-screen scheduling prioritizes:

1. selected destination metadata;
2. authentication/account probe needed for top-bar state;
3. selected page banner enrichment;
4. selected page covers;
5. non-visible work only when capacity remains.

Reader entry cancels or drains main-shelf picture work before reader maintenance takes task capacity. Account detail refresh owns its existing bounded task policy and may pause cover work.

## Layout and interaction verification

Add Clara BW layout checks for:

- signed-out Featured loading, ready, empty, failed, and stale-warning states;
- signed-in Featured with loaded, loading, and unavailable coin actions;
- Featured page 1 at its chosen safe row count;
- Featured continuation pages with six rows;
- Recent and Library pages with six rows;
- missing-cover placeholders;
- long Chinese titles and episode subtitles;
- the fixed bottom bar with each destination selected;
- page-position display above the bottom bar;
- Account and comic Back returning to the origin destination at page 1.

The bottom bar must remain wholly inside the panel and must not overlap page turns, page position, shelf rows, or tap targets.

## Behavioral and safety verification

Tests must defend these observable contracts:

- the first three comic-target banners are selected in source order while non-comic banners are skipped;
- Recommended concatenation and first-occurrence deduplication are stable;
- duplicates across Featured and Recommended remain;
- only public/non-adult artwork variants are selected;
- unresolved banner metadata cannot fail the whole feed;
- signed-out public browsing works without spawning protected fetches;
- protected destination and comic taps show sign-in while signed out;
- successful sign-in returns to Featured page 1 without resuming the target;
- coin action opens Account and no separate Account action appears;
- sign-out retains public feed/pictures and clears protected data;
- page turns expose all items in six-row continuation pages;
- destination changes and Back reset page position under the deferred-restoration rule;
- cover failures retain usable placeholder rows;
- stale and cancelled outcomes cannot change another destination;
- public requests carry no credential and authenticated requests retain their exact credential policy;
- local-day boundary and refresh tests listed above pass.

Exercise the final UI in the browser simulator and runtime simulator. Record Clara BW layout evidence and an interactive pass covering bottom navigation, signed-out Featured, sign-in gating, page turns, cover progression, protected lazy loading, Account, and sign-out.

## Implementation order constraint

The implementation plan must place the central SDK local-calendar facility and its boundary tests before BOMTOON daily-refresh wiring. No BOMTOON refresh code may call `taipei_day`, copy the daemon's private timezone parser, or introduce a fallback timezone.

After that prerequisite, implement the homepage parser/model, public network policy, navigation/state cutover, shelf layouts, cover scheduler, authentication transitions, and refresh integration in dependency order.

## Expected file areas

The implementation plan is expected to touch only the responsible layers:

- the root dependency configuration and lockfile for `jiff` 0.2;
- `crates/kobo-sdk/src/` for the central safe `LocalDay` facility and tests;
- `apps/bomtoon/src/api.rs` for public homepage/detail/image tasks and protected thumbnail query handling;
- `apps/bomtoon/src/parse.rs` for bounded homepage and thumbnail parsing;
- `apps/bomtoon/src/model.rs` for shelf comic and thumbnail fields;
- `apps/bomtoon/src/main.rs` for destination state, bottom navigation, paging, progressive covers, auth transitions, and daily refresh;
- `crates/kobo-net/` and policy tests for exact public no-credential routes and image paths;
- focused BOMTOON and SDK tests.

`jiff` is the only planned new dependency. A protocol message, persistent store, shared scrolling primitive, bundled timezone database, or runtime service is not planned.