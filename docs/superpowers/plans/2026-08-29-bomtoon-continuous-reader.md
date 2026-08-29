# BOMTOON Continuous Reader Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render plain BOMTOON episodes as discrete screen-sized pages over one logical continuous vertical strip, preserving RGB8 on verified color profiles and using the smaller format-specific bounded window there.

**Architecture:** Manifest metadata produces global page plans whose segments may cross source seams. The completed platform color plan supplies typed WebP decode, bounded consuming scaling, typed uploads, and format metrics; BOMTOON selects its exact scheduler limits from `Context::metrics().picture_format`. A generation-scoped task registry owns a Gray8 three-page/two-source/two-fetch window or an RGB8 two-page/one-source/one-fetch window, while only the selected page is uploaded to the runtime.

**Tech Stack:** Rust 2021, `kobo-sdk::{PictureFormat, PicturePixels}` task/runtime APIs, `kobo-image::Picture`, in-module Rust tests, `AppRunner` command assertions, browser simulator, runtime simulator, attended device memory evidence.

## Global Constraints

- Complete `docs/superpowers/plans/2026-08-29-color-picture-pipeline.md` first. This plan consumes its final typed interfaces and must not recreate platform decode, scaling, protocol, cache, surface, framebuffer, or waveform work.
- The reader remains discretely paginated through existing Previous/Next taps; no scrolling, offset, or scroll gesture is added.
- Display page 1 before any maintenance or lookahead work.
- Gray8: no more than three application page buffers, two combined decoded-or-fetching source slots, two image fetches/outcomes, one uploaded runtime reader picture, and four SDK tasks in flight.
- RGB8: no more than two application page buffers, one combined decoded-or-fetching source slot, one image fetch/outcome, one uploaded runtime reader picture, and two SDK tasks in flight.
- Accept only WebP reader bodies. Modeled BOMTOON live allocation must remain at or below 96 MiB; on-device BOMTOON `VmHWM <= 128 MiB` and system `MemAvailable >= 128 MiB` are release gates.
- Gray8 conservative model: `11M + C + M + 3P = 96,079,168` bytes (91.63 MiB). RGB8 conservative model: `11M + C + 2P_rgb = 93,935,424` bytes (89.58 MiB), with `M = 7,000,000`, `C = 4,194,304`, and Libra Colour `P_rgb = 6,370,560`.
- Keep signed URL validation, exact declared/decoded dimension checks, bearer-account transitions, retry, Back, reader chrome, and `PutPicture` → `SetScreen` → `DropPicture` ordering.
- Refresh signed URLs only when image count, order, dimensions, and validated paths are unchanged. Each source gets at most one manifest-refresh retry.
- Reject scrambled episodes, zero/unsupported dimensions, cumulative-height overflow, page-count overflow, typed format mismatch, and stale task outcomes.
- Gray8 assembles a whole page and dithers exactly once. RGB8 assembles a whole page and performs no app-side color quantization or dithering.
- Do not add a dependency, persisted image cache, fixture asset, BOMTOON-specific protocol type, SDK API, UI layout, or compatibility alias.
- Keep BOMTOON changes in `apps/bomtoon/src/api.rs` and `apps/bomtoon/src/main.rs`; platform files belong to the prerequisite color plan.

---

### Task 1: Request Manifest Images at the Active Panel Width

**Files:**
- Modify: `apps/bomtoon/src/api.rs:48-60,226-256`
- Modify: `apps/bomtoon/src/main.rs:422-464`
- Test: `apps/bomtoon/src/api.rs`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: `Context::metrics().width: i32` and existing content/episode aliases.
- Produces:

```rust
pub fn api::images(content: &str, episode: &str, panel_width: u32) -> Task;
```

- [ ] **Step 1: Change the API test to require an explicit width**

Call `images("hunter_q", "ep-1", 1072)` and retain all existing credential, response-ceiling, Balcony header, and referer assertions. Require:

```rust
assert_eq!(
    url,
    "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1072"
);
```

Add runner assertions for 1072 under `CLARA_BW_METRICS` and 1264 under injected Libra Colour metrics.

- [ ] **Step 2: Run the focused test and verify the signature fails**

Run: `cargo test -p kobo-bomtoon image_manifest_uses_exact_bearer_route_headers_and_ceiling`

Expected: compilation fails because `images` does not yet accept `panel_width`.

- [ ] **Step 3: Parameterize the request without weakening security**

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

