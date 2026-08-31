# BOMTOON Episode List Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a personalized BOMTOON episode screen with comic metadata, a paginated full-synopsis modal, rich episode rows, visible-page thumbnails, dates, and truthful access labels.

**Architecture:** Replace the old authenticated JSON content request with the personalized Next.js payload embedded in authenticated `/detail/{alias}` HTML. Keep parsing and input bounds in `parse.rs`, domain data in `model.rs`, task construction in `api.rs`, and app state, rendering, navigation, image ownership, and commerce integration in `main.rs`. Reuse existing `ScreenBuilder` rows, modal, diagnostics, picture cache, foreground-task, and commerce state machines; add no SDK primitive or dependency.

**Tech Stack:** Rust 2021, `kobo_json`, `kobo_sdk::Task`, `ScreenBuilder`, `kobo_image`, unit tests in app modules, browser simulator, runtime simulator.

## Global Constraints

- Work only in `/Users/nelson.liu/Developer/Cobalt/.worktrees/bomtoon-episode-list` on `feat/bomtoon-episode-list`; the main worktree must remain clean.
- Modify only `apps/bomtoon/src/api.rs`, `apps/bomtoon/src/model.rs`, `apps/bomtoon/src/parse.rs`, `apps/bomtoon/src/main.rs`, and the approved design status at `docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md`.
- Add no dependency, source module, SDK/protocol type, capability, network origin, or Store metadata change.
- Require `props.pageProps.ssrPersonalized == true` and exact alias equality before trusting episode access.
- Parse only allowlisted `ssrDetail` business fields. Never retain, copy into tests, expose, or log `userData`, email addresses, tokens, cookies, or credentials.
- Keep raw HAR files local evidence only. Tests use minimal synthetic sanitized HTML.
- Preserve quote, purchase, Gift, wallet, unresolved-mutation, reader, comment, account, and stale-result semantics.
- Unknown access states remain fail-closed `PurchaseState::Other`; they are never treated as unowned or readable.
- Unowned rows have an empty trailing status but retain the existing purchase-options action.
- Thumbnail failure degrades only to the book glyph. Detail identity, personalization, and required text/date failures use the existing foreground error and retry path.
- No automated or unattended check may spend Coin or consume a Gift.
- Run all shell commands through `rtk`.

## File Structure

- `apps/bomtoon/src/api.rs`: exact authenticated HTML detail task and existing public detail task separation.
- `apps/bomtoon/src/model.rs`: personalized comic metadata and episode list presentation fields beside existing access and commerce fields.
- `apps/bomtoon/src/parse.rs`: inert-content-aware `__NEXT_DATA__` extraction, personalization and identity gates, bounded creators/synopsis/episodes, safe `COMMON` thumbnail parsing, and existing access conversion.
- `apps/bomtoon/src/main.rs`: selected metadata, synopsis overlay state, dynamic episode row capacity, rich rows, status/date copy, thumbnail scheduling and cleanup, actions, and all app-level regression tests.
- `docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md`: final evidence status only after all gates and both simulator smoke checks pass.

---

### Task 1: Personalized Detail Contract And Clean Request Cutover

**Files:**
- Modify: `apps/bomtoon/src/api.rs:5-104,223-368`
- Modify: `apps/bomtoon/src/model.rs:33-63`
- Modify: `apps/bomtoon/src/parse.rs:10-75,473-537,756-907,2319-2435`
- Modify: `apps/bomtoon/src/main.rs:960-1016,3080-3090,3518-3524,4304-4334,6880-6925,12820-12840,14945-14960,15940-16540`
- Test: inline `#[cfg(test)]` modules in all four files

**Interfaces:**
- Consumes: existing `next_data_payload`, `bounded_array`, `bounded_string`, `positive_i64`, `public_image_url`, `EpisodeAvailability`, `PurchaseState::from_remote`, `Pending::Content`, and managed credential name `bomtoon-access-token`.
- Produces:
  - `api::detail(alias: &str) -> Task`
  - `parse::content_detail(bytes: &[u8], expected_alias: &str) -> Result<ContentDetail, ParseError>`
  - `ContentDetail { id: usize, title: String, creators: Vec<String>, synopsis: String, episodes: Vec<Episode> }`
  - `Episode` fields `opened_at: i64` and `thumbnail_url: Option<String>` in addition to all current fields
  - selected app fields `selected_creators: String` and `selected_synopsis: String`

- [ ] **Step 1: Write failing API tests for authenticated HTML detail**

Replace the two old `content` tests with one exact contract test while keeping `public_detail_is_public_and_exact` unchanged:

```rust
#[test]
fn detail_uses_managed_bearer_html_endpoint() {
    let Task::Fetch {
        url,
        offset,
        max_bytes,
        credential,
        headers,
    } = detail("365")
    else {
        panic!("detail must be a fetch");
    };
    assert_eq!(url, "https://www.bomtoon.tw/detail/365");
    assert_eq!(offset, 0);
    assert_eq!(max_bytes, 512 * 1024);
    assert!(matches!(
        credential,
        Some(value)
            if value.secret == "bomtoon-access-token"
                && value.header == SecretHeader::Bearer
    ));
    assert_eq!(
        headers,
        vec![
            Header::new("Accept", "text/html"),
            Header::new("Accept-Language", ACCEPT_LANGUAGE),
        ]
    );
    assert!(headers.iter().all(|header| {
        !header.name.eq_ignore_ascii_case("cookie")
            && !header.name.eq_ignore_ascii_case("authorization")
    }));
}
```

