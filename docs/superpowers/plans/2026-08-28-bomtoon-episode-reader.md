# BOMTOON Episode Reader Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a signed-in Kobo reader open owned and free BOMTOON episodes and read validated plain WebP pages with fixed-geometry full-screen artwork and tap-to-toggle overlay chrome.

**Architecture:** Add a screen-level `ReadingSurface` to the shared UI/protocol/SDK path so reader artwork occupies the complete panel while standard top-bar and page-position metadata can be overlaid without reflow. Extend the shared image crate with the bounded exact-width transform needed before app-local row slicing. Keep BOMTOON account manifests credentialed, CDN image requests uncredentialed, and all reader state inside the app's existing single-task state machine.

**Tech Stack:** Rust 2021, `kobo-ui`, `kobo-protocol`, `kobo-sdk`, `kobo-image`, `kobo-net`, `kobo-json`, `http` 1.3, inline Rust unit tests, Clara BW layout diagnostics, browser simulator, runtime simulator.

## Global Constraints

- Only `PurchaseState::Owned` and `PurchaseState::Free` episodes are actionable. Sample, not-owned, and unknown states remain inert.
- The manifest route is exactly `GET https://www.bomtoon.tw/api/balcony-api-v2/contents/images/<content-alias>/<episode-alias>?imageWidth=1080`.
- Manifest requests use managed bearer credential `bomtoon-access-token`, the existing Balcony headers, the exact viewer `x-referer`, and a 512 KiB body ceiling.
- Content and episode aliases are non-empty ASCII alphanumeric, underscore, or hyphen.
- Manifests contain 1 through 256 entries, with contiguous one-based `orderNo`, positive dimensions, signed URLs no longer than 1024 bytes, and null `line` and `point`.
- Any non-null `line` or `point` fails the complete manifest as unsupported scrambled content.
- Signed URLs use HTTPS, exact host `image.balcony.studio`, default port, `/tw/ep/` paths ending in `.webp`, no credentials or fragment, and exactly one non-empty `Policy`, `Signature`, and `Key-Pair-Id` query value.
- CDN image requests carry no credential and retain the 4 MiB `kobo-image::MAX_SOURCE_BYTES` ceiling.
- `kobo-image::MAX_PIXELS` is exactly 7,000,000. Every existing public pixel guard uses that same ceiling.
- Decoded image dimensions must equal the manifest dimensions before scaling.
- Reader slices are display-width by full display-height. The final slice is white-padded and top-aligned.
- Reader chrome starts hidden. Center tap toggles it. A successful page turn hides it. Boundary no-ops preserve it.
- Hidden and overlay chrome use the identical full-panel picture rectangle and page geometry.
- Only one scaled source and one uploaded slice are retained. New handle order is Put, SetScreen, then Drop old handle.
- CDN `Unauthorized` refreshes the manifest once and retries the same source/order. A second rejection does not change account state.
- Refreshed manifests must preserve count, order, dimensions, host, and path; only signed query values may change.
- Manifest `NoCredential` and `Unauthorized` map to signed-out and expired account states. CDN failures never alter account state.
- Signed URLs and credential values never enter logs, persistent state, errors, snapshots, or documentation.
- Existing non-reader screens, flowing pictures, reserved top bars/page positions, credential routes, and image fit behavior remain unchanged.
- Shell commands in this repository are prefixed with `rtk`.

---

### Task 1: Add fixed-geometry reading surfaces to `kobo-ui`

**Files:**
- Modify: `crates/kobo-ui/src/lib.rs:1252-1482`
- Modify: `crates/kobo-ui/src/lib.rs:1581-1774`
- Modify: `crates/kobo-ui/src/lib.rs:3926-4278`
- Modify: `crates/kobo-ui/src/lib.rs:9177-9754`
- Test: `crates/kobo-ui/src/lib.rs` test module near the existing page-turn tests

**Interfaces:**
- Consumes: existing `TilePicture`, `TopBar`, `PageTurns`, `Layout`, `LayoutKind::Picture`, `layout_top_bar`, `Pictures`, and reverse-order control hit testing.
- Produces: `ReadingChrome`, `ReadingSurface`, `Screen::reading_surface: Option<ReadingSurface>`, and deterministic full-panel layout for Tasks 2 and 3.

- [ ] **Step 1: Write failing reading-surface layout tests**

Add tests that construct the same `ReadingSurface` in both modes and pin geometry, visibility, and hit precedence:

```rust
#[test]
fn reading_chrome_overlays_the_same_full_panel_picture() {
    let picture = TilePicture::new(
        PictureHandle(41),
        CLARA_BW_METRICS.width as u32,
        CLARA_BW_METRICS.height as u32,
    );
    let screen = |chrome| {
        let mut screen = Screen::new(7, Vec::new())
            .with_top_bar(TopBar::new(NodeId(1), "Episode One"))
            .with_page_turns(ActionId(10), ActionId(11));
        screen.page_turns = screen
            .page_turns
            .map(|turns| turns.with_menu(ActionId(12)).with_position(4, 12));
        screen.with_reading_surface(Some(ReadingSurface::new(
            NodeId(2),
            picture,
            chrome,
        )))
    };

    let hidden = screen(ReadingChrome::Hidden)
        .layout_with(&CLARA_BW_METRICS, &Chrome::with_back(true));
    let overlay = screen(ReadingChrome::Overlay)
        .layout_with(&CLARA_BW_METRICS, &Chrome::with_back(true));
    let full_panel = Rect {
        x: 0,
        y: 0,
        width: CLARA_BW_METRICS.width,
        height: CLARA_BW_METRICS.height,
    };

    let picture_rect = |layout: &Layout| {
        layout
            .nodes
            .iter()
            .find(|node| node.kind == LayoutKind::Picture(PictureHandle(41)))
            .expect("reading picture")
            .rect
    };
    assert_eq!(picture_rect(&hidden), full_panel);
    assert_eq!(picture_rect(&overlay), full_panel);
    assert_eq!(hidden.content, full_panel);
    assert_eq!(overlay.content, full_panel);
    assert!(!hidden.nodes.iter().any(|node| matches!(
        node.kind,
        LayoutKind::TopBar | LayoutKind::PagePosition | LayoutKind::ReadingFooter
    )));
    assert!(overlay.nodes.iter().any(|node| node.kind == LayoutKind::TopBar));
    assert!(overlay
        .nodes
        .iter()
        .any(|node| node.kind == LayoutKind::ReadingFooter));
    assert!(overlay
        .nodes
        .iter()
        .any(|node| node.kind == LayoutKind::PagePosition));

    let back = overlay
        .nodes
        .iter()
        .find(|node| node.kind == LayoutKind::Back)
        .expect("overlay Back target");
    assert_eq!(
        overlay.hit_test(back.rect.x + 1, back.rect.y + 1),
        Some(ActionId::BACK)
    );
    assert_eq!(
        overlay.hit_test(CLARA_BW_METRICS.width / 2, CLARA_BW_METRICS.height / 2),
        Some(ActionId(12))
    );
}

#[test]
fn reading_surface_reports_wrong_panel_dimensions() {
    let screen = Screen::new(8, Vec::new()).with_reading_surface(Some(ReadingSurface::new(
        NodeId(1),
        TilePicture::new(PictureHandle(2), 100, 200),
        ReadingChrome::Hidden,
    )));
    assert!(screen.validate(&CLARA_BW_METRICS).iter().any(|issue| matches!(
        issue.kind,
        LayoutIssueKind::ReadingSurfaceSize { actual: (100, 200), .. }
    )));
}
```

- [ ] **Step 2: Run the focused tests and verify the missing API failure**

Run: `rtk cargo test -p kobo-ui reading_chrome_overlays_the_same_full_panel_picture`

Expected: compile failure naming missing `ReadingChrome`, `ReadingSurface`, `ReadingFooter`, or `with_reading_surface`.

- [ ] **Step 3: Add the reading-surface model and screen field**

