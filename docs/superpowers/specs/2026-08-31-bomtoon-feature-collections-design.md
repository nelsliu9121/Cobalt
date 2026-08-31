# Bomtoon Feature Collections Design

Date: 2026-08-31
Status: Approved design

## Goal

Replace the merged Feature recommendation list with a production-informed collection feed. The feed keeps collection membership visible, previews six titles per collection, and opens a dedicated adaptive full-list view from each collection heading.

The same comic may appear in several collections. Membership carries editorial meaning and remains intact.

## Scope

Included fixed collections, in ascending priority:

| Priority | ID | Display label | Source |
|---:|---|---|---|
| 2 | `newest` | `人氣新作` | Homepage `newest` |
| 3 | `weekday` | `連載作品` | Homepage `weekDay` |
| 5 | `ranking` | `排行榜` | Ranking API |
| 7 | `most-favorited` | `最多人收藏` | Aggregate favorites API |
| 8 | `only-in-bomtoon` | `只在 Bomtoon` | Homepage `onlyBom` |
| 9 | `theme-{remote-id}` | Displayable remote theme title | Themes API |
| 10 | `freetime` | `免費看` | Freetime API |

Theme groups share priority 9, retain API response order, and appear together under the non-interactive `編輯精選` heading.

Excluded from Feature:

- Continue Watching, which remains in Recent
- Hit-tag collections
- Genre collections
- Taste recommendations

The existing Featured, Recent, and Library destinations remain unchanged.

## Sources

The design uses a hybrid source model.

### Homepage

`GET https://www.bomtoon.tw/comic/main`

The homepage response supplies:

- Banner aliases
- Newest titles
- Current weekday titles
- Only-in-Bomtoon titles

The local-day refresh boundary remains authoritative. A changed local day refreshes the homepage and updates weekday content. Re-entering Feature on the same local day does not refresh a successful snapshot.

### Ranking

```text
GET /api/balcony-api-v2/contents/main/ranking/COMIC
adultToggle=true
contentsThumbnailType=VERTICAL,MAIN,SQUARE,DETAIL,HORIZONTAL_TYPE_A
mainGenre=ALL
```

### Aggregate favorites

```text
GET /api/balcony-api-v2/contents/main/favorite/COMIC
adultToggle=true
contentsThumbnailType=VERTICAL,MAIN,SQUARE,VERTICAL_NON_ADULT
mainGenre=ALL
```

This source represents aggregate favorite counts. Its stable ID is `most-favorited`; it is unrelated to a user's personal library.

### Themes

```text
GET /api/balcony-api-v2/theme
isIncludeAdult=true
displayRange=COMIC
displayPosition=
```

Each response object becomes one collection. Its identity is `theme-{remote-id}`. The display label comes from the remote title after bounded decoding; characters without coverage in the UI text face are removed from the display label. A missing ID, duplicate ID, empty displayable label, or empty title list omits that theme. Every `theme-*` collection remains grouped under the non-interactive `編輯精選` heading.

### Freetime

```text
GET /api/balcony-api-v2/contents/main/free/COMIC
adultToggle=true
contentsFreeFilter=FREETIME
contentsThumbnailType=VERTICAL,MAIN,SQUARE,VERTICAL_NON_ADULT
mainGenre=ALL
```

## Collection Model

Each collection contains:

- Stable collection ID
- Display label
- Priority and response-order tie breaker
- Ordered comic list
- Source status

Each comic placement contains:

- Alias
- Title
- Creators
- Positive view count, when supplied
- Validated vertical thumbnail candidate
- Validated square thumbnail candidate
- Optional synopsis cached by alias

Aliases are deduplicated only inside caches. The ordered lists preserve every placement returned by each source.

View counts use compact production formatting. Examples: `999`, `1K`, `1.2K`, and `1M`. Zero or missing values produce no trailing text. Ranking currently returns zero for every `viewCount`, and aggregate favorites does not supply the field; both therefore render without a trailing value.

## Image Safety and Fitting

