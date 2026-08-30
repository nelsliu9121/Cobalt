# BOMTOON Featured Shelves Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make BOMTOON open to a public Featured shelf, add fixed Featured/Recent/Library bottom navigation, show progressive comic covers, and refresh public metadata once when the runtime reports a different device-local day.

**Architecture:** Keep public Featured state independent from protected account state. Add one runtime-owned `ReadLocalDay` device request because sandboxed apps cannot read `TZ` or `/etc/localtime`; `kobod` and the simulator produce a checked `LocalDay` with Jiff. Parse the stable public homepage HTML into bounded shelf models, enrich only unresolved selected banners through bounded public detail requests, and use one generation-checked URL cache for visible shelf covers. Replace the current Library/Recent pseudo-tabs with one main destination state and the SDK's fixed `nav_bar`.

**Tech Stack:** Rust 2021, `kobo-protocol`, `kobo-sdk`, `kobod`, `kobo-sim`, Jiff 0.2, `kobo-json`, `http`, `kobo-image`, `kobo-ui`, existing `AppRunner` unit-test harness, browser simulator, runtime simulator.

## Global Constraints

- The approved behavior is in `docs/superpowers/specs/2026-08-30-bomtoon-featured-shelves-design.md`; when this plan and the specification differ, fix the plan before changing code.
- App processes remain jailed. BOMTOON must not read `TZ`, `/etc/localtime`, zoneinfo, or Jiff directly. `Context::device().read_local_day()` is the only local-calendar boundary.
- The device producer reads the firmware `TZ` value on every request. Missing, malformed, or unusable timezone/clock input returns `None`; never fall back to UTC or Taipei.
- Keep `taipei_day` unchanged for wallet expiration semantics.
- Public homepage, public detail, and cover requests carry no credential. Existing bearer credentials remain limited to exact protected Balcony routes; detail HTML and Next data remain denied to bearer credentials.
- Featured content uses public/non-adult metadata. Recent and Library remain protected and lazy-load only after authentication and destination entry.
- Use `ScreenBuilder::nav_bar`, `TileShape::Portrait`, `TilePicture`, and `RowLead::Picture`; do not add a second navigation, tile, row, or scrolling abstraction.
- Page changes and destination changes cancel obsolete cover tasks. Cache completed pictures by validated URL for this process only. Never persist public metadata or artwork.
- Respect the runner's existing maximum of four concurrent tasks. A scheduler may stop when `Context::spawn` returns `None`; it must not create an independent capacity counter that can disagree with the runner.
- Every async operation carries a generation and purpose. Unknown task IDs and stale generations are no-ops.
- Preserve existing reader, wallet, login, logout, and credential-revocation behavior except where the approved main-navigation and sign-out transitions explicitly replace it.
- No page restoration for Featured, Recent, or Library. Destination changes and Back from Account/Episodes return the selected destination to page 1.
- Use `CLARA_BW_METRICS` layout assertions. Page 1 may show fewer than six Recommended rows if the real layout does not fit; continuation pages and protected shelves target six compact rows.

---

## File and Interface Map

| Area | Existing seam | Planned interface |
|---|---|---|
| Wire value | `crates/kobo-protocol/src/lib.rs` `DeviceRequest`, `DeviceResult`, encoder/decoder | `LocalDay::new`, `year/month/day`, `ReadLocalDay`, `DeviceResult::LocalDay` |
| SDK facade | `crates/kobo-sdk/src/lib.rs` `Device<'_>` and `KoboApp::on_device_result` | `Device::read_local_day()` |
| Runtime request policy | `crates/kobo-policy/src/services.rs` exhaustive device request handling | no-capability `ReadLocalDay`, generic `None` result |
| Device producer | `crates/kobod/src/device.rs` `local_offset_seconds`, device request dispatch | fresh `TZ` → Jiff → `Option<LocalDay>` |
| Non-device daemon | `crates/kobod/src/main.rs` simulated device request dispatch | host local-day result for daemon simulation |
| Browser simulator | `crates/kobo-sim/src/lib.rs` `handle_device_request` | explicit `TZ` or fallible system zone → `Option<LocalDay>` |
| Public transport | `apps/bomtoon/src/api.rs` existing `Task::Fetch` constructors | `homepage`, `public_detail`, existing `image` reuse |
| Public/protected models | `apps/bomtoon/src/model.rs` `Comic`, `RecentEntry` | `ShelfComic`, optional cover URLs |
| Parsers | `apps/bomtoon/src/parse.rs` bounded JSON helpers | `homepage`, `public_detail`, protected thumbnail extraction |
| Credential regression | `crates/kobo-net/src/lib.rs` BOMTOON policy tests | public HTML remains unusable with bearer credential |
| App state | `apps/bomtoon/src/main.rs` `View`, `Shelf`, `Bomtoon`, `Pending` | `MainDestination`, `FeaturedState`, task purposes, picture cache |
| Main UI | `library_screen`, shelf switches, page controls | `main_screen`, Featured tiles/rows, compact protected rows, fixed nav |
| Lifecycle | `on_start`, `on_resume`, `on_foreground`, `on_device_result` | coalesced local-day observations and refresh state machine |

---

### Task 1: Add the local-day protocol and SDK request

**Files:**
- Modify: `crates/kobo-protocol/src/lib.rs`
- Modify: `crates/kobo-sdk/src/lib.rs`
- Modify: `crates/kobo-policy/src/services.rs`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LocalDay {
    year: i16,
    month: u8,
    day: u8,
}

