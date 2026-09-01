# BOMTOON Variable Episode Pagination Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show comic title, creators, and synopsis only on episode page 1; show only the comic title on later pages and fill the freed space with additional episode rows.

**Architecture:** Replace the single fixed episode-row capacity with explicit, measured episode ranges. Measure page 1 and later-page header shapes separately, then make rendering, navigation, thumbnail visibility, and refresh anchoring consume the same stored ranges.

**Tech Stack:** Rust 2021, `kobo-sdk` app callbacks, `kobo-ui::ScreenBuilder`, `CLARA_BW_METRICS`, Cargo tests.

## Global Constraints

- Page 1 shows comic title, creators, synopsis preview, and conditional `More`.
- Pages after page 1 show only the comic title above balances and episode rows.
- Candidate page capacities remain bounded by `EPISODE_ITEMS_PER_PAGE == 6`.
- Gift failure, purchase rejection, and unresolved cross-account warnings use dismissible episode modals and reserve no row space.
- Closing a Gift warning leaves the episode actionable; tapping it retries the Gift load before continuing the quote flow.
- Every episode appears exactly once; navigation, page count, thumbnails, and refresh anchoring share one range model.
- Bomtoon uses an opt-in edge pager whose band retains `DisplayMetrics::page_position_band()` height and ends at the panel bottom.
- Every page and modal shape must fit `CLARA_BW_METRICS`.
- No new dependency, protocol type, capability, origin, or Store metadata.
- Every shell command is prefixed with `rtk`.

---

## File Structure

- Modify `crates/kobo-ui/src/lib.rs`: add opt-in edge placement for a page-position band while retaining the existing touch-target height.
- Modify `apps/bomtoon/src/main.rs`: episode range state, measurement, rendering, warning modals, Gift retry continuation, navigation, thumbnail selection, refresh anchoring, and colocated behavioral tests.
- Modify `docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md`: record the approved warning-modal and edge-pager contract, then mark it verified after simulator proof.

---

### Task 1: Replace Fixed Capacity With Explicit Page Ranges

**Files:**
- Modify: `apps/bomtoon/src/main.rs:1976-2283`
- Modify tests: `apps/bomtoon/src/main.rs:10508-11091`

**Interfaces:**
- Consumes: `EPISODE_ITEMS_PER_PAGE`, `episode_screen_layout_fits`, `ScreenBuilder`, `page`, `episodes`, and existing transient commerce state.
- Produces: `episode_page_ranges(total, first_capacity, later_capacity) -> Vec<std::ops::Range<usize>>`, `Bomtoon::episode_page_range(page) -> std::ops::Range<usize>`, and stored `episode_pages: Vec<std::ops::Range<usize>>` used by every episode-page consumer.

- [ ] **Step 1: Write failing first-versus-later-page tests**

Add helpers and a regression test beside the existing episode metadata tests:

```rust
fn episode_rows(screen: &Screen) -> &[kobo_sdk::Row] {
    screen
        .nodes
        .iter()
        .find_map(|node| match node {
            Node::Rows { rows, .. } => Some(rows.as_slice()),
            _ => None,
        })
        .expect("episode rows")
}

#[test]
fn synopsis_and_creators_appear_only_on_page_one() {
    let synopsis = "A complete synopsis that belongs only on the first episode page.";
    let mut app = episode_metadata_app(synopsis.to_owned());
    app.episodes = (0..12)
        .map(|index| Episode {
            id: 900 + index,
            alias: format!("ep-{index}"),
            title: format!("Episode {index}"),
            opened_at: 1_709_136_000_000,
            thumbnail_url: None,
            purchase: model::PurchaseState::Owned,
            rent_expires_at: None,
            rent_coin: None,
            purchase_coin: None,
            gift_eligible: false,
        })
        .collect();
    app.prepare_episode_layout();

    app.page = 0;
    let first = app.episode_screen();
    let first_drawn = format!("{first:?}");
    assert!(first_drawn.contains("Hunter Q"));
    assert!(first_drawn.contains("Writer | Artist"));
    assert!(first_drawn.contains(synopsis));
    let first_rows = episode_rows(&first).len();
    assert_fits(&first);

    app.page = 1;
    let later = app.episode_screen();
    let later_drawn = format!("{later:?}");
    assert!(later_drawn.contains("Hunter Q"));
    assert!(!later_drawn.contains("Writer | Artist"));
    assert!(!later_drawn.contains(synopsis));
    assert!(!screen_button_actions(&later).contains(&action_id(EXPECTED_SYNOPSIS_MORE)));
    assert!(episode_rows(&later).len() > first_rows);
    assert_fits(&later);
}

#[test]
fn variable_episode_ranges_cover_every_episode_once() {
    let mut app = episode_metadata_app(long_synopsis());
    app.episodes = (0..19)
        .map(|index| Episode {
            id: 1_000 + index,
            alias: format!("ep-{index}"),
            title: format!("Episode {index}"),
            opened_at: 1_709_136_000_000,
            thumbnail_url: None,
            purchase: model::PurchaseState::Owned,
            rent_expires_at: None,
            rent_coin: None,
            purchase_coin: None,
            gift_eligible: false,
        })
        .collect();
    app.prepare_episode_layout();

    let observed = app
        .episode_pages
        .iter()
        .flat_map(|range| range.clone())
        .collect::<Vec<_>>();
    assert_eq!(observed, (0..app.episodes.len()).collect::<Vec<_>>());
    assert!(app.episode_pages.windows(2).all(|pages| pages[0].end == pages[1].start));
}
```