At initial and refresh call sites, convert `context.metrics().width` through `u32::try_from`, reject zero, then call `api::images`. Leave `IMAGE_MANIFEST_BYTES`, credentials, and response validation unchanged.

- [ ] **Step 4: Run API and reader-start tests**

Run: `cargo test -p kobo-bomtoon image_manifest`

Expected: all matching tests pass at both panel widths and initial manifest work remains one foreground task.

- [ ] **Step 5: Commit the width contract**

```bash
git add apps/bomtoon/src/api.rs apps/bomtoon/src/main.rs
git commit -m "perf(bomtoon): request panel-width images"
```

---

### Task 2: Plan Continuous Global Pages Without Changing Callers

**Files:**
- Modify: `apps/bomtoon/src/main.rs:61-65,1051-1155,1197-1697`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: `EpisodeImage { width, height, order, path, url }`, `kobo_image::width_scaled_size`, panel width, and panel height.
- Produces temporary pure planning interfaces while the legacy reader remains wired:

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

fn continuous_page_plan(
    images: &[EpisodeImage],
    panel_width: u32,
    panel_height: u32,
) -> Result<(Vec<PagePlan>, u16), String>;
```

- [ ] **Step 1: Add global seam planner tests**

With `panel_width = 2` and `panel_height = 4`, require two 2×2 sources to produce one page with consecutive segments:

```rust
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
```

Add seams at global rows 3, 4, and 5; several one-row sources in one page; final partial page; zero panel dimensions; cumulative height above `u32::MAX`; and page count above `u16::MAX`.

- [ ] **Step 2: Run the planner tests and verify the helper is missing**

Run: `cargo test -p kobo-bomtoon short_sources_share_a_page_without_seam_padding`

Expected: compilation fails because `continuous_page_plan` does not exist. The legacy `page_plan -> Vec<usize>` and `ReaderState::pages_per_source` remain wired throughout this task.

- [ ] **Step 3: Implement checked global interval planning**

Scale each manifest dimension with `width_scaled_size`. Build checked cumulative source starts. Use `u64::div_ceil` for page count, convert through `u16::try_from`, and create each segment from half-open interval intersection:

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

Return errors—not panics—unless every page covers destination rows contiguously from zero through `content_rows`, segments are source-ordered, and all arithmetic fits.

- [ ] **Step 4: Run planner contracts**

Run: `cargo test -p kobo-bomtoon continuous_page_plan`

Run: `cargo test -p kobo-bomtoon cumulative_height`

Expected: every boundary and overflow case passes while current reader tests remain green through the untouched legacy planner.

- [ ] **Step 5: Commit the independently green planner**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): plan continuous reader pages"
```

---

### Task 3: Assemble Typed Pages From Bounded WebP Sources

**Files:**
- Modify: `apps/bomtoon/src/main.rs`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: Task 2 plans and platform `decode_webp(bytes, format)`, `Picture::scale_to_width(self, width)`, `Picture::pixels`, and typed `PicturePixels`.
- Produces:

```rust
struct PageBuild {
    page: usize,
    format: PictureFormat,
    bytes: Vec<u8>,
    next_segment: usize,
}

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

- [ ] **Step 1: Add exact Gray8 and RGB8 assembly tests**

For Gray8, copy two short sources with row values 10 and 20 and require one output `[10, 10, 20, 20]` before dithering. Build the independent undithered page, dither it once, and compare with `finish_build` to prove page-wide diffusion.

For RGB8, copy red rows then blue rows and require:

```rust
assert_eq!(
    picture.pixels(),
    PicturePixelsRef::Rgb8(&[
        255, 0, 0, 255, 0, 0,
        0, 0, 255, 0, 0, 255,
    ])
);
```

Add final-page white padding in one-byte and three-byte formats, source/build format mismatch, wrong scaled width, truncated source bytes, and incomplete segment refusal.

- [ ] **Step 2: Run assembly tests and verify typed builds are missing**

Run: `cargo test -p kobo-bomtoon typed_page_assembly`

Expected: compilation fails because `PageBuild` and the assembler are absent.

- [ ] **Step 3: Allocate checked white page buffers**

`PageBuild::new` computes `format.byte_len(panel_width, panel_height)` and fills the complete page with 255. This produces white padding for both formats. Store byte indexes only after checked pixel-row multiplication by `format.bytes_per_pixel()`.

- [ ] **Step 4: Copy each source segment exactly once**

Require source format and width to equal the build format and panel width. Process a build only when `plan.segments[next_segment].source == source_index`. Convert source/destination rows to checked byte ranges, `copy_from_slice` once, and increment `next_segment` once. No source or page clone is allowed.

- [ ] **Step 5: Finish according to format**

Require every planned segment copied. Move `bytes` into `PicturePixels::Gray8` or `PicturePixels::Rgb8`, construct `Picture::from_pixels`, and:

```rust
if picture.format() == PictureFormat::Gray8 {
    picture.dither(PANEL_GREYS).map_err(|error| error.to_string())?;
}
```

Do not call `dither` or any color quantizer for RGB8.

- [ ] **Step 6: Add the app decode boundary**

Decode and validate in this exact order:

```rust
let decoded = kobo_image::decode_webp(bytes, reader.format)
    .map_err(|error| error.to_string())?;