impl LocalDay {
    pub const fn new(year: i16, month: u8, day: u8) -> Option<Self>;
    pub const fn year(self) -> i16;
    pub const fn month(self) -> u8;
    pub const fn day(self) -> u8;
}

pub enum DeviceRequest {
    ReadLocalDay,
}

pub enum DeviceResult {
    LocalDay(Option<LocalDay>),
}

impl Device<'_> {
    pub fn read_local_day(&mut self);
}
```

- [ ] **Step 1: Add failing protocol value and round-trip tests**

Add tests beside the existing protocol round-trip tests. Require a checked constructor, impossible Gregorian-date rejection, and wire round trips for both result states.

```rust
#[test]
fn local_day_is_checked_and_ordered() {
    let august = LocalDay::new(2026, 8, 30).expect("valid local day");
    let september = LocalDay::new(2026, 9, 1).expect("valid local day");
    assert_eq!((august.year(), august.month(), august.day()), (2026, 8, 30));
    assert!(august < september);
    assert_eq!(LocalDay::new(2026, 0, 1), None);
    assert_eq!(LocalDay::new(2026, 13, 1), None);
    assert_eq!(LocalDay::new(2026, 8, 0), None);
    assert_eq!(LocalDay::new(2026, 8, 32), None);
    assert_eq!(LocalDay::new(2026, 2, 30), None);
    assert_eq!(LocalDay::new(2025, 2, 29), None);
    assert!(LocalDay::new(2024, 2, 29).is_some());
}

#[test]
fn local_day_request_and_results_round_trip() {
    assert_round_trip(Message::DeviceRequest(DeviceRequest::ReadLocalDay));
    assert_round_trip(Message::DeviceResult(DeviceResult::LocalDay(Some(
        LocalDay::new(2026, 8, 30).expect("valid local day"),
    ))));
    assert_round_trip(Message::DeviceResult(DeviceResult::LocalDay(None)));
}
```

- [ ] **Step 2: Run the protocol tests and observe RED**

Run: `cargo test -p kobo-protocol local_day -- --nocapture`

Expected: compile errors because `LocalDay`, `ReadLocalDay`, and `DeviceResult::LocalDay` do not exist.

- [ ] **Step 3: Implement the bounded wire value and variants**

Add `LocalDay` near the other small protocol values. Validate a complete Gregorian date with a small private `days_in_month` helper; do not add Jiff to `kobo-protocol`. Runtime Jiff conversion remains the authoritative producer, while the wire decoder independently refuses invalid values. Add request/result discriminants, encoded lengths, encoder branches, decoder branches, fixtures, exhaustive matches, and bump `kobo_protocol::VERSION` from 12 to 13. Extend `kobo-policy`'s exhaustive request handling with a generic `LocalDay(None)` response and no required capability; Task 2 supplies real host values.

- [ ] **Step 4: Add the failing SDK facade test**

Use the existing `AppRunner`/command test pattern.

```rust
#[test]
fn read_local_day_emits_one_device_request() {
    let mut app = AppRunner::new(Probe::default());
    let commands = app.start();
    assert_eq!(
        commands,
        vec![Command::Device(DeviceRequest::ReadLocalDay)]
    );
}
```

The local test `Probe::on_start` must call only `context.device().read_local_day()`.

- [ ] **Step 5: Implement `Device::read_local_day` and re-export `LocalDay`**

Follow `Device::read_cover`: enqueue `DeviceRequest::ReadLocalDay` through the existing `request` helper. Re-export `LocalDay` from `kobo_sdk` alongside the other protocol values so apps never import `kobo_protocol` directly.
- [ ] **Step 6: Run focused tests and observe GREEN**

Run:

```sh
cargo test -p kobo-protocol local_day -- --nocapture
cargo test -p kobo-sdk read_local_day -- --nocapture
```

Expected: all selected tests pass; no protocol fixture or exhaustive-match failures.

- [ ] **Step 7: Commit**

```sh
git add crates/kobo-protocol/src/lib.rs crates/kobo-sdk/src/lib.rs
git commit -m "feat(runtime): expose device local day"
```

---

### Task 2: Produce local days in every runtime host

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/kobod/Cargo.toml`
- Modify: `crates/kobo-sim/Cargo.toml`
- Modify: `crates/kobod/src/device.rs`
- Modify: `crates/kobod/src/main.rs`
- Modify: `crates/kobo-sim/src/lib.rs`

**Interfaces:**

```rust
fn local_day_at(timestamp: jiff::Timestamp, time_zone: &jiff::tz::TimeZone)
    -> Option<kobo_protocol::LocalDay>;

fn device_local_day() -> Option<kobo_protocol::LocalDay>;
fn simulator_local_day() -> Option<kobo_protocol::LocalDay>;
```

Device policy: read `std::env::var("TZ")` on every request and parse that explicit POSIX rule. Simulator policy: parse explicit `TZ`; only when absent, use fallible host system-zone discovery. Neither path substitutes UTC.

- [ ] **Step 1: Add failing pure conversion tests**

Add deterministic tests in `crates/kobod/src/device.rs` using parsed timestamps and explicit time zones.