- [ ] **Step 2: Run the regression tests and confirm failure**

Run:

```bash
rtk cargo test -p kobo-bomtoon synopsis_and_creators_appear_only_on_page_one -- --exact
rtk cargo test -p kobo-bomtoon variable_episode_ranges_cover_every_episode_once -- --exact
```

Expected: compile failure because `episode_pages` does not exist, or assertion failure because later pages still render creators and synopsis and retain the first-page row capacity.

- [ ] **Step 3: Add a pure range constructor and range state**

Add this helper near `page_bounds`:

```rust
fn episode_page_ranges(
    total: usize,
    first_capacity: usize,
    later_capacity: usize,
) -> Vec<std::ops::Range<usize>> {
    let first_capacity = first_capacity.max(1);
    let later_capacity = later_capacity.max(1);
    let first_end = total.min(first_capacity);
    let mut ranges = vec![0..first_end];
    let mut start = first_end;
    while start < total {
        let end = start.saturating_add(later_capacity).min(total);
        ranges.push(start..end);
        start = end;
    }
    ranges
}
```

Replace `episode_items_per_page: Option<usize>` with:

```rust
episode_pages: Vec<std::ops::Range<usize>>,
```

Clear it wherever account or selected-comic state is cleared:

```rust
self.episode_pages.clear();
```

Add range accessors that fail closed for an absent or stale page:

```rust
fn episode_page_range(&self, page: usize) -> std::ops::Range<usize> {
    self.episode_pages
        .get(page)
        .cloned()
        .unwrap_or(0..0)
}

fn episode_page_count(&self) -> usize {
    self.episode_pages.len().max(1)
}
```

- [ ] **Step 4: Render page-specific headers and explicit ranges**

Keep `add_episode_header_preview` for page 1. Replace the unconditional header builder with:

```rust
fn add_episode_header(&self, screen: ScreenBuilder, page: usize) -> ScreenBuilder {
    if page == 0 {
        self.add_episode_header_preview(
            screen,
            self.synopsis.preview.clone(),
            self.synopsis.preview_truncated,
        )
    } else {
        screen.heading(self.episode_title_preview.clone())
    }
}
```

Change `add_episode_body` to accept `range: std::ops::Range<usize>`, `page`, and `page_count`. Iterate `range` directly and set page position from the explicit page count:

```rust
screen = screen.rows_with_trailing(range.map(|index| {
    let episode = &self.episodes[index];
    let title = display_text(&episode.title, &format!("Episode {}", episode.alias));
    let action = if episode.purchase.is_readable()
        || (episode.purchase == model::PurchaseState::NotOwned
            && !marker_belongs_to_another_account)
    {
        format!("episode-{index}")
    } else {
        format!("episode-disabled-{index}")
    };
    (
        action,
        title,
        taipei_date(episode.opened_at).unwrap_or_else(|| "Unknown date".to_owned()),
        cover_lead(&self.covers, episode.thumbnail_url.as_deref()),
        episode_status(episode, now_ms),
    )
}));

screen.page_turns(PREVIOUS_PAGE, NEXT_PAGE).page_position(
    u16::try_from(page.saturating_add(1)).unwrap_or(u16::MAX),
    u16::try_from(page_count).unwrap_or(u16::MAX),
)
```

