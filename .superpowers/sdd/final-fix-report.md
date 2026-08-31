# Feature Collections Final Fix Report

## Release migration

- Raised the workspace/runtime release from `0.4.0` to `0.5.0`.
- Raised all explicit internal `kobo-*` path dependency requirements from `0.4.0` to `0.5.0`.
- Regenerated `Cargo.lock`; its release diff changes only workspace package versions from `0.4.0` to `0.5.0` and does not upgrade registry dependencies.
- Raised the Bomtoon Store catalog release from `0.5.0` to `0.6.0` and `minimum_cobalt_version` from `0.4.0` to `0.5.0`.
- Kept protocol v16 closed; no backward compatibility shim or fake picture handle was added.

## Finding fixes and RED/GREEN evidence

1. **Runtime/catalog compatibility**
   - RED: `bomtoon_feature_release_requires_the_protocol_v16_runtime` failed with runtime `0.4.0` instead of `0.5.0`.
   - GREEN: `rtk cargo test --locked -p kobo-cli bomtoon_feature_release_requires_the_protocol_v16_runtime` — 1 passed.

2. **Retry banner convergence**
   - RED: `recovered_source_replaces_a_preserved_banner_placeholder_with_safe_artwork` retained the placeholder with no artwork after Ranking recovered.
   - GREEN: the collection map is resolved before preserved/detail banner state; the recovered safe comic replaces the placeholder. Covered by the 27-test Feature state run.

3. **Unknown collection totals**
   - RED: `unknown_total_draws_the_current_page_and_both_discovered_turn_directions` and the Bomtoon collection regression found no page-position node for `(page, 0)`.
   - GREEN: zero total now reserves the band, draws the current page alone, and exposes Previous/Next from the discovered page cursor. Bomtoon withholds the pager until a current boundary exists. Covered by focused UI and collection tests.

4. **Collection cover fallback geometry**
   - RED: the UI regression did not compile because no explicit cover-slot lead existed.
   - GREEN: `RowLead::CoverSlot(Glyph)` preserves the exact picture column and row geometry while drawing the book glyph. UI layout, protocol closed codec, SDK builder, and Bomtoon absent/loading/failed/ready cases are covered. No sentinel handle is used.

5. **Late first LocalDay**
   - RED: `late_first_day_labels_an_undated_committed_snapshot_without_refreshing` observed a new full refresh.
   - GREEN: an undated committed snapshot receives the first observed day without generation change, batch creation, or duplicate source tasks; a later changed day still refreshes. Covered by pure state and runner race tests.

6. **Both public artwork URLs across sign-out**
   - RED: `sign_out_retains_both_public_artwork_urls_and_tasks_while_clearing_protected_state` omitted the distinct square URL.
   - GREEN: public retention collects both vertical and square URLs and preserves their decoded handles/tasks while protected state is cleared.

## Green verification

- `rtk cargo test -p kobo-ui unknown_total` — 1 passed.
- `rtk cargo test -p kobo-ui page_position` — 2 passed.
- `rtk cargo test -p kobo-ui cover_slot` — 1 passed.
- `rtk cargo test -p kobo-ui described_row` — 5 passed.
- `rtk cargo test -p kobo-protocol cover_slot` — 1 passed.
- `rtk cargo test -p kobo-sdk described_rows` — 2 passed.
- `rtk cargo test -p kobo-bomtoon feature::tests` — 27 passed.
- `rtk cargo test -p kobo-bomtoon collection_` — 35 passed.
- Exact Bomtoon unknown-total, cover-fallback, late-day runner, and sign-out regressions — 1 passed each.
- `rtk cargo fmt --all -- --check` — clean.
- `rtk cargo clippy -p kobo-ui -p kobo-protocol -p kobo-sdk -p kobo-bomtoon -p kobo-cli --all-targets -- -D warnings` — no issues.

## Files

- `Cargo.toml`
- `Cargo.lock`
- `apps/catalog.json`
- `apps/bomtoon/src/feature.rs`
- `apps/bomtoon/src/main.rs`
- `crates/kobo-cli/src/main.rs`
- `crates/kobo-policy/Cargo.toml`
- `crates/kobo-protocol/Cargo.toml`
- `crates/kobo-protocol/src/lib.rs`
- `crates/kobo-sdk/Cargo.toml`
- `crates/kobo-sdk/src/lib.rs`
- `crates/kobo-text/Cargo.toml`
- `crates/kobo-ui/src/lib.rs`
- `.superpowers/sdd/final-fix-report.md`

## Validation boundary / concerns

Project-wide tests and builds were intentionally skipped. The Store `app-check` command was stopped when it began building every Store application; it is not claimed as evidence. The targeted locked catalog/runtime assertion, affected tests, formatting check, and focused affected Clippy are green. No remaining task-scoped concern is known.