Update the test import from `content` to `detail`.

- [ ] **Step 2: Run the API test and verify red**

Run:

```sh
rtk cargo test -p kobo-bomtoon api::tests::detail_uses_managed_bearer_html_endpoint -- --exact
```

Expected: compilation fails because `api::detail` does not exist.

- [ ] **Step 3: Write sanitized parser contract tests**

Add a fixture builder that cannot contain captured account data:

```rust
fn personalized_detail_html(personalized: bool, alias: &str, episodes: &str) -> Vec<u8> {
    format!(
        concat!(
            "<!doctype html><html><head>",
            "<script id=\"__NEXT_DATA__\" type=\"application/json\">",
            "{{\"props\":{{\"pageProps\":{{",
            "\"ssrPersonalized\":{personalized},",
            "\"userData\":{{\"ignored\":true}},",
            "\"ssrDetail\":{{",
            "\"id\":41,\"alias\":\"{alias}\",\"title\":\"Hunter Q\",",
            "\"creators\":[{{\"creatorId\":1,\"name\":\"Writer\",\"type\":\"WRITER\"}},",
            "{{\"creatorId\":2,\"name\":\"Artist\",\"type\":\"ARTIST\"}}],",
            "\"synopsis\":\"A complete synopsis.\",",
            "\"episodes\":[{episodes}]",
            "}}}}}}}}",
            "</script></head></html>"
        )
    )
    .into_bytes()
}

const OWNED_EPISODE: &str = concat!(
    "{\"id\":101,\"alias\":\"ep-1\",\"title\":\"Episode One\",",
    "\"openedAt\":1709136000000,",
    "\"thumbnails\":[{\"type\":\"COMMON\",",
    "\"imagePath\":\"https://image.balcony.studio/tw/ep_thumbnail/101/cover.webp\"}],",
    "\"type\":\"GENERAL\",\"isSample\":false,",
    "\"purchaseStatus\":\"POSSESSION\",\"paid\":true,",
    "\"coinKind\":\"COIN\",\"possessionCoin\":2,\"rentCoin\":1,",
    "\"permanentCoin\":2,\"isRentGift\":false}"
);

#[test]
fn personalized_detail_html_retains_metadata_episode_date_thumbnail_and_access() {
    let parsed = content_detail(
        &personalized_detail_html(true, "hunter_q", OWNED_EPISODE),
        "hunter_q",
    )
    .expect("personalized detail");
    assert_eq!(parsed.id, 41);
    assert_eq!(parsed.title, "Hunter Q");
    assert_eq!(
        parsed.creators,
        vec!["Writer".to_owned(), "Artist".to_owned()]
    );
    assert_eq!(parsed.synopsis, "A complete synopsis.");
    assert_eq!(parsed.episodes[0].opened_at, 1_709_136_000_000);
    assert_eq!(
        parsed.episodes[0].thumbnail_url.as_deref(),
        Some("https://image.balcony.studio/tw/ep_thumbnail/101/cover.webp")
    );
    assert_eq!(parsed.episodes[0].purchase, PurchaseState::Owned);
}

#[test]
fn detail_rejects_unpersonalized_or_wrong_alias_without_reading_user_data() {
    assert!(matches!(
        content_detail(
            &personalized_detail_html(false, "hunter_q", OWNED_EPISODE),
            "hunter_q"
        ),
        Err(ParseError::InvalidValue("props.pageProps.ssrPersonalized"))
    ));
    assert!(matches!(
        content_detail(
            &personalized_detail_html(true, "another", OWNED_EPISODE),
            "hunter_q"
        ),
        Err(ParseError::InvalidValue("ssrDetail.alias"))
    ));
}
```

Add focused cases for:

- missing or inert `__NEXT_DATA__`;
- wrong `ssrPersonalized` type;
- empty, oversized, or wrong-type creator names;
- empty or oversized synopsis;
- more than the episode limit;
- zero, negative, or wrong-type `openedAt`;
- non-`COMMON` thumbnail ignored;
- missing thumbnail field degrading to `None`;
- hostile host, query, fragment, extension, or path degrading to `None`;
- two `COMMON` entries returning `InvalidValue("episode.thumbnails")`;
- unknown `purchaseStatus` remaining `PurchaseState::Other`.

- [ ] **Step 4: Run parser tests and verify red**

Run:

```sh
rtk cargo test -p kobo-bomtoon parse::tests::personalized_detail_html_retains_metadata_episode_date_thumbnail_and_access -- --exact
```

Expected: compilation fails because `content_detail` lacks the expected alias argument and the model lacks the new fields.

- [ ] **Step 5: Add bounded domain fields and parser limits**