Add the semantic types beside `Screen`, then initialize the field in `Screen::new`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadingChrome {
    Hidden,
    Overlay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadingSurface {
    pub id: NodeId,
    pub picture: TilePicture,
    pub chrome: ReadingChrome,
}

impl ReadingSurface {
    #[must_use]
    pub const fn new(id: NodeId, picture: TilePicture, chrome: ReadingChrome) -> Self {
        Self { id, picture, chrome }
    }
}
```

Add this field to `Screen`:

```rust
pub reading_surface: Option<ReadingSurface>,
```

Add the consuming setter used by tests and protocol decoding:

```rust
#[must_use]
pub const fn with_reading_surface(mut self, surface: Option<ReadingSurface>) -> Self {
    self.reading_surface = surface;
    self
}
```

- [ ] **Step 4: Extract page-position layout and add full-panel layout**

Move the existing page-position node construction into one helper used by normal layout and reading overlay layout:

```rust
fn layout_page_position(
    turns: PageTurns,
    page: u16,
    of: u16,
    band: Rect,
    metrics: &DisplayMetrics,
    layout: &mut Layout,
) {
    layout.nodes.push(LayoutNode {
        id: NodeId(0),
        rect: band,
        kind: LayoutKind::PagePosition,
        text_lines: vec![format!("{page} of {of}")],
    });
    let side = min(metrics.touch_target_default(), band.width / 3);
    if page > 1 {
        layout.nodes.push(LayoutNode {
            id: NodeId(0),
            rect: Rect { width: side, ..band },
            kind: LayoutKind::PagePrevious(turns.previous),
            text_lines: Vec::new(),
        });
    }
    if page < of {
        layout.nodes.push(LayoutNode {
            id: NodeId(0),
            rect: Rect {
                x: band.x + band.width - side,
                width: side,
                ..band
            },
            kind: LayoutKind::PageNext(turns.next),
            text_lines: Vec::new(),
        });
    }
}
```

Add `layout_reading_surface` and return it early from `layout_with_selected_font` when the field is present:

```rust
fn layout_reading_surface(
    &self,
    surface: ReadingSurface,
    metrics: &DisplayMetrics,
    chrome: &Chrome,
    prose: Face,
) -> Layout {
    let panel = Rect {
        x: 0,
        y: 0,
        width: metrics.width,
        height: metrics.height,
    };
    let mut layout = Layout {
        prose_face: prose,
        content: panel,
        page_turns: self
            .page_turns
            .map_or(PagingState::None, PagingState::Declared),
        hold: self.hold,
        ..Layout::default()
    };
    layout.nodes.push(LayoutNode {
        id: surface.id,
        rect: panel,
        kind: LayoutKind::Picture(surface.picture.handle),
        text_lines: Vec::new(),
    });

    if surface.chrome == ReadingChrome::Overlay {
        if let Some(top_bar) = &self.top_bar {
            layout_top_bar(top_bar, chrome, metrics, 0, &mut layout);
        }
        if let Some((turns, (page, of))) = self
            .page_turns
            .and_then(|turns| turns.drawable_position().map(|position| (turns, position)))
        {
            let height = metrics.page_position_band();
            let band = Rect {
                x: 0,
                y: metrics.height - height,
                width: metrics.width,
                height,
            };
            layout.nodes.push(LayoutNode {
                id: NodeId(0),
                rect: band,
                kind: LayoutKind::ReadingFooter,
                text_lines: Vec::new(),
            });
            layout_page_position(turns, page, of, band, metrics, &mut layout);
        }
    }

    if let Some(overlay) = &self.overlay {
        layout_overlay(overlay, metrics, prose, &mut layout);
        layout.page_turns = PagingState::SuppressedByOverlay;
        layout.hold = None;
    }
    layout
}
```

Do not reserve status, top-bar, margin, or page-position space in this branch. Keep the existing normal branch byte-for-byte equivalent except for calling `layout_page_position`.

- [ ] **Step 5: Render and validate the new surface**

Add `LayoutKind::ReadingFooter` and render it with opaque paper:

```rust
LayoutKind::TopBar | LayoutKind::NavBar | LayoutKind::ReadingFooter => {
    fill_clipped(surface, node.rect, tone::PAPER, clip);
}
```

Add the diagnostic variant and its `Display` arm:

```rust
ReadingSurfaceSize {
    expected: (u32, u32),
    actual: (u32, u32),
},
```

```rust
LayoutIssueKind::ReadingSurfaceSize { expected, actual } => write!(
    formatter,
    "{node}: reading surface is {} by {}, expected {} by {}",
    actual.0, actual.1, expected.0, expected.1
),
```

In `diagnose_screen`, register and validate the surface before flowing nodes:

```rust
if let Some(surface) = screen.reading_surface {
    check_identifier(surface.id, &mut identifiers, &mut issues);
    match pictures {
        Some(pictures) => check_picture(
            surface.id,
            surface.picture.handle,
            surface.picture.source,
            pictures,
            &mut issues,
        ),
        None if surface.picture.source.0 == 0 || surface.picture.source.1 == 0 => {
            issues.push(LayoutIssue {
                severity: DiagnosticSeverity::Error,
                node: Some(surface.id),
                kind: LayoutIssueKind::InvalidPictureSource,
                rect: None,
            });
        }
        None => {}
    }
    let expected = (
        u32::try_from(metrics.width).unwrap_or(0),
        u32::try_from(metrics.height).unwrap_or(0),
    );
    if surface.picture.source != expected {
        issues.push(LayoutIssue {
            severity: DiagnosticSeverity::Error,
            node: Some(surface.id),
            kind: LayoutIssueKind::ReadingSurfaceSize {
                expected,
                actual: surface.picture.source,
            },
            rect: None,
        });
    }
}
```

- [ ] **Step 6: Run UI tests**

Run: `rtk cargo test -p kobo-ui`

Expected: all `kobo-ui` tests pass, including unchanged reserved-bar and flowing-picture tests.

- [ ] **Step 7: Commit the UI contract**

```bash
rtk git add crates/kobo-ui/src/lib.rs
rtk git commit -m "feat(ui): add full-screen reading surfaces"
```

### Task 2: Carry reading surfaces through `kobo-protocol`

**Files:**
- Modify: `crates/kobo-protocol/src/lib.rs:42-49`
- Modify: `crates/kobo-protocol/src/lib.rs:3177-3259`
- Modify: `crates/kobo-protocol/src/lib.rs:4140-4226`
- Modify: `crates/kobo-protocol/src/lib.rs:5104-5274`
- Test: `crates/kobo-protocol/src/lib.rs` protocol round-trip tests near line 7290

**Interfaces:**
- Consumes: `kobo_ui::{ReadingChrome, ReadingSurface, TilePicture}` from Task 1.
- Produces: wire flags `0 = absent`, `1 = hidden`, `2 = overlay`; exact surface round-trip for the SDK/runtime boundary.

- [ ] **Step 1: Add failing round-trip and invalid-discriminant tests**

```rust
#[test]
fn screen_round_trip_preserves_reading_surface_and_chrome() {
    for chrome in [ReadingChrome::Hidden, ReadingChrome::Overlay] {
        let screen = Screen::new(17, Vec::new()).with_reading_surface(Some(
            ReadingSurface::new(
                NodeId(9),
                TilePicture::new(PictureHandle(42), 1072, 1448),
                chrome,
            ),
        ));
        assert_eq!(round_trip(screen.clone()), screen);
    }
}

#[test]
fn screen_rejects_unknown_reading_surface_flag() {
    let mut reader = Reader::new(&[3]);
    assert_eq!(
        decode_reading_surface(&mut reader),
        Err(ProtocolError::InvalidValue("reading surface flag"))
    );
}
```

Extract the new field decoder as `decode_reading_surface(&mut Reader<'_>)` so the invalid discriminant is tested directly rather than found by searching an encoded frame for a coincidental zero byte.

- [ ] **Step 2: Run the focused protocol test and verify failure**

Run: `rtk cargo test -p kobo-protocol screen_round_trip_preserves_reading_surface_and_chrome`

Expected: failure because screen length, encoder, or decoder does not handle `reading_surface`.

- [ ] **Step 3: Encode and decode the fixed wire shape**

Because this inserts a field into the encoded `Screen` shape, change `pub const VERSION: u8 = 10;` to `pub const VERSION: u8 = 11;`. Do not attempt mixed-version fallback; the existing handshake rejects mismatched application/runtime binaries before decoding frames.

Account for one flag byte in `encoded_screen_len`; when present, account for four `u32` values: node ID, picture handle, source width, and source height.

Encode immediately after `reading_font`:

```rust
match screen.reading_surface {
    None => output.push(0),
    Some(surface) => {
        output.push(match surface.chrome {
            ReadingChrome::Hidden => 1,
            ReadingChrome::Overlay => 2,
        });
        push_u32(output, surface.id.0);
        push_u32(output, surface.picture.handle.0);
        push_u32(output, surface.picture.source.0);
        push_u32(output, surface.picture.source.1);
    }
}
```

Decode through the helper in the same wire position:

```rust
fn decode_reading_surface(
    reader: &mut Reader<'_>,
) -> Result<Option<ReadingSurface>, ProtocolError> {
    Ok(match reader.u8()? {
        0 => None,
        mode @ (1 | 2) => Some(ReadingSurface::new(
            NodeId(reader.u32()?),
            TilePicture::new(
                PictureHandle(reader.u32()?),
                reader.u32()?,
                reader.u32()?,
            ),
            if mode == 1 {
                ReadingChrome::Hidden
            } else {
                ReadingChrome::Overlay
            },
        )),
        _ => return Err(ProtocolError::InvalidValue("reading surface flag")),
    })
}
```

Call `decode_reading_surface(reader)?` in `decode_screen` immediately after `reading_font`, then assign the result to `screen.reading_surface` before returning.

- [ ] **Step 4: Run protocol tests**

Run: `rtk cargo test -p kobo-protocol`

Expected: all protocol tests pass, including existing screen round trips and malformed-frame refusals.

- [ ] **Step 5: Commit protocol support**

```bash
rtk git add crates/kobo-protocol/src/lib.rs
rtk git commit -m "feat(protocol): carry reading surfaces"
```

### Task 3: Expose reading surfaces from `ScreenBuilder`

**Files:**
- Modify: `crates/kobo-sdk/src/lib.rs:20-29`
- Modify: `crates/kobo-sdk/src/lib.rs:443-486`
- Modify: `crates/kobo-sdk/src/lib.rs:1548-1610`
- Modify: `crates/kobo-sdk/src/lib.rs:2580-2595`
- Test: `crates/kobo-sdk/src/lib.rs` builder tests near line 5800

**Interfaces:**
- Consumes: Task 1's `ReadingChrome` and `ReadingSurface` plus existing `TilePicture` and deterministic node IDs.
- Produces: `ScreenBuilder::reading_surface(TilePicture, ReadingChrome) -> Self` for BOMTOON.

- [ ] **Step 1: Add a failing builder test**

```rust
#[test]
fn builder_declares_one_semantic_reading_surface() {
    let picture = TilePicture::new(PictureHandle(7), 1072, 1448);
    let screen = ScreenBuilder::new("comic")
        .top_bar("Episode One")
        .reading_surface(picture, ReadingChrome::Overlay)
        .page_turns("previous", "next")
        .reading_menu("chrome")
        .page_position(2, 9)
        .build();

    let surface = screen.reading_surface.expect("reading surface");
    assert_eq!(surface.picture, picture);
    assert_eq!(surface.chrome, ReadingChrome::Overlay);
    assert!(surface.id.0 > 0);
    assert!(screen.nodes.is_empty());
}
```

- [ ] **Step 2: Run the focused SDK test and verify failure**

Run: `rtk cargo test -p kobo-sdk builder_declares_one_semantic_reading_surface`

Expected: compile failure because `ScreenBuilder::reading_surface` is absent.

- [ ] **Step 3: Add the builder field and method**

Re-export `ReadingChrome` and `ReadingSurface` with the existing `kobo_ui` types. Add `reading_surface: Option<ReadingSurface>` to `ScreenBuilder`, initialize it to `None`, and emit it from `build`.

```rust
#[must_use]
pub fn reading_surface(mut self, picture: TilePicture, chrome: ReadingChrome) -> Self {
    let id = self.next_id();
    self.reading_surface = Some(ReadingSurface::new(id, picture, chrome));
    self
}
```

A second call replaces the first surface rather than creating an unreachable second surface. Node ID allocation remains deterministic because each declaration consumes one ID.

- [ ] **Step 4: Run SDK tests**

Run: `rtk cargo test -p kobo-sdk`

Expected: all SDK tests pass.

- [ ] **Step 5: Commit the SDK API**

```bash
rtk git add crates/kobo-sdk/src/lib.rs
rtk git commit -m "feat(sdk): expose reading surfaces"
```

### Task 4: Raise the shared image ceiling and add exact-width scaling

**Files:**
- Modify: `crates/kobo-image/src/lib.rs:20-38`
- Modify: `crates/kobo-image/src/lib.rs:189-333`
- Test: `crates/kobo-image/src/lib.rs` test module

**Interfaces:**
- Consumes: existing `Picture`, `ImageError`, `checked_pixels`, and Lanczos3 resizing.
- Produces: `MAX_PIXELS = 7_000_000`, `width_scaled_size((u32, u32), u32)`, and `Picture::scale_to_width(u32)` for manifest planning and decoded images.

- [ ] **Step 1: Add failing boundary and width-scaling tests**

```rust
#[test]
fn shared_pixel_limit_accepts_exactly_seven_million() {
    assert!(Picture::from_grey(2_000, 3_500, vec![0; 7_000_000]).is_ok());
    assert_eq!(
        Picture::from_grey(1, 7_000_001, vec![0; 7_000_001]),
        Err(ImageError::TooManyPixels { pixels: 7_000_001 })
    );
}

#[test]
fn exact_width_size_and_resize_share_rounding() {
    assert_eq!(width_scaled_size((4, 8), 2), Ok((2, 4)));
    let source = Picture::from_grey(4, 8, (0..32).collect()).expect("source");
    let scaled = source.scale_to_width(2).expect("scale");
    assert_eq!((scaled.width(), scaled.height()), (2, 4));
}

#[test]
fn width_scaling_is_explicitly_allowed_to_enlarge() {
    let source = Picture::from_grey(1, 1, vec![127]).expect("source");
    let scaled = source.scale_to_width(3).expect("scale");
    assert_eq!((scaled.width(), scaled.height()), (3, 3));
}

#[test]
fn width_scaling_rejects_target_allocation_before_resize() {
    assert!(matches!(
        width_scaled_size((1, 7_000_000), 2),
        Err(ImageError::TooManyPixels { .. })
    ));
}
```

- [ ] **Step 2: Run focused tests and verify the old ceiling/API failure**

Run: `rtk cargo test -p kobo-image exact_width_size_and_resize_share_rounding`

Expected: compile failure because `width_scaled_size` and `scale_to_width` are absent.

- [ ] **Step 3: Implement one shared size calculation and exact-width resize**

Set:

```rust
pub const MAX_PIXELS: u64 = 7_000_000;
```

Add the public dimension helper:

```rust
pub fn width_scaled_size(
    source: (u32, u32),
    target_width: u32,
) -> Result<(u32, u32), ImageError> {
    if source.0 == 0 || source.1 == 0 || target_width == 0 {
        return Err(ImageError::EmptyBox);
    }
    let target_height = u32::try_from(
        (u64::from(target_width) * u64::from(source.1)) / u64::from(source.0),
    )
    .unwrap_or(u32::MAX)
    .max(1);
    checked_pixels(target_width, target_height)?;
    Ok((target_width, target_height))
}
```

Add the method and use the helper before allocating:

```rust
pub fn scale_to_width(&self, target_width: u32) -> Result<Self, ImageError> {
    let (width, height) = width_scaled_size((self.width, self.height), target_width)?;
    if width == self.width && height == self.height {
        return Ok(self.clone());
    }
    let source = image::GrayImage::from_raw(self.width, self.height, self.grey.clone())
        .ok_or_else(|| ImageError::Undecodable("the picture is not its own size".to_owned()))?;
    let scaled = image::imageops::resize(&source, width, height, FilterType::Lanczos3);
    Self::from_grey(width, height, scaled.into_raw())
}
```

Update the `MAX_PIXELS` documentation to name the 6,553,600-pixel observed BOMTOON maximum and the 7,000,000 hard boundary. Do not alter `fit`, `fit_enlarging`, `cover`, decode, alpha, grayscale, or dithering behavior.

- [ ] **Step 4: Run image tests**

Run: `rtk cargo test -p kobo-image`

Expected: all image tests pass, including JPEG, PNG, WebP, orientation, alpha, fit, cover, and new exact-width cases.

- [ ] **Step 5: Commit the image policy**

```bash
rtk git add crates/kobo-image/src/lib.rs
rtk git commit -m "feat(image): support tall comic sources"
```

### Task 5: Admit only the exact credentialed manifest route

**Files:**
- Modify: `crates/kobo-net/src/lib.rs:209-301`
- Test: `crates/kobo-net/src/lib.rs:1362-1562`

**Interfaces:**
- Consumes: existing `credential_allowed`, `has_origin`, `RequestMethod`, and BOMTOON access-token policy.
- Produces: exact allow-list recognition for the manifest GET; no permission for CDN URLs or POST.

- [ ] **Step 1: Add a failing exact-route policy test**

```rust
#[test]
fn bomtoon_image_manifest_requires_exact_get_bearer_aliases_and_query() {
    use kobo_protocol::Credential;

    let access = Credential::bearer("bomtoon-access-token");
    let exact = "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080";
    assert!(super::credential_allowed(
        "bomtoon",
        &access,
        RequestMethod::Get,
        exact
    ));

    for url in [
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/images//ep-1?imageWidth=1080",
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/?imageWidth=1080",
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter/q/ep-1?imageWidth=1080",
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep/1?imageWidth=1080",
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/%2f?imageWidth=1080",
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1",
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=720",
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080&extra=true",
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080&imageWidth=1080",
        "http://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080",
        "https://www.bomtoon.tw:444/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080",
        "https://attacker.invalid/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080",
    ] {
        assert!(!super::credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Get,
            url
        ));
    }
    assert!(!super::credential_allowed(
        "bomtoon",
        &access,
        RequestMethod::Post,
        exact
    ));
}
```

- [ ] **Step 2: Run the policy test and verify denial**

Run: `rtk cargo test -p kobo-net bomtoon_image_manifest_requires_exact_get_bearer_aliases_and_query`

Expected: the exact URL assertion fails because the route is not yet allowed.

- [ ] **Step 3: Reuse one alias predicate and add the manifest route**

```rust
fn bomtoon_alias(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn bomtoon_images_url(url: &str) -> bool {
    const PREFIX: &str =
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/";
    const SUFFIX: &str = "?imageWidth=1080";

    url.strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix(SUFFIX))
        .and_then(|aliases| aliases.split_once('/'))
        .is_some_and(|(content, episode)| {
            !episode.contains('/') && bomtoon_alias(content) && bomtoon_alias(episode)
        })
}
```

Use `bomtoon_alias` from the existing content route. Add `bomtoon_images_url(url)` to the existing BOMTOON bearer GET disjunction. Keep `has_origin` and method/credential checks unchanged.

- [ ] **Step 4: Run network policy tests**

Run: `rtk cargo test -p kobo-net`

Expected: all network tests pass, including the existing session, viewer, library, recent, and content denials.

- [ ] **Step 5: Commit the capability change**

```bash
rtk git add crates/kobo-net/src/lib.rs
rtk git commit -m "feat(net): allow BOMTOON image manifests"
```

### Task 6: Model and strictly parse plain image manifests

**Files:**
- Modify: `apps/bomtoon/Cargo.toml:9-12`
- Modify: `apps/bomtoon/src/model.rs:1-70`
- Modify: `apps/bomtoon/src/parse.rs:1-198`
- Test: `apps/bomtoon/src/model.rs` and `apps/bomtoon/src/parse.rs` test modules

**Interfaces:**
- Consumes: `http::Uri`, `kobo_image::MAX_PIXELS`, existing `kobo_json::Value`, and `ParseError` conventions.
- Produces: `EpisodeImage { order, width, height, path, url }`, `PurchaseState::is_readable`, and `parse::images(&[u8])` for Tasks 7 and 8.

- [ ] **Step 1: Add dependencies and failing model/parser tests**

Add:

```toml
http = "1.3"
kobo-image = { path = "../../crates/kobo-image" }
```

Add model eligibility coverage:

```rust
#[test]
fn only_owned_and_free_episodes_are_readable() {
    assert!(PurchaseState::Owned.is_readable());
    assert!(PurchaseState::Free.is_readable());
    assert!(!PurchaseState::Sample.is_readable());
    assert!(!PurchaseState::NotOwned.is_readable());
    assert!(!PurchaseState::Other("RENTAL".to_owned()).is_readable());
}
```

Add parser helpers and representative strict cases:

```rust
fn signed(path: &str, policy: &str, signature: &str, key: &str) -> String {
    format!(
        "https://image.balcony.studio{path}?Policy={policy}&Signature={signature}&Key-Pair-Id={key}"
    )
}

fn manifest(entries: &[String]) -> Vec<u8> {
    format!("{{\"result\":\"SUCCESS\",\"data\":[{}]}}", entries.join(",")).into_bytes()
}

fn image(order: usize, width: u32, height: u32, url: &str) -> String {
    format!(
        "{{\"orderNo\":{order},\"width\":{width},\"height\":{height},\"imagePath\":\"{url}\",\"line\":null,\"point\":null}}"
    )
}

#[test]
fn plain_manifest_requires_contiguous_order_and_exact_signed_urls() {
    let bytes = manifest(&[
        image(1, 1280, 5000, &signed("/tw/ep/one.webp", "p1", "s1", "k1")),
        image(2, 1280, 5120, &signed("/tw/ep/two.webp", "p2", "s2", "k2")),
    ]);
    let parsed = images(&bytes).expect("plain manifest");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].order, 1);
    assert_eq!(parsed[1].path, "/tw/ep/two.webp");
}

#[test]
fn scrambled_manifest_is_explicitly_unsupported() {
    let bytes = br#"{"result":"SUCCESS","data":[{"orderNo":1,"width":1280,"height":5000,"imagePath":"https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k","line":4,"point":"cipher"}]}"#;
    assert!(matches!(images(bytes), Err(ParseError::UnsupportedScramble)));
}
```

Add the remaining boundary tables with concrete fixtures:

```rust
fn signed_of_len(length: usize) -> String {
    let prefix = "https://image.balcony.studio/tw/ep/one.webp?Policy=";
    let suffix = "&Signature=s&Key-Pair-Id=k";
    assert!(length >= prefix.len() + suffix.len());
    format!(
        "{prefix}{}{suffix}",
        "p".repeat(length - prefix.len() - suffix.len())
    )
}

#[test]
fn manifest_count_and_url_length_are_bounded() {
    assert!(images(&manifest(&[])).is_err());
    let too_many = (1..=257)
        .map(|order| image(order, 1, 1, &signed("/tw/ep/one.webp", "p", "s", "k")))
        .collect::<Vec<_>>();
    assert!(images(&manifest(&too_many)).is_err());
    assert!(images(&manifest(&[image(1, 1, 1, &signed_of_len(1024))])).is_ok());
    assert!(images(&manifest(&[image(1, 1, 1, &signed_of_len(1025))])).is_err());
}

#[test]
fn manifest_dimensions_and_order_are_strict() {
    let url = signed("/tw/ep/one.webp", "p", "s", "k");
    assert!(images(&manifest(&[image(1, 2_000, 3_500, &url)])).is_ok());
    assert!(images(&manifest(&[image(1, 1, 7_000_001, &url)])).is_err());
    assert!(images(&manifest(&[image(1, 0, 1, &url)])).is_err());
    for entries in [
        vec![image(0, 1, 1, &url)],
        vec![image(2, 1, 1, &url)],
        vec![image(1, 1, 1, &url), image(1, 1, 1, &url)],
        vec![image(1, 1, 1, &url), image(3, 1, 1, &url)],
    ] {
        assert!(images(&manifest(&entries)).is_err());
    }
}

#[test]
fn signed_url_rejects_every_origin_path_and_query_mutation() {
    for url in [
        "http://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
        "https://attacker.invalid/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
        "https://image.balcony.studio:444/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
        "https://user@image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
        "https://image.balcony.studio/other/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
        "https://image.balcony.studio/tw/ep/one.png?Policy=p&Signature=s&Key-Pair-Id=k",
        "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k#fragment",
        "https://image.balcony.studio/tw/ep/one.webp?Signature=s&Key-Pair-Id=k",
        "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Policy=q&Signature=s&Key-Pair-Id=k",
        "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k&extra=x",
        "https://image.balcony.studio/tw/ep/one.webp?Policy=&Signature=s&Key-Pair-Id=k",
    ] {
        assert!(images(&manifest(&[image(1, 1, 1, url)])).is_err());
    }
}

#[test]
fn signed_query_order_is_not_semantic() {
    let url = "https://image.balcony.studio/tw/ep/one.webp?Signature=s&Key-Pair-Id=k&Policy=p";
    assert!(images(&manifest(&[image(1, 1, 1, url)])).is_ok());
}

#[test]
fn image_fields_must_exist_with_their_exact_types() {
    for bytes in [
        br#"{"result":"SUCCESS","data":[{}]}"#.as_slice(),
        br#"{"result":"SUCCESS","data":[{"orderNo":"1","width":1,"height":1,"imagePath":"x","line":null,"point":null}]}"#.as_slice(),
        br#"{"result":"SUCCESS","data":[{"orderNo":1,"width":4294967296,"height":1,"imagePath":"x","line":null,"point":null}]}"#.as_slice(),
    ] {
        assert!(images(bytes).is_err());
    }
}
```

- [ ] **Step 2: Run parser tests and verify missing API failure**

Run: `rtk cargo test -p kobo-bomtoon parse::tests`

Expected: compile failure because `images`, `EpisodeImage`, and `UnsupportedScramble` are absent.

- [ ] **Step 3: Add the image model and eligibility method**

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpisodeImage {
    pub order: usize,
    pub width: u32,
    pub height: u32,
    pub path: String,
    pub url: String,
}

impl PurchaseState {
    #[must_use]
    pub const fn is_readable(&self) -> bool {
        matches!(self, Self::Owned | Self::Free)
    }
}
```

- [ ] **Step 4: Extend `ParseError` and implement strict parsing**

Add:

```rust
UnsupportedScramble,
```

Display it as `this episode uses unsupported scrambled images` and return no source error.

Use these exact bounds:

```rust
const MAX_IMAGES: usize = 256;
const MAX_SIGNED_URL_BYTES: usize = 1024;
```

Implement:

```rust
pub fn images(bytes: &[u8]) -> Result<Vec<EpisodeImage>, ParseError> {
    let root = parse_json(bytes)?;
    if string(&root, "result", "result")? != "SUCCESS" {
        return Err(ParseError::InvalidValue("result"));
    }
    let values = field(&root, "data", "data")?
        .as_array()
        .ok_or(ParseError::WrongType("data"))?;
    if values.is_empty() || values.len() > MAX_IMAGES {
        return Err(ParseError::InvalidValue("image count"));
    }

    values
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let order = unsigned(item, "orderNo", "image.orderNo")?;
            if order != index + 1 {
                return Err(ParseError::InvalidValue("image ordering"));
            }
            let width = positive_u32(item, "width", "image.width")?;
            let height = positive_u32(item, "height", "image.height")?;
            let pixels = u64::from(width) * u64::from(height);
            if pixels > kobo_image::MAX_PIXELS {
                return Err(ParseError::InvalidValue("image dimensions"));
            }
            require_null(item, "line", "image.line")?;
            require_null(item, "point", "image.point")?;
            let url = string(item, "imagePath", "image.imagePath")?;
            let path = signed_image_path(url)?;
            Ok(EpisodeImage {
                order,
                width,
                height,
                path,
                url: url.to_owned(),
            })
        })
        .collect()
}
```

Implement `positive_u32` with `i64 -> u32` checked conversion and a nonzero check. `require_null` returns `UnsupportedScramble` for every present non-null value and `Missing` when absent.

Implement `signed_image_path` with `http::Uri`. Check byte length first. Require scheme `https`, authority with no `@`, exact host, port `None` or `443`, no fragment by successful URI parsing, and a path that starts `/tw/ep/` and ends `.webp`.

Split the raw query on `&`, then split each pair once at `=` so a signature value may itself contain `=`. Accept `Policy`, `Signature`, and `Key-Pair-Id` in any order. Track one slot per key; reject empty values, duplicate keys, unknown keys, missing keys, and empty pairs. Return `uri.path().to_owned()` only after all three slots are present.

- [ ] **Step 5: Run model and parser tests**

Run: `rtk cargo test -p kobo-bomtoon model::tests parse::tests`

Expected: all model and parser tests pass without printing URL values.

- [ ] **Step 6: Commit the manifest contract**

```bash
rtk git add apps/bomtoon/Cargo.toml apps/bomtoon/src/model.rs apps/bomtoon/src/parse.rs
rtk git commit -m "feat(bomtoon): parse plain image manifests"
```

### Task 7: Build credentialed manifest and uncredentialed image tasks

**Files:**
- Modify: `apps/bomtoon/src/api.rs:1-198`
- Modify: `apps/catalog.json:15-25`
- Test: `apps/bomtoon/src/api.rs` test module

**Interfaces:**
- Consumes: Task 5's credential allow-list and Task 6's `kobo-image` dependency.
- Produces: `api::images(content, episode) -> Task` and `api::image(url) -> Task` for Task 8.

- [ ] **Step 1: Add failing exact-task tests**

```rust
#[test]
fn image_manifest_uses_exact_bearer_route_headers_and_ceiling() {
    let Task::Fetch {
        url,
        offset,
        max_bytes,
        credential,
        headers,
    } = images("hunter_q", "ep-1")
    else {
        panic!("expected manifest fetch");
    };
    assert_eq!(url, "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080");
    assert_eq!(offset, 0);
    assert_eq!(max_bytes, 512 * 1024);
    assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
    assert!(headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("x-referer")
            && header.value == "https://www.bomtoon.tw/viewer/hunter_q/ep-1"
    }));
}