```rust
#[test]
fn local_day_changes_exactly_at_local_midnight() {
    let zone = TimeZone::posix("TST-5:30").expect("fixed POSIX zone");
    let before: Timestamp = "2026-08-29T18:29:59Z".parse().expect("timestamp");
    let at: Timestamp = "2026-08-29T18:30:00Z".parse().expect("timestamp");
    assert_eq!(local_day_at(before, &zone), LocalDay::new(2026, 8, 29));
    assert_eq!(local_day_at(at, &zone), LocalDay::new(2026, 8, 30));
}

#[test]
fn posix_daylight_rules_map_instants_to_checked_days() {
    let zone = TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").expect("POSIX zone");
    let spring: Timestamp = "2026-03-08T07:00:00Z".parse().expect("timestamp");
    let autumn: Timestamp = "2026-11-01T06:00:00Z".parse().expect("timestamp");
    assert_eq!(local_day_at(spring, &zone), LocalDay::new(2026, 3, 8));
    assert_eq!(local_day_at(autumn, &zone), LocalDay::new(2026, 11, 1));
}
```

Also cover a negative offset, a non-hour offset, two zones producing different dates for one instant, an epoch-adjacent instant, missing/invalid `TZ`, and an invalid clock conversion.

- [ ] **Step 2: Add failing request-handler tests**

For `kobod`, the non-device daemon path, and `kobo-sim`, feed `DeviceRequest::ReadLocalDay` through the existing handler seam and assert `DeviceResult::LocalDay`. Add a source-injection test proving two later calls with different explicit `TZ` strings produce different dates; do not mutate process environment from parallel tests.

```rust
#[test]
fn later_request_uses_the_later_timezone() {
    let now: Timestamp = "2026-08-30T00:30:00Z".parse().expect("timestamp");
    let west = local_day_for(now, Some("HST10"), || None);
    let east = local_day_for(now, Some("TST-14"), || None);
    assert_ne!(west, east);
}
```

- [ ] **Step 3: Run focused tests and observe RED**

Run:

```sh
cargo test -p kobod local_day -- --nocapture
cargo test -p kobo-sim local_day -- --nocapture
```

Expected: missing Jiff dependency, producer, and request-handler branches.

- [ ] **Step 4: Add Jiff once at workspace level**

Add this workspace dependency and consume it from `kobod` and `kobo-sim`:

```toml
[workspace.dependencies]
jiff = { version = "0.2", default-features = false, features = ["std", "tz-system", "tzdb-zoneinfo"] }
```

Use `jiff.workspace = true` in both runtime manifests. Do not add Jiff to `kobo-sdk`, `kobo-protocol`, or `kobo-bomtoon`. Do not enable a bundled timezone database.

- [ ] **Step 5: Implement fresh runtime producers and handler branches**

The device implementation must call `std::env::var("TZ")` inside `device_local_day`, not in a lazy/static value. Parse the explicit firmware POSIX rule, convert `Timestamp::try_from(SystemTime::now())`, then copy the checked date into `LocalDay`. Return `None` on every failure.

Add `ReadLocalDay` to the universally safe read-only request set; it requires no app capability. Keep all existing capability checks unchanged.

- [ ] **Step 6: Run focused tests and observe GREEN**

Run:

```sh
cargo test -p kobo-protocol local_day -- --nocapture
cargo test -p kobo-sdk read_local_day -- --nocapture
cargo test -p kobod local_day -- --nocapture
cargo test -p kobo-sim local_day -- --nocapture
```

Expected: all local-calendar and request-path tests pass.

- [ ] **Step 7: Commit**

```sh
git add Cargo.toml Cargo.lock crates/kobod/Cargo.toml crates/kobo-sim/Cargo.toml crates/kobod/src/device.rs crates/kobod/src/main.rs crates/kobo-sim/src/lib.rs
git commit -m "feat(runtime): produce device local day"
```

---

### Task 3: Add exact public BOMTOON task contracts

**Files:**
- Modify: `apps/bomtoon/src/api.rs`
- Modify: `crates/kobo-net/src/lib.rs` (credential-regression tests only unless an existing production policy helper requires an exhaustive branch)

**Interfaces:**

```rust
pub fn homepage() -> Task;
pub fn public_detail(alias: &str) -> Task;
```

Exact routes:

```text
GET https://www.bomtoon.tw/comic/main
GET https://www.bomtoon.tw/detail/{validated-alias}
```

Both use `credential: None`, offset zero, a 512 KiB ceiling, and exactly the existing `response_headers("text/html")` values (`Accept: text/html` plus `Accept-Language`); neither sends `Referer` nor `x-referer`. Reuse `api::image(url)` for covers.

- [ ] **Step 1: Add failing API contract tests**

```rust
#[test]
fn homepage_is_public_and_bounded() {
    let Task::Fetch { url, offset, max_bytes, credential, .. } = homepage() else {
        panic!("homepage must be a fetch");
    };
    assert_eq!(url, "https://www.bomtoon.tw/comic/main");
    assert_eq!(offset, 0);
    assert_eq!(max_bytes, 512 * 1024);
    assert_eq!(credential, None);
}

#[test]
fn public_detail_is_public_and_exact() {
    let Task::Fetch { url, credential, max_bytes, .. } = public_detail("hunter_q") else {
        panic!("detail must be a fetch");
    };
    assert_eq!(url, "https://www.bomtoon.tw/detail/hunter_q");
    assert_eq!(credential, None);
    assert_eq!(max_bytes, 512 * 1024);
}
```