Change `episode_screen_for` to accept a page-range slice during measurement and rendering:

```rust
fn episode_screen_for(
    &self,
    page: usize,
    ranges: &[std::ops::Range<usize>],
    modal: bool,
) -> Screen {
    let page = page.min(ranges.len().saturating_sub(1));
    let header = self.add_episode_header(
        ScreenBuilder::new("bomtoon-episodes").top_bar("Episodes"),
        page,
    );
    let range = ranges.get(page).cloned().unwrap_or(0..0);
    let mut screen = self.add_episode_body(header, range, page, ranges.len().max(1), false);
    if modal {
        if let Some((synopsis_page, range)) =
            self.synopsis.open_page.and_then(|synopsis_page| {
                self.synopsis
                    .pages
                    .get(synopsis_page)
                    .map(|range| (synopsis_page, range))
            })
        {
            let synopsis = &self.selected_synopsis[range.clone()];
            screen = screen.modal("Synopsis", |modal| {
                let mut modal = modal.text(synopsis);
                if synopsis_page > 0 {
                    modal = modal.button(SYNOPSIS_PREVIOUS, "Previous");
                }
                if synopsis_page.saturating_add(1) < self.synopsis.pages.len() {
                    modal = modal.button(SYNOPSIS_NEXT, "Next");
                }
                modal.button(SYNOPSIS_CLOSE, "Close")
            });
        }
    }
    screen.build()
}
```

`episode_screen` passes `&self.episode_pages`. `episode_preview_fits` measures only page 1 because the synopsis never renders on later pages.

- [ ] **Step 5: Measure first and later page capacities separately**

Replace the fixed-capacity search with a range search. Each candidate must render both actual and reserved transient states:

```rust
fn episode_ranges_fit(&self, ranges: &[std::ops::Range<usize>]) -> bool {
    ranges.iter().enumerate().all(|(page, range)| {
        let actual = self.episode_screen_for(page, ranges, false);
        if !episode_screen_layout_fits(&actual) {
            return false;
        }
        let header = self.add_episode_header(
            ScreenBuilder::new("bomtoon-episode-capacity-measure").top_bar("Episodes"),
            page,
        );
        let reserved = self
            .add_episode_body(header, range.clone(), page, ranges.len().max(1), true)
            .build();
        episode_screen_layout_fits(&reserved)
    })
}

fn measure_episode_pages(&self) -> Option<Vec<std::ops::Range<usize>>> {
    (1..=EPISODE_ITEMS_PER_PAGE).rev().find_map(|first_capacity| {
        (1..=EPISODE_ITEMS_PER_PAGE).rev().find_map(|later_capacity| {
            let ranges = episode_page_ranges(
                self.episodes.len(),
                first_capacity,
                later_capacity,
            );
            self.episode_ranges_fit(&ranges).then_some(ranges)
        })
    })
}
```

`prepare_episode_layout` keeps its existing title/creator/synopsis preview fallback loop, but stores the successful ranges:

```rust
if let Some(ranges) = self.measure_episode_pages() {
    self.episode_pages = ranges;
    return;
}
```

The minimal fallback must call `measure_episode_pages().expect(...)` after clearing the synopsis preview, then store the returned ranges.

- [ ] **Step 6: Migrate every range consumer**

Use `episode_page_range(self.page)` for visible episode thumbnail URLs and row rendering.

Restore an episode anchor by finding the explicit range containing its refreshed index:

```rust
self.page = anchor
    .and_then(|id| self.episodes.iter().position(|episode| episode.id == id))
    .and_then(|index| self.episode_pages.iter().position(|range| range.contains(&index)))
    .unwrap_or_else(|| fallback_page.min(self.episode_page_count().saturating_sub(1)));
```

Advance episode pages only when another explicit range exists:

```rust
if self.view == View::Episodes {
    if self.page.saturating_add(1) < self.episode_page_count() {
        self.page = self.page.saturating_add(1);
    }
} else {
    let next_start = self
        .page
        .saturating_add(1)
        .saturating_mul(LIBRARY_ITEMS_PER_PAGE);
    if next_start < self.destination_len() {
        self.page = self.page.saturating_add(1);
    } else if let Some(next) = self.destination_next_page() {
        let pending = match self.destination {
            MainDestination::Recent => Pending::Recent(next),
            MainDestination::Library => Pending::Library(next),
            MainDestination::Featured => {
                self.show(context);
                return;
            }
        };
        self.request_foreground(context, pending);
    }
}
```