#[test]
fn signed_image_fetch_has_no_account_credential() {
    let url = "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k";
    let Task::Fetch {
        url: actual,
        offset,
        max_bytes,
        credential,
        headers,
    } = image(url)
    else {
        panic!("expected image fetch");
    };
    assert_eq!(actual, url);
    assert_eq!(offset, 0);
    assert_eq!(max_bytes, kobo_image::MAX_SOURCE_BYTES as u32);
    assert_eq!(credential, None);
    assert!(headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("accept") && header.value == "image/webp"
    }));
}
```

- [ ] **Step 2: Run API tests and verify missing functions**

Run: `rtk cargo test -p kobo-bomtoon api::tests`

Expected: compile failure because `images` and `image` are absent.

- [ ] **Step 3: Implement the two tasks without broadening existing helpers**

```rust
const IMAGES_URL: &str =
    "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/";
const IMAGE_MANIFEST_BYTES: u32 = 512 * 1024;

pub fn images(content: &str, episode: &str) -> Task {
    let mut headers = balcony_headers();
    headers.push(Header::new(
        "x-referer",
        format!("https://www.bomtoon.tw/viewer/{content}/{episode}"),
    ));
    fetch(
        format!("{IMAGES_URL}{content}/{episode}?imageWidth=1080"),
        IMAGE_MANIFEST_BYTES,
        Credential::bearer("bomtoon-access-token"),
        headers,
    )
}

