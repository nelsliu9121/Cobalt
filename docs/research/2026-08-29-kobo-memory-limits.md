# Kobo memory limits for the BOMTOON continuous reader

**Research date:** 2026-08-29  
**Status:** evidence review and working-set analysis for the approved continuous-reader design; no spec or plan files were modified.

## Executive conclusion

Keep the **three app page buffers / two decoded-or-fetching sources / two image fetches** topology. On the Clara BW geometry, those named payloads plus two maximum compressed image responses and one runtime-held displayed page total only **28,597,632 bytes (27.272827 MiB)**. The larger Elipsa 2E geometry raises three app pages plus one runtime page from **6,209,024 bytes (5.921387 MiB)** to **10,513,152 bytes (10.026123 MiB)**. Cutting a page would save little compared with decoder, resizer, response-delivery, and upload transients.

Do **not**, however, accept the plan's current `image::imageops::resize` compatibility fallback as memory-bounded. For an allowed near-panel-width Clara input, its exact major pixel-buffer subtotal is **132,852,298 bytes (126.697824 MiB)**. With the response, one older cached source, and three app pages, the app-process subtotal is **148,702,458 bytes (141.813715 MiB)** before allocator overhead, library state, thread stacks, protocol buffers, and the runtime process. This is the dominant defect, not lookahead depth.

The strongest primary-source memory evidence found for any repository-supported device is an upstream Linux device tree for **Kobo Clara HD** that maps `0x20000000` bytes: **536,870,912 bytes (512 MiB)**. This is credible evidence for the kernel's physical address map on that board, not a Kobo-published BOM, not post-reservation Linux `MemTotal`, and not a sandbox/app allowance. Kobo's product pages do not publish RAM for the other supported models. A fleet-wide installed-RAM minimum and a per-app budget therefore remain **unknown**.

Required design correction: retain 3/2/2, but either reject a server response whose decoded width is not the requested panel width, or replace Lanczos with a measured, strip/row-bounded grayscale scaler before release. Also correct the spec's “one transient compressed response” and “exactly one runtime picture” wording: queued task outcomes, protocol copies, network decoding, and atomic upload make those statements true only under narrower steady-state definitions.

## What “memory” means

These quantities are not interchangeable:

1. **Installed physical RAM** is the DRAM populated on the board. Kobo does not publish this value on the official product pages reviewed below.
2. **Linux-visible/usable RAM** is what the booted kernel exposes, best recorded as `/proc/meminfo` `MemTotal`. It is normally below an address-map size because firmware, kernel, CMA, framebuffer, and other reserved regions consume or exclude memory. No first-party boot log or `/proc/meminfo` capture was found for the repository's supported fleet.
3. **Memory available to one sandboxed app** depends on process limits, cgroups, the runtime, other resident processes, swap, and the OOM policy. This repository has no `RLIMIT_AS`, `RLIMIT_DATA`, `RLIMIT_RSS`, `setrlimit`, `prlimit`, `memory.max`, or cgroup memory configuration. That means **no repository-defined per-app ceiling was found**; it does not prove that an external launcher or device image imposes none.

A 512 MiB device-tree region must therefore never be cited as “BOMTOON has 512 MiB.”

## Actual repository scope