Remove `episode_rows_per_page`, all division-based episode page calculations, and all `episode_items_per_page` assertions. Update existing tests to assert explicit range stability and coverage instead of one global capacity.

- [ ] **Step 7: Run focused tests and fix only contract failures**

Run:

```bash
rtk cargo test -p kobo-bomtoon synopsis_and_creators_appear_only_on_page_one
rtk cargo test -p kobo-bomtoon variable_episode_ranges_cover_every_episode_once
rtk cargo test -p kobo-bomtoon episode_capacity
rtk cargo test -p kobo-bomtoon transient_episode_controls_preserve_later_page_boundaries
rtk cargo test -p kobo-bomtoon rich_episode_capacity_fits_every_page_without_skips_or_state_drift
rtk cargo test -p kobo-bomtoon refreshed_episode_data_preserves_the_visible_episode_anchor
```

Expected: all selected tests pass; later-page tests observe no synopsis or creator text, every range fits, and all episode indices are covered once.

- [ ] **Step 8: Run the Bomtoon quality gates**

Run:

```bash
rtk cargo fmt --all -- --check
rtk cargo test -p kobo-bomtoon
rtk cargo clippy -p kobo-bomtoon --all-targets --all-features -- -D warnings
rtk git diff --check
```

Expected: formatting passes, all Bomtoon tests pass, Clippy emits no warnings, and the diff has no whitespace errors.

- [ ] **Step 9: Commit the behavior change**

```bash
rtk git add apps/bomtoon/src/main.rs
rtk git commit -m "fix(bomtoon): vary episode page capacity"
```

---

### Task 2: Remove Warning Reservations and Lower the Pager

**Files:**
- Modify: `crates/kobo-ui/src/lib.rs:1435-1486,1671-1814`
- Modify tests: `crates/kobo-ui/src/lib.rs:19220-19330`
- Modify: `apps/bomtoon/src/main.rs:34-75,1080-1160,1375-1435,2192-2295,2544-2650,5297-5342,6556-6625`
- Modify tests: `apps/bomtoon/src/main.rs:10800-11070,18446-18500,18750-18810`
- Modify: `docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md`

**Interfaces:**
- Consumes: Task 1's stored `episode_pages`, existing `PageTurns`, Gift task flow, purchase-rejection notice, and `Commerce::marker_belongs_to_another_account()`.
- Produces: `PageTurns::with_edge_position()`, app-local warning dismissal state, a deferred Gift-to-quote continuation, warning modals, and episode screens measured without transient warning text.

- [ ] **Step 1: Write failing edge-pager geometry tests**

Add a `kobo-ui` regression beside the existing page-position tests:

```rust
#[test]
fn edge_page_position_uses_the_panel_bottom_without_shrinking_targets() {
    let screen = |edge| {
        let mut screen =
            Screen::new(1, Vec::new()).with_page_turns(ActionId(7), ActionId(9));
        screen.page_turns = screen.page_turns.map(|turns| {
            let turns = turns.with_position(2, 3);
            if edge {
                turns.with_edge_position()
            } else {
                turns
            }
        });
        screen.layout_with(&CLARA_BW_METRICS, &Chrome::default())
    };

    let inset = screen(false);
    let edge = screen(true);
    let inset_position = inset
        .nodes
        .iter()
        .find(|node| node.kind == LayoutKind::PagePosition)
        .expect("inset page position");
    let edge_position = edge
        .nodes
        .iter()
        .find(|node| node.kind == LayoutKind::PagePosition)
        .expect("edge page position");

    assert_eq!(
        inset_position.rect.y + inset_position.rect.height,
        CLARA_BW_METRICS.height - CLARA_BW_METRICS.screen_margin()
    );
    assert_eq!(
        edge_position.rect.y + edge_position.rect.height,
        CLARA_BW_METRICS.height
    );
    assert_eq!(
        edge_position.rect.height,
        CLARA_BW_METRICS.page_position_band()
    );
    assert_eq!(
        edge.content.height - inset.content.height,
        CLARA_BW_METRICS.screen_margin()
    );
    assert!(edge
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                LayoutKind::PagePrevious(_) | LayoutKind::PageNext(_)
            )
        })
        .all(|node| node.rect.height >= CLARA_BW_METRICS.touch_target_minimum()));
}
```