Test invalid alias construction at the caller/parser seam; never percent-encode or normalize an unsafe alias into a different path.

- [ ] **Step 2: Add bearer-denial regression tests**

Extend the existing `authenticated_next_data_and_detail_html_are_denied` coverage with `/comic/main` and the exact public detail route. Assert `credential_allowed("bomtoon", &access, Get, url) == false` for both. This proves a later refactor cannot accidentally attach `bomtoon-access-token` to public HTML.

- [ ] **Step 3: Run focused tests and observe RED**

Run:

```sh
cargo test -p kobo-bomtoon homepage_is_public -- --nocapture
cargo test -p kobo-net authenticated_next_data_and_detail_html_are_denied -- --nocapture
```

Expected: the app test fails to compile because `homepage`/`public_detail` do not exist. The credential regression may already pass because the current allowlist denies both public routes; keep it as a permanent invariant test rather than weakening production policy to manufacture RED.
- [ ] **Step 4: Implement the two constructors**

Follow the existing `fetch` helper and header conventions. Keep constants private, response ceilings explicit, and credentials absent. Do not construct `_next/data/<build-id>` URLs.

- [ ] **Step 5: Run focused tests and observe GREEN**

Run:

```sh
cargo test -p kobo-bomtoon api::tests -- --nocapture
cargo test -p kobo-net authenticated_next_data_and_detail_html_are_denied -- --nocapture
```

Expected: all API task-shape and credential-denial tests pass.

- [ ] **Step 6: Commit**

```sh
git add apps/bomtoon/src/api.rs crates/kobo-net/src/lib.rs
git commit -m "feat(bomtoon): add public shelf requests"
```

---

### Task 4: Parse bounded homepage, detail, and protected cover metadata

**Files:**
- Modify: `apps/bomtoon/src/model.rs`
- Modify: `apps/bomtoon/src/parse.rs`

**Interfaces:**

```rust
pub struct ShelfComic {
    pub alias: String,
    pub title: String,
    pub cover_url: Option<String>,
}

pub struct BannerComic {
    pub alias: String,
}

pub struct Homepage {
    pub banners: Vec<BannerComic>,
    pub newest: Vec<ShelfComic>,
    pub week_day: Vec<ShelfComic>,
    pub only_bom: Vec<ShelfComic>,
}

pub fn homepage(bytes: &[u8]) -> Result<Homepage, ParseError>;
pub fn public_detail(bytes: &[u8], expected_alias: &str) -> Result<ShelfComic, ParseError>;
```

Add `cover_url: Option<String>` to `Comic` and `RecentEntry`.

Parser limits:

```rust
const MAX_HOMEPAGE_BANNERS: usize = 64;
const MAX_HOMEPAGE_LIST: usize = 64;
const MAX_ALIAS_BYTES: usize = 96;
const MAX_TITLE_BYTES: usize = 256;
const MAX_COVER_URL_BYTES: usize = 2048;
```

- [ ] **Step 1: Add failing homepage behavior tests**

Use inline bounded HTML containing one `script#__NEXT_DATA__`. Test:

1. exact `props.pageProps.main` traversal;
2. first comic-target banners in source order;
3. event/shop/gift/pick/empty target rejection;
4. `newest → weekDay → onlyBom` list preservation;
5. missing artwork retained as `None`;
6. malformed individual alias/title dropped;
7. missing/invalid required section fails the whole parse;
8. extra scripts and a changed build ID do not matter;
9. arrays above limits fail before retained allocation grows;
10. only approved HTTPS `image.balcony.studio:443` public image paths survive.

```rust
#[test]
fn homepage_reads_only_next_data_main() {
    let parsed = homepage(HOMEPAGE_HTML.as_bytes()).expect("homepage");
    assert_eq!(parsed.banners, [BannerComic { alias: "featured_a".into() }]);
    assert_eq!(parsed.newest[0].alias, "new_a");
    assert_eq!(parsed.week_day[0].alias, "weekday_a");
    assert_eq!(parsed.only_bom[0].alias, "only_a");
}
```

- [ ] **Step 2: Add failing detail metadata tests**

Test exact Open Graph title/image extraction from a bounded public detail document, attribute-order variation, HTML entity decoding through the existing safe helper if one exists, exact BOMTOON title-suffix removal, expected-alias preservation, missing image as `None`, missing title as failure, and hostile image origins as `None`.

```rust
#[test]
fn public_detail_extracts_title_and_cover_without_episodes() {
    let comic = public_detail(DETAIL_HTML.as_bytes(), "hunter_q").expect("detail");
    assert_eq!(comic.alias, "hunter_q");
    assert_eq!(comic.title, "Hunter Q");
    assert_eq!(comic.cover_url.as_deref(), Some("https://image.balcony.studio/tw/contents/hunter_q.webp"));
}
```

The parser must not expose or retain episode JSON from this page.

- [ ] **Step 3: Add failing protected thumbnail tests**

Extend existing library/recent fixtures with square-thumbnail fields. Assert cover URLs are retained when valid, `None` when absent/hostile, and the rest of each protected item remains available.

- [ ] **Step 4: Run focused tests and observe RED**

Run:

```sh
cargo test -p kobo-bomtoon parse::tests::homepage -- --nocapture
cargo test -p kobo-bomtoon parse::tests::public_detail -- --nocapture
cargo test -p kobo-bomtoon parse::tests::library -- --nocapture
cargo test -p kobo-bomtoon parse::tests::recent -- --nocapture
```

Expected: missing models/functions/fields and failing thumbnail assertions.

- [ ] **Step 5: Implement the bounded parsers**

Extract only the body of the exact `__NEXT_DATA__` script, then call `kobo_json::parse`. Reuse the existing `field`, `string`, `array`, `unsigned`, and URI validation patterns. Validate aliases with the same ASCII alphanumeric/underscore/hyphen rule used by protected content routes. Parse only the confirmed public/non-adult thumbnail variant.

Do not allocate all remote arrays and truncate later. Reject an over-limit remote array from its reported length before collecting retained entries.

- [ ] **Step 6: Run focused tests and observe GREEN**

Run: `cargo test -p kobo-bomtoon parse::tests -- --nocapture`

Expected: all parser tests pass, including pre-existing episode, image, wallet, library, and recent tests.

- [ ] **Step 7: Commit**

```sh
git add apps/bomtoon/src/model.rs apps/bomtoon/src/parse.rs
git commit -m "feat(bomtoon): parse public shelves"
```

---

### Task 5: Cut the main screen over to destination navigation

**Files:**
- Modify: `apps/bomtoon/src/main.rs`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MainDestination {
    #[default]
    Featured,
    Recent,
    Library,
}

enum View {
    Status,
    Main,
    Account,
    Episodes,
    Reader,
}
```

`View::Status` remains the existing sign-in screen. Remove `Shelf`, `View::Library`, `library_view_page`, `recent_view_page`, `remember_shelf_page`, and every restoration caller in one clean cutover.

- [ ] **Step 1: Add failing startup and nav tests**

Test signed-out and signed-in starts. Signed-out startup must spawn the public homepage, show Featured, and avoid protected Library/Recent fetches. Signed-in startup starts Featured and the existing lightweight account/wallet probe independently; Library and Recent remain lazy.

```rust
#[test]
fn signed_out_start_opens_public_featured_without_protected_shelves() {
    let mut runner = AppRunner::new(Bomtoon::default());
    let commands = runner.start();
    assert_eq!(runner.app().view, View::Main);
    assert_eq!(runner.app().destination, MainDestination::Featured);
    assert!(commands.iter().any(is_homepage_fetch));
    assert!(!commands.iter().any(is_library_or_recent_fetch));
}
```

Add screen tests asserting the bottom destinations are exactly `Featured`, `Recent`, `Library`, horizontally centered by the existing `nav_bar` layout, and Featured selected by default. Task 8 adds the initial `ReadLocalDay` request after its state machine tests exist.
- [ ] **Step 2: Add failing authentication-gating tests**

For a signed-out app, tapping a Featured comic, Recent, or Library must open `View::Status`. Completing sign-in must return to `View::Main`, `MainDestination::Featured`, page zero, without resuming the blocked action. The main top bar shows `Sign in` only while signed out.

- [ ] **Step 3: Add failing no-restoration tests**

Start from nonzero Recent/Library pages, switch destination or open Account/Episodes, then Back. Assert the selected destination is on page zero. Replace existing exact-restoration assertions rather than leaving contradictory tests.

- [ ] **Step 4: Run focused tests and observe RED**

Run:

```sh
cargo test -p kobo-bomtoon signed_out_start_opens_public_featured -- --nocapture
cargo test -p kobo-bomtoon authentication_gating -- --nocapture
cargo test -p kobo-bomtoon destination_returns_to_page_one -- --nocapture
```

Expected: current startup opens protected Library, current pseudo-tabs are ordinary buttons, and current restoration fields preserve nonzero pages.

- [ ] **Step 5: Implement the clean navigation cutover**

Create `main_screen`, append one fixed nav bar to all three destination screens, and route destination action IDs before destination-specific item actions. On entering Recent/Library:

- if signed out, open Status and keep Featured as the selected return destination;
- if signed in and unloaded, spawn that protected request;
- if already loaded, render page zero;
- never preload the other protected destination.

Keep the current sign-in screen implementation. Change successful sign-in completion to Featured page zero. Delete obsolete shelf/restoration code and constants in the same change.

- [ ] **Step 6: Run focused tests and observe GREEN**

Run:

```sh
cargo test -p kobo-bomtoon signed_out_start_opens_public_featured -- --nocapture
cargo test -p kobo-bomtoon authentication_gating -- --nocapture
cargo test -p kobo-bomtoon destination_returns_to_page_one -- --nocapture
cargo test -p kobo-bomtoon navigation -- --nocapture
```

Expected: selected tests pass and no old restoration test remains.

- [ ] **Step 7: Commit**

```sh
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): add main destination navigation"
```

---

### Task 6: Build the Featured feed and bounded enrichment flow

**Files:**
- Modify: `apps/bomtoon/src/main.rs`

**Interfaces:**

```rust
enum FeaturedStatus {
    Unloaded,
    Loading,
    Ready,
    Failed,
}

struct FeaturedState {
    status: FeaturedStatus,
    generation: u64,
    featured: Vec<ShelfComic>,
    recommended: Vec<ShelfComic>,
    pending_details: usize,
    page: usize,
    stale_warning: Option<String>,
}