Banner cells request a non-adult image candidate. Collection previews prefer `VERTICAL_NON_ADULT`, then a validated vertical candidate that is safe for the title. Full-list rows prefer `SQUARE`; a validated safe fallback may be cropped into the square rectangle.

Adult metadata may be present in a list response. Adult image bytes are not requested for the Feature feed. If no safe candidate exists, the cell or row uses the book glyph.

Every Feature image uses `cover` fitting. The renderer preserves source aspect ratio, centers the image, and crops the excess axis. Existing UI pictures retain `contain` fitting by default.

## Feature Feed Layout

The feed is a sequence of measured blocks:

1. Banner row
2. Fixed collection previews in priority order
3. `編輯精選` heading
4. Dynamic theme previews in response order
5. Freetime preview

### Banner row

- First three valid banner comics
- Three equal cells in one row
- Image-only presentation
- No title or caption
- Non-adult image
- Cover crop
- Entire cell opens the comic

### Standard collection preview

- Tappable zh-TW heading
- First six returned titles
- Three rows by two columns
- Vertical image rectangle with cover crop
- One clamped title line
- One clamped creators line
- Entire card opens the comic
- Heading opens the full collection

The heading is the only collection-level control. Counts and view metrics do not appear in preview cards.

### Theme previews

`編輯精選` is a non-interactive parent heading. Each theme follows it with a tappable remote theme heading and the same `3 x 2` preview component used by fixed collections.

The feed paginator treats each collection preview as one indivisible block. It never splits the six preview cards across pages. The `編輯精選` heading remains attached to the first theme preview. If the banner row and first preview cannot share a Clara page, the banner row occupies its own page.

Feature feed pages retain the main destination navigation.

## Full Collection Layout

Selecting a collection heading opens a dedicated subview and records the originating Feature page. Back returns to that page. The subview omits the main destination navigation.

Each row contains:

- Square image rectangle with cover crop
- One clamped title line
- One clamped creators line
- Synopsis clamped to two rendered lines
- Positive compact view count at the trailing edge
- Whole-row comic action
- Book-glyph image fallback

Rows are measured with the same font metrics, line clamps, trailing width, and picture column used by rendering. Each page takes the largest consecutive prefix that fits its content area. Page size is therefore adaptive.

The pager shows the current page number. It does not claim a total until the end of the collection has been discovered.

## Synopsis Loading and Adaptive Pagination

Selected collection responses do not contain synopsis text. Public comic detail HTML exposes it through `og:description`.

Opening a full collection requests public details for the next six comic placements. Six matches the maximum Library-style row density on Clara and bounds each paging step. Results are cached by alias and shared across duplicate collection placements.

For each page:

1. Start at the stored comic offset.
2. Fetch uncached detail metadata for up to six consecutive placements through the existing task queue.
3. Parse and clamp available synopsis text.
4. Measure candidate rows in API order.
5. Select the largest prefix that fits.
6. Cache fetched overflow metadata for the next page.
7. Store the selected page's start and end offsets.

Previous uses stored page boundaries and never repaginates cached pages. Next continues from the stored end offset. Opening another collection creates a separate cursor while reusing alias metadata.

A failed detail request leaves synopsis absent. The row still renders with its list metadata and remains actionable. Reopening a page may retry missing detail metadata.

## UI Primitives

The layout belongs in `kobo-ui`. Bomtoon supplies semantic content and actions.

Required reusable additions:

- `PictureFit::Contain | Cover`, with `Contain` as the compatibility default
- A two-column media-grid node carrying picture, title, summary, and action
- Optional row description text
- Explicit title, summary, and description line limits
- Measurement and pagination functions that consume the same line limits, picture columns, and trailing widths used by layout

The UI module performs glyph measurement, line clamping, image fitting, hit testing, and page packing. Bomtoon does not estimate character widths or crop decoded pixels.

## Loading and Refresh State

A refresh batch contains the homepage and four added collection sources. Work is queued behind the existing four-task runner limit.

