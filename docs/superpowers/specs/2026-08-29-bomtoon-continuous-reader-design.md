# BOMTOON continuous reader pagination

## Status

Approved design, including the bounded-scaler memory revision. Implementation has not started.

## Goal

Make a plain BOMTOON episode paginate over one logical continuous vertical strip while keeping the reader responsive on Kobo-class hardware.
“Continuous” describes how source rows are packed into discrete display pages. It does not introduce scrolling, a scroll position, or scroll gestures.

The first page should appear without preparing the entire episode. Once it is visible, the reader prepares up to three following pages so ordinary forward page turns, including turns across source-image boundaries, do not show a loading screen.

This design follows and revises the image preparation, reader state, navigation, loading, and error decisions in `2026-08-28-bomtoon-episode-reader-design.md`. Authentication, episode eligibility, signed URL validation, reading chrome, and network policy remain unchanged unless this document says otherwise.

## Current behavior and root cause

The current reader paginates every manifest image independently. `page_plan` rounds each scaled source height up to a whole number of panel-height slices. `slice_rows` white-pads the final partial slice of every source. The next source always starts on another page, even when the preceding source left usable rows. The existing `row_slices_cover_source_once_and_pad_only_the_final_page` test explicitly records that behavior.

Loading is also serialized around source boundaries. `turn_reader` drops the scaled source when navigation enters another manifest image and starts that image request only after the page turn. `accept_image` then decodes, rescales, and dithers the complete tall source before it uploads one screen-height slice. This guarantees a visible loading state at an uncached boundary and performs work for rows the reader has not reached.

## Requirements

- Treat all plain manifest images as one ordered vertical strip.
- Keep the existing discrete Previous/Next page-turn controls and interaction zones.
- Let a display page contain the end of one source and the beginning of the next.
- Add white rows only below the episode's final content row.
- Display page 1 before preparing lookahead pages.
- Maintain a sliding lookahead of at most three rendered pages.
- Keep forward page turns inside the prepared window free of network requests, decoding, scaling, dithering, and loading screens.
- Bound decoded-source, rendered-page, compressed-response, runtime-picture, and task state independently of episode length.
- Accept only WebP source bodies and keep modeled BOMTOON live allocations at or below 96 MiB.
- Preserve exact manifest-dimension validation, signed URL refresh, retry, Back, stale-outcome, picture-handle ordering, and reader-chrome behavior.
- Continue to reject scrambled episodes and unsupported image dimensions.

## Non-goals

- Downloading, decoding, stitching, or uploading a whole episode eagerly.
- Persisting comic images or rendered pages across reader sessions.
- Making backward navigation as aggressively prefetched as forward navigation.
- Changing the reusable reading-surface layout, its interaction zones, or its paginated control model.
- Adding a user-facing cache or prefetch setting.
- Hiding a foreground failure when the requested page cannot be rendered.

## Continuous page plan

Manifest metadata is sufficient to plan the episode before image bodies arrive. For each source, the app computes the exact panel-width height through `kobo_image::width_scaled_size`. These heights form consecutive half-open episode intervals:

```text
source 0: [0, h0)
source 1: [h0, h0 + h1)
source 2: [h0 + h1, h0 + h1 + h2)
```

For panel height `H`, global page `p` covers:

```text
[p * H, min((p + 1) * H, total_episode_height))
```

A page plan stores the ordered intersections between that global interval and source intervals. Each segment identifies:

- the manifest source index
- the first scaled source row
- the number of rows to copy
- the destination row in the page buffer

Segment rows are contiguous and non-overlapping. Their destination ranges begin at zero and cover the page's content height exactly. Only the final global page may cover fewer than `H` content rows; its remaining destination rows stay white.

The total page count is `ceil(total_episode_height / H)`, checked against the existing `u16` wire limit. Checked addition and conversion reject cumulative-height, offset, segment, or page-count overflow. Zero panel dimensions and zero scaled source dimensions remain errors.

An image seam exactly on a global page boundary produces no mixed page: one page ends with the preceding source and the next starts with the following source. A seam one row before or after the boundary produces a one-row segment on the appropriate mixed page. These cases define the boundary tests.

## Manifest width

The manifest request uses the current reader panel width as `imageWidth` instead of the fixed value `1080`. The width is converted and validated before the request is built. Existing content and episode alias validation, bearer credential, response ceiling, Balcony headers, and exact viewer referer remain unchanged.

