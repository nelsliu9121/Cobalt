# Default WebP Decoding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decode static WebP images through the existing `kobo_image::size()` and `kobo_image::decode()` APIs for every `kobo-image` caller.

**Architecture:** Enable the pinned `image` crate's WebP codec in `kobo-image`. Format guessing, byte and pixel limits, orientation, alpha compositing, grayscale conversion, and errors stay in the current decode path. Fixed inline fixtures prove lossy decode, lossless alpha handling, and header/decode dimension parity.

**Tech Stack:** Rust 2021, Rust 1.85.1, `image` 0.25.8, `image-webp` 0.2.4, Cargo unit tests.

## Global Constraints

- Keep `image` pinned to exactly 0.25.8 because 0.25.9 exceeds the workspace MSRV policy.
- Enable WebP by default in `kobo-image`; do not add a Cobalt feature flag.
- Support one static `Picture` result. Do not add animation frame APIs, playback, timing, or an animated-file rejection rule.
- Preserve `MAX_SOURCE_BYTES = 4 * 1024 * 1024` and `MAX_PIXELS = 4 * 1072 * 1448`.
- Preserve the public `Picture`, `ImageError`, `size()`, and `decode()` interfaces.
- Keep fixtures inline in `crates/kobo-image/src/lib.rs`; do not add binary assets.
- Do not touch BOMTOON login, credentials, API, model, episode-fetching, or UI code.
- Prefix every shell command with `rtk`.

## File Structure

- Modify `crates/kobo-image/Cargo.toml`: add the `image/webp` codec to the existing dependency feature list.
- Modify `crates/kobo-image/src/lib.rs`: add inline WebP fixtures and decoder contract tests in the existing `#[cfg(test)]` module. Production Rust stays unchanged.
- Modify `Cargo.lock`: record `image-webp` 0.2.4 and `quick-error` through Cargo resolution.

---

### Task 1: Enable and prove static WebP decoding

**Files:**
- Modify: `crates/kobo-image/src/lib.rs:555-669`
- Modify: `crates/kobo-image/Cargo.toml:9-13`
- Modify: `Cargo.lock`
- Test: `crates/kobo-image/src/lib.rs` unit-test module

**Interfaces:**
- Consumes: `pub fn size(bytes: &[u8]) -> Result<(u32, u32), ImageError>` and `pub fn decode(bytes: &[u8]) -> Result<Picture, ImageError>`.
- Produces: the same public interfaces with JPEG, PNG, and WebP format recognition. No signature or type changes.

- [ ] **Step 1: Add failing WebP fixtures and contract tests**

Add these helpers after `tiny_jpeg()` and before `decode_base64()`:

```rust
fn tiny_lossy_webp() -> Vec<u8> {
    decode_base64("UklGRiIAAABXRUJQVlA4IC4AAAAwAQCdASoBAAEAAUAmJaQAA3AA/vuUAAA=")
}

fn transparent_lossless_webp() -> Vec<u8> {
    decode_base64(
        "UklGRkIAAABXRUJQVlA4TDYAAAAvAQAAEM1VICICEeGBBAAAAAAAnL8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAYAo=",
    )
}
```

Add these tests after `a_real_jpeg_decodes_to_grey_bytes()`:

```rust
#[test]
fn a_real_lossy_webp_decodes_to_grey_bytes() {
    let picture = decode(&tiny_lossy_webp()).expect("decode the WebP");
    assert_eq!((picture.width(), picture.height()), (1, 1));
    assert_eq!(picture.grey(), &[234]);
}

#[test]
fn transparent_webp_pixels_are_composited_onto_paper() {
    let picture = decode(&transparent_lossless_webp()).expect("decode the WebP");
    assert_eq!((picture.width(), picture.height()), (2, 1));
    assert_eq!(picture.grey(), &[255, 0]);
}

#[test]
fn webp_header_size_matches_decode() {
    for webp in [tiny_lossy_webp(), transparent_lossless_webp()] {
        let picture = decode(&webp).expect("decode the WebP");
        assert_eq!(
            size(&webp).expect("read the WebP header"),
            (picture.width(), picture.height())
        );
    }
}
```

The first fixture is a real `VP8 ` lossy WebP whose decoded pixel is `[234, 234, 234, 255]`. The second is a `VP8L` lossless WebP containing a transparent black pixel followed by an opaque black pixel.

- [ ] **Step 2: Run the WebP tests and verify the red state**

Run:

```sh
rtk cargo test -p kobo-image webp -- --nocapture
```

Expected: all three new tests fail at their decode call with `ImageError::Undecodable` because the guessed WebP format has no compiled decoder.

- [ ] **Step 3: Enable the pinned WebP decoder**

Replace the dependency line in `crates/kobo-image/Cargo.toml` with:

```toml
image = { version = "=0.25.8", default-features = false, features = ["jpeg", "png", "webp"] }
```

Do not change the version or enable the `image` default feature set.

- [ ] **Step 4: Run the WebP tests and verify the green state**

Run:

```sh
rtk cargo test -p kobo-image webp -- --nocapture
```

Expected: 3 passed, 0 failed. Cargo updates `Cargo.lock` with `image-webp` 0.2.4 and `quick-error`.

- [ ] **Step 5: Run the full image-crate test suite**

Run:

```sh
rtk cargo test -p kobo-image
```

Expected: every `kobo-image` unit test passes, including the existing JPEG, PNG, limits, fitting, and dithering tests.

- [ ] **Step 6: Confirm the resolved codec feature and versions**

Run:

```sh
rtk cargo tree -p kobo-image -e features
```

Expected: the tree contains `image feature "webp"`, `image v0.25.8`, and `image-webp v0.2.4`. It must not show `image feature "default"` or unrelated default formats.

- [ ] **Step 7: Compile the decoder for the Kobo device target**

Run:

```sh
rtk cargo check -p kobo-image --target armv7-unknown-linux-musleabihf
```

Expected: `kobo-image` and `image-webp` compile successfully for the static ARMv7 musl target with no new system library requirement.

- [ ] **Step 8: Check workspace formatting**

Run:

```sh
rtk cargo fmt --all --check
```

Expected: exit 0 with no formatting diff.

- [ ] **Step 9: Run the workspace test gate**

Run:

```sh
rtk cargo test --workspace --all-targets --all-features
```

Expected: every workspace test passes.

- [ ] **Step 10: Run the workspace Clippy gate**

Run:

```sh
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: exit 0 with no warnings.

- [ ] **Step 11: Commit the implementation**

```sh
rtk git add crates/kobo-image/Cargo.toml crates/kobo-image/src/lib.rs Cargo.lock
rtk git commit -m "feat(image): decode WebP"
```
