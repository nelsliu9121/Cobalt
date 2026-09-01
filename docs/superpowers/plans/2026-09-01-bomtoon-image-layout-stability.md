# Bomtoon Image Layout Stability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render uncropped responsive Feature banners and prevent Recent and Library title rows from moving when cover pictures finish loading.

**Architecture:** Keep the existing retained UI types. Bomtoon will select the existing `RowLead::CoverSlot` for declared shelf covers that are not ready. The SDK will preserve image-strip picture fit, and `kobo-ui` will give each strip slot a responsive 289:345 ratio while fitting the full source into a horizontally centered, bottom-aligned picture rectangle.

**Tech Stack:** Rust 1.85.1, edition 2021, `kobo-bomtoon`, `kobo-sdk`, `kobo-ui`, inline Rust tests, Clara BW layout diagnostics, browser simulator, runtime simulator.

## Global Constraints

- Treat 289:345 only as an aspect ratio. Derive dimensions from the available viewport width.
- Keep three equal banner columns spanning the content width with the existing two small gaps.
- Never crop Feature banner sources. Center them horizontally and align them to the slot bottom.
- Keep the full banner slot as the tap target and preserve the three-item limit.
- Recent and Library rows with a declared but unavailable cover use the existing square `RowLead::CoverSlot`.
- Rows without a cover URL retain `RowLead::Icon`; episode rows remain unchanged.
- Do not change models, parsers, protocol encoding, networking, cover scheduling, pagination, actions, or trailing text.
- Add no dependencies and no new public configuration or retained UI node.
- Follow TDD: write each changed-contract test, observe the expected failure, then make the minimal production change.
- Prefix every shell command with `rtk`.

---

### Task 1: Stabilize Recent and Library cover slots

**Files:**
- Modify: `apps/bomtoon/src/main.rs:454-465`
- Modify: `apps/bomtoon/src/main.rs:1723-1774`
- Test: `apps/bomtoon/src/main.rs:17420-17550,18314-18363`

**Interfaces:**
- Consumes: existing `ready_cover(&CoverCache, Option<&str>) -> Option<TilePicture>`, `RowLead::{Icon, CoverSlot, Picture}`, and `CoverState`.
- Produces: private `shelf_cover_lead(&CoverCache, Option<&str>) -> RowLead`, used only by Recent and Library row builders.

- [ ] **Step 1: Add failing shelf lead state tests**

Add these tests beside the existing Recent and Library cover tests:

```rust
#[test]
fn shelf_cover_lead_reserves_a_square_only_for_declared_artwork() {
    let covers = CoverCache::default();
    let url = "https://image.balcony.studio/tw/contents/shelf.webp";

    assert_eq!(shelf_cover_lead(&covers, None), RowLead::Icon(Glyph::Book));
    assert_eq!(
        shelf_cover_lead(&covers, Some(url)),
        RowLead::CoverSlot(Glyph::Book)
    );
}

#[test]
fn shelf_cover_lead_keeps_the_slot_through_loading_and_failure() {
    let url = "https://image.balcony.studio/tw/contents/shelf.webp";
    let picture = TilePicture::new(PictureHandle(91), 289, 345);
    let mut covers = CoverCache::default();
    covers
        .entries
        .insert(url.to_owned(), CoverState::Loading(TaskId(90)));
    assert_eq!(
        shelf_cover_lead(&covers, Some(url)),
        RowLead::CoverSlot(Glyph::Book)
    );


    covers.entries.insert(url.to_owned(), CoverState::Failed);
    assert_eq!(
        shelf_cover_lead(&covers, Some(url)),
        RowLead::CoverSlot(Glyph::Book)
    );

    covers
        .entries
        .insert(url.to_owned(), CoverState::Ready(picture));
    assert_eq!(
        shelf_cover_lead(&covers, Some(url)),
        RowLead::Picture(picture, Glyph::Book)
    );
}
```

Add one integration-style layout test for both destinations:

```rust
#[test]
fn recent_and_library_title_geometry_stays_fixed_when_cover_becomes_ready() {
    let url = shelf_cover_url(0);
    let geometry = |screen: &Screen, title: &str| {
        let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::default());
        let title = layout
            .nodes
            .iter()
            .find(|node| {
                node.text_lines.len() == 1
                    && node.text_lines.first().map(String::as_str) == Some(title)
            })
            .expect("shelf title");
        let side = CLARA_BW_METRICS.touch_target_default();
        let lead = layout
            .nodes
            .iter()
            .find(|node| {
                node.text_lines.is_empty()
                    && node.rect.x == layout.content.x
                    && node.rect.width == side
                    && node.rect.height == side
                    && node.rect.y <= title.rect.y
                    && node.rect.y.saturating_add(node.rect.height)
                        >= title.rect.y.saturating_add(title.rect.height)
            })
            .expect("square shelf lead");
        (
            (lead.rect.x, lead.rect.y, lead.rect.width, lead.rect.height),
            (
                title.rect.x,
                title.rect.y,
                title.rect.width,
                title.rect.height,
            ),
        )
    };

    for (destination, title) in [
        (MainDestination::Recent, "Recent 0"),
        (MainDestination::Library, "Library 0"),
    ] {
        let mut app = Bomtoon {
            account: AccountState::Active,
            view: View::Main,
            destination,
            recent: vec![recent_shelf_entry(0, Some(url.clone()))],
            comics: vec![library_shelf_comic(0, Some(url.clone()))],
            recent_load: loaded_shelf(),
            library_load: loaded_shelf(),
            total_recent_titles: 1,
            total_library_titles: 1,
            ..Bomtoon::default()
        };

        let placeholder = app.main_screen();
        let placeholder_row = placeholder
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => rows.first(),
                _ => None,
            })
            .expect("placeholder shelf row");
        assert_eq!(placeholder_row.lead, RowLead::CoverSlot(Glyph::Book));
        let before = geometry(&placeholder, title);

        let picture = TilePicture::new(PictureHandle(92), 289, 345);
        app.covers
            .entries
            .insert(url.clone(), CoverState::Ready(picture));
        let ready = app.main_screen();
        let ready_row = ready
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => rows.first(),
                _ => None,
            })
            .expect("ready shelf row");
        assert_eq!(ready_row.lead, RowLead::Picture(picture, Glyph::Book));

        assert_eq!(geometry(&ready, title), before);
        assert_fits(&placeholder);
        assert_fits(&ready);
    }
}
```

- [ ] **Step 2: Run the new tests and verify RED**

Run:

```bash
rtk cargo test -p kobo-bomtoon shelf_cover_lead -- --nocapture
rtk cargo test -p kobo-bomtoon recent_and_library_title_geometry_stays_fixed_when_cover_becomes_ready -- --nocapture
```

Expected: compilation fails because `shelf_cover_lead` does not exist. Do not edit production code until this failure is observed.

- [ ] **Step 3: Add the shelf-specific lead helper**

Add this function beside `cover_lead`:

```rust
fn shelf_cover_lead(covers: &CoverCache, url: Option<&str>) -> RowLead {
    let Some(url) = url else {
        return RowLead::Icon(Glyph::Book);
    };
    ready_cover(covers, Some(url)).map_or(RowLead::CoverSlot(Glyph::Book), |picture| {
        RowLead::Picture(picture, Glyph::Book)
    })
}
```

Keep `cover_lead` unchanged for episode thumbnails. Replace only the two main-shelf calls:

```rust
shelf_cover_lead(&self.covers, recent.cover_url.as_deref())
```

```rust
shelf_cover_lead(&self.covers, comic.cover_url.as_deref())
```

Update the existing `compact_shelf_recent_uses_creator_rows_with_episode_trailing_and_ready_picture` assertions so declared, not-yet-ready URLs expect `RowLead::CoverSlot(Glyph::Book)` before and after the first picture completes. Do not change the Library no-URL assertion; it must remain `RowLead::Icon(Glyph::Book)`.

- [ ] **Step 4: Run shelf tests and verify GREEN**

Run:

```bash
rtk cargo test -p kobo-bomtoon shelf_cover_lead -- --nocapture
rtk cargo test -p kobo-bomtoon recent_and_library_title_geometry_stays_fixed_when_cover_becomes_ready -- --nocapture
rtk cargo test -p kobo-bomtoon compact_shelf -- --nocapture
```

Expected: all selected tests pass. Recent and Library placeholder and ready geometry are equal; rows without cover URLs still use compact icons.

- [ ] **Step 5: Commit the shelf fix**

```bash
rtk git add apps/bomtoon/src/main.rs
rtk git commit -m "fix(bomtoon): stabilize shelf cover slots"
```

---

### Task 2: Preserve image-strip contain fitting in the SDK

**Files:**
- Modify: `crates/kobo-sdk/src/lib.rs:1991-2013`
- Test: `crates/kobo-sdk/src/lib.rs:6916-6997`

**Interfaces:**
- Consumes: existing `TilePicture.fit` and `PictureFit::Contain` default from `TilePicture::new`.
- Produces: `ScreenBuilder::image_strip` tiles that preserve the caller-supplied `TilePicture` unchanged.

- [ ] **Step 1: Change the builder assertion to the desired contain contract**

In `feature_feed_tests::feature_feed_builders`, change only the image-strip assertion:

```rust
assert_eq!(
    tiles[0].picture.expect("strip picture").fit,
    PictureFit::Contain
);
```

Keep the media-grid assertion unchanged.

- [ ] **Step 2: Run the builder test and verify RED**

Run:

```bash
rtk cargo test -p kobo-sdk feature_feed_builders -- --nocapture
```

Expected: FAIL because the actual image-strip fit is `PictureFit::Cover`.

- [ ] **Step 3: Stop overriding the supplied picture fit**

Change the picture arm in `ScreenBuilder::image_strip` to:

```rust
Some(picture) => tile.with_picture(picture),
```

Update the method comment to describe responsive image-only banner targets without claiming cover cropping:

```rust
/// Adds up to three equal image-only banner targets.
```

- [ ] **Step 4: Run the builder test and verify GREEN**

Run:

```bash
rtk cargo test -p kobo-sdk feature_feed_builders -- --nocapture
```

Expected: PASS. The first strip tile retains `PictureFit::Contain`; action registration and collection limits remain unchanged.

- [ ] **Step 5: Commit the SDK contract change**

```bash
rtk git add crates/kobo-sdk/src/lib.rs
rtk git commit -m "fix(sdk): preserve banner picture fit"
```

---

### Task 3: Lay out responsive, uncropped, bottom-aligned banners

**Files:**
- Modify: `crates/kobo-ui/src/lib.rs:260-265`
- Modify: `crates/kobo-ui/src/lib.rs:4970-5002`
- Modify: `crates/kobo-ui/src/lib.rs:7106-7164`
- Test: `crates/kobo-ui/src/lib.rs:21534-21618`

**Interfaces:**
- Consumes: `Node::ImageStrip`, `TilePicture.source`, `PictureFit::Contain`, `MAX_IMAGE_STRIP_ITEMS`, `DisplayMetrics::space(Space::Small)`, and the unchanged retained protocol payload.
- Produces: private `scale_within((u32, u32), i32, i32) -> (i32, i32)` plus responsive 289:345 image-strip slot and picture rectangles.

- [ ] **Step 1: Replace the old portrait-strip test with responsive slot assertions**

Replace `image_strip_is_three_equal_image_only_targets` with:

```rust
#[test]
fn image_strip_uses_three_responsive_banner_slots() {
    let screen = Screen::new(
        1,
        vec![Node::ImageStrip {
            id: NodeId(1),
            tiles: (0..3)
                .map(|index| {
                    Tile::new(ActionId(index + 1), "", Glyph::Book).with_picture(
                        TilePicture::new(PictureHandle(index + 1), 2_890, 3_450),
                    )
                })
                .collect(),
        }],
    );
    let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::default());
    let targets = layout
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, LayoutKind::Tile(_, _)))
        .collect::<Vec<_>>();
    let gutter = CLARA_BW_METRICS.space(Space::Small);

    assert_eq!(targets.len(), 3);
    assert!(targets
        .windows(2)
        .all(|pair| pair[0].rect.width == pair[1].rect.width));
    assert!(targets.iter().all(|target| {
        target.rect.height
            == i32::try_from(i64::from(target.rect.width) * 345 / 289)
                .expect("banner height fits i32")
    }));
    assert_eq!(targets[0].rect.x, layout.content.x);
    assert_eq!(
        targets[1].rect.x,
        targets[0]
            .rect
            .x
            .saturating_add(targets[0].rect.width)
            .saturating_add(gutter)
    );
    assert_eq!(
        targets[2].rect.x,
        targets[1]
            .rect
            .x
            .saturating_add(targets[1].rect.width)
            .saturating_add(gutter)
    );
    let used = targets[0].rect.width.saturating_mul(3).saturating_add(gutter * 2);
    assert!((layout.content.width - used).abs() < 3);
    assert!(layout.nodes.iter().all(|node| node.kind != LayoutKind::TileLabel));
    assert_eq!(
        layout
            .nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.kind,
                    LayoutKind::FramedPicture(_, PictureFit::Contain)
                )
            })
            .count(),
        3
    );
}
```

- [ ] **Step 2: Add a failing source-ratio and bottom-alignment test**

Add:

```rust
#[test]
fn image_strip_contains_centers_and_bottom_aligns_each_source() {
    let sources = [(2_890, 3_450), (5_780, 3_450), (2_890, 6_900)];
    let screen = Screen::new(
        1,
        vec![Node::ImageStrip {
            id: NodeId(1),
            tiles: sources
                .into_iter()
                .enumerate()
                .map(|(index, (width, height))| {
                    let handle = u32::try_from(index + 1).expect("three banner handles fit u32");
                    Tile::new(ActionId(handle), "", Glyph::Book).with_picture(TilePicture::new(
                        PictureHandle(handle),
                        width,
                        height,
                    ))
                })
                .collect(),
        }],
    );
    let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::default());
    let targets = layout
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, LayoutKind::Tile(_, _)))
        .collect::<Vec<_>>();

    for (index, source) in sources.into_iter().enumerate() {
        let handle = PictureHandle(
            u32::try_from(index + 1).expect("three banner handles fit u32"),
        );
        let picture = layout
            .nodes
            .iter()
            .find(|node| {
                node.kind == LayoutKind::FramedPicture(handle, PictureFit::Contain)
            })
            .expect("contained banner picture");
        let slot = targets[index].rect;
        let expected = scale_within(source, slot.width, slot.height);

        assert_eq!((picture.rect.width, picture.rect.height), expected);
        assert_eq!(
            picture.rect.y.saturating_add(picture.rect.height),
            slot.y.saturating_add(slot.height)
        );
        assert!(
            (picture.rect.x * 2 + picture.rect.width - (slot.x * 2 + slot.width)).abs() <= 1
        );
        assert!(picture.rect.width <= slot.width);
        assert!(picture.rect.height <= slot.height);
    }
}

#[test]
fn image_strip_zero_sized_picture_uses_centered_placeholder() {
    let screen = Screen::new(
        1,
        vec![Node::ImageStrip {
            id: NodeId(1),
            tiles: vec![
                Tile::new(ActionId(1), "", Glyph::Book)
                    .with_picture(TilePicture::new(PictureHandle(1), 0, 0)),
            ],
        }],
    );
    let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::default());
    let slot = layout
        .nodes
        .iter()
        .find(|node| matches!(node.kind, LayoutKind::Tile(_, _)))
        .expect("banner slot")
        .rect;
    let glyph = layout
        .nodes
        .iter()
        .find(|node| node.kind == LayoutKind::TileGlyph(Glyph::Book))
        .expect("zero-sized picture placeholder")
        .rect;

    assert!(glyph.width > 0 && glyph.height > 0);
    assert!((glyph.x * 2 + glyph.width - (slot.x * 2 + slot.width)).abs() <= 1);
    assert!((glyph.y * 2 + glyph.height - (slot.y * 2 + slot.height)).abs() <= 1);
}
```