The response remains authoritative. Planning uses the dimensions returned by the manifest, and decoded dimensions must equal those values. BOMTOON has been observed to ignore `imageWidth` and return 1080-pixel-wide sources, so width mismatch is the normal compatibility path rather than a reason to fail.

`kobo_image::decode_webp` rejects a non-WebP body before generic image decoding. `Picture::scale_to_width` consumes the decoded grayscale picture. A same-width source returns unchanged with its allocation intact. A different width uses direct fixed-point bilinear resampling into the final grayscale target whose dimensions come from `width_scaled_size`; it allocates no full-source clone, floating-point image, or full-sized intermediate. Horizontal samples use an axis-global denominator and a reusable chunk of at most 2,048 three-`u32` entries (24,576 logical bytes), not one entry per target column. All supported panel widths fit in one chunk; wider generic outputs refill the same chunk while writing each row.
`MAX_PIXELS` bounds target area, not either axis independently; a valid 7,000,000 × 1 target therefore makes per-column scratch unsafe even though its output buffer is only 7,000,000 bytes.

Requesting panel-width assets is an optimization, not a validation shortcut. A server response with different declared and decoded dimensions still fails closed.

## Page rendering

A rendered page is a panel-width, panel-height grayscale `Picture`. Rendering starts with a white buffer and copies every planned segment into its destination rows. The app does not dither source images in advance. It dithers the assembled page once after all segments have been copied, so error diffusion crosses an image seam naturally and no segment boundary receives separate quantization treatment.

Prepared lookahead pages remain application-side grayscale pictures. They are not uploaded under runtime handles until selected. A page turn therefore uploads exactly one new picture, installs the new screen, then drops the previous handle in the existing safe order. At steady state the runtime holds only the displayed reader picture; during atomic replacement the old held picture and one bounded pending/new picture may coexist with bounded upload-chunk and protocol copies.

The page cache contains at most three entries after the current page. Entries are consecutive global page numbers. Navigating Next consumes the first cached page and starts replenishment at the far end. Navigating Previous may rerender from retained source data; it does not grow a second backward cache.

## Source loading and memory bounds

Rendering a page may require one or more manifest sources. Source bodies are fetched independently, decoded, dimension-checked, and scaled only when necessary. A source is retained only while it is needed to finish the current page or one of the three planned lookahead pages.

The implementation enforces these bounds:

- one uploaded runtime picture handle for the displayed page at steady state
- one old held picture plus one pending/new full page during atomic replacement
- three application-side lookahead page buffers, whether complete or in progress
- two combined source slots, each either an active image fetch or one decoded/panel-width-scaled source picture
- at most two image fetches at once
- at most two reader image outcomes queued in the runtime, within the existing SDK-wide maximum of four tasks
- one completed response body in an application callback, with separately bounded runtime queue, outcome/payload/frame, application frame/outcome, socket, and upload-chunk copies

If a page spans more sources than the decoded-source bound, the renderer copies a completed source's segments into the page buffer and releases that source before decoding the next. Episode length and manifest image count therefore do not multiply decoded-image memory.

A source can contribute to several page buffers before eviction. The source cache is derived from page-plan references rather than managed by an unbounded general-purpose cache. No new caching dependency is introduced.

The conservative cross-profile app model uses `M = 7,000,000` decoded pixels, `C = 4,194,304` compressed bytes, and the Elipsa three-page total `3P = 7,884,864` bytes. An oriented 8-bit WebP decode may transiently hold two RGBA images, luma-alpha, and gray (`11M`); with the callback body and one older cached gray source, `11M + C + M + 3P = 96,079,168` bytes (91.63 MiB). The scaler's source, final target, older cached source, three pages, callback body, and fixed 24,576-byte logical sample scratch are lower. Allocator arenas, stacks, libraries, and other unmodeled process memory are covered by the separate on-device high-water gate.
The repository supports the exact profiles in `kobo_profile::SUPPORTED_PROFILES`. An upstream Clara HD device tree maps 512 MiB, but installed RAM across the fleet, post-reservation Linux `MemTotal`, and per-app allowance remain unproven. The memory release gates are therefore policy, not a claim that an app owns device RAM: modeled BOMTOON live allocations must stay at or below 96 MiB, on-device BOMTOON `VmHWM` must stay at or below 128 MiB, and system `MemAvailable` must not fall below 128 MiB during the worst reader scenario.

