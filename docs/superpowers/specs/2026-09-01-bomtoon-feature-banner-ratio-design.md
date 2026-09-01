# Bomtoon Feature Banner Ratio Design

## Problem

The Bomtoon Feature page renders three banner covers through `ScreenBuilder::image_strip`. The builder and UI layout force every picture into a 3:5 portrait rectangle with `PictureFit::Cover`. The source WebP banners use a 289:345 aspect ratio, so the current rectangle crops visible content.

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

## Code Changes

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

## Verification

Inline `kobo-sdk` tests will prove that `image_strip` preserves contain fit instead of forcing cover fit.

Inline `kobo-ui` tests will prove:

- three slots keep equal widths and fill the available content width with existing gaps;
- slot height follows the responsive 289:345 ratio;
- exact-ratio pictures fill their slots without cover cropping;
- differing source ratios are contained, centered horizontally, and bottom aligned;
- hit targets retain the full slot geometry;
- empty and oversized strip behavior remains unchanged.

Focused SDK and UI tests will run first. The Bomtoon Feature page will then be exercised in the simulator to confirm that all three loaded banners are visible, uncropped, bottom aligned, and spread across the viewport width.

## Scope

Expected implementation files:

- `crates/kobo-sdk/src/lib.rs`
- `crates/kobo-ui/src/lib.rs`

No Bomtoon model, parser, protocol, or network behavior changes. No new UI node or public configuration surface.
