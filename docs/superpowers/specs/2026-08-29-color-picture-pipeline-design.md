# Color picture pipeline for Kobo Colour

## Status

Design approved in discussion. Written specification awaiting review.

This specification adds a platform color-picture capability and revises the color-handling portions of `2026-08-29-bomtoon-continuous-reader-design.md`. The continuous-reader implementation plan must not be executed until it has been regenerated against this specification.

## Goal

Preserve color in BOMTOON comic pictures on attended, verified Kobo Colour profiles while keeping application chrome, text, non-picture UI, monochrome devices, and unverified color devices grayscale.

A color model name, a 32-bit framebuffer, or an RGB-capable bitfield layout is not sufficient evidence that the panel refresh path is safe. Color is enabled only by an explicit profile capability backed by exact model, firmware, framebuffer, waveform, memory, exit, and recovery evidence.

## Current limitation

The current pipeline is grayscale end to end:

- `kobo-image::Picture` owns one luminance byte per pixel.
- Decode applies embedded orientation, converts color through perceptual luma, and composites alpha onto white.
- SDK and protocol `PutPicture` messages carry a field named `grey` whose length must be `width * height`.
- The runtime picture cache stores gray bytes.
- `kobo-ui::Surface` and `FramePlanner::previous` store one tone byte per pixel.
- `kobod::Painter` converts the changed gray region into equal red, green, and blue framebuffer bytes.
- `FramePlanner` selects only DU, GL16, and GC16.

This is correct for monochrome output, including color source images: opaque RGB becomes luminance, transparent pixels are composited onto white paper, and the assembled page is Floyd–Steinberg dithered to sixteen gray levels. It cannot preserve chroma on a Kaleido panel.

## Scope

This specification owns the reusable typed color-picture pipeline across image preparation, SDK, protocol, runtime cache, UI composition, frame planning, framebuffer conversion, simulator output, and profile capability checks.

Application-specific pagination, prefetch scheduling, source retention, and application memory arithmetic remain in each consumer specification. BOMTOON's selected-format behavior is owned by `2026-08-29-bomtoon-continuous-reader-design.md`.

Only picture pixels may introduce chroma. UI primitives continue to accept grayscale tones. On an RGB render surface, a UI tone `g` is written as `(g, g, g)`, preserving existing layout, contrast, and styling.

## Non-goals

- Colored UI themes, text styles, controls, backgrounds, or application-defined accent colors.
- ICC profile processing or a new color-management dependency.
- Kaleido-specific palette quantization or application-side color dithering.
- Inferring color safety from a retail model name, framebuffer depth, or channel bitfields.
- Enabling color on a profile without attended evidence from its exact supported firmware boundary.
- Changing monochrome application output or its existing three-page lookahead behavior.
- Allowing applications to open framebuffer or refresh device nodes directly.

## Capability model

`DeviceProfile` gains an optional color-picture capability. Its absence means Gray8 output, including on retail color models.

The internal capability records:

- validated framebuffer channel order and eight-bit channel layout
- full color cleaning waveform
- partial/regal color waveform
- required CFA processing flags
- color cleaning interval
- the exact profile and firmware evidence that approved those values

Applications do not receive raw waveform or framebuffer details. `DisplayMetrics` exposes only the picture format the runtime accepts:

```rust
enum PictureFormat {
    Gray8,
    Rgb8,
}
```

Both the SDK and runtime reject an RGB picture when metrics/profile capability is Gray8. This is defense in depth; a conforming application selects its decode path from `DisplayMetrics` and never sends an unsupported format.

Clara Colour and Libra Colour initially retain no enabled color-picture capability. They therefore continue to display grayscale pictures until each exact profile and firmware passes the attended gate in this document.

## Typed picture ownership

Picture payloads become explicitly formatted rather than relying on a field name:

```rust
enum PicturePixels {
    Gray8(Vec<u8>),
    Rgb8(Vec<u8>),
}
```

The owning picture type stores width, height, and `PicturePixels`. Its checked byte length is:

```text
Gray8: width * height
Rgb8:  width * height * 3
```

