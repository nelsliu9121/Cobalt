# Color Picture Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve full RGB8 picture content from decode through the Kobo framebuffer on attended, verified color profiles while every unverified profile continues to use the existing Gray8 path.

**Architecture:** A dependency-free `kobo-pixels` crate owns the shared format and owned/borrowed pixel enums, preventing `kobo-image` from depending on the renderer and preventing the protocol from depending on the decoder. `kobo-ui` owns typed retained surfaces: grayscale chrome writes equal RGB channels when a referenced RGB picture selects an RGB8 surface, and the frame planner classifies transitions from previous and current typed pixels. Device profiles opt into color only after attended evidence supplies framebuffer fields, HWTCON color waveforms, and CFA flags.

**Tech Stack:** Rust 2021, `image`, `kobo-pixels`, `kobo-ui`, `kobo-protocol`, `kobo-sdk`, `kobo-profile`, `kobo-hal`, `kobod`, `kobo-sim`, in-module unit tests, browser/runtime simulators, attended Kobo color-device probe.

## Global Constraints

- `PictureFormat` has exactly `Gray8` and `Rgb8`; `PicturePixels` has exactly `Gray8(Vec<u8>)` and `Rgb8(Vec<u8>)`.
- Gray8 requires exactly `width * height` bytes; RGB8 requires exactly `width * height * 3` bytes, row-major with red, green, blue bytes and no padding.
- UI and reading chrome remain grayscale; on RGB8 surfaces each gray tone is written as equal red, green, and blue channels.
- Generic decode preserves the existing perceptual-luma plus white-paper alpha composite for Gray8. RGB8 decode composites alpha onto white independently in red, green, and blue.
- `MAX_SOURCE_BYTES = 4 * 1024 * 1024`, `MAX_PIXELS = 7_000_000`, `AXIS_SAMPLE_CHUNK = 2_048`, and scaler axis scratch stays at or below 24,576 logical bytes.
- Do not add an external image, color-management, cache, or rendering dependency. Do not add app-side Kaleido palette tuning or color quantization.
- Gray8 cleaning remains eight panel-area equivalents. Chromatic cleaning uses four panel-area equivalents: first chromatic surface is full GCC16, later chromatic updates use GLRC16, and the next update after the accumulated threshold is full GCC16.
- A changed pixel is chromatic when either its previous or current RGB channels are unequal. Transitioning from a chromatic surface to a wholly achromatic surface forces an immediate full GCC16 color clean.
- Candidate HWTCON values are GCC16 `10`, GLRC16 `11`, and CFA G2 flags `0x600`; they remain disabled until attended validation records display, touch, exit, recovery, framebuffer channel order, waveform, and CFA evidence for that exact profile and firmware.
- Missing or invalid color capability fails closed to Gray8. A color refresh intent without a validated color capability is rejected before a device write.
- Preserve all existing grayscale app behavior, picture-cache byte limits, chunked-upload atomicity, runtime screen restoration, and protocol size bounds.

---

### Task 1: Introduce Typed Pixels and Typed UI Surfaces

**Files:**
- Modify: `Cargo.toml` workspace members
- Create: `crates/kobo-pixels/Cargo.toml`
- Create: `crates/kobo-pixels/src/lib.rs`
- Modify: `crates/kobo-ui/Cargo.toml`
- Modify: `crates/kobo-ui/src/lib.rs:304-463,8750-9260,10112-10339,13000-13440,15960-16040`
- Test: `crates/kobo-pixels/src/lib.rs`
- Test: `crates/kobo-ui/src/lib.rs`

**Interfaces:**
- Consumes: existing `DisplayMetrics`, `Surface`, `PictureCache`, `Pictures`, and grayscale drawing primitives.
- Produces the dependency-free pixel types from `kobo-pixels` and typed surfaces from `kobo-ui`:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PictureFormat {
    #[default]
    Gray8,
    Rgb8,
}