- [ ] **Step 3: Run the image-strip tests and verify RED**

Run:

```bash
rtk cargo test -p kobo-ui image_strip_ -- --nocapture
```

Expected: the responsive test fails because current slots use the 3:5 portrait shape and `PictureFit::Cover`; the placement test fails because `scale_within` does not exist; the zero-sized source test fails because it still produces a framed picture.

- [ ] **Step 4: Add responsive ratio constants and proportional scaling**

Add private constants beside `MAX_IMAGE_STRIP_ITEMS`:

```rust
const IMAGE_STRIP_ASPECT_WIDTH: i32 = 289;
const IMAGE_STRIP_ASPECT_HEIGHT: i32 = 345;
```

Add this private helper immediately after `fit_within`. Unlike `fit_within`, this helper may enlarge a source because banners must span their responsive columns:

```rust
fn scale_within(source: (u32, u32), max_width: i32, max_height: i32) -> (i32, i32) {
    let max_width = max(0, max_width);
    let max_height = max(0, max_height);
    let width = i32::try_from(source.0).unwrap_or(i32::MAX);
    let height = i32::try_from(source.1).unwrap_or(i32::MAX);
    if width <= 0 || height <= 0 || max_width == 0 || max_height == 0 {
        return (0, 0);
    }

    let scaled_height = max(
        1,
        i32::try_from(i64::from(max_width) * i64::from(height) / i64::from(width))
            .unwrap_or(i32::MAX),
    );
    if scaled_height <= max_height {
        return (max_width, scaled_height);
    }

    (
        max(
            1,
            i32::try_from(i64::from(max_height) * i64::from(width) / i64::from(height))
                .unwrap_or(i32::MAX),
        ),
        max_height,
    )
}
```

- [ ] **Step 5: Implement responsive slots and bottom-aligned picture rectangles**

In the `Node::ImageStrip` layout arm, retain the three-column and gutter calculation, then replace the portrait height with:

```rust
let cell_height = i32::try_from(
    i64::from(cell_width.max(0)) * i64::from(IMAGE_STRIP_ASPECT_HEIGHT)
        / i64::from(IMAGE_STRIP_ASPECT_WIDTH),
)
.unwrap_or(i32::MAX);
```

Replace the mark calculation and y placement with:

```rust
let fitted_picture = tile.picture.and_then(|picture| {
    let (width, height) = scale_within(picture.source, cell_width, cell_height);
    (width > 0 && height > 0).then_some((picture, width, height))
});
let (kind, mark_width, mark_height, mark_y) =
    if let Some((picture, picture_width, picture_height)) = fitted_picture {
        (
            LayoutKind::FramedPicture(picture.handle, PictureFit::Contain),
            picture_width,
            picture_height,
            cell_height.saturating_sub(picture_height),
        )
    } else {
        let size = min(
            metrics.tenth_mm(110),
            min(cell_width.max(0), cell_height.max(0)),
        );
        (
            LayoutKind::TileGlyph(tile.glyph),
            size,
            size,
            (cell_height - size) / 2,
        )
    };
layout.nodes.push(LayoutNode {
    id: *id,
    rect: Rect {
        x: cell_x.saturating_add((cell_width - mark_width) / 2),
        y: y.saturating_add(mark_y),
        width: mark_width,
        height: mark_height,
    },
    kind,
    text_lines: Vec::new(),
});
```

Do not change the tile target rectangle. It remains the full responsive slot, so taps include transparent and unused space.

- [ ] **Step 6: Run UI tests and verify GREEN**

Run:

```bash
rtk cargo test -p kobo-ui image_strip_ -- --nocapture
rtk cargo test -p kobo-ui cover_slot_fallback_keeps_ready_picture_row_geometry -- --nocapture
```

Expected: all selected tests pass. Slots use the responsive ratio, every valid picture uses contain fit, picture bottoms align with slot bottoms, zero-sized sources use centered glyph placeholders, and the existing square shelf placeholder geometry remains stable.