Every multiplication and conversion is checked before allocation, protocol encoding, cache insertion, chunk acceptance, surface creation, or framebuffer conversion. Format is part of inline and chunked upload metadata and cannot change between begin, chunks, and commit.

Generic `kobo-image::decode` remains the grayscale API for existing callers. The WebP-only reader API accepts the required output format. Grayscale-only operations remain format-safe: Floyd–Steinberg dithering is available only for Gray8 data and cannot silently quantize an RGB picture.

## WebP color handling

Both paths validate the WebP signature, compressed-byte limit, decoded dimensions, pixel limit, and embedded orientation before returning pixels.

### Gray8

The existing behavior remains:

1. Convert decoder color to perceptual eight-bit luminance plus alpha.
2. Composite luminance onto white paper with rounded integer arithmetic.
3. Scale grayscale with the bounded fixed-point bilinear scaler when necessary.
4. Assemble every page segment.
5. Dither the completed page once to sixteen gray levels so error diffusion crosses source seams.

### Rgb8

The color path:

1. Preserve the decoder-provided eight-bit red, green, and blue channels after orientation.
2. Composite alpha onto white independently for each channel:

```text
out = (channel * alpha + 255 * (255 - alpha) + 127) / 255
```

3. Scale all three channels with the same endpoint-aligned fixed-point bilinear coordinates and one final rounding step.
4. Assemble source rows into white RGB page buffers.
5. Upload RGB8 unchanged; do not apply grayscale conversion, Floyd–Steinberg gray dithering, or a Cobalt color palette.

The consuming scaler returns its original allocation on a same-width input. A mismatch allocates the final output and reuses at most 2,048 three-`u32` horizontal samples, 24,576 logical bytes. The denominator is axis-global. Supported panel widths fit in one sample chunk; wider generic images refill the same chunk while writing each row. Scratch never scales with target width.

## Consumer contract

A consumer selects `PictureFormat` once from `DisplayMetrics` before decoding its first picture. A session never switches formats midway. Losing a verified RGB upload is a foreground consumer failure, not a silent downgrade that would hide a platform defect and produce alternating color and grayscale pictures. An absent capability selects Gray8 before decode.

Each consumer owns and tests its format-specific cache, fetch, and application-memory bounds. The BOMTOON continuous-reader specification defines its Gray8 and Rgb8 page windows, source/fetch slots, response concurrency, 96 MiB modeled app gate, and foreground/background failure behavior. This platform specification owns the shared pixel, protocol, runtime, render-surface, framebuffer, waveform, and device-evidence bounds.

## SDK and protocol

SDK commands, protocol messages, chunked upload state, and runtime cache entries carry `PictureFormat`/`PicturePixels` explicitly. The legacy field name `grey` is removed in the clean cutover.

The format-aware picture byte ceiling must admit a full Libra Colour RGB page (`6,370,560` bytes). Inline upload remains limited to the existing 768 KiB. Larger pictures use existing 256 KiB chunks. Begin metadata fixes handle, dimensions, format, and expected byte count; chunks remain ordered and bounded; commit is atomic.

The runtime picture cache counts actual pixel bytes regardless of format. Its held-picture budget admits one full Libra Colour RGB page. Pending upload remains separately bounded, so one old held RGB page and one pending/new RGB page may coexist only until commit, screen replacement, and old-handle drop complete.

## Typed render surface

`kobo-ui::Surface` and `FramePlanner` support Gray8 and Rgb8 surfaces. A screen containing no verified RGB picture renders as Gray8 exactly as today. A screen containing a cached RGB picture renders as Rgb8 from the start, avoiding a gray-to-RGB promotion allocation.

Existing UI drawing methods remain tone-based:

- Gray8 surface: write one byte.
- Rgb8 surface: write three equal bytes.

Picture drawing copies typed pixels with the existing clipping and z-order. Gray pictures expand to equal RGB channels when drawn on an RGB surface. RGB pictures cannot be drawn on a Gray8 surface; surface format selection prevents that illegal state before rendering.