Change the domain structs exactly to:

```rust
pub struct ContentDetail {
    pub id: usize,
    pub title: String,
    pub creators: Vec<String>,
    pub synopsis: String,
    pub episodes: Vec<Episode>,
}

pub struct Episode {
    pub id: usize,
    pub alias: String,
    pub title: String,
    pub opened_at: i64,
    pub thumbnail_url: Option<String>,
    pub purchase: PurchaseState,
    pub rent_expires_at: Option<i64>,
    pub rent_coin: Option<usize>,
    pub purchase_coin: Option<usize>,
    pub gift_eligible: bool,
}
```

Add parser limits beside the existing remote limits:

```rust
const MAX_DETAIL_CREATORS: usize = 32;
const MAX_CREATOR_NAME_BYTES: usize = 256;
const MAX_SYNOPSIS_BYTES: usize = 16 * 1024;
const MAX_EPISODES: usize = 512;
const MAX_EPISODE_THUMBNAILS: usize = 8;
const EPISODE_THUMBNAIL_PATHS: &[&str] = &[
    "/tw/ep_thumbnail/",
    "/BOMTOON_TW/ep_thumbnail/",
];
```

These limits keep the 512 KiB response ceiling authoritative while allowing long synopses and complete series.

- [ ] **Step 6: Replace the old JSON envelope parser with personalized HTML parsing**

Implement the entry boundary with existing helpers:

```rust
pub fn content_detail(
    bytes: &[u8],
    expected_alias: &str,
) -> Result<ContentDetail, ParseError> {
    if !valid_alias(expected_alias) {
        return Err(ParseError::InvalidValue("detail alias"));
    }
    let html = str::from_utf8(bytes).map_err(ParseError::Utf8)?;
    let payload = next_data_payload(html).ok_or(ParseError::Missing("__NEXT_DATA__"))?;
    let root = kobo_json::parse(payload).map_err(ParseError::Json)?;
    let props = field(&root, "props", "props")?;
    let page_props = field(props, "pageProps", "props.pageProps")?;
    if !boolean(
        page_props,
        "ssrPersonalized",
        "props.pageProps.ssrPersonalized",
    )? {
        return Err(ParseError::InvalidValue(
            "props.pageProps.ssrPersonalized",
        ));
    }
    let detail = field(page_props, "ssrDetail", "props.pageProps.ssrDetail")?;
    let alias = bounded_string(detail, "alias", "ssrDetail.alias", MAX_ALIAS_BYTES)?;
    if alias != expected_alias {
        return Err(ParseError::InvalidValue("ssrDetail.alias"));
    }
    // Parse required title, ordered creators, synopsis, and bounded episodes here.
}
```

Use a dedicated `episode_thumbnail(value: &Value) -> Result<Option<String>, ParseError>`:

- absent `thumbnails` returns `Ok(None)`;
- a present non-array or more than eight entries is an error;
- only `type == "COMMON"` participates;
- two `COMMON` entries are ambiguous and fail;
- a missing or wrong-type `imagePath` on the selected entry fails;
- `public_image_url(url, EPISODE_THUMBNAIL_PATHS)` decides `Some(url)` versus glyph fallback `None`.

Retain all existing pricing, Gift eligibility, rental expiry, `EpisodeAvailability`, and `PurchaseState::from_remote` logic. Read `openedAt` with `positive_i64`.

- [ ] **Step 7: Add the authenticated HTML task and remove the old API constructor**

Replace `CONTENT_URL`, `CONTENT_BYTES`, and `content` with:

```rust
pub fn detail(alias: &str) -> Task {
    fetch(
        format!("{DETAIL_URL}{alias}"),
        PUBLIC_HTML_BYTES,
        Credential::bearer("bomtoon-access-token"),
        response_headers("text/html"),
    )
}
```

Keep `public_detail` unchanged and credential-free for Featured shelf enrichment.

- [ ] **Step 8: Migrate the foreground caller and selected metadata atomically**

Add app fields:

```rust
selected_creators: String,
selected_synopsis: String,
```

Use `api::detail(&self.selected_content_alias)` for `Pending::Content`. In `accept`, snapshot `self.selected_content_alias` before the mutable match if borrow checking requires it, then call:

```rust
parse::content_detail(bytes, &self.selected_content_alias)
```

On success, assign all detail fields before showing Episodes:

```rust
self.selected_content_id = Some(detail.id);
self.selected_title = detail.title;
self.selected_creators = detail.creators.join(" | ");
self.selected_synopsis = detail.synopsis;
self.episodes = detail.episodes;
```

Clear creators and synopsis wherever title identity is cleared, including sign-out, credential loss, restart, and full state cleanup. Preserve the existing reader-after-refresh page and commerce reconciliation branches.

- [ ] **Step 9: Migrate all main-module content fixtures without captured data**

Create one synthetic wrapper in `main.rs` tests:

```rust
fn detail_response(id: usize, alias: &str, title: &str, episodes: &str) -> Vec<u8> {
    format!(
        concat!(
            "<script id=\"__NEXT_DATA__\">",
            "{{\"props\":{{\"pageProps\":{{",
            "\"ssrPersonalized\":true,",
            "\"ssrDetail\":{{\"id\":{id},\"alias\":\"{alias}\",",
            "\"title\":\"{title}\",",
            "\"creators\":[{{\"name\":\"Writer\"}}],",
            "\"synopsis\":\"Synopsis\",",
            "\"episodes\":[{episodes}]}}",
            "}}}}}}",
            "</script>"
        )
    )
    .into_bytes()
}
```

Every synthetic episode used by this helper must add a positive `openedAt`; thumbnails may be omitted to exercise glyph fallback. Replace `CONTENT_RESPONSE.to_vec()` with a `content_response()` helper that passes the selected `hunter_q` alias, and pass each test's actual selected alias to one-off detail responses. Replace `RENTED_CONTENT`, `OWNED_CONTENT`, `EXPIRED_RENT`, and `TWO_UNOWNED` JSON constants with calls to the same wrapper. Keep quote, receipt, Gift, wallet, image-manifest, library, and recent fixtures unchanged.

Add `opened_at` and `thumbnail_url` to direct `Episode` literals and test constructors. Use `opened_at: 1_709_136_000_000` and `thumbnail_url: None` unless a test asserts presentation.

- [ ] **Step 10: Run the data-contract and regression tests**

Run:

```sh
rtk cargo test -p kobo-bomtoon api::tests::detail_uses_managed_bearer_html_endpoint -- --exact
rtk cargo test -p kobo-bomtoon parse::tests::personalized_detail_html_retains_metadata_episode_date_thumbnail_and_access -- --exact
rtk cargo test -p kobo-bomtoon
```

Expected: all tests pass; the full count is at least the 339-test baseline plus the new parser/API cases.

- [ ] **Step 11: Commit the clean data cutover**

```sh
rtk git add apps/bomtoon/src/api.rs apps/bomtoon/src/model.rs apps/bomtoon/src/parse.rs apps/bomtoon/src/main.rs
rtk git commit -m "feat(bomtoon): load personalized title detail"
```

---

### Task 2: Comic Header And Full Synopsis Modal

**Files:**
- Modify: `apps/bomtoon/src/main.rs:21-58,163-196,960-1016,1295-1368,1718-1788,3070-3090,4304-4334,5649-5780`
- Test: `apps/bomtoon/src/main.rs` inline tests near existing episode screen and date tests

**Interfaces:**
- Consumes: Task 1 selected fields `selected_title`, `selected_creators`, `selected_synopsis`; existing `ScreenBuilder::heading`, `text`, `button`, `modal`, `Screen::diagnostics`, `CLARA_BW_METRICS`, and `Chrome::measuring(true)`.
- Produces:
  - `SynopsisState { pages: Vec<std::ops::Range<usize>>, open_page: Option<usize> }`
  - pure `synopsis_pages(text: &str) -> Vec<Range<usize>>`
  - action constants `SYNOPSIS_MORE`, `SYNOPSIS_PREVIOUS`, `SYNOPSIS_NEXT`, `SYNOPSIS_CLOSE`
  - `add_episode_header(&self, ScreenBuilder) -> ScreenBuilder`
  - modal overlay composed by `episode_screen`

- [ ] **Step 1: Write failing header and modal behavior tests**

Seed an Episodes app with title, creators, a multi-page synopsis, one episode, known wallet/Gift values, and `CLARA_BW_METRICS`. Assert:

```rust
#[test]
fn episode_header_shows_metadata_balance_and_more_without_duplicate_title() {
    let app = episode_metadata_app(long_synopsis());
    let screen = app.episode_screen();
    assert_eq!(screen.top_bar.as_ref().expect("top bar").title, "Episodes");
    let drawn = format!("{screen:?}");
    assert!(drawn.contains("Hunter Q"));
    assert!(drawn.contains("Writer | Artist"));
    assert!(drawn.contains("Coins 10"));
    assert!(drawn.contains("Gifts 2"));
    assert!(screen.actions().contains(&action_id(SYNOPSIS_MORE)));
    assert_fits(&screen);
}

#[test]
fn synopsis_modal_pages_preserve_all_text_and_close_to_same_episode_page() {
    let synopsis = long_synopsis();
    let mut runner = AppRunner::with_metrics(
        episode_metadata_app(synopsis.clone()),
        CLARA_BW_METRICS,
    );
    runner.app_mut().page = 2;
    let open = runner.action(action_id(SYNOPSIS_MORE));
    assert!(last_screen(&open).overlay.is_some());
    let mut observed = String::new();
    loop {
        let screen = runner.app().screen();
        let overlay = screen.overlay.as_ref().expect("synopsis modal");
        observed.push_str(&overlay_text(overlay));
        assert_fits(&screen);
        if !screen.actions().contains(&action_id(SYNOPSIS_NEXT)) {
            break;
        }
        runner.action(action_id(SYNOPSIS_NEXT));
    }
    assert_eq!(observed, synopsis);
    runner.action(action_id(SYNOPSIS_CLOSE));
    assert_eq!(runner.app().page, 2);
    assert!(runner.app().screen().overlay.is_none());
}
```