enum ShelfTaskPurpose {
    Homepage { generation: u64, refresh_day: Option<LocalDay> },
    BannerDetail { generation: u64, slot: usize, alias: String },
}
```

- [ ] **Step 1: Add failing feed-selection tests**

Test the pure feed builder with banners and lists:

- first three comic-target banner aliases in source order;
- list metadata reused when present;
- unresolved selected aliases become at most three detail requests;
- Recommended concatenates newest/weekDay/onlyBom and deduplicates exact alias, first occurrence wins;
- duplicates between Featured and Recommended remain;
- a failed unresolved detail retains the banner using its validated alias as the visible fallback title and no cover;
- stale generation outcomes do nothing.

```rust
#[test]
fn recommended_deduplicates_within_lists_but_not_against_featured() {
    let plan = plan_featured(homepage_fixture());
    assert_eq!(aliases(&plan.featured), ["shared", "banner_b", "banner_c"]);
    assert_eq!(aliases(&plan.recommended), ["shared", "new_b", "weekday_a", "only_a"]);
}
```

- [ ] **Step 2: Add failing paging tests**

Test page composition independently from drawing:

```rust
#[test]
fn featured_page_one_has_tiles_then_measured_recommendations() {
    let page = featured_page(&feed_with_recommendations(20), 0, 6);
    assert_eq!(page.featured.len(), 3);
    assert_eq!(page.recommended, 0..6);
}

#[test]
fn continuation_pages_do_not_repeat_featured_tiles() {
    let page = featured_page(&feed_with_recommendations(20), 1, 6);
    assert!(page.featured.is_empty());
    assert_eq!(page.recommended, 6..12);
}
```

- [ ] **Step 3: Run focused tests and observe RED**

Run:

```sh
cargo test -p kobo-bomtoon recommended_deduplicates -- --nocapture
cargo test -p kobo-bomtoon featured_page -- --nocapture
cargo test -p kobo-bomtoon banner_detail -- --nocapture
```

Expected: Featured state/task registry/feed builder do not exist.

- [ ] **Step 4: Implement homepage and detail outcome handling**

Initial homepage failure sets Featured-local Failed state with Retry while leaving the nav/top bar actionable. On success, build Recommended immediately, fill list-resolved featured slots, and spawn only unresolved selected detail requests. Finalize Ready after all bounded detail outcomes settle. An individual detail failure supplies the validated alias as the fallback title and `None` cover; it does not fail the destination.

Use a separate `BTreeMap<TaskId, ShelfTaskPurpose>` rather than overloading `pending` or reader task registries. Every outcome checks `generation` before mutation.

- [ ] **Step 5: Implement metadata-only Featured layout**

Page zero uses `ScreenBuilder::tile_grid` with `TileShape::Portrait` and glyph-only `Tile::new` values for up to three Featured entries, then a `Recommended` section of compact rows. Continuations omit the tile grid and show six Recommended rows. Add page-turn actions and the existing page-position label, but no Previous/Next buttons.

Compute the page-zero row count from `CLARA_BW_METRICS` layout results: try six, decrement until `screen.validate_with(&CLARA_BW_METRICS)` has no overflow. Assert the chosen count is nonzero for the target profile.

- [ ] **Step 6: Run focused tests and observe GREEN**

Run: `cargo test -p kobo-bomtoon featured -- --nocapture`

Expected: feed, enrichment, paging, failure, retry, stale-generation, and metadata-only layout tests pass.

- [ ] **Step 7: Commit**

```sh
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): build the featured feed"
```

---

### Task 7: Render protected compact shelves and progressively load covers

**Files:**
- Modify: `apps/bomtoon/src/main.rs`

**Interfaces:**

```rust
enum CoverState {
    Loading(TaskId),
    Ready(TilePicture),
    Failed,
}

struct CoverTask {
    generation: u64,
    url: String,
}

struct CoverCache {
    generation: u64,
    entries: BTreeMap<String, CoverState>,
    tasks: BTreeMap<TaskId, CoverTask>,
}
```

The actual implementation may keep these fields directly on `Bomtoon` if that is shallower; do not create a new module for this state.

- [ ] **Step 1: Add failing protected row/layout tests**

Assert:

- Recent page size is six and summaries are episode titles;
- Library page size is six and summaries are `owned / total`;
- every placeholder row uses `RowLead::Icon(Glyph::Book)`;
- a completed cover changes only its row lead to `RowLead::Picture(picture, Glyph::Book)`;
- both screens retain the fixed nav bar and fit `CLARA_BW_METRICS`.

- [ ] **Step 2: Add failing cover scheduler tests**

Cover the observable scheduler contract:

1. only URLs visible on the current page are requested;
2. duplicate visible URLs produce one fetch and one decoded handle;
3. changing page/destination cancels obsolete in-flight cover tasks;
4. a stale generation completion is ignored;
5. a failed image keeps the glyph placeholder without destination error;
6. a cached completed picture is reused across Featured and Recommended;
7. task spawning stops when `Context::spawn` returns `None` and resumes after a task settles;
8. total runner tasks in flight never exceeds four.

```rust
#[test]
fn duplicate_visible_cover_url_fetches_once() {
    let (mut runner, url) = featured_with_duplicate_cover();
    let commands = runner.start();
    assert_eq!(fetches_of(&commands, &url), 1);
}