if (decoded.width(), decoded.height()) != (expected.width, expected.height) {
    return Err("BOMTOON returned different comic image dimensions.".to_owned());
}
let source = decoded
    .scale_to_width(reader.panel_width)
    .map_err(|error| error.to_string())?;
```

Validate the scaled dimensions against `width_scaled_size` before copying.

- [ ] **Step 7: Run assembly and decode contracts**

Run: `cargo test -p kobo-bomtoon page_assembly`

Run: `cargo test -p kobo-bomtoon webp`

Run: `cargo test -p kobo-bomtoon format_mismatch`

Expected: Gray8 and RGB8 seams, padding, dithering policy, WebP refusal, and exact dimensions pass.

- [ ] **Step 8: Commit typed page assembly**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): assemble typed reader pages"
```

---

### Task 4: Cut Over to Format-Bounded Reader Scheduling

**Files:**
- Modify: `apps/bomtoon/src/main.rs:42-157,350-640,702-1049,1197-1815`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: Tasks 2-3 planner and assembler.
- Produces:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReaderLimits {
    pages: usize,
    source_slots: usize,
    fetches: usize,
    tasks: usize,
}

fn reader_limits(format: PictureFormat) -> ReaderLimits {
    match format {
        PictureFormat::Gray8 => ReaderLimits { pages: 3, source_slots: 2, fetches: 2, tasks: 4 },
        PictureFormat::Rgb8 => ReaderLimits { pages: 2, source_slots: 1, fetches: 1, tasks: 2 },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReaderTaskPurpose {
    Manifest,
    ForegroundSource { source: usize, page: usize },
    PrefetchSource { source: usize },
    Maintenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReaderTaskEntry {
    generation: u64,
    purpose: ReaderTaskPurpose,
}

enum PageEntry {
    Building(PageBuild),
    Ready { page: usize, picture: Picture },
}

struct ReaderState {
    generation: u64,
    format: PictureFormat,
    limits: ReaderLimits,
    panel_width: u32,
    panel_height: u32,
    images: Vec<EpisodeImage>,
    plans: Vec<PagePlan>,
    page: usize,
    total_pages: u16,
    window: VecDeque<PageEntry>,
    source_cache: BTreeMap<usize, Picture>,
    source_fetches: BTreeMap<usize, TaskId>,
    maintenance_task: Option<TaskId>,
    picture: Option<TilePicture>,
    chrome_visible: bool,
}
```

`Bomtoon` gains `reader_generation`, `reader_tasks`, and `foreground_reader_task`. Existing `pending` and `task` remain only for Library, Recent, Content, and Logout.

- [ ] **Step 1: Write format-bound and command-order tests**

For both formats, repeatedly finish tasks in reverse order and after every callback assert:

```rust
assert!(reader.window.len() <= reader.limits.pages);
assert!(reader.source_cache.len() + reader.source_fetches.len() <= reader.limits.source_slots);
assert!(reader.source_fetches.len() <= reader.limits.fetches);
assert!(app.reader_tasks.len() <= reader.limits.tasks);
```

Assert Gray8 admits exactly 3/2/2 and RGB8 exactly 2/1/1. Add a page referencing four one-row sources and prove the one-slot RGB8 path copies and evicts sequentially without gaps.

Record page-1 command indexes and require `put_index < screen_index < maintenance_spawn_index`, with maintenance `Task::Sleep { seconds: 0 }`.

- [ ] **Step 2: Run scheduler tests and verify the legacy reader fails**

Run: `cargo test -p kobo-bomtoon format_bounded_reader_window`

Expected: failure because reader work is still source/slice based and has no format limits.

- [ ] **Step 3: Make the one clean planner/state cutover**

Delete legacy `page_plan -> Vec<usize>`, rename `continuous_page_plan` to `page_plan`, update its tests, and replace `ReaderState::pages_per_source` with the global fields above in the same edit. Delete `PageLocation`, `source`, `refreshed_current_image`, `global_page`, `slices_for`, `previous_location`, `next_location`, `slice_rows`, `install_slice`, and `Retry::Slice`. Add `Retry::Page(usize)`.

`accept_manifest` reads `context.metrics().picture_format`, selects `reader_limits`, parses/plans, creates only page 0, and starts its first required source.

- [ ] **Step 4: Separate reader tasks from the non-reader singleton**

```rust
fn spawn_reader(
    &mut self,
    context: &mut Context,
    purpose: ReaderTaskPurpose,
    work: Task,
    foreground: bool,
) -> Option<TaskId>;
```

Insert the returned ID with current generation. Set `foreground_reader_task` only for foreground work. At the start of `on_task`, remove and dispatch reader IDs before the existing singleton path. A generation mismatch, unknown ID, or late cleared ID is a no-op.

- [ ] **Step 5: Install page 1 before scheduling maintenance**

```rust
fn install_page(
    &mut self,
    context: &mut Context,
    page: usize,
    picture: Picture,
) -> Result<(), String>;
```

Consume `picture.into_pixels()` in `context.put_picture`; update page/handle state; issue `SetScreen`; drop the old handle after screen selection. Keep no application clone of the installed page. Only then queue one zero-second maintenance task.

- [ ] **Step 6: Implement one bounded maintenance pass**

In page order: extend the window to `limits.pages`; copy cached sources into every relevant build; finish consecutive complete builds; evict a source as soon as no unfinished entry references it; start missing sources while both source-slot and fetch limits admit them. Stop when full, blocked on active work, or at episode end. Collect planned spawns before calling `Context::spawn` so no reader borrow crosses SDK mutation.

Before decoding a prefetch completion, require current generation and current window relevance. Remove its `source_fetches` entry first. The combined bound then guarantees one slot for the decoded source.

- [ ] **Step 7: Run first-paint and bound tests**

Run: `cargo test -p kobo-bomtoon first_page`

Run: `cargo test -p kobo-bomtoon reader_window`

Run: `cargo test -p kobo-bomtoon source_bound`

Expected: page 1 is selected before maintenance, exact Gray8/RGB8 bounds hold after every outcome, and very short sources pass sequentially through the selected bound.

- [ ] **Step 8: Commit format-bounded scheduling**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "perf(bomtoon): bound typed reader windows"
```

---

### Task 5: Turn Pages Through the Prepared Window

**Files:**
- Modify: `apps/bomtoon/src/main.rs`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: Task 4 window and maintenance scheduler.
- Produces:

```rust
fn request_reader_page(&mut self, context: &mut Context, page: usize);
fn take_ready_page(&mut self, page: usize) -> Option<Picture>;
fn rebase_window(&mut self, context: &mut Context, page: usize);
```

- [ ] **Step 1: Add prepared-turn, replenish, miss, and Previous tests for both formats**

On prepared Next assert exactly one typed `PutPicture`, one reader `SetScreen`, no fetch, no loading screen, old `DropPicture` after screen selection, and only optional zero-second maintenance. Gray8 page 0 prepares 1-3; RGB8 prepares 1-2. After advancing one page, maintenance creates only the new far-edge page.

For a miss, retain the displayed handle while showing loading. For Previous across a seam, rerender exact global content and retain no page below the selected one.

- [ ] **Step 2: Run navigation tests and verify source/slice behavior fails**

Run: `cargo test -p kobo-bomtoon prepared_page_turn`

Run: `cargo test -p kobo-bomtoon previous_rerenders`

Expected: failures until navigation uses global page numbers and typed prepared pictures.

- [ ] **Step 3: Implement synchronous prepared Next**

Calculate page ±1 with checked boundaries. `request_reader_page` first calls `take_ready_page`. On hit, install synchronously, then queue maintenance. Do not decode, scale, dither, or spawn a fetch inside the action callback.

- [ ] **Step 4: Promote or start foreground work on a miss**

Rebase at target. If the required source is already prefetching, replace its registry purpose with `ForegroundSource` and reuse the ID. Otherwise spawn it foreground. Keep the current displayed handle and set `Retry::Page(target)`. If the page requires another active source, promote it before rerendering so loading does not flicker.

- [ ] **Step 5: Rebase Previous without a backward cache**

Remove source tasks irrelevant to the requested page and its format-sized forward window. Clear page entries, create the target page first, and rebuild through foreground work. After install, prepare only its allowed following pages.

- [ ] **Step 6: Preserve paginated controls and typed upload ordering**

Keep `.page_turns(READER_PREVIOUS, READER_NEXT)`, `.reading_menu(READER_CHROME)`, current zones, hidden/overlay chrome, and boundary no-ops. Assert every page upload format equals `context.metrics().picture_format` and ordering remains `PutPicture` → `SetScreen` → `DropPicture`.

- [ ] **Step 7: Run navigation and chrome tests**

Run: `cargo test -p kobo-bomtoon page_turn`

Run: `cargo test -p kobo-bomtoon previous`

Run: `cargo test -p kobo-bomtoon chrome`

Expected: both formats turn prepared pages without loading, misses become foreground work, backward content is exact, and no scrolling control exists.

- [ ] **Step 8: Commit global navigation**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): turn prepared global pages"
```

---

### Task 6: Coordinate Signed URL Refresh and Reader Failures

**Files:**
- Modify: `apps/bomtoon/src/main.rs`
- Test: `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: task-keyed source fetches and existing `same_assets`.
- Produces:

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

`ReaderTaskPurpose` gains `ManifestRefresh`; `ReaderState` gains `refresh_task`, `refresh_waiters`, `refresh_attempted`, and `source_failures`.

- [ ] **Step 1: Add background-failure and shared-refresh tests**

Require: background unreachable leaves current screen interactive; reaching the blocked page selects `Retry::Page`; concurrent Unauthorized outcomes share one manifest fetch; each source refreshes once; a second Unauthorized cannot loop; any changed count/order/width/height/path fails `same_assets`; bearer manifest `NoCredential` and `Unauthorized` preserve account transitions. Run the concurrency case under Gray8 and RGB8 limits.

- [ ] **Step 2: Run failure tests and verify current refresh state fails**

Run: `cargo test -p kobo-bomtoon prefetch_failure`

Run: `cargo test -p kobo-bomtoon concurrent_unauthorized`

Expected: failures because current refresh state represents only one source.

- [ ] **Step 3: Isolate background failures**

Remove the failed fetch, store one `SourceFailure` only while its source remains window-relevant, and stop maintenance there. Do not set `problem`, foreground state, or loading/error screen. Convert it to `Retry::Page(page)` only when navigation needs it.

- [ ] **Step 4: Join Unauthorized outcomes to one refresh**

Mark `refresh_attempted[source]`, store its `FetchIntent`, and spawn `ManifestRefresh` only when `refresh_task` is absent. Later unauthorized sources join the same waiters map. A source already attempted becomes foreground/background failure according to its original intent.

- [ ] **Step 5: Apply only identity-preserving URLs**

On refresh completion, clear `refresh_task`, parse, require `same_assets`, and replace only `EpisodeImage::url`. Drain still-relevant waiters in source order, foreground first, while respecting `reader.limits.fetches` and `reader.limits.source_slots`; leave excess for maintenance. Refresh failure maps foreground waiters to selected-page error and background waiters to `source_failures`.

- [ ] **Step 6: Resume exactly the global page on retry**

`Retry::Page(page)` clears only failures relevant to that page, rebases its format-sized window, and starts/promotes its first source. It does not reset `refresh_attempted`.

- [ ] **Step 7: Run failure, retry, parsing, and account tests**

Run: `cargo test -p kobo-bomtoon unauthorized`

Run: `cargo test -p kobo-bomtoon failure`

Run: `cargo test -p kobo-bomtoon retry`

Run: `cargo test -p kobo-bomtoon manifest_credentials`

Expected: background failures stay invisible until needed, refresh is shared and bounded per format, and account/security behavior is unchanged.

- [ ] **Step 8: Commit coordinated failure handling**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "fix(bomtoon): bound reader URL refresh"
```

---

### Task 7: Bound Lifecycle Cleanup and Verify Memory

**Files:**
- Modify: `apps/bomtoon/src/main.rs`
- Test: `apps/bomtoon/src/main.rs`
- Evidence: attended grayscale Kobo and enabled color Kobo, if a color profile passed the platform plan

**Interfaces:**
- Consumes: all reader task/state fields.
- Produces:

```rust
fn cancel_reader(&mut self, context: &mut Context);
```

- [ ] **Step 1: Add lifecycle cleanup tests**

Start a reader with foreground source, prefetch source(s), maintenance, refresh waiters, ready/building pages, decoded cache, and uploaded handle. For Gray8 and RGB8 assert Back, suspend, offline, logout, and link loss clear every task registry/ID/map, page/source buffer, refresh state, error, and runtime picture command exactly once. Feed late outcomes afterward and assert no mutation or respawn.

- [ ] **Step 2: Run cleanup tests and verify stale work leaks**

Run: `cargo test -p kobo-bomtoon reader_cleanup`

Expected: failures until all new reader work is centralized under one cleanup method.

- [ ] **Step 3: Centralize generation invalidation and cleanup**

Increment `reader_generation` before clearing work. Remove reader entries and all per-reader task IDs, source maps, page window, waiters, failures, and foreground selection. Emit `DropPicture` once for the live handle. Do not route reader cleanup through the non-reader `task`/`pending` singleton.

- [ ] **Step 4: Prove exact modeled bounds in tests**

Add pure byte-accounting assertions:

```rust
assert_eq!(gray8_conservative_bytes(), 96_079_168);
assert_eq!(rgb8_conservative_bytes(), 93_935_424);
assert!(gray8_conservative_bytes() <= 96 * 1024 * 1024);
assert!(rgb8_conservative_bytes() <= 96 * 1024 * 1024);
```

Also assert the largest RGB8 page is `1264 * 1680 * 3 = 6_370_560` bytes and the active reader never holds more than its format's exact page/source/fetch limits.

- [ ] **Step 5: Run the complete BOMTOON suite**

Run: `cargo test -p kobo-bomtoon`

Expected: parsing, login, library, continuous planning, typed decode, scheduling, navigation, failure, lifecycle, and memory contracts all pass.

- [ ] **Step 6: Exercise both simulator formats**

Run the browser/runtime simulator at 1072×1448 Gray8 and 1264×1680 RGB8. In each: open an episode, observe page 1 before maintenance, turn through a cross-source seam, toggle chrome, go Previous, force a fetch failure/retry, and Back out. Capture screenshots. Expected: no white seam, no loading on prepared Next, exact page position, grayscale chrome, and preserved RGB on the color simulator.

- [ ] **Step 7: Measure attended device memory gates**

On an attended Gray8 Kobo, and on an enabled color Kobo if available, exercise maximum-size sources, a seam page, forward window fill, repeated Next, Previous rebuild, Unauthorized refresh, Back, and reopen. Record BOMTOON and `kobod` `VmHWM`, system `MemAvailable`, panel behavior, touch, exit, and recovery.

Pass only when BOMTOON `VmHWM <= 128 MiB`, system `MemAvailable >= 128 MiB`, actual reader collections never exceed format limits, and the panel/device safety gates pass. Device RAM size remains unknown unless separately measured; do not infer it from device-tree evidence.

- [ ] **Step 8: Commit lifecycle and memory contracts**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "test(bomtoon): enforce reader memory bounds"
```

---

### Task 8: Run Final Workspace Gates

**Files:**
- Modify only if a gate finds a defect: `apps/bomtoon/src/api.rs`, `apps/bomtoon/src/main.rs`

**Interfaces:**
- Consumes: completed platform color pipeline and BOMTOON reader.
- Produces: formatted, warning-free, workspace-compatible implementation with simulator and attended evidence.

- [ ] **Step 1: Format and verify formatting**

Run: `cargo fmt --all`

Run: `cargo fmt --all --check`

Expected: both succeed.

- [ ] **Step 2: Run focused packages**

Run: `cargo test -p kobo-bomtoon -p kobo-image -p kobo-ui -p kobo-protocol -p kobo-sdk -p kobod -p kobo-sim`

Expected: all focused tests pass.

- [ ] **Step 3: Run workspace tests**

Run: `cargo test --workspace --all-targets --all-features`

Expected: all workspace targets pass.

- [ ] **Step 4: Run Clippy as an error gate**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: no warning or error.

- [ ] **Step 5: Confirm no obsolete reader paths remain**

Search for removed symbols and require no matches in BOMTOON:

```text
PageLocation
pages_per_source
install_slice
Retry::Slice
continuous_page_plan
```

Confirm every `context.put_picture` in BOMTOON receives `PicturePixels`, and every reader limit comes from `reader.limits` rather than duplicated constants.

- [ ] **Step 6: Commit gate-driven fixes only if needed**

```bash
git add apps/bomtoon/src/api.rs apps/bomtoon/src/main.rs
git commit -m "fix(bomtoon): satisfy reader quality gates"
```

Skip this commit when the tree is already clean.
