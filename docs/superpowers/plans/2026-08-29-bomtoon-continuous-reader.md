# BOMTOON Continuous Reader Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render plain BOMTOON episodes as discrete screen-sized pages over one logical continuous vertical image strip, with first-page priority and a bounded three-page forward lookahead.

**Architecture:** Manifest metadata produces global page plans whose segments may cross source-image seams. `Bomtoon` owns a generation-scoped reader task registry, up to three application-side page builds, up to two decoded sources, and up to two image fetches; only the selected page is uploaded to the runtime. Existing non-reader requests keep their single foreground `task`/`pending` flow.

**Tech Stack:** Rust 2021, `kobo-sdk` task/runtime APIs, `kobo-image::Picture`, in-module Rust unit tests, `AppRunner` command assertions, browser simulator, runtime simulator.

## Global Constraints

- The reader remains discretely paginated through the existing Previous/Next tap interactions; no scrolling, scroll offset, or scroll gesture is added.
- Display page 1 before preparing pages 2 through 4.
- Keep no more than three application-side page buffers, two decoded or panel-width-scaled sources, two image fetches, one uploaded runtime reader picture, and four SDK tasks in flight.
- Keep signed URL validation, exact declared/decoded dimension checks, bearer-account transitions, retry, Back, reader chrome, and `PutPicture` → `SetScreen` → `DropPicture` ordering.
- Refresh signed URLs only when image count, order, dimensions, and validated paths are unchanged; each source may use at most one manifest refresh retry.
- Reject scrambled episodes, unsupported dimensions, cumulative-height overflow, page-count overflow, and stale task outcomes.
- Do not add a dependency, persisted image cache, fixture asset, protocol type, SDK API, or UI layout.
- Keep implementation in `apps/bomtoon/src/api.rs` and `apps/bomtoon/src/main.rs`; use `Picture::grey()` for checked row copies and move same-width decoded pictures directly, so `crates/kobo-image/src/lib.rs` does not need modification.

---

### Task 1: Request Manifest Images at Panel Width

**Files:**
- Modify: `apps/bomtoon/src/api.rs:48-60,226-256`
- Modify: `apps/bomtoon/src/main.rs:422-464`
- Test: `apps/bomtoon/src/api.rs`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: `Context::metrics().width: i32` and the existing content/episode aliases.
- Produces: `api::images(content: &str, episode: &str, panel_width: u32) -> Task`.

- [ ] **Step 1: Change the API test to require the supplied width**

Replace `image_manifest_uses_exact_bearer_route_headers_and_ceiling` so it calls `images("hunter_q", "ep-1", 1072)` and asserts this exact URL while retaining every existing credential, response-ceiling, header, and referer assertion:

```rust
assert_eq!(
    url,
    "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1072"
);
```

- [ ] **Step 2: Run the API test and verify the old signature fails**

Run: `cargo test -p kobo-bomtoon image_manifest_uses_exact_bearer_route_headers_and_ceiling`

Expected: compilation fails because `images` does not accept `panel_width` yet.

- [ ] **Step 3: Parameterize the manifest request without changing its security contract**

Use this complete signature and URL construction; leave the bearer credential, `IMAGE_MANIFEST_BYTES`, Balcony headers, and exact viewer referer unchanged:

```rust
pub fn images(content: &str, episode: &str, panel_width: u32) -> Task {
    let mut headers = balcony_headers();
    headers.push(Header::new(
        "x-referer",
        format!("https://www.bomtoon.tw/viewer/{content}/{episode}"),
    ));
    fetch(
        format!("{IMAGES_URL}{content}/{episode}?imageWidth={panel_width}"),
        IMAGE_MANIFEST_BYTES,
        Credential::bearer("bomtoon-access-token"),
        headers,
    )
}
```

At both initial-manifest and refresh call sites, convert `context.metrics().width` with `u32::try_from`, reject zero, and only then call `api::images`. Initial conversion failure uses `Retry::Manifest`; refresh conversion failure keeps the existing `Retry::Image(target)` until Task 3 replaces source/slice retries with global-page retries.

- [ ] **Step 4: Add a runner assertion for the Clara panel width**

In `reader_waiting_for_manifest`, destructure the spawned `Task::Fetch` and assert:

```rust
assert!(url.ends_with("/hunter_q/ep-1?imageWidth=1072"));
```

Do not weaken `only_spawn`; initial manifest work remains one foreground task.

- [ ] **Step 5: Run the focused API and reader-start tests**