Also cover:

- short synopsis shows full preview and no `More`;
- modal first page has no Previous;
- middle page has Previous and Next;
- final page has no Next;
- Back closes the modal before leaving Episodes;
- modal-open state blocks page turns, episode taps, Gift retry, and commerce actions.

If `Screen` has no `actions()` helper, use the existing test helper that collects actions from nodes and overlay nodes; do not add production API.

- [ ] **Step 2: Run the header test and verify red**

Run:

```sh
rtk cargo test -p kobo-bomtoon tests::episode_header_shows_metadata_balance_and_more_without_duplicate_title -- --exact
```

Expected: FAIL because the top bar still contains the selected title and no synopsis action exists.

- [ ] **Step 3: Add synopsis state and UTF-8-safe measured pagination**

Add:

```rust
const SYNOPSIS_MORE: &str = "synopsis-more";
const SYNOPSIS_PREVIOUS: &str = "synopsis-previous";
const SYNOPSIS_NEXT: &str = "synopsis-next";
const SYNOPSIS_CLOSE: &str = "synopsis-close";
const SYNOPSIS_PREVIEW_BYTES: usize = 240;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SynopsisState {
    pages: Vec<std::ops::Range<usize>>,
    open_page: Option<usize>,
}
```

Implement `synopsis_pages` so it:

1. records UTF-8 character boundaries;
2. binary-searches the largest candidate suffix prefix whose worst-case modal (Previous, Next, Close all present) has no diagnostic errors under `CLARA_BW_METRICS` and `Chrome::measuring(true)`;
3. retreats to the last paragraph or whitespace boundary when doing so leaves a nonempty page;
4. preserves the exact original byte ranges with no inserted or dropped characters;
5. always advances by at least one Unicode scalar.

The fit probe must use the actual overlay primitive:

```rust
fn synopsis_modal_fits(text: &str) -> bool {
    let screen = ScreenBuilder::new("bomtoon-synopsis-measure")
        .top_bar("Episodes")
        .modal("Synopsis", |modal| {
            modal
                .text(text)
                .button(SYNOPSIS_PREVIOUS, "Previous")
                .button(SYNOPSIS_NEXT, "Next")
                .button(SYNOPSIS_CLOSE, "Close")
        })
        .build();
    !screen
        .diagnostics(&CLARA_BW_METRICS, &Chrome::measuring(true))
        .has_errors()
}
```

Build `SynopsisState::pages` immediately after accepting new detail. Clear it with selected title metadata. Do not recompute pages on every screen draw.

- [ ] **Step 4: Render the comic header and conditional modal**

Refactor `episode_screen` to begin with:

```rust
let mut screen = ScreenBuilder::new("bomtoon-episodes")
    .top_bar("Episodes")
    .heading(self.selected_title.clone())
    .text(self.selected_creators.clone());
let (preview, truncated) = comment_preview(
    &self.selected_synopsis,
    SYNOPSIS_PREVIEW_BYTES,
);
screen = screen.text(preview);
if truncated {
    screen = screen.button(SYNOPSIS_MORE, "More");
}
```

Then append the existing balance/Gift retry, purchase rejection, and cross-account notice in their current order before episode rows. After adding page turns and position but before `build`, add the modal when `open_page` is set. The overlay shows the exact `selected_synopsis[range]` and only the valid navigation buttons for that page; Close is always last.

- [ ] **Step 5: Give synopsis modal actions first ownership**

At the top of `on_action`, before Gift retry, commerce, Back, page turns, or episode actions:

```rust
if let Some(page) = self.synopsis.open_page {
    if action == action_id(SYNOPSIS_PREVIOUS) {
        self.synopsis.open_page = Some(page.saturating_sub(1));
    } else if action == action_id(SYNOPSIS_NEXT)
        && page.saturating_add(1) < self.synopsis.pages.len()
    {
        self.synopsis.open_page = Some(page.saturating_add(1));
    } else if action == action_id(SYNOPSIS_CLOSE) || action == ActionId::BACK {
        self.synopsis.open_page = None;
    }
    self.show(context);
    return;
}
```

Open page 0 only when `More` is pressed in Episodes. Clear modal state before entering Reader, opening commerce, changing comic, signing out, losing credentials, suspension, and full cleanup.

- [ ] **Step 6: Run modal, layout, and full regression tests**

Run:

```sh
rtk cargo test -p kobo-bomtoon tests::episode_header_shows_metadata_balance_and_more_without_duplicate_title -- --exact
rtk cargo test -p kobo-bomtoon tests::synopsis_modal_pages_preserve_all_text_and_close_to_same_episode_page -- --exact
rtk cargo test -p kobo-bomtoon
```

Expected: all tests pass; every constructed header and modal page has zero Clara diagnostics errors.

- [ ] **Step 7: Commit header and synopsis behavior**

```sh
rtk git add apps/bomtoon/src/main.rs
rtk git commit -m "feat(bomtoon): add episode synopsis modal"
```

---