pub fn image(url: &str) -> Task {
    Task::Fetch {
        url: url.to_owned(),
        offset: 0,
        max_bytes: kobo_image::MAX_SOURCE_BYTES as u32,
        credential: None,
        headers: response_headers("image/webp"),
    }
}
```

Keep signed URLs out of assertion messages and debug formatting.

- [ ] **Step 4: Update catalog metadata**

Change the BOMTOON catalog entry to version `0.4.0` and summary `Read owned and free BOMTOON episodes on your Kobo.` Keep its sole capability `network`.

- [ ] **Step 5: Run API tests**

Run: `rtk cargo test -p kobo-bomtoon api::tests`

Expected: all API tests pass; existing content/library/recent task tests remain unchanged.

- [ ] **Step 6: Commit task construction and metadata**

```bash
rtk git add apps/bomtoon/src/api.rs apps/catalog.json
rtk git commit -m "feat(bomtoon): request episode images"
```

### Task 8: Integrate reader state, slicing, navigation, retry, and cleanup

**Files:**
- Modify: `apps/bomtoon/src/main.rs:5-544`
- Test: `apps/bomtoon/src/main.rs:546-1047`

**Interfaces:**
- Consumes: `ReadingChrome`/`ScreenBuilder::reading_surface` from Tasks 1-3, exact-width image operations from Task 4, `EpisodeImage`/`parse::images` from Task 6, and task constructors from Task 7.
- Produces: the complete observable reader flow, including account-state separation and picture-handle lifecycle.

- [ ] **Step 1: Add failing state-machine and slicing tests**

Replace `CONTENT_RESPONSE` with one row for each eligibility class:

```rust
const CONTENT_RESPONSE: &[u8] = br#"{
    "result":"SUCCESS",
    "data":{"episodes":[
        {"alias":"ep-1","title":"Episode One","isSample":false,"purchaseStatus":"POSSESSION"},
        {"alias":"ep-2","title":"Episode Two","isSample":false,"purchaseStatus":null,"paid":false},
        {"alias":"sample","title":"Sample","isSample":true,"purchaseStatus":null},
        {"alias":"locked","title":"Locked","isSample":false,"purchaseStatus":null,"paid":true}
    ]}
}"#;