- [ ] **Step 7: Commit the UI layout change**

```bash
rtk git add crates/kobo-ui/src/lib.rs
rtk git commit -m "fix(ui): preserve responsive banner artwork"
```

---

### Task 4: Run integrated checks and simulator verification

**Files:**
- Verify only: `apps/bomtoon/src/main.rs`
- Verify only: `crates/kobo-sdk/src/lib.rs`
- Verify only: `crates/kobo-ui/src/lib.rs`

**Interfaces:**
- Consumes: Tasks 1-3 completed commits.
- Produces: focused test, package test, lint, browser simulator, and runtime simulator evidence for the approved behavior.

- [ ] **Step 1: Format and run all focused contract tests**

Run:

```bash
rtk cargo fmt --all
rtk cargo fmt --all -- --check
rtk cargo test -p kobo-bomtoon shelf_cover_lead -- --nocapture
rtk cargo test -p kobo-bomtoon recent_and_library_title_geometry_stays_fixed_when_cover_becomes_ready -- --nocapture
rtk cargo test -p kobo-sdk feature_feed_builders -- --nocapture
rtk cargo test -p kobo-ui image_strip_ -- --nocapture
rtk cargo test -p kobo-ui cover_slot_fallback_keeps_ready_picture_row_geometry -- --nocapture
```

Expected: `cargo fmt --all -- --check` passes after formatting and every focused contract test passes.

- [ ] **Step 2: Run affected package and Clippy gates**

Run:

```bash
rtk cargo test -p kobo-bomtoon
rtk cargo test -p kobo-sdk
rtk cargo test -p kobo-ui
rtk cargo clippy -p kobo-bomtoon -p kobo-sdk -p kobo-ui --all-targets --all-features -- -D warnings
```

Expected: all affected package tests pass and Clippy emits no warnings.

- [ ] **Step 3: Exercise the browser simulator on the actual UI surface**

From `apps/bomtoon`, start the long-running process through the harness process manager:

```bash
rtk cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

Expected startup output: `Kobo app simulator: http://127.0.0.1:8787`.

Open `http://127.0.0.1:8787/` with the browser automation tool and use the existing authenticated simulator state. Do not purchase or rent anything. Verify:

1. Feature shows three equal banners spanning almost all content width with two small gaps.
2. Every banner shows its complete artwork, with no top, side, or bottom crop.
3. Banner artwork is centered horizontally and aligned to the bottom of its slot.
4. Recent title and creator text does not move when each cover appears.
5. Library title and creator text does not move when each cover appears.
6. Rows without artwork remain compact book-icon rows.
7. Banner taps, shelf row taps, page turns, and bottom navigation still work.

Capture browser-tool screenshots before and after cover completion when the loading transition is observable. The deterministic geometry tests remain the proof if the local cache makes the transition too fast to capture.

- [ ] **Step 4: Exercise the runtime simulator boundary**

Stop the browser simulator through the harness process manager. From the repository root, start:

```bash
rtk cargo run -p kobo-cli -- run --sim --app bomtoon
```

Expected: the runtime, daemon, and Bomtoon app start without layout diagnostics or protocol errors. Exercise Feature, Recent, and Library without spending. Confirm the same banner and stable shelf-row behavior, then stop the process through the harness process manager.

- [ ] **Step 5: Commit formatter output if needed and verify implementation paths**

If `cargo fmt` changed an implementation file, commit only those formatter changes:

```bash
rtk git add apps/bomtoon/src/main.rs crates/kobo-sdk/src/lib.rs crates/kobo-ui/src/lib.rs
rtk git commit -m "style: format image layout changes"
```

If the formatter made no changes, skip that commit. Then run:

```bash
rtk git diff --check
rtk git diff --exit-code -- apps/bomtoon/src/main.rs crates/kobo-sdk/src/lib.rs crates/kobo-ui/src/lib.rs
```

Expected final state: the implementation paths have no unstaged changes and this work introduced no unresolved placeholder comments.