Frame differencing compares complete logical pixels, not luminance. Two colors with equal luminance still produce a changed region. The previous-frame buffer uses the same format as the current surface and resets safely when format changes.

Simulator screenshots preserve RGB8 picture color while grayscale UI remains neutral.

## Framebuffer conversion

`RegionSnapshot` gains an RGB constructor that accepts exactly `width * height * 3` bytes and packs them into the profile-validated 32-bit framebuffer layout with opaque alpha. It does not assume RGBA merely because current MediaTek color profiles report that order. Unsupported, overlapping, non-eight-bit, or otherwise inconsistent channel bitfields refuse color capability.

Gray conversion remains equal-channel and unchanged. RGB conversion is callable only through a verified color capability. Applications still never receive direct framebuffer access.

## Color refresh and residue cleanup

The current planner cleans after eight panel-area equivalents of changed grayscale pixels. That Gray8 policy remains unchanged.

Verified color surfaces use a separate four-equivalent interval:

```text
COLOR_PANEL_CLEAN_INTERVAL = 4
clean after = panel_width * panel_height * 4 changed pixels
```

The first chromatic surface receives a whole-screen color clean. Later chromatic updates use the measured partial/regal color waveform. When the accumulated changed-pixel budget reaches four screen equivalents, the next update is a whole-screen color cleaning refresh. The planner tracks whether uncleaned updates contained chroma. Leaving a chromatic screen for a grayscale screen forces an immediate whole-screen color clean, even when the numeric dirty budget has not expired, so color residue cannot survive into menus or another application.

Current external MediaTek/Kaleido evidence identifies these probe candidates:

- full color clean: `GCC16`, raw HWTCON waveform 10
- partial/regal color: `GLRC16`, raw HWTCON waveform 11
- CFA processing: G2 flags `0x600`

These values are not enabled from documentation alone. They become profile facts only after Cobalt's attended probe confirms rendered color, translated waveform IDs, completion waits, ghosting behavior, and recovery on the exact supported profile and firmware.

A changed pixel is chromatic when either its previous or current RGB value has unequal channels; removing color to neutral gray still requires the color waveform. A changed region whose previous and current pixels are all equal-channel retains existing DU/GL16/GC16 selection unless uncleaned chroma requires a whole-screen color clean.

## Failures and fallback

- Unverified or monochrome profile: advertise Gray8 and decode/display grayscale.
- RGB payload sent without capability: reject at SDK and runtime boundaries.
- Invalid format, size, channel count, chunk order, offset, or commit length: reject without replacing the live handle.
- WebP, dimension, scale, assembly, or upload failure: preserve the selected reader page and use existing foreground Try again/Back behavior.
- Background color prefetch failure: keep the visible page interactive and promote the failure only when navigation reaches it.
- Color framebuffer packing or refresh failure on a verified path: fail the runtime operation and rely on guardian restoration/normal hand-back; do not silently alter the session format.

Unknown task outcomes, cancelled generations, signed URL refresh, picture handle ordering, and authentication behavior remain as specified by the continuous reader design.

## Automated tests

### Image preparation

- RGB WebP preserves distinct red, green, and blue channels.
- Per-channel alpha compositing places transparent pixels on white.
- Embedded orientation applies before output.
- Gray8 output retains perceptual luminance behavior.
- Fixed-point RGB bilinear scaling pins exact small-matrix channel values.
- Same-width Gray8 and Rgb8 paths reuse their allocations.
- Maximum-width thin images retain the 2,048-entry sample bound.
- Dithering cannot be invoked on Rgb8 data.

### SDK, protocol, and cache

- Gray8 and Rgb8 inline messages round-trip with explicit format.
- Both formats round-trip through begin/chunk/commit.
- Checked dimensions and channel counts reject short, long, oversized, and overflowed payloads.
- RGB is refused when display capability is Gray8.
- Upload format cannot change during a pending upload.
- Failed upload does not replace a held picture.
- RGB cache accounting, eviction, and old/pending/new ownership remain bounded.

### UI and framebuffer

