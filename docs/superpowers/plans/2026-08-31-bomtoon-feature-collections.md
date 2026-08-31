# Bomtoon Feature Collections Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Bomtoon's flat Feature shelf with production-backed grouped previews and adaptive full-collection pages while preserving signed-out access, daily refresh, and safe image handling.

**Architecture:** Add three reusable retained-UI capabilities: per-picture contain/cover fitting, an image strip plus two-column media grid with tappable section headings, and described rows whose paginator shares the renderer's line clamps. Keep remote parsing in `parse.rs`, public request construction in `api.rs`, and put the new collection/source state machine in a focused `feature.rs` module consumed by `main.rs`.

**Tech Stack:** Rust 1.85.1, edition 2021, `kobo-ui`, `kobo-protocol`, `kobo-sdk`, `kobo-json`, SDK `Task`/`TaskOutcome`, Clara black-and-white layout diagnostics, browser simulator, runtime simulator.

## Global Constraints

- Do not add dependencies.
- Keep Feature available while signed out. Every new collection and detail request has `credential: None`.
- Use only SDK `Task`/`TaskOutcome`; do not create a network client, thread, or async runtime.
- Respect the runner's maximum of four concurrent tasks. Queue work when `Context::spawn` returns `None`.
- Fixed collection order and copy: `人氣新作` priority 2, `連載作品` priority 3, `排行榜` priority 5, `最多人收藏` priority 7, `只在 Bomtoon` priority 8, all `theme-{remote-id}` groups priority 9 under `編輯精選`, and `免費看` priority 10.
- Homepage HTML remains the source for banners, newest, weekday, and `onlyBom`. Ranking, aggregate favorites, themes, and freetime use the approved public API requests.
- Preserve API order and duplicate membership across collections. Deduplicate only detail and image caches by alias or validated URL.
- Banner and feed pictures use non-adult candidates and `PictureFit::Cover`. Existing pictures remain `PictureFit::Contain` by default.
- The initial source batch publishes only after every source and unresolved banner detail settles. Partial success publishes successful non-empty groups with a warning; Retry requests only failed sources.
- A changed local day refreshes every source while retaining the previous snapshot. Replace it atomically after the refresh settles.
- Full collection pages have no main navigation. Back restores the exact originating Feature feed page.
- Full-list detail fetches are bounded to the next six placements. Measure the largest fitting prefix only after that window settles; cache overflow details and page boundaries.
- Zero or missing view counts render no trailing value. Positive counts use `999`, `1K`, `1.2K`, and `1M` style formatting.
- Match existing cancellation, generation, stale-outcome, picture-handle, and typed-parser failure conventions.

---

## File Map

| File | Responsibility after this change |
|---|---|
| `crates/kobo-ui/src/lib.rs` | `PictureFit`, cover crop rasterization, image strip, media grid, tappable sections, described row layout, shared row measurement |
| `crates/kobo-protocol/src/lib.rs` | Closed wire encoding for picture fit, new node tags, tappable sections, and described-row fields |
| `crates/kobo-sdk/src/lib.rs` | Builder and `Context` APIs for the new retained UI primitives and paginator |
| `apps/bomtoon/src/model.rs` | Validated Feature comic, collection, theme, homepage, and public-detail values |
| `apps/bomtoon/src/api.rs` | Four new bounded public JSON requests plus existing homepage/detail requests |
| `apps/bomtoon/src/parse.rs` | Bounded list/theme parsing, safe thumbnail selection, synopsis extraction, remote label cleanup |
| `apps/bomtoon/src/feature.rs` | Source batch, snapshot, feed-block pages, detail cache, full-collection page boundaries, compact counts |
| `apps/bomtoon/src/main.rs` | Task dispatch, lifecycle, rendering, actions, cover scheduling, and end-to-end app tests |

---

### Task 1: Picture cover fitting across the UI wire

**Files:**
- Modify: `crates/kobo-ui/src/lib.rs:3292-3320, 6485-6740, 10731-10838, 11655-11683, 12161-12195`
- Modify: `crates/kobo-protocol/src/lib.rs:4832-4897, 5290-5302, 5850-5900, 6150-6222, 7280-7310`
- Modify: `crates/kobo-sdk/src/lib.rs:20-30, 3193-3226`
- Modify: `apps/bomtoon/src/main.rs` direct `TilePicture` struct literals in tests
- Test: inline `#[cfg(test)]` modules in all four files

**Interfaces:**
- Consumes: existing `TilePicture`, `PictureHandle`, `PicturePixelsRef`, `fit_within`, row leads, tile grids, screen wire encoding.
- Produces: `PictureFit::{Contain, Cover}`, `TilePicture::with_fit(PictureFit)`, centered source cropping, and exact wire round trips. Tasks 2, 3, 6, and 7 use these APIs.

- [ ] **Step 1: Write failing fit and crop tests**

Add these contracts to `kobo-ui` tests:

```rust
#[test]
fn tile_picture_defaults_to_contain_and_can_request_cover() {
    let picture = TilePicture::new(PictureHandle(7), 400, 200);
    assert_eq!(picture.fit, PictureFit::Contain);
    assert_eq!(picture.with_fit(PictureFit::Cover).fit, PictureFit::Cover);
}

#[test]
fn cover_fit_crops_the_source_center_without_stretching() {
    let source = PicturePixelsRef::Gray8(&[
        10, 20, 30, 40,
        50, 60, 70, 80,
    ]);
    let mut surface = Surface::new(2, 2);
    let bounds = surface.bounds();
    draw_fitted_picture(
        &mut surface,
        Rect { x: 0, y: 0, width: 2, height: 2 },
        (4, 2),
        source,
        bounds,
        PictureFit::Cover,
    );
    let PicturePixelsRef::Gray8(pixels) = surface.pixels() else {
        panic!("expected grayscale surface");
    };
    assert_eq!(pixels, &[20, 30, 60, 70]);
}

#[test]
fn contain_fit_keeps_the_existing_letterbox_geometry() {
    let target = Rect { x: 0, y: 0, width: 100, height: 100 };
    let fitted = fitted_picture((200, 100), target, PictureFit::Contain);
    assert_eq!(fitted.target, Rect { x: 0, y: 25, width: 100, height: 50 });
    assert_eq!(fitted.source, SourceWindow { x: 0, y: 0, width: 200, height: 100 });
}
```

Add a `kobo-protocol` round-trip test containing both fits:

```rust
#[test]
fn picture_fit_round_trips_in_tiles_rows_and_reading_surfaces() {
    for fit in [PictureFit::Contain, PictureFit::Cover] {
        let picture = TilePicture::new(PictureHandle(9), 190, 300).with_fit(fit);
        let screen = Screen::new(
            1,
            vec![
                Node::TileGrid {
                    id: NodeId(1),
                    tiles: vec![Tile::new(ActionId(1), "Cover", Glyph::Book)
                        .with_picture(picture)],
                    shape: TileShape::Portrait,
                },
                Node::Rows {
                    id: NodeId(2),
                    rows: vec![Row::new(
                        ActionId(2),
                        "Title",
                        "Creator",
                        RowLead::Picture(picture, Glyph::Book),
                    )],
                },
            ],
        )
        .with_reading_surface(Some(ReadingSurface::new(
            NodeId(3),
            picture,
            ReadingChrome::Hidden,
        )));
        assert_eq!(round_trip(screen.clone()), screen);
    }
}
```

- [ ] **Step 2: Run the new tests and verify RED**

Run:

```bash
rtk cargo test -p kobo-ui tile_picture_defaults_to_contain
rtk cargo test -p kobo-ui cover_fit_crops_the_source_center
rtk cargo test -p kobo-protocol picture_fit_round_trips_in_tiles_rows_and_reading_surfaces
```

Expected: compile failures because `PictureFit`, `TilePicture::with_fit`, `SourceWindow`, `fitted_picture`, and `draw_fitted_picture` do not exist.

- [ ] **Step 3: Add the closed fit type and preserve existing defaults**