The support claim is the seven profile entries in [`SUPPORTED_PROFILES`](../../crates/kobo-profile/src/lib.rs#L691-L699), not every Kobo product and not every model mentioned on the web:

| Repository profile(s) | Product/model | Machine IDs | Configured panel pixels | One 8-bit gray page |
|---|---|---:|---:|---:|
| `CLARA_BW_391`, `CLARA_BW_395` | Clara BW | N365, P365 | 1,072 × 1,448 | 1,552,256 B (1.480347 MiB) |
| `CLARA_HD_376` | Clara HD | N249 | 1,072 × 1,448 | 1,552,256 B (1.480347 MiB) |
| `CLARA_COLOUR_393` | Clara Colour | N367 | 1,072 × 1,448 | 1,552,256 B (1.480347 MiB) |
| `ELIPSA_2E_389` | Elipsa 2E | N605 | 1,404 × 1,872 | 2,628,288 B (2.506531 MiB) |
| `LIBRA_2_388` | Libra 2 | N418 | 1,264 × 1,680 | 2,123,520 B (2.025146 MiB) |
| `LIBRA_COLOUR_390` | Libra Colour | N428 | 1,264 × 1,680 | 2,123,520 B (2.025146 MiB) |

The project's public matrix says the same models are supported in [`README.md`](../../README.md#L29-L40) and [`docs/DEVICES.md`](../DEVICES.md#L7-L20). `CLARA_BW_METRICS` is useful for byte-exact tests but is **not evidence that Clara BW is the lowest-RAM device**.

## Model-by-model RAM evidence

“Unknown” below is deliberate. Secondary databases and forum assertions were not promoted to evidence.

| Supported model | Installed physical RAM | Linux address map / usable RAM | App allowance | Primary-source assessment |
|---|---:|---:|---:|---|
| Clara BW (N365/P365) | **Unknown** | **Unknown** | **Unknown** | Kobo's [official Clara BW page](https://us.kobobooks.com/products/kobo-clara-bw) lists product/storage/display facts but no RAM (accessed 2026-08-29). No model-specific first-party memory declaration was found. |
| Clara HD (N249) | Not Kobo-published; **512 MiB is strongly suggested, not a board-BOM proof** | Upstream Linux maps `0x80000000 + 0x20000000`, i.e. **536,870,912 B (512 MiB)**; post-reservation `MemTotal` is unknown | **Unknown** | Linux's model DTS names “Kobo Clara HD” and includes the E60K02 board file; that board file declares the 512 MiB region: [model DTS](https://raw.githubusercontent.com/torvalds/linux/master/arch/arm/boot/dts/nxp/imx/imx6sll-kobo-clarahd.dts), [memory node](https://raw.githubusercontent.com/torvalds/linux/master/arch/arm/boot/dts/nxp/imx/e60k02.dtsi) (both accessed 2026-08-29). |
| Clara Colour (N367) | **Unknown** | **Unknown** | **Unknown** | Kobo's [official Clara Colour page](https://us.kobobooks.com/products/kobo-clara-colour) does not publish RAM (accessed 2026-08-29). |
| Elipsa 2E (N605) | **Unknown** | **Unknown** | **Unknown** | Kobo's [official Elipsa 2E page](https://us.kobobooks.com/products/kobo-elipsa-2e) does not publish RAM (accessed 2026-08-29). |
| Libra 2 (N418) | **Unknown** | **Unknown** | **Unknown** | Kobo's [official Libra 2 page](https://us.kobobooks.com/products/kobo-libra-2) publishes storage but not RAM (accessed 2026-08-29). |
| Libra Colour (N428) | **Unknown** | **Unknown** | **Unknown** | Kobo's [official Libra Colour page](https://us.kobobooks.com/products/kobo-libra-colour) does not publish RAM (accessed 2026-08-29). |

### Why the Clara HD result is a floor, not certainty

The upstream Linux files are the best model-specific primary source located. The E60K02 memory node's size cell is exactly `0x20000000`, which converts to 512 MiB. It describes the kernel address map expected for the board, but it does not report live `MemTotal`, reserved/CMA regions, or app headroom.

The manufacturer source dump is not safer to read literally: Kobo's official [Clara hardware source archive](https://github.com/kobolabs/Kobo-Reader/tree/master/hw/imx6sll-clara) includes generic 256 MiB and 512 MiB bootloader configurations, while its generic kernel base DTS has also circulated with a 2 GiB memory node (accessed 2026-08-29). Those mutually broad build-time declarations are evidence that a source package supports configurations, not evidence of the populated Clara HD board. The model-specific upstream 512 MiB DTS is therefore the conservative engineering floor, while the supported fleet's true physical minimum remains unproven.

**Planning rule:** validate on a device whose measured `MemTotal` is the smallest in the supported fleet. Until captures exist, treat **512 MiB physical address-map evidence** as the test-floor hypothesis, never as an app budget.

## Repository-enforced bounds

### BOMTOON

The current client applies these response limits in [`apps/bomtoon/src/api.rs`](../../apps/bomtoon/src/api.rs#L7-L9):

- manifest/content response: 512 KiB;
- library response: 2 MiB;
- image response: `kobo_image::MAX_SOURCE_BYTES`, 4 MiB.

Manifest parsing limits an episode to 256 images and a signed image URL to 1,024 bytes in [`apps/bomtoon/src/parse.rs`](../../apps/bomtoon/src/parse.rs#L6-L8). The approved plan keeps page plans/build state bounded, but page-plan metadata still scales up to those explicit episode/page caps; “independent of episode length” is accurate for the image/pixel window, not for all metadata.

The approved design specifies three app-owned gray page buffers, at most two decoded/cached-or-fetching source images, at most two image fetches, and one displayed runtime picture ([design](../superpowers/specs/2026-08-29-bomtoon-continuous-reader-design.md#L83-L104)). The plan codifies `LOOKAHEAD_PAGES = 3`, `MAX_DECODED_SOURCES = 2`, and `MAX_IMAGE_FETCHES = 2` and asserts cache plus fetch slots do not exceed two ([plan](../superpowers/plans/2026-08-29-bomtoon-continuous-reader.md#L395-L449)).

### Image crate

[`kobo-image`](../../crates/kobo-image/src/lib.rs#L20-L31) enforces:

- `MAX_SOURCE_BYTES = 4,194,304` bytes;
- `MAX_PIXELS = 7,000,000` decoded gray pixels;
- a `Picture` gray buffer is one byte per pixel ([representation](../../crates/kobo-image/src/lib.rs#L130-L165));
- Floyd–Steinberg dither scratch is two `i32` rows, exactly `2 × width × 4` logical bytes ([dither](../../crates/kobo-image/src/lib.rs#L356-L400)).

The crate is pinned to `image` 0.25.8 ([lockfile](../../Cargo.lock#L537-L541)). Decode is not a one-buffer operation: the `DynamicImage`, a two-byte-per-pixel luma/alpha conversion, and the final one-byte gray vector can coexist ([decode path](../../crates/kobo-image/src/lib.rs#L537-L586)). A 90°/270° EXIF orientation can make a full internal copy; `image` documents that behavior in its [v0.25.8 orientation implementation](https://github.com/image-rs/image/blob/v0.25.8/src/images/dynimage.rs#L1155-L1178) (accessed 2026-08-29).

Scaling is more expensive. `scale_to_width` first clones the full gray source ([wrapper](../../crates/kobo-image/src/lib.rs#L189-L208)); `image` 0.25.8's Lanczos path makes an intermediate `Rgba32FImage` for the vertical pass and then the final target ([v0.25.8 scaler source](https://github.com/image-rs/image/blob/v0.25.8/src/imageops/sample.rs#L962-L1014), accessed 2026-08-29). `Rgba32F` is four 32-bit floats: 16 bytes per intermediate pixel.

The existing BOMTOON implementation calls `scale_to_width` even at equal width, so it temporarily clones the entire gray source ([current `accept_image`](../../apps/bomtoon/src/main.rs#L602-L638)). The approved plan correctly changes equal-width input to a move and invokes scaling only as a mismatch fallback ([plan](../superpowers/plans/2026-08-29-bomtoon-continuous-reader.md#L330-L361)).

### SDK, runtime, and protocol

- The SDK policy permits four tasks in flight, not two ([`MAX_TASKS_IN_FLIGHT`](../../crates/kobo-policy/src/tasks.rs#L24-L28)). Each task uses a `thread::Builder` without an explicit stack size ([submission](../../crates/kobo-policy/src/tasks.rs#L400-L443)); platform stack virtual/RSS cost is outside the byte totals below.
- A completed task owns a `Vec<u8>` and task bodies are capped at 4 MiB ([protocol limits/outcome](../../crates/kobo-protocol/src/lib.rs#L90-L103)). The task runner drains **all** queued completions into a `Vec`, so up to four outcomes may remain live in the runtime before delivery ([drain](../../crates/kobo-policy/src/tasks.rs#L458-L465)). The reader's own policy limits image fetches to two; maintenance sleep outcomes are empty and manifest refresh is capped at 512 KiB.
- Runtime `PictureCache` has an 8 MiB held-picture budget because the runtime source itself assumes a 512 MB, no-swap device ([budget rationale](../../crates/kobo-ui/src/lib.rs#L10112-L10133)). That comment is an implementation assumption, not new hardware evidence. Pending uploads are separate from held pictures.
- Inline picture data is capped at 768 KiB and upload chunks at 256 KiB ([protocol picture limits](../../crates/kobo-protocol/src/lib.rs#L62-L67)). All supported full-screen gray pages therefore use chunked upload.

## Byte-exact `CLARA_BW_METRICS` accounting

All MiB values use 1 MiB = 1,048,576 bytes. “Exact” means the logical lengths of named buffers; `Vec` metadata, capacity rounding, allocator arenas/fragmentation, code/shared libraries, stacks, TLS, kernel socket buffers, and baseline runtime surfaces are excluded unless named.

Let:

- `W = 1,072`, `H = 1,448`;
- panel page `P = W × H = 1,552,256 B`;
- maximum compressed image body `C = 4,194,304 B`;
- generic decoded bound `M = 7,000,000 B`;
- largest panel-width decoded source `S = 1,072 × floor(7,000,000 / 1,072) = 1,072 × 6,529 = 6,999,088 B`;
- upload chunk `K = 262,144 B`;
- page-width dither scratch `R = 2 × 1,072 × 4 = 8,576 B`.

### Page/source/fetch envelope

| Named logical storage | Formula | Bytes | MiB |
|---|---:|---:|---:|
| One gray page | `P` | 1,552,256 | 1.480347 |
| Three app lookahead/build/ready pages | `3P` | 4,656,768 | 4.441040 |
| Three app pages plus one runtime displayed picture | `4P` | 6,209,024 | 5.921387 |
| Two generic maximum decoded gray sources | `2M` | 14,000,000 | 13.351440 |
| Two maximum compressed image bodies | `2C` | 8,388,608 | 8.000000 |
| `3P + 2M + 2C + P` | design payload envelope | **28,597,632** | **27.272827** |
| One page-width dither operation | `R` | 8,576 | 0.008179 |

The 27.27 MiB row intentionally combines payloads that can be split between app, runtime queues, and socket delivery. It is a useful storage envelope, not a simultaneous-RSS theorem.

### Approved same-width move path

After decode, equal-width `Picture` ownership moves into the source cache: there is no source-size clone. Copying source rows into the three preallocated page vectors creates no further full-page allocation; completing a page moves its vector into `Picture`, and dither adds only `R`.

Decode itself dominates. For the largest allowed panel-width source `S`:

- expected/common 8-bit RGBA decoder case: `DynamicImage 4S + luma-alpha 2S + gray S = 7S = 48,993,616 B`;
- with the callback body, one older cached `S`, and three pages: `7S + C + S + 3P =` **64,843,776 B (61.839844 MiB)** in the app-process logical-buffer subtotal;
- a permitted 16-bit RGBA PNG conversion can reach `8S + 2S + S = 11S`; the same aggregate becomes **92,840,128 B (88.539246 MiB)**;
- a 90°/270° orientation can transiently hold two 16-bit RGBA images, `16S`; the same aggregate becomes **127,835,568 B (121.913498 MiB)**.

BOMTOON requests `Accept: image/webp`, but `decode` recognizes JPEG, PNG, and WebP from the bytes and the current API does not make WebP-only decoding a validated invariant. A WebP-only budget is justified only after validating the received representation before decode.

### Lanczos scaling fallback peak

The worst near-width Clara case under both current seven-million-pixel checks is reproducible with source **1,071 × 6,523**:

- original decoded gray: `1,071 × 6,523 = 6,986,133 B`;
- clone made by `scale_to_width`: `6,986,133 B`;
- target height: `floor(1,072 × 6,523 / 1,071) = 6,529`;
- vertical `Rgba32F` intermediate: `1,071 × 6,529 × 16 = 111,880,944 B`;
- final gray target: `1,072 × 6,529 = 6,999,088 B`.

Exact major pixel-buffer subtotal:

```text
6,986,133 + 6,986,133 + 111,880,944 + 6,999,088
= 132,852,298 bytes
= 126.697824 MiB
```

Adding buffers that the plan permits to coexist in the app callback:

```text
132,852,298 scaling pixels
+  4,194,304 compressed callback body
+  6,999,088 one older panel-width cached source
+  4,656,768 three app page buffers
=148,702,458 bytes
=141.813715 MiB
```

One runtime displayed page and one other queued maximum image outcome raise the cross-process named-buffer subtotal to **154,449,018 B (147.294062 MiB)**. The actual RSS peak is higher: filter-weight vectors, capacities/allocator overhead, runtime surfaces, network/protocol copies, sockets, and stacks are absent. Exhaustive enumeration of positive integer source dimensions satisfying both current pixel checks found this 1,071 × 6,523 case as the maximum of the four named scaler pixel buffers; it is a calculation over the pinned implementation, not a device measurement.

## Largest supported page geometry

The lookahead decision remains cheap on Elipsa 2E, the largest supported profile:

| Geometry | One page | Three app pages | Three app pages + one runtime picture | Dither scratch |
|---|---:|---:|---:|---:|
| Clara family, 1,072 × 1,448 | 1,552,256 B (1.480347 MiB) | 4,656,768 B (4.441040 MiB) | 6,209,024 B (5.921387 MiB) | 8,576 B |
| Libra family, 1,264 × 1,680 | 2,123,520 B (2.025146 MiB) | 6,370,560 B (6.075439 MiB) | 8,494,080 B (8.100586 MiB) | 10,112 B |
| Elipsa 2E, 1,404 × 1,872 | 2,628,288 B (2.506531 MiB) | 7,884,864 B (7.519592 MiB) | **10,513,152 B (10.026123 MiB)** | 11,232 B |

Saving one lookahead page saves only 1.48–2.51 MiB across this fleet. That does not compensate for a 111.88 MiB Clara scaler intermediate.

## Transients omitted by the approved wording

### Compressed network responses

“One transient compressed response per completed task callback” is too narrow:

1. The network worker reads a whole raw HTTP response. Identity/content-length delivery can retain the raw body while producing the returned body copy, approaching `2C` in logical body storage.
2. Chunked and gzip handling can successively retain raw, dechunked, expanded, and final copied bodies. The worst phase is input/header/capacity dependent, but up to roughly `4C` of logical body data is plausible before intermediates drop. It is not byte-exact from the public limit alone.
3. `TaskRunner::drain_finished` drains all queued results; the SDK-wide maximum is four bodies, while the reader design permits two 4 MiB image results.
4. Encoding a completed outcome retains its original `Vec`, copies it into a payload, then copies the payload into a frame: approximately `3C` in the runtime sender for one full result. The app receiver first owns the frame and `decode` clones the outcome body, approximately `2C`, before the frame drops. Kernel socket buffers add another unaccounted copy domain.
5. The callback receives a borrowed slice, so image decode does not clone the delivered body again; `C` nevertheless remains live until the callback returns.

Consequently, response wire/queue phases and image-decode/scale callback phases must be measured separately and under simultaneous two-fetch completion.

### Chunked picture upload

“No app-side clone” is directionally correct for the full page—`Vec` ownership moves into the SDK command—but it does not mean no transient copies:

- the app keeps the full `P` while a 256 KiB chunk is copied into a command, copied into protocol payload, and copied into the final frame; the named Clara sender subtotal can approach `P + 3K = 2,338,688 B` before headers/capacities;
- the runtime receives a frame and decode clones the chunk;
- runtime upload builds a new pending full-page `Vec` while the old displayed picture remains held;
- commit moves the new vector into the held cache, `SetScreen` switches to it, and only then is the old handle dropped.

Thus “exactly one runtime picture handle” is a **steady-state ownership invariant**, not an upload-peak memory invariant. The correct transient invariant is: one old held picture plus one bounded pending/new picture may coexist, with bounded chunk/frame copies. The 8 MiB held-picture budget alone does not cap the pending upload vector.

## Conservative release bound and measurement procedure

The following are proposed engineering gates, **not deductions about a sandbox entitlement**:

1. **Format invariant:** validate that an image response is WebP before invoking generic decode, or explicitly account for the accepted decoder's largest representation and EXIF orientation copy.
2. **App allocation budget:** keep modeled BOMTOON live allocations at or below **96 MiB**. The 61.84 MiB expected same-width subtotal leaves about 34 MiB for unmodeled app allocations; the 141.81 MiB Lanczos subtotal fails immediately.
3. **Device gates on the lowest measured supported device:** BOMTOON `VmHWM` at or below **128 MiB**, no OOM/kill, and system `MemAvailable` never below **128 MiB** during the scenario. These thresholds are deliberately conservative policy choices. They must be revised from evidence, not re-described as “512 MiB available to the app.”
4. If those gates fail, first remove/bound fallback scaling or lower `MAX_PIXELS`; reducing page lookahead is a last lever because it saves at most 2.51 MiB per page.

Measurement steps on every supported profile/firmware combination, starting with the smallest `MemTotal`:

1. Cold boot the target runtime. Capture `/proc/meminfo` (`MemTotal`, `MemAvailable`, `CmaTotal`, `CmaFree`, `SwapTotal`, `SwapFree`) and `/proc/<pid>/limits` for BOMTOON and `kobod`.
2. At reader idle and at every phase marker, sample `/proc/<pid>/status` (`VmRSS`, `VmHWM`, `RssAnon`, `VmSwap`) and `/proc/<pid>/smaps_rollup` (`Rss`, `Pss`, `Private_Dirty`) for both processes. Record system `MemAvailable` concurrently.
3. Mark these phases: reader opened; two image fetches in flight; both completions queued; first callback receiver decode; image decode/conversion; mismatch scale; three pages ready/building; upload before commit; new picture committed while old handle remains; old handle dropped.
4. Exercise 4 MiB compressed bodies at or near seven million decoded pixels. Include maximum-height panel-width WebP, simultaneous dual completions, and malformed/non-WebP response. If a scaler remains, include the Clara 1,071 × 6,523 case and corresponding near-width cases on Libra and Elipsa.
5. Add allocator-side counters in the simulator for `Vec::capacity` of responses, decoded images, page builds, scaler intermediates, protocol payload/frame, pending uploads, and held pictures. RSS alone cannot attribute a peak or reveal retained allocator arenas.
6. Repeat the worst scenario through many page seams and episode end. Pass only with stable high-water marks, no swap growth, no allocation failure/OOM, and all gates above satisfied.

## Required spec and plan amendments

No approved file was edited, but the following corrections are required before implementation is considered memory-safe:

1. **Scope/floor:** name the exact `SUPPORTED_PROFILES`. State that Clara HD has a 512 MiB upstream device-tree address map; fleet installed RAM, per-device `MemTotal`, and per-app allowance are unknown until measured. Do not call `CLARA_BW_METRICS` the memory floor.
2. **Response bound:** replace “one transient compressed response per callback” with separate bounds for network decode/copy, up to four runtime queued outcomes SDK-wide, two reader image outcomes, runtime outcome/payload/frame copies, and app frame/outcome copies.
3. **Decoder bound:** either enforce WebP bytes or include generic decoder color depth and orientation copies in the budget.
4. **Scaler bound:** replace generic Lanczos fallback with a strip/row-bounded grayscale implementation under the 96 MiB modeled app-allocation budget, or fail closed on width mismatch. The present fallback must not ship merely because source and target pixels are each at most seven million.
5. **Upload bound:** preserve one displayed handle at steady state, but document and test old held plus pending/new full page and bounded per-chunk protocol copies during replacement.
6. **Acceptance gate:** add the on-device measurement procedure and the proposed 128 MiB app-HWM/128 MiB `MemAvailable` release gates. Record measured `MemTotal` by model and firmware; replace conservative hypotheses only with captured evidence.

With those amendments, the recommended architecture is **keep three pages, keep the combined two-source window, keep two image fetches; change the scaler, response accounting, upload wording, and device-memory acceptance evidence**.