Run: `cargo test -p kobo-bomtoon image_manifest`

Expected: all matching tests pass and the manifest request uses `imageWidth=1072` under `CLARA_BW_METRICS`.

- [ ] **Step 6: Commit the request contract**

```bash
git add apps/bomtoon/src/api.rs apps/bomtoon/src/main.rs
git commit -m "perf(bomtoon): request panel-width images"
```

---

### Task 2: Plan and Assemble Continuous Global Pages

**Files:**
- Modify: `apps/bomtoon/src/main.rs:61-65,1051-1155,1197-1697`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: `EpisodeImage { width, height, order, path, url }`, panel width, and panel height.
- Produces: `PageSegment`, `PagePlan`, `PageBuild`, `page_plan`, `copy_source_into_builds`, and `finish_build` with the exact shapes below.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PageSegment {
    source: usize,
    source_row: u32,
    rows: u32,
    destination_row: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PagePlan {
    segments: Vec<PageSegment>,
    content_rows: u32,
}

struct PageBuild {
    page: usize,
    grey: Vec<u8>,
    next_segment: usize,
}

fn page_plan(
    images: &[EpisodeImage],
    panel_width: u32,
    panel_height: u32,
) -> Result<(Vec<PagePlan>, u16), String>;

fn copy_source_into_builds(
    source_index: usize,
    source: &Picture,
    plans: &[PagePlan],
    builds: &mut [PageBuild],
    panel_width: u32,
    panel_height: u32,
) -> Result<(), String>;

fn finish_build(
    build: PageBuild,
    plan: &PagePlan,
    panel_width: u32,
    panel_height: u32,
) -> Result<Picture, String>;
```

- [ ] **Step 1: Replace the per-source padding test with global seam contracts**

Delete `row_slices_cover_source_once_and_pad_only_the_final_page`. Add table-driven planner tests covering these exact scaled-height cases with `panel_width = 2` and `panel_height = 4`:

```rust
#[test]
fn short_sources_share_a_page_without_seam_padding() {
    let images = episode_images(&[(2, 2), (2, 2)]);
    let (plans, total) = page_plan(&images, 2, 4).expect("continuous plan");
    assert_eq!(total, 1);
    assert_eq!(
        plans[0],
        PagePlan {
            segments: vec![
                PageSegment { source: 0, source_row: 0, rows: 2, destination_row: 0 },
                PageSegment { source: 1, source_row: 0, rows: 2, destination_row: 2 },
            ],
            content_rows: 4,
        }
    );
}
```

Add separate tests for seams at row 4, row 3, and row 5; several one-row sources in one page; final-page `content_rows < panel_height`; cumulative-height overflow; and a page count greater than `u16::MAX`. Use manifest dimensions that exercise `kobo_image::width_scaled_size` rather than precomputed planner heights.

- [ ] **Step 2: Run the planner tests and verify the old plan fails**

Run: `cargo test -p kobo-bomtoon short_sources_share_a_page_without_seam_padding`

Expected: failure because the current `page_plan` returns per-source slice counts.

- [ ] **Step 3: Implement checked global interval planning**

Build a cumulative `Vec<u32>` of source starts. Use `checked_add` for every scaled height so a total above `u32::MAX` fails closed; exercise this with 700 metadata-only sources whose panel-width scaled height is 7,000,000. Convert validated boundaries to `u64` for intersection arithmetic, calculate `u64::div_ceil(total_height.into(), panel_height.into())`, convert the page count through `u16::try_from`, and build each page from half-open intersections:

```rust
let overlap_start = page_start.max(source_start);
let overlap_end = page_end.min(source_end);
if overlap_start < overlap_end {
    segments.push(PageSegment {
        source,
        source_row: u32::try_from(overlap_start - source_start)
            .map_err(|_| "The comic source row is not supported.".to_owned())?,
        rows: u32::try_from(overlap_end - overlap_start)
            .map_err(|_| "The comic segment height is not supported.".to_owned())?,
        destination_row: u32::try_from(overlap_start - page_start)
            .map_err(|_| "The comic destination row is not supported.".to_owned())?,
    });
}
```

Reject zero panel dimensions before planning. Assert through returned errors—not panics—that segments are ordered, source and destination row arithmetic fits, and each page's destination coverage is contiguous from row zero through `content_rows`.

- [ ] **Step 4: Add assembler tests before the assembler**

Create `Picture` sources with distinct row values and assert that two short sources produce `[10, 10, 20, 20]` without a white seam. Add a final-page assertion that only rows below `content_rows` remain `255`. Add a multi-source page whose ordered source rows, before dithering, equal one concatenated vector exactly once.

For seam dithering, assemble the undithered expected page independently, call `expected.dither(PANEL_GREYS)`, call `finish_build` on the segmented build, and assert equality. This proves one page-wide error-diffusion pass rather than one pass per segment.

- [ ] **Step 5: Implement streaming row copies and page-wide dithering**

Initialize `PageBuild::grey` with checked `panel_width * panel_height` bytes set to `255`. `copy_source_into_builds` must:

1. Require `source.width() == panel_width`.
2. Process a build only when its next planned segment names `source_index`.
3. Convert source and destination row ranges to byte ranges with checked multiplication and `usize::try_from`.
4. Require the source range to fit `source.grey()` and the destination range to fit `build.grey`.
5. `copy_from_slice` once and increment `next_segment` once.

`finish_build` must require `next_segment == plan.segments.len()`, move the existing `Vec<u8>` into `Picture::from_grey`, then call `picture.dither(PANEL_GREYS)` exactly once.

- [ ] **Step 6: Run planner and assembler contracts**

Run: `cargo test -p kobo-bomtoon page_plan`

Run: `cargo test -p kobo-bomtoon seam`

Run: `cargo test -p kobo-bomtoon source_rows`

Expected: all planner, copying, padding, and dither tests pass.

- [ ] **Step 7: Commit the continuous planning unit**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): plan continuous reader pages"
```

---

### Task 3: Install Page 1 Through a Reader Task Registry

**Files:**
- Modify: `apps/bomtoon/src/main.rs:42-128,141-157,350-374,376-640,702-829,936-1049,1197-1815`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: Task 2's `Vec<PagePlan>`, `PageBuild`, copying, and finishing functions.
- Produces: generation-scoped reader task dispatch, a zero-based global reader page, and foreground page rendering.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReaderTaskPurpose {
    Manifest,
    ForegroundSource { source: usize, page: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReaderTaskEntry {
    generation: u64,
    purpose: ReaderTaskPurpose,
}

struct ReaderState {
    generation: u64,
    panel_width: u32,
    panel_height: u32,
    images: Vec<EpisodeImage>,
    plans: Vec<PagePlan>,
    page: usize,
    total_pages: u16,
    builds: VecDeque<PageBuild>,
    picture: Option<TilePicture>,
    chrome_visible: bool,
}
```

`Bomtoon` gains:

```rust
reader_generation: u64,
reader_tasks: BTreeMap<TaskId, ReaderTaskEntry>,
foreground_reader_task: Option<TaskId>,
```

Add `Task` to the `kobo_sdk` imports and add `use std::collections::{BTreeMap, VecDeque};`.

The existing `pending: Option<Pending>` and `task: Option<TaskId>` remain solely for Library, Recent, Content, and Logout.

- [ ] **Step 1: Rewrite first-page tests around global page state**

Update reader fixtures to seed `ReaderState::page` instead of `PageLocation`. Add tests that assert:

- completing a manifest starts the source needed by global page 0;
- a page spanning two sources starts the second source only after the first callback has copied and released the first;
- page chrome reports `(1, total_pages)`;
- an upload refusal selects `Retry::Page(0)` and preserves the reader selection;
- a foreground source callback with declared/decoded dimension mismatch never issues `PutPicture`;
- a decoded source already at `panel_width` is moved directly, while a different valid width follows the checked `scale_to_width` fallback.

Generate small deterministic PNG bodies with `kobo_image::encode_png_grey` inside tests; no fixture file is added.

- [ ] **Step 2: Run the first-page tests and verify they fail**

Run: `cargo test -p kobo-bomtoon first_page`

Expected: failures because reader state and source work are still keyed by `PageLocation`.

- [ ] **Step 3: Separate non-reader and reader task spawning**

Keep `spawn` for the four non-reader `Pending` variants. Add:

```rust
fn spawn_reader(
    &mut self,
    context: &mut Context,
    purpose: ReaderTaskPurpose,
    work: Task,
    foreground: bool,
) -> Option<TaskId>;
```

On success, insert `ReaderTaskEntry { generation: self.reader_generation, purpose }` under the exact returned `TaskId`; set `foreground_reader_task` only when `foreground` is true. On a foreground spawn refusal, set the reader error for the requested global page. A reader loading screen is selected only when `foreground_reader_task.is_some()`; background-reader state must not use `pending`. Update `on_action` so foreground reader work suppresses reader actions while background reader tasks leave the visible page interactive.

- [ ] **Step 4: Replace the old manifest/image/slice pipeline with page work**

`accept_manifest` must parse, plan, store the panel dimensions and generation, create one `PageBuild` for page 0, and request its first segment's source. The source callback must remove its task entry first, then:

```rust
let decoded = kobo_image::decode(bytes).map_err(|error| error.to_string())?;
if (decoded.width(), decoded.height()) != (expected.width, expected.height) {
    return Err("BOMTOON returned different comic image dimensions.".to_owned());
}
let source = if decoded.width() == reader.panel_width {
    decoded
} else {
    decoded
        .scale_to_width(reader.panel_width)
        .map_err(|error| error.to_string())?
};
```

Validate the scaled dimensions against `width_scaled_size`, copy its segment into every relevant build, and drop it before fetching a later source when the page spans more than two sources. Once page 0 is complete, call `finish_build` and install it.

- [ ] **Step 5: Preserve runtime picture ordering in `install_page`**

Replace `install_slice` with:

```rust
fn install_page(
    &mut self,
    context: &mut Context,
    page: usize,
    picture: Picture,
) -> Result<(), String>;
```

The method allocates a new handle, calls `put_picture`, updates `reader.page`, replaces `reader.picture`, hides chrome, clears the foreground error, calls `show` to issue `SetScreen`, then drops the prior handle. No application-side clone of the installed page remains. A `finish_build` or `install_page` error calls `fail_reader(Retry::Page(page), error)` so retry resumes the requested global page.

- [ ] **Step 6: Dispatch reader outcomes before the non-reader singleton path**

At the start of `on_task`, call `self.reader_tasks.remove(&task)`. If an entry exists, clear `foreground_reader_task` only when it equals `task`, reject a generation mismatch without state mutation, handle the exact purpose, and return. Unknown reader IDs and late IDs cleared by lifecycle work remain no-ops. Only after this branch should the existing `self.task`/`self.pending` path run.

- [ ] **Step 7: Remove the per-source navigation model**

Delete `PageLocation`, `pages_per_source`, `source`, `refreshed_current_image`, `global_page`, `slices_for`, `previous_location`, `next_location`, `slice_rows`, `install_slice`, and `Retry::Slice`. Replace reader retries with `Retry::Page(usize)`, and render page-position chrome with:

```rust
.page_position(
    u16::try_from(reader.page + 1).unwrap_or(reader.total_pages),
    reader.total_pages,
)
```

- [ ] **Step 8: Run first-paint and legacy reader tests**

Run: `cargo test -p kobo-bomtoon first_page`

Run: `cargo test -p kobo-bomtoon reader`

Expected: page 1 installs from global planning, dimension checks fail closed, and existing chrome/access/error contracts pass.

- [ ] **Step 9: Commit the initial global-page pipeline**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "refactor(bomtoon): key reader work by task"
```

---

### Task 4: Prepare a Bounded Three-Page Lookahead

**Files:**
- Modify: `apps/bomtoon/src/main.rs`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: Task 3's reader registry, global plans, and `install_page`.
- Produces: bounded page/source/fetch state and zero-second maintenance callbacks.

```rust
const LOOKAHEAD_PAGES: usize = 3;
const MAX_DECODED_SOURCES: usize = 2;
const MAX_IMAGE_FETCHES: usize = 2;

enum PageEntry {
    Building(PageBuild),
    Ready { page: usize, picture: Picture },
}

enum ReaderTaskPurpose {
    Manifest,
    ForegroundSource { source: usize, page: usize },
    PrefetchSource { source: usize },
    Maintenance,
}
```

Replace Task 3's foreground-only `builds` queue with `window` and add these bounded collections:

```rust
window: VecDeque<PageEntry>,
source_cache: BTreeMap<usize, Picture>,
source_fetches: BTreeMap<usize, TaskId>,
maintenance_task: Option<TaskId>,
```

- [ ] **Step 1: Add command-order and bound tests**

Add an `AppRunner` test that completes page 1 and records command indexes. Assert:

```rust
assert!(put_index < screen_index);
assert!(screen_index < maintenance_spawn_index);
assert!(matches!(maintenance_work, Task::Sleep { seconds: 0 }));
```

Add scheduler-state tests that seed many pages and many one-row sources, repeatedly complete tasks in reverse order, and assert after every callback:

```rust
assert!(reader.window.len() <= LOOKAHEAD_PAGES);
assert!(reader.source_cache.len() <= MAX_DECODED_SOURCES);
assert!(reader.source_fetches.len() <= MAX_IMAGE_FETCHES);
assert!(reader.source_cache.len() + reader.source_fetches.len() <= MAX_DECODED_SOURCES);
assert!(app.reader_tasks.len() <= 4);
```

Also assert three ready entries are consecutive `current + 1`, `current + 2`, `current + 3`.

- [ ] **Step 2: Run the lookahead tests and verify they fail**

Run: `cargo test -p kobo-bomtoon lookahead`

Expected: failures because page 1 currently ends the preparation pipeline.

- [ ] **Step 3: Queue maintenance only after page 1 is selected**

Add:

```rust
fn queue_maintenance(&mut self, context: &mut Context) {
    let already_queued = self
        .reader
        .as_ref()
        .and_then(|reader| reader.maintenance_task)
        .is_some();
    if already_queued {
        return;
    }
    let task = self.spawn_reader(
        context,
        ReaderTaskPurpose::Maintenance,
        Task::Sleep { seconds: 0 },
        false,
    );
    if let (Some(reader), Some(task)) = (self.reader.as_mut(), task) {
        reader.maintenance_task = Some(task);
    }
}
```

Call it after `install_page` has emitted `PutPicture` and `SetScreen`. The maintenance callback clears `maintenance_task` before mutating the window.

- [ ] **Step 4: Implement a single bounded maintenance pass**

Add `maintain_reader(&mut self, context: &mut Context)`. In one call it must:

1. Extend `window` with consecutive builds until it contains three pages or reaches the episode end.
2. Process cached sources in ascending manifest order, copying each into all builds that currently expect it.
3. Convert every consecutively complete build into a dithered `Ready` picture.
4. Evict a source as soon as no unfinished entry in the current window references it.
5. Start missing sources in page/segment order while `source_fetches.len() < 2` and `source_cache.len() + source_fetches.len() < 2`.
6. Stop at the first unavailable required source, three ready pages, or the fetch limit.
7. Leave no timer queued when the window is full or blocked on active network work.

Collect planned spawns into a small local `Vec<(usize, Task)>` before calling `spawn_reader`; this avoids holding a mutable `ReaderState` borrow across `Context::spawn` and registry mutation.

- [ ] **Step 5: Accept prefetch sources only while relevant**

Before decoding a completed prefetch body, require all three conditions:

```rust
entry.generation == self.reader_generation
    && self.reader.as_ref().is_some_and(|reader| reader.generation == entry.generation)
    && self.reader.as_ref().is_some_and(|reader| reader.window_references(source))
```

Remove the exact source from `source_fetches` first. The combined cache/fetch slot bound guarantees room for this callback; decode, validate, and scale at most one body, insert it into `source_cache`, then run `maintain_reader` immediately. An irrelevant completion is dropped without decoding.

- [ ] **Step 6: Prove sequential copying handles very short sources**

Add a test whose one panel page references at least four one-row sources. Complete at most two source tasks at a time, assert each source is copied into the page before eviction, and assert the final row sequence has no gaps, overlap, or reordering. This is the observable proof that the two-source bound does not limit the number of sources per page.

- [ ] **Step 7: Run lookahead and memory-bound tests**

Run: `cargo test -p kobo-bomtoon lookahead`

Run: `cargo test -p kobo-bomtoon source_bound`

Expected: page 1 precedes maintenance; page/source/fetch/task bounds hold after every outcome.

- [ ] **Step 8: Commit bounded preparation**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "perf(bomtoon): prefetch three reader pages"
```

---

### Task 5: Turn Pages Through the Prepared Window

**Files:**
- Modify: `apps/bomtoon/src/main.rs:350-374,776-829,896-918,942-1024`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: Task 4's consecutive `window` and maintenance scheduler.
- Produces: `request_reader_page`, synchronous prepared Next, foreground misses, and rerendered Previous.

```rust
fn request_reader_page(&mut self, context: &mut Context, page: usize);
fn take_ready_page(&mut self, page: usize) -> Option<Picture>;
fn rebase_window(&mut self, context: &mut Context, page: usize);
```

- [ ] **Step 1: Add prepared-turn, replenish, miss, and Previous tests**

For prepared Next, inspect the commands returned directly by `runner.action(action_id(READER_NEXT))` and assert:

- exactly one `PutPicture` and one reader `SetScreen` occur;
- no `Task::Fetch` is spawned;
- no `bomtoon-loading` screen is selected;
- the old handle is dropped after `SetScreen`;
- any spawned work is only `Task::Sleep { seconds: 0 }`.

For replenishment, start at page 0 with pages 1–3 ready, turn to page 1, complete maintenance, and assert the only newly created page number is 4.

For a cache miss, assert the current reader picture remains owned but the loading screen is selected while the required source is foreground work. For Previous, cross a source seam, rerender the prior global page, and assert the window does not retain any page below the newly displayed page.

- [ ] **Step 2: Run navigation tests and verify the source/slice path fails**

Run: `cargo test -p kobo-bomtoon prepared_page_turn`

Run: `cargo test -p kobo-bomtoon previous_rerenders`

Expected: failures until actions operate on global page numbers and cached pictures.

- [ ] **Step 3: Implement synchronous prepared Next**

`handle_reader_action` calculates `page - 1` or `page + 1` with checked bounds. `request_reader_page` first calls `take_ready_page(page)`. On a hit, call `install_page` synchronously and only afterward `queue_maintenance`. Do not call `maintain_reader`, decode, scale, dither, or spawn a fetch inside the action callback.

- [ ] **Step 4: Promote or start foreground work on a miss**

On a missing target:

1. Rebase the three-entry window at the requested page.
2. If the first required source already has a prefetch task, replace its registry purpose with `ForegroundSource { source, page }` and set `foreground_reader_task` to that existing ID.
3. Otherwise spawn that source as foreground.
4. Keep the displayed handle in `ReaderState`, set `Retry::Page(page)`, and show the existing reader loading treatment.
5. When one foreground source completes but the page needs another active source, promote the next existing task before calling `show`, so loading does not flicker back to the old page.

- [ ] **Step 5: Rebase Previous without a backward cache**

Cancel and remove source tasks that no longer belong to the requested page or its forward window. Clear application-side page entries, create the requested previous page as the first build, and render it through the foreground path. After installation, maintenance prepares its following three pages. Never retain a second `VecDeque` or pages below the displayed global page.

- [ ] **Step 6: Keep the control UX unchanged**

Retain `.page_turns(READER_PREVIOUS, READER_NEXT)`, `.reading_menu(READER_CHROME)`, and the existing interaction zones. Boundary Previous/Next remain no-ops and preserve chrome; a successful turn hides chrome. Add assertions that reader screens still use `ReadingChrome::Hidden` or `Overlay`, never a scroll control.

- [ ] **Step 7: Run reader navigation tests**

Run: `cargo test -p kobo-bomtoon page_turn`

Run: `cargo test -p kobo-bomtoon previous`

Run: `cargo test -p kobo-bomtoon chrome`

Expected: prepared turns are fetch-free/loading-free, misses are foreground, backward continuity is exact, and controls remain paginated.

- [ ] **Step 8: Commit global navigation**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): turn prepared global pages"
```

---

### Task 6: Coordinate Signed URL Refresh and Reader Failures

**Files:**
- Modify: `apps/bomtoon/src/main.rs:43-74,443-491,574-640,832-894,1026-1049`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: task-keyed source fetches and `same_assets`.
- Produces: one shared refresh, one retry per source, background failure isolation, and global-page retry.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FetchIntent {
    Foreground { page: usize },
    Prefetch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceFailure {
    advice: String,
}
```

`ReaderTaskPurpose` gains `ManifestRefresh`; `ReaderState` gains:

```rust
refresh_task: Option<TaskId>,
refresh_waiters: BTreeMap<usize, FetchIntent>,
refresh_attempted: Vec<bool>,
source_failures: BTreeMap<usize, SourceFailure>,
```

- [ ] **Step 1: Add failure and refresh concurrency tests**

Add tests for these exact transitions:

1. A prefetch `TaskError::Unreachable` leaves the current `bomtoon-reader` screen interactive and retains ready pages before the failure.
2. Navigating to the blocked page selects the reader error with `Retry::Page(target)`; pressing Try again starts that source as foreground.
3. Two concurrent source `Unauthorized` outcomes produce one manifest `Task::Fetch`, put both source indexes in `refresh_waiters`, and retry each source no more than once after refresh.
4. A second Unauthorized from a source whose `refresh_attempted[source]` is true becomes a failure and does not spawn another manifest.
5. Changed count, order, width, height, or validated path fails `same_assets` and does not replace URLs.
6. `NoCredential` and `Unauthorized` from bearer manifest work retain the existing signed-out and expired-account transitions.

- [ ] **Step 2: Run failure tests and verify current single-source refresh fails**

Run: `cargo test -p kobo-bomtoon prefetch_failure`

Run: `cargo test -p kobo-bomtoon concurrent_unauthorized`

Expected: failures because the current refresh flag and target can represent only one source.

- [ ] **Step 3: Isolate background failures**

When a prefetch fetch, decode, dimension, scale, or copy operation fails, remove its fetch entry, store `SourceFailure` only if the source is still referenced by the window, and stop maintenance at that source. Do not set `problem`, `foreground_reader_task`, or a loading/error screen. When `request_reader_page` reaches that source, convert the stored failure into `fail_reader(Retry::Page(page), advice)` without discarding already prepared earlier pages.

- [ ] **Step 4: Join Unauthorized sources to one refresh**

On the first Unauthorized source:

1. Set `refresh_attempted[source] = true`.
2. Insert its `FetchIntent` into `refresh_waiters`.
3. Spawn one `ManifestRefresh` using the stored reader `panel_width`.
4. Store its task ID in `refresh_task`.

While `refresh_task` is set, later Unauthorized sources perform steps 1 and 2 only. A source already marked attempted becomes foreground or background failure according to its original intent.

- [ ] **Step 5: Apply only identity-preserving refreshed URLs**

On refresh completion, remove `refresh_task` first, parse the manifest, require `same_assets`, and replace only each `EpisodeImage::url`. Drain `refresh_waiters` in source order and restart still-relevant sources, promoting any foreground waiter before background waiters. Enforce the two-fetch limit; leave excess relevant waiters for the next maintenance pass.

If refresh parsing, identity, or transport fails, convert foreground waiters to the selected page error and background waiters to `source_failures`. Bearer `NoCredential` and `Unauthorized` still call the existing account-clearing transitions rather than becoming episode-local background failures.

- [ ] **Step 6: Make retry resume the global page**

`Retry::Page(page)` clears only failures relevant to that page, rebases the forward window, and starts/promotes its first required source. It does not reset `refresh_attempted`, so repeated Unauthorized responses cannot loop through another manifest refresh.

- [ ] **Step 7: Run failure, retry, parsing, and account tests**

Run: `cargo test -p kobo-bomtoon unauthorized`

Run: `cargo test -p kobo-bomtoon failure`

Run: `cargo test -p kobo-bomtoon retry`

Run: `cargo test -p kobo-bomtoon manifest_credentials`

Expected: background failures leave the page interactive, foreground failures target a global page, refresh is shared and bounded, and account transitions are unchanged.

- [ ] **Step 8: Commit coordinated failure handling**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "fix(bomtoon): bound reader URL refresh"
```

---

### Task 7: Cancel Reader Generations and Release Every Resource

**Files:**
- Modify: `apps/bomtoon/src/main.rs:376-410,734-774,936-1049`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: `reader_tasks`, generation IDs, page/source caches, and displayed picture handles.
- Produces: `clear_reader(context, show_destination)` shared by Back, account clearing, and exit.

```rust
fn clear_reader(&mut self, context: &mut Context) -> Option<PictureHandle>;
```

- [ ] **Step 1: Add lifecycle and stale-outcome tests**

For the cancellation case, seed one active manifest or refresh task, two source tasks, one maintenance task, three page entries, no decoded sources, and one displayed handle. Assert `Command::Cancel(id)` for every active reader ID, an episode-list `SetScreen`, and one `DropPicture` after that screen. In a separate cleanup case, seed two decoded sources and no source fetches. Assert both cases leave all reader registry/cache/URL/build state empty.

After Back, deliver completed outcomes for every cancelled ID and assert no commands and no state mutation. Open a new episode generation, deliver an old-generation outcome, and assert the new reader remains unchanged. Repeat cleanup assertions for account clearing and `on_exit`.

- [ ] **Step 2: Run lifecycle tests and verify partial cleanup fails**

Run: `cargo test -p kobo-bomtoon reader_generation`

Run: `cargo test -p kobo-bomtoon back_cancels`

Expected: failures until every task and cache participates in cleanup.

- [ ] **Step 3: Centralize reader invalidation**

`clear_reader` must:

1. Increment `reader_generation` with `checked_add`; on overflow, clear all reader work and retain a terminal reader problem rather than reusing a generation.
2. Drain `reader_tasks` and call `context.cancel` for each exact ID.
3. Clear `foreground_reader_task`.
4. Take `ReaderState`, thereby releasing plans, signed URLs, page builds, ready pages, decoded sources, fetch maps, failures, and refresh waiters.
5. Return the displayed `PictureHandle` for its caller to drop after any replacement screen is installed.

`leave_reader` clears selection/error state, selects Episodes, calls `show`, then drops the returned handle. `clear_account_data` uses the same helper before clearing account collections. `on_exit` calls the helper and drops the returned handle without installing another screen.

- [ ] **Step 4: Keep task callbacks stale-safe**

`on_task` removes by exact ID before any state mutation. If the entry generation differs from both `reader_generation` and the active `ReaderState::generation`, return immediately. Unknown IDs—including outcomes for registry entries drained during Back—remain no-ops and must not call `show`.

- [ ] **Step 5: Reassert picture-handle ordering**

Keep and expand `successful_page_turn_hides_chrome_and_replaces_handle_in_order` to cover a prepared page. Assert command indexes satisfy:

```rust
assert!(put_index < set_screen_index);
assert!(set_screen_index < drop_index);
```

For Back, assert `SetScreen(episodes)` precedes `DropPicture`. Account clearing and exit drop each displayed handle exactly once.

- [ ] **Step 6: Run lifecycle and all BOMTOON tests**

Run: `cargo test -p kobo-bomtoon reader_generation`

Run: `cargo test -p kobo-bomtoon back`

Run: `cargo test -p kobo-bomtoon`

Expected: late outcomes are inert, all reader resources are released, and existing access/parsing/library/recent/logout/layout contracts pass.

- [ ] **Step 7: Commit lifecycle cleanup**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "fix(bomtoon): invalidate reader generations"
```

---

### Task 8: Run Quality Gates and Simulator Verification

**Files:**
- Modify only if required by formatter or a verified failure: `apps/bomtoon/src/api.rs`, `apps/bomtoon/src/main.rs`
- Verify: `crates/kobo-image/src/lib.rs` remains unchanged

**Interfaces:**
- Consumes: completed implementation from Tasks 1–7.
- Produces: focused build evidence, repository-wide gate evidence, and browser/runtime simulator observations.

- [ ] **Step 1: Run formatting and focused host gates**

```bash
cargo fmt --all --check
cargo test -p kobo-image
cargo test -p kobo-bomtoon
cargo clippy -p kobo-image -p kobo-bomtoon --all-targets --all-features -- -D warnings
```

Expected: every command exits successfully with no warning suppression.

- [ ] **Step 2: Check the Kobo target**

```bash
cargo check -p kobo-bomtoon --target armv7-unknown-linux-musleabihf
```

Expected: the application compiles for the device target without adding target-specific exceptions.

- [ ] **Step 3: Exercise the authenticated browser simulator**

From `apps/bomtoon`, run:

```bash
cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

Use an authenticated plain episode with several source images whose scaled heights are not panel multiples. Observe and record:

- page 1 is selected before lookahead CPU work begins;
- an image seam has no white band and no repeated or missing row;
- at least four rapid forward page turns cross two source boundaries;
- prepared turns never show `bomtoon-loading`;
- pausing replenishes the window and the next prepared turn stays loading-free;
- Previous crosses a seam with exact continuity;
- reader chrome toggles without changing geometry or the paginated controls;
- Back during active lookahead leaves the episode list stable after late callbacks.

Record episode selection to first `SetScreen` and prepared Next action to `SetScreen` as diagnostic before/after timings, not pass/fail thresholds.

- [ ] **Step 4: Repeat the same path in the runtime simulator**

From the repository root, run:

```bash
cargo run -p kobo-cli -- run --sim --app bomtoon
```

Repeat the exact reading path and record the same two timings. Confirm the reader is page-turn controlled and exposes no scrolling behavior.

- [ ] **Step 5: Run repository-wide required gates**

```bash
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: every workspace test passes and Clippy emits no warnings.

- [ ] **Step 6: Commit only verified formatter or gate fixes**

If Tasks 1–7 already left the tree unchanged after all gates, do not create an empty commit. If a gate required a source correction, stage only the affected BOMTOON files and use:

```bash
git add apps/bomtoon/src/api.rs apps/bomtoon/src/main.rs
git commit -m "fix(bomtoon): satisfy reader quality gates"
```