### Task 3: Rich Episode Rows, Dates, Status, And Stable Capacity

**Files:**
- Modify: `apps/bomtoon/src/main.rs:38-40,531-554,1017-1170,1718-1788,4304-4334,5933-5959,9250-9480,12840-12910,15500-15720`
- Test: `apps/bomtoon/src/main.rs` inline tests

**Interfaces:**
- Consumes: Task 1 `Episode.opened_at`, `Episode.thumbnail_url`; Task 2 header/modal composition; existing `taipei_date`, `cover_lead`, `rows_with_trailing`, `PurchaseState`, `page_bounds`, diagnostics, and episode action dispatch.
- Produces:
  - `episode_status(episode: &Episode, now_ms: Option<i64>) -> String`
  - `episode_rows_per_page(&self) -> usize`
  - one fixed capacity used by rendering, page count, page turns, visible thumbnail selection, and post-refresh page clamping
  - rich row action naming that leaves unsupported states inert

- [ ] **Step 1: Write failing rich-row and access-copy tests**

Add one screen test with owned, rented, free, sample, unowned, and unknown episodes. Seed a ready `TilePicture` for one thumbnail. Assert exact row fields:

```rust
assert_eq!(rows[0].title, "Owned episode");
assert_eq!(rows[0].summary, "2024-02-29");
assert_eq!(rows[0].trailing.as_deref(), Some("Owned"));
assert!(matches!(rows[0].lead, RowLead::Picture(_, Glyph::Book)));
assert_eq!(rows[1].trailing.as_deref(), Some("2 hrs"));
assert_eq!(rows[2].trailing.as_deref(), Some("Free"));
assert_eq!(rows[3].trailing.as_deref(), Some("Sample"));
assert_eq!(rows[4].trailing, None);
assert_eq!(rows[5].trailing.as_deref(), Some("FUTURE"));
```

Assert the unowned action still opens quote options, while the unknown-state action emits no task and does not enter Reader or commerce.

Add a capacity test with maximum bounded title, creators, preview, balance, rejection, and cross-account notice strings. It must return at least one row, render every page without diagnostics, preserve each episode index exactly once across pages, and stop at the final page.

- [ ] **Step 2: Run the rich-row test and verify red**

Run:

```sh
rtk cargo test -p kobo-bomtoon tests::episode_rows_show_thumbnail_date_and_truthful_access_status -- --exact
```

Expected: FAIL because Episodes still renders text/buttons without row metadata.

- [ ] **Step 3: Add exact status and date presentation helpers**

Implement:

```rust
fn episode_status(episode: &Episode, now_ms: Option<i64>) -> String {
    match episode.purchase {
        model::PurchaseState::Owned => "Owned".to_owned(),
        model::PurchaseState::Rented => now_ms
            .and_then(|now| episode.remaining_rental_hours(now))
            .map_or_else(
                || "Rented".to_owned(),
                |hours| {
                    let unit = if hours == 1 { "hr" } else { "hrs" };
                    format!("{hours} {unit}")
                },
            ),
        model::PurchaseState::Free => "Free".to_owned(),
        model::PurchaseState::Sample => "Sample".to_owned(),
        model::PurchaseState::NotOwned => String::new(),
        model::PurchaseState::Other(_) => {
            display_text(episode.purchase.label(), "Other status")
        }
    }
}
```

The parser guarantees a positive timestamp. Render `taipei_date(episode.opened_at)` and use `"Unknown date"` only for arithmetic overflow; never substitute the current date.

- [ ] **Step 4: Factor episode screen composition and measure one stable capacity**

Split rendering into:

```rust
fn episode_screen_for(&self, page: usize, rows_per_page: usize, modal: bool) -> Screen
fn episode_rows_per_page(&self) -> usize
fn episode_screen(&self) -> Screen
```

`episode_rows_per_page` tries candidates from six down to one. For each candidate, it builds every page through `episode_screen_for(page, candidate, false)` and rejects the candidate if any page has Clara diagnostic errors. This makes the chosen capacity fixed for the entire selected title rather than dependent on one page's title lengths. `episode_screen` calls the helper once, clamps its local display page, and builds the final screen with modal state.

Do not call `episode_screen` recursively from the fit probe. The lower-level builder receives `rows_per_page` explicitly.

Use the same capacity in:

- `page_bounds` for row rendering;
- page count and page position;
- Previous/Next action bounds;
- post-content-refresh page clamping;
- Task 4 visible-thumbnail URL bounds.

If repeated fit work is measurable, cache the capacity in `Bomtoon` and invalidate it only when selected metadata, episode data, balance/Gift text, rejection notice, or cross-account notice changes. Do not introduce caching before a focused profile proves it necessary.

- [ ] **Step 5: Replace button strings with rich rows**

Use one `rows_with_trailing` call for the visible range:

```rust
screen = screen.rows_with_trailing((start..end).map(|index| {
    let episode = &self.episodes[index];
    let title = display_text(&episode.title, &format!("Episode {}", episode.alias));
    let action = if episode.purchase.is_readable()
        || (episode.purchase == model::PurchaseState::NotOwned
            && !marker_belongs_to_another_account)
    {
        format!("episode-{index}")
    } else {
        format!("episode-disabled-{index}")
    };
    (
        action,
        title,
        taipei_date(episode.opened_at).unwrap_or_else(|| "Unknown date".to_owned()),
        cover_lead(&self.covers, episode.thumbnail_url.as_deref()),
        episode_status(episode, now_ms),
    )
}));
```

The dispatcher continues to match only `episode-{index}`. This preserves reader and unowned purchase actions while leaving `Other` and cross-account-locked rows inert.

- [ ] **Step 6: Run row, pagination, commerce, and full regression tests**

Run:

```sh
rtk cargo test -p kobo-bomtoon tests::episode_rows_show_thumbnail_date_and_truthful_access_status -- --exact
rtk cargo test -p kobo-bomtoon episode_pagination
rtk cargo test -p kobo-bomtoon readable_and_not_owned_episode_rows_are_actions
rtk cargo test -p kobo-bomtoon episode_commerce_summary_and_actions_fit_clara
rtk cargo test -p kobo-bomtoon
```

Expected: all tests pass. Update old assertions that expected `Read` or `View options` only where the approved row copy intentionally changed; retain action and commerce assertions.

- [ ] **Step 7: Commit rich episode rows**

```sh
rtk git add apps/bomtoon/src/main.rs
rtk git commit -m "feat(bomtoon): render rich episode rows"
```

---

### Task 4: Visible-Page Episode Thumbnail Ownership

**Files:**
- Modify: `apps/bomtoon/src/main.rs:306-333,441-452,989-1016,1033-1170,2349-2402,3070-3140,3535-3566,4384-4407,5514-5540,6023-6031,12000-12450,15000-15320`
- Test: `apps/bomtoon/src/main.rs` inline cache and episode tests

**Interfaces:**
- Consumes: Task 1 validated public `Episode.thumbnail_url`; Task 3 fixed episode capacity and rich row lead; existing `CoverCache`, `CoverTask`, `CoverSource`, `api::image`, generation checks, SDK task cap, and picture upload/drop commands.
- Produces:
  - `visible_cover_urls` and `visible_cover_source` support for `View::Episodes`
  - `episode_thumbnail_urls(&self) -> BTreeSet<String>`
  - cleanup of nonvisible episode pictures without changing shelf-cover retention
  - stale task and account cleanup coverage

- [ ] **Step 1: Write failing visible-page thumbnail scheduling tests**

Add tests that seed more episodes than one measured page and assert:

- entering Episodes requests only thumbnail URLs in page 1 bounds;
- a completed visible thumbnail replaces the glyph with `RowLead::Picture`;
- Next cancels still-loading page 1 thumbnail tasks and requests only page 2 URLs;
- a completion from page 1 after the generation change cannot install;
- failed decode/fetch keeps the row and book glyph without a page-level error;
- entering Reader or returning to Main drops ready episode pictures not shared by shelf covers;
- a URL shared by a current public shelf remains retained;
- sign-out, credential loss, suspension, and exit leave no protected episode thumbnail tasks or pictures;
- a pending foreground detail/commerce task preempts thumbnail work and resumes it only after capacity is available.

Use the existing `TINY_WEBP`, `cover_fetches`, command, and picture helpers rather than adding another fake downloader.

- [ ] **Step 2: Run the visible-page test and verify red**

Run:

```sh
rtk cargo test -p kobo-bomtoon tests::episode_thumbnails_request_only_the_visible_page -- --exact
```

Expected: FAIL because `visible_cover_urls` currently returns URLs only for `View::Main`.

- [ ] **Step 3: Extend visible URL and source calculation without a second cache**

Rewrite the early `View::Main` guard into a view match. Preserve all existing Main branches byte-for-byte in behavior and add:

```rust
View::Episodes => {
    let rows = self.episode_rows_per_page();
    let (start, end) = page_bounds(self.page, self.episodes.len(), rows);
    for episode in &self.episodes[start..end] {
        push(episode.thumbnail_url.as_ref());
    }
}
```

Return `Some(CoverSource::Protected)` for Episodes. Continue to return no source during pending foreground work, queued foreground work, problems, Account, Reader, comment views, Status, and Logout.

Do not rename the cache in this feature; `CoverCache` already owns public row pictures and a rename would add no behavior.

- [ ] **Step 4: Drop nonvisible episode pictures while preserving shelf covers**

Add:

```rust
fn episode_thumbnail_urls(&self) -> BTreeSet<String> {
    self.episodes
        .iter()
        .filter_map(|episode| episode.thumbnail_url.clone())
        .collect()
}
```

When `sync_visible_covers` changes the visible set, remove entries whose URL belongs to `episode_thumbnail_urls`, is not in the new visible set, and is not in `public_cover_urls`. Cancel Loading tasks through the existing task map; call `context.drop_picture` for Ready entries; discard Failed entries. Do not evict ordinary shelf covers through this path.

Call the same cleanup before entering Reader and when clearing selected content. Existing `retain_public_cover_cache` remains the sign-out authority and must drop protected episode pictures.