impl PictureFormat {
    pub const fn bytes_per_pixel(self) -> usize;
    pub fn byte_len(self, width: u32, height: u32) -> Option<usize>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PicturePixels {
    Gray8(Vec<u8>),
    Rgb8(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PicturePixelsRef<'a> {
    Gray8(&'a [u8]),
    Rgb8(&'a [u8]),
}

impl PicturePixels {
    pub const fn format(&self) -> PictureFormat;
    pub fn as_ref(&self) -> PicturePixelsRef<'_>;
    pub fn byte_count(&self) -> usize;
    pub fn into_bytes(self) -> Vec<u8>;
}

pub struct Surface {
    pub width: usize,
    pub height: usize,
    pub format: PictureFormat,
    pixels: Vec<u8>,
}

impl Surface {
    pub fn new(width: usize, height: usize) -> Self;
    pub fn new_in(width: usize, height: usize, format: PictureFormat) -> Self;
    pub fn pixels(&self) -> PicturePixelsRef<'_>;
    pub fn bytes(&self) -> &[u8];
}
```

`DisplayMetrics` gains `pub picture_format: PictureFormat`, with `CLARA_BW_METRICS` and `Default` set to `Gray8`.

- [ ] **Step 1: Write typed-length and surface tests**

Add exact tests before implementation:

```rust
#[test]
fn picture_formats_compute_checked_byte_lengths() {
    assert_eq!(PictureFormat::Gray8.byte_len(3, 2), Some(6));
    assert_eq!(PictureFormat::Rgb8.byte_len(3, 2), Some(18));
    assert_eq!(PictureFormat::Rgb8.byte_len(u32::MAX, u32::MAX), None);
}

#[test]
fn rgb_surface_draws_gray_chrome_with_equal_channels() {
    let mut surface = Surface::new_in(2, 1, PictureFormat::Rgb8);
    surface.clear(64);
    assert_eq!(surface.bytes(), &[64, 64, 64, 64, 64, 64]);
}

#[test]
fn picture_cache_rejects_wrong_typed_lengths() {
    let mut cache = PictureCache::default();
    assert!(!cache.put(PictureHandle(1), 2, 2, PicturePixels::Rgb8(vec![0; 11])));
    assert!(cache.put(PictureHandle(1), 2, 2, PicturePixels::Rgb8(vec![0; 12])));
}
```

Retain the existing Gray8 cache and renderer assertions by wrapping their byte vectors in `PicturePixels::Gray8`.

- [ ] **Step 2: Run the focused tests and verify they fail**

Run: `cargo test -p kobo-ui picture_formats_compute_checked_byte_lengths`

Run: `cargo test -p kobo-ui rgb_surface_draws_gray_chrome_with_equal_channels`

Expected: compilation fails because the typed pixel and surface APIs do not exist.

- [ ] **Step 3: Create the shared pixel crate**

Register `crates/kobo-pixels` in the workspace. The crate has no dependencies and begins with `#![forbid(unsafe_code)]`. Implement checked multiplication only:

```rust
pub fn byte_len(self, width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(self.bytes_per_pixel())
}
```

Add `PicturePixels::format`, `as_ref`, `byte_count`, and `into_bytes` as exhaustive matches. Do not expose a raw constructor that can claim one format while carrying another. Add `kobo-pixels` to `kobo-ui` and publicly re-export its three pixel types from `kobo-ui` so current UI and SDK imports remain shallow.

- [ ] **Step 4: Make every drawing primitive format-aware**

Keep `Surface::new` as the Gray8 constructor and add `new_in`. Centralize one checked pixel write:

```rust
fn set_gray(&mut self, index: usize, gray: u8) {
    match self.format {
        PictureFormat::Gray8 => self.pixels[index] = gray,
        PictureFormat::Rgb8 => {
            let start = index * 3;
            self.pixels[start..start + 3].fill(gray);
        }
    }
}
```

Route `clear`, fill, blend, inversion, text, rules, and all other grayscale chrome writes through typed helpers. For RGB blend, blend each destination channel toward the same gray ink value. Keep pixel coordinates and clipping in units of pixels, never bytes.

- [ ] **Step 5: Type the picture cache and blitter**

Replace `HeldPicture.grey` and `PendingPicture.grey` with `PicturePixels` and an upload pair of `format: PictureFormat` plus `bytes: Vec<u8>`. Change:

```rust
pub fn put(
    &mut self,
    handle: PictureHandle,
    width: u32,
    height: u32,
    pixels: PicturePixels,
) -> bool;

pub fn begin_upload(
    &mut self,
    handle: PictureHandle,
    width: u32,
    height: u32,
    format: PictureFormat,
) -> bool;
```

Budget and eviction charge `pixels.byte_count()`. `Pictures::get` returns `PicturePixelsRef`. Gray8 copies on a Gray8 surface and expands each tone to three equal channels on an RGB8 surface. RGB8 copies three bytes per pixel on an RGB8 surface and is refused on a Gray8 surface; no RGB-to-gray fallback is permitted.

- [ ] **Step 6: Run all UI tests**

Run: `cargo test -p kobo-ui`

Expected: all existing Gray8 rendering, cache, and planner tests pass; new typed-length, RGB chrome, RGB blit, mismatch, cache-budget, and chunked-upload tests pass.

- [ ] **Step 7: Commit the typed pixel and UI model**

```bash
git add Cargo.toml Cargo.lock crates/kobo-pixels crates/kobo-ui/Cargo.toml crates/kobo-ui/src/lib.rs
git commit -m "feat(ui): add typed picture surfaces"
```

---

### Task 2: Carry Pixel Format Through Protocol and SDK

**Files:**
- Modify: `crates/kobo-protocol/Cargo.toml`
- Modify: `crates/kobo-protocol/src/lib.rs:8-68,640-690,1570-1620,2140-2190,3930-3980,8440-8520`
- Modify: `crates/kobo-sdk/src/lib.rs:8-31,2720-2740,3190-3240,4900-5000,5160-5190,6020-6080`
- Modify Gray8 callers: `apps/bomtoon/src/main.rs`, `apps/morse/src/main.rs`, `examples/audiobook/src/main.rs`, `examples/gallery/src/main.rs`, `examples/gutenbird/src/main.rs`, `examples/store/src/main.rs`, `crates/kobo-bookview/src/lib.rs`, `crates/kobo-read/examples/pageshot.rs`
- Test: `crates/kobo-protocol/src/lib.rs`
- Test: `crates/kobo-sdk/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 `PictureFormat` and `PicturePixels`.
- Produces:

```rust
pub const VERSION: u8 = 12;
pub const MAX_PICTURE_BYTES: usize = 3 * 1264 * 1680;

Message::PutPicture {
    handle: PictureHandle,
    width: u32,
    height: u32,
    pixels: PicturePixels,
}

Message::BeginPicture {
    handle: PictureHandle,
    width: u32,
    height: u32,
    format: PictureFormat,
}

Message::PictureChunk {
    handle: PictureHandle,
    offset: u32,
    bytes: Vec<u8>,
}

pub fn Context::put_picture(
    &mut self,
    handle: PictureHandle,
    width: u32,
    height: u32,
    pixels: PicturePixels,
) -> Option<TilePicture>;
```

- [ ] **Step 1: Write protocol round-trip and refusal tests**

Add Gray8 and RGB8 inline round trips, an RGB8 chunked round trip, and malformed-length refusals:

```rust
#[test]
fn rgb_picture_round_trips_with_its_format() {
    let frame = Frame {
        request_id: 1,
        message: Message::PutPicture {
            handle: PictureHandle(4),
            width: 2,
            height: 1,
            pixels: PicturePixels::Rgb8(vec![1, 2, 3, 4, 5, 6]),
        },
    };
    assert_eq!(decode(&encode(&frame).expect("encode")).expect("decode"), frame);
}
```

Assert a declared 2×1 RGB8 body with five or seven bytes is rejected, unknown format tag is rejected, and `MAX_PICTURE_BYTES + 1` is rejected before allocation.

- [ ] **Step 2: Run protocol tests and verify they fail**

Run: `cargo test -p kobo-protocol rgb_picture_round_trips_with_its_format`

Expected: compilation fails because messages still carry `grey`.

- [ ] **Step 3: Bump and encode protocol version 12**

Add `kobo-pixels` as a direct path dependency and publicly re-export `PictureFormat` and `PicturePixels` from `kobo-protocol`. Document that version 12 adds a format byte to inline and begin-picture payloads. Encode `Gray8 = 0`, `Rgb8 = 1`; reject every other tag. Validate length with `format.byte_len(width, height)` before taking or allocating bytes. Rename chunk data to `bytes` without changing chunk limits or atomic commit semantics.

- [ ] **Step 4: Type SDK commands and chunk selection**

Publicly re-export `PictureFormat`, `PicturePixels`, and `PicturePixelsRef` from `kobo-sdk`. Change `Command::PutPicture` and `Context::put_picture` to own `PicturePixels`. Validate exact typed length and `MAX_PICTURE_BYTES`. For chunked uploads, take `format` before `into_bytes()` and send it only in `BeginPicture`; each `PictureChunk` remains raw bytes with exact offsets.

- [ ] **Step 5: Migrate every existing Gray8 caller**

Wrap existing buffers explicitly:

```rust
context.put_picture(
    handle,
    width,
    height,
    PicturePixels::Gray8(grey),
)
```

Update all files listed above and SDK test apps. Do not add `put_gray_picture`, a deprecated overload, or an implicit `From<Vec<u8>>`.

- [ ] **Step 6: Run protocol, SDK, and migrated-caller tests**

Run: `cargo test -p kobo-protocol`

Run: `cargo test -p kobo-sdk`

Run: `cargo test -p kobo-bookview`

Run: `cargo test -p kobo-morse`

Expected: protocol v12 round trips both formats, malformed lengths fail closed, chunk commits remain atomic, and every current caller explicitly uploads Gray8.

- [ ] **Step 7: Commit the typed wire contract**

```bash
git add crates/kobo-protocol/Cargo.toml crates/kobo-protocol/src/lib.rs crates/kobo-sdk/src/lib.rs apps/bomtoon/src/main.rs apps/morse/src/main.rs examples/audiobook/src/main.rs examples/gallery/src/main.rs examples/gutenbird/src/main.rs examples/store/src/main.rs crates/kobo-bookview/src/lib.rs crates/kobo-read/examples/pageshot.rs
git commit -m "feat(sdk): type picture pixel uploads"
```

---

### Task 3: Preserve RGB Through Decode and Scaling

**Files:**
- Modify: `crates/kobo-image/Cargo.toml`
- Modify: `crates/kobo-image/src/lib.rs:1-208,488-969`
- Modify Gray8 consumers: `apps/bomtoon/src/main.rs`, `examples/gutenbird/src/main.rs`, `crates/kobo-bookview/src/lib.rs`, `crates/kobo-read/examples/pageshot.rs`
- Test: `crates/kobo-image/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 `PictureFormat`, `PicturePixels`, `PicturePixelsRef`; existing decode caps and image orientation.
- Produces:

```rust
pub struct Picture {
    width: u32,
    height: u32,
    pixels: PicturePixels,
}

impl Picture {
    pub fn from_pixels(width: u32, height: u32, pixels: PicturePixels) -> Result<Self, ImageError>;
    pub fn from_grey(width: u32, height: u32, grey: Vec<u8>) -> Result<Self, ImageError>;
    pub fn format(&self) -> PictureFormat;
    pub fn pixels(&self) -> PicturePixelsRef<'_>;
    pub fn into_pixels(self) -> PicturePixels;
    pub fn dither(&mut self, levels: u8) -> Result<(), ImageError>;
    pub fn scale_to_width(self, target_width: u32) -> Result<Self, ImageError>;
}

pub fn decode(bytes: &[u8]) -> Result<Picture, ImageError>;
pub fn decode_webp(bytes: &[u8], format: PictureFormat) -> Result<Picture, ImageError>;
pub fn encode_png(width: u32, height: u32, pixels: PicturePixelsRef<'_>) -> Result<Vec<u8>, ImageError>;
```

- [ ] **Step 1: Write decode, alpha, and scaler tests for both formats**

Add a 2×1 RGBA fixture generated in memory and assert:

```rust
#[test]
fn rgb_decode_preserves_color_and_composites_alpha_on_white() {
    let webp = rgba_webp(2, 1, &[[255, 0, 0, 255], [0, 0, 255, 128]]);
    let picture = decode_webp(&webp, PictureFormat::Rgb8).expect("RGB WebP");
    assert_eq!(picture.pixels(), PicturePixelsRef::Rgb8(&[255, 0, 0, 127, 127, 255]));
}
```

Keep a Gray8 regression that calls generic `decode` and asserts the current perceptual luma and white-alpha formula. Add exact bilinear RGB output, constant-color enlarge/reduce, same-width allocation reuse, WebP-only PNG refusal, 1080-to-1072/1264/1404 widths, zero width, over-`MAX_PIXELS`, and the 6,999,999×1 to 7,000,000×1 scratch-boundary test.

- [ ] **Step 2: Run the new tests and verify they fail**

Run: `cargo test -p kobo-image rgb_decode_preserves_color_and_composites_alpha_on_white`

Run: `cargo test -p kobo-image maximum_width_thin_image_keeps_axis_scratch_bounded`

Expected: compilation fails because decode and `Picture` are grayscale-only.

- [ ] **Step 3: Preserve generic Gray8 decode and add typed WebP decode**

Add `kobo-pixels` as a workspace path dependency. Keep `decode(bytes)` source-compatible and always Gray8 for existing callers. Make only `decode_webp(bytes, format)` select Gray8 or RGB8. Keep guessed-format and WebP-only validation ahead of allocation. Apply EXIF orientation first. Gray8 uses the existing luma-alpha conversion and formula. RGB8 uses oriented RGBA and this per-channel composite:

```rust
fn over_white(channel: u8, alpha: u8) -> u8 {
    let a = u16::from(alpha);
    u8::try_from((u16::from(channel) * a + 255 * (255 - a) + 127) / 255)
        .expect("white composite is one byte")
}
```

Do not color-manage, quantize, or infer panel capability in `kobo-image`.

- [ ] **Step 4: Generalize the bounded consuming scaler**

Use one `AxisSample { low: u32, high: u32, upper_weight: u32 }` chunk of at most `AXIS_SAMPLE_CHUNK = 2_048` entries. Keep the axis-global denominator and exact endpoint mapping. For Gray8 interpolate one channel; for RGB8 interpolate three channels directly into the final vector. No full-size intermediate, `Rgba32FImage`, or width-proportional axis allocation.

`dither` returns an error for RGB8 and performs the current page-wide Floyd–Steinberg pass only for Gray8.

- [ ] **Step 5: Migrate grayscale-only image consumers to exhaustive matches**

Remove `Picture::grey` and `Picture::into_grey`; their names can silently misread a future RGB picture. Uploading consumers pass `picture.into_pixels()` directly. Grayscale row-processing consumers use an exhaustive match:

```rust
let PicturePixelsRef::Gray8(grey) = picture.pixels() else {
    return Err(\"this operation requires a grayscale picture\".to_owned());
};
```

Keep `Picture::from_grey` as a checked convenience that delegates to `from_pixels(..., PicturePixels::Gray8(grey))`.

- [ ] **Step 6: Generalize PNG evidence output**

`encode_png` maps Gray8 to `ExtendedColorType::L8` and RGB8 to `ExtendedColorType::Rgb8`, with exact typed-length validation. Keep `encode_png_grey` only if non-picture framebuffer tooling still requires it; if retained, implement it as one direct call to `encode_png`, not a second encoder.

- [ ] **Step 7: Run the complete image and consumer suites**

Run: `cargo test -p kobo-image`

Run: `cargo test -p kobo-bomtoon -p kobo-bookview -p kobo-read`

Expected: orientation, caps, format refusal, alpha compositing, RGB preservation, same-width ownership reuse, exact fixed-point scaling, scratch bounds, Gray8 dithering, typed PNG encoding, and migrated grayscale consumers all pass.

- [ ] **Step 8: Commit typed image processing**

```bash
git add crates/kobo-image/Cargo.toml crates/kobo-image/src/lib.rs apps/bomtoon/src/main.rs examples/gutenbird/src/main.rs crates/kobo-bookview/src/lib.rs crates/kobo-read/examples/pageshot.rs
git commit -m "feat(image): preserve RGB picture pixels"
```

---

### Task 4: Declare Fail-Closed Color Capabilities and Display Metrics

**Files:**
- Modify: `crates/kobo-profile/Cargo.toml`
- Modify: `crates/kobo-profile/src/lib.rs`
- Modify: `crates/kobod/src/main.rs:14-50,850-910`
- Modify: `crates/kobod/src/device.rs:140-175`
- Modify: `crates/kobo-protocol/src/lib.rs` lifecycle codec and tests
- Modify: `crates/kobo-sdk/src/lib.rs:4860-4930` lifecycle client and tests
- Test: `crates/kobo-profile/src/lib.rs`
- Test: `crates/kobod/src/main.rs`
- Test: `crates/kobo-protocol/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 `PictureFormat` and `DisplayMetrics.picture_format`.
- Produces:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelField {
    pub offset: u8,
    pub length: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColorPanel {
    pub red: ChannelField,
    pub green: ChannelField,
    pub blue: ChannelField,
    pub transparency: ChannelField,
    pub clean_waveform: u32,
    pub regal_waveform: u32,
    pub cfa_flags: u32,
    pub clean_interval: u32,
}

pub struct DeviceProfile {
    // existing fields
    pub color: Option<ColorPanel>,
}

impl DeviceProfile {
    pub const fn picture_format(&self) -> PictureFormat;
}
```

All existing profile constants initially set `color: None`.

- [ ] **Step 1: Write fail-closed profile and lifecycle tests**

Assert every supported profile advertises Gray8 while color capability is absent, `CLARA_COLOUR_393` and `LIBRA_COLOUR_390` remain Gray8, and a synthetic HWTCON profile with eight-bit, non-overlapping RGBA fields plus `clean_waveform: 10`, `regal_waveform: 11`, `cfa_flags: 0x600`, and `clean_interval: 4` advertises RGB8. Assert `validate` rejects color on `MxcfbV2`, zero/equal waveforms, zero CFA flags, a cleaning interval other than four, non-eight-bit fields, overlapping fields, or fields outside 32 bits.

Add lifecycle round trips proving the runtime-supplied format reaches `Client::metrics()`.

- [ ] **Step 2: Run tests and verify missing capabilities fail**

Run: `cargo test -p kobo-profile color`

Run: `cargo test -p kobo-protocol lifecycle`

Expected: compilation fails because profiles and lifecycle metrics have no picture format.

- [ ] **Step 3: Add explicit optional capability to every profile**

Add `kobo-pixels` as a direct path dependency, then add `ChannelField`, `ColorPanel`, and `DeviceProfile.color`. Set every concrete profile literal to `None`; do not infer from model name, product number, framebuffer ID, or 32-bpp geometry. Extend profile validation exactly as the tests require.

- [ ] **Step 4: Advertise format through existing started lifecycle**

Set `metrics.picture_format = profile.picture_format()` in `metrics_for_profile`. Extend the existing lifecycle `Started` metrics payload with one validated format byte under protocol v12. Host simulation defaults to Gray8; tests may inject RGB8 metrics explicitly.

- [ ] **Step 5: Run profile, protocol, SDK, and runtime tests**

Run: `cargo test -p kobo-profile`

Run: `cargo test -p kobo-protocol`

Run: `cargo test -p kobo-sdk lifecycle`

Run: `cargo test -p kobod metrics`

Expected: every real profile remains Gray8, synthetic verified color metrics round trip as RGB8, and malformed capabilities fail validation.

- [ ] **Step 6: Commit fail-closed capability plumbing**

```bash
git add crates/kobo-profile/Cargo.toml crates/kobo-profile/src/lib.rs crates/kobod/src/main.rs crates/kobod/src/device.rs crates/kobo-protocol/src/lib.rs crates/kobo-sdk/src/lib.rs
git commit -m "feat(profile): declare color picture capability"
```

---

### Task 5: Pack RGB Pixels Into Validated Framebuffer Channels

**Files:**
- Modify: `crates/kobo-hal/src/surface.rs:1-330,313-501`
- Test: `crates/kobo-hal/src/surface.rs`

**Interfaces:**
- Consumes: Task 1 `PicturePixelsRef`, Task 4 `ColorPanel`, and existing validated `SurfaceGeometry`.
- Produces:

```rust
pub fn RegionSnapshot::from_pixels(
    geometry: SurfaceGeometry,
    region: Rect,
    pixels: PicturePixelsRef<'_>,
    color: Option<ColorPanel>,
) -> Result<Self, SurfaceError>;
```

`from_grayscale` is removed after its callers migrate.

- [ ] **Step 1: Write exact channel-packing tests**

Use synthetic measured 32-bpp channel layouts represented as `ColorPanel` fixtures. Assert two RGB pixels become exact framebuffer bytes, alpha is opaque, row stride padding is untouched inside the snapshot representation, Gray8 expands to equal RGB, and wrong typed lengths return `RegionMismatch`.

```rust
assert_eq!(snapshot.pixels(), &[0x33, 0x22, 0x11, 0xff]);
```

Use the expected byte order from the fixture's red/green/blue offsets; do not hard-code this illustrative assertion if that fixture describes another order.

- [ ] **Step 2: Run the packing tests and verify they fail**

Run: `cargo test -p kobo-hal from_rgb`

Expected: compilation fails because only `from_grayscale` exists.

- [ ] **Step 3: Implement bitfield-driven packing**

Validate 32 bpp and eight-bit, non-overlapping red, green, blue, and transparency fields before packing. Use shifts derived from the validated fields. Gray8 supplies the same channel three times; RGB8 supplies consecutive triples. Set transparency to its opaque value. Reject unsupported bitfields before constructing writable bytes.

Do not assume `ALPHA_BYTE_INDEX`, RGBA, BGRA, or host endianness for RGB writes.

- [ ] **Step 4: Migrate grayscale HAL tests to the typed entry point**

Replace direct `from_grayscale` calls with:

```rust
RegionSnapshot::from_pixels(geometry, region, PicturePixelsRef::Gray8(&gray), None)
```

Retain read/restore byte-for-byte tests unchanged.

- [ ] **Step 5: Run HAL tests**

Run: `cargo test -p kobo-hal`

Expected: exact Gray8 and RGB8 packing, placement bounds, short writes, capture, restore, and unsupported-format refusals pass.

- [ ] **Step 6: Commit framebuffer RGB packing**

```bash
git add crates/kobo-hal/src/surface.rs
git commit -m "feat(hal): pack RGB framebuffer regions"
```

---

### Task 6: Plan Chromatic Refreshes and Adaptive Color Cleaning

**Files:**
- Modify: `crates/kobo-ui/src/lib.rs:9035-9259,13320-13450`
- Modify: `crates/kobo-hal/src/refresh.rs:110-223,243-345`
- Test: `crates/kobo-ui/src/lib.rs`
- Test: `crates/kobo-hal/src/refresh.rs`

**Interfaces:**
- Consumes: typed `Surface`, Task 4 `ColorPanel`, and existing `RefreshPlan`.
- Produces:

```rust
pub const PANEL_COLOR_CLEAN_INTERVAL: u32 = 4;

pub enum PanelWaveform {
    Du,
    Gl16,
    Gc16,
    Glrc16,
    Gcc16,
}

pub enum RefreshIntent {
    FastFeedback,
    TextContent,
    QualityContent,
    ColorContent,
    ColorQuality,
}
```

`FramePlanner::new(width, height, format)` stores a same-format previous frame, `gray_dirty`, `color_dirty`, `was_chromatic`, and the existing refresh count.

- [ ] **Step 1: Write transition-state tests**

Add tests for all exact policies:

1. First Gray8 frame is full GC16.
2. Gray8 changed-pixel accounting still cleans after eight panel-area equivalents.
3. First RGB8 surface containing unequal channels is full GCC16.
4. Later chromatic change is partial GLRC16.
5. After four panel-area equivalents of actual changed chromatic pixels, the next update is full GCC16.
6. A change from `[255, 0, 0]` to `[127, 127, 127]` is classified chromatic because the previous pixel was chromatic.
7. Leaving any chromatic surface for a wholly achromatic RGB8 surface is immediate full GCC16.
8. Pure equal-channel RGB8 chrome uses DU/GL16/GC16 grayscale policy until chromatic content appears.
9. A surface whose dimensions differ from the planner returns `None`; Gray8↔RGB8 format changes compare logical pixels, replace the previous typed buffer only on commit, and preserve the color-exit rule.

- [ ] **Step 2: Run planner tests and verify they fail**

Run: `cargo test -p kobo-ui color_frame_planner`

Expected: compilation fails because the planner is Gray8-only and lacks color waveforms.

- [ ] **Step 3: Compare pixels without allocating**

Iterate in pixel units. Gray8 supplies one tone and RGB8 supplies a triple; when formats differ, compare Gray8 as three equal logical channels without allocating. Count a pixel changed when any logical channel differs. Count it chromatic when either old or new logical triple has unequal channels. Track the changed bounding box and actual changed-pixel count in one pass.

Force whole-panel GCC16 when `!was_chromatic && current_chromatic`, when `was_chromatic && !current_chromatic`, or when `color_dirty >= panel_area * 4` before adding the current update. Otherwise use GLRC16 for a transition containing any chromatic changed pixel.

- [ ] **Step 4: Lower color intents only with capability evidence**

Change HAL waveform lowering to accept `Option<ColorPanel>`. Existing intents keep existing backend constants and zero flags. `ColorContent` requires `Some(color)` and emits `color.regal_waveform`, partial mode, and `color.cfa_flags`; `ColorQuality` emits `color.clean_waveform`, full mode, and the same flags. Return a typed error when capability is absent or the backend is not HWTCON.

- [ ] **Step 5: Run UI and refresh tests**

Run: `cargo test -p kobo-ui frame_planner`

Run: `cargo test -p kobo-hal refresh`

Expected: all grayscale cadence tests remain byte-for-byte equivalent and every chromatic transition/capability refusal test passes.

- [ ] **Step 6: Commit color refresh planning**

```bash
git add crates/kobo-ui/src/lib.rs crates/kobo-hal/src/refresh.rs
git commit -m "feat(display): plan adaptive color refreshes"
```

---

### Task 7: Integrate Typed Surfaces Into Runtime and Simulator

**Files:**
- Modify: `crates/kobod/src/device.rs:48-52,843-873,940-949,1650-1710,3114-3185`
- Modify: `crates/kobo-hal/src/display.rs` refresh lowering call sites
- Modify: `crates/kobo-sim/src/lib.rs`
- Modify: `crates/kobo-cli/src/drive.rs:300-330`
- Modify: `crates/kobo-cli/src/main.rs:4400-4430,4520-4610`
- Modify: `crates/kobo-sdk/examples/preview.rs:48-65`
- Test: `crates/kobod/src/device.rs`
- Test: `crates/kobo-sim/src/lib.rs`

**Interfaces:**
- Consumes: Tasks 1-6 typed surfaces, cache, snapshots, profile capability, and color waveforms.
- Produces: runtime and simulator frames selected per screen: Gray8 when no cached RGB picture is referenced, RGB8 when a verified RGB picture is referenced; PNG evidence preserves either format.

- [ ] **Step 1: Write runtime and simulator integration tests**

Add a synthetic RGB-capable session test that uploads a 2×1 RGB picture, renders grayscale chrome around it, and asserts the runtime selects RGB8, preserves the picture triples, and writes equal-channel chrome. Assert an RGB upload is refused on a Gray8 session. Assert a Gray8 upload remains valid on an RGB-capable session, draws on Gray8 when no RGB picture is referenced, and expands to equal channels when composed with a referenced RGB picture.

Add simulator tests that its ideal/visible buffers preserve RGB triples and that GCC16/GLRC16 transition names are recorded.

- [ ] **Step 2: Run integration tests and verify they fail**

Run: `cargo test -p kobod rgb_picture`

Run: `cargo test -p kobo-sim color`

Expected: compilation fails because runtime and simulator construct Gray8 surfaces and snapshots.

- [ ] **Step 3: Select the shallowest legal surface per screen**

Add one traversal/helper that inspects only picture handles referenced by the screen and their cached formats. Return RGB8 only when metrics accept RGB8 and at least one referenced cached picture is RGB8; otherwise return Gray8. Construct `Surface::new_in` with that result, pass `surface.pixels()` and the active profile's color capability to `RegionSnapshot::from_pixels`, and let `FramePlanner` compare or reset typed previous storage safely across format changes.

Map `PanelWaveform::Glrc16` to `RefreshIntent::ColorContent` and `Gcc16` to `RefreshIntent::ColorQuality`; pass the active profile's `ColorPanel` into HAL lowering. Commit the planner only after snapshot write and refresh succeed. At message receipt, accept Gray8 on every session and accept RGB8 only when session metrics are RGB8. Keep replacement atomic: a refused or incomplete typed upload leaves the old live picture intact.

- [ ] **Step 4: Type simulator ideal and visible buffers**

Store the simulator's ideal and visible surfaces in the selected format. Keep the existing E Ink ghosting approximation for grayscale values. For RGB8, apply it independently per channel while using the shared `FramePlanner` transition; never collapse to luma. Record the exact waveform selected by the planner.

- [ ] **Step 5: Preserve color in screenshots and previews**

Route typed surfaces through `kobo_image::encode_png`. Update recording frames to carry `PicturePixels` plus format, and version or reject the old recording shape rather than reinterpreting Gray8 bytes as RGB8. The default SDK preview remains Gray8 unless explicitly constructed with RGB8 metrics.

- [ ] **Step 6: Run runtime, simulator, and CLI tests**

Run: `cargo test -p kobod`

Run: `cargo test -p kobo-sim`

Run: `cargo test -p kobo-cli`

Expected: Gray8 sessions are unchanged, synthetic RGB8 sessions preserve color end to end, format mismatch fails closed, and PNG evidence has the correct PNG color type.

- [ ] **Step 7: Commit runtime color integration**

```bash
git add crates/kobod/src/device.rs crates/kobo-hal/src/display.rs crates/kobo-sim/src/lib.rs crates/kobo-cli/src/drive.rs crates/kobo-cli/src/main.rs crates/kobo-sdk/examples/preview.rs
git commit -m "feat(runtime): render typed color surfaces"
```

---

### Task 8: Validate and Enable Each Color Device Profile

**Files:**
- Modify after evidence: `crates/kobo-profile/src/lib.rs` color profile constants and evidence comments
- Modify after evidence: `docs/devices.md` existing device-evidence table/section
- Evidence only: attended Clara Colour and/or Libra Colour device, exact installed firmware recorded

**Interfaces:**
- Consumes: Tasks 4-7 dormant color capability, the probed RGBA channel fields, and candidate waveforms/flags `GCC16 = 10`, `GLRC16 = 11`, `CFA G2 = 0x600`.
- Produces: an explicit `Some(ColorPanel)` value with every field shown in Step 4 only for each profile that independently passes the attended gate. Profiles without complete evidence remain `None` and advertise Gray8.

- [ ] **Step 1: Build and install a color-evidence runtime without enabling the profile**

Build: `cargo build -p kobod --features device-write --release`

Use the repository's attended device deployment flow. The probe must accept explicit candidate values on the command line or in a local uncommitted override; do not commit a profile capability before evidence succeeds.

- [ ] **Step 2: Record framebuffer and firmware facts**

For each device, record model/profile ID, firmware, controller, visible/virtual geometry, stride, bpp, red/green/blue/transparency bitfield offsets and lengths, and a pre-test screen snapshot checksum. Reject the gate if the runtime bitfields do not match the packing assumptions from Task 5.

- [ ] **Step 3: Exercise the attended visual matrix**

Display, inspect, and capture:

1. saturated red, green, blue, cyan, magenta, yellow, black, white;
2. equal-channel grayscale ramp;
3. a color photograph with skin tones and gradients;
4. grayscale chrome around a color picture;
5. partial GLRC16 changes confined to the picture;
6. first-color and threshold-triggered full GCC16;
7. color-to-gray exit clean;
8. touch in all attended corners after color updates;
9. normal app exit and exact original-screen restoration;
10. runtime restart/recovery after an interrupted update.

A human must confirm channel identity, no red/blue swap, readable grayscale chrome, acceptable residue, touch alignment, exit, and recovery. Simulator screenshots are not evidence for this task.

- [ ] **Step 4: Enable only the profile that passed**

For a passing HWTCON profile whose attended capture confirms eight-bit RGBA at offsets 0/8/16/24, set:

```rust
color: Some(ColorPanel {
    red: ChannelField { offset: 0, length: 8 },
    green: ChannelField { offset: 8, length: 8 },
    blue: ChannelField { offset: 16, length: 8 },
    transparency: ChannelField { offset: 24, length: 8 },
    clean_waveform: 10,
    regal_waveform: 11,
    cfa_flags: 0x600,
    clean_interval: 4,
}),
```

If the captured offsets differ, encode the captured non-overlapping eight-bit fields instead; never reuse the example offsets against contradictory device output.

Add an adjacent comment naming device, firmware, date, observed channel order, waveform results, CFA flag, touch, exit, and recovery evidence. If either color device was unavailable or failed any check, leave that profile `None` without weakening validation.

- [ ] **Step 5: Re-run profile and runtime gates**

Run: `cargo test -p kobo-profile`

Run: `cargo test -p kobod color`

Expected: enabled profiles advertise RGB8; all other profiles advertise Gray8; validation still rejects unsupported controllers and incomplete capability data.

- [ ] **Step 6: Commit attended evidence and profile enablement**

```bash
git add crates/kobo-profile/src/lib.rs docs/devices.md
git commit -m "feat(profile): enable verified Kobo color"
```

If no profile passed, do not create this commit; the software remains complete but dormant and safely falls back to Gray8.

---

### Task 9: Run Platform Quality Gates

**Files:**
- Modify only if a gate finds a defect: files changed in Tasks 1-8

**Interfaces:**
- Consumes: complete platform color pipeline.
- Produces: formatted, warning-free, workspace-compatible code and recorded simulator evidence.

- [ ] **Step 1: Format and check formatting**

Run: `cargo fmt --all`

Run: `cargo fmt --all --check`

Expected: both commands succeed.

- [ ] **Step 2: Run focused color workspace tests**

Run: `cargo test -p kobo-ui -p kobo-image -p kobo-protocol -p kobo-sdk -p kobo-profile -p kobo-hal -p kobod -p kobo-sim -p kobo-cli`

Expected: all tests pass.

- [ ] **Step 3: Run workspace tests**

Run: `cargo test --workspace --all-targets --all-features`

Expected: all targets pass under all features.

- [ ] **Step 4: Run Clippy as an error gate**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: no warning or error.

- [ ] **Step 5: Exercise both simulator formats**

Run the existing browser/runtime simulator once with default `CLARA_BW_METRICS` and once with injected RGB8 metrics. Upload a saturated test picture, navigate to a grayscale screen, and capture typed PNGs. Expected: Gray8 output remains grayscale; RGB8 output preserves color; transition log shows GCC16 first, GLRC16 partial, and GCC16 on color exit/threshold.

- [ ] **Step 6: Commit gate-driven fixes only if needed**

```bash
git add -u
git commit -m "fix(color): satisfy platform quality gates"
```

Skip this commit when the tree is already clean.