### Initial load

- Show one Feature activity state until every source settles.
- Commit successful, non-empty groups in stable order.
- Show one attention banner if any source failed.
- Show the retryable Feature failure screen if every source failed.

### Retry

Retry queues failed sources only. Existing successful groups remain available. A successful retry inserts the recovered group at its stable priority.

### Daily refresh

The previous snapshot remains visible while refresh work runs. When the batch settles, successful current groups replace their prior data. Failed current groups are omitted and named by the warning. Outcomes from stale generations have no effect.

Empty successful groups remain hidden.

## Lifecycle and Caching

- Collection and detail tasks carry generation and source identity.
- Suspension cancels queued and active detail work.
- Stale task outcomes do not mutate the current snapshot or page cursor.
- Public collection state and caches remain account-independent.
- Sign-out preserves public Feature data.
- Cover cache entries remain keyed by URL.
- Synopsis metadata remains keyed by alias.

## Parser Boundaries

Every new JSON parser requires `result == SUCCESS` and enforces bounds for:

- Collection count
- Theme count
- Titles per collection
- Alias, title, creators, label, and synopsis length
- Remote IDs and integer counts
- Thumbnail URL length, host, and path

Unknown response shapes fail the owning source. A malformed comic entry does not become an unvalidated action. Theme IDs must be unique inside one response.

Public detail parsing adds bounded `og:description` entity decoding. Missing descriptions are valid and produce no synopsis.

## Failure Behavior

- A collection source failure does not hide successful sources.
- A failed retry leaves the current successful snapshot intact.
- A cover fetch or decode failure uses the book glyph.
- A detail failure removes only the synopsis from that row.
- Missing or zero view counts leave trailing space unoccupied.
- Invalid theme identity or labels omit that theme.
- Empty collections do not create dead headings.

## Verification

### `kobo-ui`

Tests cover:

- Existing `Contain` behavior remains unchanged.
- `Cover` preserves aspect ratio and crops the excess axis.
- Two-column cards have independent actions and minimum touch targets.
- Preview title and creators lines clamp at one line.
- Full-list synopsis clamps at two lines.
- Trailing values reserve width in measurement and rendering.
- Feed pagination does not split preview blocks or orphan `編輯精選`.
- Adaptive pagination draws the exact prefix selected by measurement.

### Bomtoon

Tests cover:

- Fixed collection priority and dynamic theme response order
- Six preview comics in API order
- Duplicate aliases retained across groups
- Partial success, total failure, and failed-source-only retry
- Local-day refresh, weekday rollover, stale outcomes, and cancellation
- Theme identity and display-label cleaning
- Positive compact view-count boundaries
- Missing and zero view-count suppression
- Shared cover and synopsis cache reuse
- Failed synopsis loading with an actionable row
- Stable adaptive page boundaries and Previous behavior
- Back returning to the originating Feature page
- Sign-out preserving public Feature data

### Surface checks

Run focused `kobo-ui` and `kobo-bomtoon` tests. Exercise all Feature states with Clara black-and-white layout diagnostics. Use the browser simulator and runtime simulator to verify banner actions, preview cards, collection headings, adaptive full-list paging, Back, partial failure, retry, image cropping, and missing-image fallbacks.

A signed-out smoke check must confirm that every selected collection source remains public.

## Acceptance Criteria

- Feature shows three image-only banner cells with safe cover-cropped images.
- Every included fixed collection shows a tappable zh-TW heading and six `3 x 2` preview cards.
- Every `theme-*` collection appears below the non-interactive `編輯精選` heading and uses the same preview layout.
- Group headings open adaptive one-comic rows with square covers, title, creators, two synopsis lines, and optional compact view count.
- Back returns to the originating Feature feed page.
- Collection membership and API order are preserved.
- Partial source failures leave successful groups usable and retry only failed sources.
- Daily refresh, suspension, stale outcomes, and sign-out behave deterministically.
- All affected Clara layouts fit without clipping or overlapping controls.
