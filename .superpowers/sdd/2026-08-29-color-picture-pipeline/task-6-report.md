# Task 6 Report: Chromatic Refresh Planning

## Status

Complete.

## Implementation

- Added typed `ColorChange` classification and `PANEL_COLOR_CLEAN_INTERVAL = 4`.
- Extended `PanelWaveform` with `Glrc16` and `Gcc16`.
- Reworked `FramePlanner` to retain typed previous pixels plus independent grayscale and chromatic changed-pixel budgets.
- Gray8 and RGB8 frames are compared directly in logical pixel units. Gray8 is treated as three equal channels during cross-format comparisons; no conversion buffer is allocated.
- A changed pixel is chromatic when either its previous or current logical RGB triple has unequal channels.
- Planning remains side-effect free. Previous pixels, chromatic state, refresh number, and both cleaning budgets advance only in `commit`, after the caller reports a successful refresh.
- The first chromatic frame and every transition into or out of chromatic content use a full `Gcc16` refresh.
- Chromatic changed pixels use `Glrc16` for the first three panel-area equivalents and `Gcc16` on the fourth; failed fourth updates retry the same clean transition.
- Equal-channel RGB8 content retains the existing DU/GL16/GC16 grayscale policy and eight-panel grayscale cleaning cadence.
- Successful Gray8/RGB8 format changes replace the typed previous buffer only during commit; wrong dimensions fail closed.
- Added `ColorContent` and `ColorQuality` refresh intents plus typed `RefreshError` refusal.
- HAL lowering accepts `Option<ColorPanel>`, carries the profile's exact regal/clean waveform constants and CFA flags for HWTCON, forces partial/full mode respectively, and rejects missing capability or MXCFB color lowering.
- Existing grayscale intents retain backend-owned constants, requested update modes, and zero flags even when a color capability is present.
- Updated only unavoidable exhaustive/call-site handling in `kobo-hal::display`, the HAL re-export, `kobod`, and `kobo-sim`. Runtime RGB output remains disabled and profile capability enablement was not changed.

## RED Evidence

- `cargo test -p kobo-ui color_frame_planner` failed to compile because `Gcc16` and `Glrc16` did not exist.
- `cargo test -p kobo-hal refresh` failed to compile because color intents, capability-aware lowering, and `RefreshError` did not exist.

## Verification

- `cargo test -p kobo-ui frame_planner` — 10 passed, 0 failed.
- `cargo test -p kobo-hal refresh` — 7 passed, 0 failed.
- `cargo check -p kobod` — passed; existing dead-code warnings only.
- `cargo test -p kobo-hal --features device-write refresh` — 7 passed, 0 failed; device-write display lowering and smoke code compiled.
- `git diff --check` — passed.

## Self-review

- Exact first-three/fourth color cadence is covered with a one-pixel panel so each changed chromatic pixel equals one panel-area equivalent.
- Failure/retry coverage proves an uncommitted clean transition is reproduced exactly and does not consume cadence.
- Cross-format coverage proves equal logical pixels are ignored, format replacement occurs only on commit, and the color-exit rule survives RGB8-to-Gray8.
- Capability lowering tests use deliberately non-ABI waveform values (`10` and `11`) and CFA flags (`0x600`) so hard-coded guesses cannot pass.
- Grayscale behavior remains separately covered by the pre-existing waveform, changed-pixel budget, sparse bounding-box, and typing tests.
- Sparse changed endpoints now scan every current tone in their bounding refresh region before selecting DU, while only the two changed pixels charge the cleaning budget.
- A due grayscale clean on a still-chromatic mixed frame is promoted to full GCC16, resets both budgets only on commit, and retries identically after an uncommitted failure.
- Device-write smoke intent labeling is exhaustive for grayscale and color intents even though the attended smoke stage remains grayscale-only.

## Concerns

None.