- [ ] **Step 2: Write failing warning-modal and gap regressions**

Replace the inline Gift retry test and transient-space assertions with observable modal tests:

```rust
#[test]
fn gift_failure_uses_a_modal_without_changing_episode_ranges() {
    let (mut runner, _) = loaded_library();
    complete_initial_summary(&mut runner);
    let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
    let commands = runner.task_outcome(content, TaskOutcome::Completed(content_response()));
    let (gift, _) = only_spawn(&commands);
    let ranges = runner.app().episode_pages.clone();

    let commands = runner.task_outcome(gift, TaskOutcome::Failed(TaskError::TimedOut));
    let screen = last_screen(&commands);
    let overlay = screen.overlay.as_ref().expect("Gift warning modal");
    assert!(overlay_text(overlay).contains("Gift"));
    assert!(screen_button_actions(&screen).contains(&action_id(RETRY_GIFTS)));
    assert!(screen_button_actions(&screen).contains(&action_id(EPISODE_WARNING_CLOSE)));
    assert_eq!(runner.app().episode_pages, ranges);
    assert_fits(&screen);

    runner.action(action_id(EPISODE_WARNING_CLOSE));
    assert!(runner.app().screen().overlay.is_none());
    let commands = runner.action(action_id("episode-0"));
    let (_, work) = only_spawn(&commands);
    assert_eq!(work, api::title_gifts(41));
}

#[test]
fn purchase_and_cross_account_warnings_are_modal_and_do_not_reserve_rows() {
    let mut app = episode_metadata_app("Short synopsis.".to_owned());
    app.episodes = ordinary_episodes(18);
    app.prepare_episode_layout();
    let ranges = app.episode_pages.clone();
    let baseline_rows = episode_row_count(&app.episode_screen());

    app.purchase_rejection_notice = Some("FAIL");
    let purchase = app.episode_screen();
    assert!(purchase.overlay.is_some());
    assert_eq!(episode_row_count(&purchase), baseline_rows);
    assert_eq!(app.episode_pages, ranges);

    app.purchase_rejection_notice = None;
    install_foreign_marker(&mut app);
    let marker = app.episode_screen();
    assert!(marker.overlay.is_some());
    assert_eq!(episode_row_count(&marker), baseline_rows);
    assert_eq!(app.episode_pages, ranges);
}
```

Use existing episode fixtures and marker helpers where their names differ; do not duplicate fixture construction.

- [ ] **Step 3: Run the new tests and confirm failure**

Run:

```bash
rtk cargo test -p kobo-ui edge_page_position_uses_the_panel_bottom_without_shrinking_targets
rtk cargo test -p kobo-bomtoon gift_failure_uses_a_modal_without_changing_episode_ranges
rtk cargo test -p kobo-bomtoon purchase_and_cross_account_warnings_are_modal_and_do_not_reserve_rows
```

Expected: failure because edge placement, warning modal state, and close action do not exist and warnings still occupy the page body.

- [ ] **Step 4: Add opt-in edge placement to `PageTurns`**

Add one private placement flag and a public builder:

```rust
pub struct PageTurns {
    pub previous: ActionId,
    pub next: ActionId,
    pub menu: Option<ActionId>,
    pub position: Option<(u16, u16)>,
    edge_position: bool,
}

pub const fn new(previous: ActionId, next: ActionId) -> Self {
    Self {
        previous,
        next,
        menu: None,
        position: None,
        edge_position: false,
    }
}

#[must_use]
pub const fn with_edge_position(mut self) -> Self {
    self.edge_position = true;
    self
}
```

In non-reading layout, use the panel edge as the base only when a drawable edge position exists and no nav/bottom bar owns the edge:

```rust
let edge_position = self.nav_bar.is_none()
    && self.bottom_action.is_none()
    && self.overlay.is_none()
    && self.page_turns.is_some_and(|turns| {
        turns.edge_position && turns.drawable_position().is_some()
    });
let content_bottom = if self.nav_bar.is_some() || self.bottom_action.is_some() {
    metrics.height - metrics.nav_bar_height()
} else if edge_position {
    metrics.height
} else {
    metrics.height - metrics.screen_margin()
};
```