- [ ] **Step 5: Preserve stale completion and foreground priority invariants**

Keep `CoverTask.generation`, expected `CoverState::Loading(task)`, and source checks authoritative in `handle_cover_outcome`. Do not special-case task IDs or URLs in the outcome handler. Entering `Pending::Content` and any queued foreground work continues to call `preempt_cover_tasks`; `spawn_visible_covers` still refuses work while foreground or queued work exists and stops when `Context::spawn` reaches the SDK cap.

Add assertions that a stale thumbnail outcome emits no `PutPicture`, does not replace the current glyph/picture, and resumes queued foreground work before spawning more thumbnails.

- [ ] **Step 6: Run lifecycle and full regression tests**

Run:

```sh
rtk cargo test -p kobo-bomtoon tests::episode_thumbnails_request_only_the_visible_page -- --exact
rtk cargo test -p kobo-bomtoon cover_only_visible_page_urls_are_requested
rtk cargo test -p kobo-bomtoon sign_out
rtk cargo test -p kobo-bomtoon
```

Expected: all tests pass; existing Featured, Recent, Library, sign-out, task-cap, and picture cleanup behavior remains unchanged.

- [ ] **Step 7: Commit thumbnail ownership**

```sh
rtk git add apps/bomtoon/src/main.rs
rtk git commit -m "feat(bomtoon): load episode thumbnails"
```

---

### Task 5: Final Gates, Simulator Proof, And Design Status

**Files:**
- Modify only after proof: `docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md:3-5`
- Verify: all four source files and the complete BOMTOON app surface

**Interfaces:**
- Consumes: Tasks 1-4 complete implementation and tests.
- Produces: formatted, compiled, lint-clean, test-passing code; browser and runtime simulator evidence; truthful design status.

- [ ] **Step 1: Format and inspect only formatter changes**

Run:

```sh
rtk cargo fmt --all
rtk git diff --check
```

Expected: formatter succeeds; diff check prints nothing. Do not reformat unrelated files.

- [ ] **Step 2: Run focused final quality gates**

Run:

```sh
rtk cargo fmt --all -- --check
rtk cargo test -p kobo-bomtoon
rtk cargo clippy -p kobo-bomtoon --all-targets --all-features -- -D warnings
rtk cargo build --workspace
```

Expected:

- format check exits 0;
- all BOMTOON tests pass with a count greater than the 339-test baseline;
- focused Clippy exits 0 with no warnings;
- workspace build exits 0. Existing workspace warnings outside the focused Clippy package may still print during the build and must be reported as baseline warnings, not hidden.

- [ ] **Step 3: Run browser simulator smoke**

From `apps/bomtoon`, start the browser simulator:

```sh
rtk cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

Use the harness process manager for this long-running command. Exercise a signed-in comic from the supplied evidence account without spending:

1. open a comic and confirm title, creators, synopsis preview, balance line, episode thumbnail/title/date/status rows;
2. open `More`, page to the end, and close; confirm the same episode page returns;
3. page episodes forward and back; confirm only visible thumbnails populate;
4. open an owned episode and return;
5. open an unowned row to quote options, cancel without mutation, and return;
6. force or observe one thumbnail fallback and confirm no page-level error;
7. capture the screen or exact observed state needed to support the design status.

Expected: every screen fits Clara metrics, all actions return to the same selected comic/page where specified, and no purchase POST occurs.

- [ ] **Step 4: Run runtime simulator smoke**

From the worktree root, start:

```sh
rtk cargo run -p kobo-cli -- run --sim --app bomtoon
```

Use the harness process manager. Repeat the same non-spending flow. Confirm the authenticated `/detail/{alias}` HTML request produces `ssrPersonalized == true` through the managed bearer path. This is a release-blocking check: if the runtime receives unpersonalized HTML, stop and fix the request/authentication design rather than falling back to public data or the old endpoint.

Expected: the runtime simulator matches browser behavior and emits no credential/account data in logs.

- [ ] **Step 5: Update the design status truthfully**

Only after Steps 2-4 pass, replace the status text with:

```markdown
## Status

Implementation complete. Focused format, test, Clippy, workspace build, browser simulator, and runtime simulator gates pass. Both simulator flows were non-spending.
```

If either simulator cannot be exercised, do not write `Implementation complete`; record the exact pending gate instead.

- [ ] **Step 6: Commit final verification status**

```sh
rtk git add apps/bomtoon/src/api.rs apps/bomtoon/src/model.rs apps/bomtoon/src/parse.rs apps/bomtoon/src/main.rs docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md
rtk git commit -m "docs(bomtoon): record episode list proof"
```

If no source file changed after Task 4, stage only the design specification. Do not create an empty commit.

- [ ] **Step 7: Verify branch and main worktree state**

Run in the feature worktree:

```sh
rtk git status --short --branch
```

Expected: `feat/bomtoon-episode-list` with no uncommitted files.

Run in `/Users/nelson.liu/Developer/Cobalt`:

```sh
rtk git status --short
```

Expected: no output. The main worktree remains clean.
