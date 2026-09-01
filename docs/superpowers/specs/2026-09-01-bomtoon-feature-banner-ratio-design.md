# Bomtoon Image Layout Stability Design

## Problem

The Bomtoon Feature page renders three banner covers through `ScreenBuilder::image_strip`. The builder and UI layout force every picture into a 3:5 portrait rectangle with `PictureFit::Cover`. The source WebP banners use a 289:345 aspect ratio, so the current rectangle crops visible content.

Recent and Library title rows have a second layout shift. While a cover URL is loading, `cover_lead` supplies a small `RowLead::Icon`. Once the cover is ready, it supplies `RowLead::Picture`, which widens the shared lead column to the default touch-target size and moves every row title horizontally.

## Decision

Use 289:345 as a responsive aspect ratio for every image-strip slot. The ratio does not prescribe fixed pixel dimensions.

For content width `W` and the existing small gutter `G`:

- banner width: `(W - 2 * G) / 3`
- banner height: `banner width * 345 / 289`

Integer arithmetic will use the existing checked or saturating layout conventions. Three equal tappable slots continue to span almost all available content width with two small gaps.

## Picture Placement

Each source picture is contained within its slot rather than cropped. Its rendered rectangle is centered horizontally and bottom aligned within the 289:345 slot.

A source with the exact 289:345 ratio fills the slot. A source with a slightly different ratio remains fully visible. Any unused vertical space stays above the picture so the artwork remains anchored to the bottom. The full slot, including transparent or unused space, remains the tap target.

Missing-picture glyphs remain centered in their slots. The three-item limit and action routing do not change.

## Recent and Library Row Placeholders

Recent and Library rows with a cover URL use the existing square `RowLead::CoverSlot` until the picture is ready. `CoverSlot` and `Picture` deliberately share the same full lead column, vertical placement, and square rectangle, so image completion cannot move the title or other row content.

Rows without a cover URL keep the compact `RowLead::Icon` because no image will arrive. Episode rows keep their current behavior and remain outside this change.

## Code Changes

### Bomtoon shelf rows

In `apps/bomtoon/src/main.rs`, a shelf-specific cover lead helper will return:

- `RowLead::Icon` when no cover URL exists;
- `RowLead::CoverSlot` while a declared cover is loading or has failed;
- `RowLead::Picture` when the cover is ready.

Only the Recent and Library title row builders will use this helper. Cover requests, caching, failure state, row actions, text, trailing values, pagination, and episode rows do not change.

### SDK builder

In `crates/kobo-sdk/src/lib.rs`, `ScreenBuilder::image_strip` will stop replacing each supplied picture fit with `PictureFit::Cover`. The default `TilePicture::new` contain fit will reach layout unchanged.

### UI layout

In `crates/kobo-ui/src/lib.rs`, `Node::ImageStrip` layout will:

1. Derive three equal responsive column widths from available content width and the existing gutters.
2. Derive slot height from the 289:345 ratio.
3. Fit each source within its slot without cropping.
4. Center the fitted picture horizontally.
5. Align the fitted picture to the slot bottom.
6. Preserve the full slot as the tile hit target.

No protocol change is needed because `TilePicture` already carries source dimensions and fit mode.

## Error and Boundary Behavior

Zero-sized or unavailable pictures continue to use existing placeholder behavior. Empty strips remain zero height. Oversized strips remain limited to three items and retain their existing layout warning. Arithmetic follows existing bounded layout behavior so narrow or invalid rectangles do not overflow.

Recent and Library cover loading and failure states retain the same square lead rectangle as the ready state. Missing artwork with no URL remains a compact icon. A failed fetch can therefore keep its placeholder without shifting text.

## Verification

Inline `kobo-sdk` tests will prove that `image_strip` preserves contain fit instead of forcing cover fit.

Inline `kobo-ui` tests will prove:

- three slots keep equal widths and fill the available content width with existing gaps;
- slot height follows the responsive 289:345 ratio;
- exact-ratio pictures fill their slots without cover cropping;
- differing source ratios are contained, centered horizontally, and bottom aligned;
- hit targets retain the full slot geometry;
- empty and oversized strip behavior remains unchanged.

Inline Bomtoon tests will exercise both Recent and Library destinations and prove:

- a declared but unavailable cover uses `RowLead::CoverSlot`;
- a ready cover uses `RowLead::Picture`;
- the lead rectangle is identical before and after image completion;
- title geometry is identical before and after image completion;
- rows without a cover URL retain `RowLead::Icon`;
- row content, page controls, and layout diagnostics remain valid.

Focused Bomtoon, SDK, and UI tests will run first. The Bomtoon Feature page will then be exercised in the simulator to confirm that all three loaded banners are visible, uncropped, bottom aligned, and spread across the viewport width. Recent and Library will be exercised while covers load to confirm that title and lead geometry remain fixed when each picture appears.

## Scope

Expected implementation files:

- `apps/bomtoon/src/main.rs`
- `crates/kobo-sdk/src/lib.rs`
- `crates/kobo-ui/src/lib.rs`

No Bomtoon model, parser, protocol, or network behavior changes. No new UI node or public configuration surface.