The same-width WebP path, not lookahead pages, dominates ordinary peak memory. The former Lanczos compatibility fallback is forbidden because `image` creates a full `Rgba32F` scaling intermediate; a valid near-panel-width source can exceed the modeled allocation budget before runtime and allocator overhead.
The evidence, buffer accounting, and measurement procedure behind these limits are recorded in `docs/research/2026-08-29-kobo-memory-limits.md`.

## First paint and lookahead scheduling

Opening an episode remains manifest-first because signed image URLs and dimensions are unavailable earlier. After the manifest arrives:

1. Create the continuous page plans.
2. Fetch the source or sources required by page 1.
3. Decode, validate, and render page 1.
4. Upload and select page 1.
5. Queue an immediate zero-second maintenance task after the screen commands.

The maintenance outcome prepares pages 2 through 4. Deferring lookahead through an SDK task boundary ensures the runtime receives page 1's picture and screen commands before CPU work for later pages begins. Lookahead work never changes the visible screen.

The maintenance pass starts missing source fetches, renders every newly satisfiable consecutive page, and stops at three cached pages or a required unavailable source. Each completed prefetch repeats that bounded pass. No polling loop or recurring timer remains once the window is full.

On Next, a prepared page is promoted synchronously and another maintenance pass is queued. If the target page is not prepared, it becomes foreground work and the existing reader loading treatment is shown. This fallback preserves correctness when the user advances faster than lookahead, the network is slow, or prefetch failed.

## Task state

The current single `task` and `pending` pair cannot represent foreground and lookahead work concurrently. Reader work changes to task-ID-keyed state. Each active task records one explicit purpose:

- initial manifest
- signed manifest refresh
- foreground source fetch
- prefetch source fetch
- lookahead maintenance

Foreground task identity is tracked separately from background tasks. Only foreground work suppresses reader actions or selects the loading screen. Background prefetch and maintenance leave the current page interactive.

Every outcome removes its exact task entry before it mutates reader state. Unknown or stale task IDs remain no-ops. A completed source is accepted only when it still belongs to the active episode and is still relevant to the current or lookahead window.

Non-reader library, recent, content, and logout flows remain single-foreground-task flows. The task registry must not introduce parallel requests outside the reader.

## Signed URL refresh and failures

Decoded dimensions and asset identity retain their current checks. A manifest refresh may replace signed URLs only when image count, order, dimensions, and validated paths are unchanged.

At most one manifest refresh is active. A source that receives `Unauthorized` during foreground or prefetch work may join that refresh and be retried once with the replacement URL. Refresh-attempt state is recorded per source so concurrent or repeated failures cannot create a refresh loop.

A background prefetch failure does not replace the visible page with an error. The source is marked unavailable for lookahead, and prepared pages before it remain usable. If navigation later requires that source, the same failure becomes foreground: retry and error presentation follow the existing episode-local behavior.

A foreground manifest, fetch, decode, dimension, scale, render, or upload failure keeps the reader selection and offers Try again or Back. Retry resumes the requested global page, not merely a source index. Authentication failures on bearer manifest work retain the existing signed-out or expired-account transitions.

## Navigation and lifecycle

Reader position becomes a zero-based global page index, not a scroll offset. Page-position chrome continues to expose one-based current and total values. Previous and Next retain the existing tap interactions, remain bounded no-ops at episode ends, and hide overlay chrome after successful turns.

Back cancels every active reader task, clears task registry entries for the reader generation, drops the displayed picture handle, releases rendered pages and decoded sources, removes signed URLs, and returns to the same episode list state. Late outcomes from cancelled or prior-generation tasks cannot repopulate caches or change the screen.

Account clearing and application exit release the same state. The runtime's existing release-all behavior remains a final safety net rather than normal handle management.

## Tests

### Page planning

- Two short source images share one page with no white rows at the seam.
- A seam exactly on a page boundary creates adjacent single-source pages.
- Seams one row before and one row after a boundary create exact mixed-page segments.
- Several short sources can occupy one page without gaps, overlap, or reordering.
- Only the final episode page receives white padding.
- Total height and page-count overflow fail closed.

### Rendering