const TINY_WEBP: &[u8] = &[
    82, 73, 70, 70, 36, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 32, 24, 0, 0, 0,
    48, 1, 0, 157, 1, 42, 1, 0, 1, 0, 1, 64, 38, 37, 164, 0, 3, 112, 0, 254,
    251, 148, 0, 0,
];

fn image_manifest(path: &str, policy: &str) -> Vec<u8> {
    format!(
        "{{\"result\":\"SUCCESS\",\"data\":[{{\"orderNo\":1,\"width\":1,\"height\":1,\"imagePath\":\"https://image.balcony.studio{path}?Policy={policy}&Signature=s&Key-Pair-Id=k\",\"line\":null,\"point\":null}}]}}"
    )
    .into_bytes()
}

fn reader_waiting_for_manifest() -> (AppRunner<Bomtoon>, TaskId) {
    let (mut runner, _) = loaded_library();
    let commands = runner.action(action_id("comic-0"));
    let (content_task, _) = only_spawn(&commands);
    runner.task_outcome(
        content_task,
        TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
    );
    let commands = runner.action(action_id("episode-0"));
    let (manifest_task, _) = only_spawn(&commands);
    (runner, manifest_task)
}

fn reader_waiting_for_first_image() -> (AppRunner<Bomtoon>, TaskId) {
    let (mut runner, manifest_task) = reader_waiting_for_manifest();
    let commands = runner.task_outcome(
        manifest_task,
        TaskOutcome::Completed(image_manifest("/tw/ep/one.webp", "p1")),
    );
    let (image_task, _) = only_spawn(&commands);
    (runner, image_task)
}
```

Add tests with these observable assertions:
```rust
#[test]
fn only_owned_and_free_episode_rows_are_actions() {
    let (mut runner, _) = loaded_library();
    let commands = runner.action(action_id("comic-0"));
    let (content_task, _) = only_spawn(&commands);
    let commands = runner.task_outcome(
        content_task,
        TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
    );
    let screen = last_screen(&commands);
    let actions = screen
        .nodes
        .iter()
        .filter_map(|node| match node {
            Node::Button { action, .. } => Some(*action),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(actions.contains(&action_id("episode-0")));
    assert!(actions.contains(&action_id("episode-1")));
    assert!(!actions.contains(&action_id("episode-2")));
    assert!(!actions.contains(&action_id("episode-3")));
}

#[test]
fn owned_episode_opens_full_screen_reader_with_hidden_chrome() {
    let (mut runner, _) = loaded_library();
    let commands = runner.action(action_id("comic-0"));
    let (content_task, _) = only_spawn(&commands);
    let commands = runner.task_outcome(
        content_task,
        TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
    );
    let episode_screen = last_screen(&commands);
    assert!(format!("{episode_screen:?}").contains("Episode One"));

    let commands = runner.action(action_id("episode-0"));
    let (manifest_task, manifest_work) = only_spawn(&commands);
    assert_eq!(manifest_work, api::images("hunter_q", "ep-1"));
    let commands = runner.task_outcome(
        manifest_task,
        TaskOutcome::Completed(image_manifest("/tw/ep/one.webp", "p1")),
    );
    let (image_task, image_work) = only_spawn(&commands);
    assert!(matches!(image_work, Task::Fetch { credential: None, .. }));

    let commands = runner.task_outcome(
        image_task,
        TaskOutcome::Completed(TINY_WEBP.to_vec()),
    );
    let screen = last_screen(&commands);
    let surface = screen.reading_surface.expect("reading surface");
    assert_eq!(surface.chrome, ReadingChrome::Hidden);
    assert_eq!(surface.picture.source, (1072, 1448));
    assert!(commands.iter().any(|command| matches!(command, Command::PutPicture { .. })));
    assert_fits(&screen);
}

#[test]
fn cdn_unauthorized_refreshes_once_without_expiring_account() {
    let (mut runner, image_task) = reader_waiting_for_first_image();
    let commands = runner.task_outcome(
        image_task,
        TaskOutcome::Failed(TaskError::Unauthorized),
    );
    assert_eq!(runner.app().account, AccountState::Active);
    let (refresh_task, refresh_work) = only_spawn(&commands);
    assert_eq!(refresh_work, api::images("hunter_q", "ep-1"));

    let commands = runner.task_outcome(
        refresh_task,
        TaskOutcome::Completed(image_manifest("/tw/ep/one.webp", "p2")),
    );
    let (retry_task, retry_work) = only_spawn(&commands);
    let Task::Fetch { url, credential, .. } = retry_work else {
        panic!("expected image retry");
    };
    assert!(url.contains("Policy=p2"));
    assert_eq!(credential, None);

    let commands = runner.task_outcome(
        retry_task,
        TaskOutcome::Failed(TaskError::Unauthorized),
    );
    assert_eq!(runner.app().account, AccountState::Active);
    assert!(commands
        .iter()
        .all(|command| !matches!(command, Command::Spawn { .. })));
}

#[test]
fn row_slices_cover_source_once_and_pad_only_the_final_page() {
    let source = Picture::from_grey(2, 5, (0..10).collect()).expect("source");
    let first = slice_rows(&source, 0, 3).expect("first");
    let second = slice_rows(&source, 1, 3).expect("second");
    assert_eq!(first.grey(), &[0, 1, 2, 3, 4, 5]);
    assert_eq!(second.grey(), &[6, 7, 8, 9, 255, 255]);
}
```

Extend the test imports with `Node`, `PictureHandle`, `ReadingChrome`, and `TilePicture`. Add deterministic seeded-state coverage for gestures, source transitions, failures, stale tasks, and handle cleanup:

```rust
fn seeded_reader(
    pages_per_source: Vec<usize>,
    location: PageLocation,
    chrome_visible: bool,
) -> AppRunner<Bomtoon> {
    let width = CLARA_BW_METRICS.width as u32;
    let panel_height = CLARA_BW_METRICS.height as u32;
    let source_height = panel_height * pages_per_source[location.source] as u32;
    let images = pages_per_source
        .iter()
        .enumerate()
        .map(|(index, pages)| EpisodeImage {
            order: index + 1,
            width,
            height: panel_height * *pages as u32,
            path: format!("/tw/ep/{index}.webp"),
            url: format!(
                "https://image.balcony.studio/tw/ep/{index}.webp?Policy=p&Signature=s&Key-Pair-Id=k"
            ),
        })
        .collect();
    let total_pages = pages_per_source.iter().sum::<usize>() as u16;
    AppRunner::with_metrics(
        Bomtoon {
            view: View::Reader,
            selected_content_alias: "hunter_q".to_owned(),
            reader_selection: Some(EpisodeSelection {
                content_alias: "hunter_q".to_owned(),
                episode_alias: "ep-1".to_owned(),
                title: "Episode One".to_owned(),
            }),
            reader: Some(ReaderState {
                images,
                pages_per_source,
                location,
                total_pages,
                source: Some(
                    Picture::from_grey(
                        width,
                        source_height,
                        vec![127; width as usize * source_height as usize],
                    )
                    .expect("scaled source"),
                ),
                picture: Some(TilePicture::new(PictureHandle(7), width, panel_height)),
                chrome_visible,
                refreshed_current_image: false,
            }),
            ..Bomtoon::default()
        },
        CLARA_BW_METRICS,
    )
}

#[test]
fn center_toggles_chrome_and_boundary_noop_preserves_it() {
    let mut runner = seeded_reader(
        vec![1],
        PageLocation { source: 0, slice: 0 },
        false,
    );
    let commands = runner.action(action_id(READER_CHROME));
    assert_eq!(
        last_screen(&commands).reading_surface.expect("surface").chrome,
        ReadingChrome::Overlay
    );
    runner.action(action_id(READER_NEXT));
    assert!(runner.app().reader.as_ref().expect("reader").chrome_visible);
    let commands = runner.action(action_id(READER_CHROME));
    assert_eq!(
        last_screen(&commands).reading_surface.expect("surface").chrome,
        ReadingChrome::Hidden
    );
}

#[test]
fn successful_page_turn_hides_chrome_and_replaces_handle_in_order() {
    let mut runner = seeded_reader(
        vec![2],
        PageLocation { source: 0, slice: 0 },
        true,
    );
    let commands = runner.action(action_id(READER_NEXT));
    let reader = runner.app().reader.as_ref().expect("reader");
    assert_eq!(reader.location, PageLocation { source: 0, slice: 1 });
    assert!(!reader.chrome_visible);
    let put = commands.iter().position(|command| matches!(command, Command::PutPicture { .. })).expect("PutPicture");
    let set = commands.iter().position(|command| matches!(command, Command::SetScreen(_))).expect("SetScreen");
    let drop = commands.iter().position(|command| matches!(command, Command::DropPicture(_))).expect("DropPicture");
    assert!(put < set && set < drop);
}

#[test]
fn source_boundary_targets_are_exact_and_reversible() {
    let pages = [2, 3];
    assert_eq!(
        next_location(&pages, PageLocation { source: 0, slice: 1 }),
        Some(PageLocation { source: 1, slice: 0 })
    );
    assert_eq!(
        previous_location(&pages, PageLocation { source: 1, slice: 0 }),
        Some(PageLocation { source: 0, slice: 1 })
    );
    assert_eq!(previous_location(&pages, PageLocation { source: 0, slice: 0 }), None);
    assert_eq!(next_location(&pages, PageLocation { source: 1, slice: 2 }), None);
}

#[test]
fn refreshed_manifest_rejects_changed_asset_identity() {
    for refreshed in [
        image_manifest("/tw/ep/different.webp", "p2"),
        String::from_utf8(image_manifest("/tw/ep/one.webp", "p2"))
            .expect("JSON")
            .replace("\"height\":1", "\"height\":2")
            .into_bytes(),
    ] {
        let (mut runner, image_task) = reader_waiting_for_first_image();
        let commands = runner.task_outcome(
            image_task,
            TaskOutcome::Failed(TaskError::Unauthorized),
        );
        let (refresh_task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(
            refresh_task,
            TaskOutcome::Completed(refreshed),
        );
        assert!(runner.app().problem.is_some());
        assert!(commands
            .iter()
            .all(|command| !matches!(command, Command::Spawn { .. })));
    }
}

#[test]
fn manifest_credentials_alone_change_account_state() {
    for (error, expected) in [
        (TaskError::NoCredential, AccountState::SignedOut),
        (TaskError::Unauthorized, AccountState::Expired),
    ] {
        let (mut runner, manifest_task) = reader_waiting_for_manifest();
        runner.task_outcome(manifest_task, TaskOutcome::Failed(error));
        assert_eq!(runner.app().account, expected);
    }
}

#[test]
fn stale_image_outcome_cannot_mutate_reader() {
    let (mut runner, image_task) = reader_waiting_for_first_image();
    let before = runner.app().pending;
    let commands = runner.task_outcome(
        TaskId(image_task.0 + 1),
        TaskOutcome::Completed(TINY_WEBP.to_vec()),
    );
    assert!(commands.is_empty());
    assert_eq!(runner.app().pending, before);
}

#[test]
fn back_and_logout_release_reader_state_and_picture() {
    let mut runner = seeded_reader(
        vec![1],
        PageLocation { source: 0, slice: 0 },
        true,
    );
    let commands = runner.action(ActionId::BACK);
    assert_eq!(runner.app().view, View::Episodes);
    assert!(runner.app().reader.is_none());
    let set = commands.iter().position(|command| matches!(command, Command::SetScreen(_))).expect("SetScreen");
    let drop = commands.iter().position(|command| matches!(command, Command::DropPicture(_))).expect("DropPicture");
    assert!(set < drop);

    let mut runner = seeded_reader(
        vec![1],
        PageLocation { source: 0, slice: 0 },
        false,
    );
    runner.app_mut().pending = Some(Pending::Logout);
    runner.app_mut().task = Some(TaskId(77));
    let commands = runner.task_outcome(TaskId(77), TaskOutcome::Completed(Vec::new()));
    assert!(runner.app().reader.is_none());
    assert!(commands
        .iter()
        .any(|command| matches!(command, Command::DropPicture(_))));
}

#[test]
fn image_failure_retry_stays_on_selected_episode() {
    let (mut runner, image_task) = reader_waiting_for_first_image();
    runner.task_outcome(
        image_task,
        TaskOutcome::Completed(vec![0, 1, 2, 3]),
    );
    let commands = runner.action(action_id(RETRY));
    let (_, work) = only_spawn(&commands);
    assert!(matches!(work, Task::Fetch { credential: None, .. }));
    let selection = runner.app().reader_selection.as_ref().expect("selection");
    assert_eq!(selection.content_alias, "hunter_q");
    assert_eq!(selection.episode_alias, "ep-1");
}
```

- [ ] **Step 2: Run the reader test and verify the missing state failure**

Run: `rtk cargo test -p kobo-bomtoon owned_episode_opens_full_screen_reader_with_hidden_chrome`

Expected: failure because episode rows are inert and reader pending/state variants are absent.


- [ ] **Step 3: Add reader state types and pure geometry helpers**
Extend production imports with `kobo_image::{Picture, PANEL_GREYS}`, `kobo_sdk::{PictureHandle, ReadingChrome, TilePicture}`, and `model::EpisodeImage`. Add constants:

```rust
const READER_PREVIOUS: &str = "reader-previous";
const READER_NEXT: &str = "reader-next";
const READER_CHROME: &str = "reader-chrome";
```

Extend `View` with `Reader`. Extend `Pending` with `Manifest`, `ManifestRefresh(PageLocation)`, and `Image(PageLocation)`. Add these types:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PageLocation {
    source: usize,
    slice: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Retry {
    #[default]
    Restart,
    Manifest,
    Image(PageLocation),
    Slice,
}

struct EpisodeSelection {
    content_alias: String,
    episode_alias: String,
    title: String,
}

struct ReaderState {
    images: Vec<EpisodeImage>,
    pages_per_source: Vec<usize>,
    location: PageLocation,
    total_pages: u16,
    source: Option<Picture>,
    picture: Option<TilePicture>,
    chrome_visible: bool,
    refreshed_current_image: bool,
}

impl ReaderState {
    fn global_page(&self) -> u16 {
        let before = self.pages_per_source[..self.location.source]
            .iter()
            .sum::<usize>();
        u16::try_from(before + self.location.slice + 1).unwrap_or(self.total_pages)
    }
}
```

Add `selected_content_alias: String`, `reader_selection: Option<EpisodeSelection>`, `reader: Option<ReaderState>`, `retry: Retry`, and `next_picture_handle: u32` to `Bomtoon`. The derived defaults are empty/`None`/`Retry::Restart`/zero. Set `selected_content_alias` in `open_comic`; keep it while returning from Reader to Episodes.

Add exact geometry helpers:

```rust
fn slices_for(height: u32, panel_height: u32) -> Option<usize> {
    if height == 0 || panel_height == 0 {
        return None;
    }
    let height = usize::try_from(height).ok()?;
    let panel = usize::try_from(panel_height).ok()?;
    Some(height.div_ceil(panel))
}

fn page_plan(
    images: &[EpisodeImage],
    panel_width: u32,
    panel_height: u32,
) -> Result<(Vec<usize>, u16), String> {
    let mut pages = Vec::with_capacity(images.len());
    let mut total = 0_usize;
    for image in images {
        let (_, scaled_height) = kobo_image::width_scaled_size(
            (image.width, image.height),
            panel_width,
        )
        .map_err(|error| error.to_string())?;
        let count = slices_for(scaled_height, panel_height)
            .ok_or_else(|| "The comic page dimensions are not supported.".to_owned())?;
        total = total
            .checked_add(count)
            .ok_or_else(|| "The comic has too many pages.".to_owned())?;
        pages.push(count);
    }
    let total = u16::try_from(total)
        .map_err(|_| "The comic has too many pages.".to_owned())?;
    Ok((pages, total))
}

fn previous_location(pages: &[usize], current: PageLocation) -> Option<PageLocation> {
    if current.slice > 0 {
        return Some(PageLocation {
            source: current.source,
            slice: current.slice - 1,
        });
    }
    let source = current.source.checked_sub(1)?;
    Some(PageLocation {
        source,
        slice: pages.get(source)?.checked_sub(1)?,
    })
}

fn next_location(pages: &[usize], current: PageLocation) -> Option<PageLocation> {
    let count = *pages.get(current.source)?;
    let slice = current.slice.checked_add(1)?;
    if slice < count {
        return Some(PageLocation {
            source: current.source,
            slice,
        });
    }
    let source = current.source.checked_add(1)?;
    pages.get(source).map(|_| PageLocation { source, slice: 0 })
}

fn slice_rows(source: &Picture, slice: usize, panel_height: u32) -> Result<Picture, String> {
    let width = usize::try_from(source.width())
        .map_err(|_| "The comic width is not supported.".to_owned())?;
    let panel = usize::try_from(panel_height)
        .map_err(|_| "The panel height is not supported.".to_owned())?;
    if panel == 0 {
        return Err("The panel height is not supported.".to_owned());
    }
    let start_row = slice
        .checked_mul(panel)
        .ok_or_else(|| "The comic page offset is too large.".to_owned())?;
    let source_height = usize::try_from(source.height())
        .map_err(|_| "The comic height is not supported.".to_owned())?;
    if start_row >= source_height {
        return Err("The comic page offset is outside the image.".to_owned());
    }
    let copied_rows = (source_height - start_row).min(panel);
    let output_len = width
        .checked_mul(panel)
        .ok_or_else(|| "The comic slice is too large.".to_owned())?;
    let copied_len = width
        .checked_mul(copied_rows)
        .ok_or_else(|| "The comic slice is too large.".to_owned())?;
    let source_start = width
        .checked_mul(start_row)
        .ok_or_else(|| "The comic page offset is too large.".to_owned())?;
    let source_end = source_start
        .checked_add(copied_len)
        .ok_or_else(|| "The comic page offset is too large.".to_owned())?;
    let source_rows = source
        .grey()
        .get(source_start..source_end)
        .ok_or_else(|| "The comic pixels do not match their dimensions.".to_owned())?;
    let mut grey = vec![255; output_len];
    grey[..copied_len].copy_from_slice(source_rows);
    Picture::from_grey(source.width(), panel_height, grey).map_err(|error| error.to_string())
}
```

- [ ] **Step 4: Make episode eligibility and reader screens explicit**

Replace each episode row with a button only when `episode.purchase.is_readable()`:

```rust
let label = format!(
    "{} [{}] - {}",
    display_text(&episode.title, &title_fallback),
    episode.alias,
    status
);
if episode.purchase.is_readable() {
    screen = screen.button(format!("episode-{index}"), label);
} else {
    screen = screen.text(label);
}
```

Add a normal loading/error screen for the three reader pending states. Add `reader_screen`:

```rust
fn reader_screen(&self) -> Screen {
    let selection = self
        .reader_selection
        .as_ref()
        .expect("reader view without episode selection");
    let reader = self.reader.as_ref().expect("reader view without reader state");
    let picture = reader.picture.expect("reader view without uploaded slice");
    ScreenBuilder::new("bomtoon-reader")
        .top_bar(selection.title.clone())
        .reading_surface(
            picture,
            if reader.chrome_visible {
                ReadingChrome::Overlay
            } else {
                ReadingChrome::Hidden
            },
        )
        .page_turns(READER_PREVIOUS, READER_NEXT)
        .reading_menu(READER_CHROME)
        .page_position(reader.global_page(), reader.total_pages)
        .build()
}
```

`show` gives application-owned Back to ready Episodes screens, ready Reader screens, and reader error screens. Pending loading screens do not accept navigation.

- [ ] **Step 5: Start manifests and install decoded slices**

When an owned/free episode action is tapped, create `reader_selection` from `selected_content_alias` plus the episode alias and display title, clear reader-specific errors, set `View::Reader`, and spawn `Pending::Manifest` with `api::images`. This selection exists before the manifest returns, so manifest loading, manifest errors, retry, and Back all retain the exact episode identity.

On manifest completion:

1. Parse with `parse::images`.
2. Build `page_plan` from `context.metrics().width` and `.height` converted to `u32`.
3. Create `ReaderState` at source 0/slice 0 with hidden chrome.
4. Spawn `Pending::Image(PageLocation { source: 0, slice: 0 })`.

On image completion, perform this order:

```rust
let decoded = kobo_image::decode(bytes).map_err(|error| error.to_string())?;
let expected = &reader.images[target.source];
if (decoded.width(), decoded.height()) != (expected.width, expected.height) {
    return Err("BOMTOON returned different comic image dimensions.".to_owned());
}
let mut scaled = decoded
    .scale_to_width(panel_width)
    .map_err(|error| error.to_string())?;
scaled.dither(PANEL_GREYS);
reader.source = Some(scaled);
reader.location = target;
reader.chrome_visible = false;
```

Create the slice, allocate a fresh `PictureHandle`, call `context.put_picture`, store the returned `TilePicture`, call `show` to queue `SetScreen`, then call `context.drop_picture` for the old handle. If slice creation or upload fails, show an episode-specific error with `Retry::Slice` and do not leave a screen referring to a dropped handle.

- [ ] **Step 6: Implement navigation, one refresh, retry, and cleanup**

Use distinct constants `reader-previous`, `reader-next`, and `reader-chrome`.

- Center toggles `chrome_visible` and redraws without replacing the picture.
- A valid within-source turn changes `location.slice`, sets chrome hidden, uploads a new slice, queues the screen, and drops the old handle.
- A valid cross-source turn sets chrome hidden, drops the scaled source, sets `refreshed_current_image = false`, and spawns the target uncredentialed image request.
- First/last boundary actions do nothing and preserve chrome.
- Back from Reader queues the Episodes screen before dropping the current handle, then clears `reader`, `reader_selection`, signed URLs, and scaled source data while retaining `selected_content_alias` for the episode list.
- Back from Episodes preserves the existing return-to-shelf behavior.

On `Pending::Image(target)` plus `TaskError::Unauthorized`, if `refreshed_current_image` is false, set it true and spawn `Pending::ManifestRefresh(target)`. On refresh completion, parse the entire manifest and require this predicate before atomically replacing URLs:

```rust
fn same_assets(current: &[EpisodeImage], refreshed: &[EpisodeImage]) -> bool {
    current.len() == refreshed.len()
        && current.iter().zip(refreshed).all(|(old, new)| {
            old.order == new.order
                && old.width == new.width
                && old.height == new.height
                && old.path == new.path
        })
}
```

Retry the same `PageLocation` with the refreshed URL while leaving `refreshed_current_image = true`. A second CDN `Unauthorized` records an image error and `Retry::Image(target)` without changing `AccountState`. A refresh shape/path mismatch also records an episode image error and `Retry::Image(target)` without replacing any URL. Only `Pending::Manifest` and `Pending::ManifestRefresh` map `NoCredential`/`Unauthorized` to signed-out/expired.

Every first-attempt source fetch sets `refreshed_current_image = false`; only the fetch immediately following a successful refresh preserves `true`. `Retry::Manifest` restarts the selected manifest. `Retry::Image(target)` resets the one-refresh budget and restarts that CDN image. `Retry::Slice` reuploads the retained source slice. Existing library/content failures retain `Retry::Restart`.

Change account clearing to accept `&mut Context`, queue release of any reader handle, and clear selection, manifest URLs, source, slice, episodes, library, and recent state. Keep stale task rejection as the first `on_task` check.

- [ ] **Step 7: Run the full BOMTOON test target**

Run: `rtk cargo test -p kobo-bomtoon`

Expected: all existing account/library/recent tests and all reader/API/parser/layout/state tests pass.

- [ ] **Step 8: Verify the target build**

Run: `rtk cargo check -p kobo-bomtoon --target armv7-unknown-linux-musleabihf`

Expected: successful armv7 musl check with no errors.

- [ ] **Step 9: Commit the integrated reader**

```bash
rtk git add apps/bomtoon/src/main.rs
rtk git commit -m "feat(bomtoon): read plain episodes"
```

### Task 9: Exercise the real UI and run repository gates

**Files:**
- No source changes expected.
- Modify only task-relevant files if verification exposes a defect, then rerun the failing check and its package test before continuing.

**Interfaces:**
- Consumes: the complete reader from Tasks 1-8 and the existing managed BOMTOON credential.
- Produces: browser/runtime evidence and final workspace gate evidence.

- [ ] **Step 1: Run focused package checks together**

Run:

```bash
rtk cargo test -p kobo-ui
rtk cargo test -p kobo-protocol
rtk cargo test -p kobo-sdk
rtk cargo test -p kobo-image
rtk cargo test -p kobo-net
rtk cargo test -p kobo-bomtoon
rtk cargo check -p kobo-bomtoon --target armv7-unknown-linux-musleabihf
```

Expected: every command exits successfully.

- [ ] **Step 2: Exercise the browser simulator visually**

From `apps/bomtoon`, start:

```bash
rtk cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

Use the browser automation tool against the actual simulator surface. With the existing managed credential:

1. Open a title and its episode list.
2. Confirm only Owned and Free rows are tappable.
3. Open the authenticated-free episode.
4. Confirm artwork reaches all panel edges and chrome starts hidden.
5. Center-tap; confirm opaque title/Back header and page-position footer appear without moving or scaling the image.
6. Center-tap again; confirm both bands disappear.
7. Show chrome and turn a page; confirm the page changes and chrome hides.
8. Turn within one source, cross a source boundary, and return across it.
9. Wait past the original 60-second signed-URL lifetime, then reach an unfetched source and confirm transparent manifest refresh/retry.
10. Show chrome and use Back; confirm return to the episode list.

Expected: every step matches the approved specification; no URL, token, or credential value appears in visible text or captured logs.

- [ ] **Step 3: Exercise the runtime simulator**

Run from the repository root:

```bash
rtk cargo run -p kobo-cli -- run --sim --app bomtoon
```

Repeat open, chrome toggle, page turn, source-boundary Back/Forward, and Back-to-episodes behavior.

Expected: behavior matches the browser simulator and picture handles remain valid through every screen transition.

- [ ] **Step 4: Run format, workspace tests, and Clippy**

Run:

```bash
rtk cargo fmt --all --check
rtk cargo test --workspace --all-targets --all-features
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all three repository gates exit successfully with no warnings.

- [ ] **Step 5: Commit only verification fixes, if any**

If verification required a source correction, stage only the task-relevant files and use:

```bash
rtk git commit -m "fix(bomtoon): correct reader verification issue"
```

If no correction was required, do not create an empty commit.