#[test]
fn obsolete_page_cover_tasks_are_cancelled() {
    let (mut runner, first_page_tasks) = loading_featured_covers();
    let commands = runner.action(action_id(NEXT_PAGE));
    assert_eq!(cancelled_tasks(&commands), first_page_tasks);
}
```

Sign-out partitioning is verified in Task 9 after protected/public cleanup is implemented.
- [ ] **Step 3: Run focused tests and observe RED**

Run:

```sh
cargo test -p kobo-bomtoon compact_shelf -- --nocapture
cargo test -p kobo-bomtoon cover_ -- --nocapture
```

Expected: current protected shelves show three text buttons, and no shelf cover registry exists.

- [ ] **Step 4: Implement compact protected rows**

Use `ScreenBuilder::rows` or `rows_with_trailing` with `RowLead::Picture(picture, Glyph::Book)` when ready and `RowLead::Icon(Glyph::Book)` otherwise. Keep comic title as title. Use episode title for Recent summary and `format!("{} / {}", owned, total)` for Library summary. Page-turn actions update the destination page and immediately reprioritize visible covers.

- [ ] **Step 5: Implement the URL-keyed scheduler**

On every visible-page change:

1. increment the visible-cover generation;
2. compute a `BTreeSet<String>` of validated visible URLs;
3. cancel loading tasks whose URLs left that set;
4. reuse every Ready cache entry;
5. spawn missing URLs in visible order until `Context::spawn` returns `None`;
6. decode completed bytes with the existing `kobo_image` path;
7. install one `PictureHandle`, construct a dimensioned `TilePicture`, update the URL entry, redraw, and schedule the next missing visible URL.

Do not persist bytes. Do not retry a failed URL automatically in the same generation. A later explicit destination Retry or metadata refresh may clear Failed for URLs visible in the new generation.
- [ ] **Step 6: Run focused tests and observe GREEN**

Run:

```sh
cargo test -p kobo-bomtoon compact_shelf -- --nocapture
cargo test -p kobo-bomtoon cover_ -- --nocapture
cargo test -p kobo-bomtoon layout -- --nocapture
```

Expected: compact-shelf, cover scheduling, task-cap, cache, cancellation, sign-out partitioning, and Clara layout tests pass.

- [ ] **Step 7: Commit**

```sh
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): load visible shelf covers"
```

---

### Task 8: Wire runtime local-day observations to daily refresh

**Files:**
- Modify: `apps/bomtoon/src/main.rs`

**Interfaces:**

```rust
struct FeaturedRefresh {
    loaded_day: Option<LocalDay>,
    desired_day: Option<LocalDay>,
    active_day: Option<LocalDay>,
    local_day_pending: bool,
}

fn request_local_day(&mut self, context: &mut Context);
fn observe_local_day(&mut self, context: &mut Context, observed: Option<LocalDay>);
```

- [ ] **Step 1: Add the required failing transition tests**

Test the state machine without wall-clock access:

```rust
#[test]
fn first_known_day_establishes_baseline_without_refresh() {
    let mut app = ready_featured_with_day(None);
    let commands = observe(&mut app, LocalDay::new(2026, 8, 30));
    assert!(homepage_fetches(&commands).is_empty());
    assert_eq!(app.featured.refresh.loaded_day, LocalDay::new(2026, 8, 30));
}

