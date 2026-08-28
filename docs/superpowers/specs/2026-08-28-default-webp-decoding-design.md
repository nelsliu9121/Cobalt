# Default WebP decoding in kobo-image

## Status

Approved design. Implementation has not started.

## Goal

Cobalt must decode static WebP through the same public API as JPEG and PNG. Enable the codec in `kobo-image` for every caller. BOMTOON can then consume comic pages through the decoder; its login, API, and viewer work stays separate.

## Current state

`kobo-image` depends on `image` 0.25.8 with default features disabled. Its enabled codecs are `jpeg` and `png`. Both `size()` and `decode()` use `ImageReader::with_guessed_format()`, so format selection already happens inside the `image` crate.

The public decode path enforces two limits:

- `MAX_SOURCE_BYTES` rejects inputs larger than 4 MiB before format detection.
- `MAX_PIXELS` rejects dimensions larger than four Clara BW panels before the decoded pixel buffer is built.

After decoding, `kobo-image` applies embedded orientation, converts color to luminance with alpha, composites transparent pixels onto white paper, and returns one byte of grey per pixel in `Picture`.

The pinned `image` release exposes WebP through its `webp` Cargo feature, which selects the Rust `image-webp` decoder. Once enabled, the existing reader and decoder APIs handle the format.

## Design

Add `webp` to the `image` dependency features in `crates/kobo-image/Cargo.toml`:

```toml
image = { version = "=0.25.8", default-features = false, features = ["jpeg", "png", "webp"] }
```

No new Cobalt feature flag is added. `kobo-image` has one default format contract, and all binaries that depend on it compile with JPEG, PNG, and WebP support.

Production code stays unchanged. `size()` still detects the format, creates its decoder, reads orientation and dimensions, and applies the pixel ceiling. `decode()` passes that decoder through `DynamicImage::from_decoder()`, orientation, and paper compositing; WebP decoder failures use `ImageError::Undecodable`, while bytes with no recognized format remain `ImageError::UnknownFormat`, exactly as JPEG and PNG do today.

The Cargo lockfile records the `image-webp` dependency selected by `image` 0.25.8. The decoder does not add a platform library or a C linker requirement.

## Static image contract

Each input yields one `Picture`. Lossy WebP, lossless WebP, alpha, and embedded orientation use that existing result type. No frame API exists.

Animated WebP is outside this contract; Cobalt adds neither playback nor a rejection rule. The static `ImageDecoder` path determines the returned picture.

## Resource limits and errors

The WebP path inherits the same limits as JPEG and PNG. The limits and error variants do not change.

`size()` must continue to agree with the dimensions returned by `decode()`, including orientation. This invariant matters to book layout because pages reserve image space before the pixel decode runs.

Transparency must continue to use white paper as the background. Dropping alpha before grayscale conversion would turn transparent dark pixels into black marks.

## Tests

Add fixed, real WebP fixtures to the existing `kobo-image` unit-test module. Keep the fixtures inline, matching the existing real JPEG fixture, so the tests do not add asset files.

Tests cover these observable contracts:

1. A lossy WebP decodes to the expected width, height, and grey buffer length.
2. A lossless WebP with transparent and opaque pixels composites onto white paper correctly.
3. `size()` and `decode()` return matching dimensions for WebP.

Existing tests continue to cover source size, decoded pixel limits, orientation, alpha compositing, grayscale conversion, and fit behavior. No BOMTOON UI test is added because this change does not fetch or draw an episode page.

## Verification

Run these focused checks first:

```sh
cargo test -p kobo-image
cargo check -p kobo-image --target armv7-unknown-linux-musleabihf
```

Then run the repository gates:

```sh
cargo fmt --all --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Inspect the resolved feature tree to confirm `image/webp` is enabled through `kobo-image` and `image` remains pinned to 0.25.8.

## Files

Implementation changes are limited to:

- `crates/kobo-image/Cargo.toml`
- `crates/kobo-image/src/lib.rs`
- `Cargo.lock`

This design document and its implementation plan live under `docs/superpowers/`.

## Non-goals

- BOMTOON login or credential handling
- BOMTOON API, model, episode-fetching, or screen changes
- Animated WebP playback
- WebP encoding
- New public image APIs or error variants
- Changes to image byte or pixel ceilings
