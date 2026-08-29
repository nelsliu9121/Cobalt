# Task 7 Report: Typed Runtime and Simulator Integration

## Status

Complete.

## Implementation

- Added one shared `kobo-ui::surface_format_for` traversal covering reading surfaces, overlays, nested cards/bands, inline formulae, rows, tiles, and picture nodes. It selects RGB8 only when the accepted metrics are RGB8 and a referenced cached picture is RGB8; unrelated or absent pictures leave the frame Gray8.
- Added validated, zero-copy `Surface::from_pixels` construction for complete typed buffers.
- Routed all device screen rendering through per-screen format selection. Gray8 screens remain Gray8 even in an RGB-capable synthetic session; Gray8 pictures expand to equal channels only when sharing an RGB8 surface.
- Added device-side defense-in-depth at both inline and chunked picture receipt. Gray8 is accepted by every session, RGB8 only by RGB8 metrics, and rejected or incomplete replacement uploads leave the old live picture intact.
- Removed the runtime's Gray8-only framebuffer guard. Changed regions now retain their `PicturePixels` type, pass `surface.pixels()` through region extraction, and use the active verified profile's `ColorPanel` during `RegionSnapshot` packing.
- Mapped GLRC16/GCC16 planner transitions to `ColorContent`/`ColorQuality`; `DisplaySession` already lowered those intents with `profile.color` from Task 6, so no additional HAL change was needed.
- Centralized runtime planner sequencing in `Painter::paint_with`: planning is side-effect free, framebuffer packing/write/refresh run first, and planner commit happens only after the complete operation succeeds. Failed output retries refresh number one.
- Replaced simulator byte vectors with typed Gray8/RGB8 ideal and visible buffers. Full transitions copy typed pixels; partial RGB transitions apply the existing residue approximation independently to red, green, and blue without luma conversion.
- Simulator embedding frames now convert typed buffers to exact RGBA, preserving RGB triples and using opaque alpha. Browser frame endpoints use typed `kobo_image::encode_png`, and the browser decodes those PNGs into its canvas.
- Updated simulator screenshots and `kobo shot` to carry the PNG format instead of assuming one raw gray byte per pixel. Gray8 evidence remains grayscale PNG; RGB8 evidence is RGB PNG.
- Updated recorded frames to hold typed `PicturePixels`. `KOBOCST1` remains explicitly and permanently Gray8, while unknown recording versions remain rejected rather than reinterpreting old gray bytes as RGB.
- Updated the SDK preview example to construct the surface from `metrics.picture_format` and write typed PNGs. Its default Clara BW metrics remain Gray8.
- Added only the unavoidable `kobo-image` dependencies and shared `kobo-ui` helper; no real device profile gained color capability and no BOMTOON code changed.

## RED Evidence

- `cargo test -p kobod --features device-write rgb_picture` failed to compile on the missing session upload gate, per-screen render helper, typed region extraction, typed surface constructor, and commit-after-output helper.
- `cargo test -p kobo-sim color` failed to compile because `PanelPreview` still accepted no dimensions, stored untyped byte vectors, lacked exact RGBA/PNG conversion, and could not construct synthetic RGB surfaces.

## Verification

- `cargo test -p kobod --features device-write rgb_picture` — 5 passed, 0 failed.
- `cargo test -p kobod --features device-write` — 83 passed, 0 failed.
- `cargo test -p kobod` — 52 passed, 0 failed; 11 pre-existing default-feature dead-code warnings.
- `cargo test -p kobo-sim color` — 2 passed, 0 failed.
- `cargo test -p kobo-sim` — 27 passed, 0 failed.
- `cargo test -p kobo-cli recording` — 2 passed, 0 failed.
- `cargo test -p kobo-cli` — 246 passed, 0 failed.
- `cargo run -p kobo-sdk --example preview` — wrote `preview-launcher.png` and `preview-reading.png` successfully.
- `git diff --check` — passed.

## Simulator Smoke

- Started the actual built-in simulator at `127.0.0.1:8877` and opened it in Chromium.
- The browser surface reported `Frame loaded; layout clean.`, a 1072×1448 canvas, GC16, refresh count 1, and opaque rendered pixels.
- Direct `/frame` evidence returned `Content-Type: image/png`, a valid PNG signature, and PNG color type 0 for the default Gray8 profile.
- `cargo run -p kobo-cli -- shot --address 127.0.0.1:8877 --out target/task7-simulator-shot.png` completed and the resulting 1072×1448 counter screenshot decoded visibly.
- Synthetic RGB simulator tests separately prove RGB PNG color type 2, because all real simulator/device profiles intentionally remain Gray8 in this task.

## Self-review

- RGB upload, per-screen selection, picture triples, equal-channel chrome, Gray8-on-RGB behavior, unreferenced RGB shallowness, profile-based framebuffer packing, and failed-output retry are all covered by runtime tests.
- Exact Gray8/RGB8-to-RGBA bytes, ideal/visible RGB triples, independent per-channel residue, equal-frame suppression, distinct-color detection, GLRC16/GCC16 names, and PNG color types are covered by simulator tests.
- The selection traversal examines only handles present in the retained screen and never scans the whole cache.
- Runtime region extraction uses checked dimensions and byte offsets and preserves the source format without a full-frame conversion.
- The active profile capability reaches both pixel packing and refresh lowering. Missing capability still fails closed in the existing HAL typed errors.
- Planner state cannot advance before packing, framebuffer write, and refresh all complete.
- Existing grayscale runtime, simulator, screenshot, recording, and preview behavior remains covered and passed.
- No real profile capability, BOMTOON file, todo tracker, formatter, linter, or workspace-wide suite was touched or run.

## Concerns

None.