#[test]
fn later_different_day_refreshes_exactly_once() {
    let mut app = ready_featured_with_day(LocalDay::new(2026, 8, 30));
    let first = observe(&mut app, LocalDay::new(2026, 8, 31));
    let repeated = observe(&mut app, LocalDay::new(2026, 8, 31));
    assert_eq!(homepage_fetches(&first).len(), 1);
    assert!(homepage_fetches(&repeated).is_empty());
}
```

Also require:

- current `None` does not overwrite a known baseline;
- same day is a no-op;
- a newer day observed during an active refresh remains `desired_day` and starts after the active request settles;
- refresh success atomically replaces both sections, records the target, clears warning, resets page zero, and reprioritizes covers;
- refresh failure keeps old feed/pictures/baseline, sets nonblocking warning, and exposes one Retry;
- Retry reuses the same refresh path;
- stale/unknown device results are no-ops;
- overlapping start/resume/foreground/Featured-entry calls emit one `ReadLocalDay` request.

- [ ] **Step 2: Run focused tests and observe RED**

Run: `cargo test -p kobo-bomtoon local_day -- --nocapture`

Expected: no BOMTOON device-result handling or refresh state exists.

- [ ] **Step 3: Request local day at every required boundary**

Call the coalescing helper from:

- initial Featured startup;
- `KoboApp::on_resume`;
- `KoboApp::on_foreground`;
- the action that enters Featured.

Handle only `(DeviceRequest::ReadLocalDay, DeviceResult::LocalDay(day))` in `on_device_result`. Clear `local_day_pending` before evaluating the observation so a follow-up request can be issued. Mismatched result variants leave state unchanged and surface no guessed date.

- [ ] **Step 4: Implement the one-active/one-desired refresh state machine**

A first `Some(day)` establishes the baseline. A different known day sets `desired_day`; if no homepage refresh is active, move it to `active_day` and spawn one homepage task tagged with that target. When a refresh settles, compare the latest desired day before recording the active target or starting another request.

The refresh task uses the same homepage parser and banner enrichment flow as initial load. Only the commit point differs: preserve the old Ready feed until all new metadata/enrichment work settles, then atomically swap.

- [ ] **Step 5: Run focused tests and observe GREEN**

Run:

```sh
cargo test -p kobo-bomtoon local_day -- --nocapture
cargo test -p kobo-bomtoon refresh_ -- --nocapture
cargo test -p kobo-bomtoon featured -- --nocapture
```

Expected: every baseline, coalescing, newer-day, success, failure, Retry, and stale-generation test passes.

- [ ] **Step 6: Commit**

```sh
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): refresh featured by local day"
```

---

### Task 9: Complete account, sign-out, and end-to-end regressions

**Files:**
- Modify: `apps/bomtoon/src/main.rs`

- [ ] **Step 1: Add failing top-bar/account transition tests**

Assert all three signed-in main destinations show tappable aggregate coin balance opening Account, with no separate Account button. Signed-out Featured shows Sign in. Account owns Sign out. Back from Account returns the previously selected destination at page zero. Back from Episodes returns its source destination at page zero.

- [ ] **Step 2: Add failing sign-out partition tests**

After loading public and protected feeds/pictures, sign out and assert:

- view is main Featured page zero;
- Featured and Recommended metadata remain;
- public cover handles remain reusable;
- Library, Recent, Account detail, episodes, reader state, and their protected cover references are empty;
- protected tasks are cancelled/invalidated;
- public homepage/cover tasks are not cancelled merely because the account changed;
- tapping Recent/Library now opens Status.

- [ ] **Step 3: Run focused tests and observe RED**

Run:

```sh
cargo test -p kobo-bomtoon top_bar_account -- --nocapture
cargo test -p kobo-bomtoon sign_out_retains_public -- --nocapture
cargo test -p kobo-bomtoon back_returns_destination_page_one -- --nocapture
```

Expected: current Account/Sign out controls live on the old library screen and current cleanup does not partition public/protected state.

- [ ] **Step 4: Finish the transition cleanup**

Move sign-out exclusively to Account. Make the signed-in top-bar coin value the Account action on Featured, Recent, and Library. Split cleanup into explicit public-preserving protected cleanup and full-exit cleanup; do not condition behavior on URL string heuristics when task purpose already identifies ownership.

Delete obsolete Account buttons, Previous/Next controls, shelf page fields, pseudo-tab constants, and tests for removed restoration behavior.

- [ ] **Step 5: Run all BOMTOON tests**

Run: `cargo test -p kobo-bomtoon --all-targets --all-features`

Expected: all BOMTOON tests pass, including pre-existing login, reader, continuous-reader, wallet, credential, and cancellation suites.

- [ ] **Step 6: Commit**

```sh
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): complete featured shelf transitions"
```

---

### Task 10: Verify real layouts, simulators, and workspace gates

**Files:**
- Modify only files required by failures found below.

- [ ] **Step 1: Run format and focused static checks**

```sh
cargo fmt --all --check
cargo clippy -p kobo-protocol -p kobo-sdk -p kobod -p kobo-sim -p kobo-bomtoon --all-targets --all-features -- -D warnings
```

Expected: both commands exit 0.

- [ ] **Step 2: Run runtime boundary suites**

```sh
cargo test -p kobo-protocol --all-targets --all-features
cargo test -p kobo-sdk --all-targets --all-features
cargo test -p kobod --all-targets --all-features
cargo test -p kobo-sim --all-targets --all-features
```

Expected: all pass, including protocol version/round-trip, local-day conversion, request handling, and simulator coverage.

- [ ] **Step 3: Run the BOMTOON browser simulator**

From `apps/bomtoon`:

```sh
cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

Exercise the actual surface:

1. signed-out startup shows Featured and a fixed Featured/Recent/Library bar;
2. Featured placeholder metadata renders before covers;
3. covers replace placeholders without row/tile movement;
4. page turns show continuation Recommended rows without repeating Featured;
5. signed-out comic/Recent/Library opens Sign in;
6. successful sign-in returns Featured page 1;
7. Recent and Library lazy-load and show six compact cover rows where metrics permit;
8. coin balance opens Account; Sign out returns public Featured and clears protected shelves;
9. starting the simulator with an explicit valid `TZ` produces no UTC/Taipei fallback warning. The deterministic runtime tests from Task 2 prove that a later request re-reads a changed explicit timezone.

Capture the simulator output/screenshot locations in the PR description or operator notes; do not create a new evidence document unless requested.

- [ ] **Step 4: Run the runtime simulator**

```sh
cargo run -p kobo-cli -- run --sim --app bomtoon
```

Exercise Featured → Recent → Library → Featured, page turns, comic open/Back, Account/Back, and sign-out. Expected: no overflow, dead touch target, task-cap failure, credential prompt on public requests, or restored nonzero destination page.

- [ ] **Step 5: Run full workspace gates**

```sh
cargo fmt --all --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all commands exit 0.

- [ ] **Step 6: Inspect scope and commit only verification fixes**

Review the changed files against this plan and the approved specification. Remove dead aliases, obsolete constants, compatibility paths, unused parser fields, and accidental documentation. Search changed files for `TODO`, `TBD`, `placeholder`, `unimplemented!`, and `todo!`; the only allowed use of “placeholder” is user-visible glyph-placeholder behavior, not unfinished code.

If verification required code changes:

Stage only the exact files changed to repair verification failures, then run:

```sh
git commit -m "fix(bomtoon): close featured shelf regressions"
```

Do not create an empty verification commit.