Keep `position_band == metrics.page_position_band()`. Existing inset behavior and reading-surface behavior remain unchanged.

- [ ] **Step 5: Add explicit episode-warning presentation state**

Add:

```rust
const EPISODE_WARNING_CLOSE: &str = "episode-warning-close";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EpisodeWarning {
    CrossAccount,
    PurchaseRejected(&'static str),
    GiftFailure,
}
```

Store dismissal and continuation state on `Bomtoon`:

```rust
gift_warning_dismissed: bool,
cross_account_warning_dismissed: bool,
pending_gift_quote: Option<usize>,
```

Reset these fields when account data, selected comic state, or Gift state is cleared. Set `gift_warning_dismissed = false` whenever a display or quote-continuation Gift load fails. Reset `cross_account_warning_dismissed` when a new selected comic is opened or when commerce transitions from no foreign marker to a foreign marker.

Derive the visible warning in strict priority:

```rust
fn episode_warning(&self) -> Option<EpisodeWarning> {
    if self.commerce.marker_belongs_to_another_account()
        && !self.cross_account_warning_dismissed
    {
        Some(EpisodeWarning::CrossAccount)
    } else if let Some(result) = self.purchase_rejection_notice {
        Some(EpisodeWarning::PurchaseRejected(result))
    } else if self.gifts.error && !self.gift_warning_dismissed {
        Some(EpisodeWarning::GiftFailure)
    } else {
        None
    }
}
```

- [ ] **Step 6: Render warnings as modals and remove reservation**

`add_episode_body` always renders the balance as text. Remove `reserve_transient_space`, the inline `Retry Gift` button, purchase rejection text, and cross-account warning text. Keep the cross-account marker check only for disabling unowned row actions.

After the synopsis modal branch, add the warning overlay when no synopsis modal is open:

```rust
if modal && self.synopsis.open_page.is_none() {
    if let Some(warning) = self.episode_warning() {
        screen = match warning {
            EpisodeWarning::CrossAccount => screen.modal("Account warning", |modal| {
                modal
                    .text("A purchase is unresolved for another account. Restore the original account to refresh its status.")
                    .button(EPISODE_WARNING_CLOSE, "Close")
            }),
            EpisodeWarning::PurchaseRejected(result) => {
                screen.modal("Purchase rejected", |modal| {
                    modal
                        .text(result)
                        .button(EPISODE_WARNING_CLOSE, "Close")
                })
            }
            EpisodeWarning::GiftFailure => screen.modal("Gift unavailable", |modal| {
                modal
                    .text("BOMTOON Gift status could not be loaded.")
                    .button(RETRY_GIFTS, "Retry")
                    .button(EPISODE_WARNING_CLOSE, "Close")
            }),
        };
    }
}
```

`episode_ranges_fit` measures only the unchanged underlying episode page. It no longer constructs a worst-case reserved-warning body.

Before returning the built episode screen, opt its page turns into edge placement:

```rust
let mut screen = screen.build();
screen.page_turns = screen
    .page_turns
    .map(kobo_sdk::PageTurns::with_edge_position);
screen
```

Use the actual re-export path available from `kobo_sdk`; if the type is inferred, `.map(|turns| turns.with_edge_position())` avoids a new import.

- [ ] **Step 7: Handle modal dismissal and Gift continuation**

Handle warning actions before ordinary episode actions:

```rust
if self.view == View::Episodes && action == action_id(EPISODE_WARNING_CLOSE) {
    match self.episode_warning() {
        Some(EpisodeWarning::CrossAccount) => {
            self.cross_account_warning_dismissed = true;
        }
        Some(EpisodeWarning::PurchaseRejected(_)) => {
            self.purchase_rejection_notice = None;
        }
        Some(EpisodeWarning::GiftFailure) => {
            self.gift_warning_dismissed = true;
        }
        None => {}
    }
    self.show(context);
    return;
}
```

Retry clears dismissal and starts the existing display Gift request. When an unowned episode is tapped while `gifts.error` is true, store its index in `pending_gift_quote` and start a Gift request instead of quoting immediately. Add a `GiftTaskPurpose::Quote { episode: usize }` variant. On success, clear `pending_gift_quote` and call `open_episode(context, episode)`; on failure, retain the episode index, reopen the Gift warning, and do not start a quote. Validate the index and selected title/account generation through the existing Gift task identity checks before continuing.