- Tone primitives render identical neutral values on Gray8 and Rgb8 surfaces.
- RGB tiles preserve channels through clipping, scaling boundaries, and grayscale chrome overlap.
- Equal-luminance color changes still invalidate the frame.
- Surface-format transitions reset previous-frame state without out-of-bounds comparisons.
- RGB region packing follows tested RGBA and BGRA bitfields.
- Invalid bitfields cannot produce a color capability.
- Gray regions retain existing waveform selection.
- Chromatic regions select the verified partial color intent.
- The first chromatic frame uses a full color clean; after four screen-equivalents accumulate, the next update forces another full color clean.
- A color-to-gray pixel change still selects the color waveform.
- Leaving chromatic content forces a full color clean and resets color residue state.

### Consumer conformance

- A consumer can select Gray8 or Rgb8 solely from `DisplayMetrics`.
- Unverified color profiles exercise the unchanged Gray8 fallback.
- Consumer-specific RGB copying, cache, concurrency, and memory tests remain in that consumer's specification.
- BOMTOON's exact seam, scheduling, upload-order, and 96 MiB contracts are defined only in the linked continuous-reader specification.

## Simulator and attended verification

Browser and runtime simulators verify typed screenshots, clipping, chrome overlap, seam continuity, rapid turns, fallback, and format-aware memory accounting. A simulator does not approve Kaleido waveforms or physical color.

For each exact color profile and firmware candidate:

1. Capture `/proc/meminfo`, BOMTOON and `kobod` limits/status/smaps-rollup, framebuffer bitfields, and baseline screenshots.
2. Display full-screen red, green, blue, cyan, magenta, yellow, white, black, neutral gradients, color gradients, and a photographic fixture.
3. Record submitted and translated waveform IDs, CFA flags, submit time, completion time, and physical photographs for partial GLRC16 and full GCC16 candidates.
4. Turn through at least five chromatic full-screen pages. Confirm that four accumulated screen-equivalents cause the next update to perform a full clean and remove visible residue.
5. Leave the color reader for grayscale UI. Confirm the immediate color clean removes chromatic residue.
6. Exercise clipping, grayscale chrome over color, Back during active work, suspend/resume, application exit, guardian restoration, and clean stock-reader restart.
7. Exercise near-seven-million-pixel WebP, one maximum response, two RGB page buffers, pending upload, old/new coexistence, and repeated page seams.
8. Require BOMTOON `VmHWM <= 128 MiB`, `kobod VmHWM <= 128 MiB`, system `MemAvailable >= 128 MiB`, no swap growth, no allocation failure/OOM, correct physical colors, bounded ghosting, successful waits, and successful restoration.

If any gate fails, leave `color_picture_ready` absent for that profile. The released behavior remains grayscale rather than assuming partial color support.

## External evidence

The candidate waveform mapping is corroborated, not treated as device approval, by:

- NickelDissolve's MediaTek/Kaleido reverse engineering and hardware logs: <https://github.com/nicoverbruggen/NickelDissolve/blob/main/ABOUT.md>
- KOReader's HWTCON implementation, which defines conditional Kaleido `GCC16`/`GLRC16` waveforms and CFA processing: <https://github.com/koreader/koreader-base/blob/master/ffi/framebuffer_mxcfb.lua>

Repository profile comments remain authoritative about Cobalt's present support boundary: the Libra Colour framebuffer is 32-bit RGBA, but the profile explicitly does not claim that Cobalt's color path works without attended waveform evidence.

## Expected implementation areas

Platform work is expected in:

- `crates/kobo-profile`
- `crates/kobo-image`
- `crates/kobo-sdk`
- `crates/kobo-protocol`
- `crates/kobo-ui`
- `crates/kobo-hal`
- `crates/kobod`
- simulator/screenshot paths that currently assume one byte per pixel

Consumer integration files are owned by their consumer specifications. BOMTOON integration remains in `apps/bomtoon/src/api.rs` and `apps/bomtoon/src/main.rs`.

No new image, color-management, cache, or rendering dependency is planned.