- Segment copying preserves every source row exactly once before dithering.
- Dithering runs on the assembled page, including across a source seam.
- WebP source bodies are validated before decode.
- A same-width decoded source keeps its original grayscale allocation.
- Fixed-point bilinear scaling produces exact pinned pixels for a small matrix and preserves a constant image.
- Scaling to Clara, Libra, and Elipsa panel widths agrees with `width_scaled_size`.
- A mismatched valid source width allocates only the source grayscale buffer, final target buffer, and at most 24,576 logical bytes of horizontal sample scratch.
- Scaling a valid 6,999,999 × 1 source to the maximum 7,000,000 × 1 target keeps sample scratch at 2,048 entries rather than allocating per target column.
- A decoded dimension mismatch is rejected before scaling or segment copy.

### Loading and caching

- Page 1 picture and screen commands precede the maintenance task command.
- Lookahead contains no more than three consecutive pages.
- Decoded source and concurrent fetch counts never exceed their bounds.
- Next within the prepared window uploads the cached page without spawning a foreground fetch or showing a loading screen.
- Consuming a page queues replenishment for exactly the new far-edge page.
- Very short sources render through the two-source bound by copying and evicting sequentially.
- Previous rerenders correctly without creating an unbounded backward cache.

### Memory and delivery

- Source-cache plus active-fetch slots never exceed two.
- Two simultaneous maximum image completions stay within the response queue and protocol-copy accounting.
- Page replacement permits one old runtime picture and one pending/new picture only until the new screen is selected.
- The bounded bilinear path stays below the 96 MiB modeled app-allocation gate.
- On-device high-water and available-memory measurements satisfy the release gates on the lowest-`MemTotal` supported profile.

### Failure and lifecycle

- A prefetch failure leaves the current page visible and interactive.
- Reaching the failed source promotes it to foreground retry behavior.
- Concurrent unauthorized source failures produce one manifest refresh and at most one retry per source.
- A refreshed manifest with changed asset identity is rejected.
- Back cancels manifest, source, and maintenance tasks and drops the displayed handle.
- Outcomes from cancelled tasks and a prior reader generation are ignored.
- Picture command ordering remains Put, SetScreen, then Drop.

Existing reader chrome, access, parsing, URL policy, account, retry, and layout tests remain passing. The former per-source final-padding test is replaced by continuous-strip contracts rather than retained under a renamed expectation.

## Simulator verification

Use an authenticated plain episode containing several source images with non-panel-multiple scaled heights.

Browser simulator:

- open the episode and observe page 1 before lookahead preparation completes
- verify a page containing an image seam has no white band or repeated/missing rows
- advance rapidly through at least four pages and across two source boundaries
- confirm prepared turns do not show the reader loading screen
- pause for lookahead, advance again, and confirm the window replenishes
- navigate backward across a seam and confirm exact content continuity
- show and hide reader chrome without changing page geometry
- press Back while lookahead work is active and confirm the episode list remains stable

Repeat the same reading path in the runtime simulator. Capture timing for episode selection to first `SetScreen`, and for prepared Next action to its `SetScreen`, before and after the change. These timings are diagnostic evidence, not brittle test thresholds.

On the lowest-`MemTotal` available supported device, capture `/proc/meminfo` and BOMTOON/kobod `/proc/<pid>/status` at reader open, two fetches in flight, two completions queued, decode, 1080-to-panel scaling, three pages ready, pending upload, new-picture commit, and old-handle drop. Exercise near-seven-million-pixel WebP sources and simultaneous maximum compressed responses. Release requires BOMTOON `VmHWM <= 128 MiB`, system `MemAvailable >= 128 MiB`, no swap growth, and no allocation failure or OOM.

## Focused verification commands

```sh
cargo fmt --all --check
cargo test -p kobo-image
cargo test -p kobo-bomtoon
cargo clippy -p kobo-image -p kobo-bomtoon --all-targets --all-features -- -D warnings
cargo check -p kobo-bomtoon --target armv7-unknown-linux-musleabihf
```

After focused verification and simulator evidence, run the repository-wide test and Clippy gates required by `AGENTS.md`.

## Expected files

Implementation should remain focused in:

- `apps/bomtoon/src/api.rs`
- `apps/bomtoon/src/main.rs`
- `crates/kobo-image/src/lib.rs`

Tests remain beside affected Rust code. No new dependency, fixture asset, protocol type, SDK API, or UI layout change is planned.