In `kobo-ui`, replace `TilePicture` with:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PictureFit {
    #[default]
    Contain,
    Cover,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TilePicture {
    pub handle: PictureHandle,
    pub source: (u32, u32),
    pub fit: PictureFit,
}

impl TilePicture {
    #[must_use]
    pub const fn new(handle: PictureHandle, width: u32, height: u32) -> Self {
        Self {
            handle,
            source: (width, height),
            fit: PictureFit::Contain,
        }
    }

    #[must_use]
    pub const fn with_fit(mut self, fit: PictureFit) -> Self {
        self.fit = fit;
        self
    }
}
```

Re-export `PictureFit` from `kobo-sdk`. Convert every direct `TilePicture { handle, source }` literal in the workspace to `TilePicture::new(handle, source.0, source.1)` so no caller silently chooses a fit.

Add `fit: PictureFit` to `Node::Picture`; `ScreenBuilder::drawn_picture` copies `picture.fit` into it. Add the fit to `LayoutKind::Picture` and `LayoutKind::FramedPicture` so layout cannot discard the decision before rendering.

- [ ] **Step 4: Implement centered cover source windows**

Add the complete geometry contract beside `fit_within`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceWindow {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FittedPicture {
    target: Rect,
    source: SourceWindow,
}

fn fitted_picture(source: (u32, u32), target: Rect, fit: PictureFit) -> FittedPicture {
    let source_width = usize::try_from(source.0).unwrap_or(0);
    let source_height = usize::try_from(source.1).unwrap_or(0);
    if source_width == 0 || source_height == 0 || target.width <= 0 || target.height <= 0 {
        return FittedPicture {
            target: Rect { width: 0, height: 0, ..target },
            source: SourceWindow { x: 0, y: 0, width: 0, height: 0 },
        };
    }
    if fit == PictureFit::Contain {
        let (width, height) = fit_within(source, target.width, target.height);
        return FittedPicture {
            target: Rect {
                x: target.x + (target.width - width) / 2,
                y: target.y + (target.height - height) / 2,
                width,
                height,
            },
            source: SourceWindow { x: 0, y: 0, width: source_width, height: source_height },
        };
    }
    let target_width = usize::try_from(target.width).unwrap_or(0);
    let target_height = usize::try_from(target.height).unwrap_or(0);
    let (crop_width, crop_height) = if source_width.saturating_mul(target_height)
        > source_height.saturating_mul(target_width)
    {
        (source_height.saturating_mul(target_width) / target_height.max(1), source_height)
    } else {
        (source_width, source_width.saturating_mul(target_height) / target_width.max(1))
    };
    FittedPicture {
        target,
        source: SourceWindow {
            x: (source_width - crop_width) / 2,
            y: (source_height - crop_height) / 2,
            width: crop_width.max(1),
            height: crop_height.max(1),
        },
    }
}
```

Refactor the current sampler into `draw_picture_window`, adding `window.x` and `window.y` to every source lookup. `draw_fitted_picture` calls `fitted_picture` and then `draw_picture_window`. Tile, row-lead, retained picture, and reading-surface rendering pass `picture.fit`; contain remains byte-for-byte equivalent to the current path.

- [ ] **Step 5: Encode fit once for every `TilePicture` wire occurrence**

Add protocol helpers and use them for reading surfaces, tile pictures, row-lead pictures, and every decoder counterpart:

```rust
fn encode_tile_picture(output: &mut Vec<u8>, picture: TilePicture) {
    push_u32(output, picture.handle.0);
    push_u32(output, picture.source.0);
    push_u32(output, picture.source.1);
    output.push(match picture.fit {
        PictureFit::Contain => 0,
        PictureFit::Cover => 1,
    });
}

fn decode_tile_picture(reader: &mut Reader<'_>) -> Result<TilePicture, ProtocolError> {
    let handle = PictureHandle(reader.u32()?);
    let width = reader.u32()?;
    let height = reader.u32()?;
    let fit = match reader.u8()? {
        0 => PictureFit::Contain,
        1 => PictureFit::Cover,
        _ => return Err(ProtocolError::InvalidValue("picture fit")),
    };
    Ok(TilePicture::new(handle, width, height).with_fit(fit))
}
```

`Node::Picture` is encoded as separate handle/source fields today, so append the same fit tag to node tag 17 and decode it with the same `0/1` match. Do not construct an implicit `TilePicture` or lose `framed`.

Add an invalid-fit decoder test expecting `ProtocolError::InvalidValue("picture fit")`.

- [ ] **Step 6: Run focused tests and commit**

Run:

```bash
rtk cargo test -p kobo-ui picture_fit
rtk cargo test -p kobo-ui cover_fit
rtk cargo test -p kobo-protocol picture_fit
rtk cargo test -p kobo-sdk put_picture
```

Expected: all selected tests pass; existing `TilePicture::new` callers retain contain fitting.

Commit:

```bash
rtk git add crates/kobo-ui/src/lib.rs crates/kobo-protocol/src/lib.rs crates/kobo-sdk/src/lib.rs
rtk git commit -m "feat(ui): add cover picture fitting"
```

---

### Task 2: Feature feed presentation primitives

**Files:**
- Modify: `crates/kobo-ui/src/lib.rs:2744-3206, 3368-3475, 6200-6760, 9900-10250, 11200-11720`
- Modify: `crates/kobo-protocol/src/lib.rs:3590-3635, 4650-4900, 5500-6170, 7200-7500`
- Modify: `crates/kobo-sdk/src/lib.rs:444-500, 1880-1970`
- Test: inline `#[cfg(test)]` modules in all three files

**Interfaces:**
- Consumes: Task 1 `TilePicture::with_fit(PictureFit::Cover)`, existing `Tile`, `Node`, `Section`, builder action registration, layout diagnostics and hit testing.
- Produces: `Node::ImageStrip`, `Node::MediaGrid`, optional `Section.action`, `ScreenBuilder::image_strip`, `ScreenBuilder::media_grid`, and `ScreenBuilder::tappable_section`. Task 6 renders the grouped feed with them.

- [ ] **Step 1: Write failing layout, hit-test, and builder tests**

Add `kobo-ui` tests asserting:

```rust
#[test]
fn image_strip_is_three_equal_image_only_targets() {
    let screen = Screen::new(
        1,
        vec![Node::ImageStrip {
            id: NodeId(1),
            tiles: (0..3)
                .map(|index| {
                    Tile::new(ActionId(index + 1), "", Glyph::Book).with_picture(
                        TilePicture::new(PictureHandle(index + 1), 300, 500)
                            .with_fit(PictureFit::Cover),
                    )
                })
                .collect(),
        }],
    );
    let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::default());
    let targets = layout.nodes.iter().filter(|node| matches!(node.kind, LayoutKind::Tile(_, _))).collect::<Vec<_>>();
    assert_eq!(targets.len(), 3);
    assert!(targets.windows(2).all(|pair| pair[0].rect.width == pair[1].rect.width));
    assert!(layout.nodes.iter().all(|node| node.kind != LayoutKind::TileLabel));
}

#[test]
fn media_grid_places_six_cards_as_three_rows_by_two_columns() {
    let screen = Screen::new(
        1,
        vec![Node::MediaGrid {
            id: NodeId(1),
            tiles: (0..6)
                .map(|index| {
                    Tile::new(ActionId(index + 1), format!("Title {index}"), Glyph::Book)
                        .with_subtitle(format!("Creator {index}"))
                })
                .collect(),
        }],
    );
    let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::default());
    let cards = layout.nodes.iter().filter(|node| matches!(node.kind, LayoutKind::MediaCard(_))).collect::<Vec<_>>();
    assert_eq!(cards.len(), 6);
    assert_eq!(cards[0].rect.y, cards[1].rect.y);
    assert!(cards[2].rect.y > cards[0].rect.y);
    assert!(cards.iter().all(|card| card.rect.height >= CLARA_BW_METRICS.touch_target_minimum()));
}

#[test]
fn tappable_section_uses_the_heading_rect_as_its_target() {
    let action = ActionId(42);
    let screen = Screen::new(
        1,
        vec![Node::Section {
            id: NodeId(1),
            title: "人氣新作".to_owned(),
            value: None,
            action: Some(action),
        }],
    );
    let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::default());
    let section = layout.nodes.iter().find(|node| node.kind == LayoutKind::Section(Some(action))).expect("section");
    assert_eq!(layout.hit_test(section.rect.x + 1, section.rect.y + 1), Some(action));
}
```

Add SDK builder tests that inspect the retained nodes and registered actions. Add protocol round-trip tests for both new nodes and both interactive/non-interactive sections.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
rtk cargo test -p kobo-ui image_strip_is_three_equal_image_only_targets
rtk cargo test -p kobo-ui media_grid_places_six_cards_as_three_rows_by_two_columns
rtk cargo test -p kobo-ui tappable_section_uses_the_heading_rect_as_its_target
rtk cargo test -p kobo-sdk feature_feed_builders
rtk cargo test -p kobo-protocol feature_feed_nodes_round_trip
```

Expected: compile failures for the new node variants, layout kinds, and builders.

- [ ] **Step 3: Extend the retained node model without changing existing tile grids**

Change the relevant variants to:

```rust
Node::Section {
    id: NodeId,
    title: String,
    value: Option<String>,
    action: Option<ActionId>,
}
Node::ImageStrip {
    id: NodeId,
    tiles: Vec<Tile>,
}
Node::MediaGrid {
    id: NodeId,
    tiles: Vec<Tile>,
}
```

Use new node tags `31` for `ImageStrip` and `32` for `MediaGrid`. Add `action` to the existing section payload as `0 = absent`, `1 + u32 = present`. Reuse exact tile field encoding for action, label, glyph, optional picture, state, badge, and subtitle; extract `encode_tile`/`decode_tile` helpers rather than introducing a second tile wire shape.

Validation limits:

```rust
pub const MAX_IMAGE_STRIP_ITEMS: usize = 3;
pub const MAX_MEDIA_GRID_ITEMS: usize = 6;
```

Oversized retained nodes report the existing limit diagnostic and the SDK builders truncate with a builder warning. Empty nodes remain valid and zero-height.

- [ ] **Step 4: Implement fixed, non-scrolling layout geometry**

`ImageStrip`:

```rust
let columns = 3_i32;
let gutter = metrics.space(Space::Small);
let cell_width = (width - gutter * 2) / columns;
let cell_height = cell_width * TileShape::Portrait.eighths() / 8;
```

Each cell emits one enabled/disabled tile target and either a full-cell `FramedPicture(handle, PictureFit::Cover)` or centered book glyph. It emits no title/subtitle layout nodes.

`MediaGrid`:

```rust
let columns = 2_i32;
let rows = 3_i32;
let column_gap = metrics.space(Space::Small);
let row_gap = metrics.space(Space::Tight);
let cell_width = (width - column_gap) / columns;
let cell_height = max(
    metrics.touch_target_default(),
    (bottom - y - row_gap * (rows - 1)) / rows,
);
let picture_width = min(cell_width * 2 / 5, cell_height * 2 / 3);
let picture_height = min(cell_height, picture_width * TileShape::Portrait.eighths() / 8);
```

Each card emits `LayoutKind::MediaCard(action)`, a left portrait picture/glyph, one clamped caption title line, and one clamped caption summary line. The entire cell is the target. Stop before any cell whose bottom exceeds the available area so diagnostics expose a block that the app failed to paginate.

A section emits `LayoutKind::Section(action)`. Hit testing returns the optional action before non-control fallthrough. The existing visual section treatment remains unchanged.

- [ ] **Step 5: Add SDK builders with deterministic action names**

Add:

```rust
pub fn image_strip<I, N>(mut self, items: I) -> Self
where
    I: IntoIterator<Item = (N, Glyph, Option<TilePicture>)>,
    N: AsRef<str>,
```

Every supplied picture is converted with `.with_fit(PictureFit::Cover)` before it enters its `Tile`.

Add:

```rust
pub fn media_grid<I, N, T, S>(mut self, items: I) -> Self
where
    I: IntoIterator<Item = (N, T, S, Glyph, Option<TilePicture>)>,
    N: AsRef<str>,
    T: Into<String>,
    S: Into<String>,
```

Construct each tile with `Tile::new(self.register(name), title, glyph).with_subtitle(summary)` and cover-fit any supplied picture.

Add:

```rust
pub fn tappable_section(
    mut self,
    name: impl AsRef<str>,
    title: impl Into<String>,
) -> Self {
    let id = self.next_id();
    let action = self.register(name.as_ref());
    self.nodes.push(Node::Section { id, title: title.into(), value: None, action: Some(action) });
    self
}
```

Existing `section` and `section_with_value` always set `action: None`.

- [ ] **Step 6: Run focused tests and commit**

Run:

```bash
rtk cargo test -p kobo-ui image_strip
rtk cargo test -p kobo-ui media_grid
rtk cargo test -p kobo-ui tappable_section
rtk cargo test -p kobo-sdk feature_feed_builders
rtk cargo test -p kobo-protocol feature_feed_nodes
```

Expected: all pass, including Clara fit and target-size assertions.

Commit:

```bash
rtk git add crates/kobo-ui/src/lib.rs crates/kobo-protocol/src/lib.rs crates/kobo-sdk/src/lib.rs
rtk git commit -m "feat(ui): add media feed primitives"
```

---

### Task 3: Bounded described rows and shared pagination

**Files:**
- Modify: `crates/kobo-ui/src/lib.rs:3566-3640, 6368-6465, 8350-8525, 9900-10250, 11600-11700`
- Modify: `crates/kobo-protocol/src/lib.rs:4832-4851, 6150-6222, 7420-7480`
- Modify: `crates/kobo-sdk/src/lib.rs:2000-2110, 2960-3060`
- Test: inline `#[cfg(test)]` modules in all three files

**Interfaces:**
- Consumes: Task 1 cover-fit row leads and existing row/trailing/menu measurement.
- Produces: `RowLineLimits`, `Row.description`, `Row::with_description`, `Row::with_line_limits`, `ScreenBuilder::described_rows_with_trailing`, and `Context::paginate_described_rows_with_trailing`. Task 7 consumes them.

- [ ] **Step 1: Write failing row layout and paginator parity tests**

Add:

```rust
#[test]
fn described_row_clamps_title_creator_and_synopsis_to_one_one_two_lines() {
    let row = Row::new(ActionId(1), "A very long title repeated repeatedly", "Creator repeated repeatedly", Glyph::Book)
        .with_description("Synopsis repeated until it would occupy at least four lines on Clara")
        .with_line_limits(RowLineLimits::new(1, 1, 2));
    let screen = Screen::new(1, vec![Node::Rows { id: NodeId(1), rows: vec![row] }]);
    let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::default());
    assert_eq!(lines_for(&layout, LayoutKind::RowTitle), 1);
    assert_eq!(lines_for(&layout, LayoutKind::RowSummary), 1);
    assert_eq!(lines_for(&layout, LayoutKind::RowDescription), 2);
}

#[test]
fn described_row_paginator_matches_the_rows_the_layout_can_draw() {
    let rows = (0..12)
        .map(|index| (format!("Title {index}"), format!("Creator {index}"), "Two line synopsis with enough text to wrap on Clara".to_owned(), format!("{}K", index + 1)))
        .collect::<Vec<_>>();
    let borrowed = rows.iter().map(|(a, b, c, d)| (a.as_str(), b.as_str(), c.as_str(), d.as_str())).collect::<Vec<_>>();
    let pages = paginate_described_rows_with_trailing(
        &borrowed,
        RowLineLimits::new(1, 1, 2),
        &CLARA_BW_METRICS,
        CLARA_BW_METRICS.prose_area(true, false),
    );
    for page in pages {
        let screen = Screen::new(
            1,
            vec![Node::Rows {
                id: NodeId(1),
                rows: page.into_iter().map(|index| {
                    let (title, creator, synopsis, trailing) = &rows[index];
                    Row::new(ActionId(index as u32 + 1), title, creator, Glyph::Book)
                        .with_description(synopsis)
                        .with_trailing(trailing)
                        .with_line_limits(RowLineLimits::new(1, 1, 2))
                }).collect(),
            }],
        );
        assert!(!screen.diagnostics(&CLARA_BW_METRICS, &Chrome::measuring(true)).has_errors());
    }
}
```

Add SDK builder and protocol round-trip tests covering empty/non-empty descriptions and limits `0` and `1/1/2`.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
rtk cargo test -p kobo-ui described_row_clamps
rtk cargo test -p kobo-ui described_row_paginator_matches
rtk cargo test -p kobo-sdk described_rows_builder
rtk cargo test -p kobo-protocol described_rows_round_trip
```

Expected: compile failures for the new row fields and APIs.

- [ ] **Step 3: Add explicit row line limits with behavior-preserving defaults**

Use zero to mean unlimited so every existing row keeps its current wrapping:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RowLineLimits {
    pub title: u8,
    pub summary: u8,
    pub description: u8,
}

impl RowLineLimits {
    #[must_use]
    pub const fn new(title: u8, summary: u8, description: u8) -> Self {
        Self { title, summary, description }
    }
}
```

Add `description: String` and `line_limits: RowLineLimits` to `Row`; initialize them to empty/default in `Row::new`. Add:

```rust
#[must_use]
pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = description.into();
    self
}

#[must_use]
pub const fn with_line_limits(mut self, limits: RowLineLimits) -> Self {
    self.line_limits = limits;
    self
}
```

- [ ] **Step 4: Make renderer and paginator call one clamp-aware height helper**

Replace separate title/summary height calculations with:

```rust
fn limited_lines(text: &str, width: i32, size: FontSize, limit: u8) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    if limit == 0 {
        wrap_text(text, width, size)
    } else {
        wrap_text(&clamp_lines(text, width, size, usize::from(limit)), width, size)
    }
}
```

Both `layout_node(Node::Rows)` and `measured_row_height` must use this helper for title, summary, and description after `row_title_width_beside` reserves the exact lead/menu/trailing columns. Add `LayoutKind::RowDescription` below summary and draw it in the muted caption tone. The row's touch target remains at least `touch_target_default()`.

Add:

```rust
pub fn paginate_described_rows_with_trailing(
    rows: &[(&str, &str, &str, &str)],
    limits: RowLineLimits,
    metrics: &DisplayMetrics,
    area: ProseArea,
) -> Vec<Vec<usize>>
```

It passes each `(title, summary, description, trailing)` through the same measured helper with `menu = false` and `row_mark_column(metrics)`.

- [ ] **Step 5: Add SDK builder, `Context` wrapper, and wire fields**

Builder signature:

```rust
pub fn described_rows_with_trailing<I, N, T, S, D, L, V>(
    mut self,
    limits: RowLineLimits,
    rows: I,
) -> Self
where
    I: IntoIterator<Item = (N, T, S, D, L, V)>,
    N: AsRef<str>,
    T: Into<String>,
    S: Into<String>,
    D: Into<String>,
    L: Into<RowLead>,
    V: Into<String>,
```

Construct each row in this order: `Row::new`, `with_description`, optional `with_trailing`, `with_line_limits`. Preserve `MAX_ROWS` truncation warnings.

Context wrapper:

```rust
pub fn paginate_described_rows_with_trailing(
    &self,
    rows: &[(&str, &str, &str, &str)],
    limits: RowLineLimits,
    nav_bar: bool,
) -> Vec<Vec<usize>> {
    kobo_ui::paginate_described_rows_with_trailing(
        rows,
        limits,
        &self.metrics,
        self.paged_area(nav_bar),
    )
}
```

Append description string and three line-limit bytes to each encoded row. Decode invalid lengths through existing string bounds; every `u8` line limit is valid. Extend screen round-trip fixtures.

- [ ] **Step 6: Run focused tests and commit**

Run:

```bash
rtk cargo test -p kobo-ui described_row
rtk cargo test -p kobo-ui described_rows
rtk cargo test -p kobo-sdk described_rows
rtk cargo test -p kobo-protocol described_rows
```

Expected: all pass and existing row tests remain unchanged.

Commit:

```bash
rtk git add crates/kobo-ui/src/lib.rs crates/kobo-protocol/src/lib.rs crates/kobo-sdk/src/lib.rs
rtk git commit -m "feat(ui): add bounded described rows"
```

---

### Task 4: Parse and request production Feature collections

**Files:**
- Modify: `crates/kobo-ui/src/lib.rs:7675-7691` and inline tests
- Modify: `crates/kobo-sdk/src/lib.rs:20-30`
- Modify: `apps/bomtoon/src/model.rs:1-31, 232-250, 289-498`
- Modify: `apps/bomtoon/src/api.rs:5-46, 223-258, 260-725`
- Modify: `apps/bomtoon/src/parse.rs:1-31, 143-205, 756-835` and inline tests
- Modify: `apps/bomtoon/src/main.rs` interim flat-Feature model and tests

**Interfaces:**
- Consumes: approved endpoint/query strings, existing `public_image_url`, entity decoding, alias/title/creator bounds, `kobo-json` values.
- Produces: `FeatureComic`, `FeatureCollection`, `ThemeCollection`, `PublicDetail`, enriched `Homepage`, `parse::public_collection`, `parse::themes`, synopsis-aware `parse::public_detail`, and four public API task constructors. Tasks 5-7 consume them.

- [ ] **Step 1: Add parser fixtures and failing boundary tests**

Use small inline JSON/HTML fixtures, not the HAR. Cover these observable contracts:

```rust
#[test]
fn public_collection_preserves_order_membership_and_safe_images() {
    let comics = public_collection(PUBLIC_COLLECTION_RESPONSE).expect("collection");
    assert_eq!(comics.iter().map(|comic| comic.alias.as_str()).collect::<Vec<_>>(), ["shared", "second"]);
    assert_eq!(comics[0].creators, "Writer, Artist");
    assert_eq!(comics[0].view_count, Some(1_200));
    assert!(comics[0].vertical_url.as_deref().is_some_and(|url| url.contains("vertical-safe")));
    assert!(comics[0].square_url.as_deref().is_some_and(|url| url.contains("square")));
}

#[test]
fn adult_collection_item_never_exposes_a_thumbnail_url() {
    let comics = public_collection(ADULT_COLLECTION_RESPONSE).expect("collection");
    assert_eq!(comics[0].vertical_url, None);
    assert_eq!(comics[0].square_url, None);
}

#[test]
fn themes_omit_invalid_duplicate_and_empty_groups() {
    let themes = themes(THEME_RESPONSE).expect("themes");
    assert_eq!(themes.iter().map(|theme| theme.id).collect::<Vec<_>>(), [1785]);
    assert_eq!(themes[0].label, "給我一次重來的機會🕙");
    assert_eq!(themes[0].comics.len(), 6);
}

#[test]
fn public_detail_decodes_and_bounds_synopsis() {
    let detail = public_detail(DETAIL_HTML, "safe-alias").expect("detail");
    assert_eq!(detail.alias, "safe-alias");
    assert_eq!(detail.synopsis.as_deref(), Some("A & B begin again."));
}
```

Also test: non-`SUCCESS` result, over-limit arrays, invalid aliases, empty titles, overlong creators, overlong synopsis, invalid/foreign image URLs, zero view count normalization, missing `viewCount`, missing theme ID/title/list, and duplicate theme IDs.

- [ ] **Step 2: Add failing API request tests**

For `ranking`, `most_favorited`, `themes`, and `freetime`, assert exact URL/query, `credential: None`, JSON `Accept`, balcony headers, offset zero, and `max_bytes == PUBLIC_COLLECTION_BYTES`. Also keep homepage/detail as public HTML.

Run:

```bash
rtk cargo test -p kobo-bomtoon public_collection
rtk cargo test -p kobo-bomtoon themes_omit
rtk cargo test -p kobo-bomtoon public_detail_decodes
rtk cargo test -p kobo-bomtoon feature_collection_requests_are_public_and_exact
```

Expected: compile failures for the new model, parser, and API functions.

- [ ] **Step 3: Replace `ShelfComic` with the complete placement model**

Add:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureComic {
    pub alias: String,
    pub title: String,
    pub creators: String,
    pub view_count: Option<u64>,
    pub vertical_url: Option<String>,
    pub square_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureCollection {
    pub id: String,
    pub label: String,
    pub priority: u8,
    pub order: usize,
    pub comics: Vec<FeatureComic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThemeCollection {
    pub id: u64,
    pub label: String,
    pub comics: Vec<FeatureComic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicDetail {
    pub alias: String,
    pub title: String,
    pub synopsis: Option<String>,
}
```

Change `Homepage` lists to `Vec<FeatureComic>`. Keep banners as aliases. Add pure fixed-collection constructors in Task 5 rather than embedding priorities in the parser.

Keep Task 4 independently compilable: migrate the current flat Feature implementation and its fixtures from `ShelfComic` to `FeatureComic` in the same task. Its temporary cover selection is `vertical_url.as_ref().or(square_url.as_ref())`; banner fallbacks use empty creators, `None` count, and no image. Map a successful `PublicDetail` into the matching banner placement's alias/title only. Remove every `ShelfComic` import, constructor, helper, and assertion before running the Task 4 tests. Task 5 then replaces this temporary flat state with grouped collections.

- [ ] **Step 4: Add exact public JSON task constructors**

Add `PUBLIC_COLLECTION_BYTES: u32 = 512 * 1024` and:

```rust
const MAIN_API: &str = "https://www.bomtoon.tw/api/balcony-api-v2/contents/main/";
const THEME_API: &str = "https://www.bomtoon.tw/api/balcony-api-v2/theme";

pub fn ranking() -> Task {
    public_json_fetch(format!("{MAIN_API}ranking/COMIC?adultToggle=true&contentsThumbnailType=VERTICAL,MAIN,SQUARE,DETAIL,HORIZONTAL_TYPE_A&mainGenre=ALL"))
}

pub fn most_favorited() -> Task {
    public_json_fetch(format!("{MAIN_API}favorite/COMIC?adultToggle=true&contentsThumbnailType=VERTICAL,MAIN,SQUARE,VERTICAL_NON_ADULT&mainGenre=ALL"))
}

pub fn themes() -> Task {
    public_json_fetch(format!("{THEME_API}?isIncludeAdult=true&displayRange=COMIC&displayPosition="))
}

pub fn freetime() -> Task {
    public_json_fetch(format!("{MAIN_API}free/COMIC?adultToggle=true&contentsFreeFilter=FREETIME&contentsThumbnailType=VERTICAL,MAIN,SQUARE,VERTICAL_NON_ADULT&mainGenre=ALL"))
}

fn public_json_fetch(url: String) -> Task {
    Task::Fetch {
        url,
        offset: 0,
        max_bytes: PUBLIC_COLLECTION_BYTES,
        credential: None,
        headers: balcony_headers(),
    }
}
```


- [ ] **Step 5: Implement bounded collection and theme parsing**

Use `MAX_FEATURE_COMICS = 64`, `MAX_THEMES = 32`, `MAX_SYNOPSIS_BYTES = 2048`, and the existing alias/title/creator/image bounds. `public_collection` requires `result == "SUCCESS"` and a bounded `data[]`.

For each comic:

1. Validate alias, title, creators, and adult flag (`isAdult` or theme `badgeAdult`).
2. Normalize `viewCount` to `Some(value)` only when positive.
3. Select vertical in order `VERTICAL_NON_ADULT`, `VERTICAL`, `COVER`.
4. Select square as `SQUARE`, then clone the validated vertical fallback.
5. If adult is not explicitly false, expose neither image URL.
6. Skip an invalid comic rather than invalidating valid siblings; reject an over-limit containing array.

`themes` keeps response order, tracks IDs in a `BTreeSet`, trims bounded remote titles, and omits missing/empty labels or title lists. Add `kobo_ui::drawable_text_in(title, Face::Text)`, re-export it from `kobo-sdk`, and implement it as one pass through the installed typesetter's `has_glyph`; when no typesetter is installed, retain the input. Factor its predicate-taking core so a `kobo-ui` unit test rejects the clock emoji while retaining CJK text. Task 5 applies this UI-owned filter when converting parsed themes into display collections.

Extend `public_detail` to parse bounded `og:description` and stop exposing `og:image`, which has no list-level adult safety flag. A missing or empty description yields `None`; malformed title/alias remains a typed error.

- [ ] **Step 6: Run parser/API/model tests and commit**

Run:

```bash
rtk cargo test -p kobo-bomtoon parse::tests::public_collection
rtk cargo test -p kobo-bomtoon parse::tests::themes
rtk cargo test -p kobo-bomtoon parse::tests::public_detail
rtk cargo test -p kobo-bomtoon api::tests::feature_collection_requests_are_public_and_exact
rtk cargo test -p kobo-bomtoon parse::tests::theme_labels
```

Expected: all pass; fixtures prove bounds and image refusal.

Commit:

```bash
rtk git add crates/kobo-ui/src/lib.rs crates/kobo-sdk/src/lib.rs apps/bomtoon/src/model.rs apps/bomtoon/src/api.rs apps/bomtoon/src/parse.rs
rtk git commit -m "feat(bomtoon): parse Feature collections"
```

---

### Task 5: Atomic source batches, partial failure, and daily refresh

**Files:**
- Create: `apps/bomtoon/src/feature.rs`
- Modify: `apps/bomtoon/src/main.rs:1-18, 256-440, 962-1200, 3117-3508, 5979-6053`
- Test: `apps/bomtoon/src/feature.rs` inline tests and `apps/bomtoon/src/main.rs` integration-style runner tests

**Interfaces:**
- Consumes: Task 4 request/parser/model APIs, existing `LocalDay`, `Context::spawn`, task generations, cover cache, and stale refresh conventions.
- Produces: `FeatureSource`, `FeatureSnapshot`, `FeatureBatch`, `FeaturedState`, atomic publishing, failed-source retry, and source-task purposes. Tasks 6 and 7 render/use the snapshot.

- [ ] **Step 1: Write failing pure state tests**

Cover exact source order and collection order:

```rust
#[test]
fn source_batch_publishes_successful_non_empty_groups_after_every_source_settles() {
    let mut state = FeaturedState::default();
    state.begin_full_batch(None);
    state.settle(SourceResult::homepage(homepage_fixture()));
    state.settle(SourceResult::failure(FeatureSource::Ranking));
    state.settle(SourceResult::collection(FeatureSource::MostFavorited, comics("favorite", 2)));
    state.settle(SourceResult::themes(theme_fixture()));
    assert!(state.snapshot().is_none());
    state.settle(SourceResult::collection(FeatureSource::Freetime, comics("free", 2)));
    let snapshot = state.publish_ready_banner_details().expect("snapshot");
    assert_eq!(snapshot.collections.iter().map(|group| group.id.as_str()).collect::<Vec<_>>(), [
        "newest", "weekday", "most-favorited", "only-in-bomtoon", "theme-1785", "freetime",
    ]);
    assert_eq!(snapshot.failed_sources, BTreeSet::from([FeatureSource::Ranking]));
    assert!(snapshot.warning.is_some());
}

#[test]
fn failed_source_retry_keeps_snapshot_and_replaces_only_failed_slots() {
    let mut state = state_with_partial_snapshot();
    let before = state.snapshot().expect("snapshot").clone();
    assert_eq!(state.begin_failed_retry(), vec![FeatureSource::Ranking]);
    assert_eq!(state.snapshot(), Some(&before));
    state.settle(SourceResult::collection(FeatureSource::Ranking, comics("rank", 2)));
    let after = state.publish_ready_banner_details().expect("snapshot");
    assert!(after.collection("most-favorited").is_some());
    assert!(after.collection("ranking").is_some());
    assert!(after.failed_sources.is_empty());
}

#[test]
fn daily_refresh_retains_old_snapshot_until_the_new_batch_is_atomic() {
    let mut state = state_with_ready_snapshot(LocalDay::new(2026, 8, 31).expect("day"));
    let before = state.snapshot().expect("snapshot").clone();
    state.observe_day(LocalDay::new(2026, 9, 1).expect("day"));
    assert_eq!(state.snapshot(), Some(&before));
    settle_all_sources(&mut state, refreshed_results());
    assert_ne!(state.snapshot(), Some(&before));
}
```

Also test: all sources fail -> failed screen state; successful empty sources are omitted but not retryable failures; duplicate comic aliases remain in each group; themes share priority 9 and response order; unknown/stale generation is a no-op; missing banners use glyph placeholders; a banner alias found in any successful collection avoids a detail fetch.

- [ ] **Step 2: Define the focused Feature domain module**

Start `main.rs` with `mod feature;`. Put these closed types in `feature.rs`:

```rust
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FeatureSource {
    Homepage,
    Ranking,
    MostFavorited,
    Themes,
    Freetime,
}

pub const FEATURE_SOURCES: [FeatureSource; 5] = [
    FeatureSource::Homepage,
    FeatureSource::Ranking,
    FeatureSource::MostFavorited,
    FeatureSource::Themes,
    FeatureSource::Freetime,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceResult {
    Homepage(Homepage),
    Collection {
        source: FeatureSource,
        comics: Vec<FeatureComic>,
    },
    Themes(Vec<ThemeCollection>),
    Failure(FeatureSource),
}

impl SourceResult {
    pub fn homepage(homepage: Homepage) -> Self {
        Self::Homepage(homepage)
    }

    pub fn collection(source: FeatureSource, comics: Vec<FeatureComic>) -> Self {
        Self::Collection { source, comics }
    }

    pub fn themes(themes: Vec<ThemeCollection>) -> Self {
        Self::Themes(themes)
    }

    pub const fn failure(source: FeatureSource) -> Self {
        Self::Failure(source)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureSnapshot {
    pub banners: Vec<FeatureComic>,
    pub collections: Vec<FeatureCollection>,
    pub sources: BTreeMap<FeatureSource, Vec<FeatureCollection>>,
    pub failed_sources: BTreeSet<FeatureSource>,
    pub warning: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceStatus {
    Queued,
    Pending,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureBatch {
    pub generation: u64,
    pub refresh_day: Option<LocalDay>,
    pub retry_only: bool,
    pub statuses: BTreeMap<FeatureSource, SourceStatus>,
    pub queued: VecDeque<FeatureSource>,
    pub collections: BTreeMap<FeatureSource, Vec<FeatureCollection>>,
    pub banners: Vec<BannerComic>,
    pub pending_banner_aliases: VecDeque<String>,
    pub resolved_banners: BTreeMap<String, FeatureComic>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeaturedState {
    pub generation: u64,
    pub snapshot: Option<FeatureSnapshot>,
    pub batch: Option<FeatureBatch>,
    pub feed_page: usize,
    pub loaded_day: Option<LocalDay>,
    pub desired_day: Option<LocalDay>,
    pub local_day_pending: bool,
}
```

`FeatureBatch::settled()` returns true only when no source is `Queued` or `Pending` and `pending_banner_aliases` is empty. `FeaturedState::snapshot()` always returns the old snapshot during a batch.

For failed-only retry, clone `snapshot.sources` into the new batch, mark those preserved sources `Ready`, and queue only `snapshot.failed_sources`. Full refresh starts with an empty source map. Publication always rebuilds the flattened, sorted `collections` vector from the batch source map, so a failed daily source does not retain stale groups after the atomic swap.

Fixed collection conversion is exact:

```rust
fn homepage_collections(homepage: Homepage) -> Vec<FeatureCollection> {
    [
        ("newest", "人氣新作", 2, homepage.newest),
        ("weekday", "連載作品", 3, homepage.week_day),
        ("only-in-bomtoon", "只在 Bomtoon", 8, homepage.only_bom),
    ]
    .into_iter()
    .filter(|(_, _, _, comics)| !comics.is_empty())
    .enumerate()
    .map(|(order, (id, label, priority, comics))| FeatureCollection {
        id: id.to_owned(),
        label: label.to_owned(),
        priority,
        order,
        comics,
    })
    .collect()
}
```

Ranking/favorites/freetime use priorities 5/7/10. Themes filter the parsed remote label with `drawable_text_in(&theme.label, Face::Text)`, omit an empty filtered label, map to `theme-{id}` at priority 9, and preserve the response index as `order`. Final sort key is `(priority, order)`.

- [ ] **Step 3: Replace the single-homepage task purpose with source-aware purposes**

In `main.rs` use:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
enum FeatureTaskPurpose {
    Source { generation: u64, source: FeatureSource },
    BannerDetail { generation: u64, alias: String },
}
```

Rename `shelf_tasks` to `feature_tasks` and `superseded_shelf_tasks` to `superseded_feature_tasks` with LSP rename if rust-analyzer is available at execution time. If unavailable, update every textual use and let the compiler enumerate omissions.

`spawn_feature_source` matches the source to `api::homepage`, `ranking`, `most_favorited`, `themes`, or `freetime`. It marks Pending only after `Context::spawn` returns a task ID. `resume_feature_capacity` repeatedly takes the queue front until spawn returns `None`; it never maintains a second capacity count.

- [ ] **Step 4: Parse and settle each task exactly once**

Source outcome handling:

```rust
let result = match (source, outcome) {
    (FeatureSource::Homepage, TaskOutcome::Completed(bytes)) =>
        parse::homepage(&bytes).map(SourceResult::homepage),
    (FeatureSource::Themes, TaskOutcome::Completed(bytes)) =>
        parse::themes(&bytes).map(SourceResult::themes),
    (FeatureSource::Ranking | FeatureSource::MostFavorited | FeatureSource::Freetime,
     TaskOutcome::Completed(bytes)) =>
        parse::public_collection(&bytes).map(|comics| SourceResult::collection(source, comics)),
    (_, TaskOutcome::Failed(_) | TaskOutcome::Cancelled) => Err(ParseError::InvalidValue("Feature source")),
};
```

Do not persist `TaskError` text. Convert any failed transport or parser outcome to that source's `Failed` status. When all source slots settle, resolve the first three banner aliases from successful collection placements. Queue `public_detail` only for unresolved aliases to recover display text and synopsis. Whether detail succeeds or fails, an unresolved banner keeps empty creators/count and no image because detail HTML does not prove the artwork is non-adult. Detail failure does not mark Homepage failed.

Publishing rules:

1. At least one non-empty collection -> `snapshot = Some`, generic warning when failures exist.
2. No non-empty collection and at least one failed source -> failed state with Retry.
3. Successful empty sources are omitted and are not placed in `failed_sources`.
4. Retry initializes source slots from the committed snapshot, queues only `failed_sources`, and retains the snapshot until settlement.
5. Full daily refresh queues all five sources and updates `loaded_day` only after publication.

- [ ] **Step 5: Integrate start/resume/suspend/exit generation behavior**

`open_public_main` starts a full batch and requests local day. `observe_local_day` starts a full refresh only when the observed day differs from `loaded_day`; a second day observed during an active batch replaces `desired_day` and starts after current settlement.

`clear_all_state`:

```rust
self.featured.generation = self.featured.generation.wrapping_add(1);
for task in std::mem::take(&mut self.feature_tasks).into_keys() {
    context.cancel(task);
}
self.superseded_feature_tasks.clear();
self.featured.batch = None;
self.featured.local_day_pending = false;
self.featured.desired_day = None;
```

Suspension cancels collection-detail work added in Task 7 but does not discard the committed public snapshot. Exit clears it with all other process state. Stale task IDs/generations never redraw or mutate the snapshot.

- [ ] **Step 6: Add runner tests for scheduling and retries**

Use `AppRunner` to prove:

- startup spawns at most four source tasks, then starts the fifth after one outcome;
- initial feed remains Loading until all source tasks and unresolved banners settle;
- one failed source still publishes successful collections and one Retry action;
- Retry emits only the failed endpoint;
- refresh leaves old aliases visible until the last new result;
- a newer local day supersedes an older refresh deterministically;
- cancellation and stale outcomes do not publish;
- signed-out startup emits every request with no credential.

Run:

```bash
rtk cargo test -p kobo-bomtoon feature::tests
rtk cargo test -p kobo-bomtoon source_batch
rtk cargo test -p kobo-bomtoon failed_source_retry
rtk cargo test -p kobo-bomtoon daily_refresh
```

Expected: all pass.

- [ ] **Step 7: Commit the source state machine**

```bash
rtk git add apps/bomtoon/src/feature.rs apps/bomtoon/src/main.rs
rtk git commit -m "feat(bomtoon): load grouped Feature sources"
```

---

### Task 6: Render and navigate the grouped Feature feed

**Files:**
- Modify: `apps/bomtoon/src/feature.rs`
- Modify: `apps/bomtoon/src/main.rs:21-58, 394-553, 1018-1110, 1406-1496, 1652-1716, 5649-5893`
- Test: inline tests in both files

**Interfaces:**
- Consumes: Task 2 image strip/media grid/tappable sections, Task 5 committed snapshot and source retry, existing public cover cache.
- Produces: measured indivisible feed blocks, exact collection heading actions, grouped themes, page turns, feed-origin state, and visible public cover scheduling. Task 7 opens full collections from these actions.

- [ ] **Step 1: Write failing feed planning and screen tests**

Pure planner tests:

```rust
#[test]
fn feed_blocks_keep_editorial_heading_with_the_first_theme() {
    let blocks = feed_blocks(&snapshot_fixture());
    let themed = blocks
        .iter()
        .filter(|block| matches!(block, FeedBlock::ThemeWithHeading(_)))
        .count();
    assert_eq!(themed, 1);
}

#[test]
fn feed_pages_never_split_or_duplicate_a_collection_preview() {
    let snapshot = snapshot_fixture();
    let pages = feed_pages(&snapshot, &CLARA_BW_METRICS);
    let collection_blocks = pages
        .iter()
        .flat_map(|page| &page.blocks)
        .filter(|block| matches!(block, FeedBlock::Collection(_) | FeedBlock::ThemeWithHeading(_)))
        .count();
    assert_eq!(collection_blocks, snapshot.collections.len());
    assert!(pages.iter().all(|page| page_fits(page, &snapshot, &CLARA_BW_METRICS)));
}
```

Runner/screen tests must assert:

- first page has up to three `ImageStrip` targets and no banner labels;
- each rendered collection has one tappable zh-TW section and one six-card `MediaGrid`;
- fixed order includes `只在 Bomtoon` before `編輯精選`;
- exactly one non-interactive `編輯精選` section precedes the first `theme-*` heading;
- theme headings are tappable and remain in response order;
- card actions open the comic path, heading actions open collection path;
- page position and page turns match measured feed pages;
- every Clara page has no clipping, overlap, unsupported-character, or target-size errors.

- [ ] **Step 2: Define indivisible feed blocks and measured pages**

In `feature.rs`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedBlock {
    Banners,
    Collection(usize),
    ThemeWithHeading(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedPage {
    pub blocks: Vec<FeedBlock>,
}
```

`feed_blocks` emits Banners only when non-empty. It emits fixed collections by sorted snapshot order. The first priority-9 collection becomes `ThemeWithHeading`; later themes are `Collection`. Freetime follows all themes.

`feed_pages` grows one candidate page at a time. Build the exact candidate with a shared `add_feed_blocks` helper, add top bar, nav bar, and page position, then call `diagnostics(metrics, &Chrome::measuring(true))`. If adding a block creates an error, commit the previous non-empty page and retry the block on a fresh page. A block that cannot fit alone is a test failure; never split its six cards. Banner plus first preview may share a page when diagnostics allow it.

- [ ] **Step 3: Render exact banner and preview contents**

Use stable action names:

```rust
fn collection_action(id: &str) -> String { format!("feature-collection-{id}") }
fn comic_action(collection: &str, index: usize) -> String {
    format!("feature-comic-{collection}-{index}")
}
```

Banner cells use `feature-banner-{index}`, first three placements, book fallback, and `vertical_url` before `square_url`. They render through `image_strip` and contain no text.

Every collection block:

```rust
screen
    .tappable_section(collection_action(&collection.id), collection.label.clone())
    .media_grid(collection.comics.iter().take(6).enumerate().map(|(index, comic)| {
        (
            comic_action(&collection.id, index),
            display_text(&comic.title, &format!("BOMTOON {}", comic.alias)),
            display_text(&comic.creators, ""),
            Glyph::Book,
            ready_cover(covers, comic.vertical_url.as_deref()),
        )
    }))
```

Before the first theme, insert `.section("編輯精選")`, then its tappable remote label. Fixed labels are exact literals from Global Constraints.

- [ ] **Step 4: Route feed actions without flattening membership**

For a heading action, locate the collection by ID, store `origin_feed_page = featured.feed_page`, and call Task 7's `open_collection`.

For a preview card, locate that collection and placement index directly. Do not flatten or deduplicate groups. Pass alias/title to a refactored `open_selected_comic(context, alias, title, pending_index)` helper; retain current signed-out behavior.

Page turns mutate only `featured.feed_page`, clamp to `feed_pages.len() - 1`, and never touch Recent/Library `page`.

- [ ] **Step 5: Schedule only visible public covers**

`visible_cover_urls` for Feature feed walks current page blocks in visual order:

1. banner candidate URLs;
2. first six vertical candidates for each visible collection;
3. deduplicate URL fetches only in the cover cache.

When page changes, cancel obsolete in-flight public covers using the existing generation/task mechanism. A comic in two groups can point to the same cached picture but remains two distinct actions and placements.

- [ ] **Step 6: Render partial warnings without consuming a content block page**

When `snapshot.warning` exists, render one attention banner and Retry button above the page's blocks. Measure pages with that warning present so no preview clips. Retry calls Task 5 `begin_failed_retry`; the visible snapshot and current feed page remain while requests run.

If no snapshot exists and batch is active, render activity. If no snapshot exists and all sources failed, render failure plus Retry. Do not show empty headings.

- [ ] **Step 7: Run feed tests and commit**

Run:

```bash
rtk cargo test -p kobo-bomtoon feed_blocks
rtk cargo test -p kobo-bomtoon grouped_feature_feed
rtk cargo test -p kobo-bomtoon feature_feed_pages
rtk cargo test -p kobo-bomtoon visible_feature_covers
```

Expected: all pass and every constructed Clara screen fits.

Commit:

```bash
rtk git add apps/bomtoon/src/feature.rs apps/bomtoon/src/main.rs
rtk git commit -m "feat(bomtoon): render grouped Feature feed"
```

---

### Task 7: Adaptive full collections with lazy synopsis details

**Files:**
- Modify: `apps/bomtoon/src/feature.rs`
- Modify: `apps/bomtoon/src/main.rs:60-80, 962-1110, 1295-1367, 1406-1496, 3117-3143, 3449-3498, 5592-5615, 5649-5977`
- Test: inline tests in both files

**Interfaces:**
- Consumes: Task 3 described-row builder/paginator, Task 4 public detail parser, Task 5 feature task map/generation, Task 6 collection IDs and origin page, existing cover cache and comic open flow.
- Produces: `View::FeatureCollection`, six-detail windows, alias cache, stable page ranges, one-row comic actions, exact Back restoration, and collection cover scheduling.

- [ ] **Step 1: Write failing detail-window and page-boundary tests**

Pure state contracts:

```rust
#[test]
fn collection_requests_only_the_next_six_uncached_aliases() {
    let mut view = CollectionView::new("ranking", 3, 14);
    let aliases = (0..14).map(|index| format!("comic-{index}")).collect::<Vec<_>>();
    assert_eq!(view.next_detail_window(&aliases, &BTreeMap::new()), aliases[0..6]);
}

#[test]
fn adaptive_page_stores_largest_fitting_prefix_and_reuses_overflow_details() {
    let mut view = CollectionView::new("ranking", 3, 12);
    view.commit_page(0, 4);
    assert_eq!(view.pages, vec![0..4]);
    assert_eq!(view.next_start(), 4);
    assert_eq!(view.next_detail_window(&aliases(12), &ready_details(0..6)), aliases(4..10));
}

#[test]
fn back_restores_exact_originating_feed_page() {
    let mut state = app_with_open_collection("ranking", 4);
    state.close_collection();
    assert_eq!(state.view, View::Main);
    assert_eq!(state.featured.feed_page, 4);
}
```

Runner tests must cover: no detail request before heading tap; six requests maximum after tap; fewer requests for aliases already cached; all six settle before first page boundary is chosen; detail failure yields empty synopsis but active row; zero/missing counts have no trailing node; cover crop is square; Next uses cached overflow before fetching; Back cancels unresolved details; stale outcomes are no-ops; suspension cancels detail work; no nav bar; owned Back works while signed out.

- [ ] **Step 2: Add collection and detail-cache state**

In `feature.rs`:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DetailState {
    Loading(TaskId),
    Ready(PublicDetail),
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionView {
    pub generation: u64,
    pub collection_id: String,
    pub origin_feed_page: usize,
    pub page: usize,
    pub pages: Vec<std::ops::Range<usize>>,
    pub window_start: usize,
    pub window_end: usize,
    pub pending_aliases: BTreeSet<String>,
    pub queued_aliases: VecDeque<String>,
}
```

Add `detail_generation: u64`, `detail_cache: BTreeMap<String, DetailState>`, and `collection: Option<CollectionView>` to `FeaturedState`. Cache entries are alias-keyed across every collection. `DetailState::Failed` is settled and renders no synopsis; a new explicit Feature refresh may clear failed entries, but page navigation must not retry them automatically.

Add pure:

```rust
pub fn compact_count(value: Option<u64>) -> String
```

Rules: `None|Some(0) => ""`; below 1,000 decimal exact; 1,000 through 999,999 uses K with one decimal only when non-zero; 1,000,000 and above uses M with the same rule. Tests include 999, 1_000, 1_200, 12_000, 1_000_000.

- [ ] **Step 3: Open a collection and queue a bounded window**

Add `View::FeatureCollection`. `open_collection` increments `featured.detail_generation`, stores it on `CollectionView.generation`, stores the exact feed page, resets collection page/boundaries, sets the first window to placements `0..min(6, len)`, and queues only aliases absent from `detail_cache`. It then calls `resume_feature_capacity`.

Extend the task purpose in `main.rs` with:

```rust
CollectionDetail {
    generation: u64,
    collection_generation: u64,
    collection_id: String,
    alias: String,
},
```

`generation` must match the Feature snapshot generation; `collection_generation` and `collection_id` must match the active `CollectionView`. Only then may an outcome parse with `parse::public_detail` and store `Ready` or `Failed`. Remove the alias from `pending_aliases`, resume queued aliases, and call `settle_collection_window` only when both queued and pending sets are empty.

- [ ] **Step 4: Measure and store the largest fitting prefix**

After a six-placement window settles, build borrowed tuples for its placements:

```rust
let owned = candidates
    .iter()
    .map(|comic| {
        let synopsis = detail_cache
            .get(&comic.alias)
            .and_then(|state| match state {
                DetailState::Ready(detail) => detail.synopsis.clone(),
                DetailState::Loading(_) | DetailState::Failed => None,
            })
            .unwrap_or_default();
        (
            display_title(comic),
            display_creators(comic),
            synopsis,
            compact_count(comic.view_count),
        )
    })
    .collect::<Vec<_>>();
let measured = owned
    .iter()
    .map(|(title, creators, synopsis, trailing)| {
        (
            title.as_str(),
            creators.as_str(),
            synopsis.as_str(),
            trailing.as_str(),
        )
    })
    .collect::<Vec<_>>();
let pages = context.paginate_described_rows_with_trailing(
    &measured,
    RowLineLimits::new(1, 1, 2),
    false,
);
let count = pages.first().map_or(1, |page| page.len().max(1));
```

Commit `start..start + count.min(candidates.len())` to `CollectionView.pages`. The remaining settled candidates stay cached. Next first reuses an existing boundary; otherwise it starts at the prior range end and opens the next six-placement window.

- [ ] **Step 5: Render full rows with exact text and square cover policy**

`collection_screen` has top bar `collection.label`, no nav bar, page turns, and page position. While the current window is unsettled, retain already committed current page if one exists; on initial page show activity.

Render:

```rust
screen.described_rows_with_trailing(
    RowLineLimits::new(1, 1, 2),
    range.clone().map(|index| {
        let comic = &collection.comics[index];
        let synopsis = synopsis_for(&self.featured.detail_cache, &comic.alias);
        (
            comic_action(&collection.id, index),
            display_text(&comic.title, &format!("BOMTOON {}", comic.alias)),
            display_text(&comic.creators, ""),
            synopsis,
            cover_lead(
                &self.covers,
                comic.square_url.as_deref().or(comic.vertical_url.as_deref()),
            ),
            compact_count(comic.view_count),
        )
    }),
)
```

Before passing the row lead, convert any ready picture to `.with_fit(PictureFit::Cover)`. The existing book glyph remains the fallback. The whole row action opens the comic. A failed synopsis does not disable it.

- [ ] **Step 6: Integrate Back, page turns, covers, and lifecycle**

`show` owns Back whenever `view == View::FeatureCollection`, regardless of account state. Handle `ActionId::BACK` before account-gated Back logic:

```rust
if self.view == View::FeatureCollection && action == ActionId::BACK {
    self.cancel_collection_details(context);
    let origin = self.featured.collection.take().map_or(0, |view| view.origin_feed_page);
    self.featured.feed_page = origin;
    self.view = View::Main;
    self.destination = MainDestination::Featured;
    self.show(context);
    return;
}
```

Previous/Next mutate `CollectionView.page`; Next queues a new window only when no stored boundary exists. Visible cover URLs are square-first for the committed current range. Leaving, suspension, sign-out transition, and exit cancel pending collection details, increment `featured.detail_generation`, clear queued aliases, and retain settled detail-cache values only while the public Feature snapshot remains valid.

Full collection never renders `MAIN_DESTINATIONS`.

- [ ] **Step 7: Run collection tests and commit**

Run:

```bash
rtk cargo test -p kobo-bomtoon collection_requests_only
rtk cargo test -p kobo-bomtoon adaptive_collection
rtk cargo test -p kobo-bomtoon collection_back
rtk cargo test -p kobo-bomtoon collection_detail
rtk cargo test -p kobo-bomtoon collection_screen
```

Expected: all pass; Clara diagnostics show no clipping or overlap for long title/creator/synopsis/count fixtures.

Commit:

```bash
rtk git add apps/bomtoon/src/feature.rs apps/bomtoon/src/main.rs
rtk git commit -m "feat(bomtoon): add full Feature collections"
```

---

### Task 8: Focused gates and real-surface verification

**Files:**
- Modify only files already touched when a verification failure identifies a defect.
- Do not add screenshots or documentation unless the user requests committed artifacts.

**Interfaces:**
- Consumes: completed Tasks 1-7.
- Produces: compile, test, lint, browser-simulator, runtime-simulator, and public-source evidence for the accepted behavior.

- [ ] **Step 1: Format and run focused package tests**

Run from the isolated worktree root:

```bash
rtk cargo fmt --all -- --check
rtk cargo test -p kobo-ui -p kobo-protocol -p kobo-sdk -p kobo-bomtoon
```

Expected: formatting passes; all package tests pass with only the repository's already-ignored tests ignored.

- [ ] **Step 2: Run focused Clippy and workspace build**

```bash
rtk cargo clippy -p kobo-ui -p kobo-protocol -p kobo-sdk -p kobo-bomtoon --all-targets --all-features -- -D warnings
rtk cargo build --workspace
```

Expected: zero warnings from Clippy and a successful workspace build. Existing build warnings outside the changed packages may still appear only if they are not promoted by this focused command; do not suppress them.

- [ ] **Step 3: Exercise the browser simulator**

From `apps/bomtoon`, start:

```bash
rtk cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

Use the browser automation tool against the emitted simulator URL. Verify visually and by actions:

1. Signed-out Feature loads the public feed.
2. Banner has three image-only cover-cropped targets.
3. Fixed headings use exact zh-TW copy and order, including `只在 Bomtoon` at priority 8.
4. Every `theme-*` heading is grouped under one non-interactive `編輯精選` heading.
5. Every preview is three rows by two cards and headings open the matching full collection.
6. Full rows show square crops, one title line, one creator line, at most two synopsis lines, and only positive compact counts.
7. Next/Previous preserve block/page boundaries.
8. Back from a full collection returns to the exact feed page that opened it.
9. Missing images show book glyphs without changing target sizes.

Expected: no clipped text, overlapping controls, stretched pictures, empty heading, or wrong target.

- [ ] **Step 4: Exercise runtime simulator task and failure behavior**

Stop the browser dev process, then run:

```bash
rtk cargo run -p kobo-cli -- run --sim --app bomtoon
```

Exercise: initial load, one simulated source failure, Retry, daily refresh trigger if the simulator exposes local-day injection, collection detail failure, Back, and suspension/resume. Confirm the old snapshot stays visible during refresh and partial success remains actionable.

Expected: at most four tasks in flight, Retry requests failed sources only, stale/cancelled outcomes do not redraw, and no task is left attached to a closed collection.

- [ ] **Step 5: Confirm public-source behavior without credentials**

Use the app's recorded simulator task commands or request diagnostics. Confirm homepage, ranking, favorites, themes, freetime, and public detail tasks all carry `credential: None`. Confirm the signed-out run reaches at least one non-empty group from each successful selected source.

Expected: no authentication prompt or protected credential reference for Feature metadata/artwork.

- [ ] **Step 6: Run final changed-contract tests after any simulator fixes**

```bash
rtk cargo fmt --all -- --check
rtk cargo test -p kobo-ui -p kobo-protocol -p kobo-sdk -p kobo-bomtoon
rtk cargo clippy -p kobo-ui -p kobo-protocol -p kobo-sdk -p kobo-bomtoon --all-targets --all-features -- -D warnings
```

Expected: all pass.

- [ ] **Step 7: Commit verification-driven fixes if any**

If Steps 1-6 required code changes:

```bash
rtk git add crates/kobo-ui/src/lib.rs crates/kobo-protocol/src/lib.rs crates/kobo-sdk/src/lib.rs apps/bomtoon/src/api.rs apps/bomtoon/src/feature.rs apps/bomtoon/src/main.rs apps/bomtoon/src/model.rs apps/bomtoon/src/parse.rs
rtk git commit -m "fix(bomtoon): complete Feature collection verification"
```

If no code changed, do not create an empty commit.