- [ ] **Step 8: Run focused UI and commerce regressions**

Run:

```bash
rtk cargo test -p kobo-ui edge_page_position_uses_the_panel_bottom_without_shrinking_targets
rtk cargo test -p kobo-bomtoon gift_failure
rtk cargo test -p kobo-bomtoon purchase_and_cross_account_warnings_are_modal_and_do_not_reserve_rows
rtk cargo test -p kobo-bomtoon transient_episode_controls_preserve_later_page_boundaries
rtk cargo test -p kobo-bomtoon quote_requote_and_marker_acknowledgement_order_the_purchase_post
rtk cargo test -p kobo-bomtoon rich_episode_capacity_fits_every_page_without_skips_or_state_drift
```

Expected: all selected tests pass; warnings are overlays, tapping after dismissed Gift failure reloads Gift state before quote, ranges do not change, edge controls meet minimum target height, and existing commerce ordering remains intact.

- [ ] **Step 9: Run affected quality gates**

Run:

```bash
rtk cargo fmt --all -- --check
rtk cargo test -p kobo-ui
rtk cargo test -p kobo-bomtoon
rtk cargo clippy -p kobo-ui --all-targets --all-features -- -D warnings
rtk cargo clippy -p kobo-bomtoon --all-targets --all-features -- -D warnings
rtk git diff --check
```

Expected: all tests pass, Clippy emits no warnings, formatting passes, and the diff has no whitespace errors.

- [ ] **Step 10: Commit the correction**

```bash
rtk git add crates/kobo-ui/src/lib.rs apps/bomtoon/src/main.rs docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md docs/superpowers/plans/2026-09-01-bomtoon-variable-episode-pagination.md
rtk git commit -m "fix(bomtoon): reclaim episode page space"
```

---

### Task 3: Verify the Authenticated Simulator Surface

**Files:**
- Modify: `docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md:3-5`

**Interfaces:**
- Consumes: authenticated simulator credential installed by `kobo bomtoon login --sim`, the explicit page ranges from Task 1, and warning-modal/edge-pager behavior from Task 2.
- Produces: browser evidence that metadata differs by page, warnings consume no row space, the pager touches the panel bottom, every episode page remains stable, and diagnostics remain clean.

- [ ] **Step 1: Launch the browser simulator**

Run from `apps/bomtoon`:

```bash
rtk cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

Expected: `Kobo app simulator: http://127.0.0.1:8787`.

- [ ] **Step 2: Exercise the authenticated episode pages**

Open `http://127.0.0.1:8787/`, select a comic with enough episodes for at least two pages, and record these observations without making a purchase:

- Page 1 shows comic title, creators, synopsis preview, and conditional `More`.
- Page 2 shows the comic title but no creator text, synopsis text, or `More`.
- Page 2 contains more episode rows than page 1 for the selected normal-length comic.
- The page-position band ends at the panel bottom; Previous and Next retain full-height tap targets.
- No episode page leaves the former warning-reservation gap below its last row.
- A safely induced Gift lookup failure opens a modal with `Retry` and `Close`, if the authenticated flow exposes a non-spending failure path; otherwise the focused regression test is the proof for this failure-only state.
- Closing the Gift warning leaves the underlying page unchanged, and tapping the unowned episode retries Gift loading before any quote request.
- Previous returns to the unchanged page-1 range.
- Next returns to the unchanged page-2 range.
- `/diagnostics` returns `{"issues":[]}`.

- [ ] **Step 3: Mark the approved follow-up complete**

Replace the pending status sentence with observed proof:

```markdown
Implementation complete. Page 1 retains title, creators, synopsis preview, and conditional `More`; later pages retain only the title and use separately measured episode ranges. Transient warnings render as modals without reserving row space, and the opt-in episode pager reaches the panel bottom while retaining full-height targets. Focused format, test, Clippy, browser simulator, and layout-diagnostics gates pass. The authenticated browser flow was non-spending.
```

Keep the existing runtime-simulator boundary text after this sentence.

- [ ] **Step 4: Verify and commit the status update**

Run:

```bash
rtk git diff --check
rtk git add docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md
rtk git commit -m "docs(bomtoon): record pagination proof"
```

Expected: no whitespace errors and one documentation commit.
