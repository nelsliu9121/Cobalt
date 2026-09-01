mod api;
mod commerce;
mod feature;
mod model;
mod parse;
#[cfg(test)]
use feature::FEATURE_SOURCES;
use feature::{
    compact_count, feed_blocks, CollectionView, DetailState, FeatureSnapshot, FeatureSource,
    FeaturedState, FeedBlock, FeedPage, SourceResult,
};

use kobo_image::{Picture, PictureFormat, PicturePixels, PicturePixelsRef, PANEL_GREYS};
use kobo_sdk::{
    action_id, drawable_text_in, ActionId, BannerLevel, Chrome, Context, ControlState,
    DeviceRequest, DeviceResult, DiagnosticSeverity, DisplayMetrics, Face, Failure, Glyph, KoboApp,
    LayoutIssueKind, LocalDay, PictureFit, PictureHandle, ReadingChrome, RowLead, RowLineLimits,
    Screen, ScreenBuilder, StoreResult, TaskError, TaskId, TaskOutcome, TilePicture,
    CLARA_BW_METRICS,
};
#[cfg(test)]
use model::FeatureComic;
use model::{
    display_text, AssetKind, AssetSubtype, Comic, Comment, Episode, EpisodeImage, ExpirationRow,
    RecentEntry, WalletSummary,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const TITLE: &str = "BOMTOON";
const RETRY: &str = "retry";
const SIGN_IN: &str = "sign-in";
const SIGN_OUT: &str = "sign-out";
const PREVIOUS_PAGE: &str = "previous-page";
const NEXT_PAGE: &str = "next-page";
const FEATURED: &str = "featured";
const RECENT: &str = "recent";
const LIBRARY: &str = "library";
const ACCOUNT: &str = "account";
const RETRY_BALANCES: &str = "retry-balances";
const RETRY_GIFTS: &str = "retry-gifts";
const USE_GIFT: &str = "commerce-use-gift";
const RENT: &str = "commerce-rent";
const BUY: &str = "commerce-buy";
const CANCEL_COMMERCE: &str = "commerce-cancel";
const REFRESH_COMMERCE: &str = "commerce-refresh";
const LIBRARY_ITEMS_PER_PAGE: usize = 6;
const EPISODE_ITEMS_PER_PAGE: usize = 6;
const ACCOUNT_HISTORY_ITEMS_PER_PAGE: usize = 3;
const HISTORY_WINDOW_MS: i64 = 90 * 24 * 60 * 60 * 1_000;
const READER_PREVIOUS: &str = "reader-previous";
const READER_NEXT: &str = "reader-next";
const READER_CHROME: &str = "reader-chrome";
const ALL_COMMENTS: &str = "all-comments";
const COMMENTS_PREVIOUS: &str = "comments-previous";
const COMMENTS_NEXT: &str = "comments-next";
const REPLIES_PREVIOUS: &str = "replies-previous";
const REPLIES_NEXT: &str = "replies-next";
const APPENDIX_COMMENT_LIMIT: usize = 4;
const APPENDIX_COMMENT_PREVIEW_BYTES: usize = 96;
const COMMENT_LIST_PREVIEW_BYTES: usize = 320;
const COMMENT_DETAIL_BYTES: usize = 320;
const SYNOPSIS_MORE: &str = "synopsis-more";
const SYNOPSIS_PREVIOUS: &str = "synopsis-previous";
const SYNOPSIS_NEXT: &str = "synopsis-next";
const SYNOPSIS_CLOSE: &str = "synopsis-close";
const SYNOPSIS_PREVIEW_BYTES: usize = 240;
const EPISODE_TITLE_PREVIEW_BYTES: usize = 160;
const EPISODE_CREATORS_PREVIEW_BYTES: usize = 160;
// Mirrors kobo_protocol::MAX_STRING_LEN; kobo-sdk does not expose that ceiling.
const SYNOPSIS_MODAL_MAX_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SynopsisState {
    pages: Vec<std::ops::Range<usize>>,
    open_page: Option<usize>,
    preview: String,
    preview_truncated: bool,
}

const MAIN_DESTINATIONS: [(&str, &str); 3] = [
    (FEATURED, "Featured"),
    (RECENT, "Recent"),
    (LIBRARY, "Library"),
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum View {
    #[default]
    Status,
    Main,
    FeatureCollection,
    Account,
    Episodes,
    Reader,
    CommentAppendix,
    Comments,
    Replies,
}

impl View {
    const fn is_reader_flow(self) -> bool {
        matches!(
            self,
            Self::Reader | Self::CommentAppendix | Self::Comments | Self::Replies
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MainDestination {
    #[default]
    Featured,
    Recent,
    Library,
}

impl MainDestination {
    const fn index(self) -> usize {
        match self {
            Self::Featured => 0,
            Self::Recent => 1,
            Self::Library => 2,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Featured => "Featured",
            Self::Recent => "Recent",
            Self::Library => "Library",
        }
    }
}

fn unix_time_ms() -> Option<i64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(elapsed.as_millis()).ok()
}

fn history_start_ms_at(now_ms: i64) -> Option<i64> {
    now_ms.checked_sub(HISTORY_WINDOW_MS)
}

fn history_start_ms() -> Option<i64> {
    history_start_ms_at(unix_time_ms()?)
}

fn taipei_day(timestamp_ms: i64) -> Option<i64> {
    const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
    const TAIPEI_OFFSET_MS: i64 = 8 * 60 * 60 * 1_000;
    let utc_day = timestamp_ms.div_euclid(DAY_MS);
    let utc_day_ms = timestamp_ms.rem_euclid(DAY_MS);
    utc_day.checked_add(i64::from(utc_day_ms >= DAY_MS - TAIPEI_OFFSET_MS))
}

fn taipei_date(timestamp_ms: i64) -> Option<String> {
    let days = taipei_day(timestamp_ms)?;
    let shifted = days.checked_add(719_468)?;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn taipei_datetime(timestamp_ms: i64) -> Option<String> {
    const TAIPEI_OFFSET_MS: i64 = 8 * 60 * 60 * 1_000;
    const HOUR_MS: i64 = 60 * 60 * 1_000;
    const MINUTE_MS: i64 = 60 * 1_000;
    let local = timestamp_ms.checked_add(TAIPEI_OFFSET_MS)?;
    let time = local.rem_euclid(24 * HOUR_MS);
    let hour = time / HOUR_MS;
    let minute = time.rem_euclid(HOUR_MS) / MINUTE_MS;
    Some(format!(
        "{} {hour:02}:{minute:02}",
        taipei_date(timestamp_ms)?
    ))
}

fn comment_preview(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}...", text[..end].trim_end()), true)
}

fn normalized_header_preview(text: &str, max_bytes: usize, fallback: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let visible = if normalized.is_empty() {
        fallback
    } else {
        &normalized
    };
    comment_preview(visible, max_bytes).0
}
fn normalized_header_len(text: &str) -> usize {
    text.split_whitespace()
        .map(str::len)
        .enumerate()
        .map(|(index, len)| len + usize::from(index > 0))
        .sum()
}

fn synopsis_modal_fits(text: &str) -> bool {
    let screen = ScreenBuilder::new("bomtoon-synopsis-measure")
        .top_bar("Episodes")
        .modal("Synopsis", |modal| {
            modal
                .text(text)
                .button(SYNOPSIS_PREVIOUS, "Previous")
                .button(SYNOPSIS_NEXT, "Next")
                .button(SYNOPSIS_CLOSE, "Close")
        })
        .build();
    let diagnostics = screen.diagnostics(&CLARA_BW_METRICS, &Chrome::measuring(true));
    let all_overlay_nodes_fit = screen.overlay.as_ref().is_some_and(|overlay| {
        overlay.nodes.iter().all(|node| {
            diagnostics
                .layout
                .nodes
                .iter()
                .any(|laid_out| laid_out.id == node.id())
        })
    });
    !diagnostics.has_errors() && all_overlay_nodes_fit
}

fn synopsis_page_break(text: &str, start: usize, end: usize) -> Option<usize> {
    let candidate = &text[start..end];
    if let Some(paragraph) = candidate.rfind("\n\n") {
        let boundary = start.saturating_add(paragraph).saturating_add(2);
        if boundary > start {
            return Some(boundary);
        }
    }
    candidate
        .char_indices()
        .rev()
        .find_map(|(offset, character)| {
            character.is_whitespace().then(|| {
                start
                    .saturating_add(offset)
                    .saturating_add(character.len_utf8())
            })
        })
}

fn synopsis_pages(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut boundaries = text
        .char_indices()
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    if boundaries.last().copied() != Some(text.len()) {
        boundaries.push(text.len());
    }
    let mut pages = Vec::new();
    let mut start_index: usize = 0;
    while start_index.saturating_add(1) < boundaries.len() {
        let start = boundaries[start_index];
        let maximum_end = start
            .saturating_add(SYNOPSIS_MODAL_MAX_BYTES)
            .min(text.len());
        let last_candidate = boundaries
            .partition_point(|boundary| *boundary <= maximum_end)
            .saturating_sub(1);
        let mut low = start_index.saturating_add(1);
        let mut high = last_candidate.saturating_add(1);
        while low < high {
            let middle = low + (high - low) / 2;
            if synopsis_modal_fits(&text[start..boundaries[middle]]) {
                low = middle.saturating_add(1);
            } else {
                high = middle;
            }
        }
        let fitting_index = low
            .saturating_sub(1)
            .max(start_index.saturating_add(1))
            .min(boundaries.len().saturating_sub(1));
        let fitting_end = boundaries[fitting_index];
        let end = if fitting_end < text.len() {
            synopsis_page_break(text, start, fitting_end).unwrap_or(fitting_end)
        } else {
            fitting_end
        };
        let end_index = boundaries
            .binary_search(&end)
            .expect("synopsis break is a UTF-8 boundary");
        pages.push(start..end);
        start_index = end_index;
    }
    pages
}

fn comment_detail_page(text: &str, page: usize) -> Option<(&str, usize)> {
    let page_end = |start: usize| {
        let mut end = start.saturating_add(COMMENT_DETAIL_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        end
    };
    let mut total_pages = usize::from(text.is_empty());
    let mut cursor = 0;
    while cursor < text.len() {
        cursor = page_end(cursor);
        total_pages += 1;
    }
    if page >= total_pages {
        return None;
    }
    let mut start = 0;
    for _ in 0..page {
        start = page_end(start);
    }
    Some((&text[start..page_end(start)], total_pages))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pending {
    Library(usize),
    Recent(usize),
    Content(usize),
    Logout,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ShelfLoadState {
    pending_page: Option<usize>,
    error: Option<(usize, String)>,
    loaded: bool,
}

impl ShelfLoadState {
    fn begin(&mut self, page: usize) {
        self.pending_page = Some(page);
        self.error = None;
    }

    fn finish(&mut self) {
        self.pending_page = None;
        self.error = None;
        self.loaded = true;
    }

    fn fail(&mut self, page: usize, message: impl Into<String>) {
        self.pending_page = None;
        self.error = Some((page, message.into()));
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AccountState {
    #[default]
    Checking,
    Active,
    SignedOut,
    Expired,
    RevocationUnconfirmed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ConnectionState {
    #[default]
    Unknown,
    Online,
    Offline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarkerStoreOperation {
    Load,
    Save,
    Forget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FeatureTaskPurpose {
    Source {
        generation: u64,
        source: FeatureSource,
    },
    BannerDetail {
        generation: u64,
        alias: String,
    },
    CollectionDetail {
        generation: u64,
        collection_generation: u64,
        collection_id: String,
        alias: String,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum FeatureCapacityState {
    #[default]
    Ready,
    ResumeAfterCommerce,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CoverState {
    Loading(TaskId),
    Ready(TilePicture),
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoverSource {
    Public,
    Protected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CoverTask {
    generation: u64,
    url: String,
    source: CoverSource,
}

#[derive(Default)]
struct CoverCache {
    generation: u64,
    entries: BTreeMap<String, CoverState>,
    tasks: BTreeMap<TaskId, CoverTask>,
    visible_urls: Vec<String>,
    visible_source: Option<CoverSource>,
}

fn collection_action(id: &str) -> String {
    format!("feature-collection-{id}")
}

fn comic_action(collection: &str, index: usize) -> String {
    format!("feature-comic-{collection}-{index}")
}
fn ready_cover(covers: &CoverCache, url: Option<&str>) -> Option<TilePicture> {
    match url.and_then(|url| covers.entries.get(url)) {
        Some(CoverState::Ready(picture)) => Some(*picture),
        Some(CoverState::Loading(_) | CoverState::Failed) | None => None,
    }
}

fn cover_lead(covers: &CoverCache, url: Option<&str>) -> RowLead {
    ready_cover(covers, url).map_or(RowLead::Icon(Glyph::Book), |picture| {
        RowLead::Picture(picture, Glyph::Book)
    })
}

fn episode_status(episode: &Episode, now_ms: Option<i64>) -> String {
    match episode.purchase {
        model::PurchaseState::Owned => "Owned".to_owned(),
        model::PurchaseState::Rented => now_ms
            .and_then(|now| episode.remaining_rental_hours(now))
            .map_or_else(
                || "Rented".to_owned(),
                |hours| {
                    let unit = if hours == 1 { "hr" } else { "hrs" };
                    format!("{hours} {unit}")
                },
            ),
        model::PurchaseState::Free => "Free".to_owned(),
        model::PurchaseState::Sample => "Sample".to_owned(),
        model::PurchaseState::NotOwned => String::new(),
        model::PurchaseState::Other(_) => display_text(episode.purchase.label(), "Other status"),
    }
}

fn episode_screen_layout_fits(screen: &Screen) -> bool {
    (0..2).all(|_| {
        let diagnostics = screen.diagnostics(&CLARA_BW_METRICS, &Chrome::measuring(true));
        if diagnostics.issues.iter().any(|issue| {
            issue.severity == DiagnosticSeverity::Error
                && !matches!(issue.kind, LayoutIssueKind::UnsupportedCharacter { .. })
        }) {
            return false;
        }
        let content = diagnostics.layout.content;
        screen.nodes.iter().all(|node| match node {
            kobo_sdk::Node::Rows { rows, .. } => rows.iter().all(|row| {
                diagnostics.layout.nodes.iter().any(|layout_node| {
                    layout_node.kind.acts_on() == Some(row.action)
                        && layout_node.rect.y >= content.y
                        && layout_node.rect.y.saturating_add(layout_node.rect.height)
                            <= content.y.saturating_add(content.height)
                })
            }),
            _ => true,
        })
    })
}

fn collection_cover_lead(covers: &CoverCache, url: Option<&str>) -> RowLead {
    ready_cover(covers, url).map_or(RowLead::CoverSlot(Glyph::Book), |picture| {
        RowLead::Picture(picture.with_fit(PictureFit::Cover), Glyph::Book)
    })
}

fn synopsis_for(cache: &BTreeMap<String, DetailState>, alias: &str) -> String {
    match cache.get(alias) {
        Some(DetailState::Ready(detail)) => detail.synopsis.clone().unwrap_or_default(),
        Some(DetailState::Loading(_) | DetailState::Failed) | None => String::new(),
    }
}

fn display_content_title(title: &str, alias: &str) -> String {
    let drawable = drawable_text_in(title, Face::Text);
    if !title.is_empty() && drawable == title {
        drawable
    } else {
        format!("BOMTOON {alias}")
    }
}

fn add_feed_blocks(
    mut screen: ScreenBuilder,
    snapshot: &FeatureSnapshot,
    covers: &CoverCache,
    blocks: &[FeedBlock],
) -> ScreenBuilder {
    for block in blocks {
        match *block {
            FeedBlock::Banners => {
                screen = screen.image_strip(snapshot.banners.iter().take(3).enumerate().map(
                    |(index, comic)| {
                        (
                            format!("feature-banner-{index}"),
                            Glyph::Book,
                            ready_cover(
                                covers,
                                comic
                                    .vertical_url
                                    .as_ref()
                                    .or(comic.square_url.as_ref())
                                    .map(String::as_str),
                            ),
                        )
                    },
                ));
            }
            FeedBlock::Collection(index) | FeedBlock::ThemeWithHeading(index) => {
                let collection = &snapshot.collections[index];
                if matches!(block, FeedBlock::ThemeWithHeading(_)) {
                    screen = screen.section("編輯精選");
                }
                screen = screen
                    .tappable_section(collection_action(&collection.id), collection.label.clone())
                    .media_grid(collection.comics.iter().take(6).enumerate().map(
                        |(index, comic)| {
                            (
                                comic_action(&collection.id, index),
                                display_content_title(&comic.title, &comic.alias),
                                display_text(&comic.creators, ""),
                                Glyph::Book,
                                ready_cover(covers, comic.vertical_url.as_deref()),
                            )
                        },
                    ));
            }
        }
    }
    screen
}

fn add_feed_warning(screen: ScreenBuilder, warning: Option<&str>) -> ScreenBuilder {
    match warning {
        Some(warning) => screen
            .banner(BannerLevel::Attention, warning)
            .primary_button(RETRY, "Try again"),
        None => screen,
    }
}

fn measured_feed_screen(
    page: &FeedPage,
    snapshot: &FeatureSnapshot,
    warning: Option<&str>,
) -> Screen {
    let screen = ScreenBuilder::new("bomtoon-featured-measure")
        .top_bar("Featured")
        .top_bar_action(ACCOUNT, "Coins 18446744073709551615");
    let screen = add_feed_warning(screen, warning);
    add_feed_blocks(screen, snapshot, &CoverCache::default(), &page.blocks)
        .page_turns(PREVIOUS_PAGE, NEXT_PAGE)
        .page_position(u16::MAX, u16::MAX)
        .nav_bar(MainDestination::Featured.index(), MAIN_DESTINATIONS)
        .build()
}

fn page_fits_with_warning(
    page: &FeedPage,
    snapshot: &FeatureSnapshot,
    warning: Option<&str>,
    metrics: &DisplayMetrics,
) -> bool {
    !measured_feed_screen(page, snapshot, warning)
        .diagnostics(metrics, &Chrome::measuring(true))
        .has_errors()
}

#[cfg(test)]
fn page_fits(page: &FeedPage, snapshot: &FeatureSnapshot, metrics: &DisplayMetrics) -> bool {
    page_fits_with_warning(page, snapshot, snapshot.warning.as_deref(), metrics)
}

fn feed_pages_with_warning(
    snapshot: &FeatureSnapshot,
    warning: Option<&str>,
    metrics: &DisplayMetrics,
) -> Vec<FeedPage> {
    let blocks = feed_blocks(snapshot);
    if blocks.is_empty() {
        return vec![FeedPage { blocks }];
    }
    let mut pages = Vec::new();
    let mut current = Vec::new();
    for block in blocks {
        let mut candidate = current.clone();
        candidate.push(block);
        let candidate = FeedPage { blocks: candidate };
        if page_fits_with_warning(&candidate, snapshot, warning, metrics) {
            current = candidate.blocks;
            continue;
        }
        assert!(
            !current.is_empty(),
            "Feature feed block {block:?} does not fit an empty page"
        );
        pages.push(FeedPage { blocks: current });
        let fresh = FeedPage {
            blocks: vec![block],
        };
        assert!(
            page_fits_with_warning(&fresh, snapshot, warning, metrics),
            "Feature feed block {block:?} does not fit an empty page"
        );
        current = fresh.blocks;
    }
    pages.push(FeedPage { blocks: current });
    pages
}

fn feed_pages(snapshot: &FeatureSnapshot, metrics: &DisplayMetrics) -> Vec<FeedPage> {
    feed_pages_with_warning(snapshot, snapshot.warning.as_deref(), metrics)
}

fn featured_feed_pages(featured: &FeaturedState, metrics: &DisplayMetrics) -> Vec<FeedPage> {
    featured.snapshot().map_or_else(Vec::new, |snapshot| {
        if featured.warning() == snapshot.warning.as_deref() {
            feed_pages(snapshot, metrics)
        } else {
            feed_pages_with_warning(snapshot, featured.warning(), metrics)
        }
    })
}

fn add_featured_content(
    screen: ScreenBuilder,
    featured: &FeaturedState,
    covers: &CoverCache,
) -> ScreenBuilder {
    if featured.is_failed() {
        return screen
            .banner(BannerLevel::Attention, "Featured could not be loaded.")
            .primary_button(RETRY, "Try again");
    }
    if let Some(snapshot) = featured.snapshot() {
        let pages = featured_feed_pages(featured, &CLARA_BW_METRICS);
        let page_index = featured.feed_page.min(pages.len().saturating_sub(1));
        let screen = add_feed_warning(screen, featured.warning());
        let screen = add_feed_blocks(screen, snapshot, covers, &pages[page_index].blocks);
        return screen.page_turns(PREVIOUS_PAGE, NEXT_PAGE).page_position(
            u16::try_from(page_index.saturating_add(1)).unwrap_or(u16::MAX),
            u16::try_from(pages.len()).unwrap_or(u16::MAX),
        );
    }
    if featured.is_loading() {
        screen.activity("Loading Featured", None)
    } else {
        screen.text("Featured has not loaded yet.")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ViewportSegment {
    source: usize,
    source_row: u32,
    rows: u32,
    destination_row: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ViewportPlan {
    segments: Vec<ViewportSegment>,
    content_rows: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StripSource {
    start_row: u64,
    end_row: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EpisodeStrip {
    sources: Vec<StripSource>,
    total_rows: u64,
}

#[derive(Debug, Eq, PartialEq)]
struct ViewportBuild {
    offset_rows: u64,
    plan: ViewportPlan,
    format: PictureFormat,
    bytes: Vec<u8>,
    next_segment: usize,
}

impl ViewportBuild {
    fn new(
        offset_rows: u64,
        plan: ViewportPlan,
        format: PictureFormat,
        panel_width: u32,
        panel_height: u32,
    ) -> Result<Self, String> {
        let byte_len = format
            .byte_len(panel_width, panel_height)
            .ok_or_else(|| "The comic viewport byte length is not supported.".to_owned())?;
        Ok(Self {
            offset_rows,
            plan,
            format,
            bytes: vec![255; byte_len],
            next_segment: 0,
        })
    }
}

const MAX_READER_DECODED_PIXELS: usize = 7_000_000;
const MAX_READER_COMPRESSED_BYTES: usize = 4_194_304;
const ELIPSA_THREE_GRAY8_PAGES_BYTES: usize = 1_404 * 1_872 * 3;
const LARGEST_RGB8_PAGE_BYTES: usize = 1_264 * 1_680 * 3;
const MODELED_READER_ALLOCATION_LIMIT_BYTES: usize = 96 * 1024 * 1024;

const fn gray8_conservative_bytes() -> usize {
    11 * MAX_READER_DECODED_PIXELS
        + MAX_READER_COMPRESSED_BYTES
        + MAX_READER_DECODED_PIXELS
        + ELIPSA_THREE_GRAY8_PAGES_BYTES
}

const fn rgb8_conservative_bytes() -> usize {
    11 * MAX_READER_DECODED_PIXELS + MAX_READER_COMPRESSED_BYTES + 2 * LARGEST_RGB8_PAGE_BYTES
}

const _: () = {
    assert!(gray8_conservative_bytes() == 96_079_168);
    assert!(rgb8_conservative_bytes() == 93_935_424);
    assert!(gray8_conservative_bytes() <= MODELED_READER_ALLOCATION_LIMIT_BYTES);
    assert!(rgb8_conservative_bytes() <= MODELED_READER_ALLOCATION_LIMIT_BYTES);
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReaderLimits {
    pages: usize,
    source_slots: usize,
    fetches: usize,
    tasks: usize,
}

fn reader_limits(format: PictureFormat) -> ReaderLimits {
    match format {
        PictureFormat::Gray8 => ReaderLimits {
            pages: 3,
            source_slots: 2,
            fetches: 2,
            tasks: 4,
        },
        PictureFormat::Rgb8 => ReaderLimits {
            pages: 2,
            source_slots: 1,
            fetches: 1,
            tasks: 2,
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FetchIntent {
    Foreground { offset_rows: u64 },
    Prefetch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceFailure {
    advice: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReaderTaskPurpose {
    Manifest,
    ManifestRefresh,
    ForegroundSource { source: usize, offset_rows: u64 },
    PrefetchSource { source: usize },
    Maintenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReaderTaskEntry {
    generation: u64,
    purpose: ReaderTaskPurpose,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WalletTaskPurpose {
    Summary { generation: u64 },
    CoinHistory { generation: u64 },
    TicketHistory { generation: u64 },
}

impl WalletTaskPurpose {
    const fn history(self) -> Option<(AssetKind, u64)> {
        match self {
            Self::CoinHistory { generation } => Some((AssetKind::Coin, generation)),
            Self::TicketHistory { generation } => Some((AssetKind::Ticket, generation)),
            Self::Summary { .. } => None,
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent wallet request and section states can fail and refresh separately"
)]
#[derive(Default)]
struct WalletState {
    summary: Option<WalletSummary>,
    summary_error: bool,
    summary_stale: bool,
    summary_generation: u64,
    summary_task: Option<TaskId>,
    summary_refresh_queued: bool,
    detail_generation: u64,
    tasks: BTreeMap<TaskId, WalletTaskPurpose>,
    coin_history: Vec<ExpirationRow>,
    detail_queue: VecDeque<(WalletTaskPurpose, i64)>,
    ticket_history: Vec<ExpirationRow>,
    coin_history_error: bool,
    ticket_history_error: bool,
}

impl WalletState {
    fn request_summary_generation(&mut self) -> Option<u64> {
        if self.summary_task.is_some() {
            self.summary_refresh_queued = true;
            return None;
        }
        self.summary_generation = self.summary_generation.wrapping_add(1);
        Some(self.summary_generation)
    }

    fn take_queued_summary_refresh(&mut self) -> bool {
        std::mem::take(&mut self.summary_refresh_queued)
    }

    fn accept_summary(&mut self, generation: u64, summary: WalletSummary) -> bool {
        if generation != self.summary_generation {
            return false;
        }
        self.summary = Some(summary);
        self.summary_error = false;
        self.summary_stale = false;
        true
    }

    fn accept_history(
        &mut self,
        generation: u64,
        kind: AssetKind,
        rows: Vec<ExpirationRow>,
    ) -> bool {
        if generation != self.detail_generation {
            return false;
        }
        match kind {
            AssetKind::Coin => {
                self.coin_history = rows;
                self.coin_history_error = false;
            }
            AssetKind::Ticket => {
                self.ticket_history = rows;
                self.ticket_history_error = false;
            }
        }
        true
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GiftTaskPurpose {
    Display,
    Reconcile { generation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GiftTask {
    id: TaskId,
    generation: u64,
    title_id: usize,
    account_scope: commerce::AccountScope,
    purpose: GiftTaskPurpose,
}

#[derive(Default)]
struct TitleGiftState {
    title_id: Option<usize>,
    available: Option<usize>,
    error: bool,
    generation: u64,
    task: Option<GiftTask>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum CommerceTaskPurpose {
    Quote {
        generation: u64,
        account_scope: commerce::AccountScope,
        selection: commerce::Selection,
        purchase: model::PurchaseType,
    },
    Post {
        generation: u64,
        account_scope: commerce::AccountScope,
        marker: commerce::UnresolvedMutationV1,
    },
    ReconcileContent {
        generation: u64,
        account_scope: commerce::AccountScope,
        selection: commerce::Selection,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommerceTask {
    id: TaskId,
    purpose: CommerceTaskPurpose,
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum Evidence<T> {
    NotRequired,
    Pending,
    Value(T),
    Failed,
}

impl<T> Evidence<T> {
    const fn settled(&self) -> bool {
        !matches!(self, Self::Pending)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentEvidence {
    Entitled,
    NotEntitled,
    Contradictory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReconciliationState {
    generation: u64,
    account_scope: commerce::AccountScope,
    marker: Option<commerce::UnresolvedMutationV1>,
    post_accepted: bool,
    content: Evidence<ContentEvidence>,
    wallet: Evidence<usize>,
    gifts: Evidence<usize>,
    wallet_generation: Option<u64>,
    gift_generation: Option<u64>,
}

enum ViewportEntry {
    Building(ViewportBuild),
    Ready { offset_rows: u64, picture: Picture },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Retry {
    #[default]
    Restart,
    Manifest,
    Viewport(u64),
}

struct EpisodeSelection {
    content_alias: String,
    episode_alias: String,
    title: String,
}

struct ReaderState {
    generation: u64,
    format: PictureFormat,
    limits: ReaderLimits,
    panel_width: u32,
    panel_height: u32,
    images: Vec<EpisodeImage>,
    strip: EpisodeStrip,
    offset_rows: u64,
    window: VecDeque<ViewportEntry>,
    source_cache: BTreeMap<usize, Picture>,
    source_fetches: BTreeMap<usize, TaskId>,
    maintenance_task: Option<TaskId>,
    refresh_task: Option<TaskId>,
    refresh_waiters: BTreeMap<usize, FetchIntent>,
    refresh_attempted: BTreeMap<usize, FetchIntent>,
    source_failures: BTreeMap<usize, SourceFailure>,
    picture: Option<TilePicture>,
    chrome_visible: bool,
}

#[derive(Default)]
enum CommentAppendixState {
    #[default]
    Unloaded,
    Loading,
    Ready {
        comments: Vec<Comment>,
        total_items: usize,
    },
    Empty,
    Failed(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PageArrival {
    First,
    Last,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommentTaskPurpose {
    AppendixHot,
    AppendixFallback,
    Comments {
        page: usize,
        arrival: PageArrival,
    },
    Replies {
        comment_id: usize,
        page: usize,
        arrival: PageArrival,
        show_parent: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CommentTask {
    id: TaskId,
    purpose: CommentTaskPurpose,
}

struct CommentPageState {
    comments: Vec<Comment>,
    number: usize,
    total_pages: usize,
    total_items: usize,
    item: usize,
    error: Option<(usize, String)>,
}

struct ReplyState {
    parent: Comment,
    replies: Vec<Comment>,
    number: usize,
    total_pages: usize,
    total_items: usize,
    show_parent: bool,
    text_page: usize,
    item: usize,
    error: Option<(usize, String)>,
}

struct PlannedReaderSpawn {
    source: usize,
    purpose: ReaderTaskPurpose,
    foreground: bool,
    url: String,
}

#[derive(Default)]
struct ReaderMaintenancePlan {
    spawns: Vec<PlannedReaderSpawn>,
    promotion: Option<(TaskId, usize, u64)>,
    refresh_promotion: Option<TaskId>,
    ready: Option<(u64, Picture)>,
}

struct RebasedReaderWindow {
    build: ViewportBuild,
    cached_sources: BTreeSet<usize>,
    fetches: BTreeMap<usize, TaskId>,
}

#[derive(Default)]
struct Bomtoon {
    account: AccountState,
    connection: ConnectionState,
    account_scope: Option<commerce::AccountScope>,
    scope_task: Option<TaskId>,
    scope_refresh_pending: bool,
    commerce: commerce::Commerce,
    marker_store: Option<MarkerStoreOperation>,
    commerce_generation: u64,
    commerce_task: Option<CommerceTask>,
    queued_commerce_command: Option<commerce::CommerceCommand>,
    commerce_episode: Option<usize>,
    pending_purchase_rejection: Option<&'static str>,
    purchase_rejection_notice: Option<&'static str>,
    retained_quote: Option<commerce::QuotePresentation>,
    reconciliation: Option<ReconciliationState>,
    reconciliation_post_accepted: bool,
    view: View,
    destination: MainDestination,
    pending: Option<Pending>,
    task: Option<TaskId>,
    queued_foreground: Option<Pending>,
    wallet: WalletState,
    gifts: TitleGiftState,
    featured: FeaturedState,
    feature_tasks: BTreeMap<TaskId, FeatureTaskPurpose>,
    superseded_feature_tasks: BTreeSet<TaskId>,
    feature_capacity_state: FeatureCapacityState,
    covers: CoverCache,
    comics: Vec<Comic>,
    recent: Vec<RecentEntry>,
    library_load: ShelfLoadState,
    recent_load: ShelfLoadState,
    episodes: Vec<Episode>,
    selected_content_id: Option<usize>,
    selected_content_alias: String,
    selected_title: String,
    selected_creators: String,
    selected_synopsis: String,
    synopsis: SynopsisState,
    episode_title_preview: String,
    episode_creators_preview: String,
    episode_items_per_page: Option<usize>,
    reader_selection: Option<EpisodeSelection>,
    reader_after_content_refresh: Option<usize>,
    reader: Option<ReaderState>,
    reader_generation: u64,
    reader_tasks: BTreeMap<TaskId, ReaderTaskEntry>,
    foreground_reader_task: Option<TaskId>,
    reader_manifest_pending: bool,
    comment_appendix: CommentAppendixState,
    comment_task: Option<CommentTask>,
    comments: Option<CommentPageState>,
    replies: Option<ReplyState>,
    retry: Retry,
    next_picture_handle: u32,
    page: usize,
    next_library_page: Option<usize>,
    next_recent_page: Option<usize>,
    total_library_titles: usize,
    total_recent_titles: usize,
    problem: Option<String>,
}
impl Bomtoon {
    fn show(&mut self, context: &mut Context) {
        self.sync_visible_covers(context);
        let owns_back = self.view == View::FeatureCollection
            || (self.account == AccountState::Active
                && match self.view {
                    View::Account | View::Episodes => {
                        self.pending.is_none()
                            && self.queued_foreground.is_none()
                            && self.problem.is_none()
                    }
                    View::Reader | View::CommentAppendix | View::Comments | View::Replies => true,
                    View::Status | View::Main | View::FeatureCollection => false,
                });
        context.set_screen(self.screen().with_own_back(owns_back));
    }

    fn visible_cover_urls(&self) -> Vec<String> {
        if !matches!(
            self.view,
            View::Main | View::FeatureCollection | View::Episodes
        ) || self.pending.is_some()
            || self.queued_foreground.is_some()
            || self.commerce_task.is_some()
            || self.queued_commerce_command.is_some()
            || self.problem.is_some()
            || self.foreground_reader_task.is_some()
        {
            return Vec::new();
        }
        let mut seen = BTreeSet::new();
        let mut visible = Vec::new();
        let mut push = |url: Option<&String>| {
            if let Some(url) = url {
                if seen.insert(url.clone()) {
                    visible.push(url.clone());
                }
            }
        };
        if self.view == View::FeatureCollection {
            if let Some((view, collection)) = self.featured.collection.as_ref().and_then(|view| {
                self.featured
                    .snapshot()
                    .and_then(|snapshot| snapshot.collection(&view.collection_id))
                    .map(|collection| (view, collection))
            }) {
                if let Some(range) = view.pages.get(view.page) {
                    for comic in &collection.comics[range.clone()] {
                        push(comic.square_url.as_ref().or(comic.vertical_url.as_ref()));
                    }
                }
            }
            return visible;
        }
        match self.view {
            View::Main => match self.destination {
                MainDestination::Featured if self.featured.snapshot().is_some() => {
                    let snapshot = self.featured.snapshot().expect("checked Feature snapshot");
                    let pages = featured_feed_pages(&self.featured, &CLARA_BW_METRICS);
                    let page = &pages[self.featured.feed_page.min(pages.len().saturating_sub(1))];
                    for block in &page.blocks {
                        match *block {
                            FeedBlock::Banners => {
                                for comic in snapshot.banners.iter().take(3) {
                                    push(comic.vertical_url.as_ref().or(comic.square_url.as_ref()));
                                }
                            }
                            FeedBlock::Collection(index) | FeedBlock::ThemeWithHeading(index) => {
                                for comic in snapshot.collections[index].comics.iter().take(6) {
                                    push(comic.vertical_url.as_ref());
                                }
                            }
                        }
                    }
                }
                MainDestination::Recent if self.recent_load.loaded => {
                    let (start, end) =
                        page_bounds(self.page, self.recent.len(), LIBRARY_ITEMS_PER_PAGE);
                    for entry in &self.recent[start..end] {
                        push(entry.cover_url.as_ref());
                    }
                }
                MainDestination::Library if self.library_load.loaded => {
                    let (start, end) =
                        page_bounds(self.page, self.comics.len(), LIBRARY_ITEMS_PER_PAGE);
                    for comic in &self.comics[start..end] {
                        push(comic.cover_url.as_ref());
                    }
                }
                MainDestination::Featured | MainDestination::Recent | MainDestination::Library => {}
            },
            View::Episodes => {
                let rows = self.episode_rows_per_page();
                let (start, end) = page_bounds(self.page, self.episodes.len(), rows);
                for episode in &self.episodes[start..end] {
                    push(episode.thumbnail_url.as_ref());
                }
            }
            View::Status
            | View::Account
            | View::Reader
            | View::CommentAppendix
            | View::Comments
            | View::Replies
            | View::FeatureCollection => {}
        }
        visible
    }

    fn visible_cover_source(&self) -> Option<CoverSource> {
        if self.pending.is_some()
            || self.queued_foreground.is_some()
            || self.commerce_task.is_some()
            || self.queued_commerce_command.is_some()
            || self.problem.is_some()
            || self.foreground_reader_task.is_some()
        {
            return None;
        }
        match self.view {
            View::FeatureCollection => Some(CoverSource::Public),
            View::Main => Some(match self.destination {
                MainDestination::Featured => CoverSource::Public,
                MainDestination::Recent | MainDestination::Library => CoverSource::Protected,
            }),
            View::Episodes => Some(CoverSource::Protected),
            View::Status
            | View::Account
            | View::Reader
            | View::CommentAppendix
            | View::Comments
            | View::Replies => None,
        }
    }

    fn sync_visible_covers(&mut self, context: &mut Context) {
        if self.view == View::Account || self.pending == Some(Pending::Logout) {
            return;
        }
        let visible = self.visible_cover_urls();
        let visible_source = self.visible_cover_source();
        if visible != self.covers.visible_urls || visible_source != self.covers.visible_source {
            self.covers.generation = self.covers.generation.wrapping_add(1);
            let generation = self.covers.generation;
            let visible_set = visible.iter().cloned().collect::<BTreeSet<_>>();
            let episode_urls = self.episode_thumbnail_urls();
            let public_urls = self.public_cover_urls();
            self.evict_episode_thumbnails(context, &visible_set, &episode_urls, &public_urls);
            let obsolete = self
                .covers
                .tasks
                .iter()
                .filter(|(_, task)| !visible_set.contains(&task.url))
                .map(|(task, cover)| (*task, cover.url.clone()))
                .collect::<Vec<_>>();
            for (task, url) in obsolete {
                context.cancel(task);
                self.covers.tasks.remove(&task);
                if self.covers.entries.get(&url) == Some(&CoverState::Loading(task)) {
                    self.covers.entries.remove(&url);
                }
            }
            for task in self.covers.tasks.values_mut() {
                if visible_set.contains(&task.url) {
                    task.generation = generation;
                    if let Some(source) = visible_source {
                        task.source = source;
                    }
                }
            }
            for url in &visible {
                if self.covers.entries.get(url) == Some(&CoverState::Failed) {
                    self.covers.entries.remove(url);
                }
            }
            self.covers.visible_urls = visible;
            self.covers.visible_source = visible_source;
        }
        self.spawn_visible_covers(context);
    }

    fn spawn_visible_covers(&mut self, context: &mut Context) {
        if self.pending.is_some()
            || self.queued_foreground.is_some()
            || self.commerce_task.is_some()
            || self.queued_commerce_command.is_some()
        {
            return;
        }
        let Some(source) = self.covers.visible_source else {
            return;
        };
        for url in self.covers.visible_urls.clone() {
            if !self.cover_url_owned(source, &url) {
                continue;
            }
            if self.covers.entries.contains_key(&url) {
                continue;
            }
            let Some(task) = context.spawn(api::image(&url)) else {
                break;
            };
            self.covers
                .entries
                .insert(url.clone(), CoverState::Loading(task));
            self.covers.tasks.insert(
                task,
                CoverTask {
                    generation: self.covers.generation,
                    url,
                    source,
                },
            );
        }
    }

    fn episode_balance_label(&self) -> String {
        let coins = self
            .wallet
            .summary
            .and_then(|summary| summary.coins.total())
            .map_or_else(
                || {
                    if self.wallet.summary_task.is_some() {
                        "Coins…".to_owned()
                    } else {
                        "Coins unavailable".to_owned()
                    }
                },
                |total| format!("Coins {total}"),
            );
        let gifts = self.gifts.available.map_or_else(
            || {
                if self.gifts.task.is_some() {
                    "Gifts…".to_owned()
                } else {
                    "Gifts unavailable".to_owned()
                }
            },
            |available| {
                if self.gifts.error {
                    format!("Gifts {available} (refresh failed)")
                } else if self.gifts.task.is_some() {
                    format!("Gifts {available} (refreshing)")
                } else {
                    format!("Gifts {available}")
                }
            },
        );
        format!("{coins} · {gifts}")
    }

    fn quote_episode_title(&self) -> String {
        self.commerce_episode
            .and_then(|index| self.episodes.get(index))
            .map_or_else(
                || "Episode options".to_owned(),
                |episode| display_text(&episode.title, &format!("Episode {}", episode.alias)),
            )
    }

    fn quote_screen(&self) -> Option<Screen> {
        if self.commerce.state() == commerce::CommerceState::AcceptedButStale {
            if self.commerce.marker_belongs_to_another_account() {
                return None;
            }
            return Some(
                ScreenBuilder::new("bomtoon-commerce-stale")
                    .top_bar(self.selected_title.clone())
                    .heading("Accepted, refresh needed")
                    .text("Confirm this episode and its affected balance before trying again.")
                    .divider()
                    .button(REFRESH_COMMERCE, "Refresh status")
                    .owns_back(true)
                    .build(),
            );
        }
        let presentation = self
            .commerce
            .quote_presentation()
            .or(self.retained_quote.as_ref())?;
        let disabled_reasons = [
            commerce::Action::UseGift,
            commerce::Action::Rent,
            commerce::Action::Buy,
        ]
        .into_iter()
        .filter_map(|action| {
            let control = presentation.control(action);
            control
                .disabled_reason
                .as_ref()
                .map(|reason| format!("{}: {reason}", control.label))
        })
        .collect::<Vec<_>>()
        .join("\n");
        let gift = presentation.control(commerce::Action::UseGift);
        let rent = presentation.control(commerce::Action::Rent);
        let buy = presentation.control(commerce::Action::Buy);
        let cancel = presentation.control(commerce::Action::Cancel);
        let interactive = self.commerce.state() == commerce::CommerceState::Choosing;
        let state = |enabled| {
            if enabled {
                ControlState::Enabled
            } else {
                ControlState::Disabled
            }
        };
        let mut screen = ScreenBuilder::new("bomtoon-commerce-quote")
            .top_bar(self.selected_title.clone())
            .heading(self.quote_episode_title())
            .text(self.episode_balance_label())
            .divider();
        if presentation.quote_changed {
            screen = screen.text("Options changed. Review the current price and availability.");
        }
        if !disabled_reasons.is_empty() {
            screen = screen.text(disabled_reasons);
        }
        Some(
            screen
                .button_with_state(
                    USE_GIFT,
                    gift.label.clone(),
                    state(interactive && gift.enabled),
                )
                .button_with_state(RENT, rent.label.clone(), state(interactive && rent.enabled))
                .button_with_state(BUY, buy.label.clone(), state(interactive && buy.enabled))
                .button_with_state(
                    CANCEL_COMMERCE,
                    cancel.label.clone(),
                    state(interactive && cancel.enabled),
                )
                .owns_back(true)
                .build(),
        )
    }

    fn screen(&self) -> Screen {
        if let Some(pending) = self.pending.or(self.queued_foreground) {
            let message = match pending {
                Pending::Content(_) => Some("Loading episode purchase status"),
                Pending::Logout => Some("Signing out"),
                Pending::Library(_) | Pending::Recent(_) => None,
            };
            if let Some(message) = message {
                return ScreenBuilder::new("bomtoon-loading")
                    .top_bar(TITLE)
                    .activity(message, None)
                    .build();
            }
        }
        if self.reader_manifest_pending {
            return ScreenBuilder::new("bomtoon-loading")
                .top_bar(self.reader_title())
                .activity("Loading comic pages", None)
                .build();
        }
        if let Some(entry) = self
            .foreground_reader_task
            .and_then(|task| self.reader_tasks.get(&task))
        {
            let retains_page = matches!(entry.purpose, ReaderTaskPurpose::ForegroundSource { .. })
                && self
                    .reader
                    .as_ref()
                    .is_some_and(|reader| reader.picture.is_some());
            if !retains_page {
                let message = match entry.purpose {
                    ReaderTaskPurpose::ForegroundSource { .. } => "Loading comic image",
                    ReaderTaskPurpose::Manifest
                    | ReaderTaskPurpose::ManifestRefresh
                    | ReaderTaskPurpose::PrefetchSource { .. }
                    | ReaderTaskPurpose::Maintenance => "Loading comic pages",
                };
                return ScreenBuilder::new("bomtoon-loading")
                    .top_bar(self.reader_title())
                    .activity(message, None)
                    .build();
            }
        }
        if self.account == AccountState::Checking && self.connection == ConnectionState::Offline {
            return ScreenBuilder::new("bomtoon-offline")
                .top_bar(TITLE)
                .failure_state(Failure::of(TaskError::Offline), RETRY)
                .build();
        }
        if let Some(problem) = &self.problem {
            let title = if self.view.is_reader_flow() {
                self.reader_title()
            } else {
                TITLE
            };
            return ScreenBuilder::new("bomtoon-error")
                .top_bar(title)
                .banner(BannerLevel::Attention, problem.clone())
                .primary_button(RETRY, "Try again")
                .build();
        }
        if let Some(screen) = self.quote_screen() {
            return screen;
        }
        match self.view {
            View::Status if self.account != AccountState::Active => self.signed_out_screen(),
            View::Status => ScreenBuilder::new("bomtoon-status")
                .top_bar(TITLE)
                .text("No request has started.")
                .primary_button(RETRY, "Connect")
                .build(),
            View::Main => self.main_screen(),
            View::FeatureCollection => self.collection_screen(),
            View::Account => self.account_screen(),
            View::Episodes => self.episode_screen(),
            View::Reader => self.reader_screen(),
            View::CommentAppendix => self.comment_appendix_screen(),
            View::Comments => self.comments_screen(),
            View::Replies => self.replies_screen(),
        }
    }

    fn reader_title(&self) -> &str {
        self.reader_selection
            .as_ref()
            .map_or(TITLE, |selection| selection.title.as_str())
    }

    fn signed_out_screen(&self) -> Screen {
        let mut screen = ScreenBuilder::new("bomtoon-signed-out").top_bar(TITLE);
        screen = match self.account {
            AccountState::Expired => {
                screen.banner(BannerLevel::Attention, "Your BOMTOON sign-in has expired.")
            }
            AccountState::RevocationUnconfirmed => screen.banner(
                BannerLevel::Attention,
                Failure::of(TaskError::RevocationUnconfirmed).advice,
            ),
            AccountState::Checking | AccountState::SignedOut | AccountState::Active => screen,
        };
        screen
            .text("Run this on your Mac:\nkobo bomtoon login --device <Kobo IP>")
            .primary_button(RETRY, "Try again")
            .build()
    }

    fn account_action_label(&self) -> String {
        match self
            .wallet
            .summary
            .and_then(|summary| summary.coins.total())
        {
            Some(total) => format!("Coins {total}"),
            None if self.wallet.summary_task.is_some() => "Coins…".to_owned(),
            None => "Coins unavailable".to_owned(),
        }
    }

    fn main_screen(&self) -> Screen {
        let mut screen = ScreenBuilder::new(match self.destination {
            MainDestination::Featured => "bomtoon-featured",
            MainDestination::Recent => "bomtoon-recent",
            MainDestination::Library => "bomtoon-library",
        })
        .top_bar(self.destination.title());
        if self.account == AccountState::Active {
            screen = screen.top_bar_action(ACCOUNT, self.account_action_label());
        } else {
            screen = screen.top_bar_action(SIGN_IN, "Sign in");
        }

        match self.destination {
            MainDestination::Featured => {
                screen = add_featured_content(screen, &self.featured, &self.covers);
            }
            MainDestination::Recent => {
                if self.recent_load.pending_page.is_some() {
                    screen = screen.activity("Loading recent reading", None);
                } else if let Some((_, error)) = &self.recent_load.error {
                    screen = screen
                        .banner(BannerLevel::Attention, error.clone())
                        .primary_button(RETRY, "Try again");
                } else {
                    let (start, end) =
                        page_bounds(self.page, self.recent.len(), LIBRARY_ITEMS_PER_PAGE);
                    screen = screen.rows_with_trailing((start..end).map(|index| {
                        let recent = &self.recent[index];
                        let episode_fallback = format!("Episode {}", recent.episode_alias);
                        (
                            format!("comic-{index}"),
                            recent.content_title.clone(),
                            recent.creators.clone(),
                            cover_lead(&self.covers, recent.cover_url.as_deref()),
                            display_text(&recent.episode_title, &episode_fallback),
                        )
                    }));
                    let pages = self
                        .destination_total()
                        .max(self.recent.len())
                        .div_ceil(LIBRARY_ITEMS_PER_PAGE)
                        .max(1);
                    screen = screen.page_turns(PREVIOUS_PAGE, NEXT_PAGE).page_position(
                        u16::try_from(self.page.saturating_add(1)).unwrap_or(u16::MAX),
                        u16::try_from(pages).unwrap_or(u16::MAX),
                    );
                }
            }
            MainDestination::Library => {
                if self.library_load.pending_page.is_some() {
                    screen = screen.activity("Loading your library", None);
                } else if let Some((_, error)) = &self.library_load.error {
                    screen = screen
                        .banner(BannerLevel::Attention, error.clone())
                        .primary_button(RETRY, "Try again");
                } else {
                    let (start, end) =
                        page_bounds(self.page, self.comics.len(), LIBRARY_ITEMS_PER_PAGE);
                    screen = screen.rows_with_trailing((start..end).map(|index| {
                        let comic = &self.comics[index];
                        (
                            format!("comic-{index}"),
                            comic.title.clone(),
                            comic.creators.clone(),
                            cover_lead(&self.covers, comic.cover_url.as_deref()),
                            format!("{} / {}", comic.owned_episodes, comic.total_episodes),
                        )
                    }));
                    let pages = self
                        .destination_total()
                        .max(self.comics.len())
                        .div_ceil(LIBRARY_ITEMS_PER_PAGE)
                        .max(1);
                    screen = screen.page_turns(PREVIOUS_PAGE, NEXT_PAGE).page_position(
                        u16::try_from(self.page.saturating_add(1)).unwrap_or(u16::MAX),
                        u16::try_from(pages).unwrap_or(u16::MAX),
                    );
                }
            }
        }

        screen
            .nav_bar(self.destination.index(), MAIN_DESTINATIONS)
            .build()
    }

    fn collection_screen(&self) -> Screen {
        let Some((view, collection)) = self.featured.collection.as_ref().and_then(|view| {
            self.featured
                .snapshot()
                .and_then(|snapshot| snapshot.collection(&view.collection_id))
                .map(|collection| (view, collection))
        }) else {
            return ScreenBuilder::new("bomtoon-feature-collection")
                .top_bar("Featured")
                .text("This collection is no longer available.")
                .build();
        };

        let mut screen =
            ScreenBuilder::new("bomtoon-feature-collection").top_bar(collection.label.clone());
        if let Some(range) = view.pages.get(view.page) {
            screen = screen.described_rows_with_trailing(
                RowLineLimits::new(1, 1, 2),
                range.clone().map(|index| {
                    let comic = &collection.comics[index];
                    (
                        comic_action(&collection.id, index),
                        display_content_title(&comic.title, &comic.alias),
                        display_text(&comic.creators, ""),
                        synopsis_for(&self.featured.detail_cache, &comic.alias),
                        collection_cover_lead(
                            &self.covers,
                            comic
                                .square_url
                                .as_deref()
                                .or(comic.vertical_url.as_deref()),
                        ),
                        compact_count(comic.view_count),
                    )
                }),
            );
        } else {
            screen = screen.activity("Loading collection details", None);
        }
        if view.pages.get(view.page).is_none() {
            return screen.build();
        }

        let complete = view
            .pages
            .last()
            .is_some_and(|range| range.end >= collection.comics.len());
        let total = if complete { view.pages.len() } else { 0 };
        screen
            .page_turns(PREVIOUS_PAGE, NEXT_PAGE)
            .page_position(
                u16::try_from(view.page.saturating_add(1)).unwrap_or(u16::MAX),
                u16::try_from(total).unwrap_or(u16::MAX),
            )
            .build()
    }

    fn history_is_loading(&self, kind: AssetKind) -> bool {
        self.wallet
            .tasks
            .values()
            .copied()
            .chain(self.wallet.detail_queue.iter().map(|(purpose, _)| *purpose))
            .any(|purpose| purpose.history() == Some((kind, self.wallet.detail_generation)))
    }

    fn history_status(&self, kind: AssetKind) -> String {
        let (rows, error) = match kind {
            AssetKind::Coin => (
                self.wallet.coin_history.len(),
                self.wallet.coin_history_error,
            ),
            AssetKind::Ticket => (
                self.wallet.ticket_history.len(),
                self.wallet.ticket_history_error,
            ),
        };
        if self.history_is_loading(kind) {
            if rows == 0 {
                "Loading…".to_owned()
            } else {
                format!("Refreshing… · {rows} cached")
            }
        } else if error {
            if rows == 0 {
                "Unavailable".to_owned()
            } else {
                format!("Unavailable · {rows} cached")
            }
        } else if rows == 0 {
            match kind {
                AssetKind::Coin => "No coin expiration records".to_owned(),
                AssetKind::Ticket => "No ticket expiration records".to_owned(),
            }
        } else if rows == 1 {
            "1 entry".to_owned()
        } else {
            format!("{rows} entries")
        }
    }

    fn history_row_label_at(row: &ExpirationRow, now_ms: Option<i64>) -> String {
        let kind = match row.kind {
            AssetKind::Coin => "Coin",
            AssetKind::Ticket => "Ticket",
        };
        let subtype = match row.subtype {
            AssetSubtype::Standard => "Standard",
            AssetSubtype::Bonus => "Bonus",
            AssetSubtype::Free => "Free",
        };
        let expiration = match (row.expires_at, now_ms) {
            (Some(timestamp), Some(now)) => {
                match (
                    taipei_day(timestamp),
                    taipei_day(now),
                    taipei_date(timestamp),
                ) {
                    (Some(expiration_day), Some(current_day), Some(expiration_date)) => {
                        if expiration_day <= current_day {
                            format!("Expired {expiration_date}")
                        } else {
                            format!("Expires {expiration_date}")
                        }
                    }
                    _ => "Expiration unavailable".to_owned(),
                }
            }
            (Some(_), None) => "Expiration unavailable".to_owned(),
            (None, _) => "No expiry".to_owned(),
        };
        format!("{kind} · {subtype} · {} · {expiration}", row.quantity)
    }

    fn account_screen(&self) -> Screen {
        let mut screen = ScreenBuilder::new("bomtoon-account")
            .top_bar("Account")
            .top_bar_action(SIGN_OUT, "Sign out");
        screen = match self.wallet.summary {
            Some(summary) => screen
                .section_with_value(
                    "Coins",
                    summary
                        .coins
                        .total()
                        .map_or_else(|| "Unavailable".to_owned(), |total| total.to_string()),
                )
                .facts([
                    ("Standard coins", summary.coins.standard.to_string()),
                    ("Bonus coins", summary.coins.bonus.to_string()),
                    ("Free coins", summary.coins.free.to_string()),
                ])
                .section_with_value(
                    "Tickets",
                    summary
                        .tickets
                        .total()
                        .map_or_else(|| "Unavailable".to_owned(), |total| total.to_string()),
                )
                .facts([
                    ("Standard tickets", summary.tickets.standard.to_string()),
                    ("Bonus tickets", summary.tickets.bonus.to_string()),
                    ("Free tickets", summary.tickets.free.to_string()),
                ]),
            None if self.wallet.summary_task.is_some() => {
                screen.section_with_value("Balances", "Loading…")
            }
            None => screen
                .section_with_value("Balances", "Unavailable")
                .text("Balances unavailable."),
        };
        if self.wallet.summary_stale {
            screen = screen.banner(BannerLevel::Attention, "Balances may be out of date.");
        }
        screen = screen
            .section_with_value("Coin history", self.history_status(AssetKind::Coin))
            .section_with_value("Ticket history", self.history_status(AssetKind::Ticket));

        let row_count = self
            .wallet
            .coin_history
            .len()
            .saturating_add(self.wallet.ticket_history.len());
        let (start, end) = page_bounds(self.page, row_count, ACCOUNT_HISTORY_ITEMS_PER_PAGE);
        let now_ms = unix_time_ms();
        if start < end {
            screen = screen.paged_list(
                u16::try_from(self.page.saturating_add(1)).unwrap_or(u16::MAX),
                self.wallet
                    .coin_history
                    .iter()
                    .chain(&self.wallet.ticket_history)
                    .skip(start)
                    .take(end - start)
                    .map(|row| Self::history_row_label_at(row, now_ms)),
            );
        }
        let pages = row_count.div_ceil(ACCOUNT_HISTORY_ITEMS_PER_PAGE).max(1);
        screen = screen.page_turns(PREVIOUS_PAGE, NEXT_PAGE).page_position(
            u16::try_from(self.page.saturating_add(1)).unwrap_or(u16::MAX),
            u16::try_from(pages).unwrap_or(u16::MAX),
        );
        if self.wallet.summary_error
            || self.wallet.coin_history_error
            || self.wallet.ticket_history_error
        {
            screen = screen.button(RETRY_BALANCES, "Retry balances");
        }
        screen.build()
    }

    fn destination_len(&self) -> usize {
        match self.destination {
            MainDestination::Featured => 0,
            MainDestination::Recent => self.recent.len(),
            MainDestination::Library => self.comics.len(),
        }
    }

    fn destination_total(&self) -> usize {
        match self.destination {
            MainDestination::Featured => 0,
            MainDestination::Recent => self.total_recent_titles,
            MainDestination::Library => self.total_library_titles,
        }
    }

    fn destination_next_page(&self) -> Option<usize> {
        match self.destination {
            MainDestination::Featured => None,
            MainDestination::Recent => self.next_recent_page,
            MainDestination::Library => self.next_library_page,
        }
    }

    fn select_destination(&mut self, context: &mut Context, target: MainDestination) {
        self.page = 0;
        if target == MainDestination::Featured {
            self.featured.feed_page = 0;
            self.request_local_day(context);
        }
        self.problem = None;
        if target != MainDestination::Featured && self.account != AccountState::Active {
            self.destination = MainDestination::Featured;
            self.view = View::Status;
            return;
        }

        self.destination = target;
        self.view = View::Main;
        let needs_request = match target {
            MainDestination::Recent => {
                !self.recent_load.loaded
                    && self.recent_load.pending_page.is_none()
                    && self.recent_load.error.is_none()
            }
            MainDestination::Library => {
                !self.library_load.loaded
                    && self.library_load.pending_page.is_none()
                    && self.library_load.error.is_none()
            }
            MainDestination::Featured => false,
        };
        if needs_request {
            let pending = match target {
                MainDestination::Recent => Pending::Recent(0),
                MainDestination::Library => Pending::Library(0),
                MainDestination::Featured => return,
            };
            self.request_foreground(context, pending);
        }
    }

    fn add_episode_header_preview(
        &self,
        screen: ScreenBuilder,
        preview: String,
        truncated: bool,
    ) -> ScreenBuilder {
        let screen = screen
            .heading(self.episode_title_preview.clone())
            .text(self.episode_creators_preview.clone())
            .text(preview);
        if truncated {
            screen.button(SYNOPSIS_MORE, "More")
        } else {
            screen
        }
    }

    fn episode_preview_fits(&self, preview: String, truncated: bool) -> bool {
        let header = self.add_episode_header_preview(
            ScreenBuilder::new("bomtoon-episode-preview-measure").top_bar("Episodes"),
            preview,
            truncated,
        );
        let pages = self.episodes.len().max(1);
        (0..pages).all(|page| {
            let screen = self.add_episode_body(header.clone(), 1, page, true).build();
            !screen
                .diagnostics(&CLARA_BW_METRICS, &Chrome::measuring(true))
                .has_errors()
        })
    }

    fn measure_episode_synopsis_preview(&self) -> (String, bool) {
        let text = &self.selected_synopsis;
        if text.len() <= SYNOPSIS_PREVIEW_BYTES {
            let full = text.clone();
            if self.episode_preview_fits(full.clone(), false) {
                return (full, false);
            }
        }

        let mut maximum_end = text.len().min(SYNOPSIS_PREVIEW_BYTES);
        while !text.is_char_boundary(maximum_end) {
            maximum_end -= 1;
        }
        let mut boundaries = text
            .char_indices()
            .map(|(offset, _)| offset)
            .take_while(|offset| *offset <= maximum_end)
            .collect::<Vec<_>>();
        if maximum_end < text.len() && boundaries.last().copied() != Some(maximum_end) {
            boundaries.push(maximum_end);
        }
        if boundaries.is_empty() {
            return (String::new(), false);
        }

        let (smallest, smallest_truncated) = comment_preview(text, boundaries[0]);
        let mut best = self
            .episode_preview_fits(smallest, smallest_truncated)
            .then_some(0);
        let mut low = 1;
        let mut high = boundaries.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let (candidate, truncated) = comment_preview(text, boundaries[middle]);
            if self.episode_preview_fits(candidate, truncated) {
                best = Some(middle);
                low = middle.saturating_add(1);
            } else {
                high = middle;
            }
        }
        comment_preview(text, boundaries[best.unwrap_or(0)])
    }

    fn add_episode_header(&self, screen: ScreenBuilder) -> ScreenBuilder {
        self.add_episode_header_preview(
            screen,
            self.synopsis.preview.clone(),
            self.synopsis.preview_truncated,
        )
    }

    fn episode_capacity_fits(&self, header: &ScreenBuilder, rows_per_page: usize) -> bool {
        let pages = self.episodes.len().div_ceil(rows_per_page).max(1);
        (0..pages).all(|page| {
            let actual = self.episode_screen_for(page, rows_per_page, false);
            if !episode_screen_layout_fits(&actual) {
                return false;
            }
            let reserved = self
                .add_episode_body(header.clone(), rows_per_page, page, true)
                .build();
            episode_screen_layout_fits(&reserved)
        })
    }

    fn measure_episode_page_capacity(&mut self) -> Option<usize> {
        let header = self.add_episode_header(
            ScreenBuilder::new("bomtoon-episode-capacity-measure").top_bar("Episodes"),
        );
        let mut stable_capacity = EPISODE_ITEMS_PER_PAGE;
        for _ in 0..2 {
            let measured_capacity = (1..=EPISODE_ITEMS_PER_PAGE).rev().find(|rows_per_page| {
                self.episode_items_per_page = Some(*rows_per_page);
                self.episode_capacity_fits(&header, *rows_per_page)
            });
            let Some(measured_capacity) = measured_capacity else {
                self.episode_items_per_page = None;
                return None;
            };
            stable_capacity = stable_capacity.min(measured_capacity);
        }
        let capacity = if self.selected_title.len() > EPISODE_TITLE_PREVIEW_BYTES
            || self.selected_creators.len() > EPISODE_CREATORS_PREVIEW_BYTES
        {
            1
        } else {
            stable_capacity
        };
        self.episode_capacity_fits(&header, capacity)
            .then_some(capacity)
    }

    fn prepare_episode_layout(&mut self) {
        let title_fits_default =
            normalized_header_len(&self.selected_title) <= EPISODE_TITLE_PREVIEW_BYTES;
        let creators_fit_default =
            normalized_header_len(&self.selected_creators) <= EPISODE_CREATORS_PREVIEW_BYTES;
        for (title_bytes, creator_bytes) in [
            (EPISODE_TITLE_PREVIEW_BYTES, EPISODE_CREATORS_PREVIEW_BYTES),
            (96, 96),
            (48, 48),
            (24, 24),
        ] {
            self.episode_items_per_page = None;
            let title_bytes = if title_fits_default {
                EPISODE_TITLE_PREVIEW_BYTES
            } else {
                title_bytes
            };
            let creator_bytes = if creators_fit_default {
                EPISODE_CREATORS_PREVIEW_BYTES
            } else {
                creator_bytes
            };
            self.episode_title_preview =
                normalized_header_preview(&self.selected_title, title_bytes, "Comic");
            self.episode_creators_preview =
                normalized_header_preview(&self.selected_creators, creator_bytes, "Creators");
            let (preview, preview_truncated) = self.measure_episode_synopsis_preview();
            self.synopsis.preview = preview;
            self.synopsis.preview_truncated = preview_truncated;
            if let Some(rows_per_page) = self.measure_episode_page_capacity() {
                self.episode_items_per_page = Some(rows_per_page);
                return;
            }
        }

        self.episode_title_preview = normalized_header_preview(
            &self.selected_title,
            if title_fits_default {
                EPISODE_TITLE_PREVIEW_BYTES
            } else {
                24
            },
            "Comic",
        );
        self.episode_creators_preview = normalized_header_preview(
            &self.selected_creators,
            if creators_fit_default {
                EPISODE_CREATORS_PREVIEW_BYTES
            } else {
                24
            },
            "Creators unavailable",
        );
        self.synopsis.preview.clear();
        self.synopsis.preview_truncated = !self.selected_synopsis.is_empty();
        self.episode_items_per_page = None;
        let rows_per_page = self
            .measure_episode_page_capacity()
            .expect("minimal episode header must fit one measured row");
        self.episode_items_per_page = Some(rows_per_page);
    }

    fn episode_rows_per_page(&self) -> usize {
        self.episode_items_per_page.unwrap_or(1)
    }

    fn visible_episode_anchor(&self) -> Option<usize> {
        if self.view != View::Episodes {
            return None;
        }
        let (start, end) =
            page_bounds(self.page, self.episodes.len(), self.episode_rows_per_page());
        (start < end).then(|| self.episodes[start].id)
    }

    fn restore_episode_page(&mut self, anchor: Option<usize>, fallback_page: usize) {
        let rows_per_page = self.episode_rows_per_page();
        let last_page = self
            .episodes
            .len()
            .saturating_sub(1)
            .checked_div(rows_per_page)
            .unwrap_or(0);
        self.page = anchor
            .and_then(|id| self.episodes.iter().position(|episode| episode.id == id))
            .map_or_else(
                || fallback_page.min(last_page),
                |index| index / rows_per_page,
            );
    }

    fn add_episode_body(
        &self,
        screen: ScreenBuilder,
        rows_per_page: usize,
        page: usize,
        reserve_transient_space: bool,
    ) -> ScreenBuilder {
        let mut screen = if reserve_transient_space || self.gifts.error {
            screen.button(
                RETRY_GIFTS,
                format!("{} · Retry Gift", self.episode_balance_label()),
            )
        } else {
            screen.text(self.episode_balance_label())
        };
        if reserve_transient_space {
            screen = screen.text("Purchase rejected: FAIL");
        } else if let Some(result) = self.purchase_rejection_notice {
            screen = screen.text(format!("Purchase rejected: {result}"));
        }
        let marker_belongs_to_another_account = self.commerce.marker_belongs_to_another_account();
        if reserve_transient_space || marker_belongs_to_another_account {
            screen = screen.text(
                "A purchase is unresolved for another account. Restore the original account to refresh its status.",
            );
        }
        let (start, end) = page_bounds(page, self.episodes.len(), rows_per_page);
        let now_ms = unix_time_ms();
        screen = screen.rows_with_trailing((start..end).map(|index| {
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
        let episode_pages = self.episodes.len().div_ceil(rows_per_page).max(1);
        screen.page_turns(PREVIOUS_PAGE, NEXT_PAGE).page_position(
            u16::try_from(page.saturating_add(1)).unwrap_or(u16::MAX),
            u16::try_from(episode_pages).unwrap_or(u16::MAX),
        )
    }

    fn episode_screen_for(&self, page: usize, rows_per_page: usize, modal: bool) -> Screen {
        let header =
            self.add_episode_header(ScreenBuilder::new("bomtoon-episodes").top_bar("Episodes"));
        let mut screen = self.add_episode_body(header, rows_per_page, page, false);
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

    fn episode_screen(&self) -> Screen {
        let rows_per_page = self.episode_rows_per_page();
        let last_page = self
            .episodes
            .len()
            .saturating_sub(1)
            .checked_div(rows_per_page)
            .unwrap_or(0);
        self.episode_screen_for(self.page.min(last_page), rows_per_page, true)
    }

    fn reader_screen(&self) -> Screen {
        let reader = self
            .reader
            .as_ref()
            .expect("reader view without reader state");
        let loading = self.foreground_reader_task.is_some_and(|task| {
            self.reader_tasks.get(&task).is_some_and(|entry| {
                matches!(entry.purpose, ReaderTaskPurpose::ForegroundSource { .. })
            })
        });
        let chrome = if loading {
            ReadingChrome::OverlayBusy
        } else if reader.chrome_visible {
            ReadingChrome::Overlay
        } else {
            ReadingChrome::Hidden
        };
        self.reader_screen_with_chrome(chrome)
    }

    fn reader_screen_with_chrome(&self, chrome: ReadingChrome) -> Screen {
        let selection = self
            .reader_selection
            .as_ref()
            .expect("reader view without episode selection");
        let reader = self
            .reader
            .as_ref()
            .expect("reader view without reader state");
        let picture = reader.picture.expect("reader view without uploaded slice");
        ScreenBuilder::new("bomtoon-reader")
            .top_bar(selection.title.clone())
            .reading_surface(picture, chrome)
            .page_turns(READER_PREVIOUS, READER_NEXT)
            .reading_menu(READER_CHROME)
            .reading_progress(
                reader_progress_percent(
                    reader.offset_rows,
                    reader.panel_height,
                    reader.strip.total_rows,
                ),
                previous_viewport_offset(reader.offset_rows, reader.panel_height).is_some(),
                next_viewport_offset(
                    reader.offset_rows,
                    reader.panel_height,
                    reader.strip.total_rows,
                )
                .is_some(),
            )
            .build()
    }
    fn comment_context(&self) -> String {
        let episode = self
            .reader_selection
            .as_ref()
            .map_or("", |selection| selection.title.as_str());
        match (self.selected_title.is_empty(), episode.is_empty()) {
            (false, false) => format!("{} | {episode}", self.selected_title),
            (false, true) => self.selected_title.clone(),
            (true, false) => episode.to_owned(),
            (true, true) => String::new(),
        }
    }

    fn comment_metadata(comment: &Comment) -> String {
        let created_at =
            taipei_datetime(comment.created_at).unwrap_or_else(|| "Unknown date".to_owned());
        format!(
            "{created_at} | {} likes | {} replies",
            comment.like_count, comment.reply_count
        )
    }

    fn comment_appendix_screen(&self) -> Screen {
        let mut screen = ScreenBuilder::new("bomtoon-comment-appendix")
            .top_bar("Comments")
            .secondary(self.comment_context());
        screen = match &self.comment_appendix {
            CommentAppendixState::Unloaded | CommentAppendixState::Loading => {
                screen.activity("Loading comments", None)
            }
            CommentAppendixState::Failed(problem) => screen
                .banner(BannerLevel::Attention, problem.clone())
                .primary_button(RETRY, "Try again"),
            CommentAppendixState::Empty => screen.text("No comments yet"),
            CommentAppendixState::Ready {
                comments,
                total_items,
            } => {
                let rows = comments.iter().map(|comment| {
                    let (preview, _) =
                        comment_preview(&comment.text, APPENDIX_COMMENT_PREVIEW_BYTES);
                    (
                        comment.author.clone(),
                        format!("{preview}\n{}", Self::comment_metadata(comment)),
                    )
                });
                screen
                    .facts(rows)
                    .button(ALL_COMMENTS, format!("All comments ({total_items})"))
            }
        };
        screen.build()
    }

    fn comments_screen(&self) -> Screen {
        let mut screen = ScreenBuilder::new("bomtoon-comments")
            .top_bar("All comments")
            .secondary(self.comment_context());
        let Some(state) = &self.comments else {
            return screen.activity("Loading comments", None).build();
        };
        if let Some((_, problem)) = &state.error {
            screen = screen
                .banner(BannerLevel::Attention, problem.clone())
                .button(RETRY, "Try again");
        }
        let Some(comment) = state.comments.get(state.item) else {
            screen = screen.text("No visible comments on this page");
            if state.number > 0 {
                screen = screen.button(COMMENTS_PREVIOUS, "Previous comment");
            }
            if state.number.saturating_add(1) < state.total_pages {
                screen = screen.button(COMMENTS_NEXT, "Next comment");
            }
            return screen.build();
        };
        let position = state
            .number
            .saturating_mul(20)
            .saturating_add(state.item)
            .saturating_add(1)
            .min(state.total_items);
        let author = if comment.is_best() {
            format!("BEST | {}", comment.author)
        } else {
            comment.author.clone()
        };
        let (preview, truncated) = comment_preview(&comment.text, COMMENT_LIST_PREVIEW_BYTES);
        screen = screen
            .section_with_value(
                author,
                format!("{position}-{} of {}", position, state.total_items),
            )
            .secondary(Self::comment_metadata(comment))
            .text(preview);
        if comment.reply_count > 0 || truncated {
            let label = if comment.reply_count > 0 {
                format!("Replies ({})", comment.reply_count)
            } else {
                "Read full comment".to_owned()
            };
            screen = screen.button(format!("comment-{}", comment.id), label);
        }
        let has_previous = state.item > 0 || state.number > 0;
        let has_next = state.item + 1 < state.comments.len()
            || state.number.saturating_add(1) < state.total_pages;
        if has_previous {
            screen = screen.button(COMMENTS_PREVIOUS, "Previous comment");
        }
        if has_next {
            screen = screen.button(COMMENTS_NEXT, "Next comment");
        }
        screen.build()
    }

    fn replies_screen(&self) -> Screen {
        let mut screen = ScreenBuilder::new("bomtoon-replies")
            .top_bar("Replies")
            .secondary(self.comment_context());
        let Some(state) = &self.replies else {
            return screen.activity("Loading replies", None).build();
        };
        if let Some((_, problem)) = &state.error {
            screen = screen
                .banner(BannerLevel::Attention, problem.clone())
                .button(RETRY, "Try again");
        }
        if !state.show_parent && state.replies.get(state.item).is_none() {
            screen = screen.text("No visible replies on this page");
            if state.number > 0 {
                screen = screen.button(REPLIES_PREVIOUS, "Previous");
            }
            if state.number.saturating_add(1) < state.total_pages {
                screen = screen.button(REPLIES_NEXT, "Next");
            }
            return screen.build();
        }
        let (comment, section, position) = if state.show_parent {
            (&state.parent, "Parent comment".to_owned(), None)
        } else if let Some(reply) = state.replies.get(state.item) {
            let position = state
                .number
                .saturating_mul(10)
                .saturating_add(state.item)
                .saturating_add(1)
                .min(state.total_items);
            (
                reply,
                "Reply".to_owned(),
                Some(format!("{position}-{} of {}", position, state.total_items)),
            )
        } else {
            unreachable!("visible reply disappeared")
        };
        let (text, text_pages) = comment_detail_page(&comment.text, state.text_page)
            .expect("reply text page is out of range");
        let value = position.map_or_else(
            || format!("Part {} of {text_pages}", state.text_page.saturating_add(1)),
            |position| {
                format!(
                    "{position} | Part {} of {text_pages}",
                    state.text_page.saturating_add(1)
                )
            },
        );
        screen = screen
            .section_with_value(section, value)
            .heading(comment.author.clone())
            .secondary(Self::comment_metadata(comment))
            .text(text.to_owned());
        if state.show_parent
            && state.text_page.saturating_add(1) == text_pages
            && state.replies.is_empty()
            && state.total_items == 0
            && state.error.is_none()
        {
            screen = screen.text("No replies yet");
        }
        let has_previous = state.text_page > 0 || !state.show_parent || state.number > 0;
        let has_next = state.text_page.saturating_add(1) < text_pages
            || (state.show_parent
                && (!state.replies.is_empty()
                    || state.number.saturating_add(1) < state.total_pages))
            || (!state.show_parent
                && (state.item + 1 < state.replies.len()
                    || state.number.saturating_add(1) < state.total_pages));
        if has_previous {
            screen = screen.button(REPLIES_PREVIOUS, "Previous");
        }
        if has_next {
            screen = screen.button(REPLIES_NEXT, "Next");
        }
        screen.build()
    }

    fn cancel_reader(&mut self, context: &mut Context) {
        self.reader_manifest_pending = false;
        self.reader_generation = self.reader_generation.wrapping_add(1);
        for task in std::mem::take(&mut self.reader_tasks).into_keys() {
            context.cancel(task);
        }
        self.foreground_reader_task = None;
        let picture = self
            .reader
            .take()
            .and_then(|reader| reader.picture)
            .map(|picture| picture.handle);
        if let Some(handle) = picture {
            context.drop_picture(handle);
        }
    }

    fn current_commerce_scope(&self) -> Option<commerce::AccountScope> {
        match (self.account, self.connection, self.account_scope) {
            (AccountState::Active, ConnectionState::Online, Some(scope)) => Some(scope),
            _ => None,
        }
    }

    fn cancel_title_gift_task(&mut self, context: &mut Context) {
        self.gifts.generation = self.gifts.generation.wrapping_add(1);
        if let Some(task) = self.gifts.task.take() {
            context.cancel(task.id);
        }
    }

    fn clear_title_gifts(&mut self, context: &mut Context) {
        self.cancel_title_gift_task(context);
        self.gifts.title_id = None;
        self.gifts.available = None;
        self.gifts.error = false;
    }

    fn refresh_title_gifts(&mut self, context: &mut Context, purpose: GiftTaskPurpose) {
        let Some(title_id) = self.selected_content_id else {
            self.gifts.error = true;
            return;
        };
        self.refresh_title_gifts_for(context, title_id, purpose);
    }

    fn refresh_title_gifts_for(
        &mut self,
        context: &mut Context,
        title_id: usize,
        purpose: GiftTaskPurpose,
    ) -> Option<u64> {
        let Some(account_scope) = self.current_commerce_scope() else {
            self.gifts.error = true;
            return None;
        };
        self.cancel_title_gift_task(context);
        if self.gifts.title_id != Some(title_id) {
            self.gifts.available = None;
        }
        self.gifts.title_id = Some(title_id);
        self.gifts.error = false;
        let generation = self.gifts.generation;
        let Some(id) = context.spawn(api::title_gifts(title_id)) else {
            self.gifts.error = true;
            return None;
        };
        self.gifts.task = Some(GiftTask {
            id,
            generation,
            title_id,
            account_scope,
            purpose,
        });
        Some(generation)
    }

    fn handle_title_gift_outcome(
        &mut self,
        context: &mut Context,
        task: GiftTask,
        outcome: TaskOutcome,
    ) {
        if task.generation != self.gifts.generation
            || self.gifts.title_id != Some(task.title_id)
            || self.account_scope != Some(task.account_scope)
        {
            return;
        }
        if matches!(outcome, TaskOutcome::Failed(TaskError::NoCredential)) {
            self.finish_credential_loss(context, AccountState::SignedOut);
            return;
        }
        if matches!(outcome, TaskOutcome::Failed(TaskError::Unauthorized)) {
            self.finish_credential_loss(context, AccountState::Expired);
            return;
        }
        let observed = match outcome {
            TaskOutcome::Completed(bytes)
                if self.current_commerce_scope() == Some(task.account_scope) =>
            {
                if let Ok(balance) = parse::gift_balance(&bytes) {
                    self.gifts.available = Some(balance.available);
                    self.gifts.error = false;
                    Ok(balance.available)
                } else {
                    self.gifts.error = true;
                    Err(())
                }
            }
            TaskOutcome::Completed(_) | TaskOutcome::Failed(_) => {
                self.gifts.error = true;
                Err(())
            }
            TaskOutcome::Cancelled => Err(()),
        };
        match task.purpose {
            GiftTaskPurpose::Display => {}
            GiftTaskPurpose::Reconcile { generation } => {
                if let Some(reconciliation) = self.reconciliation.as_mut() {
                    if reconciliation.generation == generation
                        && reconciliation.gift_generation == Some(task.generation)
                        && reconciliation.account_scope == task.account_scope
                    {
                        reconciliation.gifts = observed.map_or(Evidence::Failed, Evidence::Value);
                    }
                }
            }
        }
        self.finish_reconciliation(context);
        self.show(context);
    }

    fn cancel_account_history(&mut self, context: &mut Context) {
        self.wallet.detail_generation = self.wallet.detail_generation.wrapping_add(1);
        self.wallet.detail_queue.clear();
        let history_tasks = self
            .wallet
            .tasks
            .iter()
            .filter_map(|(task, purpose)| purpose.history().map(|_| *task))
            .collect::<Vec<_>>();
        for task in history_tasks {
            self.wallet.tasks.remove(&task);
            context.cancel(task);
        }
    }

    fn cancel_wallet(&mut self, context: &mut Context) {
        self.wallet.summary_generation = self.wallet.summary_generation.wrapping_add(1);
        self.wallet.detail_generation = self.wallet.detail_generation.wrapping_add(1);
        self.wallet.detail_queue.clear();
        for task in std::mem::take(&mut self.wallet.tasks).into_keys() {
            context.cancel(task);
        }
        self.wallet.summary_task = None;
        self.wallet.summary_refresh_queued = false;
    }

    fn refresh_asset_summary(&mut self, context: &mut Context) {
        if self.view.is_reader_flow() {
            self.wallet.summary_refresh_queued = true;
            return;
        }
        self.wallet.summary_refresh_queued = false;
        let Some(generation) = self.wallet.request_summary_generation() else {
            return;
        };
        let Some(task) = context.spawn(api::asset_summary()) else {
            self.wallet.summary_refresh_queued = true;
            return;
        };
        self.wallet.summary_error = false;
        self.wallet.summary_task = Some(task);
        self.wallet
            .tasks
            .insert(task, WalletTaskPurpose::Summary { generation });
    }

    fn resume_deferred_summary(&mut self, context: &mut Context) {
        if !self.view.is_reader_flow() && self.wallet.summary_refresh_queued {
            self.refresh_asset_summary(context);
        }
    }

    fn set_history_error(&mut self, kind: AssetKind, error: bool) {
        match kind {
            AssetKind::Coin => self.wallet.coin_history_error = error,
            AssetKind::Ticket => self.wallet.ticket_history_error = error,
        }
    }

    fn queue_history(&mut self, generation: u64, kind: AssetKind, start_ms: i64) {
        let purpose = match kind {
            AssetKind::Coin => WalletTaskPurpose::CoinHistory { generation },
            AssetKind::Ticket => WalletTaskPurpose::TicketHistory { generation },
        };
        let already_requested = self
            .wallet
            .detail_queue
            .iter()
            .any(|(queued, _)| *queued == purpose)
            || self.wallet.tasks.values().any(|active| *active == purpose);
        if already_requested {
            return;
        }
        self.set_history_error(kind, false);
        self.wallet.detail_queue.push_back((purpose, start_ms));
    }

    fn drain_deferred_details(&mut self, context: &mut Context) -> bool {
        let mut error_changed = false;
        while let Some((purpose, start_ms)) = self.wallet.detail_queue.front().copied() {
            let Some((kind, generation)) = purpose.history() else {
                self.wallet.detail_queue.pop_front();
                continue;
            };
            if generation != self.wallet.detail_generation {
                self.wallet.detail_queue.pop_front();
                continue;
            }
            let work = api::expiration_history(kind, start_ms);
            if !work.is_sendable() {
                self.wallet.detail_queue.pop_front();
                self.set_history_error(kind, true);
                error_changed = true;
                continue;
            }
            let Some(task) = context.spawn(work) else {
                break;
            };
            self.wallet.detail_queue.pop_front();
            self.wallet.tasks.insert(task, purpose);
        }
        error_changed
    }

    fn resume_deferred_wallet(&mut self, context: &mut Context) {
        self.resume_deferred_summary(context);
        if self.drain_deferred_details(context) {
            self.show(context);
        }
    }

    fn refresh_account_details(&mut self, context: &mut Context) {
        self.refresh_account_details_from(context, history_start_ms());
    }

    fn refresh_account_details_from(&mut self, context: &mut Context, start_ms: Option<i64>) {
        self.wallet.detail_generation = self.wallet.detail_generation.wrapping_add(1);
        let generation = self.wallet.detail_generation;
        self.wallet.detail_queue.clear();
        let old_details = self
            .wallet
            .tasks
            .iter()
            .filter_map(|(task, purpose)| {
                matches!(
                    purpose,
                    WalletTaskPurpose::CoinHistory { .. } | WalletTaskPurpose::TicketHistory { .. }
                )
                .then_some(*task)
            })
            .collect::<Vec<_>>();
        for task in old_details {
            self.wallet.tasks.remove(&task);
            context.cancel(task);
        }
        let Some(start_ms) = start_ms else {
            self.wallet.coin_history_error = true;
            self.wallet.ticket_history_error = true;
            return;
        };
        self.queue_history(generation, AssetKind::Coin, start_ms);
        self.queue_history(generation, AssetKind::Ticket, start_ms);
        self.drain_deferred_details(context);
    }

    fn retry_account_balances(&mut self, context: &mut Context) {
        self.refresh_asset_summary(context);
        let retry_coin = self.wallet.coin_history_error;
        let retry_ticket = self.wallet.ticket_history_error;
        if !retry_coin && !retry_ticket {
            return;
        }
        let Some(start_ms) = history_start_ms() else {
            return;
        };
        let generation = self.wallet.detail_generation;
        if retry_coin {
            self.queue_history(generation, AssetKind::Coin, start_ms);
        }
        if retry_ticket {
            self.queue_history(generation, AssetKind::Ticket, start_ms);
        }
        self.drain_deferred_details(context);
    }

    fn open_account(&mut self, context: &mut Context) {
        self.page = 0;
        self.featured.feed_page = 0;
        self.problem = None;
        self.view = View::Account;
        self.refresh_asset_summary(context);
        self.refresh_account_details(context);
        self.show(context);
    }

    fn clamp_account_history_page(&mut self) {
        if self.view != View::Account {
            return;
        }
        let row_count = self
            .wallet
            .coin_history
            .len()
            .saturating_add(self.wallet.ticket_history.len());
        let last_page = row_count
            .saturating_sub(1)
            .checked_div(ACCOUNT_HISTORY_ITEMS_PER_PAGE)
            .unwrap_or(0);
        self.page = self.page.min(last_page);
    }

    fn owns_public_cover_url(&self, url: &str) -> bool {
        self.featured.snapshot().is_some_and(|snapshot| {
            snapshot
                .banners
                .iter()
                .chain(
                    snapshot
                        .collections
                        .iter()
                        .flat_map(|collection| collection.comics.iter()),
                )
                .any(|comic| {
                    comic.vertical_url.as_deref() == Some(url)
                        || comic.square_url.as_deref() == Some(url)
                })
        })
    }

    fn cover_url_owned(&self, source: CoverSource, url: &str) -> bool {
        match source {
            CoverSource::Public => self.owns_public_cover_url(url),
            CoverSource::Protected => {
                self.account == AccountState::Active
                    && self.covers.visible_source == Some(CoverSource::Protected)
                    && self
                        .covers
                        .visible_urls
                        .iter()
                        .any(|visible| visible == url)
            }
        }
    }

    fn is_episode_thumbnail_url(&self, url: &str) -> bool {
        self.episodes
            .iter()
            .any(|episode| episode.thumbnail_url.as_deref() == Some(url))
    }

    fn episode_thumbnail_urls(&self) -> BTreeSet<String> {
        self.episodes
            .iter()
            .filter_map(|episode| episode.thumbnail_url.clone())
            .collect()
    }

    fn evict_episode_thumbnails(
        &mut self,
        context: &mut Context,
        retained: &BTreeSet<String>,
        episode_urls: &BTreeSet<String>,
        public_urls: &BTreeSet<String>,
    ) {
        let obsolete_tasks = self
            .covers
            .tasks
            .iter()
            .filter(|(_, task)| episode_urls.contains(&task.url) && !retained.contains(&task.url))
            .map(|(task, cover)| (*task, cover.url.clone()))
            .collect::<Vec<_>>();
        for (task, url) in obsolete_tasks {
            context.cancel(task);
            self.covers.tasks.remove(&task);
            if self.covers.entries.get(&url) == Some(&CoverState::Loading(task)) {
                self.covers.entries.remove(&url);
            }
        }

        let obsolete_entries = self
            .covers
            .entries
            .keys()
            .filter(|url| {
                episode_urls.contains(*url)
                    && !retained.contains(*url)
                    && !public_urls.contains(*url)
            })
            .cloned()
            .collect::<Vec<_>>();
        for url in obsolete_entries {
            if let Some(CoverState::Ready(picture)) = self.covers.entries.remove(&url) {
                context.drop_picture(picture.handle);
            }
        }

        self.covers
            .visible_urls
            .retain(|url| !episode_urls.contains(url) || retained.contains(url));
    }
    fn clear_episode_thumbnail_cache(&mut self, context: &mut Context) {
        let episode_urls = self.episode_thumbnail_urls();
        if episode_urls.is_empty() {
            return;
        }
        let public_urls = self.public_cover_urls();
        self.covers.generation = self.covers.generation.wrapping_add(1);
        let generation = self.covers.generation;
        self.evict_episode_thumbnails(context, &BTreeSet::new(), &episode_urls, &public_urls);
        for task in self.covers.tasks.values_mut() {
            task.generation = generation;
            if public_urls.contains(&task.url) {
                task.source = CoverSource::Public;
            }
        }
        if self.covers.visible_urls.is_empty() {
            self.covers.visible_source = None;
        }
    }
    fn public_cover_urls(&self) -> BTreeSet<String> {
        self.featured
            .snapshot()
            .into_iter()
            .flat_map(|snapshot| {
                snapshot.banners.iter().chain(
                    snapshot
                        .collections
                        .iter()
                        .flat_map(|collection| collection.comics.iter()),
                )
            })
            .flat_map(|comic| {
                comic
                    .vertical_url
                    .iter()
                    .chain(comic.square_url.iter())
                    .cloned()
            })
            .collect()
    }

    fn retain_public_cover_cache(&mut self, context: &mut Context) {
        let public_urls = self.public_cover_urls();
        self.covers.generation = self.covers.generation.wrapping_add(1);
        let generation = self.covers.generation;
        let protected_tasks = self
            .covers
            .tasks
            .iter()
            .filter(|(_, task)| {
                task.source == CoverSource::Protected && !public_urls.contains(&task.url)
            })
            .map(|(task, cover)| (*task, cover.url.clone()))
            .collect::<Vec<_>>();
        for (task, url) in protected_tasks {
            context.cancel(task);
            self.covers.tasks.remove(&task);
            if self.covers.entries.get(&url) == Some(&CoverState::Loading(task)) {
                self.covers.entries.remove(&url);
            }
        }
        for task in self.covers.tasks.values_mut() {
            task.generation = generation;
            if public_urls.contains(&task.url) {
                task.source = CoverSource::Public;
            }
        }

        let entries = std::mem::take(&mut self.covers.entries);
        for (url, state) in entries {
            let retained_task = match state {
                CoverState::Loading(task) => self.covers.tasks.contains_key(&task),
                CoverState::Ready(_) | CoverState::Failed => false,
            };
            if public_urls.contains(&url) || retained_task {
                self.covers.entries.insert(url, state);
            } else if let CoverState::Ready(picture) = state {
                context.drop_picture(picture.handle);
            }
        }
        self.covers
            .visible_urls
            .retain(|url| self.covers.entries.contains_key(url));
        self.covers.visible_source =
            (!self.covers.visible_urls.is_empty()).then_some(CoverSource::Public);
    }

    fn authentication(&self) -> commerce::Authentication {
        match (self.account, self.account_scope) {
            (AccountState::Active, Some(scope)) => commerce::Authentication::Authenticated(scope),
            (AccountState::SignedOut | AccountState::RevocationUnconfirmed, _) => {
                commerce::Authentication::SignedOut
            }
            (AccountState::Expired, _) => commerce::Authentication::Expired,
            (AccountState::Checking | AccountState::Active, _) => commerce::Authentication::Unknown,
        }
    }

    fn connectivity(&self) -> commerce::Connectivity {
        match self.connection {
            ConnectionState::Unknown => commerce::Connectivity::Unknown,
            ConnectionState::Online => commerce::Connectivity::Online,
            ConnectionState::Offline => commerce::Connectivity::Offline,
        }
    }

    fn start_reconciliation(
        &mut self,
        context: &mut Context,
        selection: &commerce::Selection,
        refresh_wallet: bool,
        refresh_gifts: bool,
    ) {
        let Some(account_scope) = self.current_commerce_scope() else {
            return;
        };
        self.commerce_generation = self.commerce_generation.wrapping_add(1);
        let generation = self.commerce_generation;
        let marker = self.commerce.reconciliation_marker().cloned();
        self.reconciliation = Some(ReconciliationState {
            generation,
            account_scope,
            marker,
            post_accepted: std::mem::take(&mut self.reconciliation_post_accepted),
            content: Evidence::Pending,
            wallet: if refresh_wallet {
                Evidence::Pending
            } else {
                Evidence::NotRequired
            },
            gifts: if refresh_gifts {
                Evidence::Pending
            } else {
                Evidence::NotRequired
            },
            wallet_generation: None,
            gift_generation: None,
        });

        if let Some(id) = context.spawn(api::detail(&selection.title_alias)) {
            self.commerce_task = Some(CommerceTask {
                id,
                purpose: CommerceTaskPurpose::ReconcileContent {
                    generation,
                    account_scope,
                    selection: Clone::clone(selection),
                },
            });
        } else if let Some(reconciliation) = self.reconciliation.as_mut() {
            reconciliation.content = Evidence::Failed;
        }

        if refresh_wallet {
            if let Some(active) = self.wallet.summary_task.take() {
                self.wallet.tasks.remove(&active);
                context.cancel(active);
            }
            self.wallet.summary_refresh_queued = false;
            self.wallet.summary_generation = self.wallet.summary_generation.wrapping_add(1);
            let wallet_generation = self.wallet.summary_generation;
            if let Some(id) = context.spawn(api::asset_summary()) {
                self.wallet.summary_task = Some(id);
                self.wallet.tasks.insert(
                    id,
                    WalletTaskPurpose::Summary {
                        generation: wallet_generation,
                    },
                );
                if let Some(reconciliation) = self.reconciliation.as_mut() {
                    reconciliation.wallet_generation = Some(wallet_generation);
                }
            } else if let Some(reconciliation) = self.reconciliation.as_mut() {
                reconciliation.wallet = Evidence::Failed;
            }
        }

        if refresh_gifts {
            let gift_generation = self.refresh_title_gifts_for(
                context,
                selection.title_id,
                GiftTaskPurpose::Reconcile { generation },
            );
            if let Some(reconciliation) = self.reconciliation.as_mut() {
                if let Some(gift_generation) = gift_generation {
                    reconciliation.gift_generation = Some(gift_generation);
                } else {
                    reconciliation.gifts = Evidence::Failed;
                }
            }
        }
        self.finish_reconciliation(context);
    }

    fn content_evidence(
        marker: Option<&commerce::UnresolvedMutationV1>,
        episodes: &[Episode],
    ) -> ContentEvidence {
        let Some(marker) = marker else {
            return ContentEvidence::Entitled;
        };
        let Some(episode) = episodes.iter().find(|episode| {
            episode.id == marker.episode_id && episode.alias == marker.episode_alias
        }) else {
            return ContentEvidence::NotEntitled;
        };
        let expected = match marker.purchase_type {
            model::PurchaseType::RentGift | model::PurchaseType::Rent => {
                episode.purchase == model::PurchaseState::Rented
            }
            model::PurchaseType::Possession => episode.purchase == model::PurchaseState::Owned,
        };
        if expected {
            ContentEvidence::Entitled
        } else if episode.purchase == model::PurchaseState::NotOwned {
            ContentEvidence::NotEntitled
        } else {
            ContentEvidence::Contradictory
        }
    }

    fn handle_reconciliation_content(
        &mut self,
        context: &mut Context,
        generation: u64,
        account_scope: commerce::AccountScope,
        selection: &commerce::Selection,
        outcome: TaskOutcome,
    ) {
        let matching_reconciliation = self.reconciliation.as_ref().is_some_and(|reconciliation| {
            reconciliation.generation == generation && reconciliation.account_scope == account_scope
        });
        if !matching_reconciliation {
            return;
        }
        if matches!(&outcome, TaskOutcome::Failed(TaskError::NoCredential)) {
            self.finish_credential_loss(context, AccountState::SignedOut);
            return;
        }
        if matches!(&outcome, TaskOutcome::Failed(TaskError::Unauthorized)) {
            self.finish_credential_loss(context, AccountState::Expired);
            return;
        }
        let reconciliation = self
            .reconciliation
            .as_ref()
            .expect("matching reconciliation disappeared");
        let result = match outcome {
            TaskOutcome::Completed(bytes)
                if self.current_commerce_scope() == Some(account_scope) =>
            {
                match parse::content_detail(&bytes, &selection.title_alias) {
                    Ok(detail) if detail.id == selection.title_id => {
                        let evidence = Self::content_evidence(
                            reconciliation.marker.as_ref(),
                            &detail.episodes,
                        );
                        if self.selected_content_id == Some(selection.title_id)
                            && self.selected_content_alias == selection.title_alias
                        {
                            let episode_anchor = self.visible_episode_anchor();
                            let episode_page = self.page;
                            self.synopsis = SynopsisState {
                                pages: synopsis_pages(&detail.synopsis),
                                open_page: None,
                                ..SynopsisState::default()
                            };
                            self.selected_title = detail.title;
                            self.selected_creators = detail.creators.join(" | ");
                            self.selected_synopsis = detail.synopsis;
                            self.clear_episode_thumbnail_cache(context);
                            self.episodes = detail.episodes;
                            self.prepare_episode_layout();
                            self.restore_episode_page(episode_anchor, episode_page);
                        }
                        Evidence::Value(evidence)
                    }
                    Ok(_) | Err(_) => Evidence::Failed,
                }
            }
            TaskOutcome::Completed(_) | TaskOutcome::Failed(_) | TaskOutcome::Cancelled => {
                Evidence::Failed
            }
        };
        if let Some(reconciliation) = self.reconciliation.as_mut() {
            if reconciliation.generation == generation {
                reconciliation.content = result;
            }
        }
        self.finish_reconciliation(context);
    }

    fn finish_reconciliation(&mut self, context: &mut Context) {
        let Some(reconciliation) = self.reconciliation.as_ref() else {
            return;
        };
        if !reconciliation.content.settled()
            || !reconciliation.wallet.settled()
            || !reconciliation.gifts.settled()
        {
            return;
        }
        let reconciliation = self
            .reconciliation
            .take()
            .expect("settled reconciliation disappeared");
        let conclusive = match (
            reconciliation.marker.as_ref(),
            &reconciliation.content,
            &reconciliation.wallet,
            &reconciliation.gifts,
        ) {
            (None, Evidence::Value(_), Evidence::NotRequired, Evidence::NotRequired) => true,
            (Some(marker), Evidence::Value(entitlement), wallet, gifts) => {
                let (spent, unchanged) = match marker.purchase_type {
                    model::PurchaseType::RentGift => {
                        let current = match gifts {
                            Evidence::Value(current) => Some(*current),
                            Evidence::NotRequired | Evidence::Pending | Evidence::Failed => None,
                        };
                        (
                            marker
                                .pre_mutation_title_gifts
                                .and_then(|before| before.checked_sub(1))
                                .zip(current)
                                .is_some_and(|(expected, current)| expected == current),
                            marker
                                .pre_mutation_title_gifts
                                .zip(current)
                                .is_some_and(|(before, current)| before == current),
                        )
                    }
                    model::PurchaseType::Rent | model::PurchaseType::Possession => {
                        let current = match wallet {
                            Evidence::Value(current) => Some(*current),
                            Evidence::NotRequired | Evidence::Pending | Evidence::Failed => None,
                        };
                        (
                            marker
                                .pre_mutation_spendable_coin
                                .and_then(|before| before.checked_sub(marker.quoted_price))
                                .zip(current)
                                .is_some_and(|(expected, current)| expected == current),
                            marker
                                .pre_mutation_spendable_coin
                                .zip(current)
                                .is_some_and(|(before, current)| before == current),
                        )
                    }
                };
                match entitlement {
                    ContentEvidence::Entitled => spent,
                    ContentEvidence::NotEntitled => !reconciliation.post_accepted && unchanged,
                    ContentEvidence::Contradictory => false,
                }
            }
            (
                Some(_) | None,
                Evidence::NotRequired | Evidence::Pending | Evidence::Failed,
                _,
                _,
            )
            | (None, Evidence::Value(_), _, _) => false,
        };
        if conclusive
            && reconciliation.marker.as_ref().is_some_and(|marker| {
                marker.purchase_type == model::PurchaseType::Possession
                    && reconciliation.content == Evidence::Value(ContentEvidence::Entitled)
            })
        {
            self.library_load.loaded = false;
        }
        let effects = self.commerce.reconciled(
            reconciliation.account_scope,
            if conclusive {
                commerce::Reconciliation::Conclusive
            } else {
                commerce::Reconciliation::Incomplete
            },
        );
        self.apply_commerce_effects(context, effects);
    }

    fn observe_reconciliation_wallet(&mut self, purpose: WalletTaskPurpose, outcome: &TaskOutcome) {
        let Some(reconciliation) = self.reconciliation.as_ref() else {
            return;
        };
        let Some(expected_generation) = reconciliation.wallet_generation else {
            return;
        };
        if purpose
            != (WalletTaskPurpose::Summary {
                generation: expected_generation,
            })
        {
            return;
        }
        let account_scope = reconciliation.account_scope;
        let evidence = match outcome {
            TaskOutcome::Completed(bytes)
                if self.current_commerce_scope() == Some(account_scope) =>
            {
                parse::asset_summary(bytes)
                    .ok()
                    .and_then(|summary| summary.coins.total())
                    .map_or(Evidence::Failed, Evidence::Value)
            }
            TaskOutcome::Completed(_) | TaskOutcome::Failed(_) | TaskOutcome::Cancelled => {
                Evidence::Failed
            }
        };
        if let Some(reconciliation) = self.reconciliation.as_mut() {
            if reconciliation.wallet_generation == Some(expected_generation) {
                reconciliation.wallet = evidence;
            }
        }
    }
    fn try_spawn_commerce_command(
        &mut self,
        context: &mut Context,
        command: commerce::CommerceCommand,
    ) -> Option<commerce::CommerceCommand> {
        match command {
            commerce::CommerceCommand::FetchQuote {
                selection,
                purchase,
            } => {
                let Some(account_scope) = self.current_commerce_scope() else {
                    let effects = self.commerce.quote_failed();
                    self.apply_commerce_effects(context, effects);
                    return None;
                };
                let work = api::quote(&selection.title_alias, &selection.episode_alias, purchase);
                let Some(id) = context.spawn(work) else {
                    return Some(commerce::CommerceCommand::FetchQuote {
                        selection,
                        purchase,
                    });
                };
                self.commerce_generation = self.commerce_generation.wrapping_add(1);
                self.commerce_task = Some(CommerceTask {
                    id,
                    purpose: CommerceTaskPurpose::Quote {
                        generation: self.commerce_generation,
                        account_scope,
                        selection,
                        purchase,
                    },
                });
                None
            }
            commerce::CommerceCommand::Post(marker) => {
                let work =
                    api::purchase(&marker.title_alias, marker.episode_id, marker.purchase_type);
                let Some(id) = context.spawn(work) else {
                    return Some(commerce::CommerceCommand::Post(marker));
                };
                self.commerce_generation = self.commerce_generation.wrapping_add(1);
                self.commerce_task = Some(CommerceTask {
                    id,
                    purpose: CommerceTaskPurpose::Post {
                        generation: self.commerce_generation,
                        account_scope: marker.account_scope,
                        marker,
                    },
                });
                None
            }
            commerce::CommerceCommand::SaveMarker(_)
            | commerce::CommerceCommand::ForgetMarker
            | commerce::CommerceCommand::RefreshContent(_) => {
                unreachable!("only network commerce commands can be deferred")
            }
        }
    }

    fn resume_queued_commerce(&mut self, context: &mut Context) {
        if self.commerce_task.is_some() {
            return;
        }
        let Some(command) = self.queued_commerce_command.take() else {
            return;
        };
        if let Some(command) = self.try_spawn_commerce_command(context, command) {
            self.queued_commerce_command = Some(command);
        }
    }

    fn apply_commerce_effects(
        &mut self,
        context: &mut Context,
        effects: commerce::CommerceEffects,
    ) {
        if self.commerce.state() == commerce::CommerceState::Idle {
            self.queued_commerce_command = None;
            self.retained_quote = None;
            self.commerce_episode = None;
        }
        let redraw = effects.redraw;
        let refresh_wallet = effects.refresh_wallet;
        let refresh_gifts = effects.refresh_gifts;
        if self.marker_store.is_none() {
            match effects.command {
                Some(commerce::CommerceCommand::SaveMarker(value)) => {
                    self.marker_store = Some(MarkerStoreOperation::Save);
                    context.store().save(commerce::MARKER_KEY, value);
                }
                Some(commerce::CommerceCommand::ForgetMarker) => {
                    self.marker_store = Some(MarkerStoreOperation::Forget);
                    context.store().forget(commerce::MARKER_KEY);
                }
                Some(
                    command @ (commerce::CommerceCommand::FetchQuote { .. }
                    | commerce::CommerceCommand::Post(_)),
                ) => {
                    let preempting_feature = self.preempt_feature_tasks_for_commerce(context);
                    if self.covers.tasks.is_empty() && !preempting_feature {
                        if let Some(command) = self.try_spawn_commerce_command(context, command) {
                            let effects = match command {
                                commerce::CommerceCommand::FetchQuote { .. } => {
                                    self.commerce.quote_failed()
                                }
                                commerce::CommerceCommand::Post(_) => self
                                    .commerce
                                    .mutation_finished(commerce::PostOutcome::Ambiguous),
                                commerce::CommerceCommand::SaveMarker(_)
                                | commerce::CommerceCommand::ForgetMarker
                                | commerce::CommerceCommand::RefreshContent(_) => {
                                    unreachable!("only network commands reach the task scheduler")
                                }
                            };
                            self.apply_commerce_effects(context, effects);
                            return;
                        }
                    } else {
                        self.preempt_cover_tasks(context);
                        if let Some(command) = self.try_spawn_commerce_command(context, command) {
                            self.queued_commerce_command = Some(command);
                        }
                    }
                }
                Some(commerce::CommerceCommand::RefreshContent(selection)) => {
                    self.start_reconciliation(context, &selection, refresh_wallet, refresh_gifts);
                }
                None => {}
            }
        }
        if matches!(
            self.commerce.state(),
            commerce::CommerceState::LoadingSafetyState
                | commerce::CommerceState::Idle
                | commerce::CommerceState::AcceptedButStale
        ) && self.feature_capacity_state == FeatureCapacityState::ResumeAfterCommerce
        {
            self.feature_capacity_state = FeatureCapacityState::Ready;
            self.resume_capacity_work(context);
        }
        if redraw {
            self.show(context);
        }
    }

    fn handle_commerce_task(
        &mut self,
        context: &mut Context,
        task: CommerceTask,
        outcome: TaskOutcome,
    ) {
        let purpose = match task.purpose {
            CommerceTaskPurpose::ReconcileContent {
                generation,
                account_scope,
                selection,
            } => {
                if generation == self.commerce_generation {
                    self.handle_reconciliation_content(
                        context,
                        generation,
                        account_scope,
                        &selection,
                        outcome,
                    );
                }
                return;
            }
            purpose => purpose,
        };
        let (generation, account_scope) = match &purpose {
            CommerceTaskPurpose::Quote {
                generation,
                account_scope,
                ..
            }
            | CommerceTaskPurpose::Post {
                generation,
                account_scope,
                ..
            } => (*generation, *account_scope),
            CommerceTaskPurpose::ReconcileContent { .. } => unreachable!(),
        };
        if generation != self.commerce_generation
            || self.current_commerce_scope() != Some(account_scope)
        {
            return;
        }
        if matches!(outcome, TaskOutcome::Failed(TaskError::NoCredential)) {
            self.finish_credential_loss(context, AccountState::SignedOut);
            return;
        }
        if matches!(outcome, TaskOutcome::Failed(TaskError::Unauthorized)) {
            self.finish_credential_loss(context, AccountState::Expired);
            return;
        }
        let effects = match purpose {
            CommerceTaskPurpose::Quote { selection, .. } => {
                self.handle_quote_outcome(&selection, outcome)
            }
            CommerceTaskPurpose::Post { marker, .. } => self.handle_post_outcome(&marker, outcome),
            CommerceTaskPurpose::ReconcileContent { .. } => unreachable!(),
        };
        self.apply_commerce_effects(context, effects);
    }

    fn handle_quote_outcome(
        &mut self,
        selection: &commerce::Selection,
        outcome: TaskOutcome,
    ) -> commerce::CommerceEffects {
        let selection_is_current = self.selected_content_id == Some(selection.title_id)
            && self.selected_content_alias == selection.title_alias
            && self.episodes.iter().any(|episode| {
                episode.id == selection.episode_id && episode.alias == selection.episode_alias
            });
        match outcome {
            TaskOutcome::Completed(bytes) if selection_is_current => match parse::quote(&bytes) {
                Ok(quote) => {
                    let spendable = self
                        .wallet
                        .summary
                        .and_then(|summary| summary.coins.total());
                    let active_rental = self.episodes.iter().any(|episode| {
                        episode.id == selection.episode_id
                            && episode.alias == selection.episode_alias
                            && episode.purchase == model::PurchaseState::Rented
                    });
                    let title_gifts = (!self.gifts.error
                        && self.gifts.task.is_none()
                        && self.gifts.title_id == Some(selection.title_id))
                    .then_some(self.gifts.available)
                    .flatten();
                    self.commerce
                        .quote_received(quote, spendable, title_gifts, active_rental)
                }
                Err(_) => self.commerce.quote_failed(),
            },
            TaskOutcome::Completed(_) | TaskOutcome::Failed(_) | TaskOutcome::Cancelled => {
                self.commerce.quote_failed()
            }
        }
    }

    fn handle_post_outcome(
        &mut self,
        marker: &commerce::UnresolvedMutationV1,
        outcome: TaskOutcome,
    ) -> commerce::CommerceEffects {
        let (outcome, accepted) = match outcome {
            TaskOutcome::Completed(bytes)
                if parse::purchase_receipt(&bytes).is_ok_and(|receipt| {
                    receipt.purchase_type == marker.purchase_type
                        && receipt.content_alias == marker.title_alias
                        && receipt.episode_alias == marker.episode_alias
                        && receipt.coin_use.aggregate == marker.quoted_price
                }) =>
            {
                (commerce::PostOutcome::Accepted, true)
            }
            TaskOutcome::Completed(bytes) => {
                if let Some(result) = parse::purchase_rejection_result(&bytes) {
                    self.pending_purchase_rejection = Some(result);
                    (commerce::PostOutcome::ExplicitRejection, false)
                } else {
                    (commerce::PostOutcome::Ambiguous, false)
                }
            }
            TaskOutcome::Failed(_) | TaskOutcome::Cancelled => {
                (commerce::PostOutcome::Ambiguous, false)
            }
        };
        self.reconciliation_post_accepted = accepted;
        self.commerce.mutation_finished(outcome)
    }

    fn update_commerce_safety(&mut self, context: &mut Context) {
        let effects = self
            .commerce
            .safety_changed(self.authentication(), self.connectivity());
        self.apply_commerce_effects(context, effects);
    }
    fn clear_commerce_access(&mut self, context: &mut Context) {
        if let Some(task) = self.scope_task.take() {
            context.cancel(task);
        }
        if let Some(task) = self.commerce_task.take() {
            context.cancel(task.id);
        }
        self.commerce_generation = self.commerce_generation.wrapping_add(1);
        self.queued_commerce_command = None;
        self.reconciliation = None;
        self.scope_refresh_pending = false;
        self.account_scope = None;
        self.connection = ConnectionState::Unknown;
        let _ = self.commerce.safety_changed(
            commerce::Authentication::Unknown,
            commerce::Connectivity::Unknown,
        );
    }

    fn begin_commerce_safety(&mut self, context: &mut Context) {
        self.account = AccountState::Checking;
        self.connection = ConnectionState::Unknown;
        self.scope_refresh_pending = true;
        self.account_scope = None;
        self.commerce = commerce::Commerce::new();
        self.marker_store = Some(MarkerStoreOperation::Load);
        context.store().load(commerce::MARKER_KEY);
        if let Some(task) = context.spawn(api::account_scope()) {
            self.scope_task = Some(task);
            self.scope_refresh_pending = false;
        }
    }

    fn request_commerce_scope(&mut self, context: &mut Context) {
        if self.scope_task.is_some() {
            return;
        }
        self.account_scope = None;
        self.connection = ConnectionState::Unknown;
        let _ = self.commerce.safety_changed(
            commerce::Authentication::Unknown,
            commerce::Connectivity::Unknown,
        );
        self.scope_refresh_pending = true;
        if let Some(task) = context.spawn(api::account_scope()) {
            self.scope_task = Some(task);
            self.scope_refresh_pending = false;
        }
    }
    fn finish_credential_loss(&mut self, context: &mut Context, account: AccountState) {
        self.transition_after_credential_loss(context, account);
    }

    fn handle_scope_outcome(&mut self, context: &mut Context, outcome: TaskOutcome) {
        match outcome {
            TaskOutcome::Completed(bytes) => {
                self.connection = ConnectionState::Online;
                if let Ok(scope) = commerce::AccountScope::from_bytes(&bytes) {
                    self.account = AccountState::Active;
                    self.account_scope = Some(scope);
                } else {
                    self.account = AccountState::Active;
                    self.account_scope = None;
                }
                self.problem = None;
                self.update_commerce_safety(context);
            }
            TaskOutcome::Failed(TaskError::NoCredential) => {
                self.finish_credential_loss(context, AccountState::SignedOut);
            }
            TaskOutcome::Failed(TaskError::Unauthorized) => {
                self.finish_credential_loss(context, AccountState::Expired);
            }
            TaskOutcome::Failed(TaskError::Denied) => {
                self.account = AccountState::Active;
                self.account_scope = None;
                self.connection = ConnectionState::Online;
                self.problem = None;
                self.update_commerce_safety(context);
            }
            TaskOutcome::Failed(TaskError::Offline) => {
                if self.account == AccountState::Checking {
                    self.clear_protected_state(context);
                    self.account = AccountState::Checking;
                }
                self.connection = ConnectionState::Offline;
                self.problem = None;
                self.retry = Retry::Restart;
                self.update_commerce_safety(context);
            }
            TaskOutcome::Failed(error) => {
                self.problem = Some(Failure::of(error).advice.to_owned());
                self.retry = Retry::Restart;
            }
            TaskOutcome::Cancelled => {}
        }
    }

    fn observe_connectivity(&mut self, context: &mut Context, outcome: &TaskOutcome) {
        if matches!(outcome, TaskOutcome::Failed(TaskError::Offline)) {
            self.connection = ConnectionState::Offline;
            self.update_commerce_safety(context);
        }
    }

    fn clear_protected_state(&mut self, context: &mut Context) {
        self.clear_commerce_access(context);
        self.pending_purchase_rejection = None;
        self.purchase_rejection_notice = None;
        if let Some(task) = self.task.take() {
            context.cancel(task);
        }
        self.pending = None;
        self.queued_foreground = None;
        self.clear_comment_state(context);
        self.cancel_reader(context);
        self.cancel_wallet(context);
        self.clear_title_gifts(context);
        self.clear_episode_thumbnail_cache(context);
        self.retain_public_cover_cache(context);
        self.wallet.summary = None;
        self.wallet.summary_error = false;
        self.wallet.summary_stale = false;
        self.wallet.coin_history.clear();
        self.wallet.ticket_history.clear();
        self.wallet.coin_history_error = false;
        self.wallet.ticket_history_error = false;
        self.comics.clear();
        self.recent.clear();
        self.library_load = ShelfLoadState::default();
        self.recent_load = ShelfLoadState::default();
        self.episodes.clear();
        self.selected_content_id = None;
        self.selected_content_alias.clear();
        self.selected_title.clear();
        self.selected_creators.clear();
        self.selected_synopsis.clear();
        self.synopsis = SynopsisState::default();
        self.episode_title_preview.clear();
        self.episode_creators_preview.clear();
        self.episode_items_per_page = None;
        self.reader_selection = None;
        self.problem = None;
        self.retry = Retry::Restart;
        self.page = 0;
        self.next_library_page = None;
        self.next_recent_page = None;
        self.total_library_titles = 0;
        self.total_recent_titles = 0;
    }

    fn transition_after_credential_loss(&mut self, context: &mut Context, account: AccountState) {
        if self.featured.collection.is_some() {
            self.cancel_collection_details(context);
            self.featured.collection = None;
        }
        self.clear_protected_state(context);
        self.account = account;
        self.connection = ConnectionState::Online;
        self.destination = MainDestination::Featured;
        self.featured.feed_page = 0;
        self.view = if account == AccountState::SignedOut {
            View::Main
        } else {
            View::Status
        };
        self.problem = None;
        let effects = self
            .commerce
            .safety_changed(self.authentication(), commerce::Connectivity::Online);
        self.apply_commerce_effects(context, effects);
    }

    fn clear_all_state(&mut self, context: &mut Context) {
        if self.featured.collection.is_some()
            || self
                .feature_tasks
                .values()
                .any(|purpose| matches!(purpose, FeatureTaskPurpose::CollectionDetail { .. }))
        {
            self.cancel_collection_details(context);
            self.featured.collection = None;
        }
        if self.view.is_reader_flow() {
            self.view = View::Episodes;
        }
        self.clear_protected_state(context);
        self.featured.generation = self.featured.generation.wrapping_add(1);
        for task in std::mem::take(&mut self.feature_tasks).into_keys() {
            context.cancel(task);
        }
        self.superseded_feature_tasks.clear();
        self.feature_capacity_state = FeatureCapacityState::Ready;
        self.featured.snapshot_generation = self.featured.snapshot_generation.wrapping_add(1);
        self.featured.snapshot = None;
        self.featured.detail_cache.clear();
        self.featured.batch = None;
        self.featured.feed_page = 0;
        self.featured.loaded_day = None;
        self.featured.local_day_pending = false;
        self.featured.desired_day = None;
        self.covers.generation = self.covers.generation.wrapping_add(1);
        for task in std::mem::take(&mut self.covers.tasks).into_keys() {
            context.cancel(task);
        }
        for state in std::mem::take(&mut self.covers.entries).into_values() {
            if let CoverState::Ready(picture) = state {
                context.drop_picture(picture.handle);
            }
        }
        self.covers.visible_urls.clear();
        self.covers.visible_source = None;
    }

    fn request_local_day(&mut self, context: &mut Context) {
        if self.featured.local_day_pending {
            return;
        }
        self.featured.local_day_pending = true;
        context.device().read_local_day();
    }

    fn observe_local_day(&mut self, context: &mut Context, observed: Option<LocalDay>) {
        let Some(day) = observed else {
            return;
        };
        if self.featured.observe_day(day) {
            self.resume_capacity_work(context);
        }
    }

    fn feature_source_work(source: FeatureSource) -> kobo_sdk::Task {
        match source {
            FeatureSource::Homepage => api::homepage(),
            FeatureSource::Ranking => api::ranking(),
            FeatureSource::MostFavorited => api::most_favorited(),
            FeatureSource::Themes => api::themes(),
            FeatureSource::Freetime => api::freetime(),
        }
    }

    fn spawn_feature_source(
        &mut self,
        context: &mut Context,
        generation: u64,
        source: FeatureSource,
    ) -> bool {
        if generation != self.featured.generation {
            return false;
        }
        let Some(task) = context.spawn(Self::feature_source_work(source)) else {
            return false;
        };
        if !self.featured.mark_source_pending(generation, source) {
            context.cancel(task);
            return false;
        }
        self.feature_tasks
            .insert(task, FeatureTaskPurpose::Source { generation, source });
        true
    }

    fn resume_feature_capacity(&mut self, context: &mut Context) -> bool {
        let mut spawned = false;
        while let Some(source) = self.featured.queued_source() {
            let generation = self.featured.generation;
            if !self.spawn_feature_source(context, generation, source) {
                return spawned;
            }
            spawned = true;
        }

        let generation = self.featured.generation;
        for alias in self.featured.pending_banner_aliases() {
            let active = self.feature_tasks.values().any(|purpose| {
                matches!(
                    purpose,
                    FeatureTaskPurpose::BannerDetail {
                        generation: task_generation,
                        alias: task_alias,
                    } if *task_generation == generation && task_alias == alias
                )
            });
            if active {
                continue;
            }
            let Some(task) = context.spawn(api::public_detail(alias)) else {
                return spawned;
            };
            self.feature_tasks.insert(
                task,
                FeatureTaskPurpose::BannerDetail {
                    generation,
                    alias: alias.to_owned(),
                },
            );
            spawned = true;
        }

        loop {
            let next = self.featured.collection.as_ref().and_then(|collection| {
                collection.queued_aliases.front().map(|alias| {
                    (
                        self.featured.snapshot_generation,
                        collection.generation,
                        collection.collection_id.clone(),
                        alias.clone(),
                    )
                })
            });
            let Some((generation, collection_generation, collection_id, alias)) = next else {
                break;
            };
            if self.featured.detail_cache.contains_key(&alias) {
                if let Some(collection) = self.featured.collection.as_mut() {
                    collection.queued_aliases.pop_front();
                }
                continue;
            }
            let Some(task) = context.spawn(api::public_detail(&alias)) else {
                return spawned;
            };
            let Some(collection) = self.featured.collection.as_mut().filter(|collection| {
                collection.generation == collection_generation
                    && collection.collection_id == collection_id
                    && collection.queued_aliases.front() == Some(&alias)
            }) else {
                context.cancel(task);
                return spawned;
            };
            collection.queued_aliases.pop_front();
            collection.pending_aliases.insert(alias.clone());
            self.featured
                .detail_cache
                .insert(alias.clone(), DetailState::Loading(task));
            self.feature_tasks.insert(
                task,
                FeatureTaskPurpose::CollectionDetail {
                    generation,
                    collection_generation,
                    collection_id,
                    alias,
                },
            );
            spawned = true;
        }
        spawned
    }

    fn start_feature_batch(&mut self, context: &mut Context, refresh_day: Option<LocalDay>) {
        let superseded = self
            .feature_tasks
            .iter()
            .filter_map(|(task, purpose)| {
                (!matches!(purpose, FeatureTaskPurpose::CollectionDetail { .. })).then_some(*task)
            })
            .collect::<Vec<_>>();
        for task in superseded {
            if self.superseded_feature_tasks.insert(task) {
                context.cancel(task);
            }
        }
        self.featured.begin_full_batch(refresh_day);
        self.resume_capacity_work(context);
    }

    fn retry_failed_feature_sources(&mut self, context: &mut Context) -> bool {
        if self.featured.begin_failed_retry().is_empty() {
            return false;
        }
        self.resume_capacity_work(context);
        true
    }

    fn handle_feature_outcome(
        &mut self,
        context: &mut Context,
        purpose: FeatureTaskPurpose,
        outcome: TaskOutcome,
    ) -> bool {
        match purpose {
            FeatureTaskPurpose::Source { generation, source } => {
                if generation != self.featured.generation {
                    return false;
                }
                let result = match (source, outcome) {
                    (FeatureSource::Homepage, TaskOutcome::Completed(bytes)) => {
                        parse::homepage(&bytes).map(SourceResult::homepage)
                    }
                    (FeatureSource::Themes, TaskOutcome::Completed(bytes)) => {
                        parse::themes(&bytes).map(SourceResult::themes)
                    }
                    (
                        FeatureSource::Ranking
                        | FeatureSource::MostFavorited
                        | FeatureSource::Freetime,
                        TaskOutcome::Completed(bytes),
                    ) => parse::public_collection(&bytes)
                        .map(|comics| SourceResult::collection(source, comics)),
                    (_, TaskOutcome::Failed(_) | TaskOutcome::Cancelled) => {
                        Err(parse::ParseError::InvalidValue("Feature source"))
                    }
                }
                .unwrap_or_else(|_| SourceResult::failure(source));
                if !self.featured.settle_generation(generation, result) {
                    return false;
                }
                let published = self.featured.snapshot_generation;
                self.featured.publish_ready_banner_details();
                if self.featured.snapshot_generation != published {
                    self.reconcile_published_collection(context);
                }
                true
            }
            FeatureTaskPurpose::BannerDetail { generation, alias } => {
                if generation != self.featured.generation {
                    return false;
                }
                let detail = match outcome {
                    TaskOutcome::Completed(bytes) => parse::public_detail(&bytes, &alias).ok(),
                    TaskOutcome::Failed(_) | TaskOutcome::Cancelled => None,
                };
                if !self
                    .featured
                    .settle_banner_detail_generation(generation, &alias, detail)
                {
                    return false;
                }
                let published = self.featured.snapshot_generation;
                self.featured.publish_ready_banner_details();
                if self.featured.snapshot_generation != published {
                    self.reconcile_published_collection(context);
                }
                true
            }
            FeatureTaskPurpose::CollectionDetail {
                generation,
                collection_generation,
                collection_id,
                alias,
            } => {
                if generation != self.featured.snapshot_generation
                    || !self.featured.collection.as_ref().is_some_and(|collection| {
                        collection.generation == collection_generation
                            && collection.collection_id == collection_id
                            && collection.pending_aliases.contains(&alias)
                    })
                {
                    return false;
                }
                let detail = match outcome {
                    TaskOutcome::Completed(bytes) => parse::public_detail(&bytes, &alias)
                        .map_or(DetailState::Failed, DetailState::Ready),
                    TaskOutcome::Failed(_) | TaskOutcome::Cancelled => DetailState::Failed,
                };
                self.featured.detail_cache.insert(alias.clone(), detail);
                if let Some(collection) = self.featured.collection.as_mut() {
                    collection.pending_aliases.remove(&alias);
                }
                self.resume_capacity_work(context);
                self.settle_collection_window(context);
                true
            }
        }
    }

    fn settle_collection_window(&mut self, context: &Context) {
        let Some((collection_id, start, end)) =
            self.featured.collection.as_ref().and_then(|view| {
                (view.queued_aliases.is_empty()
                    && view.pending_aliases.is_empty()
                    && view.window_end > view.window_start
                    && view.next_start() == view.window_start)
                    .then(|| {
                        (
                            view.collection_id.clone(),
                            view.window_start,
                            view.window_end,
                        )
                    })
            })
        else {
            return;
        };
        let Some(collection) = self
            .featured
            .snapshot()
            .and_then(|snapshot| snapshot.collection(&collection_id))
        else {
            return;
        };
        let candidates = &collection.comics[start..end.min(collection.comics.len())];
        if candidates.is_empty() {
            return;
        }
        let candidate_count = candidates.len();
        let owned = candidates
            .iter()
            .map(|comic| {
                (
                    display_content_title(&comic.title, &comic.alias),
                    display_text(&comic.creators, ""),
                    synopsis_for(&self.featured.detail_cache, &comic.alias),
                    compact_count(comic.view_count),
                )
            })
            .collect::<Vec<_>>();
        let measured = owned
            .iter()
            .map(|(title, creators, synopsis, trailing)| {
                (
                    title.as_str(),
                    creators.as_str(),
                    synopsis.as_str(),
                    trailing.as_str(),
                )
            })
            .collect::<Vec<_>>();
        let pages = context.paginate_described_rows_with_trailing(
            &measured,
            RowLineLimits::new(1, 1, 2),
            false,
        );
        let count = pages.first().map_or(1, |page| page.len().max(1));
        if let Some(view) = self.featured.collection.as_mut().filter(|view| {
            view.collection_id == collection_id
                && view.window_start == start
                && view.window_end == end
                && view.queued_aliases.is_empty()
                && view.pending_aliases.is_empty()
        }) {
            let discovery_origin = view.pages.len().saturating_sub(1);
            let should_advance = view.page == discovery_origin;
            view.commit_page(start, count.min(candidate_count));
            if should_advance {
                view.page = view.pages.len().saturating_sub(1);
            }
        }
    }

    fn queue_collection_window(&mut self, context: &mut Context) {
        let Some(collection_id) = self
            .featured
            .collection
            .as_ref()
            .map(|view| view.collection_id.clone())
        else {
            return;
        };
        let Some(aliases) = self
            .featured
            .snapshot()
            .and_then(|snapshot| snapshot.collection(&collection_id))
            .map(|collection| {
                collection
                    .comics
                    .iter()
                    .map(|comic| comic.alias.clone())
                    .collect::<Vec<_>>()
            })
        else {
            return;
        };
        if let Some(view) = self.featured.collection.as_mut() {
            view.queue_detail_window(&aliases, &self.featured.detail_cache);
        }
        self.resume_capacity_work(context);
        self.settle_collection_window(context);
    }

    fn cancel_collection_details(&mut self, context: &mut Context) {
        self.featured.detail_generation = self.featured.detail_generation.wrapping_add(1);
        let tasks = self
            .feature_tasks
            .iter()
            .filter_map(|(task, purpose)| {
                matches!(purpose, FeatureTaskPurpose::CollectionDetail { .. }).then_some(*task)
            })
            .collect::<Vec<_>>();
        for task in tasks {
            if let Some(FeatureTaskPurpose::CollectionDetail { alias, .. }) =
                self.feature_tasks.remove(&task)
            {
                context.cancel(task);
                if self.featured.detail_cache.get(&alias) == Some(&DetailState::Loading(task)) {
                    self.featured.detail_cache.remove(&alias);
                }
            }
        }
        if let Some(view) = self.featured.collection.as_mut() {
            for alias in &view.pending_aliases {
                if matches!(
                    self.featured.detail_cache.get(alias),
                    Some(DetailState::Loading(_))
                ) {
                    self.featured.detail_cache.remove(alias);
                }
            }
            view.generation = self.featured.detail_generation;
            view.pending_aliases.clear();
            view.queued_aliases.clear();
        }
    }

    fn reconcile_published_collection(&mut self, context: &mut Context) {
        let active_collection = self
            .featured
            .collection
            .as_ref()
            .map(|view| (view.collection_id.clone(), view.origin_feed_page));
        self.cancel_collection_details(context);
        self.featured.detail_cache.clear();
        let Some((collection_id, origin_feed_page)) = active_collection else {
            return;
        };
        let last_page = featured_feed_pages(&self.featured, &CLARA_BW_METRICS)
            .len()
            .saturating_sub(1);
        let origin_feed_page = origin_feed_page.min(last_page);
        let replacement_len = self
            .featured
            .snapshot()
            .and_then(|snapshot| snapshot.collection(&collection_id))
            .map(|collection| collection.comics.len());
        if let Some(len) = replacement_len {
            let mut view = CollectionView::new(&collection_id, origin_feed_page, len);
            view.generation = self.featured.detail_generation;
            self.featured.collection = Some(view);
            self.queue_collection_window(context);
            return;
        }

        self.featured.collection = None;
        self.destination = MainDestination::Featured;
        self.view = View::Main;
        self.featured.feed_page = origin_feed_page;
    }

    fn resume_cancelled_collection_window(&mut self, context: &mut Context) {
        let should_resume = self.featured.collection.as_ref().is_some_and(|view| {
            view.queued_aliases.is_empty()
                && view.pending_aliases.is_empty()
                && view.window_start == view.next_start()
                && view.window_end > view.window_start
        });
        if should_resume {
            self.queue_collection_window(context);
        }
    }

    fn open_public_main(&mut self, context: &mut Context) {
        self.destination = MainDestination::Featured;
        self.view = View::Main;
        self.page = 0;
        self.request_local_day(context);
        self.refresh_asset_summary(context);
        self.request_foreground(context, Pending::Library(0));
        self.start_feature_batch(context, None);
    }

    fn restart(&mut self, context: &mut Context) {
        self.problem = None;
        self.clear_protected_state(context);
        self.begin_commerce_safety(context);
        self.open_public_main(context);
    }

    fn foreground_work(&self, pending: Pending) -> kobo_sdk::Task {
        match pending {
            Pending::Library(page) => api::library(page),
            Pending::Recent(page) => api::recent(page),
            Pending::Content(_) => api::detail(&self.selected_content_alias),
            Pending::Logout => api::logout(),
        }
    }

    fn begin_shelf_load(&mut self, pending: Pending) {
        match pending {
            Pending::Library(page) => self.library_load.begin(page),
            Pending::Recent(page) => self.recent_load.begin(page),
            Pending::Content(_) | Pending::Logout => {}
        }
    }

    fn preempt_feature_tasks_for_commerce(&mut self, context: &mut Context) -> bool {
        let tasks = self
            .feature_tasks
            .iter()
            .filter(|(task, purpose)| {
                !self.superseded_feature_tasks.contains(task)
                    && !matches!(purpose, FeatureTaskPurpose::CollectionDetail { .. })
            })
            .map(|(task, purpose)| (*task, purpose.clone()))
            .collect::<Vec<_>>();
        for (task, purpose) in &tasks {
            if let FeatureTaskPurpose::Source { generation, source } = purpose {
                self.featured.requeue_source(*generation, *source);
            }
            if self.superseded_feature_tasks.insert(*task) {
                context.cancel(*task);
            }
        }
        let preempted = !tasks.is_empty();
        if preempted {
            self.feature_capacity_state = FeatureCapacityState::ResumeAfterCommerce;
        }
        preempted
    }

    fn preempt_cover_tasks(&mut self, context: &mut Context) {
        for (task, cover) in std::mem::take(&mut self.covers.tasks) {
            context.cancel(task);
            if self.covers.entries.get(&cover.url) == Some(&CoverState::Loading(task)) {
                self.covers.entries.remove(&cover.url);
            }
        }
    }

    fn request_foreground(&mut self, context: &mut Context, pending: Pending) {
        if self.pending == Some(pending) || self.queued_foreground == Some(pending) {
            return;
        }
        self.begin_shelf_load(pending);
        if matches!(pending, Pending::Content(_)) {
            self.preempt_cover_tasks(context);
        }
        if self.task.is_some() {
            if self.queued_foreground.is_none() {
                self.queued_foreground = Some(pending);
            }
            self.preempt_cover_tasks(context);
            return;
        }
        let work = self.foreground_work(pending);
        if let Some(task) = context.spawn(work) {
            self.task = Some(task);
            self.pending = Some(pending);
        } else {
            self.queued_foreground = Some(pending);
            self.preempt_cover_tasks(context);
        }
    }

    fn resume_queued_foreground(&mut self, context: &mut Context) {
        if self.task.is_some() {
            return;
        }
        let Some(pending) = self.queued_foreground.take() else {
            return;
        };
        let work = self.foreground_work(pending);
        if let Some(task) = context.spawn(work) {
            self.task = Some(task);
            self.pending = Some(pending);
        } else {
            self.queued_foreground = Some(pending);
        }
    }

    fn resume_capacity_work(&mut self, context: &mut Context) {
        if self.scope_refresh_pending && self.scope_task.is_none() {
            if let Some(task) = context.spawn(api::account_scope()) {
                self.scope_task = Some(task);
                self.scope_refresh_pending = false;
            }
        }
        if self.scope_refresh_pending {
            return;
        }
        self.resume_queued_foreground(context);
        if self.queued_foreground.is_some() {
            return;
        }
        if self.reader_manifest_pending {
            self.start_manifest(context);
        }
        if self.reader_manifest_pending {
            return;
        }
        if self.queued_commerce_command.is_some() {
            self.resume_queued_commerce(context);
        }
        if self.queued_commerce_command.is_some() {
            return;
        }
        self.resume_deferred_wallet(context);
        if self.wallet.summary_refresh_queued || !self.wallet.detail_queue.is_empty() {
            return;
        }
        if matches!(
            self.commerce.state(),
            commerce::CommerceState::Quoting
                | commerce::CommerceState::Choosing
                | commerce::CommerceState::Requoting
                | commerce::CommerceState::PersistingIntent
                | commerce::CommerceState::Mutating
                | commerce::CommerceState::Reconciling
                | commerce::CommerceState::ClearingIntent
        ) {
            self.feature_capacity_state = FeatureCapacityState::ResumeAfterCommerce;
            return;
        }
        self.resume_feature_capacity(context);
        self.spawn_visible_covers(context);
    }

    fn spawn_reader(
        &mut self,
        context: &mut Context,
        purpose: ReaderTaskPurpose,
        work: kobo_sdk::Task,
        foreground: bool,
    ) -> Option<TaskId> {
        let at_task_limit = self.reader.as_ref().map_or_else(
            || !self.reader_tasks.is_empty(),
            |reader| self.reader_tasks.len() >= reader.limits.tasks,
        );
        if at_task_limit || (foreground && self.foreground_reader_task.is_some()) {
            return None;
        }
        let task = context.spawn(work)?;
        self.reader_tasks.insert(
            task,
            ReaderTaskEntry {
                generation: self.reader_generation,
                purpose,
            },
        );
        if foreground {
            self.foreground_reader_task = Some(task);
        }
        Some(task)
    }

    fn start_manifest(&mut self, context: &mut Context) {
        self.reader_manifest_pending = false;
        let Some((content_alias, episode_alias)) =
            self.reader_selection.as_ref().map(|selection| {
                (
                    selection.content_alias.clone(),
                    selection.episode_alias.clone(),
                )
            })
        else {
            self.problem = Some("The selected episode is no longer available.".to_owned());
            self.retry = Retry::Restart;
            return;
        };
        let Ok(panel_width) = u32::try_from(context.metrics().width) else {
            self.fail_reader(Retry::Manifest, "The panel width is not supported.");
            return;
        };
        if panel_width == 0 {
            self.fail_reader(Retry::Manifest, "The panel width is not supported.");
            return;
        }
        self.cancel_reader(context);
        self.retry = Retry::Manifest;
        if self
            .spawn_reader(
                context,
                ReaderTaskPurpose::Manifest,
                api::images(&content_alias, &episode_alias, panel_width),
                true,
            )
            .is_none()
        {
            self.reader_manifest_pending = true;
        }
    }

    fn start_reader_source(
        &mut self,
        context: &mut Context,
        source: usize,
        purpose: ReaderTaskPurpose,
        foreground: bool,
    ) -> Option<TaskId> {
        let url = self.reader.as_ref()?.images.get(source)?.url.clone();
        let task = self.spawn_reader(context, purpose, api::image(&url), foreground)?;
        let reader = self.reader.as_mut()?;
        reader.source_fetches.insert(source, task);
        Some(task)
    }

    fn intent_relevant(&self, source: usize, intent: FetchIntent) -> bool {
        let Some(reader) = self.reader.as_ref() else {
            return false;
        };
        match intent {
            FetchIntent::Foreground { offset_rows } => {
                self.retry == Retry::Viewport(offset_rows)
                    && source_relevant_to_offset(source, offset_rows, &reader.window)
            }
            FetchIntent::Prefetch => source_relevant_to_window(source, &reader.window),
        }
    }

    fn record_source_failure(
        &mut self,
        source: usize,
        intent: FetchIntent,
        advice: impl Into<String>,
    ) {
        if !self.intent_relevant(source, intent) {
            return;
        }
        let advice = advice.into();
        match intent {
            FetchIntent::Foreground { offset_rows } => {
                self.fail_reader(Retry::Viewport(offset_rows), advice);
            }
            FetchIntent::Prefetch => {
                if let Some(reader) = self.reader.as_mut() {
                    reader
                        .source_failures
                        .insert(source, SourceFailure { advice });
                }
            }
        }
    }

    fn fail_manifest_refresh(&mut self, advice: impl Into<String>) {
        let advice = advice.into();
        let waiters = self.reader.as_mut().map(|reader| {
            reader.refresh_task = None;
            std::mem::take(&mut reader.refresh_waiters)
        });
        let Some(waiters) = waiters else {
            return;
        };
        let mut foreground = None;
        for (source, intent) in waiters {
            match intent {
                FetchIntent::Foreground { offset_rows } if foreground.is_none() => {
                    foreground = Some(offset_rows);
                }
                FetchIntent::Foreground { .. } | FetchIntent::Prefetch => {
                    self.record_source_failure(source, FetchIntent::Prefetch, advice.clone());
                }
            }
        }
        if let Some(offset_rows) = foreground {
            self.fail_reader(Retry::Viewport(offset_rows), advice);
        }
    }

    fn request_manifest_refresh(
        &mut self,
        context: &mut Context,
        source: usize,
        intent: FetchIntent,
    ) {
        let attempted = self
            .reader
            .as_ref()
            .and_then(|reader| reader.refresh_attempted.get(&source).copied());
        if let Some(original) = attempted {
            let terminal = match original {
                FetchIntent::Prefetch => FetchIntent::Prefetch,
                FetchIntent::Foreground { offset_rows }
                    if self.retry == Retry::Viewport(offset_rows) =>
                {
                    original
                }
                FetchIntent::Foreground { .. } => intent,
            };
            self.record_source_failure(
                source,
                terminal,
                Failure::of(TaskError::Unauthorized).advice,
            );
            return;
        }
        let existing = {
            let Some(reader) = self.reader.as_mut() else {
                return;
            };
            reader.refresh_attempted.insert(source, intent);
            reader.refresh_waiters.insert(source, intent);
            reader.refresh_task
        };
        if let Some(refresh) = existing {
            if matches!(intent, FetchIntent::Foreground { .. })
                && self.foreground_reader_task.is_none()
            {
                self.foreground_reader_task = Some(refresh);
            }
            return;
        }
        let Some((content_alias, episode_alias, panel_width)) =
            self.reader_selection.as_ref().and_then(|selection| {
                self.reader.as_ref().map(|reader| {
                    (
                        selection.content_alias.clone(),
                        selection.episode_alias.clone(),
                        reader.panel_width,
                    )
                })
            })
        else {
            self.fail_manifest_refresh("The selected episode is no longer available.");
            return;
        };
        let foreground = matches!(intent, FetchIntent::Foreground { .. });
        let Some(task) = self.spawn_reader(
            context,
            ReaderTaskPurpose::ManifestRefresh,
            api::images(&content_alias, &episode_alias, panel_width),
            foreground,
        ) else {
            self.fail_manifest_refresh("The comic image URLs could not be refreshed.");
            return;
        };
        if let Some(reader) = self.reader.as_mut() {
            reader.refresh_task = Some(task);
        }
    }

    fn drain_refresh_waiters(&mut self, context: &mut Context) -> Result<(), String> {
        let desired_offset = match self.retry {
            Retry::Viewport(offset_rows) => Some(offset_rows),
            Retry::Restart | Retry::Manifest => None,
        };
        let foreground_available = self.foreground_reader_task.is_none();
        let available_tasks = self.reader.as_ref().map_or(0, |reader| {
            reader.limits.tasks.saturating_sub(self.reader_tasks.len())
        });
        let candidates = {
            let Some(reader) = self.reader.as_mut() else {
                return Ok(());
            };
            if reader.refresh_task.is_some() {
                return Ok(());
            }
            refreshed_source_candidates(
                reader,
                desired_offset,
                foreground_available,
                available_tasks,
            )
        };
        for (source, intent) in candidates {
            let purpose = match intent {
                FetchIntent::Foreground { offset_rows } => ReaderTaskPurpose::ForegroundSource {
                    source,
                    offset_rows,
                },
                FetchIntent::Prefetch => ReaderTaskPurpose::PrefetchSource { source },
            };
            let foreground = matches!(intent, FetchIntent::Foreground { .. });
            if self
                .start_reader_source(context, source, purpose, foreground)
                .is_none()
            {
                return Err("The refreshed comic image request could not be started.".to_owned());
            }
            if let Some(reader) = self.reader.as_mut() {
                reader.refresh_waiters.remove(&source);
                reader.source_failures.remove(&source);
            }
        }
        Ok(())
    }

    fn accept_manifest_refresh(&mut self, context: &mut Context, task: TaskId, bytes: &[u8]) {
        let current = self
            .reader
            .as_ref()
            .is_some_and(|reader| reader.refresh_task == Some(task));
        if !current {
            return;
        }
        if let Some(reader) = self.reader.as_mut() {
            reader.refresh_task = None;
        }
        let refreshed = match parse::images(bytes) {
            Ok(refreshed) => refreshed,
            Err(error) => {
                self.fail_manifest_refresh(error.to_string());
                return;
            }
        };
        let matches = self
            .reader
            .as_ref()
            .is_some_and(|reader| same_assets(&reader.images, &refreshed));
        if !matches {
            self.fail_manifest_refresh("BOMTOON returned different comic image metadata.");
            return;
        }
        if let Some(reader) = self.reader.as_mut() {
            for (current, replacement) in reader.images.iter_mut().zip(refreshed) {
                current.url = replacement.url;
            }
        }
        if let Err(error) = self.maintain_reader(context, true, None) {
            self.fail_manifest_refresh(error);
        }
    }

    fn handle_source_failure(
        &mut self,
        context: &mut Context,
        task: TaskId,
        source: usize,
        intent: FetchIntent,
        error: TaskError,
    ) {
        if let Some(reader) = self.reader.as_mut() {
            if reader.source_fetches.get(&source) == Some(&task) {
                reader.source_fetches.remove(&source);
            }
        }
        match error {
            TaskError::Unauthorized => self.request_manifest_refresh(context, source, intent),
            error => self.record_source_failure(source, intent, Failure::of(error).advice),
        }
        if self.problem.is_none() {
            if let Err(error) = self.maintain_reader(context, true, None) {
                self.record_source_failure(source, intent, error);
            }
        }
    }

    fn handle_source_cancelled(
        &mut self,
        context: &mut Context,
        task: TaskId,
        source: usize,
        intent: FetchIntent,
    ) {
        if let Some(reader) = self.reader.as_mut() {
            if reader.source_fetches.get(&source) == Some(&task) {
                reader.source_fetches.remove(&source);
            }
        }
        self.record_source_failure(source, intent, "The request was cancelled.");
        if self.problem.is_none() {
            if let Err(error) = self.maintain_reader(context, true, None) {
                self.record_source_failure(source, intent, error);
            }
        }
    }

    fn fail_reader(&mut self, retry: Retry, message: impl Into<String>) {
        self.problem = Some(message.into());
        self.retry = retry;
    }

    fn next_handle(&mut self) -> Result<PictureHandle, String> {
        let handle = PictureHandle(self.next_picture_handle);
        self.next_picture_handle = self
            .next_picture_handle
            .checked_add(1)
            .ok_or_else(|| "The picture handle limit was reached.".to_owned())?;
        Ok(handle)
    }

    fn install_viewport(
        &mut self,
        context: &mut Context,
        offset_rows: u64,
        picture: Picture,
    ) -> Result<(), String> {
        let handle = self.next_handle()?;
        let width = picture.width();
        let height = picture.height();
        let uploaded = context
            .put_picture(handle, width, height, picture.into_pixels())
            .ok_or_else(|| "The comic viewport could not be uploaded.".to_owned())?;
        let old = {
            let reader = self
                .reader
                .as_mut()
                .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
            reader.offset_rows = offset_rows;
            reader.chrome_visible = false;
            reader.picture.replace(uploaded)
        };
        self.problem = None;
        self.retry = Retry::Viewport(offset_rows);
        self.show(context);
        if let Some(old) = old {
            context.drop_picture(old.handle);
        }
        let pending_maintenance = self
            .reader
            .as_ref()
            .and_then(|reader| reader.maintenance_task);
        if pending_maintenance.is_some_and(|task| {
            self.reader_tasks.get(&task).is_some_and(|entry| {
                entry.generation == self.reader_generation
                    && entry.purpose == ReaderTaskPurpose::Maintenance
            })
        }) {
            return Ok(());
        }
        if let Some(reader) = self.reader.as_mut() {
            reader.maintenance_task = None;
        }
        let maintenance = self
            .spawn_reader(
                context,
                ReaderTaskPurpose::Maintenance,
                kobo_sdk::Task::Sleep { seconds: 0 },
                false,
            )
            .ok_or_else(|| "The reader maintenance task could not be started.".to_owned())?;
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
        reader.maintenance_task = Some(maintenance);
        Ok(())
    }

    fn accept_manifest(&mut self, context: &mut Context, bytes: &[u8]) {
        let metrics = context.metrics();
        let panel_width = u32::try_from(metrics.width);
        let panel_height = u32::try_from(metrics.height);
        let planned = match (panel_width, panel_height) {
            (Ok(width), Ok(height)) => parse::images(bytes)
                .map_err(|error| error.to_string())
                .and_then(|images| {
                    episode_strip(&images, width, height)
                        .map(|strip| (images, strip, width, height))
                }),
            _ => Err("The panel dimensions are not supported.".to_owned()),
        };
        match planned {
            Ok((images, strip, panel_width, panel_height)) => {
                let first_plan = match viewport_plan(&strip, 0, panel_height) {
                    Ok(plan) => plan,
                    Err(error) => {
                        self.fail_reader(Retry::Manifest, error);
                        return;
                    }
                };
                let first_source = first_plan.segments.first().map(|segment| segment.source);
                let Some(first_source) = first_source else {
                    self.fail_reader(Retry::Manifest, "The comic has no readable content.");
                    return;
                };
                let format = metrics.picture_format;
                let first_build =
                    match ViewportBuild::new(0, first_plan, format, panel_width, panel_height) {
                        Ok(build) => build,
                        Err(error) => {
                            self.fail_reader(Retry::Manifest, error);
                            return;
                        }
                    };
                let mut window = VecDeque::new();
                window.push_back(ViewportEntry::Building(first_build));
                self.reader = Some(ReaderState {
                    generation: self.reader_generation,
                    format,
                    limits: reader_limits(format),
                    panel_width,
                    panel_height,
                    images,
                    strip,
                    offset_rows: 0,
                    window,
                    source_cache: BTreeMap::new(),
                    source_fetches: BTreeMap::new(),
                    maintenance_task: None,
                    refresh_task: None,
                    refresh_waiters: BTreeMap::new(),
                    refresh_attempted: BTreeMap::new(),
                    source_failures: BTreeMap::new(),
                    picture: None,
                    chrome_visible: false,
                });
                self.retry = Retry::Viewport(0);
                if self
                    .start_reader_source(
                        context,
                        first_source,
                        ReaderTaskPurpose::ForegroundSource {
                            source: first_source,
                            offset_rows: 0,
                        },
                        true,
                    )
                    .is_none()
                {
                    self.fail_reader(
                        Retry::Viewport(0),
                        "The first comic image could not be requested.",
                    );
                }
            }
            Err(error) => self.fail_reader(Retry::Manifest, error),
        }
    }

    fn maintain_reader(
        &mut self,
        context: &mut Context,
        extend_window: bool,
        install_target: Option<u64>,
    ) -> Result<bool, String> {
        self.drain_refresh_waiters(context)?;
        let available_tasks = self
            .reader
            .as_ref()
            .map_or(0, |reader| reader.limits.tasks)
            .saturating_sub(self.reader_tasks.len());
        let ReaderMaintenancePlan {
            spawns,
            promotion,
            refresh_promotion,
            ready,
        } = {
            let reader = self
                .reader
                .as_mut()
                .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
            plan_reader_maintenance(reader, available_tasks, extend_window, install_target)?
        };
        if let Some((task, source, offset_rows)) = promotion {
            let entry = self.reader_tasks.get_mut(&task).ok_or_else(|| {
                "The comic image request registry changed unexpectedly.".to_owned()
            })?;
            if entry.generation != self.reader_generation {
                return Err("The comic image request generation changed unexpectedly.".to_owned());
            }
            entry.purpose = ReaderTaskPurpose::ForegroundSource {
                source,
                offset_rows,
            };
            self.foreground_reader_task = Some(task);
        }
        if let Some(task) = refresh_promotion {
            let entry = self.reader_tasks.get(&task).ok_or_else(|| {
                "The comic image URL refresh registry changed unexpectedly.".to_owned()
            })?;
            if entry.generation != self.reader_generation
                || entry.purpose != ReaderTaskPurpose::ManifestRefresh
            {
                return Err(
                    "The comic image URL refresh generation changed unexpectedly.".to_owned(),
                );
            }
            self.foreground_reader_task = Some(task);
        }
        if let Some((offset_rows, picture)) = ready {
            self.install_viewport(context, offset_rows, picture)?;
            return Ok(true);
        }
        for spawn in spawns {
            let Some(task) = self.spawn_reader(
                context,
                spawn.purpose,
                api::image(&spawn.url),
                spawn.foreground,
            ) else {
                return Err("The comic image request could not be started.".to_owned());
            };
            let reader = self
                .reader
                .as_mut()
                .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
            reader.source_fetches.insert(spawn.source, task);
        }
        self.drain_refresh_waiters(context)?;
        Ok(false)
    }

    fn accept_reader_source(
        &mut self,
        context: &mut Context,
        task: TaskId,
        source: usize,
        intent: FetchIntent,
        bytes: &[u8],
    ) -> bool {
        let desired_offset = match self.retry {
            Retry::Viewport(offset_rows) => Some(offset_rows),
            Retry::Restart | Retry::Manifest => None,
        };
        let install_target = match intent {
            FetchIntent::Foreground { offset_rows } => Some(offset_rows),
            FetchIntent::Prefetch => None,
        };
        let foreground_active = self.foreground_reader_task.is_some();
        let (relevant, defer_maintenance) = {
            let Some(reader) = self.reader.as_mut() else {
                return false;
            };
            if reader.source_fetches.get(&source) != Some(&task) {
                return false;
            }
            reader.source_fetches.remove(&source);
            let relevant_to_window = source_relevant_to_window(source, &reader.window);
            let relevant_to_desired = desired_offset
                .is_some_and(|target| source_relevant_to_offset(source, target, &reader.window));
            (
                relevant_to_window || relevant_to_desired,
                matches!(intent, FetchIntent::Prefetch)
                    && (foreground_active || !relevant_to_window),
            )
        };
        if !relevant {
            return false;
        }
        let decoded = self.reader.as_ref().map_or_else(
            || Err("The selected episode is no longer available.".to_owned()),
            |reader| {
                reader.images.get(source).map_or_else(
                    || Err("The selected comic image is no longer available.".to_owned()),
                    |expected| {
                        decode_reader_source(bytes, expected, reader.format, reader.panel_width)
                    },
                )
            },
        );
        let source_picture = match decoded {
            Ok(picture) => picture,
            Err(error) => {
                self.record_source_failure(source, intent, error);
                return false;
            }
        };
        let source_limit_reached = self.reader.as_ref().is_none_or(|reader| {
            reader
                .source_cache
                .len()
                .saturating_add(reader.source_fetches.len())
                >= reader.limits.source_slots
        });
        if source_limit_reached {
            self.record_source_failure(
                source,
                intent,
                "The comic source window exceeded its format limit.",
            );
            return false;
        }
        let Some(reader) = self.reader.as_mut() else {
            return false;
        };
        reader.source_cache.insert(source, source_picture);
        reader.source_failures.remove(&source);
        if defer_maintenance {
            return false;
        }
        match self.maintain_reader(
            context,
            matches!(intent, FetchIntent::Prefetch),
            install_target,
        ) {
            Ok(shown) => shown,
            Err(error) => {
                self.record_source_failure(source, intent, error);
                false
            }
        }
    }

    fn accept(&mut self, context: &mut Context, pending: Pending, bytes: &[u8]) -> bool {
        let selected_content_alias = self.selected_content_alias.clone();
        match pending {
            Pending::Logout => {
                if bytes.is_empty() {
                    self.transition_after_credential_loss(context, AccountState::SignedOut);
                } else {
                    self.problem = Some("BOMTOON returned unexpected sign-out data.".to_owned());
                }
            }
            Pending::Library(expected) => match parse::library(bytes) {
                Ok(page) if page.number == expected => {
                    self.library_load.finish();
                    self.total_library_titles = page.total_items;
                    self.comics.extend(page.comics);
                    self.next_library_page =
                        (page.number + 1 < page.total_pages).then_some(page.number + 1);
                    if self.destination == MainDestination::Library && expected != 0 {
                        self.page = self.page.saturating_add(1);
                    }
                }
                Ok(_) => self
                    .library_load
                    .fail(expected, "BOMTOON returned a different library page."),
                Err(error) => self.library_load.fail(expected, error.to_string()),
            },
            Pending::Recent(expected) => match parse::recent(bytes) {
                Ok(page) if page.number == expected => {
                    self.recent_load.finish();
                    self.total_recent_titles = page.total_items;
                    self.recent.extend(page.entries);
                    self.next_recent_page =
                        (page.number + 1 < page.total_pages).then_some(page.number + 1);
                    if self.destination == MainDestination::Recent && expected != 0 {
                        self.page = self.page.saturating_add(1);
                    }
                }
                Ok(_) => self
                    .recent_load
                    .fail(expected, "BOMTOON returned a different recent page."),
                Err(error) => self.recent_load.fail(expected, error.to_string()),
            },
            Pending::Content(_index) => {
                match parse::content_detail(bytes, &selected_content_alias) {
                    Ok(detail) => {
                        let reader_after_refresh = self.reader_after_content_refresh.take();
                        let episode_page = self.page;
                        let episode_anchor = self.visible_episode_anchor();
                        let refreshed_episode_id = reader_after_refresh
                            .and_then(|index| self.episodes.get(index))
                            .map(|episode| episode.id);
                        self.selected_content_id = Some(detail.id);
                        self.synopsis = SynopsisState {
                            pages: synopsis_pages(&detail.synopsis),
                            open_page: None,
                            ..SynopsisState::default()
                        };
                        self.selected_title = detail.title;
                        self.selected_creators = detail.creators.join(" | ");
                        self.selected_synopsis = detail.synopsis;
                        self.clear_episode_thumbnail_cache(context);
                        self.episodes = detail.episodes;
                        self.prepare_episode_layout();
                        self.restore_episode_page(
                            episode_anchor,
                            if reader_after_refresh.is_some() {
                                episode_page
                            } else {
                                0
                            },
                        );
                        self.view = View::Episodes;
                        let refreshed_index = refreshed_episode_id.and_then(|id| {
                            self.episodes.iter().position(|episode| episode.id == id)
                        });
                        let refreshed_rental = refreshed_index.is_some_and(|index| {
                            self.episodes.get(index).is_some_and(|episode| {
                                episode.purchase == model::PurchaseState::Rented
                            })
                        });
                        if let Some(index) = refreshed_index.filter(|_| refreshed_rental) {
                            self.start_reader_episode(context, index);
                        } else {
                            self.refresh_title_gifts(context, GiftTaskPurpose::Display);
                        }
                    }
                    Err(error) => self.problem = Some(error.to_string()),
                }
            }
        }
        false
    }

    fn open_selected_comic(
        &mut self,
        context: &mut Context,
        alias: &str,
        title: &str,
        pending_index: usize,
    ) {
        if self.featured.collection.is_some() {
            self.cancel_collection_details(context);
            self.featured.collection = None;
        }
        if self.account != AccountState::Active {
            self.destination = MainDestination::Featured;
            self.view = View::Status;
            self.page = 0;
            self.problem = None;
            self.show(context);
            return;
        }
        self.clear_episode_thumbnail_cache(context);
        self.view = View::Main;
        self.pending_purchase_rejection = None;
        self.purchase_rejection_notice = None;
        self.page = 0;
        self.clear_title_gifts(context);
        self.selected_content_id = None;
        self.selected_content_alias.clear();
        self.selected_content_alias.push_str(alias);
        self.selected_title = display_content_title(title, alias);
        self.selected_creators.clear();
        self.selected_synopsis.clear();
        self.synopsis = SynopsisState::default();
        self.episode_title_preview.clear();
        self.episode_creators_preview.clear();
        self.episode_items_per_page = None;
        self.problem = None;
        self.retry = Retry::Restart;
        self.request_foreground(context, Pending::Content(pending_index));
        self.show(context);
    }

    fn open_comic(&mut self, context: &mut Context, index: usize) {
        let selected = match self.destination {
            MainDestination::Featured => None,
            MainDestination::Library => self
                .comics
                .get(index)
                .map(|comic| (comic.alias.clone(), comic.title.clone())),
            MainDestination::Recent => self
                .recent
                .get(index)
                .map(|recent| (recent.content_alias.clone(), recent.content_title.clone())),
        };
        let Some((alias, title)) = selected else {
            return;
        };
        self.open_selected_comic(context, &alias, &title, index);
    }

    fn open_collection(&mut self, context: &mut Context, id: &str) {
        let Some(len) = self
            .featured
            .snapshot()
            .and_then(|snapshot| snapshot.collection(id))
            .map(|collection| collection.comics.len())
        else {
            return;
        };
        self.featured.detail_generation = self.featured.detail_generation.wrapping_add(1);
        let mut view = CollectionView::new(id, self.featured.feed_page, len);
        view.generation = self.featured.detail_generation;
        self.featured.collection = Some(view);
        self.destination = MainDestination::Featured;
        self.view = View::FeatureCollection;
        self.queue_collection_window(context);
        self.show(context);
    }

    fn start_reader_episode(&mut self, context: &mut Context, index: usize) {
        let Some(episode) = self.episodes.get(index) else {
            return;
        };
        let Some((episode_alias, title)) = episode.purchase.is_readable().then(|| {
            (
                episode.alias.clone(),
                display_text(&episode.title, &format!("Episode {}", episode.alias)),
            )
        }) else {
            return;
        };
        self.clear_episode_thumbnail_cache(context);
        self.synopsis.open_page = None;
        self.cancel_title_gift_task(context);
        self.clear_comment_state(context);
        self.reader_selection = Some(EpisodeSelection {
            content_alias: self.selected_content_alias.clone(),
            episode_alias,
            title,
        });
        self.reader = None;
        self.problem = None;
        self.view = View::Reader;
        self.start_manifest(context);
    }

    fn open_episode(&mut self, context: &mut Context, index: usize) {
        let Some(episode) = self.episodes.get(index) else {
            return;
        };
        let commerce_state = self.commerce.state();
        let active_transaction = matches!(
            commerce_state,
            commerce::CommerceState::Quoting
                | commerce::CommerceState::Choosing
                | commerce::CommerceState::Requoting
                | commerce::CommerceState::PersistingIntent
                | commerce::CommerceState::Mutating
                | commerce::CommerceState::Reconciling
                | commerce::CommerceState::ClearingIntent
        );
        if active_transaction
            || (episode.purchase == model::PurchaseState::NotOwned
                && commerce_state != commerce::CommerceState::Idle)
        {
            return;
        }
        self.synopsis.open_page = None;
        self.pending_purchase_rejection = None;
        self.purchase_rejection_notice = None;
        if episode.purchase == model::PurchaseState::NotOwned {
            let Some(title_id) = self.selected_content_id else {
                return;
            };
            let selection = commerce::Selection {
                title_id,
                title_alias: self.selected_content_alias.clone(),
                episode_id: episode.id,
                episode_alias: episode.alias.clone(),
            };
            let effects = self
                .commerce
                .begin_quote(selection, model::PurchaseType::Possession);
            if matches!(
                &effects.command,
                Some(commerce::CommerceCommand::FetchQuote { .. })
            ) {
                self.commerce_episode = Some(index);
            }
            self.apply_commerce_effects(context, effects);
            return;
        }
        if episode.purchase == model::PurchaseState::Rented
            && unix_time_ms().and_then(|now| episode.remaining_rental_hours(now)) == Some(0)
        {
            self.reader_after_content_refresh = Some(index);
            self.request_foreground(context, Pending::Content(index));
            self.show(context);
            return;
        }
        self.start_reader_episode(context, index);
    }

    fn leave_reader(&mut self, context: &mut Context) {
        self.clear_comment_state(context);
        self.reader_selection = None;
        self.problem = None;
        self.retry = Retry::Restart;
        self.view = View::Episodes;
        self.show(context);
        self.cancel_reader(context);
        self.refresh_title_gifts(context, GiftTaskPurpose::Display);
        self.resume_deferred_summary(context);
    }

    fn take_ready_viewport(&mut self, offset_rows: u64) -> Option<Picture> {
        let reader = self.reader.as_mut()?;
        if !matches!(
            reader.window.front(),
            Some(ViewportEntry::Ready {
                offset_rows: ready_offset,
                ..
            }) if *ready_offset == offset_rows
        ) {
            return None;
        }
        match reader.window.pop_front() {
            Some(ViewportEntry::Ready { picture, .. }) => Some(picture),
            Some(ViewportEntry::Building(_)) | None => None,
        }
    }

    fn prepare_rebased_window(&self, offset_rows: u64) -> Result<RebasedReaderWindow, String> {
        let reader = self
            .reader
            .as_ref()
            .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
        let plan = viewport_plan(&reader.strip, offset_rows, reader.panel_height)?;
        let required_source = plan
            .segments
            .first()
            .ok_or_else(|| "The selected comic viewport is empty.".to_owned())?
            .source;
        let source_relevant =
            |source: usize| plan.segments.iter().any(|segment| segment.source == source);
        let is_active_source = |source: usize, task: TaskId| {
            self.reader_tasks.get(&task).is_some_and(|entry| {
                entry.generation == self.reader_generation
                    && matches!(
                        entry.purpose,
                        ReaderTaskPurpose::ForegroundSource {
                            source: task_source,
                            ..
                        } | ReaderTaskPurpose::PrefetchSource {
                            source: task_source,
                        } if task_source == source
                    )
            })
        };
        let required_cached = reader.source_cache.contains_key(&required_source);
        let required_fetch = (!required_cached)
            .then(|| reader.source_fetches.get(&required_source).copied())
            .flatten()
            .filter(|task| is_active_source(required_source, *task));
        let required_retained = required_cached || required_fetch.is_some();
        let retained_capacity = reader
            .limits
            .source_slots
            .saturating_sub(usize::from(!required_retained));
        let mut kept_sources = BTreeSet::new();
        let mut kept_cache_sources = BTreeSet::new();
        let mut kept_fetches = BTreeMap::new();
        if required_cached {
            kept_sources.insert(required_source);
            kept_cache_sources.insert(required_source);
        } else if let Some(task) = required_fetch {
            kept_sources.insert(required_source);
            kept_fetches.insert(required_source, task);
        }
        for &source in reader.source_cache.keys() {
            if kept_sources.len() == retained_capacity {
                break;
            }
            if source_relevant(source) && kept_sources.insert(source) {
                kept_cache_sources.insert(source);
            }
        }
        for (&source, &task) in &reader.source_fetches {
            if kept_sources.len() == retained_capacity
                || kept_fetches.len() == reader.limits.fetches
            {
                break;
            }
            if source_relevant(source)
                && is_active_source(source, task)
                && kept_sources.insert(source)
            {
                kept_fetches.insert(source, task);
            }
        }
        let build = ViewportBuild::new(
            offset_rows,
            plan,
            reader.format,
            reader.panel_width,
            reader.panel_height,
        )?;
        Ok(RebasedReaderWindow {
            build,
            cached_sources: kept_cache_sources,
            fetches: kept_fetches,
        })
    }

    fn rebase_window(&mut self, context: &mut Context, offset_rows: u64) {
        let RebasedReaderWindow {
            build,
            cached_sources: kept_cache_sources,
            fetches: kept_fetches,
        } = match self.prepare_rebased_window(offset_rows) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_reader(Retry::Viewport(offset_rows), error);
                return;
            }
        };

        let mut kept_tasks = kept_fetches.values().copied().collect::<BTreeSet<_>>();
        if let Some(refresh) = self.reader.as_ref().and_then(|reader| reader.refresh_task) {
            kept_tasks.insert(refresh);
        }
        let cancelled = self
            .reader_tasks
            .keys()
            .copied()
            .filter(|task| !kept_tasks.contains(task))
            .collect::<Vec<_>>();
        for task in cancelled {
            self.reader_tasks.remove(&task);
            context.cancel(task);
        }
        for (&source, &task) in &kept_fetches {
            let Some(entry) = self.reader_tasks.get_mut(&task) else {
                self.fail_reader(
                    Retry::Viewport(offset_rows),
                    "The comic image request registry changed unexpectedly.",
                );
                return;
            };
            entry.purpose = ReaderTaskPurpose::PrefetchSource { source };
        }
        self.foreground_reader_task = None;
        let Some(reader) = self.reader.as_mut() else {
            self.fail_reader(
                Retry::Viewport(offset_rows),
                "The selected episode is no longer available.",
            );
            return;
        };
        reader.window.clear();
        reader.window.push_back(ViewportEntry::Building(build));
        reader
            .source_cache
            .retain(|source, _| kept_cache_sources.contains(source));
        reader.source_fetches = kept_fetches;
        reader.maintenance_task = None;
        self.problem = None;
        self.retry = Retry::Viewport(offset_rows);

        match self.maintain_reader(context, false, Some(offset_rows)) {
            Ok(_) => {}
            Err(error) => self.fail_reader(Retry::Viewport(offset_rows), error),
        }
    }

    fn request_reader_viewport(&mut self, context: &mut Context, offset_rows: u64) {
        if let Some(picture) = self.take_ready_viewport(offset_rows) {
            context.set_screen(self.reader_screen_with_chrome(ReadingChrome::OverlayBusy));
            if let Err(error) = self.install_viewport(context, offset_rows, picture) {
                self.fail_reader(Retry::Viewport(offset_rows), error);
                self.show(context);
            }
            return;
        }
        self.rebase_window(context, offset_rows);
        self.show(context);
    }

    fn retry(&mut self, context: &mut Context) -> bool {
        let retry = self.retry;
        self.problem = None;
        match retry {
            Retry::Restart => self.restart(context),
            Retry::Manifest => self.start_manifest(context),
            Retry::Viewport(offset_rows) => {
                let failed = self
                    .reader
                    .as_ref()
                    .and_then(|reader| {
                        viewport_plan(&reader.strip, offset_rows, reader.panel_height).ok()
                    })
                    .into_iter()
                    .flat_map(|plan| plan.segments)
                    .map(|segment| segment.source)
                    .collect::<BTreeSet<_>>();
                if let Some(reader) = self.reader.as_mut() {
                    reader
                        .source_failures
                        .retain(|source, _| !failed.contains(source));
                }
                self.rebase_window(context, offset_rows);
            }
        }
        false
    }

    fn fail_task(&mut self, context: &mut Context, pending: Pending, error: TaskError) {
        if matches!(pending, Pending::Content(_)) {
            self.reader_after_content_refresh = None;
        }
        match (pending, error) {
            (Pending::Logout, TaskError::RevocationUnconfirmed) => {
                self.transition_after_credential_loss(context, AccountState::RevocationUnconfirmed);
            }
            (Pending::Logout, TaskError::LocalStorage) => {
                self.problem = Some("Could not remove the local BOMTOON sign-in data.".to_owned());
            }
            (
                Pending::Library(_) | Pending::Recent(_) | Pending::Content(_),
                TaskError::NoCredential,
            ) => {
                self.transition_after_credential_loss(context, AccountState::SignedOut);
            }
            (
                Pending::Library(_) | Pending::Recent(_) | Pending::Content(_),
                TaskError::Unauthorized,
            ) => {
                self.transition_after_credential_loss(context, AccountState::Expired);
            }
            (Pending::Library(page), error) => {
                self.library_load.fail(page, Failure::of(error).advice);
            }
            (Pending::Recent(page), error) => {
                self.recent_load.fail(page, Failure::of(error).advice);
            }
            (Pending::Content(_) | Pending::Logout, error) => {
                self.problem = Some(Failure::of(error).advice.to_owned());
                self.retry = Retry::Restart;
            }
        }
    }
    fn handle_wallet_credential_failure(&mut self, context: &mut Context, account: AccountState) {
        self.transition_after_credential_loss(context, account);
        self.show(context);
    }

    fn handle_wallet_outcome(
        &mut self,
        context: &mut Context,
        task: TaskId,
        purpose: WalletTaskPurpose,
        outcome: TaskOutcome,
    ) {
        if self.wallet.summary_task == Some(task) {
            self.wallet.summary_task = None;
        }
        let outcome = match outcome {
            TaskOutcome::Failed(TaskError::NoCredential) => {
                self.handle_wallet_credential_failure(context, AccountState::SignedOut);
                return;
            }
            TaskOutcome::Failed(TaskError::Unauthorized) => {
                self.handle_wallet_credential_failure(context, AccountState::Expired);
                return;
            }
            outcome => outcome,
        };
        let mut redraw = false;
        match purpose {
            WalletTaskPurpose::Summary { generation } => {
                if generation == self.wallet.summary_generation {
                    redraw = true;
                    match outcome {
                        TaskOutcome::Completed(bytes) => {
                            if let Ok(summary) = parse::asset_summary(&bytes) {
                                self.wallet.accept_summary(generation, summary);
                            } else {
                                self.wallet.summary_error = true;
                                self.wallet.summary_stale = self.wallet.summary.is_some();
                            }
                        }
                        TaskOutcome::Failed(_) => {
                            self.wallet.summary_error = true;
                            self.wallet.summary_stale = self.wallet.summary.is_some();
                        }
                        TaskOutcome::Cancelled => {}
                    }
                }
                if self.wallet.take_queued_summary_refresh() {
                    self.refresh_asset_summary(context);
                    redraw = true;
                }
            }
            WalletTaskPurpose::CoinHistory { generation }
            | WalletTaskPurpose::TicketHistory { generation } => {
                if generation == self.wallet.detail_generation {
                    redraw = true;
                    let kind = match purpose {
                        WalletTaskPurpose::CoinHistory { .. } => AssetKind::Coin,
                        WalletTaskPurpose::TicketHistory { .. } => AssetKind::Ticket,
                        WalletTaskPurpose::Summary { .. } => unreachable!(),
                    };
                    match outcome {
                        TaskOutcome::Completed(bytes) => {
                            match parse::expiration_history(&bytes, kind) {
                                Ok(rows) => {
                                    self.wallet.accept_history(generation, kind, rows);
                                }
                                Err(_) => self.set_history_error(kind, true),
                            }
                        }
                        TaskOutcome::Failed(_) => self.set_history_error(kind, true),
                        TaskOutcome::Cancelled => {}
                    }
                    self.clamp_account_history_page();
                }
            }
        }
        if redraw {
            self.show(context);
        }
    }

    fn spawn_comment_task(
        &mut self,
        context: &mut Context,
        purpose: CommentTaskPurpose,
        work: kobo_sdk::Task,
    ) -> bool {
        if self.comment_task.is_some() {
            return false;
        }
        let Some(id) = context.spawn(work) else {
            return false;
        };
        self.comment_task = Some(CommentTask { id, purpose });
        true
    }

    fn comment_aliases(&self) -> Option<(String, String)> {
        self.reader_selection.as_ref().map(|selection| {
            (
                selection.content_alias.clone(),
                selection.episode_alias.clone(),
            )
        })
    }

    fn start_comment_appendix(&mut self, context: &mut Context) {
        if self.account != AccountState::Active {
            return;
        }
        self.view = View::CommentAppendix;
        self.comment_appendix = CommentAppendixState::Loading;
        let Some((content_alias, episode_alias)) = self.comment_aliases() else {
            self.comment_appendix =
                CommentAppendixState::Failed("The selected episode is unavailable.".to_owned());
            self.show(context);
            return;
        };
        if !self.spawn_comment_task(
            context,
            CommentTaskPurpose::AppendixHot,
            api::comments(&content_alias, &episode_alias, api::CommentOrder::Hot, 0),
        ) {
            self.comment_appendix =
                CommentAppendixState::Failed("No task slot is available.".to_owned());
        }
        self.show(context);
    }

    fn start_comment_page(&mut self, context: &mut Context, page: usize, arrival: PageArrival) {
        self.view = View::Comments;
        if let Some(state) = self.comments.as_mut() {
            state.error = None;
        }
        let Some((content_alias, episode_alias)) = self.comment_aliases() else {
            self.comments = Some(CommentPageState {
                comments: Vec::new(),
                number: page,
                total_pages: 0,
                total_items: 0,
                item: 0,
                error: Some((page, "The selected episode is unavailable.".to_owned())),
            });
            self.show(context);
            return;
        };
        if !self.spawn_comment_task(
            context,
            CommentTaskPurpose::Comments { page, arrival },
            api::comments(
                &content_alias,
                &episode_alias,
                api::CommentOrder::Newest,
                page,
            ),
        ) {
            let problem = "No task slot is available.".to_owned();
            if let Some(state) = self.comments.as_mut() {
                state.error = Some((page, problem));
            } else {
                self.comments = Some(CommentPageState {
                    comments: Vec::new(),
                    number: page,
                    total_pages: 0,
                    total_items: 0,
                    item: 0,
                    error: Some((page, problem)),
                });
            }
        }
        self.show(context);
    }

    fn start_replies(&mut self, context: &mut Context, parent: Comment) {
        self.view = View::Replies;
        self.replies = Some(ReplyState {
            parent,
            replies: Vec::new(),
            number: 0,
            total_pages: 0,
            total_items: 0,
            show_parent: true,
            text_page: 0,
            item: 0,
            error: None,
        });
        self.start_reply_page(context, 0, PageArrival::First, true);
    }

    fn start_reply_page(
        &mut self,
        context: &mut Context,
        page: usize,
        arrival: PageArrival,
        show_parent: bool,
    ) {
        let Some(comment_id) = self.replies.as_ref().map(|state| state.parent.id) else {
            self.show(context);
            return;
        };
        if let Some(state) = self.replies.as_mut() {
            state.error = None;
        }
        if !self.spawn_comment_task(
            context,
            CommentTaskPurpose::Replies {
                comment_id,
                page,
                arrival,
                show_parent,
            },
            api::replies(comment_id, api::CommentOrder::Hot, page),
        ) {
            if let Some(state) = self.replies.as_mut() {
                state.error = Some((page, "No task slot is available.".to_owned()));
            }
        }
        self.show(context);
    }

    fn fail_comment_task(&mut self, purpose: CommentTaskPurpose, problem: String) {
        match purpose {
            CommentTaskPurpose::AppendixHot | CommentTaskPurpose::AppendixFallback => {
                self.comment_appendix = CommentAppendixState::Failed(problem);
            }
            CommentTaskPurpose::Comments { page, .. } => {
                if let Some(state) = self.comments.as_mut() {
                    state.error = Some((page, problem));
                } else {
                    self.comments = Some(CommentPageState {
                        comments: Vec::new(),
                        number: page,
                        total_pages: 0,
                        total_items: 0,
                        item: 0,
                        error: Some((page, problem)),
                    });
                }
            }
            CommentTaskPurpose::Replies { page, .. } => {
                if let Some(state) = self.replies.as_mut() {
                    state.error = Some((page, problem));
                }
            }
        }
    }

    fn accept_appendix_hot(&mut self, context: &mut Context, bytes: &[u8]) {
        let purpose = CommentTaskPurpose::AppendixHot;
        let page = match parse::comments(bytes) {
            Ok(page) => page,
            Err(error) => {
                self.fail_comment_task(purpose, error.to_string());
                return;
            }
        };
        let comments = page
            .comments
            .into_iter()
            .filter(Comment::is_best)
            .take(APPENDIX_COMMENT_LIMIT)
            .collect::<Vec<_>>();
        if comments.is_empty() && page.total_items > 0 {
            let Some((content_alias, episode_alias)) = self.comment_aliases() else {
                self.fail_comment_task(purpose, "The selected episode is unavailable.".to_owned());
                return;
            };
            if !self.spawn_comment_task(
                context,
                CommentTaskPurpose::AppendixFallback,
                api::comments(&content_alias, &episode_alias, api::CommentOrder::Newest, 0),
            ) {
                self.fail_comment_task(purpose, "No task slot is available.".to_owned());
            }
        } else if comments.is_empty() {
            self.comment_appendix = CommentAppendixState::Empty;
        } else {
            self.comment_appendix = CommentAppendixState::Ready {
                comments,
                total_items: page.total_items,
            };
        }
    }

    fn accept_appendix_fallback(&mut self, bytes: &[u8]) {
        let purpose = CommentTaskPurpose::AppendixFallback;
        match parse::comments(bytes) {
            Ok(page) => {
                let comments = page
                    .comments
                    .into_iter()
                    .take(APPENDIX_COMMENT_LIMIT)
                    .collect::<Vec<_>>();
                if comments.is_empty() {
                    self.comment_appendix = CommentAppendixState::Empty;
                } else {
                    self.comment_appendix = CommentAppendixState::Ready {
                        comments,
                        total_items: page.total_items,
                    };
                }
            }
            Err(error) => self.fail_comment_task(purpose, error.to_string()),
        }
    }

    fn accept_comments(&mut self, purpose: CommentTaskPurpose, arrival: PageArrival, bytes: &[u8]) {
        match parse::comments(bytes) {
            Ok(page) => {
                let item = match arrival {
                    PageArrival::First => 0,
                    PageArrival::Last => page.comments.len().saturating_sub(1),
                };
                self.comments = Some(CommentPageState {
                    comments: page.comments,
                    number: page.number,
                    total_pages: page.total_pages,
                    total_items: page.total_items,
                    item,
                    error: None,
                });
            }
            Err(error) => self.fail_comment_task(purpose, error.to_string()),
        }
    }

    fn accept_replies(
        &mut self,
        purpose: CommentTaskPurpose,
        arrival: PageArrival,
        show_parent: bool,
        bytes: &[u8],
    ) {
        match parse::replies(bytes) {
            Ok(page) => {
                let item = match arrival {
                    PageArrival::First => 0,
                    PageArrival::Last => page.replies.len().saturating_sub(1),
                };
                let text_page = if show_parent {
                    0
                } else {
                    page.replies
                        .get(item)
                        .and_then(|reply| comment_detail_page(&reply.text, 0))
                        .map_or(0, |(_, pages)| match arrival {
                            PageArrival::First => 0,
                            PageArrival::Last => pages.saturating_sub(1),
                        })
                };
                self.replies = Some(ReplyState {
                    parent: page.parent,
                    replies: page.replies,
                    number: page.number,
                    total_pages: page.total_pages,
                    total_items: page.total_items,
                    show_parent,
                    text_page,
                    item,
                    error: None,
                });
            }
            Err(error) => self.fail_comment_task(purpose, error.to_string()),
        }
    }

    fn accept_comment_task(
        &mut self,
        context: &mut Context,
        purpose: CommentTaskPurpose,
        bytes: &[u8],
    ) {
        match purpose {
            CommentTaskPurpose::AppendixHot => self.accept_appendix_hot(context, bytes),
            CommentTaskPurpose::AppendixFallback => self.accept_appendix_fallback(bytes),
            CommentTaskPurpose::Comments { arrival, .. } => {
                self.accept_comments(purpose, arrival, bytes);
            }
            CommentTaskPurpose::Replies {
                arrival,
                show_parent,
                ..
            } => self.accept_replies(purpose, arrival, show_parent, bytes),
        }
    }

    fn handle_comment_outcome(
        &mut self,
        context: &mut Context,
        purpose: CommentTaskPurpose,
        outcome: TaskOutcome,
    ) {
        match outcome {
            TaskOutcome::Completed(bytes) => self.accept_comment_task(context, purpose, &bytes),
            TaskOutcome::Failed(TaskError::NoCredential) => {
                self.transition_after_credential_loss(context, AccountState::SignedOut);
                self.show(context);
                return;
            }
            TaskOutcome::Failed(TaskError::Unauthorized) => {
                self.transition_after_credential_loss(context, AccountState::Expired);
                self.show(context);
                return;
            }
            TaskOutcome::Failed(error) => {
                self.fail_comment_task(purpose, Failure::of(error).advice.to_owned());
            }
            TaskOutcome::Cancelled => {
                self.fail_comment_task(purpose, "The request was cancelled.".to_owned());
            }
        }
        self.show(context);
    }

    fn cancel_comment_task(&mut self, context: &mut Context) {
        if let Some(task) = self.comment_task.take() {
            context.cancel(task.id);
        }
    }

    fn clear_comment_state(&mut self, context: &mut Context) {
        self.cancel_comment_task(context);
        self.comment_appendix = CommentAppendixState::default();
        self.comments = None;
        self.replies = None;
    }

    fn retry_comment_action(&mut self, context: &mut Context) {
        match self.view {
            View::CommentAppendix => self.start_comment_appendix(context),
            View::Comments => {
                let Some(state) = self.comments.as_ref() else {
                    self.show(context);
                    return;
                };
                let page = state.error.as_ref().map_or(state.number, |(page, _)| *page);
                let arrival = if page < state.number {
                    PageArrival::Last
                } else {
                    PageArrival::First
                };
                self.start_comment_page(context, page, arrival);
            }
            View::Replies => {
                let Some(state) = self.replies.as_ref() else {
                    self.show(context);
                    return;
                };
                let page = state.error.as_ref().map_or(state.number, |(page, _)| *page);
                let arrival = if page < state.number {
                    PageArrival::Last
                } else {
                    PageArrival::First
                };
                let show_parent =
                    state.show_parent && state.total_pages == 0 && state.replies.is_empty();
                self.start_reply_page(context, page, arrival, show_parent);
            }
            _ => self.show(context),
        }
    }

    fn handle_comment_list_action(&mut self, context: &mut Context, action: ActionId) {
        if action == action_id(COMMENTS_PREVIOUS) {
            let target = self.comments.as_mut().and_then(|state| {
                if state.item > 0 {
                    state.item -= 1;
                    None
                } else {
                    state.number.checked_sub(1)
                }
            });
            if let Some(page) = target {
                self.start_comment_page(context, page, PageArrival::Last);
            } else {
                self.show(context);
            }
            return;
        }
        if action == action_id(COMMENTS_NEXT) {
            let target = self.comments.as_mut().and_then(|state| {
                if state.item + 1 < state.comments.len() {
                    state.item += 1;
                    None
                } else {
                    state
                        .number
                        .checked_add(1)
                        .filter(|page| *page < state.total_pages)
                }
            });
            if let Some(page) = target {
                self.start_comment_page(context, page, PageArrival::First);
            } else {
                self.show(context);
            }
            return;
        }
        if let Some(parent) = self
            .comments
            .as_ref()
            .and_then(|state| state.comments.get(state.item))
            .filter(|comment| action == action_id(&format!("comment-{}", comment.id)))
            .cloned()
        {
            self.start_replies(context, parent);
        } else {
            self.show(context);
        }
    }

    fn previous_reply(&mut self, context: &mut Context) {
        let target = self.replies.as_mut().and_then(|state| {
            if state.text_page > 0 {
                state.text_page -= 1;
                return None;
            }
            if state.show_parent {
                return None;
            }
            if state.item > 0 {
                state.item -= 1;
                state.text_page = state
                    .replies
                    .get(state.item)
                    .and_then(|reply| comment_detail_page(&reply.text, 0))
                    .map_or(0, |(_, pages)| pages.saturating_sub(1));
                return None;
            }
            if let Some(page) = state.number.checked_sub(1) {
                return Some((page, PageArrival::Last));
            }
            state.show_parent = true;
            state.text_page = comment_detail_page(&state.parent.text, 0)
                .map_or(0, |(_, pages)| pages.saturating_sub(1));
            None
        });
        if let Some((page, arrival)) = target {
            self.start_reply_page(context, page, arrival, false);
        } else {
            self.show(context);
        }
    }

    fn next_reply(&mut self, context: &mut Context) {
        let target = self.replies.as_mut().and_then(|state| {
            if !state.show_parent && state.replies.get(state.item).is_none() {
                return state
                    .number
                    .checked_add(1)
                    .filter(|page| *page < state.total_pages)
                    .map(|page| (page, PageArrival::First));
            }
            let comment = if state.show_parent {
                &state.parent
            } else {
                state.replies.get(state.item)?
            };
            let pages = comment_detail_page(&comment.text, 0).map_or(1, |(_, pages)| pages);
            if state.text_page.saturating_add(1) < pages {
                state.text_page += 1;
                return None;
            }
            if state.show_parent {
                if !state.replies.is_empty() {
                    state.show_parent = false;
                    state.item = 0;
                    state.text_page = 0;
                    return None;
                }
            } else if state.item + 1 < state.replies.len() {
                state.item += 1;
                state.text_page = 0;
                return None;
            }
            state
                .number
                .checked_add(1)
                .filter(|page| *page < state.total_pages)
                .map(|page| (page, PageArrival::First))
        });
        if let Some((page, arrival)) = target {
            self.start_reply_page(context, page, arrival, false);
        } else {
            self.show(context);
        }
    }

    fn handle_reply_action(&mut self, context: &mut Context, action: ActionId) {
        if action == action_id(REPLIES_PREVIOUS) {
            self.previous_reply(context);
        } else if action == action_id(REPLIES_NEXT) {
            self.next_reply(context);
        } else {
            self.show(context);
        }
    }

    fn handle_comment_action(&mut self, context: &mut Context, action: ActionId) {
        if self.comment_task.is_some() {
            self.show(context);
        } else if action == action_id(RETRY) {
            self.retry_comment_action(context);
        } else {
            match self.view {
                View::CommentAppendix if action == action_id(ALL_COMMENTS) => {
                    self.start_comment_page(context, 0, PageArrival::First);
                }
                View::Comments => self.handle_comment_list_action(context, action),
                View::Replies => self.handle_reply_action(context, action),
                _ => self.show(context),
            }
        }
    }

    fn handle_reader_action(&mut self, context: &mut Context, action: ActionId) {
        if action == action_id(READER_CHROME) {
            if let Some(reader) = self.reader.as_mut() {
                reader.chrome_visible = !reader.chrome_visible;
            }
            self.show(context);
            return;
        }
        if action == action_id(READER_PREVIOUS) {
            let target = self.reader.as_ref().and_then(|reader| {
                previous_viewport_offset(reader.offset_rows, reader.panel_height)
            });
            if let Some(offset_rows) = target {
                self.request_reader_viewport(context, offset_rows);
            }
            return;
        }
        if action == action_id(READER_NEXT) {
            let target = self.reader.as_ref().and_then(|reader| {
                next_viewport_offset(
                    reader.offset_rows,
                    reader.panel_height,
                    reader.strip.total_rows,
                )
            });
            if let Some(offset_rows) = target {
                self.request_reader_viewport(context, offset_rows);
            } else if self.reader.is_some() {
                self.start_comment_appendix(context);
            }
            return;
        }
        self.show(context);
    }

    fn handle_manifest_outcome(&mut self, context: &mut Context, outcome: TaskOutcome) -> bool {
        match outcome {
            TaskOutcome::Completed(bytes) => self.accept_manifest(context, &bytes),
            TaskOutcome::Failed(TaskError::NoCredential) => {
                self.transition_after_credential_loss(context, AccountState::SignedOut);
            }
            TaskOutcome::Failed(TaskError::Unauthorized) => {
                self.transition_after_credential_loss(context, AccountState::Expired);
            }
            TaskOutcome::Failed(error) => {
                self.fail_reader(Retry::Manifest, Failure::of(error).advice);
            }
            TaskOutcome::Cancelled => {
                self.fail_reader(Retry::Manifest, "The request was cancelled.");
            }
        }
        false
    }

    fn clear_manifest_refresh(&mut self, task: TaskId) {
        if let Some(reader) = self.reader.as_mut() {
            if reader.refresh_task == Some(task) {
                reader.refresh_task = None;
            }
        }
    }

    fn handle_manifest_refresh_outcome(
        &mut self,
        context: &mut Context,
        task: TaskId,
        outcome: TaskOutcome,
    ) -> bool {
        match outcome {
            TaskOutcome::Completed(bytes) => self.accept_manifest_refresh(context, task, &bytes),
            TaskOutcome::Failed(error) => {
                self.clear_manifest_refresh(task);
                match error {
                    TaskError::NoCredential => {
                        self.transition_after_credential_loss(context, AccountState::SignedOut);
                    }
                    TaskError::Unauthorized => {
                        self.transition_after_credential_loss(context, AccountState::Expired);
                    }
                    error => self.fail_manifest_refresh(Failure::of(error).advice),
                }
            }
            TaskOutcome::Cancelled => {
                self.clear_manifest_refresh(task);
                self.fail_manifest_refresh("The request was cancelled.");
            }
        }
        false
    }

    fn handle_reader_source_outcome(
        &mut self,
        context: &mut Context,
        task: TaskId,
        source: usize,
        intent: FetchIntent,
        outcome: TaskOutcome,
    ) -> bool {
        match outcome {
            TaskOutcome::Completed(bytes) => {
                self.accept_reader_source(context, task, source, intent, &bytes)
            }
            TaskOutcome::Failed(error) => {
                self.handle_source_failure(context, task, source, intent, error);
                false
            }
            TaskOutcome::Cancelled => {
                self.handle_source_cancelled(context, task, source, intent);
                false
            }
        }
    }

    fn handle_maintenance_outcome(
        &mut self,
        context: &mut Context,
        task: TaskId,
        outcome: &TaskOutcome,
    ) -> bool {
        let offset_rows = self.reader.as_ref().map_or(0, |reader| reader.offset_rows);
        if let Some(reader) = self.reader.as_mut() {
            if reader.maintenance_task == Some(task) {
                reader.maintenance_task = None;
            }
        }
        match outcome {
            TaskOutcome::Completed(_) => {
                if let Err(error) = self.maintain_reader(context, true, None) {
                    self.fail_reader(Retry::Viewport(offset_rows), error);
                }
            }
            TaskOutcome::Failed(error) => {
                self.fail_reader(Retry::Viewport(offset_rows), Failure::of(*error).advice);
            }
            TaskOutcome::Cancelled => {
                self.fail_reader(Retry::Viewport(offset_rows), "The request was cancelled.");
            }
        }
        false
    }

    fn handle_reader_outcome(
        &mut self,
        context: &mut Context,
        task: TaskId,
        entry: ReaderTaskEntry,
        outcome: TaskOutcome,
    ) {
        if entry.generation != self.reader_generation {
            return;
        }
        if !matches!(entry.purpose, ReaderTaskPurpose::Manifest)
            && self
                .reader
                .as_ref()
                .is_none_or(|reader| reader.generation != entry.generation)
        {
            return;
        }
        if matches!(&outcome, TaskOutcome::Failed(TaskError::Offline)) {
            self.leave_reader(context);
            return;
        }
        let shown = match entry.purpose {
            ReaderTaskPurpose::Manifest => self.handle_manifest_outcome(context, outcome),
            ReaderTaskPurpose::ManifestRefresh => {
                self.handle_manifest_refresh_outcome(context, task, outcome)
            }
            ReaderTaskPurpose::ForegroundSource {
                source,
                offset_rows,
            } => self.handle_reader_source_outcome(
                context,
                task,
                source,
                FetchIntent::Foreground { offset_rows },
                outcome,
            ),
            ReaderTaskPurpose::PrefetchSource { source } => self.handle_reader_source_outcome(
                context,
                task,
                source,
                FetchIntent::Prefetch,
                outcome,
            ),
            ReaderTaskPurpose::Maintenance => {
                self.handle_maintenance_outcome(context, task, &outcome)
            }
        };
        if !shown {
            self.show(context);
        }
    }

    fn handle_cover_outcome(
        &mut self,
        context: &mut Context,
        task: TaskId,
        cover: CoverTask,
        outcome: TaskOutcome,
    ) -> bool {
        let loading = self.covers.entries.get(&cover.url) == Some(&CoverState::Loading(task));
        if cover.generation != self.covers.generation
            || !loading
            || !self.cover_url_owned(cover.source, &cover.url)
        {
            if loading {
                self.covers.entries.remove(&cover.url);
            }
            return false;
        }
        let state = match outcome {
            TaskOutcome::Completed(bytes) => kobo_image::decode(&bytes)
                .ok()
                .and_then(|picture| {
                    let handle = self.next_handle().ok()?;
                    let width = picture.width();
                    let height = picture.height();
                    context.put_picture(handle, width, height, picture.into_pixels())
                })
                .map_or(CoverState::Failed, CoverState::Ready),
            TaskOutcome::Failed(_) | TaskOutcome::Cancelled => CoverState::Failed,
        };
        self.covers.entries.insert(cover.url, state);
        true
    }

    fn cancel_task(&mut self, pending: Pending) {
        match pending {
            Pending::Library(page) => self.library_load.fail(page, "The request was cancelled."),
            Pending::Recent(page) => self.recent_load.fail(page, "The request was cancelled."),
            Pending::Content(_) | Pending::Logout => {
                self.problem = Some("The request was cancelled.".to_owned());
                self.retry = Retry::Restart;
            }
        }
    }
    fn handle_foreground_task_outcome(
        &mut self,
        context: &mut Context,
        task: TaskId,
        outcome: TaskOutcome,
    ) {
        if self.task != Some(task) {
            self.resume_capacity_work(context);
            return;
        }
        self.task = None;
        let Some(pending) = self.pending.take() else {
            self.resume_capacity_work(context);
            return;
        };
        self.observe_connectivity(context, &outcome);
        let shown = match outcome {
            TaskOutcome::Completed(bytes) => self.accept(context, pending, &bytes),
            TaskOutcome::Failed(error) => {
                self.fail_task(context, pending, error);
                false
            }
            TaskOutcome::Cancelled => {
                self.cancel_task(pending);
                false
            }
        };
        if !shown {
            self.show(context);
        }
        self.resume_capacity_work(context);
    }
}

impl KoboApp for Bomtoon {
    fn on_start(&mut self, context: &mut Context) {
        self.restart(context);
        self.show(context);
    }

    fn on_resume(&mut self, context: &mut Context) {
        self.request_local_day(context);
        self.request_commerce_scope(context);
        self.resume_cancelled_collection_window(context);
        if self.view == View::FeatureCollection {
            self.show(context);
        }
    }

    fn on_suspend(&mut self, context: &mut Context) {
        self.synopsis.open_page = None;
        self.clear_episode_thumbnail_cache(context);
        if self.featured.collection.is_some() {
            self.cancel_collection_details(context);
        }
        self.clear_commerce_access(context);
        if self.view.is_reader_flow()
            || self.reader_selection.is_some()
            || self.reader.is_some()
            || !self.reader_tasks.is_empty()
            || self.comment_task.is_some()
        {
            self.clear_comment_state(context);
            self.reader_selection = None;
            self.problem = None;
            self.retry = Retry::Restart;
            if self.view.is_reader_flow() {
                self.view = View::Episodes;
            }
            self.cancel_reader(context);
            self.resume_deferred_summary(context);
        }
    }

    fn on_background(&mut self, context: &mut Context) {
        self.clear_commerce_access(context);
        if self.featured.collection.is_some() {
            self.cancel_collection_details(context);
        }
    }

    fn on_foreground(&mut self, context: &mut Context) {
        self.request_local_day(context);
        self.request_commerce_scope(context);
        self.resume_cancelled_collection_window(context);
    }

    fn on_device_result(
        &mut self,
        context: &mut Context,
        request: DeviceRequest,
        result: DeviceResult,
    ) {
        if !self.featured.local_day_pending {
            return;
        }
        if let (DeviceRequest::ReadLocalDay, DeviceResult::LocalDay(observed)) = (request, result) {
            self.featured.local_day_pending = false;
            self.observe_local_day(context, observed);
        }
    }

    fn on_exit(&mut self, context: &mut Context) {
        self.clear_all_state(context);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered action dispatcher keeps Back, retry, and view ownership explicit"
    )]
    fn on_action(&mut self, context: &mut Context, action: ActionId) {
        if let Some(page) = self.synopsis.open_page {
            if action == action_id(SYNOPSIS_PREVIOUS) {
                self.synopsis.open_page = Some(page.saturating_sub(1));
            } else if action == action_id(SYNOPSIS_NEXT)
                && page.saturating_add(1) < self.synopsis.pages.len()
            {
                self.synopsis.open_page = Some(page.saturating_add(1));
            } else if action == action_id(SYNOPSIS_CLOSE) || action == ActionId::BACK {
                self.synopsis.open_page = None;
            }
            self.show(context);
            return;
        }
        if self.view == View::Episodes
            && action == action_id(SYNOPSIS_MORE)
            && !self.synopsis.pages.is_empty()
        {
            self.synopsis.open_page = Some(0);
            self.show(context);
            return;
        }
        if self.view == View::FeatureCollection && action == ActionId::BACK {
            self.cancel_collection_details(context);
            let origin = self
                .featured
                .collection
                .take()
                .map_or(0, |view| view.origin_feed_page);
            self.featured.feed_page = origin;
            self.view = View::Main;
            self.destination = MainDestination::Featured;
            self.show(context);
            return;
        }
        if self.view == View::Episodes && action == action_id(RETRY_GIFTS) {
            self.refresh_title_gifts(context, GiftTaskPurpose::Display);
            self.show(context);
            return;
        }
        if self.view == View::Episodes && action == ActionId::BACK {
            match self.commerce.state() {
                commerce::CommerceState::Quoting
                | commerce::CommerceState::Choosing
                | commerce::CommerceState::Requoting => {
                    if let Some(task) = self.commerce_task.take() {
                        context.cancel(task.id);
                    }
                    self.commerce_generation = self.commerce_generation.wrapping_add(1);
                    self.retained_quote = None;
                    self.commerce_episode = None;
                    let effects = self.commerce.cancel_unpersisted();
                    self.apply_commerce_effects(context, effects);
                    return;
                }
                commerce::CommerceState::PersistingIntent
                | commerce::CommerceState::Mutating
                | commerce::CommerceState::Reconciling
                | commerce::CommerceState::ClearingIntent => {
                    self.show(context);
                    return;
                }
                commerce::CommerceState::AcceptedButStale => {
                    if !self.commerce.marker_belongs_to_another_account() {
                        self.show(context);
                        return;
                    }
                }
                commerce::CommerceState::LoadingSafetyState | commerce::CommerceState::Idle => {}
            }
        }
        if self.commerce.state() == commerce::CommerceState::AcceptedButStale
            && !self.commerce.marker_belongs_to_another_account()
        {
            if action == action_id(REFRESH_COMMERCE) {
                let effects = self.commerce.refresh_status();
                self.apply_commerce_effects(context, effects);
            } else {
                self.show(context);
            }
            return;
        }
        if self.view == View::Episodes && self.commerce.state() == commerce::CommerceState::Choosing
        {
            let choice = if action == action_id(USE_GIFT) {
                Some(commerce::Action::UseGift)
            } else if action == action_id(RENT) {
                Some(commerce::Action::Rent)
            } else if action == action_id(BUY) {
                Some(commerce::Action::Buy)
            } else if action == action_id(CANCEL_COMMERCE) || action == ActionId::BACK {
                Some(commerce::Action::Cancel)
            } else {
                None
            };
            if let Some(choice) = choice {
                if choice != commerce::Action::Cancel {
                    self.retained_quote = self.commerce.quote_presentation().cloned();
                }
                let effects = self.commerce.choose(choice);
                if choice == commerce::Action::Cancel {
                    self.commerce_episode = None;
                    self.retained_quote = None;
                }
                self.apply_commerce_effects(context, effects);
                return;
            }
        }
        if action == ActionId::BACK {
            match self.view {
                View::Reader => {
                    self.leave_reader(context);
                    return;
                }
                View::CommentAppendix => {
                    self.cancel_comment_task(context);
                    self.view = View::Reader;
                    self.show(context);
                    return;
                }
                View::Comments => {
                    self.cancel_comment_task(context);
                    self.view = View::CommentAppendix;
                    self.show(context);
                    return;
                }
                View::Replies => {
                    self.cancel_comment_task(context);
                    self.replies = None;
                    self.view = View::Comments;
                    self.show(context);
                    return;
                }
                View::Status
                | View::Main
                | View::FeatureCollection
                | View::Account
                | View::Episodes => {}
            }
        }
        if matches!(
            self.view,
            View::CommentAppendix | View::Comments | View::Replies
        ) {
            self.handle_comment_action(context, action);
            return;
        }
        if self.view == View::FeatureCollection {
            if action == action_id(PREVIOUS_PAGE) {
                if let Some(view) = self.featured.collection.as_mut() {
                    view.page = view.page.saturating_sub(1);
                }
                self.show(context);
                return;
            }
            if action == action_id(NEXT_PAGE) {
                let has_stored_next = self
                    .featured
                    .collection
                    .as_ref()
                    .is_some_and(|view| view.page.saturating_add(1) < view.pages.len());
                if has_stored_next {
                    if let Some(view) = self.featured.collection.as_mut() {
                        view.page = view.page.saturating_add(1);
                    }
                } else {
                    let can_discover = self.featured.collection.as_ref().is_some_and(|view| {
                        view.queued_aliases.is_empty()
                            && view.pending_aliases.is_empty()
                            && self
                                .featured
                                .snapshot()
                                .and_then(|snapshot| snapshot.collection(&view.collection_id))
                                .is_some_and(|collection| {
                                    view.next_start() < collection.comics.len()
                                })
                    });
                    if can_discover {
                        self.queue_collection_window(context);
                    }
                }
                self.show(context);
                return;
            }
            let selected = self.featured.collection.as_ref().and_then(|view| {
                let range = view.pages.get(view.page)?;
                let collection = self
                    .featured
                    .snapshot()
                    .and_then(|snapshot| snapshot.collection(&view.collection_id))?;
                range.clone().find_map(|index| {
                    let comic = &collection.comics[index];
                    (action == action_id(&comic_action(&collection.id, index)))
                        .then(|| (comic.alias.clone(), comic.title.clone(), index))
                })
            });
            if let Some((alias, title, index)) = selected {
                self.open_selected_comic(context, &alias, &title, index);
                return;
            }
            self.show(context);
            return;
        }
        let ready = self.account == AccountState::Active
            && self.problem.is_none()
            && self.pending.is_none()
            && self.queued_foreground.is_none()
            && self.foreground_reader_task.is_none();
        if action == ActionId::BACK && ready && matches!(self.view, View::Account | View::Episodes)
        {
            if self.view == View::Account {
                self.cancel_account_history(context);
            }
            self.view = View::Main;
            self.page = 0;
            self.featured.feed_page = 0;
            if self.destination == MainDestination::Library && !self.library_load.loaded {
                self.comics.clear();
                self.total_library_titles = 0;
                self.next_library_page = None;
                self.library_load = ShelfLoadState::default();
                self.request_foreground(context, Pending::Library(0));
            }
            self.show(context);
            return;
        }
        if action == action_id(RETRY) && self.problem.is_none() && self.view == View::Main {
            let retry_page = match self.destination {
                MainDestination::Recent => self.recent_load.error.as_ref().map(|(page, _)| *page),
                MainDestination::Library => self.library_load.error.as_ref().map(|(page, _)| *page),
                MainDestination::Featured => None,
            };
            if let Some(page) = retry_page {
                let pending = match self.destination {
                    MainDestination::Recent => Pending::Recent(page),
                    MainDestination::Library => Pending::Library(page),
                    MainDestination::Featured => unreachable!("Featured has no shelf retry"),
                };
                self.request_foreground(context, pending);
                self.show(context);
                return;
            }
        }
        if action == action_id(RETRY)
            && self.problem.is_none()
            && self.view == View::Main
            && self.destination == MainDestination::Featured
            && self.featured.has_failed_sources()
            && self
                .featured
                .batch
                .as_ref()
                .is_none_or(feature::FeatureBatch::settled)
        {
            self.retry_failed_feature_sources(context);
            self.show(context);
            return;
        }
        let retry_visible = self.problem.is_some()
            || self.view == View::Status
            || (self.account == AccountState::Checking
                && self.connection == ConnectionState::Offline);
        if action == action_id(RETRY) && self.pending.is_none() && retry_visible {
            if !self.retry(context) {
                self.show(context);
            }
            return;
        }
        if self.view == View::Main
            && self.account != AccountState::Active
            && action == action_id(SIGN_IN)
        {
            self.destination = MainDestination::Featured;
            self.page = 0;
            self.view = View::Status;
            self.show(context);
            return;
        }
        let blocks_main_navigation = self
            .pending
            .into_iter()
            .chain(self.queued_foreground)
            .any(|pending| matches!(pending, Pending::Content(_) | Pending::Logout));
        if self.view == View::Main && !blocks_main_navigation {
            let target = if action == action_id(FEATURED) {
                Some(MainDestination::Featured)
            } else if action == action_id(RECENT) {
                Some(MainDestination::Recent)
            } else if action == action_id(LIBRARY) {
                Some(MainDestination::Library)
            } else {
                None
            };
            if let Some(target) = target {
                self.select_destination(context, target);
                self.show(context);
                return;
            }
        }
        let featured_content_ready = self.problem.is_none()
            && self.pending.is_none()
            && self.queued_foreground.is_none()
            && self.foreground_reader_task.is_none();
        if self.view == View::Main
            && self.destination == MainDestination::Featured
            && featured_content_ready
        {
            if action == action_id(PREVIOUS_PAGE) {
                let last_page = featured_feed_pages(&self.featured, &CLARA_BW_METRICS)
                    .len()
                    .saturating_sub(1);
                self.featured.feed_page = self.featured.feed_page.min(last_page).saturating_sub(1);
                self.show(context);
                return;
            }
            if action == action_id(NEXT_PAGE) {
                let pages = featured_feed_pages(&self.featured, &CLARA_BW_METRICS);
                self.featured.feed_page = self
                    .featured
                    .feed_page
                    .saturating_add(1)
                    .min(pages.len().saturating_sub(1));
                self.show(context);
                return;
            }
            let selected = self.featured.snapshot().and_then(|snapshot| {
                snapshot
                    .banners
                    .iter()
                    .take(3)
                    .enumerate()
                    .find(|(index, _)| action == action_id(&format!("feature-banner-{index}")))
                    .map(|(index, comic)| (comic.alias.clone(), comic.title.clone(), index))
                    .or_else(|| {
                        snapshot.collections.iter().find_map(|collection| {
                            collection.comics.iter().take(6).enumerate().find_map(
                                |(index, comic)| {
                                    (action == action_id(&comic_action(&collection.id, index)))
                                        .then(|| (comic.alias.clone(), comic.title.clone(), index))
                                },
                            )
                        })
                    })
            });
            if let Some((alias, title, index)) = selected {
                self.open_selected_comic(context, &alias, &title, index);
                return;
            }
            let collection_id = self.featured.snapshot().and_then(|snapshot| {
                snapshot.collections.iter().find_map(|collection| {
                    (action == action_id(&collection_action(&collection.id)))
                        .then(|| collection.id.clone())
                })
            });
            if let Some(collection_id) = collection_id {
                self.open_collection(context, &collection_id);
                return;
            }
        }

        if !ready {
            self.show(context);
            return;
        }
        if self.view == View::Reader {
            self.handle_reader_action(context, action);
            return;
        }
        if self.view == View::Account {
            if action == action_id(SIGN_OUT) {
                self.synopsis.open_page = None;
                self.request_foreground(context, Pending::Logout);
            } else if action == action_id(RETRY_BALANCES)
                && (self.wallet.summary_error
                    || self.wallet.coin_history_error
                    || self.wallet.ticket_history_error)
            {
                self.retry_account_balances(context);
            } else if action == action_id(PREVIOUS_PAGE) {
                self.page = self.page.saturating_sub(1);
            } else if action == action_id(NEXT_PAGE) {
                let row_count = self
                    .wallet
                    .coin_history
                    .len()
                    .saturating_add(self.wallet.ticket_history.len());
                let next_start = self
                    .page
                    .saturating_add(1)
                    .saturating_mul(ACCOUNT_HISTORY_ITEMS_PER_PAGE);
                if next_start < row_count {
                    self.page = self.page.saturating_add(1);
                }
            }
            self.show(context);
            return;
        }
        if self.view == View::Main && action == action_id(ACCOUNT) {
            self.open_account(context);
            return;
        } else if action == action_id(PREVIOUS_PAGE) {
            self.page = self.page.saturating_sub(1);
        } else if action == action_id(NEXT_PAGE) {
            let items_per_page = if self.view == View::Episodes {
                self.episode_rows_per_page()
            } else {
                LIBRARY_ITEMS_PER_PAGE
            };
            let next_start = self.page.saturating_add(1).saturating_mul(items_per_page);
            if self.view == View::Episodes {
                if next_start < self.episodes.len() {
                    self.page = self.page.saturating_add(1);
                }
            } else if next_start < self.destination_len() {
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
        } else if self.view == View::Main && self.destination != MainDestination::Featured {
            for index in 0..self.destination_len() {
                if action == action_id(&format!("comic-{index}")) {
                    self.open_comic(context, index);
                    return;
                }
            }
        } else if self.view == View::Episodes {
            for index in 0..self.episodes.len() {
                if action == action_id(&format!("episode-{index}")) {
                    self.open_episode(context, index);
                    self.show(context);
                    return;
                }
            }
        }
        self.show(context);
    }

    fn on_task(&mut self, context: &mut Context, task: TaskId, outcome: TaskOutcome) {
        if self.comment_task.is_some_and(|active| active.id == task) {
            let purpose = self
                .comment_task
                .take()
                .expect("matching comment task disappeared")
                .purpose;
            self.observe_connectivity(context, &outcome);
            self.handle_comment_outcome(context, purpose, outcome);
            self.resume_capacity_work(context);
            return;
        }
        if self.scope_task == Some(task) {
            self.scope_task = None;
            self.handle_scope_outcome(context, outcome);
            self.show(context);
            self.resume_capacity_work(context);
            return;
        }
        if self
            .commerce_task
            .as_ref()
            .is_some_and(|active| active.id == task)
        {
            let commerce_task = self
                .commerce_task
                .take()
                .expect("matching commerce task disappeared");
            self.observe_connectivity(context, &outcome);
            self.handle_commerce_task(context, commerce_task, outcome);
            self.resume_capacity_work(context);
            return;
        }
        if self.gifts.task.is_some_and(|gift| gift.id == task) {
            let gift = self
                .gifts
                .task
                .take()
                .expect("matching title Gift task disappeared");
            self.observe_connectivity(context, &outcome);
            self.handle_title_gift_outcome(context, gift, outcome);
            self.resume_capacity_work(context);
            return;
        }
        if let Some(cover) = self.covers.tasks.remove(&task) {
            if cover.source != CoverSource::Protected || !self.is_episode_thumbnail_url(&cover.url)
            {
                self.observe_connectivity(context, &outcome);
            }
            self.resume_queued_foreground(context);
            let changed = self.handle_cover_outcome(context, task, cover, outcome);
            if changed {
                self.show(context);
            }
            self.resume_capacity_work(context);
            return;
        }
        if let Some(purpose) = self.feature_tasks.remove(&task) {
            if self.superseded_feature_tasks.remove(&task) {
                self.resume_capacity_work(context);
                return;
            }
            self.observe_connectivity(context, &outcome);
            let changed = self.handle_feature_outcome(context, purpose, outcome);
            self.resume_capacity_work(context);
            if changed
                && (self.view == View::FeatureCollection
                    || (self.view == View::Main && self.destination == MainDestination::Featured))
            {
                self.show(context);
            }
            return;
        }
        if let Some(purpose) = self.wallet.tasks.remove(&task) {
            self.observe_connectivity(context, &outcome);
            self.observe_reconciliation_wallet(purpose, &outcome);
            self.resume_queued_foreground(context);
            self.handle_wallet_outcome(context, task, purpose, outcome);
            self.finish_reconciliation(context);
            self.resume_capacity_work(context);
            return;
        }
        if let Some(entry) = self.reader_tasks.remove(&task) {
            self.observe_connectivity(context, &outcome);
            if self.foreground_reader_task == Some(task) {
                self.foreground_reader_task = None;
            }
            self.resume_queued_foreground(context);
            self.handle_reader_outcome(context, task, entry, outcome);
            self.resume_capacity_work(context);
            return;
        }
        self.handle_foreground_task_outcome(context, task, outcome);
    }

    fn on_store(&mut self, context: &mut Context, result: StoreResult) {
        let effects = match (&self.marker_store, result) {
            (Some(MarkerStoreOperation::Load), StoreResult::Loaded { key, value })
                if key == commerce::MARKER_KEY =>
            {
                self.marker_store = None;
                Some(self.commerce.marker_loaded(value.as_deref()))
            }
            (Some(MarkerStoreOperation::Save), StoreResult::Saved { key })
                if key == commerce::MARKER_KEY =>
            {
                self.marker_store = None;
                Some(self.commerce.marker_saved(&key))
            }
            (Some(MarkerStoreOperation::Forget), StoreResult::Forgotten { key })
                if key == commerce::MARKER_KEY =>
            {
                self.marker_store = None;
                self.retained_quote = None;
                let effects = self.commerce.marker_forgotten(&key);
                if self.commerce.state() == commerce::CommerceState::Idle {
                    self.purchase_rejection_notice = self.pending_purchase_rejection.take();
                }
                Some(effects)
            }
            (Some(_), StoreResult::Denied(error)) => {
                self.marker_store = None;
                Some(self.commerce.store_denied(error))
            }
            _ => None,
        };
        if let Some(effects) = effects {
            self.apply_commerce_effects(context, effects);
        }
    }
}

fn viewport_build(reader: &ReaderState, offset_rows: u64) -> Result<ViewportBuild, String> {
    let plan = viewport_plan(&reader.strip, offset_rows, reader.panel_height)?;
    ViewportBuild::new(
        offset_rows,
        plan,
        reader.format,
        reader.panel_width,
        reader.panel_height,
    )
}

fn entry_offset(entry: &ViewportEntry) -> u64 {
    match entry {
        ViewportEntry::Building(build) => build.offset_rows,
        ViewportEntry::Ready { offset_rows, .. } => *offset_rows,
    }
}

fn source_relevant_to_window(source: usize, window: &VecDeque<ViewportEntry>) -> bool {
    window.iter().any(|entry| {
        let ViewportEntry::Building(build) = entry else {
            return false;
        };
        build.plan.segments[build.next_segment..]
            .iter()
            .any(|segment| segment.source == source)
    })
}

fn source_relevant_to_offset(
    source: usize,
    offset_rows: u64,
    window: &VecDeque<ViewportEntry>,
) -> bool {
    window.iter().any(|entry| {
        let ViewportEntry::Building(build) = entry else {
            return false;
        };
        build.offset_rows == offset_rows
            && build.plan.segments[build.next_segment..]
                .iter()
                .any(|segment| segment.source == source)
    })
}

fn extend_reader_window(reader: &mut ReaderState) -> Result<(), String> {
    while reader.window.len() < reader.limits.pages {
        let previous_offset = reader
            .window
            .back()
            .map_or(reader.offset_rows, entry_offset);
        let Some(next_offset) = next_viewport_offset(
            previous_offset,
            reader.panel_height,
            reader.strip.total_rows,
        ) else {
            break;
        };
        let build = viewport_build(reader, next_offset)?;
        reader.window.push_back(ViewportEntry::Building(build));
    }
    Ok(())
}

fn discard_stale_reader_failures(reader: &mut ReaderState) {
    let ReaderState {
        source_failures,
        window,
        ..
    } = reader;
    source_failures.retain(|source, _| source_relevant_to_window(*source, window));
}

fn update_reader_builds(reader: &mut ReaderState) -> Result<(), String> {
    {
        let ReaderState {
            source_cache,
            window,
            panel_width,
            panel_height,
            ..
        } = reader;
        for (&source, picture) in source_cache.iter() {
            for entry in window.iter_mut() {
                if let ViewportEntry::Building(build) = entry {
                    copy_source_into_builds(
                        source,
                        picture,
                        std::slice::from_mut(build),
                        *panel_width,
                        *panel_height,
                    )?;
                }
            }
        }
    }

    let mut index = 0;
    while index < reader.window.len() {
        let complete = match reader.window.get(index) {
            Some(ViewportEntry::Ready { .. }) => {
                index += 1;
                continue;
            }
            Some(ViewportEntry::Building(build)) => build.next_segment == build.plan.segments.len(),
            None => break,
        };
        if !complete {
            break;
        }
        let Some(ViewportEntry::Building(build)) = reader.window.remove(index) else {
            return Err("The comic viewport window changed unexpectedly.".to_owned());
        };
        let offset_rows = build.offset_rows;
        let picture = finish_build(build, reader.panel_width, reader.panel_height)?;
        reader.window.insert(
            index,
            ViewportEntry::Ready {
                offset_rows,
                picture,
            },
        );
        index += 1;
    }
    Ok(())
}

fn retain_reader_sources(reader: &mut ReaderState) {
    let ReaderState {
        source_cache,
        window,
        ..
    } = reader;
    source_cache.retain(|source, _| source_relevant_to_window(*source, window));
}

fn take_installable_reader_viewport(
    reader: &mut ReaderState,
    target: u64,
) -> Result<Option<(u64, Picture)>, String> {
    let installable = matches!(
        reader.window.front(),
        Some(ViewportEntry::Ready { offset_rows, .. }) if *offset_rows == target
    );
    if !installable {
        return Ok(None);
    }
    let Some(ViewportEntry::Ready {
        offset_rows,
        picture,
    }) = reader.window.pop_front()
    else {
        return Err("The ready comic viewport changed unexpectedly.".to_owned());
    };
    Ok(Some((offset_rows, picture)))
}

fn plan_foreground_reader_source(
    reader: &mut ReaderState,
    offset_rows: u64,
    spawn_limit: usize,
    plan: &mut ReaderMaintenancePlan,
) -> Result<(), String> {
    let missing = reader.window.iter().find_map(|entry| match entry {
        ViewportEntry::Building(build) if build.offset_rows == offset_rows => build
            .plan
            .segments
            .get(build.next_segment)
            .map(|segment| segment.source),
        ViewportEntry::Building(_) | ViewportEntry::Ready { .. } => None,
    });
    let Some(source) = missing else {
        return Ok(());
    };
    if let Some(failure) = reader.source_failures.get(&source) {
        return Err(failure.advice.clone());
    }
    if let Some(intent) = reader.refresh_waiters.get_mut(&source) {
        *intent = FetchIntent::Foreground { offset_rows };
        plan.refresh_promotion = reader.refresh_task;
    } else if let Some(&task) = reader.source_fetches.get(&source) {
        plan.promotion = Some((task, source, offset_rows));
    } else if !reader.source_cache.contains_key(&source) && spawn_limit > 0 {
        let url = reader
            .images
            .get(source)
            .ok_or_else(|| "The selected comic image is no longer available.".to_owned())?
            .url
            .clone();
        plan.spawns.push(PlannedReaderSpawn {
            source,
            purpose: ReaderTaskPurpose::ForegroundSource {
                source,
                offset_rows,
            },
            foreground: true,
            url,
        });
    }
    Ok(())
}

fn plan_prefetch_reader_sources(
    reader: &ReaderState,
    spawn_limit: usize,
    plan: &mut ReaderMaintenancePlan,
) -> Result<(), String> {
    'viewports: for entry in &reader.window {
        let ViewportEntry::Building(build) = entry else {
            continue;
        };
        for segment in &build.plan.segments[build.next_segment..] {
            let source = segment.source;
            if reader.source_failures.contains_key(&source) {
                break 'viewports;
            }
            if reader.refresh_waiters.contains_key(&source) {
                continue;
            }
            if reader.source_cache.contains_key(&source)
                || reader.source_fetches.contains_key(&source)
                || plan.spawns.iter().any(|spawn| spawn.source == source)
            {
                continue;
            }
            let url = reader
                .images
                .get(source)
                .ok_or_else(|| "The selected comic image is no longer available.".to_owned())?
                .url
                .clone();
            plan.spawns.push(PlannedReaderSpawn {
                source,
                purpose: ReaderTaskPurpose::PrefetchSource { source },
                foreground: false,
                url,
            });
            if plan.spawns.len() == spawn_limit {
                break 'viewports;
            }
        }
    }
    Ok(())
}

fn plan_reader_maintenance(
    reader: &mut ReaderState,
    available_tasks: usize,
    extend_window: bool,
    install_target: Option<u64>,
) -> Result<ReaderMaintenancePlan, String> {
    if extend_window {
        extend_reader_window(reader)?;
    }
    discard_stale_reader_failures(reader);
    update_reader_builds(reader)?;
    retain_reader_sources(reader);
    let ready = match install_target {
        Some(target) => take_installable_reader_viewport(reader, target)?,
        None => None,
    };
    let mut plan = ReaderMaintenancePlan {
        ready,
        ..ReaderMaintenancePlan::default()
    };
    if plan.ready.is_some() {
        return Ok(plan);
    }

    let combined = reader
        .source_cache
        .len()
        .saturating_add(reader.source_fetches.len());
    let available_slots = reader.limits.source_slots.saturating_sub(combined);
    let available_fetches = reader
        .limits
        .fetches
        .saturating_sub(reader.source_fetches.len());
    let spawn_limit = available_tasks.min(available_slots).min(available_fetches);
    if let Some(offset_rows) = install_target {
        plan_foreground_reader_source(reader, offset_rows, spawn_limit, &mut plan)?;
    } else if spawn_limit > 0
        && (reader.refresh_task.is_some() || reader.refresh_waiters.is_empty())
    {
        plan_prefetch_reader_sources(reader, spawn_limit, &mut plan)?;
    }
    Ok(plan)
}

fn refreshed_source_candidates(
    reader: &mut ReaderState,
    desired_offset: Option<u64>,
    foreground_available: bool,
    available_tasks: usize,
) -> Vec<(usize, FetchIntent)> {
    let stale = reader
        .refresh_waiters
        .iter()
        .filter_map(|(&source, &intent)| {
            let relevant = match intent {
                FetchIntent::Foreground { offset_rows } => {
                    desired_offset == Some(offset_rows)
                        && source_relevant_to_offset(source, offset_rows, &reader.window)
                }
                FetchIntent::Prefetch => source_relevant_to_window(source, &reader.window),
            };
            (!relevant
                || reader.source_cache.contains_key(&source)
                || reader.source_fetches.contains_key(&source))
            .then_some(source)
        })
        .collect::<Vec<_>>();
    for source in stale {
        reader.refresh_waiters.remove(&source);
    }

    let source_capacity = reader.limits.source_slots.saturating_sub(
        reader
            .source_cache
            .len()
            .saturating_add(reader.source_fetches.len()),
    );
    let fetch_capacity = reader
        .limits
        .fetches
        .saturating_sub(reader.source_fetches.len());
    let mut capacity = available_tasks.min(source_capacity).min(fetch_capacity);
    if !foreground_available
        && reader
            .refresh_waiters
            .values()
            .any(|intent| matches!(intent, FetchIntent::Foreground { .. }))
    {
        capacity = 0;
    }

    let mut candidates = Vec::new();
    if foreground_available && capacity > 0 {
        if let Some((&source, &intent)) = reader
            .refresh_waiters
            .iter()
            .find(|(_, intent)| matches!(intent, FetchIntent::Foreground { .. }))
        {
            candidates.push((source, intent));
            capacity -= 1;
        }
    }
    if capacity > 0 {
        candidates.extend(
            reader
                .refresh_waiters
                .iter()
                .filter(|(_, intent)| matches!(intent, FetchIntent::Prefetch))
                .take(capacity)
                .map(|(&source, &intent)| (source, intent)),
        );
    }
    candidates
}

fn validate_continuous_viewport(plan: &ViewportPlan) -> Result<(), String> {
    let mut next_destination = 0_u32;
    let mut previous_source = None;
    for segment in &plan.segments {
        if segment.rows == 0 || segment.destination_row != next_destination {
            return Err("The comic page plan is not contiguous.".to_owned());
        }
        if let Some(previous) = previous_source {
            if segment.source < previous {
                return Err("The comic page plan is not source-ordered.".to_owned());
            }
        }
        segment
            .source_row
            .checked_add(segment.rows)
            .ok_or_else(|| "The comic source interval is not supported.".to_owned())?;
        next_destination = next_destination
            .checked_add(segment.rows)
            .ok_or_else(|| "The comic destination interval is not supported.".to_owned())?;
        if next_destination > plan.content_rows {
            return Err("The comic page plan is not contiguous.".to_owned());
        }
        previous_source = Some(segment.source);
    }
    if next_destination != plan.content_rows {
        return Err("The comic page plan is not contiguous.".to_owned());
    }
    Ok(())
}

fn viewport_stride(panel_height: u32, percent: u64) -> u64 {
    (u64::from(panel_height) * percent + 50) / 100
}

fn next_viewport_offset(offset_rows: u64, panel_height: u32, total_rows: u64) -> Option<u64> {
    let visible_end = offset_rows.checked_add(u64::from(panel_height))?;
    if visible_end >= total_rows {
        return None;
    }
    offset_rows.checked_add(viewport_stride(panel_height, 90))
}

fn previous_viewport_offset(offset_rows: u64, panel_height: u32) -> Option<u64> {
    (offset_rows > 0).then(|| offset_rows.saturating_sub(viewport_stride(panel_height, 50)))
}

fn reader_progress_percent(offset_rows: u64, panel_height: u32, total_rows: u64) -> u8 {
    if total_rows == 0 {
        return 0;
    }
    let visible_end = offset_rows
        .saturating_add(u64::from(panel_height))
        .min(total_rows);
    u8::try_from(visible_end.saturating_mul(100) / total_rows).unwrap_or(100)
}

fn episode_strip(
    images: &[EpisodeImage],
    panel_width: u32,
    panel_height: u32,
) -> Result<EpisodeStrip, String> {
    if panel_width == 0 || panel_height == 0 {
        return Err("The comic page dimensions are not supported.".to_owned());
    }

    let mut total_rows = 0_u64;
    let mut sources = Vec::with_capacity(images.len());
    for image in images {
        let (_, scaled_height) =
            kobo_image::width_scaled_size((image.width, image.height), panel_width)
                .map_err(|error| error.to_string())?;
        let start_row = total_rows;
        total_rows = total_rows
            .checked_add(u64::from(scaled_height))
            .ok_or_else(|| "The comic height is not supported.".to_owned())?;
        sources.push(StripSource {
            start_row,
            end_row: total_rows,
        });
    }

    let page_count = total_rows.div_ceil(u64::from(panel_height));
    u16::try_from(page_count).map_err(|_| "The comic has too many pages.".to_owned())?;
    Ok(EpisodeStrip {
        sources,
        total_rows,
    })
}

fn viewport_plan(
    strip: &EpisodeStrip,
    offset_rows: u64,
    panel_height: u32,
) -> Result<ViewportPlan, String> {
    if panel_height == 0 || offset_rows >= strip.total_rows {
        return Err("The comic viewport is outside the episode.".to_owned());
    }
    let viewport_end = offset_rows
        .checked_add(u64::from(panel_height))
        .ok_or_else(|| "The comic viewport interval is not supported.".to_owned())?
        .min(strip.total_rows);
    let content_rows = u32::try_from(
        viewport_end
            .checked_sub(offset_rows)
            .ok_or_else(|| "The comic viewport interval is not supported.".to_owned())?,
    )
    .map_err(|_| "The comic page height is not supported.".to_owned())?;
    let mut segments = Vec::new();
    for (source, interval) in strip
        .sources
        .iter()
        .enumerate()
        .skip_while(|(_, interval)| interval.end_row <= offset_rows)
        .take_while(|(_, interval)| interval.start_row < viewport_end)
    {
        let overlap_start = offset_rows.max(interval.start_row);
        let overlap_end = viewport_end.min(interval.end_row);
        let source_row = overlap_start
            .checked_sub(interval.start_row)
            .ok_or_else(|| "The comic source row is not supported.".to_owned())?;
        let rows = overlap_end
            .checked_sub(overlap_start)
            .ok_or_else(|| "The comic segment height is not supported.".to_owned())?;
        let destination_row = overlap_start
            .checked_sub(offset_rows)
            .ok_or_else(|| "The comic destination row is not supported.".to_owned())?;
        segments.push(ViewportSegment {
            source,
            source_row: u32::try_from(source_row)
                .map_err(|_| "The comic source row is not supported.".to_owned())?,
            rows: u32::try_from(rows)
                .map_err(|_| "The comic segment height is not supported.".to_owned())?,
            destination_row: u32::try_from(destination_row)
                .map_err(|_| "The comic destination row is not supported.".to_owned())?,
        });
    }
    let plan = ViewportPlan {
        segments,
        content_rows,
    };
    validate_continuous_viewport(&plan)?;
    Ok(plan)
}

fn row_byte_offset(row: u32, width: u32, format: PictureFormat) -> Option<usize> {
    usize::try_from(row)
        .ok()?
        .checked_mul(usize::try_from(width).ok()?)?
        .checked_mul(format.bytes_per_pixel())
}

fn copy_source_into_builds(
    source_index: usize,
    source: &Picture,
    builds: &mut [ViewportBuild],
    panel_width: u32,
    panel_height: u32,
) -> Result<(), String> {
    for build in builds {
        let Some(segment) = build.plan.segments.get(build.next_segment) else {
            continue;
        };
        if segment.source != source_index {
            continue;
        }
        if source.format() != build.format {
            return Err("The comic source format does not match the viewport build.".to_owned());
        }
        if source.width() != panel_width {
            return Err("The scaled comic image width does not match the panel.".to_owned());
        }
        let expected_build_len = build
            .format
            .byte_len(panel_width, panel_height)
            .ok_or_else(|| "The comic viewport byte length is not supported.".to_owned())?;
        if build.bytes.len() != expected_build_len {
            return Err("The comic viewport pixels do not match their dimensions.".to_owned());
        }

        let source_bytes = match source.pixels() {
            PicturePixelsRef::Gray8(bytes) if build.format == PictureFormat::Gray8 => bytes,
            PicturePixelsRef::Rgb8(bytes) if build.format == PictureFormat::Rgb8 => bytes,
            _ => {
                return Err("The comic source format does not match the viewport build.".to_owned());
            }
        };
        let expected_source_len = build
            .format
            .byte_len(source.width(), source.height())
            .ok_or_else(|| "The comic source byte length is not supported.".to_owned())?;
        if source_bytes.len() != expected_source_len {
            return Err("The comic source pixels do not match their dimensions.".to_owned());
        }

        let copied_len = row_byte_offset(segment.rows, panel_width, build.format)
            .ok_or_else(|| "The comic segment byte length is not supported.".to_owned())?;
        let source_start = row_byte_offset(segment.source_row, panel_width, build.format)
            .ok_or_else(|| "The comic source byte offset is not supported.".to_owned())?;
        let source_end = source_start
            .checked_add(copied_len)
            .ok_or_else(|| "The comic source byte interval is not supported.".to_owned())?;
        let destination_start = row_byte_offset(segment.destination_row, panel_width, build.format)
            .ok_or_else(|| "The comic viewport byte offset is not supported.".to_owned())?;
        let destination_end = destination_start
            .checked_add(copied_len)
            .ok_or_else(|| "The comic viewport byte interval is not supported.".to_owned())?;
        let source_rows = source_bytes.get(source_start..source_end).ok_or_else(|| {
            "The comic source pixels do not cover the planned segment.".to_owned()
        })?;
        let destination = build
            .bytes
            .get_mut(destination_start..destination_end)
            .ok_or_else(|| {
                "The comic viewport pixels do not cover the planned segment.".to_owned()
            })?;
        destination.copy_from_slice(source_rows);
        build.next_segment = build
            .next_segment
            .checked_add(1)
            .ok_or_else(|| "The comic viewport has too many segments.".to_owned())?;
    }
    Ok(())
}

fn finish_build(
    build: ViewportBuild,
    panel_width: u32,
    panel_height: u32,
) -> Result<Picture, String> {
    if build.next_segment != build.plan.segments.len() {
        return Err("The comic viewport build is incomplete.".to_owned());
    }
    let pixels = match build.format {
        PictureFormat::Gray8 => PicturePixels::Gray8(build.bytes),
        PictureFormat::Rgb8 => PicturePixels::Rgb8(build.bytes),
    };
    let mut picture = Picture::from_pixels(panel_width, panel_height, pixels)
        .map_err(|error| error.to_string())?;
    if picture.format() == PictureFormat::Gray8 {
        picture
            .dither(PANEL_GREYS)
            .map_err(|error| error.to_string())?;
    }
    Ok(picture)
}

fn decode_reader_source(
    bytes: &[u8],
    expected: &EpisodeImage,
    format: PictureFormat,
    panel_width: u32,
) -> Result<Picture, String> {
    let decoded = kobo_image::decode_webp(bytes, format).map_err(|error| error.to_string())?;
    if (decoded.width(), decoded.height()) != (expected.width, expected.height) {
        return Err("BOMTOON returned different comic image dimensions.".to_owned());
    }
    let source = decoded
        .scale_to_width(panel_width)
        .map_err(|error| error.to_string())?;
    let expected_scaled =
        kobo_image::width_scaled_size((expected.width, expected.height), panel_width)
            .map_err(|error| error.to_string())?;
    if (source.width(), source.height()) != expected_scaled {
        return Err("The scaled comic image dimensions do not match the page plan.".to_owned());
    }
    Ok(source)
}

fn same_assets(current: &[EpisodeImage], refreshed: &[EpisodeImage]) -> bool {
    current.len() == refreshed.len()
        && current.iter().zip(refreshed).all(|(old, new)| {
            old.order == new.order
                && old.width == new.width
                && old.height == new.height
                && old.path == new.path
        })
}

fn page_bounds(page: usize, count: usize, items_per_page: usize) -> (usize, usize) {
    let start = page.saturating_mul(items_per_page).min(count);
    (start, start.saturating_add(items_per_page).min(count))
}

fn main() -> ExitCode {
    match kobo_sdk::run("bomtoon", Bomtoon::default()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bomtoon: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kobo_sdk::{
        AppRunner, Chrome, Command, DisplayMetrics, Lifecycle, Node, Overlay, PictureHandle,
        ReadingChrome, SecretHeader, StoreError, StoreRequest, StoreResult, Task, TilePicture,
        CLARA_BW_METRICS,
    };

    const LIBRARY_RESPONSE: &[u8] = br#"{
        "result":"SUCCESS",
        "data":{
            "content":[{
                "alias":"hunter_q",
                "title":"Hunter Q",
                "creators":"Hunter Writer | Hunter Artist",
                "collectionCount":1,
                "episodeCount":2
            }],
            "number":0,
            "totalPages":1,
            "totalElements":1
        }
    }"#;
    const RECENT_RESPONSE: &[u8] = br#"{
        "result":"SUCCESS",
        "data":{
            "content":[{
                "alias":"hunter_q",
                "title":"Hunter Q",
                "creators":"Hunter Writer | Hunter Artist",
                "episode":{"alias":"60","title":"Episode 60"}
            }],
            "number":0,
            "totalPages":1,
            "totalElements":1
        }
    }"#;
    const ASSET_RESPONSE: &[u8] = br#"{
        "result":"SUCCESS",
        "data":{
            "coinBalance":{"coin":7,"bonusCoin":2,"freeCoin":1},
            "ticketBalance":{"ticket":3,"bonusTicket":1,"freeTicket":0}
        }
    }"#;
    const COIN_HISTORY_RESPONSE: &[u8] = br#"{
        "result":"SUCCESS",
        "data":[{
            "title":"Monthly coins",
            "coin":5,
            "coinExpiredAt":1819728000000,
            "bonusCoin":0,
            "freeCoin":0
        }]
    }"#;
    const EMPTY_HISTORY_RESPONSE: &[u8] = br#"{"result":"SUCCESS","data":[]}"#;
    const MULTI_TICKET_HISTORY_RESPONSE: &[u8] = br#"{
        "result":"SUCCESS",
        "data":[
            {
                "title":"First grant",
                "ticket":11,
                "ticketExpiredAt":1819728000000,
                "bonusTicket":12,
                "bonusTicketExpiredAt":0,
                "freeTicket":0
            },
            {
                "title":"Second grant",
                "ticket":21,
                "ticketExpiredAt":1819728000000,
                "bonusTicket":0,
                "freeTicket":22,
                "freeTicketExpiredAt":0
            }
        ]
    }"#;
    const REMOTE_LIBRARY_RESPONSE: &[u8] = br#"{
        "result":"SUCCESS",
        "data":{
            "content":[{
                "alias":"remote-first",
                "title":"Remote First",
                "creators":"Remote Creator",
                "collectionCount":1,
                "episodeCount":1
            }],
            "number":1,
            "totalPages":2,
            "totalElements":31
        }
    }"#;
    const REMOTE_LIBRARY_PAGE_SIZE: usize = 30;

    fn detail_response(id: usize, alias: &str, title: &str, episodes: &str) -> Vec<u8> {
        format!(
            concat!(
                "<script id=\"__NEXT_DATA__\">",
                "{{\"props\":{{\"pageProps\":{{",
                "\"ssrPersonalized\":true,",
                "\"ssrDetail\":{{\"id\":{id},\"alias\":\"{alias}\",",
                "\"title\":\"{title}\",",
                "\"creators\":[{{\"name\":\"Writer\"}}],",
                "\"synopsis\":\"Synopsis\",",
                "\"episodes\":[{episodes}]}}",
                "}}}}}}",
                "</script>"
            ),
            id = id,
            alias = alias,
            title = title,
            episodes = episodes,
        )
        .into_bytes()
    }

    fn content_response() -> Vec<u8> {
        detail_response(
            41,
            "hunter_q",
            "Localized Title",
            concat!(
                "{\"id\":101,\"alias\":\"ep-1\",\"title\":\"Episode One\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"POSSESSION\"},",
                "{\"id\":102,\"alias\":\"ep-2\",\"title\":\"Episode Two\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":null,\"paid\":false},",
                "{\"id\":103,\"alias\":\"sample\",\"title\":\"Sample\",\"openedAt\":1709136000000,\"isSample\":true,\"purchaseStatus\":null},",
                "{\"id\":104,\"alias\":\"rented\",\"title\":\"Rented Episode\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"RENT\"},",
                "{\"id\":105,\"alias\":\"paid\",\"title\":\"Paid Episode\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"NONE\",\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2}"
            ),
        )
    }

    fn coin_only_content_response() -> Vec<u8> {
        detail_response(
            42,
            "hunter_q",
            "Coin Only",
            concat!(
                "{\"id\":201,\"alias\":\"owned\",\"title\":\"Owned Episode\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"POSSESSION\"},",
                "{\"id\":202,\"alias\":\"coin\",\"title\":\"Coin Episode\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"NONE\",\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2}"
            ),
        )
    }
    const TINY_WEBP: &[u8] = &[
        82, 73, 70, 70, 36, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 32, 24, 0, 0, 0, 48, 1, 0, 157, 1,
        42, 1, 0, 1, 0, 1, 64, 38, 37, 164, 0, 3, 112, 0, 254, 251, 148, 0, 0,
    ];
    const RED_1X1_WEBP: &[u8] = &[
        82, 73, 70, 70, 26, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 14, 0, 0, 0, 47, 0, 0, 0, 16,
        205, 85, 32, 34, 2, 209, 255, 136, 4,
    ];
    const BLACK_1X3_WEBP: &[u8] = &[
        82, 73, 70, 70, 68, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 56, 0, 0, 0, 47, 0, 128, 0,
        16, 205, 85, 32, 34, 2, 30, 72, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 128, 136, 72, 1,
    ];
    const WHITE_1X2_WEBP: &[u8] = &[
        82, 73, 70, 70, 68, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 56, 0, 0, 0, 47, 0, 64, 0, 16,
        205, 85, 32, 34, 2, 30, 72, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0, 128, 136, 200, 0,
    ];

    fn image_manifest(path: &str, policy: &str) -> Vec<u8> {
        format!(
            "{{\"result\":\"SUCCESS\",\"data\":[{{\"orderNo\":1,\"width\":1,\"height\":1,\"imagePath\":\"https://image.balcony.studio{path}?Policy={policy}&Signature=s&Key-Pair-Id=k\",\"line\":null,\"point\":null}}]}}"
        )
        .into_bytes()
    }
    fn image_manifest_sources(count: usize) -> Vec<u8> {
        image_manifest_sources_with_policy(count, "p")
    }

    fn image_manifest_sources_with_policy(count: usize, policy: &str) -> Vec<u8> {
        let images = (0..count)
            .map(|source| {
                format!(
                    "{{\"orderNo\":{},\"width\":1,\"height\":1,\"imagePath\":\"https://image.balcony.studio/tw/ep/{source}.webp?Policy={policy}{source}&Signature=s&Key-Pair-Id=k\",\"line\":null,\"point\":null}}",
                    source + 1
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"result\":\"SUCCESS\",\"data\":[{images}]}}").into_bytes()
    }

    fn comment_entry(
        id: usize,
        author: &str,
        text: &str,
        likes: usize,
        replies: usize,
        created_at: i64,
    ) -> String {
        format!(
            concat!(
                "{{\"commentManageId\":40615,\"commentId\":{id},\"title\":\"Hunter Q\",",
                "\"subTitle\":\"Episode One\",\"nickname\":\"{author}\",\"profileImage\":null,",
                "\"isHighLightProfile\":false,\"comment\":\"{text}\",\"commentImagePath\":null,",
                "\"emoticonImageId\":null,\"emoticonUrl\":null,\"emoticonContentsId\":null,",
                "\"replyCount\":{replies},\"likeCount\":{likes},\"createdAt\":{created_at},",
                "\"pageNumber\":0,\"status\":{{\"like\":false,\"delete\":false,",
                "\"blind\":false,\"block\":false,\"report\":false,\"mine\":false}}}}"
            ),
            id = id,
            author = author,
            text = text,
            replies = replies,
            likes = likes,
            created_at = created_at,
        )
    }

    fn comments_response(
        entries: &[String],
        number: usize,
        total_pages: usize,
        total: usize,
    ) -> Vec<u8> {
        let content = entries.join(",");
        format!(
            concat!(
                "{{\"result\":\"SUCCESS\",\"data\":{{\"commentManageId\":40615,",
                "\"contentsIsAdult\":false,\"comment\":{{\"content\":[{content}],",
                "\"pageable\":{{}},\"totalPages\":{total_pages},\"last\":false,",
                "\"totalElements\":{total},\"first\":{first},\"size\":20,\"number\":{number},",
                "\"sort\":{{}},\"numberOfElements\":{count},\"empty\":{empty}}}}}}}"
            ),
            content = content,
            total_pages = total_pages,
            total = total,
            first = number == 0,
            number = number,
            count = entries.len(),
            empty = entries.is_empty(),
        )
        .into_bytes()
    }

    fn replies_response(parent: &str, replies: &[String], total: usize) -> Vec<u8> {
        let replies = replies.join(",");
        let page = format!(
            concat!(
                "{{\"content\":[{replies}],\"pageable\":{{}},\"totalPages\":1,",
                "\"last\":true,\"totalElements\":{total},\"first\":true,\"size\":10,",
                "\"number\":0,\"sort\":{{}},\"numberOfElements\":{count},",
                "\"empty\":{empty}}}"
            ),
            replies = replies,
            total = total,
            count = usize::from(total > 0),
            empty = total == 0,
        );
        [
            r#"{"result":"SUCCESS","data":{"comment":"#,
            parent,
            r#","reply":"#,
            &page,
            "}}",
        ]
        .concat()
        .into_bytes()
    }

    fn reader_metrics(format: PictureFormat, height: i32) -> DisplayMetrics {
        DisplayMetrics {
            width: 1,
            height,
            picture_format: format,
            ..CLARA_BW_METRICS
        }
    }

    fn assert_reader_bounds(app: &Bomtoon) {
        let reader = app.reader.as_ref().expect("reader state");
        assert!(reader.window.len() <= reader.limits.pages);
        assert!(
            reader.source_cache.len() + reader.source_fetches.len() <= reader.limits.source_slots
        );
        assert!(reader.source_fetches.len() <= reader.limits.fetches);
        assert!(app.reader_tasks.len() <= reader.limits.tasks);
    }

    fn episode_image(source: usize, width: u32, height: u32) -> EpisodeImage {
        EpisodeImage {
            order: source + 1,
            width,
            height,
            path: format!("/tw/ep/{source}.webp"),
            url: format!("https://image.balcony.studio/tw/ep/{source}.webp"),
        }
    }

    #[test]
    fn viewport_plan_starts_at_an_arbitrary_strip_row_across_a_seam() {
        let images = [episode_image(0, 2, 5), episode_image(1, 2, 5)];
        let strip = episode_strip(&images, 2, 4).expect("episode strip");

        assert_eq!(strip.total_rows, 10);
        assert_eq!(
            viewport_plan(&strip, 3, 4).expect("viewport plan"),
            ViewportPlan {
                segments: vec![
                    ViewportSegment {
                        source: 0,
                        source_row: 3,
                        rows: 2,
                        destination_row: 0,
                    },
                    ViewportSegment {
                        source: 1,
                        source_row: 0,
                        rows: 2,
                        destination_row: 2,
                    },
                ],
                content_rows: 4,
            }
        );
    }

    #[test]
    fn asymmetric_viewport_turns_are_relative_to_the_current_offset() {
        let first_next = next_viewport_offset(0, 100, 1_000).expect("first next offset");
        let previous = previous_viewport_offset(first_next, 100).expect("previous viewport offset");
        let second_next = next_viewport_offset(previous, 100, 1_000).expect("second next offset");

        assert_eq!((first_next, previous, second_next), (90, 40, 130));
        assert_eq!(previous_viewport_offset(previous, 100), Some(0));
        assert_eq!(previous_viewport_offset(0, 100), None);
        assert_eq!(next_viewport_offset(900, 100, 1_000), None);
    }

    #[test]
    fn reading_progress_uses_the_installed_viewports_visible_end() {
        assert_eq!(reader_progress_percent(0, 100, 1_000), 10);
        assert_eq!(reader_progress_percent(279, 100, 1_000), 37);
        assert_eq!(reader_progress_percent(899, 100, 1_000), 99);
        assert_eq!(reader_progress_percent(900, 100, 1_000), 100);
    }

    #[test]
    fn short_sources_share_a_viewport_without_seam_padding() {
        let images = [episode_image(0, 2, 2), episode_image(1, 2, 2)];
        let strip = episode_strip(&images, 2, 4).expect("episode strip");

        assert_eq!(
            viewport_plan(&strip, 0, 4).expect("viewport plan"),
            ViewportPlan {
                segments: vec![
                    ViewportSegment {
                        source: 0,
                        source_row: 0,
                        rows: 2,
                        destination_row: 0,
                    },
                    ViewportSegment {
                        source: 1,
                        source_row: 0,
                        rows: 2,
                        destination_row: 2,
                    },
                ],
                content_rows: 4,
            }
        );
    }

    #[test]
    fn episode_strip_rejects_zero_dimensions() {
        let valid = episode_image(0, 2, 2);
        assert!(episode_strip(std::slice::from_ref(&valid), 0, 4).is_err());
        assert!(episode_strip(std::slice::from_ref(&valid), 2, 0).is_err());
        assert!(episode_strip(&[episode_image(0, 0, 2)], 2, 4).is_err());
        assert!(episode_strip(&[episode_image(0, 2, 0)], 2, 4).is_err());
    }

    #[test]
    fn cumulative_height_above_u32_uses_checked_global_intervals() {
        let source_height = 1_u32 << 22;
        let source_count = usize::try_from(u64::from(u32::MAX) / u64::from(source_height) + 1)
            .expect("source count");
        let images = (0..source_count)
            .map(|source| episode_image(source, 1, source_height))
            .collect::<Vec<_>>();

        let strip = episode_strip(&images, 1, u32::MAX).expect("u64 episode strip");
        assert_eq!(strip.total_rows, u64::from(u32::MAX) + 1);
        assert_eq!(
            viewport_plan(&strip, u64::from(u32::MAX), u32::MAX).expect("final viewport"),
            ViewportPlan {
                segments: vec![ViewportSegment {
                    source: source_count - 1,
                    source_row: source_height - 1,
                    rows: 1,
                    destination_row: 0,
                }],
                content_rows: 1,
            }
        );
    }

    #[test]
    fn episode_strip_rejects_more_than_u16_virtual_pages() {
        let height = u32::from(u16::MAX) + 1;
        let error = episode_strip(&[episode_image(0, 1, height)], 1, 1)
            .expect_err("virtual page count above u16 must fail");
        assert_eq!(error, "The comic has too many pages.");
    }

    fn seam_plan() -> ViewportPlan {
        ViewportPlan {
            segments: vec![
                ViewportSegment {
                    source: 0,
                    source_row: 0,
                    rows: 1,
                    destination_row: 0,
                },
                ViewportSegment {
                    source: 1,
                    source_row: 0,
                    rows: 1,
                    destination_row: 1,
                },
            ],
            content_rows: 2,
        }
    }

    #[test]
    fn typed_viewport_assembly_gray8_dithers_once_after_the_source_seam() {
        let mut builds = vec![
            ViewportBuild::new(0, seam_plan(), PictureFormat::Gray8, 2, 2).expect("Gray8 build"),
        ];
        let first = Picture::from_grey(2, 1, vec![10, 10]).expect("first source");
        let second = Picture::from_grey(2, 1, vec![20, 20]).expect("second source");

        copy_source_into_builds(0, &first, &mut builds, 2, 2).expect("first segment");
        assert_eq!(builds[0].bytes, [10, 10, 255, 255]);
        assert_eq!(builds[0].next_segment, 1);
        copy_source_into_builds(0, &first, &mut builds, 2, 2).expect("duplicate source is ignored");
        assert_eq!(builds[0].next_segment, 1);
        copy_source_into_builds(1, &second, &mut builds, 2, 2).expect("second segment");
        assert_eq!(builds[0].bytes, [10, 10, 20, 20]);

        let mut expected =
            Picture::from_grey(2, 2, vec![10, 10, 20, 20]).expect("undithered viewport");
        expected.dither(PANEL_GREYS).expect("whole-viewport dither");
        let picture =
            finish_build(builds.pop().expect("build"), 2, 2).expect("finished Gray8 viewport");

        assert_eq!(picture.pixels(), expected.pixels());
    }

    #[test]
    fn typed_viewport_assembly_rgb8_preserves_exact_colors_across_the_source_seam() {
        let mut builds = vec![
            ViewportBuild::new(0, seam_plan(), PictureFormat::Rgb8, 2, 2).expect("RGB8 build"),
        ];
        let red = Picture::from_pixels(2, 1, PicturePixels::Rgb8(vec![255, 0, 0, 255, 0, 0]))
            .expect("red source");
        let blue = Picture::from_pixels(2, 1, PicturePixels::Rgb8(vec![0, 0, 255, 0, 0, 255]))
            .expect("blue source");

        copy_source_into_builds(0, &red, &mut builds, 2, 2).expect("red segment");
        copy_source_into_builds(1, &blue, &mut builds, 2, 2).expect("blue segment");
        let picture =
            finish_build(builds.pop().expect("build"), 2, 2).expect("finished RGB8 viewport");

        assert_eq!(
            picture.pixels(),
            PicturePixelsRef::Rgb8(&[255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255,])
        );
    }

    #[test]
    fn viewport_assembly_final_padding_is_white_and_bytes_per_pixel_correct() {
        let plan = ViewportPlan {
            segments: vec![ViewportSegment {
                source: 0,
                source_row: 0,
                rows: 1,
                destination_row: 0,
            }],
            content_rows: 1,
        };

        let grey = Picture::from_grey(2, 1, vec![0, 0]).expect("Gray8 source");
        let mut grey_builds = vec![
            ViewportBuild::new(0, plan.clone(), PictureFormat::Gray8, 2, 2).expect("Gray8 build"),
        ];
        copy_source_into_builds(0, &grey, &mut grey_builds, 2, 2).expect("Gray8 segment");
        let grey_viewport =
            finish_build(grey_builds.pop().expect("Gray8 build"), 2, 2).expect("Gray8 viewport");
        assert_eq!(
            grey_viewport.pixels(),
            PicturePixelsRef::Gray8(&[0, 0, 255, 255])
        );

        let rgb = Picture::from_pixels(2, 1, PicturePixels::Rgb8(vec![1, 2, 3, 4, 5, 6]))
            .expect("RGB8 source");
        let mut rgb_builds =
            vec![ViewportBuild::new(0, plan, PictureFormat::Rgb8, 2, 2).expect("RGB8 build")];
        copy_source_into_builds(0, &rgb, &mut rgb_builds, 2, 2).expect("RGB8 segment");
        let rgb_viewport =
            finish_build(rgb_builds.pop().expect("RGB8 build"), 2, 2).expect("RGB8 viewport");
        assert_eq!(
            rgb_viewport.pixels(),
            PicturePixelsRef::Rgb8(&[1, 2, 3, 4, 5, 6, 255, 255, 255, 255, 255, 255])
        );
    }

    #[test]
    fn viewport_assembly_format_mismatch_is_refused() {
        let plan = ViewportPlan {
            segments: vec![ViewportSegment {
                source: 0,
                source_row: 0,
                rows: 1,
                destination_row: 0,
            }],
            content_rows: 1,
        };
        let source = Picture::from_grey(2, 1, vec![0, 0]).expect("Gray8 source");
        let mut builds =
            vec![ViewportBuild::new(0, plan, PictureFormat::Rgb8, 2, 1).expect("RGB8 build")];

        assert!(
            copy_source_into_builds(0, &source, &mut builds, 2, 1).is_err(),
            "a Gray8 source must not enter an RGB8 build"
        );
    }

    #[test]
    fn viewport_assembly_wrong_scaled_width_is_refused() {
        let plan = ViewportPlan {
            segments: vec![ViewportSegment {
                source: 0,
                source_row: 0,
                rows: 1,
                destination_row: 0,
            }],
            content_rows: 1,
        };
        let source = Picture::from_grey(1, 1, vec![0]).expect("narrow source");
        let mut builds =
            vec![ViewportBuild::new(0, plan, PictureFormat::Gray8, 2, 1).expect("build")];

        assert!(copy_source_into_builds(0, &source, &mut builds, 2, 1).is_err());
    }

    #[test]
    fn viewport_assembly_truncated_source_rows_are_refused() {
        let plan = ViewportPlan {
            segments: vec![ViewportSegment {
                source: 0,
                source_row: 0,
                rows: 2,
                destination_row: 0,
            }],
            content_rows: 2,
        };
        let source = Picture::from_grey(2, 1, vec![0, 0]).expect("one-row source");
        let mut builds =
            vec![ViewportBuild::new(0, plan, PictureFormat::Gray8, 2, 2).expect("build")];

        assert!(copy_source_into_builds(0, &source, &mut builds, 2, 2).is_err());
    }

    #[test]
    fn viewport_assembly_incomplete_build_is_refused() {
        let mut builds =
            vec![ViewportBuild::new(0, seam_plan(), PictureFormat::Gray8, 2, 2).expect("build")];
        let source = Picture::from_grey(2, 1, vec![0, 0]).expect("first source");
        copy_source_into_builds(0, &source, &mut builds, 2, 2).expect("first segment");

        assert!(finish_build(builds.pop().expect("build"), 2, 2).is_err());
    }

    #[test]
    fn viewport_assembly_rejects_unrepresentable_buffer() {
        assert!(
            ViewportBuild::new(0, seam_plan(), PictureFormat::Rgb8, u32::MAX, u32::MAX,).is_err()
        );
    }

    #[test]
    fn webp_decode_boundary_preserves_format_and_validates_scaled_dimensions() {
        let expected = episode_image(0, 1, 1);
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let source =
                decode_reader_source(TINY_WEBP, &expected, format, 2).expect("bounded WebP");
            assert_eq!((source.width(), source.height()), (2, 2));
            assert_eq!(source.format(), format);
        }
    }

    #[test]
    fn webp_decode_boundary_refuses_wrong_manifest_dimensions_before_scaling() {
        let expected = episode_image(0, 2, 2);
        let error = decode_reader_source(TINY_WEBP, &expected, PictureFormat::Gray8, 0)
            .expect_err("manifest dimensions must be checked before the invalid scale width");

        assert_eq!(error, "BOMTOON returned different comic image dimensions.");
    }

    #[test]
    fn webp_decode_boundary_refuses_non_webp_and_platform_oversize_sources() {
        let png = kobo_image::encode_png_grey(1, 1, &[0]).expect("valid non-WebP picture");
        assert!(
            decode_reader_source(&png, &episode_image(0, 1, 1), PictureFormat::Gray8, 1,).is_err()
        );
        let oversized = vec![0; 4 * 1024 * 1024 + 1];
        let error =
            decode_reader_source(&oversized, &episode_image(0, 1, 1), PictureFormat::Gray8, 1)
                .expect_err("the platform source-byte bound must run before WebP parsing");
        assert_eq!(
            error,
            "the picture is 4194305 bytes, and 4194304 is the most that is read"
        );
    }

    fn started() -> (AppRunner<Bomtoon>, Vec<Command>) {
        let mut runner = AppRunner::with_metrics(Bomtoon::default(), CLARA_BW_METRICS);
        let commands = runner.start();
        (runner, commands)
    }

    fn started_ready_for_homepage() -> (AppRunner<Bomtoon>, TaskId) {
        let (mut runner, commands) = started();
        let homepage = fetch_task_with(&commands, "/comic/main").0;
        let scope = scope_task(&commands);
        runner.task_outcome(
            scope,
            TaskOutcome::Completed(b"00112233445566778899aabbccddeeff".to_vec()),
        );
        for source in FEATURE_SOURCES {
            settle_source(
                &mut runner,
                source,
                TaskOutcome::Completed(empty_source_response(source)),
            );
        }
        let library = fetch_task_with(&commands, "/library?").0;
        let summary = fetch_task_with(&commands, "/asset/user").0;
        runner.task_outcome(library, TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()));
        runner.task_outcome(summary, TaskOutcome::Completed(ASSET_RESPONSE.to_vec()));
        (runner, homepage)
    }

    fn spawns(commands: &[Command]) -> Vec<(TaskId, Task)> {
        commands
            .iter()
            .filter_map(|command| match command {
                Command::Spawn { task, work } => Some((*task, work.clone())),
                _ => None,
            })
            .collect()
    }

    fn scope_task(commands: &[Command]) -> TaskId {
        spawns(commands)
            .into_iter()
            .find_map(|(task, work)| (work == api::account_scope()).then_some(task))
            .expect("credential scope task")
    }

    fn assert_no_post_or_marker_forget(commands: &[Command]) {
        assert!(
            commands.iter().all(|command| !matches!(
                command,
                Command::Spawn {
                    work: Task::Post { .. },
                    ..
                }
            )),
            "commerce emitted POST: {commands:?}"
        );
        assert!(
            commands.iter().all(|command| !matches!(
                command,
                Command::Store(StoreRequest::Forget { key }) if key == commerce::MARKER_KEY
            )),
            "commerce forgot its marker: {commands:?}"
        );
    }

    #[test]
    fn startup_preserves_scope_wallet_library_and_one_feature_source() {
        let (runner, commands) = started();
        let spawned = spawns(&commands);

        assert_eq!(runner.app().account, AccountState::Checking);
        assert_eq!(runner.app().connection, ConnectionState::Unknown);
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::LoadingSafetyState
        );
        assert_eq!(spawned.len(), 4);
        for work in [
            api::account_scope(),
            api::asset_summary(),
            api::library(0),
            api::homepage(),
        ] {
            assert!(spawned.iter().any(|(_, spawned)| *spawned == work));
        }
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::Store(StoreRequest::Load { key }) if key == commerce::MARKER_KEY
        )));
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn startup_marker_then_scope_settles_safety_without_early_commerce() {
        let (mut runner, commands) = started();
        let scope = scope_task(&commands);

        let marker_commands = runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::LoadingSafetyState
        );
        assert_no_post_or_marker_forget(&marker_commands);

        let scope_commands = runner.task_outcome(
            scope,
            TaskOutcome::Completed(b"00112233445566778899aabbccddeeff".to_vec()),
        );
        assert_eq!(runner.app().account, AccountState::Active);
        assert_eq!(runner.app().connection, ConnectionState::Online);
        assert_eq!(runner.app().commerce.state(), commerce::CommerceState::Idle);
        assert_no_post_or_marker_forget(&scope_commands);
    }

    #[test]
    fn startup_scope_then_marker_settles_safety_without_early_commerce() {
        let (mut runner, commands) = started();
        let scope = scope_task(&commands);

        let scope_commands = runner.task_outcome(
            scope,
            TaskOutcome::Completed(b"00112233445566778899aabbccddeeff".to_vec()),
        );
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::LoadingSafetyState
        );
        assert_no_post_or_marker_forget(&scope_commands);

        let marker_commands = runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });
        assert_eq!(runner.app().account, AccountState::Active);
        assert_eq!(runner.app().connection, ConnectionState::Online);
        assert_eq!(runner.app().commerce.state(), commerce::CommerceState::Idle);
        assert_no_post_or_marker_forget(&marker_commands);
    }

    fn test_scope(bytes: &[u8; 32]) -> commerce::AccountScope {
        commerce::AccountScope::from_bytes(bytes).expect("test account scope")
    }

    fn marker_for(scope: commerce::AccountScope) -> Vec<u8> {
        commerce::encode_marker(&commerce::UnresolvedMutationV1 {
            account_scope: scope,
            title_id: 41,
            title_alias: "hunter_q".to_owned(),
            episode_id: 105,
            episode_alias: "paid".to_owned(),
            purchase_type: model::PurchaseType::Rent,
            quoted_price: 1,
            pre_mutation_spendable_coin: Some(10),
            pre_mutation_title_gifts: None,
        })
        .expect("test unresolved marker")
    }

    fn startup_with_marker(value: Option<Vec<u8>>) -> (AppRunner<Bomtoon>, TaskId) {
        let (mut runner, commands) = started();
        let scope = scope_task(&commands);
        let marker_commands = runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value,
        });
        assert_no_post_or_marker_forget(&marker_commands);
        (runner, scope)
    }

    #[test]
    fn account_scope_same_marker_reconciles_without_post_or_forget() {
        const SCOPE: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        let (mut runner, task) = startup_with_marker(Some(marker_for(test_scope(SCOPE))));

        let commands = runner.task_outcome(task, TaskOutcome::Completed(SCOPE.to_vec()));

        assert_eq!(runner.app().account, AccountState::Active);
        assert_eq!(runner.app().connection, ConnectionState::Online);
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::Reconciling
        );
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn account_scope_different_marker_allows_reading_but_locks_commerce() {
        const OWNER: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        const READER: &[u8; 32] = b"ffeeddccbbaa99887766554433221100";
        let (mut runner, task) = startup_with_marker(Some(marker_for(test_scope(OWNER))));

        let commands = runner.task_outcome(task, TaskOutcome::Completed(READER.to_vec()));

        assert_eq!(runner.app().account, AccountState::Active);
        assert_eq!(runner.app().connection, ConnectionState::Online);
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::AcceptedButStale
        );
        assert!(runner.app().commerce.marker_belongs_to_another_account());
        assert_no_post_or_marker_forget(&commands);
        for source in FEATURE_SOURCES {
            settle_source(
                &mut runner,
                source,
                TaskOutcome::Completed(empty_source_response(source)),
            );
        }

        seed_all_account_data(&mut runner);
        let app = runner.app_mut();
        app.view = View::Episodes;
        app.selected_content_alias = "hunter_q".to_owned();
        app.page = 0;
        app.pending = None;
        app.task = None;
        app.episodes.push(Episode {
            id: 105,
            alias: "paid".to_owned(),
            title: "Paid Episode".to_owned(),
            opened_at: 1_709_136_000_000,
            thumbnail_url: None,
            purchase: model::PurchaseState::NotOwned,
            rent_expires_at: None,
            rent_coin: Some(1),
            purchase_coin: Some(2),
            gift_eligible: true,
        });
        app.prepare_episode_layout();

        let screen = runner.app().screen();
        let drawn = format!("{screen:?}");
        assert!(
            drawn.contains(
                "A purchase is unresolved for another account. Restore the original account"
            ),
            "{drawn}"
        );
        let rows = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("episode rows");
        assert!(rows.iter().any(|row| row.action == action_id("episode-0")));
        assert!(rows.iter().any(|row| {
            row.title == "Paid Episode"
                && row.action == action_id("episode-disabled-1")
                && row.trailing.is_none()
        }));

        let commands = runner.action(action_id("episode-1"));
        assert!(spawns(&commands).is_empty(), "{commands:?}");
        assert!(commands
            .iter()
            .all(|command| !matches!(command, Command::Store(_))));
        assert_no_post_or_marker_forget(&commands);
        assert_eq!(runner.app().view, View::Episodes);

        let commands = runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Main);
        assert!(spawns(&commands).is_empty(), "{commands:?}");
        assert_no_post_or_marker_forget(&commands);

        runner.app_mut().view = View::Episodes;
        let commands = runner.action(action_id("episode-0"));
        let (_, work) = only_spawn(&commands);
        assert_eq!(work, api::images("hunter_q", "ep-1", 1072));
        assert_eq!(runner.app().view, View::Reader);
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn account_scope_credential_failures_map_without_clearing_marker() {
        const OWNER: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        for (error, expected) in [
            (TaskError::NoCredential, AccountState::SignedOut),
            (TaskError::Unauthorized, AccountState::Expired),
        ] {
            let (mut runner, task) = startup_with_marker(Some(marker_for(test_scope(OWNER))));

            let commands = runner.task_outcome(task, TaskOutcome::Failed(error));

            assert_eq!(runner.app().account, expected);
            assert_eq!(runner.app().connection, ConnectionState::Online);
            assert_no_post_or_marker_forget(&commands);
        }
    }

    #[test]
    fn account_scope_legacy_credential_keeps_reading_and_locks_commerce() {
        const OWNER: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        let (mut runner, task) = startup_with_marker(Some(marker_for(test_scope(OWNER))));

        let commands = runner.task_outcome(task, TaskOutcome::Failed(TaskError::Denied));

        assert_eq!(runner.app().account, AccountState::Active);
        assert_eq!(runner.app().connection, ConnectionState::Online);
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::LoadingSafetyState
        );
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn offline_cold_start_keeps_auth_unknown_and_marker_intact() {
        const OWNER: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        let (mut runner, task) = startup_with_marker(Some(marker_for(test_scope(OWNER))));

        let commands = runner.task_outcome(task, TaskOutcome::Failed(TaskError::Offline));

        assert_eq!(runner.app().account, AccountState::Checking);
        assert_eq!(runner.app().connection, ConnectionState::Offline);
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::LoadingSafetyState
        );
        assert_no_post_or_marker_forget(&commands);
        assert!(
            format!("{:?}", last_screen(&commands)).contains("Join Wi-Fi"),
            "cold-start offline did not expose the standard network recovery surface"
        );
        for cancelled in cancelled_tasks(&commands) {
            runner.task_outcome(cancelled, TaskOutcome::Cancelled);
        }
        let retry_commands = runner.action(action_id(RETRY));
        let mut resumed = Vec::new();
        for cancelled in cancelled_tasks(&retry_commands) {
            resumed.extend(runner.task_outcome(cancelled, TaskOutcome::Cancelled));
        }
        assert_eq!(runner.app().account, AccountState::Checking);
        assert_eq!(runner.app().connection, ConnectionState::Unknown);
        assert!(spawns(&retry_commands)
            .into_iter()
            .chain(spawns(&resumed))
            .any(|(_, work)| work == api::account_scope()));
        assert!(retry_commands.iter().any(|command| matches!(
            command,
            Command::Store(StoreRequest::Load { key }) if key == commerce::MARKER_KEY
        )));
        assert_no_post_or_marker_forget(&retry_commands);
    }

    #[test]
    fn offline_warm_session_keeps_loaded_reading_and_locks_commerce() {
        const SCOPE: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        let (mut runner, task) = startup_with_marker(None);
        runner.task_outcome(task, TaskOutcome::Completed(SCOPE.to_vec()));
        runner.app_mut().comics.push(Comic {
            alias: "kept".to_owned(),
            title: "Kept".to_owned(),
            creators: String::new(),
            cover_url: None,
            owned_episodes: 1,
            total_episodes: 1,
        });
        let summary = runner
            .app()
            .wallet
            .summary_task
            .expect("startup wallet task");

        let commands = runner.task_outcome(summary, TaskOutcome::Failed(TaskError::Offline));

        assert_eq!(runner.app().account, AccountState::Active);
        assert_eq!(runner.app().connection, ConnectionState::Offline);
        assert_eq!(runner.app().comics.len(), 1);
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn offline_late_unowned_task_cannot_lock_a_new_session() {
        const SCOPE: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        let (mut runner, task) = startup_with_marker(None);
        runner.task_outcome(task, TaskOutcome::Completed(SCOPE.to_vec()));
        assert_eq!(runner.app().connection, ConnectionState::Online);

        let commands = runner.task_outcome(TaskId(999), TaskOutcome::Failed(TaskError::Offline));

        assert_eq!(runner.app().connection, ConnectionState::Online);
        assert_no_post_or_marker_forget(&commands);
    }

    fn commerce_quote() -> model::Quote {
        model::Quote {
            content_id: 41,
            episode_id: 105,
            content_alias: "hunter_q".to_owned(),
            episode_alias: "paid".to_owned(),
            is_available: false,
            coin_kind: "COIN".to_owned(),
            rent_coin: 1,
            possession_coin: 2,
            permanent_coin: Some(2),
            is_rent_gift: true,
            is_possession_gift: false,
        }
    }

    fn commerce_selection() -> commerce::Selection {
        commerce::Selection {
            title_id: 41,
            title_alias: "hunter_q".to_owned(),
            episode_id: 105,
            episode_alias: "paid".to_owned(),
        }
    }

    fn choosing_commerce(scope: commerce::AccountScope) -> commerce::Commerce {
        let mut commerce = commerce::Commerce::new();
        commerce.safety_changed(
            commerce::Authentication::Authenticated(scope),
            commerce::Connectivity::Online,
        );
        commerce.marker_loaded(None);
        commerce.begin_quote(commerce_selection(), model::PurchaseType::Rent);
        commerce.quote_received(commerce_quote(), Some(10), Some(0), false);
        assert_eq!(commerce.state(), commerce::CommerceState::Choosing);
        commerce
    }

    fn persisting_commerce(scope: commerce::AccountScope) -> commerce::Commerce {
        let mut commerce = commerce::Commerce::new();
        commerce.safety_changed(
            commerce::Authentication::Authenticated(scope),
            commerce::Connectivity::Online,
        );
        commerce.marker_loaded(None);
        commerce.begin_quote(commerce_selection(), model::PurchaseType::Rent);
        commerce.quote_received(commerce_quote(), Some(10), Some(0), false);
        commerce.choose(commerce::Action::Rent);
        let effects = commerce.quote_received(commerce_quote(), Some(10), Some(0), false);
        assert!(matches!(
            effects.command,
            Some(commerce::CommerceCommand::SaveMarker(_))
        ));
        commerce
    }

    fn clearing_commerce(scope: commerce::AccountScope) -> commerce::Commerce {
        let mut commerce = persisting_commerce(scope);
        assert!(matches!(
            commerce.marker_saved(commerce::MARKER_KEY).command,
            Some(commerce::CommerceCommand::Post(_))
        ));
        assert!(matches!(
            commerce
                .mutation_finished(commerce::PostOutcome::ExplicitRejection)
                .command,
            Some(commerce::CommerceCommand::ForgetMarker)
        ));
        commerce
    }

    #[test]
    fn unresolved_marker_denied_load_locks_in_both_callback_orders() {
        const SCOPE: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        for scope_first in [false, true] {
            let (mut runner, commands) = started();
            let scope = scope_task(&commands);
            let mut observed = Vec::new();
            if scope_first {
                observed.extend(runner.task_outcome(scope, TaskOutcome::Completed(SCOPE.to_vec())));
            }
            observed.extend(runner.store_result(StoreResult::Denied(StoreError::Unwritable)));
            if !scope_first {
                observed.extend(runner.task_outcome(scope, TaskOutcome::Completed(SCOPE.to_vec())));
            }

            assert_eq!(
                runner.app().commerce.state(),
                commerce::CommerceState::AcceptedButStale
            );
            assert_no_post_or_marker_forget(&observed);
        }
    }

    #[test]
    fn unresolved_marker_load_requires_exact_key_and_operation() {
        let (mut runner, _) = started();

        runner.store_result(StoreResult::Loaded {
            key: "other".to_owned(),
            value: None,
        });
        assert_eq!(runner.app().marker_store, Some(MarkerStoreOperation::Load));
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::LoadingSafetyState
        );

        runner.store_result(StoreResult::Saved {
            key: commerce::MARKER_KEY.to_owned(),
        });
        assert_eq!(runner.app().marker_store, Some(MarkerStoreOperation::Load));
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::LoadingSafetyState
        );

        runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });
        assert_eq!(runner.app().marker_store, None);
    }

    #[test]
    fn unresolved_marker_store_callbacks_require_exact_key_and_operation() {
        let scope = test_scope(b"00112233445566778899aabbccddeeff");
        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::Active,
            connection: ConnectionState::Online,
            account_scope: Some(scope),
            commerce: persisting_commerce(scope),
            marker_store: Some(MarkerStoreOperation::Save),
            ..Bomtoon::default()
        });

        runner.store_result(StoreResult::Saved {
            key: "other".to_owned(),
        });
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::PersistingIntent
        );
        assert_eq!(runner.app().marker_store, Some(MarkerStoreOperation::Save));

        runner.store_result(StoreResult::Forgotten {
            key: commerce::MARKER_KEY.to_owned(),
        });
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::PersistingIntent
        );
        assert_eq!(runner.app().marker_store, Some(MarkerStoreOperation::Save));

        runner.store_result(StoreResult::Saved {
            key: commerce::MARKER_KEY.to_owned(),
        });
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::Mutating
        );
        assert_eq!(runner.app().marker_store, None);
    }

    #[test]
    fn unresolved_marker_denied_save_emits_no_post() {
        let scope = test_scope(b"00112233445566778899aabbccddeeff");
        let (mut runner, _) = started();
        runner.app_mut().commerce = persisting_commerce(scope);
        runner.app_mut().marker_store = Some(MarkerStoreOperation::Save);

        let commands = runner.store_result(StoreResult::Denied(StoreError::Unwritable));

        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::Choosing
        );
        assert_eq!(runner.app().marker_store, None);
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn unresolved_marker_denied_forget_stays_locked_and_matching_forget_unlocks() {
        let scope = test_scope(b"00112233445566778899aabbccddeeff");
        let (mut denied, _) = started();
        denied.app_mut().commerce = clearing_commerce(scope);
        denied.app_mut().marker_store = Some(MarkerStoreOperation::Forget);

        let commands = denied.store_result(StoreResult::Denied(StoreError::Unwritable));
        assert_eq!(
            denied.app().commerce.state(),
            commerce::CommerceState::AcceptedButStale
        );
        assert_eq!(denied.app().marker_store, None);
        assert_no_post_or_marker_forget(&commands);

        let (mut forgotten, _) = started();
        forgotten.app_mut().commerce = clearing_commerce(scope);
        forgotten.app_mut().marker_store = Some(MarkerStoreOperation::Forget);
        forgotten.store_result(StoreResult::Forgotten {
            key: commerce::MARKER_KEY.to_owned(),
        });
        assert_eq!(
            forgotten.app().commerce.state(),
            commerce::CommerceState::Idle
        );
        assert_eq!(forgotten.app().marker_store, None);
    }

    #[test]
    fn unresolved_marker_interrupted_save_and_post_reload_without_second_post() {
        const SCOPE: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        let scope = test_scope(SCOPE);
        let marker = marker_for(scope);
        for commerce in [persisting_commerce(scope), {
            let mut commerce = persisting_commerce(scope);
            commerce.marker_saved(commerce::MARKER_KEY);
            commerce
        }] {
            let (mut interrupted, _) = started();
            interrupted.app_mut().commerce = commerce;
            interrupted.app_mut().account_scope = Some(scope);
            let exit_commands = interrupted.exit();
            assert_no_post_or_marker_forget(&exit_commands);

            let (mut restarted, task) = startup_with_marker(Some(marker.clone()));
            let commands = restarted.task_outcome(task, TaskOutcome::Completed(SCOPE.to_vec()));
            assert_eq!(
                restarted.app().commerce.state(),
                commerce::CommerceState::Reconciling
            );
            assert_no_post_or_marker_forget(&commands);
        }
    }

    #[derive(Clone, Copy)]
    enum CommerceInterruption {
        Suspend,
        Background,
        Exit,
    }

    fn interrupt_commerce(
        runner: &mut AppRunner<Bomtoon>,
        interruption: CommerceInterruption,
    ) -> Vec<Command> {
        match interruption {
            CommerceInterruption::Suspend => runner.suspend(),
            CommerceInterruption::Background => runner.lifecycle(Lifecycle::Background),
            CommerceInterruption::Exit => runner.exit(),
        }
    }

    #[test]
    fn unresolved_marker_lifecycle_cancels_scope_without_forgetting_marker() {
        for interruption in [
            CommerceInterruption::Suspend,
            CommerceInterruption::Background,
            CommerceInterruption::Exit,
        ] {
            let (mut runner, commands) = started();
            let scope = scope_task(&commands);
            let commands = interrupt_commerce(&mut runner, interruption);
            assert!(commands.contains(&Command::Cancel(scope)));
            assert_eq!(runner.app().scope_task, None);
            assert_no_post_or_marker_forget(&commands);
        }
    }

    #[test]
    fn unresolved_marker_lifecycle_clears_volatile_commerce_without_forget() {
        let scope = test_scope(b"00112233445566778899aabbccddeeff");
        for interruption in [
            CommerceInterruption::Suspend,
            CommerceInterruption::Background,
            CommerceInterruption::Exit,
        ] {
            let (mut runner, _) = started();
            runner.app_mut().account = AccountState::Active;
            runner.app_mut().connection = ConnectionState::Online;
            runner.app_mut().account_scope = Some(scope);
            runner.app_mut().commerce = choosing_commerce(scope);

            let commands = interrupt_commerce(&mut runner, interruption);

            assert_eq!(
                runner.app().commerce.state(),
                commerce::CommerceState::LoadingSafetyState
            );
            assert_no_post_or_marker_forget(&commands);
        }
    }

    #[test]
    fn unresolved_marker_logout_preserves_marker_and_clears_commerce_access() {
        const SCOPE: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        let scope = test_scope(SCOPE);
        let (mut runner, scope_task) = startup_with_marker(Some(marker_for(scope)));
        runner.task_outcome(scope_task, TaskOutcome::Completed(SCOPE.to_vec()));
        let library = runner.app().task.expect("startup library task");
        runner.task_outcome(library, TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()));
        runner.app_mut().view = View::Account;
        let commands = runner.action(action_id(SIGN_OUT));
        let logout = spawns(&commands)
            .into_iter()
            .find_map(|(task, work)| (work == api::logout()).then_some(task))
            .unwrap_or_else(|| {
                assert_eq!(runner.app().queued_foreground, Some(Pending::Logout));
                let homepage = feature_task(&runner, FeatureSource::Homepage);
                let resumed = runner.task_outcome(
                    homepage,
                    TaskOutcome::Completed(empty_source_response(FeatureSource::Homepage)),
                );
                spawns(&resumed)
                    .into_iter()
                    .find_map(|(task, work)| (work == api::logout()).then_some(task))
                    .unwrap_or_else(|| panic!("logout did not win released capacity: {resumed:?}"))
            });

        let commands = runner.task_outcome(logout, TaskOutcome::Completed(Vec::new()));

        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_eq!(runner.app().account_scope, None);
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::AcceptedButStale
        );
        assert_no_post_or_marker_forget(&commands);
    }

    fn fetch_task_with(commands: &[Command], needle: &str) -> (TaskId, Task) {
        spawns(commands)
            .into_iter()
            .find(|(_, work)| matches!(work, Task::Fetch { url, .. } if url.contains(needle)))
            .unwrap_or_else(|| panic!("matching fetch task {needle}: {commands:?}"))
    }

    fn only_spawn(commands: &[Command]) -> (TaskId, Task) {
        let mut spawned = spawns(commands).into_iter();
        let first = spawned.next().expect("one spawned task");
        assert!(spawned.next().is_none(), "more than one task was spawned");
        first
    }

    fn last_screen(commands: &[Command]) -> Screen {
        commands
            .iter()
            .rev()
            .find_map(|command| match command {
                Command::SetScreen(screen) => Some(screen.clone()),
                _ => None,
            })
            .expect("a screen command")
    }

    fn retry_button_count(screen: &Screen) -> usize {
        screen
            .nodes
            .iter()
            .filter(
                |node| matches!(node, Node::Button { action, .. } if *action == action_id(RETRY)),
            )
            .count()
    }

    fn assert_fits(screen: &Screen) {
        let issues = screen.diagnostics(&CLARA_BW_METRICS, &Chrome::measuring(true));
        assert!(
            !issues.has_errors(),
            "screen does not fit Clara BW: {issues:?}"
        );
    }

    fn assert_login_instructions(screen: &Screen) {
        let drawn = format!("{screen:?}");
        assert!(
            drawn.contains("Run this on your Mac:"),
            "missing operator preface: {drawn}"
        );
        assert!(
            drawn.contains("kobo bomtoon login --device <Kobo IP>"),
            "missing exact login command: {drawn}"
        );
        assert!(drawn.contains("Try again"), "missing retry action: {drawn}");
        assert_fits(screen);
    }

    fn loaded_library_with_metrics(metrics: DisplayMetrics) -> (AppRunner<Bomtoon>, Vec<Command>) {
        let mut runner = AppRunner::with_metrics(Bomtoon::default(), metrics);
        let commands = runner.start();
        let scope = scope_task(&commands);
        runner.task_outcome(
            scope,
            TaskOutcome::Completed(b"00112233445566778899aabbccddeeff".to_vec()),
        );
        for source in FEATURE_SOURCES {
            settle_source(
                &mut runner,
                source,
                TaskOutcome::Completed(empty_source_response(source)),
            );
        }
        let (library, _) = fetch_task_with(&commands, "/library?");
        runner.action(action_id(LIBRARY));
        let commands =
            runner.task_outcome(library, TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()));
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Library);
        (runner, commands)
    }

    fn loaded_library() -> (AppRunner<Bomtoon>, Vec<Command>) {
        loaded_library_with_metrics(CLARA_BW_METRICS)
    }

    fn complete_initial_summary(runner: &mut AppRunner<Bomtoon>) {
        let task = runner
            .app()
            .wallet
            .summary_task
            .expect("initial summary task");
        runner.task_outcome(task, TaskOutcome::Completed(ASSET_RESPONSE.to_vec()));
        assert_eq!(runner.app().wallet.summary, Some(test_wallet_summary()));
    }

    fn test_wallet_summary() -> WalletSummary {
        WalletSummary {
            coins: model::AssetAmounts {
                standard: 7,
                bonus: 2,
                free: 1,
            },
            tickets: model::AssetAmounts {
                standard: 3,
                bonus: 1,
                free: 0,
            },
        }
    }

    fn loaded_shelf() -> ShelfLoadState {
        ShelfLoadState {
            loaded: true,
            ..ShelfLoadState::default()
        }
    }

    fn expiration_row(
        kind: model::AssetKind,
        subtype: model::AssetSubtype,
        quantity: usize,
        expires_at: Option<i64>,
    ) -> ExpirationRow {
        ExpirationRow {
            kind,
            subtype,
            quantity,
            expires_at,
            description: None,
        }
    }

    fn expiration_request_start(work: &Task) -> i64 {
        let Task::Fetch { url, .. } = work else {
            panic!("expected expiration fetch");
        };
        url.split_once("?createdAt=")
            .and_then(|(_, query)| query.split('&').next())
            .and_then(|value| value.parse().ok())
            .expect("createdAt query")
    }

    fn current_time_ms() -> i64 {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time after epoch");
        i64::try_from(elapsed.as_millis()).expect("current milliseconds fit i64")
    }

    fn reader_waiting_for_manifest_with_metrics(
        metrics: DisplayMetrics,
    ) -> (AppRunner<Bomtoon>, TaskId, Vec<Command>) {
        let (mut runner, _) = loaded_library_with_metrics(metrics);
        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);
        runner.task_outcome(content_task, TaskOutcome::Completed(content_response()));
        let commands = runner.action(action_id("episode-0"));
        let (manifest_task, _) = only_spawn(&commands);
        (runner, manifest_task, commands)
    }

    fn reader_waiting_for_manifest() -> (AppRunner<Bomtoon>, TaskId, Vec<Command>) {
        reader_waiting_for_manifest_with_metrics(CLARA_BW_METRICS)
    }

    fn reader_waiting_for_first_image() -> (AppRunner<Bomtoon>, TaskId) {
        let (mut runner, manifest_task, _) = reader_waiting_for_manifest();
        let commands = runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest("/tw/ep/one.webp", "p1")),
        );
        let (image_task, _) = only_spawn(&commands);
        (runner, image_task)
    }

    fn seeded_reader_with_metrics(
        metrics: DisplayMetrics,
        source_count: usize,
        current_viewport: usize,
        chrome_visible: bool,
    ) -> AppRunner<Bomtoon> {
        let width = u32::try_from(metrics.width).expect("positive panel width");
        let panel_height = u32::try_from(metrics.height).expect("positive panel height");
        let format = metrics.picture_format;
        let images = (0..source_count)
            .map(|source| episode_image(source, width, panel_height))
            .collect::<Vec<_>>();
        let strip = episode_strip(&images, width, panel_height).expect("seeded episode strip");
        let offset_rows = u64::try_from(current_viewport)
            .expect("viewport index")
            .saturating_mul(u64::from(panel_height));
        let limits = reader_limits(format);
        let mut window = VecDeque::new();
        let mut next_offset = next_viewport_offset(offset_rows, panel_height, strip.total_rows);
        while let Some(offset_rows) = next_offset {
            if window.len() == limits.pages {
                break;
            }
            let byte_len = format
                .byte_len(width, panel_height)
                .expect("seeded viewport byte length");
            let pixels = match format {
                PictureFormat::Gray8 => PicturePixels::Gray8(vec![127; byte_len]),
                PictureFormat::Rgb8 => PicturePixels::Rgb8(vec![127; byte_len]),
            };
            let picture =
                Picture::from_pixels(width, panel_height, pixels).expect("ready viewport");
            window.push_back(ViewportEntry::Ready {
                offset_rows,
                picture,
            });
            next_offset = next_viewport_offset(offset_rows, panel_height, strip.total_rows);
        }
        AppRunner::with_metrics(
            Bomtoon {
                account: AccountState::Active,
                view: View::Reader,
                selected_content_alias: "hunter_q".to_owned(),
                reader_selection: Some(EpisodeSelection {
                    content_alias: "hunter_q".to_owned(),
                    episode_alias: "ep-1".to_owned(),
                    title: "Episode One".to_owned(),
                }),
                reader: Some(ReaderState {
                    generation: 1,
                    format,
                    limits,
                    panel_width: width,
                    panel_height,
                    images,
                    strip,
                    offset_rows,
                    window,
                    source_cache: BTreeMap::new(),
                    source_fetches: BTreeMap::new(),
                    maintenance_task: None,
                    refresh_task: None,
                    refresh_waiters: BTreeMap::new(),
                    refresh_attempted: BTreeMap::new(),
                    source_failures: BTreeMap::new(),
                    picture: Some(TilePicture::new(PictureHandle(7), width, panel_height)),
                    chrome_visible,
                }),
                reader_generation: 1,
                ..Bomtoon::default()
            },
            metrics,
        )
    }

    fn seeded_reader(
        page_count: usize,
        current_page: usize,
        chrome_visible: bool,
    ) -> AppRunner<Bomtoon> {
        seeded_reader_with_metrics(CLARA_BW_METRICS, page_count, current_page, chrome_visible)
    }

    fn fully_populated_reader(format: PictureFormat) -> (AppRunner<Bomtoon>, [TaskId; 4]) {
        let metrics = reader_metrics(format, 2);
        let mut runner = seeded_reader_with_metrics(metrics, 5, 0, true);
        let foreground = TaskId(41);
        let prefetch = TaskId(42);
        let maintenance = TaskId(43);
        let refresh = TaskId(44);
        let tasks = [foreground, prefetch, maintenance, refresh];
        {
            let app = runner.app_mut();
            app.pending = Some(Pending::Content(99));
            app.task = Some(TaskId(99));
            app.problem = Some("reader failure".to_owned());
            app.retry = Retry::Viewport(0);
            app.reader_tasks = BTreeMap::from([
                (
                    foreground,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::ForegroundSource {
                            source: 0,
                            offset_rows: 0,
                        },
                    },
                ),
                (
                    prefetch,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::PrefetchSource { source: 1 },
                    },
                ),
                (
                    maintenance,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::Maintenance,
                    },
                ),
                (
                    refresh,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::ManifestRefresh,
                    },
                ),
            ]);
            app.foreground_reader_task = Some(foreground);
            let reader = app.reader.as_mut().expect("reader");
            reader.source_fetches = BTreeMap::from([(0, foreground), (1, prefetch)]);
            reader.maintenance_task = Some(maintenance);
            reader.refresh_task = Some(refresh);
            reader
                .refresh_waiters
                .insert(3, FetchIntent::Foreground { offset_rows: 3 });
            reader.refresh_attempted.insert(3, FetchIntent::Prefetch);
            reader.source_failures.insert(
                4,
                SourceFailure {
                    advice: "prefetch failed".to_owned(),
                },
            );
            let source_pixels = match format {
                PictureFormat::Gray8 => PicturePixels::Gray8(vec![63; 2]),
                PictureFormat::Rgb8 => PicturePixels::Rgb8(vec![63; 6]),
            };
            reader.source_cache.insert(
                2,
                Picture::from_pixels(1, 2, source_pixels).expect("cached source"),
            );
            let ready_pixels = match format {
                PictureFormat::Gray8 => PicturePixels::Gray8(vec![127; 2]),
                PictureFormat::Rgb8 => PicturePixels::Rgb8(vec![127; 6]),
            };
            let plan = viewport_plan(&reader.strip, 2, 2).expect("building viewport plan");
            reader.window = VecDeque::from([
                ViewportEntry::Ready {
                    offset_rows: 1,
                    picture: Picture::from_pixels(1, 2, ready_pixels).expect("ready viewport"),
                },
                ViewportEntry::Building(
                    ViewportBuild::new(2, plan, format, 1, 2).expect("building viewport"),
                ),
            ]);
        }
        (runner, tasks)
    }

    fn assert_reader_cleanup(
        runner: &mut AppRunner<Bomtoon>,
        commands: &[Command],
        tasks: [TaskId; 4],
        settled: Option<TaskId>,
        non_reader_pending: Option<Pending>,
        non_reader_task: Option<TaskId>,
        cancelled_non_reader_task: Option<TaskId>,
    ) {
        let app = runner.app();
        assert_eq!(app.reader_generation, 2);
        assert!(app.reader_tasks.is_empty());
        assert!(app.foreground_reader_task.is_none());
        assert!(app.reader.is_none());
        assert!(app.reader_selection.is_none());
        assert!(app.problem.is_none());
        assert_eq!(app.retry, Retry::Restart);
        assert_eq!(app.pending, non_reader_pending);
        assert_eq!(app.task, non_reader_task);

        let cancelled = commands
            .iter()
            .filter_map(|command| match command {
                Command::Cancel(task) => Some(*task),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut expected_cancelled = tasks
            .into_iter()
            .filter(|task| Some(*task) != settled)
            .collect::<BTreeSet<_>>();
        if let Some(task) = cancelled_non_reader_task {
            expected_cancelled.insert(task);
        }
        assert_eq!(cancelled, expected_cancelled);
        assert_eq!(
            commands
                .iter()
                .filter_map(|command| match command {
                    Command::DropPicture(handle) => Some(*handle),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![PictureHandle(7)]
        );
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::Spawn { .. })));

        let generation = runner.app().reader_generation;
        for task in tasks {
            let commands = runner.task_outcome(task, TaskOutcome::Cancelled);
            assert!(commands.is_empty());
            let app = runner.app();
            assert_eq!(app.reader_generation, generation);
            assert!(app.reader_tasks.is_empty());
            assert!(app.reader.is_none());
            assert!(app.reader_selection.is_none());
            assert_eq!(app.pending, non_reader_pending);
            assert_eq!(app.task, non_reader_task);
        }
    }

    fn prepared_reader(format: PictureFormat) -> AppRunner<Bomtoon> {
        let metrics = reader_metrics(format, 1);
        let (mut runner, manifest_task, _) = reader_waiting_for_manifest_with_metrics(metrics);
        runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest_sources(8)),
        );
        let limits = reader_limits(format);
        for _ in 0..32 {
            let settled = {
                let reader = runner.app().reader.as_ref().expect("reader");
                reader.window.len() == limits.pages
                    && reader
                        .window
                        .iter()
                        .all(|entry| matches!(entry, ViewportEntry::Ready { .. }))
                    && runner.app().reader_tasks.is_empty()
            };
            if settled {
                assert_reader_bounds(runner.app());
                return runner;
            }
            let (&task, entry) = runner
                .app()
                .reader_tasks
                .iter()
                .next()
                .expect("reader preparation must have runnable work");
            let bytes = match entry.purpose {
                ReaderTaskPurpose::ForegroundSource { .. }
                | ReaderTaskPurpose::PrefetchSource { .. } => TINY_WEBP.to_vec(),
                ReaderTaskPurpose::Manifest
                | ReaderTaskPurpose::ManifestRefresh
                | ReaderTaskPurpose::Maintenance => Vec::new(),
            };
            runner.task_outcome(task, TaskOutcome::Completed(bytes));
            assert_reader_bounds(runner.app());
        }
        panic!("reader preparation did not settle");
    }

    fn reader_task_completion(purpose: ReaderTaskPurpose) -> Vec<u8> {
        match purpose {
            ReaderTaskPurpose::ForegroundSource { .. }
            | ReaderTaskPurpose::PrefetchSource { .. } => RED_1X1_WEBP.to_vec(),
            ReaderTaskPurpose::Manifest
            | ReaderTaskPurpose::ManifestRefresh
            | ReaderTaskPurpose::Maintenance => Vec::new(),
        }
    }

    fn uploaded_picture(commands: &[Command]) -> (u32, u32, PicturePixels) {
        let uploads = commands
            .iter()
            .filter_map(|command| match command {
                Command::PutPicture {
                    width,
                    height,
                    pixels,
                    ..
                } => Some((
                    *width,
                    *height,
                    match pixels {
                        PicturePixels::Gray8(bytes) => PicturePixels::Gray8(bytes.clone()),
                        PicturePixels::Rgb8(bytes) => PicturePixels::Rgb8(bytes.clone()),
                    },
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(uploads.len(), 1, "expected exactly one picture upload");
        uploads.into_iter().next().expect("picture upload")
    }

    fn strict_picture_png(width: u32, height: u32, pixels: &PicturePixels) -> Vec<u8> {
        let pixels_ref = match pixels {
            PicturePixels::Gray8(bytes) => PicturePixelsRef::Gray8(bytes),
            PicturePixels::Rgb8(bytes) => PicturePixelsRef::Rgb8(bytes),
        };
        let png = kobo_image::encode_png(width, height, pixels_ref).expect("encode evidence PNG");
        let decoded = kobo_image::decode_png(&png).expect("strictly decode evidence PNG");
        assert_eq!((decoded.width(), decoded.height()), (width, height));
        assert_eq!(decoded.pixels(), pixels_ref);
        png
    }

    fn seam_previous_reader(
        format: PictureFormat,
    ) -> (AppRunner<Bomtoon>, TaskId, Option<TaskId>, TaskId) {
        let metrics = reader_metrics(format, 2);
        let images = vec![episode_image(0, 1, 3), episode_image(1, 1, 2)];
        let strip = episode_strip(&images, 1, 2).expect("episode strip");
        assert_eq!(strip.total_rows, 5);
        let first_task = TaskId(41);
        let second_task = TaskId(42);
        let maintenance_task = TaskId(43);
        let mut source_fetches = BTreeMap::from([(0, first_task)]);
        let mut reader_tasks = BTreeMap::from([(
            first_task,
            ReaderTaskEntry {
                generation: 1,
                purpose: ReaderTaskPurpose::PrefetchSource { source: 0 },
            },
        )]);
        let active_second = (format == PictureFormat::Gray8).then_some(second_task);
        if let Some(task) = active_second {
            source_fetches.insert(1, task);
            reader_tasks.insert(
                task,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: ReaderTaskPurpose::PrefetchSource { source: 1 },
                },
            );
        }
        reader_tasks.insert(
            maintenance_task,
            ReaderTaskEntry {
                generation: 1,
                purpose: ReaderTaskPurpose::Maintenance,
            },
        );
        let backward_pixels = match format {
            PictureFormat::Gray8 => PicturePixels::Gray8(vec![127; 2]),
            PictureFormat::Rgb8 => PicturePixels::Rgb8(vec![127; 6]),
        };
        let backward_picture =
            Picture::from_pixels(1, 2, backward_pixels).expect("backward cached page");
        let window = VecDeque::from([ViewportEntry::Ready {
            offset_rows: 4,
            picture: backward_picture,
        }]);
        let picture = TilePicture::new(PictureHandle(7), 1, 2);
        let runner = AppRunner::with_metrics(
            Bomtoon {
                account: AccountState::Active,
                view: View::Reader,
                selected_content_alias: "hunter_q".to_owned(),
                reader_selection: Some(EpisodeSelection {
                    content_alias: "hunter_q".to_owned(),
                    episode_alias: "ep-1".to_owned(),
                    title: "Episode One".to_owned(),
                }),
                reader: Some(ReaderState {
                    generation: 1,
                    format,
                    limits: reader_limits(format),
                    panel_width: 1,
                    panel_height: 2,
                    images,
                    strip,
                    offset_rows: 3,
                    window,
                    source_cache: BTreeMap::new(),
                    source_fetches,
                    maintenance_task: Some(maintenance_task),
                    refresh_task: None,
                    refresh_waiters: BTreeMap::new(),
                    refresh_attempted: BTreeMap::new(),
                    source_failures: BTreeMap::new(),
                    picture: Some(picture),
                    chrome_visible: false,
                }),
                reader_generation: 1,
                reader_tasks,
                next_picture_handle: 8,
                ..Bomtoon::default()
            },
            metrics,
        );
        (runner, first_task, active_second, maintenance_task)
    }

    fn seed_all_account_data(runner: &mut AppRunner<Bomtoon>) {
        let app = runner.app_mut();
        app.recent.push(RecentEntry {
            content_alias: "hunter_q".to_owned(),
            content_title: "Hunter Q".to_owned(),
            creators: String::new(),
            cover_url: None,
            episode_alias: "ep-1".to_owned(),
            episode_title: "Episode One".to_owned(),
        });
        app.episodes.push(Episode {
            id: 101,
            alias: "ep-1".to_owned(),
            title: "Episode One".to_owned(),
            opened_at: 1_709_136_000_000,
            thumbnail_url: None,
            purchase: model::PurchaseState::Owned,
            rent_expires_at: None,
            rent_coin: None,
            purchase_coin: None,
            gift_eligible: false,
        });
        app.selected_title = "Hunter Q".to_owned();
        app.selected_creators = "Writer | Artist".to_owned();
        app.selected_synopsis = "Stored synopsis".to_owned();
        app.synopsis = SynopsisState {
            pages: synopsis_pages(&app.selected_synopsis),
            open_page: None,
            ..SynopsisState::default()
        };
        app.selected_content_id = Some(41);
        app.prepare_episode_layout();
        app.page = 3;
        app.next_library_page = Some(4);
        app.next_recent_page = Some(5);
        app.total_library_titles = 91;
        app.total_recent_titles = 62;
        app.library_load.loaded = true;
        app.recent_load.loaded = true;
    }

    fn assert_all_account_data_cleared(app: &Bomtoon) {
        assert!(app.comics.is_empty());
        assert!(app.recent.is_empty());
        assert!(app.episodes.is_empty());
        assert_eq!(app.selected_content_id, None);
        assert!(app.selected_title.is_empty());
        assert!(app.selected_creators.is_empty());
        assert!(app.selected_synopsis.is_empty());
        assert_eq!(app.synopsis, SynopsisState::default());
        assert_eq!(app.episode_items_per_page, None);
        assert_eq!(app.page, 0);
        assert_eq!(app.next_library_page, None);
        assert_eq!(app.next_recent_page, None);
        assert_eq!(app.total_library_titles, 0);
        assert_eq!(app.total_recent_titles, 0);
        assert!(!app.library_load.loaded);
        assert!(!app.recent_load.loaded);
    }

    fn assert_seeded_account_data_is_kept(app: &Bomtoon) {
        assert_eq!(app.comics.len(), 1);
        assert_eq!(app.recent.len(), 1);
        assert_eq!(app.episodes.len(), 1);
        assert_eq!(app.selected_content_id, Some(41));
        assert_eq!(app.selected_title, "Hunter Q");
        assert_eq!(app.selected_creators, "Writer | Artist");
        assert_eq!(app.selected_synopsis, "Stored synopsis");
        assert!(!app.synopsis.pages.is_empty());
        assert_eq!(app.synopsis.open_page, None);
        assert_eq!(app.page, 3);
        assert_eq!(app.next_library_page, Some(4));
        assert_eq!(app.next_recent_page, Some(5));
        assert_eq!(app.total_library_titles, 91);
        assert_eq!(app.total_recent_titles, 62);
        assert!(app.library_load.loaded);
        assert!(app.recent_load.loaded);
    }

    fn begin_logout(runner: &mut AppRunner<Bomtoon>) -> TaskId {
        runner.app_mut().view = View::Account;
        let commands = runner.action(action_id(SIGN_OUT));
        let (task, work) = only_spawn(&commands);
        assert_eq!(
            work,
            Task::RevokeCredential {
                credential: "bomtoon-access-token".to_owned(),
            }
        );
        task
    }

    fn failed_start(error: TaskError) -> (AppRunner<Bomtoon>, Vec<Command>) {
        let (mut runner, commands) = started();
        let task = scope_task(&commands);
        let commands = runner.task_outcome(task, TaskOutcome::Failed(error));
        (runner, commands)
    }

    fn failed_library_action(action: &str, error: TaskError) -> (AppRunner<Bomtoon>, Vec<Command>) {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id(action));
        let (task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(task, TaskOutcome::Failed(error));
        (runner, commands)
    }

    fn is_homepage_fetch(command: &Command) -> bool {
        matches!(
            command,
            Command::Spawn {
                work: Task::Fetch { url, .. },
                ..
            } if url == "https://www.bomtoon.tw/comic/main"
        )
    }

    fn is_library_or_recent_fetch(command: &Command) -> bool {
        matches!(
            command,
            Command::Spawn {
                work: Task::Fetch { url, .. },
                ..
            } if url.contains("/library?") || url.contains("/recent?")
        )
    }

    #[test]
    fn top_bar_account_is_the_single_coin_action_on_all_signed_in_destinations() {
        for destination in [
            MainDestination::Featured,
            MainDestination::Recent,
            MainDestination::Library,
        ] {
            let screen = Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                destination,
                library_load: loaded_shelf(),
                recent_load: loaded_shelf(),
                wallet: WalletState {
                    summary: Some(test_wallet_summary()),
                    ..WalletState::default()
                },
                ..Bomtoon::default()
            }
            .screen();
            let top_bar = screen.top_bar.as_ref().expect("main top bar");

            assert_eq!(top_bar.title, destination.title());
            assert_eq!(top_bar.actions.len(), 1);
            assert_eq!(top_bar.actions[0].action, action_id(ACCOUNT));
            assert_eq!(top_bar.actions[0].label, "Coins 10");
            assert!(screen.nodes.iter().all(|node| {
                !matches!(
                    node,
                    Node::Button { action, .. }
                        if *action == action_id(ACCOUNT) || *action == action_id(SIGN_OUT)
                )
            }));
        }

        for (wallet, expected) in [
            (
                WalletState {
                    summary_task: Some(TaskId(90)),
                    ..WalletState::default()
                },
                "Coins…",
            ),
            (WalletState::default(), "Coins unavailable"),
        ] {
            let screen = Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                wallet,
                ..Bomtoon::default()
            }
            .screen();
            assert_eq!(
                screen.top_bar.expect("main top bar").actions[0].label,
                expected
            );
        }

        let signed_out = Bomtoon {
            account: AccountState::SignedOut,
            view: View::Main,
            ..Bomtoon::default()
        }
        .screen();
        let actions = &signed_out.top_bar.expect("signed-out top bar").actions;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action, action_id(SIGN_IN));
        assert_eq!(actions[0].label, "Sign in");

        let account = Bomtoon {
            view: View::Account,
            ..Bomtoon::default()
        }
        .screen();
        let actions = &account.top_bar.expect("Account top bar").actions;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action, action_id(SIGN_OUT));
        assert_eq!(actions[0].label, "Sign out");
    }

    #[test]
    fn start_rechecks_credentials_instead_of_trusting_previous_account_state() {
        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::SignedOut,
            ..Bomtoon::default()
        });

        let commands = runner.start();

        assert_eq!(runner.app().account, AccountState::Checking);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        let spawned = spawns(&commands);
        for work in [
            api::account_scope(),
            api::asset_summary(),
            api::library(0),
            api::homepage(),
        ] {
            assert!(spawned.iter().any(|(_, spawned)| *spawned == work));
        }
    }

    #[test]
    fn navigation_uses_one_fixed_featured_recent_library_bar() {
        for (destination, selected) in [
            (MainDestination::Featured, 0),
            (MainDestination::Recent, 1),
            (MainDestination::Library, 2),
        ] {
            let screen = Bomtoon {
                view: View::Main,
                destination,
                library_load: loaded_shelf(),
                recent_load: loaded_shelf(),
                ..Bomtoon::default()
            }
            .screen();
            let bar = screen.nav_bar.as_ref().expect("one fixed destination bar");

            assert_eq!(bar.selected, Some(selected));
            assert_eq!(
                bar.destinations
                    .iter()
                    .map(|destination| destination.label.as_str())
                    .collect::<Vec<_>>(),
                ["Featured", "Recent", "Library"]
            );
            assert_eq!(
                bar.destinations
                    .iter()
                    .map(|destination| destination.action)
                    .collect::<Vec<_>>(),
                [action_id(FEATURED), action_id(RECENT), action_id(LIBRARY)]
            );
            let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::measuring(true));
            let y = CLARA_BW_METRICS.height - CLARA_BW_METRICS.nav_bar_height() / 2;
            for (x, action) in [
                (CLARA_BW_METRICS.width / 6, action_id(FEATURED)),
                (CLARA_BW_METRICS.width / 2, action_id(RECENT)),
                (
                    CLARA_BW_METRICS.width - CLARA_BW_METRICS.width / 6,
                    action_id(LIBRARY),
                ),
            ] {
                assert_eq!(
                    layout.hit_test(x, y),
                    Some(action),
                    "destination is not centered in its third"
                );
            }
            assert!(
                screen.nodes.iter().all(|node| {
                    !matches!(
                        node,
                        Node::Button { action, .. }
                            if *action == action_id(FEATURED)
                                || *action == action_id(RECENT)
                                || *action == action_id(LIBRARY)
                    )
                }),
                "destinations must not be in-flow pseudo-tabs"
            );
            assert_fits(&screen);
        }
    }

    #[test]
    fn navigation_lazy_protected_destinations_fetch_only_the_selected_shelf() {
        for (action, destination, expected_path, other_path) in [
            (RECENT, MainDestination::Recent, "/recent?", "/library?"),
            (LIBRARY, MainDestination::Library, "/library?", "/recent?"),
        ] {
            let mut runner = AppRunner::new(Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                destination: MainDestination::Featured,
                ..Bomtoon::default()
            });

            let commands = runner.action(action_id(action));

            assert_eq!(runner.app().view, View::Main);
            assert_eq!(runner.app().destination, destination);
            assert_eq!(runner.app().page, 0);
            assert_eq!(spawns(&commands).len(), 1);
            assert!(spawns(&commands).iter().any(
                |(_, work)| matches!(work, Task::Fetch { url, .. } if url.contains(expected_path))
            ));
            assert!(spawns(&commands).iter().all(
                |(_, work)| !matches!(work, Task::Fetch { url, .. } if url.contains(other_path))
            ));
        }

        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::Active,
            view: View::Main,
            destination: MainDestination::Recent,
            recent_load: loaded_shelf(),
            page: 4,
            ..Bomtoon::default()
        });
        let commands = runner.action(action_id(RECENT));
        assert_eq!(runner.app().page, 0);
        assert!(spawns(&commands).is_empty());
    }

    #[test]
    fn recent_load_failure_retry_and_late_success_stay_destination_local() {
        let (mut runner, _) = started_ready_for_homepage();

        let commands = runner.action(action_id(RECENT));
        let (first, _) = fetch_task_with(&commands, "/recent?");
        let loading = last_screen(&commands);
        assert_eq!(runner.app().recent_load.pending_page, Some(0));
        assert!(loading.nav_bar.is_some());
        assert!(format!("{loading:?}").contains("Loading recent reading"));

        let commands = runner.action(action_id(FEATURED));
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert!(last_screen(&commands).nav_bar.is_some());

        runner.task_outcome(first, TaskOutcome::Failed(TaskError::Offline));
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert!(runner.app().problem.is_none());
        assert!(runner.app().recent_load.error.is_some());

        let commands = runner.action(action_id(RECENT));
        let error_screen = last_screen(&commands);
        assert_eq!(runner.app().destination, MainDestination::Recent);
        assert!(error_screen.nav_bar.is_some());
        assert!(error_screen.nodes.iter().any(
            |node| matches!(node, Node::Button { action, .. } if *action == action_id(RETRY))
        ));
        assert_fits(&error_screen);

        let commands = runner.action(action_id(RETRY));
        let (retry, _) = fetch_task_with(&commands, "/recent?");
        assert_eq!(runner.app().recent_load.pending_page, Some(0));
        runner.action(action_id(FEATURED));

        runner.task_outcome(retry, TaskOutcome::Completed(RECENT_RESPONSE.to_vec()));
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert!(runner.app().recent_load.loaded);
        assert!(runner.app().recent_load.error.is_none());

        let commands = runner.action(action_id(RECENT));
        assert!(spawns(&commands).is_empty());
        assert_eq!(runner.app().destination, MainDestination::Recent);
        assert!(format!("{:?}", last_screen(&commands)).contains("Episode"));
    }

    #[test]
    fn protected_shelf_work_allows_destination_switches_and_serializes_one_next_shelf() {
        let (mut runner, _) = started_ready_for_homepage();
        runner.app_mut().library_load.loaded = false;
        runner.app_mut().comics.clear();

        let recent_commands = runner.action(action_id(RECENT));
        let (recent, _) = fetch_task_with(&recent_commands, "/recent?");
        let library_commands = runner.action(action_id(LIBRARY));
        assert!(spawns(&library_commands).is_empty());
        assert_eq!(runner.app().destination, MainDestination::Library);
        assert_eq!(runner.app().queued_foreground, Some(Pending::Library(0)));
        assert!(last_screen(&library_commands).nav_bar.is_some());

        let commands =
            runner.task_outcome(recent, TaskOutcome::Completed(RECENT_RESPONSE.to_vec()));
        let (library, work) = only_spawn(&commands);
        assert_eq!(work, api::library(0));
        assert_eq!(runner.app().destination, MainDestination::Library);
        assert!(runner.app().recent_load.loaded);
        assert_eq!(runner.app().pending, Some(Pending::Library(0)));

        runner.action(action_id(FEATURED));
        runner.task_outcome(library, TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()));
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert!(runner.app().library_load.loaded);
        assert!(runner.app().library_load.error.is_none());
    }

    #[test]
    fn navigation_pending_comic_ignores_destination_taps() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (task, _) = only_spawn(&commands);
        assert_eq!(runner.app().pending, Some(Pending::Content(0)));

        let commands = runner.action(action_id(FEATURED));

        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Library);
        assert_eq!(runner.app().pending, Some(Pending::Content(0)));
        assert_eq!(runner.app().task, Some(task));
        assert!(spawns(&commands).is_empty());

        runner.task_outcome(task, TaskOutcome::Completed(content_response()));
        assert_eq!(runner.app().view, View::Episodes);
        assert_eq!(runner.app().destination, MainDestination::Library);
        assert_eq!(runner.app().pending, None);
        assert_eq!(runner.app().task, None);
    }

    #[test]
    fn authentication_gating_blocks_signed_out_main_actions() {
        for action in [RECENT, LIBRARY] {
            let mut runner = AppRunner::new(Bomtoon {
                account: AccountState::SignedOut,
                view: View::Main,
                destination: MainDestination::Featured,
                ..Bomtoon::default()
            });

            let commands = runner.action(action_id(action));

            assert_eq!(runner.app().view, View::Status);
            assert_eq!(runner.app().destination, MainDestination::Featured);
            assert_eq!(runner.app().page, 0);
            assert!(!commands.iter().any(is_library_or_recent_fetch));
        }

        let mut app = Bomtoon {
            account: AccountState::SignedOut,
            view: View::Main,
            destination: MainDestination::Featured,
            page: 3,
            ..Bomtoon::default()
        };
        let mut context = Context::default();
        app.open_selected_comic(&mut context, "public-comic", "Public Comic", 0);
        assert_eq!(app.view, View::Status);
        assert_eq!(app.destination, MainDestination::Featured);
        assert_eq!(app.page, 0);
        assert!(spawns(&context.take_commands()).is_empty());

        let signed_out_main = Bomtoon {
            account: AccountState::SignedOut,
            view: View::Main,
            ..Bomtoon::default()
        }
        .screen();
        assert_eq!(
            signed_out_main
                .top_bar
                .expect("main top bar")
                .actions
                .iter()
                .map(|action| action.label.as_str())
                .collect::<Vec<_>>(),
            ["Sign in"]
        );
        let signed_in_main = Bomtoon {
            account: AccountState::Active,
            view: View::Main,
            ..Bomtoon::default()
        }
        .screen();
        assert!(signed_in_main
            .top_bar
            .expect("main top bar")
            .actions
            .iter()
            .all(|action| action.label != "Sign in"));
    }

    #[test]
    fn authentication_gating_sign_in_returns_to_featured_without_resuming_action() {
        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::SignedOut,
            view: View::Main,
            destination: MainDestination::Featured,
            page: 4,
            ..Bomtoon::default()
        });

        runner.action(action_id(RECENT));
        let commands = runner.action(action_id(RETRY));

        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert_eq!(runner.app().page, 0);
        assert!(commands.iter().any(is_homepage_fetch));
        assert!(!spawns(&commands)
            .iter()
            .any(|(_, work)| matches!(work, Task::Fetch { url, .. } if url.contains("/recent?"))));
        assert!(!spawns(&commands).iter().any(|(_, work)| matches!(
            work,
            Task::Fetch { url, .. }
                if url.contains("/api/balcony-api-v2/contents/")
                    && !url.contains("/api/balcony-api-v2/contents/main/")
        )));
    }

    #[test]
    fn back_returns_destination_page_one_after_account_and_episodes() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.app_mut().destination = MainDestination::Recent;
        runner.app_mut().recent_load.loaded = true;
        runner.app_mut().page = 4;

        runner.action(action_id(LIBRARY));
        runner.action(action_id(RECENT));
        assert_eq!(runner.app().destination, MainDestination::Recent);
        assert_eq!(runner.app().page, 0);

        runner.app_mut().page = 3;
        runner.action(action_id(ACCOUNT));
        runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Recent);
        assert_eq!(runner.app().page, 0);

        runner.app_mut().page = 2;
        runner.app_mut().view = View::Episodes;
        runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Recent);
        assert_eq!(runner.app().page, 0);

        runner.app_mut().view = View::Main;
        runner.app_mut().destination = MainDestination::Featured;
        runner.app_mut().featured.feed_page = 2;
        runner.app_mut().page = 7;
        runner.action(action_id(ACCOUNT));
        assert_eq!(runner.app().view, View::Account);
        assert_eq!(runner.app().featured.feed_page, 0);
        runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert_eq!(runner.app().featured.feed_page, 0);
        assert_eq!(runner.app().page, 0);
    }
    const EXPECTED_SYNOPSIS_MORE: &str = "synopsis-more";
    const EXPECTED_SYNOPSIS_PREVIOUS: &str = "synopsis-previous";
    const EXPECTED_SYNOPSIS_NEXT: &str = "synopsis-next";
    const EXPECTED_SYNOPSIS_CLOSE: &str = "synopsis-close";

    fn long_synopsis() -> String {
        use std::fmt::Write as _;

        let mut synopsis = String::new();
        for number in 0..80 {
            writeln!(
                synopsis,
                "Part {number}: 사냥꾼 Q follows the signal through the old city while every clue changes what the team believes."
            )
            .expect("writing to String");
            synopsis.push('\n');
        }
        synopsis
    }

    fn episode_metadata_app(synopsis: String) -> Bomtoon {
        const SCOPE: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        let pages = synopsis_pages(&synopsis);
        let mut app = Bomtoon {
            account: AccountState::Active,
            connection: ConnectionState::Online,
            account_scope: Some(test_scope(SCOPE)),
            view: View::Episodes,
            selected_content_id: Some(41),
            selected_content_alias: "hunter_q".to_owned(),
            selected_title: "Hunter Q".to_owned(),
            selected_creators: "Writer | Artist".to_owned(),
            selected_synopsis: synopsis,
            synopsis: SynopsisState {
                pages,
                open_page: None,
                ..SynopsisState::default()
            },
            wallet: WalletState {
                summary: Some(WalletSummary {
                    coins: model::AssetAmounts {
                        standard: 10,
                        bonus: 0,
                        free: 0,
                    },
                    tickets: model::AssetAmounts::default(),
                }),
                ..WalletState::default()
            },
            gifts: TitleGiftState {
                title_id: Some(41),
                available: Some(2),
                ..TitleGiftState::default()
            },
            episodes: vec![Episode {
                id: 105,
                alias: "ep-1".to_owned(),
                title: "Episode One".to_owned(),
                opened_at: 1_709_136_000_000,
                thumbnail_url: None,
                purchase: model::PurchaseState::Owned,
                rent_expires_at: None,
                rent_coin: None,
                purchase_coin: None,
                gift_eligible: false,
            }],
            ..Bomtoon::default()
        };
        app.prepare_episode_layout();
        app
    }
    fn episode_metadata_runner(synopsis: String) -> AppRunner<Bomtoon> {
        let mut runner = AppRunner::with_metrics(episode_metadata_app(synopsis), CLARA_BW_METRICS);
        runner.app_mut().prepare_episode_layout();
        runner
    }

    fn screen_button_actions(screen: &Screen) -> Vec<ActionId> {
        screen
            .nodes
            .iter()
            .chain(
                screen
                    .overlay
                    .iter()
                    .flat_map(|overlay| overlay.nodes.iter()),
            )
            .filter_map(|node| match node {
                Node::Button { action, .. } => Some(*action),
                _ => None,
            })
            .collect()
    }

    fn overlay_text(overlay: &Overlay) -> String {
        overlay
            .nodes
            .iter()
            .filter_map(|node| match node {
                Node::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn episode_header_shows_metadata_balance_and_more_without_duplicate_title() {
        let app = episode_metadata_app(long_synopsis());
        let screen = app.episode_screen();

        assert_eq!(screen.top_bar.as_ref().expect("top bar").title, "Episodes");
        let drawn = format!("{screen:?}");
        assert!(drawn.contains("Hunter Q"));
        assert_eq!(drawn.matches("Hunter Q").count(), 1);
        assert!(drawn.contains("Writer | Artist"));
        assert!(drawn.contains("Coins 10"));
        assert!(drawn.contains("Gifts 2"));
        assert!(screen_button_actions(&screen).contains(&action_id(EXPECTED_SYNOPSIS_MORE)));
        assert_fits(&screen);
    }

    #[test]
    fn short_synopsis_shows_the_full_preview_without_more() {
        let synopsis = "A complete short synopsis.";
        let app = episode_metadata_app(synopsis.to_owned());
        let screen = app.episode_screen();
        let drawn = format!("{screen:?}");

        assert!(drawn.contains(synopsis));
        assert!(!screen_button_actions(&screen).contains(&action_id(EXPECTED_SYNOPSIS_MORE)));
        assert_fits(&screen);
    }
    #[test]
    fn whitespace_collapsed_synopsis_pages_remain_protocol_encodable() {
        let synopsis = format!("Lead{}Tail", " ".repeat(17_000));
        let mut runner = episode_metadata_runner(synopsis.clone());
        assert!(runner.app().synopsis.pages.iter().all(|range| {
            range.len() <= SYNOPSIS_MODAL_MAX_BYTES
                && synopsis.is_char_boundary(range.start)
                && synopsis.is_char_boundary(range.end)
        }));
        runner.action(action_id(EXPECTED_SYNOPSIS_MORE));

        let mut observed = String::new();
        loop {
            let screen = runner.app().screen();
            let overlay = screen.overlay.as_ref().expect("synopsis modal");
            observed.push_str(&overlay_text(overlay));
            assert_fits(&screen);
            if !screen_button_actions(&screen).contains(&action_id(EXPECTED_SYNOPSIS_NEXT)) {
                break;
            }
            runner.action(action_id(EXPECTED_SYNOPSIS_NEXT));
        }

        assert_eq!(observed, synopsis);
    }

    #[test]
    fn newline_heavy_preview_keeps_a_full_episode_page_visible() {
        let synopsis = "Line\n".repeat(48);
        assert_eq!(synopsis.len(), SYNOPSIS_PREVIEW_BYTES);
        let mut app = episode_metadata_app(synopsis.clone());
        app.episodes = (0..EPISODE_ITEMS_PER_PAGE)
            .map(|index| Episode {
                id: 100 + index,
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
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.app_mut().prepare_episode_layout();
        let mut observed = BTreeSet::new();
        loop {
            let screen = runner.app().episode_screen();
            let actions = screen_button_actions(&screen);
            assert!(actions.contains(&action_id(EXPECTED_SYNOPSIS_MORE)));
            let rows = screen
                .nodes
                .iter()
                .find_map(|node| match node {
                    Node::Rows { rows, .. } => Some(rows),
                    _ => None,
                })
                .expect("episode rows");
            for index in 0..EPISODE_ITEMS_PER_PAGE {
                if rows
                    .iter()
                    .any(|row| row.action == action_id(&format!("episode-{index}")))
                {
                    observed.insert(index);
                }
            }
            assert!(!rows.is_empty());
            assert_fits(&screen);
            let (current, total) = screen
                .page_turns
                .as_ref()
                .and_then(|turns| turns.position)
                .expect("episode position");
            if current == total {
                break;
            }
            runner.action(action_id(NEXT_PAGE));
        }
        assert_eq!(
            observed,
            (0..EPISODE_ITEMS_PER_PAGE).collect::<BTreeSet<_>>()
        );

        runner.action(action_id(EXPECTED_SYNOPSIS_MORE));
        let mut modal_text = String::new();
        loop {
            let screen = runner.app().screen();
            let overlay = screen
                .overlay
                .as_ref()
                .expect("render-truncated synopsis modal");
            modal_text.push_str(&overlay_text(overlay));
            assert_fits(&screen);
            if !screen_button_actions(&screen).contains(&action_id(EXPECTED_SYNOPSIS_NEXT)) {
                break;
            }
            runner.action(action_id(EXPECTED_SYNOPSIS_NEXT));
        }
        assert_eq!(modal_text, synopsis);
    }
    #[test]
    fn episode_capacity_fits_later_button_pages_without_skips_or_duplicates() {
        let synopsis = "Line\n".repeat(48);
        let mut app = episode_metadata_app(synopsis);
        app.episodes = (0..12)
            .map(|index| Episode {
                id: 100 + index,
                alias: format!("ep-{index}"),
                title: format!("Episode {index}"),
                opened_at: 1_709_136_000_000,
                thumbnail_url: None,
                purchase: if index < EPISODE_ITEMS_PER_PAGE {
                    model::PurchaseState::Other("Locked".to_owned())
                } else {
                    model::PurchaseState::Owned
                },
                rent_expires_at: None,
                rent_coin: None,
                purchase_coin: None,
                gift_eligible: false,
            })
            .collect();
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.app_mut().prepare_episode_layout();
        let mut observed = BTreeSet::new();

        loop {
            let screen = runner.app().episode_screen();
            let rows = screen.nodes.iter().find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            });
            for row in rows.into_iter().flatten() {
                let index = runner
                    .app()
                    .episodes
                    .iter()
                    .position(|episode| episode.title == row.title)
                    .expect("row identifies an episode");
                assert!(observed.insert(index), "episode {index} was duplicated");
            }
            assert_fits(&screen);
            let (current, total) = screen
                .page_turns
                .as_ref()
                .and_then(|turns| turns.position)
                .expect("episode position");
            if current == total {
                break;
            }
            runner.action(action_id(NEXT_PAGE));
        }

        assert_eq!(observed, (0..12).collect());
    }
    #[test]
    fn transient_episode_controls_preserve_later_page_boundaries() {
        const OWNER: &[u8; 32] = b"ffeeddccbbaa99887766554433221100";
        const READER: &[u8; 32] = b"00112233445566778899aabbccddeeff";

        fn snapshot(app: &Bomtoon) -> (usize, (u16, u16), BTreeSet<usize>) {
            let capacity = app.episode_rows_per_page();
            let screen = app.episode_screen();
            assert_fits(&screen);
            let position = screen
                .page_turns
                .as_ref()
                .and_then(|turns| turns.position)
                .expect("episode position");
            let indexes = screen
                .nodes
                .iter()
                .find_map(|node| match node {
                    Node::Rows { rows, .. } => Some(rows),
                    _ => None,
                })
                .into_iter()
                .flatten()
                .map(|row| {
                    app.episodes
                        .iter()
                        .position(|episode| episode.title == row.title)
                        .expect("row identifies an episode")
                })
                .collect();
            (capacity, position, indexes)
        }

        let mut app = episode_metadata_app("Short synopsis.".to_owned());
        app.selected_creators = "Writer\nArtist\nColorist".to_owned();
        app.episodes = (0..18)
            .map(|index| Episode {
                id: 100 + index,
                alias: format!("ep-{index}"),
                title: format!("Episode {index}"),
                opened_at: 1_709_136_000_000,
                thumbnail_url: None,
                purchase: model::PurchaseState::Owned,
                rent_expires_at: None,
                rent_coin: Some(1),
                purchase_coin: Some(2),
                gift_eligible: true,
            })
            .collect();
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.app_mut().prepare_episode_layout();
        runner.app_mut().page = 1;
        let baseline = snapshot(runner.app());
        assert!(baseline.0 > 1, "{baseline:?}");
        assert!(!baseline.2.is_empty());

        runner.app_mut().gifts.error = true;
        assert_eq!(snapshot(runner.app()), baseline);

        runner.app_mut().purchase_rejection_notice = Some("FAIL");
        assert_eq!(snapshot(runner.app()), baseline);

        let marker = marker_for(test_scope(OWNER));
        let _ = runner.app_mut().commerce.marker_loaded(Some(&marker));
        let _ = runner.app_mut().commerce.safety_changed(
            commerce::Authentication::Authenticated(test_scope(READER)),
            commerce::Connectivity::Online,
        );
        assert!(runner.app().commerce.marker_belongs_to_another_account());
        assert_eq!(snapshot(runner.app()), baseline);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn episode_rows_show_thumbnail_date_and_truthful_access_status() {
        const HOUR_MS: i64 = 60 * 60 * 1_000;
        let now_ms = unix_time_ms().expect("host clock");
        let thumbnail_url = "https://image.balcony.studio/tw/ep_thumbnail/owned.webp".to_owned();
        let picture = TilePicture::new(PictureHandle(73), 60, 60);
        let episode = |id: usize,
                       title: &str,
                       purchase: model::PurchaseState,
                       rent_expires_at: Option<i64>,
                       thumbnail_url: Option<String>| Episode {
            id,
            alias: format!("ep-{id}"),
            title: title.to_owned(),
            opened_at: 1_709_136_000_000,
            thumbnail_url,
            purchase,
            rent_expires_at,
            rent_coin: Some(1),
            purchase_coin: Some(2),
            gift_eligible: false,
        };
        let mut app = episode_metadata_app("Synopsis.".to_owned());
        app.episodes = vec![
            episode(
                0,
                "Owned episode",
                model::PurchaseState::Owned,
                None,
                Some(thumbnail_url.clone()),
            ),
            episode(
                1,
                "Rented episode",
                model::PurchaseState::Rented,
                Some(now_ms + 2 * HOUR_MS),
                None,
            ),
            episode(2, "Free episode", model::PurchaseState::Free, None, None),
            episode(
                3,
                "Sample episode",
                model::PurchaseState::Sample,
                None,
                None,
            ),
            episode(
                4,
                "Unowned episode",
                model::PurchaseState::NotOwned,
                None,
                None,
            ),
            episode(
                5,
                "Unknown episode",
                model::PurchaseState::Other("FUTURE".to_owned()),
                None,
                None,
            ),
        ];
        app.covers
            .entries
            .insert(thumbnail_url, CoverState::Ready(picture));
        app.episode_items_per_page = Some(app.episodes.len());
        let account_scope = app.account_scope.expect("account scope");
        let _ = app.commerce.marker_loaded(None);
        let _ = app.commerce.safety_changed(
            commerce::Authentication::Authenticated(account_scope),
            commerce::Connectivity::Online,
        );

        let screen = app.episode_screen();
        let rows = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("episode rows");
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0].title, "Owned episode");
        assert_eq!(rows[0].summary, "2024-02-29");
        assert_eq!(rows[0].trailing.as_deref(), Some("Owned"));
        assert!(matches!(
            rows[0].lead,
            RowLead::Picture(lead, Glyph::Book) if lead == picture
        ));
        assert_eq!(rows[1].trailing.as_deref(), Some("2 hrs"));
        assert_eq!(rows[2].trailing.as_deref(), Some("Free"));
        assert_eq!(rows[3].trailing.as_deref(), Some("Sample"));
        assert_eq!(rows[4].trailing, None);
        assert_eq!(rows[5].trailing.as_deref(), Some("FUTURE"));
        assert_eq!(rows[4].action, action_id("episode-4"));
        assert_eq!(rows[5].action, action_id("episode-disabled-5"));

        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        let commands = runner.action(action_id("episode-disabled-5"));
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::Spawn { .. })));
        assert_eq!(runner.app().view, View::Episodes);
        assert_eq!(runner.app().commerce.state(), commerce::CommerceState::Idle);

        let commands = runner.action(action_id("episode-4"));
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::Spawn { .. })));
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::Quoting
        );
    }

    #[test]
    fn rich_episode_capacity_fits_every_page_without_skips_or_state_drift() {
        let mut app = episode_metadata_app("節".repeat(80));
        app.selected_title = "界".repeat(85);
        app.selected_creators = "作".repeat(170);
        app.wallet.summary = Some(WalletSummary {
            coins: model::AssetAmounts {
                standard: usize::MAX,
                bonus: 0,
                free: 0,
            },
            tickets: model::AssetAmounts::default(),
        });
        app.episodes = (0..18)
            .map(|index| Episode {
                id: 500 + index,
                alias: format!("ep-{index}"),
                title: format!("{index}{}", "集".repeat(169)),
                opened_at: i64::MAX,
                thumbnail_url: None,
                purchase: model::PurchaseState::Other("未".repeat(42)),
                rent_expires_at: None,
                rent_coin: Some(usize::MAX),
                purchase_coin: Some(usize::MAX),
                gift_eligible: true,
            })
            .collect();
        app.prepare_episode_layout();
        let capacity = app.episode_rows_per_page();
        assert!((1..=EPISODE_ITEMS_PER_PAGE).contains(&capacity));
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        let mut observed = BTreeSet::new();

        loop {
            let screen = runner.app().episode_screen();
            assert_fits(&screen);
            let rows = screen
                .nodes
                .iter()
                .find_map(|node| match node {
                    Node::Rows { rows, .. } => Some(rows),
                    _ => None,
                })
                .expect("episode rows");
            assert!(!rows.is_empty());
            for row in rows {
                let index = (0..runner.app().episodes.len())
                    .find(|index| row.action == action_id(&format!("episode-disabled-{index}")))
                    .expect("row action identifies an episode");
                assert!(observed.insert(index), "episode {index} was duplicated");
            }
            let (current, total) = screen
                .page_turns
                .as_ref()
                .and_then(|turns| turns.position)
                .expect("episode position");
            if current == total {
                let final_page = runner.app().page;
                runner.action(action_id(NEXT_PAGE));
                assert_eq!(runner.app().page, final_page);
                break;
            }
            runner.action(action_id(NEXT_PAGE));
            assert_eq!(runner.app().episode_rows_per_page(), capacity);
        }
        assert_eq!(observed, (0..runner.app().episodes.len()).collect());

        runner.app_mut().gifts.error = true;
        runner.app_mut().purchase_rejection_notice = Some("FAIL");
        assert_eq!(runner.app().episode_rows_per_page(), capacity);
        assert_fits(&runner.app().episode_screen());
    }

    #[test]
    fn newline_heavy_maximum_episode_header_caches_only_a_fitted_capacity() {
        let mut app = episode_metadata_app("Synopsis.".to_owned());
        app.selected_title = "T\n".repeat(128);
        app.selected_creators = "C\n".repeat(128);
        assert_eq!(app.selected_title.len(), 256);
        assert_eq!(app.selected_creators.len(), 256);
        app.episodes = vec![Episode {
            id: 900,
            alias: "newline-heavy".to_owned(),
            title: "Visible episode".to_owned(),
            opened_at: 1_709_136_000_000,
            thumbnail_url: None,
            purchase: model::PurchaseState::Owned,
            rent_expires_at: None,
            rent_coin: None,
            purchase_coin: None,
            gift_eligible: false,
        }];

        app.prepare_episode_layout();
        assert_eq!(app.selected_title, "T\n".repeat(128));
        assert_eq!(app.selected_creators, "C\n".repeat(128));
        assert!(!app.episode_title_preview.contains('\n'));
        assert!(!app.episode_creators_preview.contains('\n'));

        assert!(app.episode_items_per_page.is_some());
        let screen = app.episode_screen();
        assert!(
            episode_screen_layout_fits(&screen),
            "cached capacity did not pass its one-row probe: {screen:?}"
        );
        assert!(screen.nodes.iter().any(|node| matches!(
            node,
            Node::Rows { rows, .. }
                if rows.len() == 1 && rows[0].action == action_id("episode-0")
        )));
        assert_fits(&screen);
    }

    #[test]
    fn unicode_episode_header_preserves_korean_and_cjk_copy_while_fitting() {
        let title = "사냥꾼 Q와 漫畫".to_owned();
        let creators = "글 김작가 | 그림 林老師".to_owned();
        let mut app = episode_metadata_app("짧은 줄거리.".to_owned());
        app.selected_title.clone_from(&title);
        app.selected_creators.clone_from(&creators);

        app.prepare_episode_layout();

        assert_eq!(app.selected_title, title);
        assert_eq!(app.selected_creators, creators);
        let screen = app.episode_screen();
        assert!(screen.nodes.iter().any(|node| matches!(
            node,
            Node::Heading { text, .. } if text == &title
        )));
        assert!(screen.nodes.iter().any(|node| matches!(
            node,
            Node::Text { text, .. } if text == &creators
        )));
        assert!(screen.nodes.iter().any(|node| matches!(
            node,
            Node::Rows { rows, .. }
                if rows.len() == 1 && rows[0].action == action_id("episode-0")
        )));
        assert!(episode_screen_layout_fits(&screen));
    }

    #[test]
    fn refreshed_episode_data_preserves_the_visible_episode_anchor() {
        const HOUR_MS: i64 = 60 * 60 * 1_000;
        let now_ms = unix_time_ms().expect("host clock");
        let mut app = episode_metadata_app("Synopsis.".to_owned());
        app.episodes = (0..12)
            .map(|index| Episode {
                id: 700 + index,
                alias: format!("anchor-{index}"),
                title: format!("Anchor {index}"),
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
        let capacity = app.episode_rows_per_page();
        assert!(app.episodes.len() > capacity);
        app.page = 1;
        let anchor_index = capacity;
        let anchor_alias = app.episodes[anchor_index].alias.clone();
        app.episodes[anchor_index].purchase = model::PurchaseState::Rented;
        app.episodes[anchor_index].rent_expires_at = Some(now_ms - HOUR_MS);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);

        let commands = runner.action(action_id(&format!("episode-{anchor_index}")));
        let (refresh_task, _) = only_spawn(&commands);
        let reordered = (0..12)
            .filter(|index| *index != anchor_index)
            .chain(std::iter::once(anchor_index))
            .map(|index| {
                format!(
                    "{{\"id\":{},\"alias\":\"anchor-{index}\",\"title\":\"Anchor {index}\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"POSSESSION\"}}",
                    700 + index
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        runner.task_outcome(
            refresh_task,
            TaskOutcome::Completed(detail_response(41, "hunter_q", "Hunter Q", &reordered)),
        );

        assert_eq!(runner.app().view, View::Episodes);
        let refreshed_index = runner
            .app()
            .episodes
            .iter()
            .position(|episode| episode.alias == anchor_alias)
            .expect("refreshed anchor");
        let refreshed_capacity = runner.app().episode_rows_per_page();
        assert_eq!(runner.app().page, refreshed_index / refreshed_capacity);
        let (start, end) = page_bounds(
            runner.app().page,
            runner.app().episodes.len(),
            refreshed_capacity,
        );
        assert!((start..end).contains(&refreshed_index));
    }

    #[test]
    fn accepting_content_precomputes_synopsis_pages() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);

        runner.task_outcome(content_task, TaskOutcome::Completed(content_response()));
        assert_eq!(
            runner.app().synopsis.pages,
            synopsis_pages(&runner.app().selected_synopsis)
        );
        assert_eq!(runner.app().synopsis.open_page, None);
        assert!(!runner.app().synopsis.pages.is_empty());
        assert!(runner.app().episode_items_per_page.is_some());
        assert!(!runner.app().synopsis.preview.is_empty());
    }

    #[test]
    fn changing_comic_clears_synopsis_pagination() {
        let mut app = episode_metadata_app(long_synopsis());
        app.view = View::Main;
        app.destination = MainDestination::Library;
        app.synopsis.open_page = None;
        app.comics = vec![Comic {
            alias: "another_comic".to_owned(),
            title: "Another Comic".to_owned(),
            creators: String::new(),
            cover_url: None,
            owned_episodes: 0,
            total_episodes: 1,
        }];
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);

        runner.action(action_id("comic-0"));

        assert_eq!(runner.app().synopsis, SynopsisState::default());
        assert_eq!(runner.app().episode_items_per_page, None);
    }

    #[test]
    fn suspending_closes_the_synopsis_modal() {
        let mut runner = episode_metadata_runner(long_synopsis());
        runner.action(action_id(EXPECTED_SYNOPSIS_MORE));
        assert!(runner.app().screen().overlay.is_some());

        runner.suspend();

        assert_eq!(runner.app().synopsis.open_page, None);
        assert!(runner.app().screen().overlay.is_none());
    }

    #[test]
    fn synopsis_modal_pages_preserve_all_text_and_close_to_same_episode_page() {
        let synopsis = long_synopsis();
        let mut runner = episode_metadata_runner(synopsis.clone());
        runner.app_mut().page = 2;

        let open = runner.action(action_id(EXPECTED_SYNOPSIS_MORE));
        assert!(last_screen(&open).overlay.is_some());

        let mut observed = String::new();
        let mut page = 0;
        loop {
            let screen = runner.app().screen();
            let overlay = screen.overlay.as_ref().expect("synopsis modal");
            observed.push_str(&overlay_text(overlay));
            let actions = screen_button_actions(&screen);
            if page == 0 {
                assert!(!actions.contains(&action_id(EXPECTED_SYNOPSIS_PREVIOUS)));
            } else {
                assert!(actions.contains(&action_id(EXPECTED_SYNOPSIS_PREVIOUS)));
            }
            assert!(actions.contains(&action_id(EXPECTED_SYNOPSIS_CLOSE)));
            assert_fits(&screen);
            if !actions.contains(&action_id(EXPECTED_SYNOPSIS_NEXT)) {
                break;
            }
            if page == 1 {
                assert!(actions.contains(&action_id(EXPECTED_SYNOPSIS_NEXT)));
            }
            runner.action(action_id(EXPECTED_SYNOPSIS_NEXT));
            page += 1;
        }

        assert!(page >= 2, "fixture must exercise a middle modal page");
        assert_eq!(observed, synopsis);
        runner.action(action_id(EXPECTED_SYNOPSIS_CLOSE));
        assert_eq!(runner.app().page, 2);
        assert!(runner.app().screen().overlay.is_none());
    }

    #[test]
    fn back_closes_synopsis_before_leaving_episodes() {
        let mut runner = episode_metadata_runner(long_synopsis());
        runner.app_mut().page = 2;
        runner.action(action_id(EXPECTED_SYNOPSIS_MORE));

        runner.action(ActionId::BACK);

        assert_eq!(runner.app().view, View::Episodes);
        assert_eq!(runner.app().page, 2);
        assert!(runner.app().screen().overlay.is_none());
    }

    #[test]
    fn open_synopsis_blocks_underlying_episode_actions() {
        const SCOPE: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        let scope = test_scope(SCOPE);
        let mut runner = episode_metadata_runner(long_synopsis());
        runner.app_mut().page = 2;
        runner.action(action_id(EXPECTED_SYNOPSIS_MORE));

        let commands = runner.action(action_id(PREVIOUS_PAGE));
        assert_eq!(runner.app().page, 2);
        assert!(spawns(&commands).is_empty());

        let commands = runner.action(action_id("episode-0"));
        assert_eq!(runner.app().view, View::Episodes);
        assert!(spawns(&commands).is_empty());

        runner.app_mut().gifts.error = true;
        let commands = runner.action(action_id(RETRY_GIFTS));
        assert!(runner.app().gifts.error);
        assert!(runner.app().gifts.task.is_none());
        assert!(spawns(&commands).is_empty());

        runner.app_mut().commerce = choosing_commerce(scope);
        for action in [USE_GIFT, RENT, BUY, CANCEL_COMMERCE] {
            let commands = runner.action(action_id(action));
            assert_eq!(
                runner.app().commerce.state(),
                commerce::CommerceState::Choosing
            );
            assert!(spawns(&commands).is_empty());
        }
    }

    #[test]
    fn readable_and_not_owned_episode_rows_are_actions() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);
        runner.task_outcome(content_task, TaskOutcome::Completed(content_response()));
        let capacity = runner.app().episode_rows_per_page();
        let pages = runner.app().episodes.len().div_ceil(capacity);
        let mut actions = Vec::new();
        for page in 0..pages {
            let screen = runner.app().episode_screen_for(page, capacity, false);
            actions.extend(
                screen
                    .nodes
                    .iter()
                    .find_map(|node| match node {
                        Node::Rows { rows, .. } => Some(rows),
                        _ => None,
                    })
                    .into_iter()
                    .flatten()
                    .map(|row| row.action),
            );
        }
        assert!(actions.contains(&action_id("episode-0")));
        assert!(actions.contains(&action_id("episode-1")));
        assert!(actions.contains(&action_id("episode-2")));
        assert!(actions.contains(&action_id("episode-3")));
        assert!(actions.contains(&action_id("episode-4")));
        assert_eq!(runner.app().selected_content_id, Some(41));
    }

    #[test]
    fn episode_page_removes_direct_ticket_ui_and_labels_rentals() {
        let (mut runner, _) = loaded_library();
        runner.app_mut().wallet.summary = Some(WalletSummary {
            coins: model::AssetAmounts::default(),
            tickets: model::AssetAmounts {
                standard: 3,
                bonus: 1,
                free: 0,
            },
        });
        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);
        let commands =
            runner.task_outcome(content_task, TaskOutcome::Completed(content_response()));
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(!drawn.contains("Tickets 4"));
        assert!(!drawn.contains("Ticket ·"));
        assert!(drawn.contains("Rented"));
        assert!(screen.nodes.iter().any(|node| matches!(
            node,
            Node::Rows { rows, .. }
                if rows.iter().any(|row| row.action == action_id("episode-3"))
        )));
        assert_fits(&screen);
    }

    #[test]
    fn coin_only_comic_has_no_ticket_ui() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(
            content_task,
            TaskOutcome::Completed(coin_only_content_response()),
        );
        let drawn = format!("{:?}", last_screen(&commands));
        assert!(!drawn.contains("Tickets"));
        assert!(!drawn.contains("Ticket ·"));
    }

    #[test]
    fn episode_content_without_cached_wallet_does_not_fetch_wallet() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.app_mut().wallet.summary = None;

        let commands = runner.action(action_id("comic-0"));
        let (content_task, work) = only_spawn(&commands);
        assert!(matches!(
            work,
            Task::Fetch { ref url, .. }
                if url.contains("/detail/hunter_q")
        ));
        let commands =
            runner.task_outcome(content_task, TaskOutcome::Completed(content_response()));
        let (_, gift_work) = only_spawn(&commands);
        assert_eq!(gift_work, api::title_gifts(41));
        assert!(spawns(&commands).iter().all(
            |(_, work)| !matches!(work, Task::Fetch { url, .. } if url.contains("/asset/user"))
        ));
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(!drawn.contains("Tickets"));
        assert!(!drawn.contains("Ticket ·"));
        assert!(drawn.contains("Rented"));
        assert_fits(&screen);
    }

    #[test]
    fn rental_hour_labels_fit_clara_bw() {
        const HOUR_MS: i64 = 60 * 60 * 1_000;
        let now_ms = unix_time_ms().expect("host clock");
        let rental = |id, alias: &str, rent_expires_at| Episode {
            id,
            alias: alias.to_owned(),
            title: alias.to_owned(),
            opened_at: 1_709_136_000_000,
            thumbnail_url: None,
            purchase: model::PurchaseState::Rented,
            rent_expires_at,
            rent_coin: None,
            purchase_coin: None,
            gift_eligible: false,
        };
        let mut app = Bomtoon {
            view: View::Episodes,
            selected_title: "Rental labels".to_owned(),
            episodes: vec![
                rental(1, "Two days", Some(now_ms + 48 * HOUR_MS)),
                rental(2, "One hour", Some(now_ms + HOUR_MS)),
                rental(3, "Elapsed", Some(now_ms)),
                rental(4, "Unknown expiry", None),
            ],
            ..Bomtoon::default()
        };
        app.prepare_episode_layout();

        let screen = app.episode_screen();
        let rows = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("rental rows");
        assert_eq!(
            rows.iter()
                .map(|row| row.trailing.as_deref())
                .collect::<Vec<_>>(),
            [Some("48 hrs"), Some("1 hr"), Some("0 hrs"), Some("Rented")]
        );
        assert!(rows
            .iter()
            .all(|row| row.action != action_id("episode-disabled-0")));
        assert_fits(&screen);
    }

    #[test]
    fn image_manifest_uses_active_panel_width() {
        let libra_colour_metrics = DisplayMetrics {
            width: 1264,
            height: 1680,
            picture_format: PictureFormat::Rgb8,
            ..CLARA_BW_METRICS
        };
        for (metrics, panel_width) in [(CLARA_BW_METRICS, 1072), (libra_colour_metrics, 1264)] {
            let (_, _, commands) = reader_waiting_for_manifest_with_metrics(metrics);
            let (_, manifest_work) = only_spawn(&commands);
            assert_eq!(manifest_work, api::images("hunter_q", "ep-1", panel_width));
        }
    }

    #[test]
    fn owned_episode_opens_full_screen_reader_with_hidden_chrome() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);
        let commands =
            runner.task_outcome(content_task, TaskOutcome::Completed(content_response()));
        let episode_screen = last_screen(&commands);
        assert!(format!("{episode_screen:?}").contains("Episode One"));

        let commands = runner.action(action_id("episode-0"));
        let (manifest_task, manifest_work) = only_spawn(&commands);
        assert_eq!(manifest_work, api::images("hunter_q", "ep-1", 1072));
        let commands = runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest("/tw/ep/one.webp", "p1")),
        );
        let (image_task, image_work) = only_spawn(&commands);
        assert!(matches!(
            image_work,
            Task::Fetch {
                credential: None,
                ..
            }
        ));

        let commands = runner.task_outcome(image_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        let screen = last_screen(&commands);
        let surface = screen.reading_surface.expect("reading surface");
        assert_eq!(surface.chrome, ReadingChrome::Hidden);
        assert_eq!(surface.picture.source, (1072, 1448));
        let turns = screen.page_turns.expect("reader page turns");
        assert_eq!(turns.previous, action_id(READER_PREVIOUS));
        assert_eq!(turns.next, action_id(READER_NEXT));
        assert_eq!(turns.menu, Some(action_id(READER_CHROME)));
        let progress = turns.progress.expect("reader progress");
        assert_eq!(progress.percent, 100);
        assert!(!progress.previous);
        assert!(!progress.next);
        assert!(turns.position.is_none());
        assert!(
            screen.nodes.is_empty(),
            "reader must not expose scrolling controls"
        );
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::PutPicture { .. })));
        assert_fits(&screen);
    }

    #[test]
    fn center_toggles_chrome_and_previous_boundary_noop_preserves_it() {
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_CHROME));
        assert_eq!(
            last_screen(&commands)
                .reading_surface
                .expect("surface")
                .chrome,
            ReadingChrome::Overlay
        );
        assert!(runner.action(action_id(READER_PREVIOUS)).is_empty());
        assert!(runner.app().reader.as_ref().expect("reader").chrome_visible);
        let commands = runner.action(action_id(READER_CHROME));
        assert_eq!(
            last_screen(&commands)
                .reading_surface
                .expect("surface")
                .chrome,
            ReadingChrome::Hidden
        );
    }

    #[test]
    fn final_image_requests_hot_comments_only_when_next_is_pressed() {
        let mut runner = seeded_reader(1, 0, false);

        let commands = runner.action(action_id(READER_NEXT));

        let (_, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::comments("hunter_q", "ep-1", api::CommentOrder::Hot, 0)
        );
    }

    #[test]
    fn best_comments_append_after_images_without_reader_progress_or_badges() {
        let mut runner = seeded_reader(1, 0, false);
        runner.app_mut().selected_title = "Hunter Q".to_owned();
        let commands = runner.action(action_id(READER_NEXT));
        let (task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(
            task,
            TaskOutcome::Completed(comments_response(
                &[
                    comment_entry(1, "Best reader", "Loved this episode", 40, 1, 10),
                    comment_entry(2, "Ordinary reader", "Me too", 39, 0, 9),
                ],
                0,
                1,
                2,
            )),
        );

        assert_eq!(runner.app().view, View::CommentAppendix);
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(drawn.contains("Comments"));
        assert!(drawn.contains("Hunter Q"));
        assert!(drawn.contains("Episode One"));
        assert!(drawn.contains("Best reader"));
        assert!(!drawn.contains("Ordinary reader"));
        assert!(!drawn.contains("BEST"));
        assert!(drawn.contains("All comments (2)"));
        assert!(screen.page_turns.is_none());
        assert!(screen.reading_surface.is_none());
        assert_fits(&screen);
    }

    #[test]
    fn all_comments_and_replies_restore_the_exact_navigation_stack() {
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_NEXT));
        let (appendix_task, _) = only_spawn(&commands);
        runner.task_outcome(
            appendix_task,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(1, "Best reader", "Hot", 40, 0, 10)],
                0,
                1,
                2,
            )),
        );

        let commands = runner.action(action_id(ALL_COMMENTS));
        let (comments_task, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::comments("hunter_q", "ep-1", api::CommentOrder::Newest, 0)
        );
        runner.task_outcome(
            comments_task,
            TaskOutcome::Completed(comments_response(
                &[
                    comment_entry(11, "Newest", "A comment", 41, 1, 20),
                    comment_entry(12, "Older", "Another comment", 0, 0, 19),
                ],
                0,
                1,
                2,
            )),
        );
        assert_eq!(runner.app().view, View::Comments);

        let commands = runner.action(action_id("comment-11"));
        let (reply_task, work) = only_spawn(&commands);
        assert_eq!(work, api::replies(11, api::CommentOrder::Hot, 0));
        let parent = comment_entry(11, "Newest", "A comment", 41, 1, 20);
        let commands = runner.task_outcome(
            reply_task,
            TaskOutcome::Completed(replies_response(
                &parent,
                &[comment_entry(21, "Reply", "Answer", 0, 0, 21)],
                1,
            )),
        );
        assert_eq!(runner.app().view, View::Replies);
        let drawn = format!("{:?}", last_screen(&commands));
        assert!(drawn.contains("Replies"));
        assert!(drawn.contains("A comment"));
        assert!(!drawn.contains("Answer"));
        assert!(!drawn.contains("BEST"));
        let commands = runner.action(action_id(REPLIES_NEXT));
        let drawn = format!("{:?}", last_screen(&commands));
        assert!(drawn.contains("Answer"));
        assert!(!drawn.contains("BEST"));

        runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Comments);
        runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::CommentAppendix);
        runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Reader);
        assert_eq!(runner.app().reader.as_ref().expect("reader").offset_rows, 0);
    }

    #[test]
    fn long_parent_comment_is_paged_in_full_before_replies() {
        let long_parent = "Parent ".repeat(100);
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_NEXT));
        let (appendix, _) = only_spawn(&commands);
        runner.task_outcome(
            appendix,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(11, "Author", &long_parent, 40, 1, 20)],
                0,
                1,
                1,
            )),
        );
        let commands = runner.action(action_id(ALL_COMMENTS));
        let (comments, _) = only_spawn(&commands);
        runner.task_outcome(
            comments,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(11, "Author", &long_parent, 40, 1, 20)],
                0,
                1,
                1,
            )),
        );
        let commands = runner.action(action_id("comment-11"));
        let (replies, _) = only_spawn(&commands);
        let parent = comment_entry(11, "Author", &long_parent, 40, 1, 20);
        runner.task_outcome(
            replies,
            TaskOutcome::Completed(replies_response(
                &parent,
                &[comment_entry(21, "Reply", "Answer", 0, 0, 21)],
                1,
            )),
        );
        let screen = runner.app().screen();
        assert!(!format!("{screen:?}").contains("Answer"));
        assert_fits(&screen);

        for _ in 0..2 {
            let commands = runner.action(action_id(REPLIES_NEXT));
            assert!(!format!("{:?}", last_screen(&commands)).contains("Answer"));
            assert_fits(&last_screen(&commands));
        }
        let commands = runner.action(action_id(REPLIES_NEXT));
        assert!(format!("{:?}", last_screen(&commands)).contains("Answer"));
        assert_fits(&last_screen(&commands));
    }

    #[test]
    fn appendix_falls_back_to_newest_when_hot_has_no_best_comments() {
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_NEXT));
        let (hot, _) = only_spawn(&commands);
        let commands = runner.task_outcome(
            hot,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(
                    1,
                    "Ordinary reader",
                    "Still useful",
                    39,
                    0,
                    10,
                )],
                0,
                1,
                1,
            )),
        );
        let (newest, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::comments("hunter_q", "ep-1", api::CommentOrder::Newest, 0)
        );

        let commands = runner.task_outcome(
            newest,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(
                    1,
                    "Ordinary reader",
                    "Still useful",
                    39,
                    0,
                    10,
                )],
                0,
                1,
                1,
            )),
        );
        let drawn = format!("{:?}", last_screen(&commands));
        assert!(drawn.contains("Ordinary reader"));
        assert!(drawn.contains("All comments (1)"));
        assert!(!drawn.contains("BEST"));
    }

    #[test]
    fn maximum_length_comment_previews_fit_clara() {
        let long = "Long comment ".repeat(1_000);
        let entries = [
            comment_entry(1, "One", &long, 43, 0, 10),
            comment_entry(2, "Two", &long, 42, 0, 9),
            comment_entry(3, "Three", &long, 41, 0, 8),
            comment_entry(4, "Four", &long, 40, 0, 7),
        ];
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_NEXT));
        let (hot, _) = only_spawn(&commands);
        let commands = runner.task_outcome(
            hot,
            TaskOutcome::Completed(comments_response(&entries, 0, 1, 4)),
        );
        let screen = last_screen(&commands);
        assert!(format!("{screen:?}").contains("All comments (4)"));
        assert_fits(&screen);

        let commands = runner.action(action_id(ALL_COMMENTS));
        let (newest, _) = only_spawn(&commands);
        runner.task_outcome(
            newest,
            TaskOutcome::Completed(comments_response(&entries[..1], 0, 1, 1)),
        );
        let screen = runner.app().screen();
        assert!(format!("{screen:?}").contains("Read full comment"));
        assert_fits(&screen);
    }

    #[test]
    fn empty_appendix_omits_all_comments_action() {
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_NEXT));
        let (hot, _) = only_spawn(&commands);
        let commands =
            runner.task_outcome(hot, TaskOutcome::Completed(comments_response(&[], 0, 0, 0)));

        let drawn = format!("{:?}", last_screen(&commands));
        assert!(drawn.contains("No comments yet"));
        assert!(!drawn.contains("All comments"));
        assert_eq!(runner.tasks_in_flight(), 0);
    }

    #[test]
    fn comment_credential_loss_redraws_the_signed_out_state() {
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_NEXT));
        let (comments, _) = only_spawn(&commands);
        let commands = runner.task_outcome(comments, TaskOutcome::Failed(TaskError::Unauthorized));

        assert_eq!(runner.app().account, AccountState::Expired);
        assert_eq!(runner.app().view, View::Status);
        let screen = last_screen(&commands);
        assert!(format!("{screen:?}").contains("expired"));
    }

    #[test]
    fn failed_later_comment_page_keeps_rows_and_retries_the_same_page() {
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_NEXT));
        let (hot, _) = only_spawn(&commands);
        runner.task_outcome(
            hot,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(1, "Best", "Hot", 40, 0, 10)],
                0,
                1,
                21,
            )),
        );
        let commands = runner.action(action_id(ALL_COMMENTS));
        let (page_zero, _) = only_spawn(&commands);
        runner.task_outcome(
            page_zero,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(11, "Newest", "Cached comment", 0, 0, 20)],
                0,
                2,
                21,
            )),
        );

        let commands = runner.action(action_id(COMMENTS_NEXT));
        let (page_one, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::comments("hunter_q", "ep-1", api::CommentOrder::Newest, 1)
        );
        let commands = runner.task_outcome(page_one, TaskOutcome::Failed(TaskError::TimedOut));
        let drawn = format!("{:?}", last_screen(&commands));
        assert!(drawn.contains("Cached comment"));
        assert!(drawn.contains("Try again"));

        let commands = runner.action(action_id(RETRY));
        let (_, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::comments("hunter_q", "ep-1", api::CommentOrder::Newest, 1)
        );
    }

    #[test]
    fn retrying_a_previous_comment_page_restores_its_last_item() {
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_NEXT));
        let (hot, _) = only_spawn(&commands);
        runner.task_outcome(
            hot,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(1, "Best", "Hot", 40, 0, 10)],
                0,
                1,
                21,
            )),
        );
        let commands = runner.action(action_id(ALL_COMMENTS));
        let (page_zero, _) = only_spawn(&commands);
        runner.task_outcome(
            page_zero,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(11, "Page zero", "Newest", 0, 0, 20)],
                0,
                2,
                21,
            )),
        );
        let commands = runner.action(action_id(COMMENTS_NEXT));
        let (page_one, _) = only_spawn(&commands);
        runner.task_outcome(
            page_one,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(21, "Page one", "Older", 0, 0, 1)],
                1,
                2,
                21,
            )),
        );

        let commands = runner.action(action_id(COMMENTS_PREVIOUS));
        let (previous, _) = only_spawn(&commands);
        runner.task_outcome(previous, TaskOutcome::Failed(TaskError::TimedOut));
        let commands = runner.action(action_id(RETRY));
        let (retry, _) = only_spawn(&commands);
        runner.task_outcome(
            retry,
            TaskOutcome::Completed(comments_response(
                &[
                    comment_entry(11, "First", "Newest", 0, 0, 20),
                    comment_entry(12, "Last", "Next", 0, 0, 19),
                ],
                0,
                2,
                21,
            )),
        );

        let state = runner.app().comments.as_ref().expect("comments");
        assert_eq!(state.item, 1);
        assert_eq!(state.comments[state.item].author, "Last");
    }

    #[test]
    fn filtered_comment_page_can_advance_to_the_next_server_page() {
        let hidden = comment_entry(11, "Hidden", "Removed", 0, 0, 20).replacen(
            "\"delete\":false",
            "\"delete\":true",
            1,
        );
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_NEXT));
        let (hot, _) = only_spawn(&commands);
        runner.task_outcome(
            hot,
            TaskOutcome::Completed(comments_response(
                &[comment_entry(1, "Best", "Hot", 40, 0, 10)],
                0,
                1,
                21,
            )),
        );
        let commands = runner.action(action_id(ALL_COMMENTS));
        let (page_zero, _) = only_spawn(&commands);
        let commands = runner.task_outcome(
            page_zero,
            TaskOutcome::Completed(comments_response(&[hidden], 0, 2, 21)),
        );
        assert!(format!("{:?}", last_screen(&commands)).contains("Next comment"));

        let commands = runner.action(action_id(COMMENTS_NEXT));
        let (_, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::comments("hunter_q", "ep-1", api::CommentOrder::Newest, 1)
        );
    }

    #[test]
    fn filtered_reply_page_can_advance_to_the_next_server_page() {
        let mut runner = seeded_reader(1, 0, false);
        let parent = comment_entry(11, "Author", "Parent", 40, 11, 20);
        let commands = runner.action(action_id(READER_NEXT));
        let (appendix, _) = only_spawn(&commands);
        runner.task_outcome(
            appendix,
            TaskOutcome::Completed(comments_response(std::slice::from_ref(&parent), 0, 1, 1)),
        );
        let commands = runner.action(action_id(ALL_COMMENTS));
        let (comments, _) = only_spawn(&commands);
        runner.task_outcome(
            comments,
            TaskOutcome::Completed(comments_response(std::slice::from_ref(&parent), 0, 1, 1)),
        );
        let commands = runner.action(action_id("comment-11"));
        let (replies, _) = only_spawn(&commands);
        let hidden = comment_entry(21, "Hidden", "Removed", 0, 0, 21).replacen(
            "\"blind\":false",
            "\"blind\":true",
            1,
        );
        let response =
            String::from_utf8(replies_response(&parent, std::slice::from_ref(&hidden), 11))
                .expect("reply fixture")
                .replacen("\"totalPages\":1", "\"totalPages\":3", 1)
                .into_bytes();
        runner.task_outcome(replies, TaskOutcome::Completed(response));
        let screen = runner.app().screen();
        assert!(format!("{screen:?}").contains("Next"));

        let commands = runner.action(action_id(REPLIES_NEXT));
        let (page_one, work) = only_spawn(&commands);
        assert_eq!(work, api::replies(11, api::CommentOrder::Hot, 1));
        let response = String::from_utf8(replies_response(&parent, &[hidden], 11))
            .expect("reply fixture")
            .replacen("\"totalPages\":1", "\"totalPages\":3", 1)
            .replacen("\"number\":0", "\"number\":1", 1)
            .into_bytes();
        runner.task_outcome(page_one, TaskOutcome::Completed(response));
        let screen = runner.app().screen();
        assert!(format!("{screen:?}").contains("Previous"));
        assert!(format!("{screen:?}").contains("Next"));
        let commands = runner.action(action_id(REPLIES_NEXT));
        let (_, work) = only_spawn(&commands);
        assert_eq!(work, api::replies(11, api::CommentOrder::Hot, 2));
    }

    #[test]
    fn successful_page_turn_shows_busy_footer_before_replacing_handle() {
        let mut runner = seeded_reader(2, 0, true);
        let commands = runner.action(action_id(READER_NEXT));
        let reader = runner.app().reader.as_ref().expect("reader");
        assert_eq!(reader.offset_rows, viewport_stride(reader.panel_height, 90));
        assert!(!reader.chrome_visible);
        let screens = commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| match command {
                Command::SetScreen(screen) => Some((index, screen)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(screens.len(), 2, "busy screen followed by the new page");
        let busy = screens[0].1.reading_surface.expect("busy reading surface");
        assert_eq!(busy.picture.handle, PictureHandle(7));
        assert_eq!(busy.chrome, ReadingChrome::OverlayBusy);
        let busy_progress = screens[0]
            .1
            .page_turns
            .expect("busy page turns")
            .progress
            .expect("busy progress");
        assert_eq!(busy_progress.percent, 50);
        let installed_progress = screens[1]
            .1
            .page_turns
            .expect("installed page turns")
            .progress
            .expect("installed progress");
        assert_eq!(
            installed_progress.percent,
            reader_progress_percent(
                reader.offset_rows,
                reader.panel_height,
                reader.strip.total_rows,
            )
        );
        assert!(installed_progress.previous);
        let put = commands
            .iter()
            .position(|command| matches!(command, Command::PutPicture { .. }))
            .expect("PutPicture");
        let drop = commands
            .iter()
            .position(|command| matches!(command, Command::DropPicture(_)))
            .expect("DropPicture");
        assert!(screens[0].0 < put && put < screens[1].0 && screens[1].0 < drop);
    }

    fn assert_prepared_turn_commands(
        commands: &[Command],
        format: PictureFormat,
        old_handle: PictureHandle,
    ) {
        let put_indices = commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| {
                matches!(command, Command::PutPicture { .. }).then_some(index)
            })
            .collect::<Vec<_>>();
        let set_indices = commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| {
                matches!(command, Command::SetScreen(_)).then_some(index)
            })
            .collect::<Vec<_>>();
        let drop_indices = commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| {
                matches!(command, Command::DropPicture(_)).then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(put_indices.len(), 1);
        assert_eq!(set_indices.len(), 2);
        assert_eq!(drop_indices.len(), 1);
        assert!(
            set_indices[0] < put_indices[0]
                && put_indices[0] < set_indices[1]
                && set_indices[1] < drop_indices[0]
        );
        match (&commands[put_indices[0]], format) {
            (
                Command::PutPicture {
                    pixels: PicturePixels::Gray8(_),
                    ..
                },
                PictureFormat::Gray8,
            )
            | (
                Command::PutPicture {
                    pixels: PicturePixels::Rgb8(_),
                    ..
                },
                PictureFormat::Rgb8,
            ) => {}
            (command, _) => panic!("wrong typed page upload: {command:?}"),
        }
        let Command::SetScreen(busy_screen) = &commands[set_indices[0]] else {
            unreachable!();
        };
        let busy = busy_screen
            .reading_surface
            .expect("prepared turn busy reading surface");
        assert_eq!(busy.picture.handle, old_handle);
        assert_eq!(busy.chrome, ReadingChrome::OverlayBusy);
        let Command::SetScreen(page_screen) = &commands[set_indices[1]] else {
            unreachable!();
        };
        let page = page_screen
            .reading_surface
            .expect("prepared turn page reading surface");
        assert_eq!(page.chrome, ReadingChrome::Hidden);
        assert!(matches!(
            commands[drop_indices[0]],
            Command::DropPicture(handle) if handle == old_handle
        ));
        assert!(!commands.iter().any(|command| matches!(
            command,
            Command::Spawn {
                work: Task::Fetch { .. },
                ..
            }
        )));
        assert!(commands.iter().all(|command| {
            !matches!(command, Command::Spawn { .. })
                || matches!(
                    command,
                    Command::Spawn {
                        work: Task::Sleep { seconds: 0 },
                        ..
                    }
                )
        }));
    }

    #[test]
    fn prepared_page_turn_is_typed_synchronous_and_replenishes_only_the_far_edge() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let mut runner = prepared_reader(format);
            let limits = reader_limits(format);
            let initial_pages = runner
                .app()
                .reader
                .as_ref()
                .expect("reader")
                .window
                .iter()
                .map(entry_offset)
                .collect::<Vec<_>>();
            assert_eq!(
                initial_pages,
                (1..=u64::try_from(limits.pages).expect("viewport limit")).collect::<Vec<_>>()
            );
            let old_handle = runner
                .app()
                .reader
                .as_ref()
                .and_then(|reader| reader.picture)
                .expect("displayed page")
                .handle;

            let commands = runner.action(action_id(READER_NEXT));
            assert_prepared_turn_commands(&commands, format, old_handle);

            let reader = runner.app().reader.as_ref().expect("reader");
            assert_eq!(reader.offset_rows, 1);
            assert_eq!(
                reader.window.iter().map(entry_offset).collect::<Vec<_>>(),
                (2..=u64::try_from(limits.pages).expect("viewport limit")).collect::<Vec<_>>()
            );
            assert!(reader
                .window
                .iter()
                .all(|entry| matches!(entry, ViewportEntry::Ready { .. })));
            let maintenance = reader.maintenance_task.expect("turn maintenance");

            let commands = runner.task_outcome(maintenance, TaskOutcome::Completed(Vec::new()));
            assert!(!commands.iter().any(|command| matches!(
                command,
                Command::PutPicture { .. } | Command::SetScreen(_) | Command::DropPicture(_)
            )));
            let (far_edge_task, work) = only_spawn(&commands);
            assert!(matches!(work, Task::Fetch { .. }));
            let far_edge = limits.pages + 1;
            let far_edge_offset = u64::try_from(far_edge).expect("far edge offset");
            assert_eq!(
                runner
                    .app()
                    .reader_tasks
                    .get(&far_edge_task)
                    .map(|entry| entry.purpose),
                Some(ReaderTaskPurpose::PrefetchSource { source: far_edge })
            );
            let reader = runner.app().reader.as_ref().expect("reader");
            assert_eq!(
                reader.window.iter().map(entry_offset).collect::<Vec<_>>(),
                (2..=far_edge_offset).collect::<Vec<_>>()
            );
            assert!(matches!(
                reader.window.back(),
                Some(ViewportEntry::Building(build)) if build.offset_rows == far_edge_offset
            ));
            assert_reader_bounds(runner.app());
        }
    }

    struct Task7SimulatorFlow {
        runner: AppRunner<Bomtoon>,
        width: u32,
        height: u32,
        first_pixels: PicturePixels,
        first_png: Vec<u8>,
    }

    fn start_task7_simulator_flow(
        format: PictureFormat,
        width: i32,
        height: i32,
    ) -> Task7SimulatorFlow {
        let metrics = DisplayMetrics {
            width,
            height,
            picture_format: format,
            ..CLARA_BW_METRICS
        };
        let expected_width = u32::try_from(width).expect("positive panel width");
        let expected_height = u32::try_from(height).expect("positive panel height");
        let (mut runner, manifest_task, _) = reader_waiting_for_manifest_with_metrics(metrics);
        runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest_sources(6)),
        );
        let first_page_commands = (0..16)
            .find_map(|_| {
                let (&task, entry) = runner
                    .app()
                    .reader_tasks
                    .iter()
                    .next()
                    .expect("first page reader work");
                let commands = runner.task_outcome(
                    task,
                    TaskOutcome::Completed(reader_task_completion(entry.purpose)),
                );
                assert_reader_bounds(runner.app());
                commands
                    .iter()
                    .any(|command| {
                        matches!(
                            command,
                            Command::SetScreen(screen) if screen.reading_surface.is_some()
                        )
                    })
                    .then_some(commands)
            })
            .expect("first page before maintenance");
        let first_put = first_page_commands
            .iter()
            .position(|command| matches!(command, Command::PutPicture { .. }))
            .expect("first page upload");
        let first_screen = first_page_commands
            .iter()
            .position(|command| {
                matches!(
                    command,
                    Command::SetScreen(screen) if screen.reading_surface.is_some()
                )
            })
            .expect("first reader screen");
        let first_maintenance = first_page_commands
            .iter()
            .position(|command| {
                matches!(
                    command,
                    Command::Spawn {
                        work: Task::Sleep { seconds: 0 },
                        ..
                    }
                )
            })
            .expect("first maintenance");
        assert!(first_put < first_screen && first_screen < first_maintenance);
        let reader = runner.app().reader.as_ref().expect("first page reader");
        assert_eq!(reader.offset_rows, 0);
        assert!(
            reader.window.is_empty(),
            "maintenance ran before first paint"
        );
        let (page_width, page_height, first_pixels) = uploaded_picture(&first_page_commands);
        assert_eq!((page_width, page_height), (expected_width, expected_height));
        let first_png = strict_picture_png(page_width, page_height, &first_pixels);
        Task7SimulatorFlow {
            runner,
            width: page_width,
            height: page_height,
            first_pixels,
            first_png,
        }
    }

    fn settle_task7_reader(runner: &mut AppRunner<Bomtoon>, format: PictureFormat) {
        let limits = reader_limits(format);
        for _ in 0..64 {
            let settled = {
                let app = runner.app();
                let reader = app.reader.as_ref().expect("lookahead reader");
                let expected = limits.pages;
                reader.window.len() == expected
                    && reader
                        .window
                        .iter()
                        .all(|entry| matches!(entry, ViewportEntry::Ready { .. }))
                    && app.reader_tasks.is_empty()
            };
            if settled {
                break;
            }
            let (&task, entry) = runner
                .app()
                .reader_tasks
                .iter()
                .next()
                .expect("lookahead reader work");
            runner.task_outcome(
                task,
                TaskOutcome::Completed(reader_task_completion(entry.purpose)),
            );
            assert_reader_bounds(runner.app());
        }
        let reader = runner.app().reader.as_ref().expect("prepared lookahead");
        assert_eq!(reader.window.len(), limits.pages);
        assert!(reader
            .window
            .iter()
            .all(|entry| matches!(entry, ViewportEntry::Ready { .. })));
        assert!(runner.app().reader_tasks.is_empty());
    }

    fn task7_seam_next(
        runner: &mut AppRunner<Bomtoon>,
        format: PictureFormat,
        page_width: u32,
        page_height: u32,
    ) -> Vec<u8> {
        let next_commands = runner.action(action_id(READER_NEXT));
        assert_eq!(
            runner
                .app()
                .reader
                .as_ref()
                .expect("next reader")
                .offset_rows,
            viewport_stride(page_height, 90)
        );
        assert!(next_commands.iter().any(|command| {
            matches!(
                command,
                Command::SetScreen(screen) if screen.reading_surface.is_some()
            )
        }));
        assert!(!next_commands.iter().any(|command| {
            matches!(
                command,
                Command::Spawn {
                    work: Task::Fetch { .. },
                    ..
                }
            )
        }));
        let (next_width, next_height, next_pixels) = uploaded_picture(&next_commands);
        assert_eq!((next_width, next_height), (page_width, page_height));
        let channels = if format == PictureFormat::Gray8 { 1 } else { 3 };
        let seam_row = usize::try_from(
            2_u32
                .checked_mul(page_width)
                .and_then(|boundary| boundary.checked_sub(page_height))
                .expect("seam row"),
        )
        .expect("seam row fits");
        let stride = usize::try_from(page_width)
            .expect("page width fits")
            .checked_mul(channels)
            .expect("row stride");
        let next_bytes = match &next_pixels {
            PicturePixels::Gray8(bytes) | PicturePixels::Rgb8(bytes) => bytes,
        };
        match (format, &next_pixels) {
            (PictureFormat::Gray8, PicturePixels::Gray8(_)) => {}
            (PictureFormat::Rgb8, PicturePixels::Rgb8(bytes)) => {
                assert!(bytes.chunks_exact(3).all(|pixel| pixel == [255, 0, 0]));
            }
            _ => panic!("{format:?} simulator upload used the wrong pixel type"),
        }
        for row in seam_row.saturating_sub(1)..=seam_row {
            let start = row.checked_mul(stride).expect("row offset");
            assert!(
                next_bytes[start..start + stride]
                    .iter()
                    .any(|sample| *sample != u8::MAX),
                "{format:?} left a white row at the source seam"
            );
        }
        let next_png = strict_picture_png(next_width, next_height, &next_pixels);
        let chrome_commands = runner.action(action_id(READER_CHROME));
        assert_eq!(
            last_screen(&chrome_commands)
                .reading_surface
                .expect("reader chrome surface")
                .chrome,
            ReadingChrome::Overlay
        );
        assert_eq!(
            runner
                .app()
                .reader
                .as_ref()
                .expect("chrome reader")
                .offset_rows,
            viewport_stride(page_height, 90)
        );
        next_png
    }

    fn task7_retry_previous(
        runner: &mut AppRunner<Bomtoon>,
        page_width: u32,
        page_height: u32,
        first_pixels: &PicturePixels,
    ) -> Vec<u8> {
        let previous_commands = runner.action(action_id(READER_PREVIOUS));
        let previous_surface = last_screen(&previous_commands)
            .reading_surface
            .expect("current page remains while the previous page loads");
        assert_eq!(previous_surface.chrome, ReadingChrome::OverlayBusy);
        assert_eq!(
            Some(previous_surface.picture),
            runner.app().reader.as_ref().expect("reader").picture
        );
        let failed_source = runner
            .app()
            .foreground_reader_task
            .expect("Previous foreground source");
        let failure_commands =
            runner.task_outcome(failed_source, TaskOutcome::Failed(TaskError::TimedOut));
        assert!(!failure_commands
            .iter()
            .any(|command| matches!(command, Command::Spawn { .. })));
        assert!(runner.app().problem.is_some());
        let failed_offset = previous_viewport_offset(
            runner.app().reader.as_ref().expect("reader").offset_rows,
            page_height,
        )
        .expect("previous offset");
        assert_eq!(runner.app().retry, Retry::Viewport(failed_offset));

        let mut retry_commands = runner.action(action_id(RETRY));
        assert!(retry_commands.iter().any(|command| {
            matches!(
                command,
                Command::Spawn {
                    work: Task::Fetch { .. },
                    ..
                }
            )
        }));
        let previous_upload = (0..16)
            .find_map(|_| {
                if retry_commands
                    .iter()
                    .any(|command| matches!(command, Command::PutPicture { .. }))
                {
                    return Some(uploaded_picture(&retry_commands));
                }
                let task = runner
                    .app()
                    .foreground_reader_task
                    .or_else(|| runner.app().reader_tasks.keys().next().copied())
                    .expect("retry reader work");
                let purpose = runner
                    .app()
                    .reader_tasks
                    .get(&task)
                    .expect("retry task entry")
                    .purpose;
                retry_commands = runner.task_outcome(
                    task,
                    TaskOutcome::Completed(reader_task_completion(purpose)),
                );
                assert_reader_bounds(runner.app());
                None
            })
            .expect("Previous page after retry");
        assert_eq!(previous_upload.0, page_width);
        assert_eq!(previous_upload.1, page_height);
        assert_eq!(&previous_upload.2, first_pixels);
        assert_eq!(
            runner
                .app()
                .reader
                .as_ref()
                .expect("Previous reader")
                .offset_rows,
            failed_offset
        );
        strict_picture_png(previous_upload.0, previous_upload.1, &previous_upload.2)
    }

    fn run_task7_simulator_flow(
        format: PictureFormat,
        width: i32,
        height: i32,
        label: &str,
        evidence_dir: Option<&std::path::Path>,
    ) -> String {
        let Task7SimulatorFlow {
            mut runner,
            width: page_width,
            height: page_height,
            first_pixels,
            first_png,
        } = start_task7_simulator_flow(format, width, height);
        settle_task7_reader(&mut runner, format);
        let next_png = task7_seam_next(&mut runner, format, page_width, page_height);
        let previous_png =
            task7_retry_previous(&mut runner, page_width, page_height, &first_pixels);
        let back_commands = runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Episodes);
        assert!(runner.app().reader.is_none());
        assert_eq!(
            back_commands
                .iter()
                .filter(|command| matches!(command, Command::DropPicture(_)))
                .count(),
            1
        );
        if let Some(directory) = evidence_dir {
            for (name, png) in [
                (format!("{label}-page1-before-maintenance.png"), first_png),
                (format!("{label}-seam-next.png"), next_png),
                (format!("{label}-previous-after-retry.png"), previous_png),
            ] {
                std::fs::write(directory.join(name), png).expect("write simulator screenshot");
            }
        }
        format!(
            "{label} {width}x{height} {format:?}: page1-before-maintenance; \
             prepared seam Next without fetch/loading; chrome Overlay at page 2; \
             Previous fetch failed; app retry restored \
             exact page 1 pixels; Back dropped one picture"
        )
    }

    #[test]
    fn both_format_simulator_flow_is_typed_seamless_retryable_and_bounded() {
        let evidence_dir =
            std::env::var_os("BOMTOON_TASK7_EVIDENCE_DIR").map(std::path::PathBuf::from);
        if let Some(directory) = &evidence_dir {
            std::fs::create_dir_all(directory).expect("create simulator evidence directory");
        }
        let evidence_log = [
            (PictureFormat::Gray8, 1_072, 1_448, "gray8"),
            (PictureFormat::Rgb8, 1_264, 1_680, "rgb8"),
        ]
        .map(|(format, width, height, label)| {
            run_task7_simulator_flow(format, width, height, label, evidence_dir.as_deref())
        });
        if let Some(directory) = evidence_dir {
            std::fs::write(
                directory.join("task-7-simulator.log"),
                format!("{}\n", evidence_log.join("\n")),
            )
            .expect("write simulator log");
        }
    }

    #[test]
    fn page_turn_miss_promotes_prefetch_and_retains_displayed_page_while_loading() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let metrics = reader_metrics(format, 1);
            let mut runner = seeded_reader_with_metrics(metrics, 3, 0, false);
            let prefetch = TaskId(41);
            {
                let app = runner.app_mut();
                let reader = app.reader.as_mut().expect("reader");
                reader.window.clear();
                let plan = viewport_plan(&reader.strip, 1, 1).expect("viewport-one plan");
                reader.window.push_back(ViewportEntry::Building(
                    ViewportBuild::new(1, plan, format, 1, 1).expect("viewport-one build"),
                ));
                reader.source_fetches.insert(1, prefetch);
                app.reader_tasks.insert(
                    prefetch,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::PrefetchSource { source: 1 },
                    },
                );
            }

            let commands = runner.action(action_id(READER_NEXT));
            assert!(!commands
                .iter()
                .any(|command| matches!(command, Command::Spawn { .. } | Command::Cancel(_))));
            assert!(!commands.iter().any(|command| matches!(
                command,
                Command::PutPicture { .. } | Command::DropPicture(_)
            )));
            let screen = last_screen(&commands);
            let surface = screen
                .reading_surface
                .expect("the displayed page remains visible while its replacement loads");
            assert_eq!(surface.picture.handle, PictureHandle(7));
            assert_eq!(surface.chrome, ReadingChrome::OverlayBusy);
            let app = runner.app();
            let reader = app.reader.as_ref().expect("reader");
            assert_eq!(reader.offset_rows, 0);
            assert_eq!(
                reader.picture.map(|picture| picture.handle),
                Some(PictureHandle(7))
            );
            assert_eq!(app.retry, Retry::Viewport(1));
            assert_eq!(app.foreground_reader_task, Some(prefetch));
            assert_eq!(
                app.reader_tasks.get(&prefetch).map(|entry| entry.purpose),
                Some(ReaderTaskPurpose::ForegroundSource {
                    source: 1,
                    offset_rows: 1,
                })
            );
            assert_eq!(reader.source_fetches.get(&1), Some(&prefetch));
            assert_reader_bounds(app);
        }
    }

    #[test]
    fn previous_rebase_cancels_prefetch_outside_the_target_viewport() {
        let format = PictureFormat::Gray8;
        let mut runner = seeded_reader_with_metrics(reader_metrics(format, 1), 6, 3, false);
        let target_task = TaskId(51);
        let future_task = TaskId(52);
        {
            let app = runner.app_mut();
            let reader = app.reader.as_mut().expect("reader");
            reader.source_fetches = BTreeMap::from([(2, target_task), (4, future_task)]);
            app.reader_tasks = BTreeMap::from([
                (
                    target_task,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::PrefetchSource { source: 2 },
                    },
                ),
                (
                    future_task,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::PrefetchSource { source: 4 },
                    },
                ),
            ]);
        }

        let commands = runner.action(action_id(READER_PREVIOUS));
        assert!(commands.contains(&Command::Cancel(future_task)));
        assert!(!commands.contains(&Command::Cancel(target_task)));
        let reader = runner.app().reader.as_ref().expect("reader");
        assert_eq!(
            reader.window.iter().map(entry_offset).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(runner.app().foreground_reader_task, Some(target_task));
        assert!(!reader.source_fetches.contains_key(&4));
        assert!(!runner.app().reader_tasks.contains_key(&future_task));

        let commands = runner.task_outcome(future_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        assert!(commands.is_empty());
        let reader = runner.app().reader.as_ref().expect("reader");
        assert!(!reader.source_cache.contains_key(&4));
        assert_reader_bounds(runner.app());
    }

    #[test]
    fn previous_rebase_evicts_cached_sources_outside_the_new_forward_range() {
        let format = PictureFormat::Gray8;
        let mut runner = seeded_reader_with_metrics(reader_metrics(format, 1), 6, 5, false);
        runner
            .app_mut()
            .reader
            .as_mut()
            .expect("reader")
            .source_cache
            .insert(
                0,
                Picture::from_grey(1, 1, vec![0]).expect("unrelated cached source"),
            );
        assert_reader_bounds(runner.app());

        runner.action(action_id(READER_PREVIOUS));

        let reader = runner.app().reader.as_ref().expect("reader");
        assert!(!reader.source_cache.contains_key(&0));
        assert_eq!(
            reader.window.iter().map(entry_offset).collect::<Vec<_>>(),
            vec![4]
        );
        assert_reader_bounds(runner.app());
    }

    fn assert_previous_seam_upload(
        runner: &AppRunner<Bomtoon>,
        commands: &[Command],
        format: PictureFormat,
    ) -> TaskId {
        let (uploaded_format, uploaded) = commands
            .iter()
            .find_map(|command| match command {
                Command::PutPicture {
                    pixels: PicturePixels::Gray8(pixels),
                    ..
                } => Some((PictureFormat::Gray8, pixels.as_slice())),
                Command::PutPicture {
                    pixels: PicturePixels::Rgb8(pixels),
                    ..
                } => Some((PictureFormat::Rgb8, pixels.as_slice())),
                _ => None,
            })
            .expect("rebuilt seam upload");
        assert_eq!(uploaded_format, format);
        match format {
            PictureFormat::Gray8 => assert_eq!(uploaded, &[0, 255]),
            PictureFormat::Rgb8 => assert_eq!(uploaded, &[0, 0, 0, 255, 255, 255]),
        }
        let put = commands
            .iter()
            .position(|command| matches!(command, Command::PutPicture { .. }))
            .expect("PutPicture");
        let set = commands
            .iter()
            .position(|command| matches!(command, Command::SetScreen(_)))
            .expect("SetScreen");
        let drop = commands
            .iter()
            .position(|command| matches!(command, Command::DropPicture(PictureHandle(7))))
            .expect("old DropPicture");
        assert!(put < set && set < drop);
        let reader = runner.app().reader.as_ref().expect("reader");
        assert_eq!(reader.offset_rows, 2);
        assert!(reader.window.iter().all(|entry| entry_offset(entry) >= 2));
        reader.maintenance_task.expect("seam maintenance")
    }

    #[test]
    fn previous_rerenders_exact_global_seam_and_keeps_no_backward_pages() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, first_task, active_second, maintenance_task) =
                seam_previous_reader(format);
            let commands = runner.action(action_id(READER_PREVIOUS));
            assert!(!commands
                .iter()
                .any(|command| matches!(command, Command::Spawn { .. })));
            assert_eq!(
                commands
                    .iter()
                    .filter_map(|command| match command {
                        Command::Cancel(task) => Some(*task),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                vec![maintenance_task]
            );
            let previous_surface = last_screen(&commands)
                .reading_surface
                .expect("current page remains while the previous page loads");
            assert_eq!(previous_surface.chrome, ReadingChrome::OverlayBusy);
            assert_eq!(
                Some(previous_surface.picture),
                runner.app().reader.as_ref().expect("reader").picture
            );
            assert_eq!(
                runner
                    .app()
                    .reader_tasks
                    .get(&first_task)
                    .map(|entry| entry.purpose),
                Some(ReaderTaskPurpose::ForegroundSource {
                    source: 0,
                    offset_rows: 2,
                })
            );
            let reader = runner.app().reader.as_ref().expect("reader");
            assert_eq!(
                reader.window.iter().map(entry_offset).collect::<Vec<_>>(),
                vec![2]
            );
            assert!(reader.window.iter().all(|entry| entry_offset(entry) >= 2));

            let commands =
                runner.task_outcome(first_task, TaskOutcome::Completed(BLACK_1X3_WEBP.to_vec()));
            assert!(
                commands.iter().all(|command| {
                    !matches!(command, Command::SetScreen(screen) if screen.reading_surface.is_some())
                }),
                "{format:?} rerendered the old page before the seam was ready: {commands:?}"
            );
            assert_eq!(
                runner
                    .app()
                    .reader
                    .as_ref()
                    .and_then(|reader| reader.picture)
                    .map(|picture| picture.handle),
                Some(PictureHandle(7))
            );
            let second_task = if let Some(task) = active_second {
                assert!(!commands
                    .iter()
                    .any(|command| matches!(command, Command::Spawn { .. })));
                task
            } else {
                let (task, work) = only_spawn(&commands);
                assert!(matches!(work, Task::Fetch { .. }));
                task
            };
            assert_eq!(runner.app().foreground_reader_task, Some(second_task));
            assert_eq!(
                runner
                    .app()
                    .reader_tasks
                    .get(&second_task)
                    .map(|entry| entry.purpose),
                Some(ReaderTaskPurpose::ForegroundSource {
                    source: 1,
                    offset_rows: 2,
                })
            );

            let commands =
                runner.task_outcome(second_task, TaskOutcome::Completed(WHITE_1X2_WEBP.to_vec()));
            let maintenance = assert_previous_seam_upload(&runner, &commands, format);

            runner.task_outcome(maintenance, TaskOutcome::Completed(Vec::new()));
            let reader = runner.app().reader.as_ref().expect("reader");
            assert!(reader.window.iter().all(|entry| entry_offset(entry) >= 2));
            assert_reader_bounds(runner.app());
        }
    }

    #[test]
    fn manifest_credentials_alone_change_account_state() {
        for (error, expected) in [
            (TaskError::NoCredential, AccountState::SignedOut),
            (TaskError::Unauthorized, AccountState::Expired),
        ] {
            let (mut runner, manifest_task, _) = reader_waiting_for_manifest();
            runner.task_outcome(manifest_task, TaskOutcome::Failed(error));
            assert_eq!(runner.app().account, expected);
        }
    }

    #[test]
    fn stale_image_outcome_cannot_mutate_reader() {
        let (mut runner, image_task) = reader_waiting_for_first_image();
        let before = runner.app().reader_tasks.clone();
        let commands = runner.task_outcome(
            TaskId(image_task.0 + 1),
            TaskOutcome::Completed(TINY_WEBP.to_vec()),
        );
        assert!(commands.is_empty());
        assert_eq!(runner.app().reader_tasks, before);
    }
    #[test]
    fn reader_generation_mismatch_and_unknown_task_are_no_ops() {
        let mut runner = seeded_reader(1, 0, false);
        assert!(runner
            .task_outcome(TaskId(90), TaskOutcome::Completed(Vec::new()))
            .is_empty());
        runner.app_mut().reader_tasks.insert(
            TaskId(91),
            ReaderTaskEntry {
                generation: 0,
                purpose: ReaderTaskPurpose::Maintenance,
            },
        );
        let commands = runner.task_outcome(TaskId(91), TaskOutcome::Completed(Vec::new()));
        assert!(commands.is_empty());
        let reader = runner.app().reader.as_ref().expect("reader");
        assert_eq!(reader.generation, 1);
        assert_eq!(reader.offset_rows, 0);
        assert_eq!(
            reader.picture.map(|picture| picture.handle),
            Some(PictureHandle(7))
        );
    }

    #[test]
    fn back_during_reader_loading_cancels_task_and_ignores_late_outcome() {
        let (mut runner, manifest_task, commands) = reader_waiting_for_manifest();
        assert!(last_screen(&commands).owns_back);

        let commands = runner.action(ActionId::BACK);
        assert!(commands.contains(&Command::Cancel(manifest_task)));
        assert_eq!(runner.app().view, View::Episodes);
        assert_eq!(runner.app().pending, None);
        assert_eq!(runner.app().task, None);
        assert!(runner.app().reader_selection.is_none());

        let commands = runner.task_outcome(manifest_task, TaskOutcome::Cancelled);
        assert!(commands.is_empty());
    }

    #[test]
    fn back_and_logout_release_reader_state_and_picture() {
        let mut runner = seeded_reader(1, 0, true);
        let commands = runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Episodes);
        assert!(runner.app().reader.is_none());
        let set = commands
            .iter()
            .position(|command| matches!(command, Command::SetScreen(_)))
            .expect("SetScreen");
        let drop = commands
            .iter()
            .position(|command| matches!(command, Command::DropPicture(_)))
            .expect("DropPicture");
        assert!(set < drop);

        let mut runner = seeded_reader(1, 0, false);
        runner.app_mut().pending = Some(Pending::Logout);
        runner.app_mut().task = Some(TaskId(77));
        let commands = runner.task_outcome(TaskId(77), TaskOutcome::Completed(Vec::new()));
        assert!(runner.app().reader.is_none());
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::DropPicture(_))));
    }

    #[test]
    fn reader_cleanup_back_clears_every_populated_reader_collection_once() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, tasks) = fully_populated_reader(format);
            let commands = runner.action(ActionId::BACK);
            assert_eq!(runner.app().view, View::Episodes);
            let set = commands
                .iter()
                .position(|command| matches!(command, Command::SetScreen(_)))
                .expect("episode screen");
            let drop = commands
                .iter()
                .position(|command| matches!(command, Command::DropPicture(_)))
                .expect("reader picture drop");
            assert!(set < drop);
            assert_reader_cleanup(
                &mut runner,
                &commands,
                tasks,
                None,
                Some(Pending::Content(99)),
                Some(TaskId(99)),
                None,
            );
        }
    }

    #[test]
    fn reader_cleanup_suspend_and_link_loss_clear_only_reader_work() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            for exit in [false, true] {
                let (mut runner, tasks) = fully_populated_reader(format);
                let commands = if exit {
                    runner.exit()
                } else {
                    runner.suspend()
                };
                assert_eq!(runner.app().view, View::Episodes);
                let (pending, task) = if exit {
                    (None, None)
                } else {
                    (Some(Pending::Content(99)), Some(TaskId(99)))
                };
                assert_reader_cleanup(
                    &mut runner,
                    &commands,
                    tasks,
                    None,
                    pending,
                    task,
                    exit.then_some(TaskId(99)),
                );
            }
        }
    }

    #[test]
    fn reader_cleanup_offline_ignores_every_late_outcome() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, tasks) = fully_populated_reader(format);
            let settled = tasks[0];
            let commands = runner.task_outcome(settled, TaskOutcome::Failed(TaskError::Offline));
            assert_eq!(runner.app().view, View::Episodes);
            assert_reader_cleanup(
                &mut runner,
                &commands,
                tasks,
                Some(settled),
                Some(Pending::Content(99)),
                Some(TaskId(99)),
                None,
            );
        }
    }

    #[test]
    fn reader_cleanup_logout_clears_populated_reader_after_singleton_settles() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, tasks) = fully_populated_reader(format);
            runner.app_mut().pending = Some(Pending::Logout);
            runner.app_mut().task = Some(TaskId(77));
            let commands = runner.task_outcome(TaskId(77), TaskOutcome::Completed(Vec::new()));
            assert_eq!(runner.app().account, AccountState::SignedOut);
            assert_reader_cleanup(&mut runner, &commands, tasks, None, None, None, None);
        }
    }

    #[test]
    fn image_failure_retry_stays_on_selected_episode() {
        let (mut runner, image_task) = reader_waiting_for_first_image();
        runner.task_outcome(image_task, TaskOutcome::Completed(vec![0, 1, 2, 3]));
        let commands = runner.action(action_id(RETRY));
        let (_, work) = only_spawn(&commands);
        assert!(matches!(
            work,
            Task::Fetch {
                credential: None,
                ..
            }
        ));
        let selection = runner.app().reader_selection.as_ref().expect("selection");
        assert_eq!(selection.content_alias, "hunter_q");
        assert_eq!(selection.episode_alias, "ep-1");
    }

    #[test]
    fn startup_loads_public_sources_and_account_scope_independently() {
        let (_, commands) = started();
        let spawned = spawns(&commands);
        assert_eq!(spawned.len(), 4);
        for work in [
            api::account_scope(),
            api::asset_summary(),
            api::library(0),
            api::homepage(),
        ] {
            assert!(spawned.iter().any(|(_, spawned)| *spawned == work));
        }
    }

    #[test]
    fn summary_refresh_requests_coalesce_into_one_follow_up() {
        let mut wallet = WalletState {
            summary_task: Some(TaskId(7)),
            ..WalletState::default()
        };
        assert_eq!(wallet.request_summary_generation(), None);
        assert_eq!(wallet.request_summary_generation(), None);
        assert!(wallet.summary_refresh_queued);

        wallet.summary_task = None;
        assert!(wallet.take_queued_summary_refresh());
        assert!(!wallet.take_queued_summary_refresh());
    }

    #[test]
    fn queued_summary_refresh_waits_until_reader_releases_task_capacity() {
        let (mut runner, commands) = started();
        let (summary_task, _) = fetch_task_with(&commands, "/asset/user");
        let mut reader_fixture = seeded_reader(1, 0, false);
        let (selection, reader, reader_generation) = {
            let fixture = reader_fixture.app_mut();
            (
                fixture.reader_selection.take(),
                fixture.reader.take(),
                fixture.reader_generation,
            )
        };
        let app = runner.app_mut();
        app.view = View::Reader;
        app.reader_selection = selection;
        app.reader = reader;
        app.reader_generation = reader_generation;
        app.wallet.summary_refresh_queued = true;

        let commands = runner.task_outcome(
            summary_task,
            TaskOutcome::Completed(ASSET_RESPONSE.to_vec()),
        );
        assert!(spawns(&commands).is_empty());
        assert!(runner.app().wallet.summary_refresh_queued);

        let commands = runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Episodes);
        assert_eq!(spawns(&commands).len(), 1);
        assert_eq!(
            fetch_task_with(&commands, "/asset/user").1,
            api::asset_summary()
        );
        assert!(!runner.app().wallet.summary_refresh_queued);
    }

    fn saturated_reader_with_deferred_summary() -> (AppRunner<Bomtoon>, Vec<TaskId>) {
        let metrics = reader_metrics(PictureFormat::Gray8, 1);
        let (mut runner, _) = loaded_library_with_metrics(metrics);
        complete_initial_summary(&mut runner);
        let commands = runner.action(action_id(ACCOUNT));
        let (summary_task, _) = fetch_task_with(&commands, "/asset/user");
        let (coin_task, _) = fetch_task_with(&commands, "coinKind=COIN");
        let (ticket_task, _) = fetch_task_with(&commands, "coinKind=TICKET");
        runner.app_mut().wallet.summary_refresh_queued = true;
        let back = runner.action(ActionId::BACK);
        assert!(back.contains(&Command::Cancel(coin_task)));
        assert!(back.contains(&Command::Cancel(ticket_task)));
        runner.task_outcome(coin_task, TaskOutcome::Cancelled);

        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);
        runner.task_outcome(content_task, TaskOutcome::Completed(content_response()));
        let commands = runner.action(action_id("episode-0"));
        let (manifest_task, _) = only_spawn(&commands);
        assert_eq!(runner.tasks_in_flight(), 4);

        runner.task_outcome(
            summary_task,
            TaskOutcome::Completed(ASSET_RESPONSE.to_vec()),
        );
        assert!(runner.app().wallet.summary_refresh_queued);
        let commands = runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest_sources(8)),
        );
        let (source_task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(source_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        let (maintenance_task, _) = only_spawn(&commands);
        runner.task_outcome(maintenance_task, TaskOutcome::Completed(Vec::new()));

        assert_eq!(runner.tasks_in_flight(), 4);
        let reader_tasks = runner
            .app()
            .reader_tasks
            .keys()
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(reader_tasks.len(), 2);
        (runner, reader_tasks)
    }

    #[test]
    fn full_reader_capacity_keeps_deferred_summary_until_cancel_settles() {
        let (mut runner, reader_tasks) = saturated_reader_with_deferred_summary();
        let commands = runner.action(ActionId::BACK);

        assert!(spawns(&commands).is_empty());
        assert!(runner.app().wallet.summary_refresh_queued);
        assert_eq!(runner.app().wallet.summary_task, None);
        assert!(!runner.app().wallet.summary_error);
        assert_eq!(
            commands
                .iter()
                .filter_map(|command| match command {
                    Command::Cancel(task) => Some(*task),
                    _ => None,
                })
                .collect::<BTreeSet<_>>(),
            reader_tasks.iter().copied().collect()
        );

        let commands = runner.task_outcome(reader_tasks[0], TaskOutcome::Cancelled);
        assert_eq!(spawns(&commands).len(), 1);
        assert_eq!(
            fetch_task_with(&commands, "/asset/user").1,
            api::asset_summary()
        );
        assert!(!runner.app().wallet.summary_refresh_queued);
    }

    #[test]
    fn suspend_resumes_deferred_summary_after_reader_cancel_settles() {
        let (mut runner, reader_tasks) = saturated_reader_with_deferred_summary();
        let commands = runner.suspend();

        assert!(spawns(&commands).is_empty());
        assert!(runner.app().wallet.summary_refresh_queued);
        assert_eq!(runner.app().view, View::Episodes);

        let commands = runner.task_outcome(reader_tasks[0], TaskOutcome::Cancelled);
        assert_eq!(spawns(&commands).len(), 1);
        assert_eq!(
            fetch_task_with(&commands, "/asset/user").1,
            api::asset_summary()
        );
        assert!(!runner.app().wallet.summary_refresh_queued);
    }

    #[test]
    fn cancelled_wallet_outcomes_preserve_cached_data_without_errors() {
        let (mut runner, commands) = started();
        let (summary_task, _) = fetch_task_with(&commands, "/asset/user");
        runner.app_mut().wallet.summary = Some(test_wallet_summary());
        runner.task_outcome(summary_task, TaskOutcome::Cancelled);
        assert_eq!(runner.app().wallet.summary, Some(test_wallet_summary()));
        assert!(!runner.app().wallet.summary_error);
        assert!(!runner.app().wallet.summary_stale);

        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        let commands = runner.action(action_id(ACCOUNT));
        let (coin_task, _) = fetch_task_with(&commands, "coinKind=COIN");
        let cached = expiration_row(
            model::AssetKind::Coin,
            model::AssetSubtype::Standard,
            5,
            None,
        );
        runner.app_mut().wallet.coin_history = vec![cached.clone()];
        runner.task_outcome(coin_task, TaskOutcome::Cancelled);
        assert_eq!(runner.app().wallet.coin_history, [cached]);
        assert!(!runner.app().wallet.coin_history_error);
    }

    #[test]
    fn exit_cancels_and_invalidates_all_wallet_tasks() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        let commands = runner.action(action_id(ACCOUNT));
        let wallet_tasks = spawns(&commands)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        assert_eq!(wallet_tasks.len(), 3);
        let summary_generation = runner.app().wallet.summary_generation;
        let detail_generation = runner.app().wallet.detail_generation;

        let commands = runner.exit();
        let cancelled = commands
            .iter()
            .filter_map(|command| match command {
                Command::Cancel(task) => Some(*task),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(cancelled, wallet_tasks);
        assert!(runner.app().wallet.tasks.is_empty());
        assert_eq!(runner.app().wallet.summary_task, None);
        assert!(!runner.app().wallet.summary_refresh_queued);
        assert_ne!(runner.app().wallet.summary_generation, summary_generation);
        assert_ne!(runner.app().wallet.detail_generation, detail_generation);
    }

    #[test]
    fn stale_summary_generation_is_rejected() {
        let mut wallet = WalletState::default();
        let stale = wallet.request_summary_generation().expect("generation 1");
        let current = wallet.request_summary_generation().expect("generation 2");
        assert_ne!(stale, current);
        wallet.summary = Some(model::WalletSummary {
            coins: model::AssetAmounts {
                standard: 20,
                bonus: 0,
                free: 0,
            },
            tickets: model::AssetAmounts::default(),
        });
        assert!(!wallet.accept_summary(
            stale,
            model::WalletSummary {
                coins: model::AssetAmounts {
                    standard: 10,
                    bonus: 0,
                    free: 0,
                },
                tickets: model::AssetAmounts::default(),
            },
        ));
        assert_eq!(
            wallet.summary.and_then(|summary| summary.coins.total()),
            Some(20)
        );
    }

    #[test]
    fn library_and_recent_top_bars_show_only_aggregate_coins() {
        let (mut runner, _) = loaded_library();
        runner.app_mut().wallet.summary = Some(WalletSummary {
            coins: model::AssetAmounts {
                standard: 7,
                bonus: 2,
                free: 1,
            },
            tickets: model::AssetAmounts {
                standard: 3,
                bonus: 1,
                free: 0,
            },
        });

        let screen = runner.app().screen();
        let drawn = format!("{screen:?}");
        let top_bar = screen.top_bar.as_ref().expect("Library top bar");
        assert_eq!(top_bar.title, "Library");
        assert_eq!(top_bar.actions[0].label, "Coins 10");
        assert!(!drawn.contains("Tickets"));
        assert_fits(&screen);

        runner.app_mut().destination = MainDestination::Recent;
        let screen = runner.app().screen();
        let drawn = format!("{screen:?}");
        let top_bar = screen.top_bar.as_ref().expect("Recent top bar");
        assert_eq!(top_bar.title, "Recent");
        assert_eq!(top_bar.actions[0].label, "Coins 10");
        assert!(!drawn.contains("Tickets"));
        assert_fits(&screen);
    }

    #[test]
    fn library_and_recent_top_bars_show_coin_loading() {
        let (mut runner, _) = loaded_library();
        runner.app_mut().wallet.summary = None;
        assert!(runner.app().wallet.summary_task.is_some());

        let screen = runner.app().screen();
        let drawn = format!("{screen:?}");
        let top_bar = screen.top_bar.as_ref().expect("Library top bar");
        assert_eq!(top_bar.title, "Library");
        assert_eq!(top_bar.actions[0].label, "Coins…");
        assert!(!drawn.contains("Tickets"));
        assert_fits(&screen);

        runner.app_mut().destination = MainDestination::Recent;
        let screen = runner.app().screen();
        let drawn = format!("{screen:?}");
        let top_bar = screen.top_bar.as_ref().expect("Recent top bar");
        assert_eq!(top_bar.title, "Recent");
        assert_eq!(top_bar.actions[0].label, "Coins…");
        assert!(!drawn.contains("Tickets"));
        assert_fits(&screen);
    }

    #[test]
    fn library_and_recent_top_bars_show_coin_unavailable() {
        let (mut runner, _) = loaded_library();
        runner.app_mut().wallet.summary = None;
        runner.app_mut().wallet.summary_task = None;
        runner.app_mut().wallet.summary_error = true;

        let screen = runner.app().screen();
        let drawn = format!("{screen:?}");
        let top_bar = screen.top_bar.as_ref().expect("Library top bar");
        assert_eq!(top_bar.title, "Library");
        assert_eq!(top_bar.actions[0].label, "Coins unavailable");
        assert!(!drawn.contains("Tickets"));
        assert_fits(&screen);

        runner.app_mut().destination = MainDestination::Recent;
        let screen = runner.app().screen();
        let drawn = format!("{screen:?}");
        let top_bar = screen.top_bar.as_ref().expect("Recent top bar");
        assert_eq!(top_bar.title, "Recent");
        assert_eq!(top_bar.actions[0].label, "Coins unavailable");
        assert!(!drawn.contains("Tickets"));
        assert_fits(&screen);
    }

    #[test]
    fn wallet_date_history_window_is_exactly_ninety_days_and_checked() {
        assert_eq!(HISTORY_WINDOW_MS, 7_776_000_000);
        assert_eq!(history_start_ms_at(HISTORY_WINDOW_MS + 42), Some(42));
        assert_eq!(history_start_ms_at(i64::MIN), None);
    }

    #[test]
    fn wallet_date_formats_fixed_asia_taipei_civil_dates() {
        assert_eq!(taipei_date(0), Some("1970-01-01".to_owned()));
        assert_eq!(
            taipei_date(1_709_136_000_000),
            Some("2024-02-29".to_owned())
        );
        assert_eq!(
            taipei_date(1_735_660_800_000),
            Some("2025-01-01".to_owned())
        );
        assert_eq!(taipei_date(i64::MAX), Some("292278994-08-17".to_owned()));
    }

    #[test]
    fn comment_datetime_uses_the_configured_taipei_timezone() {
        assert_eq!(
            taipei_datetime(1_754_582_509_710),
            Some("2025-08-08 00:01".to_owned())
        );
        assert_eq!(taipei_datetime(i64::MAX), None);
    }

    #[test]
    fn comment_preview_is_bounded_without_splitting_utf8() {
        assert_eq!(
            comment_preview("Short", COMMENT_LIST_PREVIEW_BYTES),
            ("Short".to_owned(), false)
        );
        let text = "界".repeat(300);
        let (preview, truncated) = comment_preview(&text, COMMENT_LIST_PREVIEW_BYTES);
        assert!(truncated);
        assert!(preview.ends_with("..."));
        assert!(preview.len() <= COMMENT_LIST_PREVIEW_BYTES + 3);
        assert!(str::from_utf8(preview.as_bytes()).is_ok());
    }

    #[test]
    fn comment_detail_pages_preserve_full_utf8_text() {
        let text = format!("{} end", "界".repeat(300));
        let mut rebuilt = String::new();
        let mut page = 0;
        while let Some((part, total_pages)) = comment_detail_page(&text, page) {
            assert!(total_pages > 1);
            assert!(part.len() <= COMMENT_DETAIL_BYTES);
            rebuilt.push_str(part);
            page += 1;
        }
        assert_eq!(rebuilt, text);
    }

    #[test]
    fn account_history_classifies_future_expired_and_no_expiry_rows() {
        let future = expiration_row(
            model::AssetKind::Coin,
            model::AssetSubtype::Standard,
            1,
            Some(1_819_728_000_000),
        );
        let expired = expiration_row(
            model::AssetKind::Ticket,
            model::AssetSubtype::Bonus,
            2,
            Some(1_735_660_800_000),
        );
        let far_future = expiration_row(
            model::AssetKind::Coin,
            model::AssetSubtype::Bonus,
            4,
            Some(253_402_300_800_000),
        );
        let no_expiry =
            expiration_row(model::AssetKind::Ticket, model::AssetSubtype::Free, 3, None);
        let now = Some(1_735_660_800_000);

        assert_eq!(
            Bomtoon::history_row_label_at(&future, now),
            "Coin · Standard · 1 · Expires 2027-09-01"
        );
        assert_eq!(
            Bomtoon::history_row_label_at(&far_future, now),
            "Coin · Bonus · 4 · Expires 10000-01-01"
        );
        assert_eq!(
            Bomtoon::history_row_label_at(&expired, now),
            "Ticket · Bonus · 2 · Expired 2025-01-01"
        );
        assert_eq!(
            Bomtoon::history_row_label_at(&no_expiry, now),
            "Ticket · Free · 3 · No expiry"
        );
    }

    #[test]
    fn account_clock_failure_keeps_cached_data_and_marks_both_histories_unavailable() {
        let summary_task = TaskId(40);
        let coin_task = TaskId(41);
        let ticket_task = TaskId(42);
        let cached_coin = expiration_row(
            model::AssetKind::Coin,
            model::AssetSubtype::Standard,
            5,
            None,
        );
        let cached_ticket = expiration_row(
            model::AssetKind::Ticket,
            model::AssetSubtype::Bonus,
            2,
            None,
        );
        let mut app = Bomtoon {
            wallet: WalletState {
                summary: Some(test_wallet_summary()),
                summary_task: Some(summary_task),
                detail_generation: 8,
                tasks: BTreeMap::from([
                    (summary_task, WalletTaskPurpose::Summary { generation: 3 }),
                    (coin_task, WalletTaskPurpose::CoinHistory { generation: 8 }),
                    (
                        ticket_task,
                        WalletTaskPurpose::TicketHistory { generation: 8 },
                    ),
                ]),
                coin_history: vec![cached_coin.clone()],
                ticket_history: vec![cached_ticket.clone()],
                ..WalletState::default()
            },
            ..Bomtoon::default()
        };
        let mut context = Context::default();

        app.refresh_account_details_from(&mut context, None);

        assert_eq!(app.wallet.detail_generation, 9);
        assert_eq!(app.wallet.summary, Some(test_wallet_summary()));
        assert_eq!(app.wallet.summary_task, Some(summary_task));
        assert_eq!(
            app.wallet.tasks,
            BTreeMap::from([(summary_task, WalletTaskPurpose::Summary { generation: 3 })])
        );
        assert_eq!(app.wallet.coin_history, [cached_coin]);
        assert_eq!(app.wallet.ticket_history, [cached_ticket]);
        assert!(app.wallet.coin_history_error);
        assert!(app.wallet.ticket_history_error);
    }

    #[test]
    fn account_open_renders_cached_balances_and_starts_exact_requests() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        let before = current_time_ms()
            .checked_sub(HISTORY_WINDOW_MS)
            .expect("history start");

        let library = runner.app().screen();
        let actions = &library.top_bar.expect("Library top bar").actions;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action, action_id(ACCOUNT));
        assert_eq!(actions[0].label, "Coins 10");
        let commands = runner.action(action_id(ACCOUNT));
        let after = current_time_ms()
            .checked_sub(HISTORY_WINDOW_MS)
            .expect("history start");

        assert_eq!(runner.app().view, View::Account);
        let spawned = spawns(&commands);
        assert_eq!(spawned.len(), 3);
        assert!(spawned
            .iter()
            .any(|(_, work)| *work == api::asset_summary()));
        let (_, coin_work) = fetch_task_with(&commands, "coinKind=COIN");
        let (_, ticket_work) = fetch_task_with(&commands, "coinKind=TICKET");
        let start = expiration_request_start(&coin_work);
        assert!((before..=after).contains(&start));
        assert_eq!(
            coin_work,
            api::expiration_history(model::AssetKind::Coin, start)
        );
        assert_eq!(
            ticket_work,
            api::expiration_history(model::AssetKind::Ticket, start)
        );

        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        for expected in [
            "Account",
            "Coins",
            "10",
            "Standard coins",
            "7",
            "Bonus coins",
            "2",
            "Free coins",
            "1",
            "Tickets",
            "4",
            "Standard tickets",
            "3",
            "Bonus tickets",
            "Free tickets",
            "Coin history",
            "Ticket history",
            "Loading…",
        ] {
            assert!(drawn.contains(expected), "missing {expected}: {drawn}");
        }
        assert!(screen.owns_back);
        assert_fits(&screen);
    }

    #[test]
    fn account_stale_detail_generation_cannot_replace_cached_rows() {
        let cached = expiration_row(
            model::AssetKind::Coin,
            model::AssetSubtype::Standard,
            20,
            Some(1_819_728_000_000),
        );
        let stale = expiration_row(
            model::AssetKind::Coin,
            model::AssetSubtype::Standard,
            10,
            None,
        );
        let current = expiration_row(
            model::AssetKind::Ticket,
            model::AssetSubtype::Bonus,
            3,
            None,
        );
        let mut wallet = WalletState {
            detail_generation: 2,
            coin_history: vec![cached.clone()],
            ..WalletState::default()
        };

        assert!(!wallet.accept_history(1, model::AssetKind::Coin, vec![stale]));
        assert_eq!(wallet.coin_history, [cached]);
        assert!(wallet.accept_history(2, model::AssetKind::Ticket, vec![current.clone()]));
        assert_eq!(wallet.ticket_history, [current]);
    }

    #[test]
    fn account_partial_history_failure_keeps_success_and_retries_only_failure() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        let commands = runner.action(action_id(ACCOUNT));
        let (summary_task, _) = fetch_task_with(&commands, "/asset/user");
        let (coin_task, _) = fetch_task_with(&commands, "coinKind=COIN");
        let (ticket_task, _) = fetch_task_with(&commands, "coinKind=TICKET");

        runner.task_outcome(
            coin_task,
            TaskOutcome::Completed(COIN_HISTORY_RESPONSE.to_vec()),
        );
        let commands = runner.task_outcome(ticket_task, TaskOutcome::Failed(TaskError::Offline));

        assert_eq!(runner.app().wallet.coin_history.len(), 1);
        assert!(runner.app().wallet.ticket_history.is_empty());
        assert!(!runner.app().wallet.coin_history_error);
        assert!(runner.app().wallet.ticket_history_error);
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(drawn.contains("Coin · Standard · 5 · Expires 2027-09-01"));
        assert!(drawn.contains("Ticket history"));
        assert!(drawn.contains("Unavailable"));
        assert!(drawn.contains("Retry balances"));
        assert_fits(&screen);

        runner.task_outcome(
            summary_task,
            TaskOutcome::Completed(ASSET_RESPONSE.to_vec()),
        );
        let commands = runner.action(action_id(RETRY_BALANCES));
        let spawned = spawns(&commands);
        assert_eq!(spawned.len(), 2);
        assert!(spawned
            .iter()
            .any(|(_, work)| *work == api::asset_summary()));
        assert!(spawned.iter().any(
            |(_, work)| matches!(work, Task::Fetch { url, .. } if url.contains("coinKind=TICKET"))
        ));
        assert!(spawned.iter().all(
            |(_, work)| !matches!(work, Task::Fetch { url, .. } if url.contains("coinKind=COIN"))
        ));
        assert_eq!(runner.app().wallet.coin_history.len(), 1);
    }

    #[test]
    fn account_history_is_combined_formatted_and_bounded_by_page() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        let app = runner.app_mut();
        app.view = View::Account;
        app.page = 0;
        app.wallet.coin_history = vec![
            expiration_row(
                model::AssetKind::Coin,
                model::AssetSubtype::Standard,
                101,
                Some(1_819_728_000_000),
            ),
            expiration_row(
                model::AssetKind::Coin,
                model::AssetSubtype::Bonus,
                102,
                None,
            ),
            expiration_row(
                model::AssetKind::Coin,
                model::AssetSubtype::Free,
                103,
                Some(1_819_728_000_000),
            ),
            expiration_row(
                model::AssetKind::Coin,
                model::AssetSubtype::Standard,
                104,
                None,
            ),
        ];
        app.wallet.ticket_history = vec![
            expiration_row(
                model::AssetKind::Ticket,
                model::AssetSubtype::Standard,
                201,
                Some(1_819_728_000_000),
            ),
            expiration_row(
                model::AssetKind::Ticket,
                model::AssetSubtype::Free,
                202,
                None,
            ),
        ];

        let screen = runner.app().screen();
        let first = format!("{screen:?}");
        assert!(first.contains("Coin · Standard · 101 · Expires 2027-09-01"));
        assert!(first.contains("Coin · Bonus · 102 · No expiry"));
        assert!(first.contains("Coin · Free · 103 · Expires 2027-09-01"));
        assert!(!first.contains("Coin · Standard · 104 · No expiry"));
        let turns = screen.page_turns.as_ref().expect("Account page turns");
        assert_eq!(turns.previous, action_id(PREVIOUS_PAGE));
        assert_eq!(turns.next, action_id(NEXT_PAGE));
        assert!(screen.nodes.iter().all(|node| {
            !matches!(
                node,
                Node::Button { action, .. }
                    if *action == action_id(PREVIOUS_PAGE)
                        || *action == action_id(NEXT_PAGE)
            )
        }));
        assert_fits(&screen);

        let commands = runner.action(action_id(NEXT_PAGE));
        let screen = last_screen(&commands);
        let second = format!("{screen:?}");
        assert!(!second.contains("Coin · Standard · 101 · Expires 2027-09-01"));
        assert!(second.contains("Coin · Standard · 104 · No expiry"));
        assert!(second.contains("Ticket · Standard · 201 · Expires 2027-09-01"));
        assert!(second.contains("Ticket · Free · 202 · No expiry"));
        assert_eq!(
            screen.page_turns.as_ref().and_then(|turns| turns.position),
            Some((2, 2))
        );
        assert_fits(&screen);

        let commands = runner.action(action_id(PREVIOUS_PAGE));
        assert_eq!(runner.app().page, 0);
        assert!(format!("{:?}", last_screen(&commands))
            .contains("Coin · Standard · 101 · Expires 2027-09-01"));
    }

    #[test]
    fn account_reentry_after_back_starts_fresh_histories_in_server_order() {
        let (mut runner, _) = loaded_library();
        let first_open = runner.action(action_id(ACCOUNT));
        let (old_coin, _) = fetch_task_with(&first_open, "coinKind=COIN");
        let (old_ticket, _) = fetch_task_with(&first_open, "coinKind=TICKET");

        let back = runner.action(ActionId::BACK);
        assert!(back.contains(&Command::Cancel(old_coin)));
        assert!(back.contains(&Command::Cancel(old_ticket)));
        runner.task_outcome(old_coin, TaskOutcome::Cancelled);
        runner.task_outcome(old_ticket, TaskOutcome::Cancelled);

        let second_open = runner.action(action_id(ACCOUNT));
        let (current_coin, _) = fetch_task_with(&second_open, "coinKind=COIN");
        let (current_ticket, work) = fetch_task_with(&second_open, "coinKind=TICKET");
        assert!(matches!(
            work,
            Task::Fetch { ref url, .. } if url.contains("coinKind=TICKET")
        ));
        assert_ne!(current_coin, current_ticket);
        assert!(!runner.app().wallet.coin_history_error);
        assert!(!runner.app().wallet.ticket_history_error);

        runner.task_outcome(
            current_ticket,
            TaskOutcome::Completed(MULTI_TICKET_HISTORY_RESPONSE.to_vec()),
        );
        assert_eq!(
            runner
                .app()
                .wallet
                .ticket_history
                .iter()
                .map(|row| row.quantity)
                .collect::<Vec<_>>(),
            vec![11, 12, 21, 22]
        );
    }

    #[test]
    fn account_open_uses_capacity_left_by_cancelled_reader_work() {
        let (mut runner, image_task) = reader_waiting_for_first_image();
        let commands = runner.action(ActionId::BACK);
        assert!(commands.contains(&Command::Cancel(image_task)));
        let gift = fetch_task_with(&commands, "/gift/contents/detail?").0;
        runner.task_outcome(image_task, TaskOutcome::Cancelled);
        runner.task_outcome(
            gift,
            TaskOutcome::Completed(
                br#"{"result":"SUCCESS","data":{"receivedGifts":[],"receivableGifts":[]}}"#
                    .to_vec(),
            ),
        );
        runner.action(ActionId::BACK);

        let commands = runner.action(action_id(ACCOUNT));
        assert_eq!(runner.app().view, View::Account);
        fetch_task_with(&commands, "coinKind=COIN");
        fetch_task_with(&commands, "coinKind=TICKET");
        assert!(!runner.app().wallet.coin_history_error);
        assert!(!runner.app().wallet.ticket_history_error);
    }

    #[test]
    fn account_reentry_refreshes_details_and_settled_screen_does_not_poll() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        let commands = runner.action(action_id(ACCOUNT));
        let (summary_task, _) = fetch_task_with(&commands, "/asset/user");
        let (coin_task, _) = fetch_task_with(&commands, "coinKind=COIN");
        let (ticket_task, _) = fetch_task_with(&commands, "coinKind=TICKET");
        runner.task_outcome(
            summary_task,
            TaskOutcome::Completed(ASSET_RESPONSE.to_vec()),
        );
        runner.task_outcome(
            coin_task,
            TaskOutcome::Completed(EMPTY_HISTORY_RESPONSE.to_vec()),
        );
        let commands = runner.task_outcome(
            ticket_task,
            TaskOutcome::Completed(EMPTY_HISTORY_RESPONSE.to_vec()),
        );
        let drawn = format!("{:?}", last_screen(&commands));
        assert!(drawn.contains("Coin history"));
        assert!(drawn.contains("Ticket history"));
        assert!(drawn.contains("No coin expiration records"));
        assert!(drawn.contains("No ticket expiration records"));

        let commands = runner.action(action_id("account-redraw"));
        assert!(spawns(&commands).is_empty(), "Account must not poll");
        runner.action(ActionId::BACK);
        let commands = runner.action(action_id(ACCOUNT));
        assert_eq!(spawns(&commands).len(), 3);
        assert!(fetch_task_with(&commands, "/asset/user").1 == api::asset_summary());
        fetch_task_with(&commands, "coinKind=COIN");
        fetch_task_with(&commands, "coinKind=TICKET");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one layout matrix keeps every Account boundary state under the same metrics"
    )]
    fn account_stale_summary_and_all_boundary_layout_states_fit_clara() {
        let maximum = Bomtoon {
            view: View::Account,
            wallet: WalletState {
                summary: Some(WalletSummary {
                    coins: model::AssetAmounts {
                        standard: usize::MAX,
                        bonus: usize::MAX,
                        free: usize::MAX,
                    },
                    tickets: model::AssetAmounts {
                        standard: usize::MAX,
                        bonus: usize::MAX,
                        free: usize::MAX,
                    },
                }),
                summary_stale: true,
                ..WalletState::default()
            },
            ..Bomtoon::default()
        }
        .screen();
        let maximum_drawn = format!("{maximum:?}");
        assert!(maximum_drawn.contains(&usize::MAX.to_string()));
        assert!(maximum_drawn.contains("Balances may be out of date."));

        let unavailable = Bomtoon {
            view: View::Account,
            wallet: WalletState {
                summary_error: true,
                coin_history_error: true,
                ticket_history_error: true,
                ..WalletState::default()
            },
            ..Bomtoon::default()
        }
        .screen();
        let unavailable_drawn = format!("{unavailable:?}");
        assert!(unavailable_drawn.contains("Balances unavailable."));
        assert!(unavailable_drawn.matches("Unavailable").count() >= 2);
        assert!(unavailable_drawn.contains("Retry balances"));

        let empty = Bomtoon {
            view: View::Account,
            wallet: WalletState {
                summary: Some(test_wallet_summary()),
                ..WalletState::default()
            },
            ..Bomtoon::default()
        }
        .screen();
        let empty_drawn = format!("{empty:?}");
        assert!(empty_drawn.contains("No coin expiration records"));
        assert!(empty_drawn.contains("No ticket expiration records"));

        let loading_generation = 7;
        let loading = Bomtoon {
            view: View::Account,
            wallet: WalletState {
                summary_task: Some(TaskId(10)),
                detail_generation: loading_generation,
                tasks: BTreeMap::from([
                    (
                        TaskId(11),
                        WalletTaskPurpose::CoinHistory {
                            generation: loading_generation,
                        },
                    ),
                    (
                        TaskId(12),
                        WalletTaskPurpose::TicketHistory {
                            generation: loading_generation,
                        },
                    ),
                ]),
                ..WalletState::default()
            },
            ..Bomtoon::default()
        }
        .screen();
        assert!(format!("{loading:?}").matches("Loading…").count() >= 3);

        let full = Bomtoon {
            view: View::Account,
            wallet: WalletState {
                summary: Some(test_wallet_summary()),
                coin_history: (1..=4)
                    .map(|quantity| {
                        expiration_row(
                            model::AssetKind::Coin,
                            model::AssetSubtype::Standard,
                            quantity,
                            Some(1_819_728_000_000),
                        )
                    })
                    .collect(),
                ticket_history: (5..=6)
                    .map(|quantity| {
                        expiration_row(
                            model::AssetKind::Ticket,
                            model::AssetSubtype::Bonus,
                            quantity,
                            None,
                        )
                    })
                    .collect(),
                ..WalletState::default()
            },
            ..Bomtoon::default()
        }
        .screen();
        assert_eq!(
            full.page_turns.as_ref().and_then(|turns| turns.position),
            Some((1, 2))
        );

        for screen in [maximum, unavailable, empty, loading, full] {
            assert_fits(&screen);
        }
    }

    struct SignOutScenario {
        public_url: String,
        square_url: String,
        shared_url: String,
        shared_loading_url: String,
        protected_url: String,
        public_cover_task: TaskId,
        protected_cover_task: TaskId,
        public_homepage_task: TaskId,
        wallet_task: TaskId,
        reader_task: TaskId,
        shared_cover_task: TaskId,
        shared_picture: TilePicture,
        square_picture: TilePicture,
        protected_picture: TilePicture,
    }

    fn sign_out_scenario() -> SignOutScenario {
        SignOutScenario {
            public_url: "https://image.balcony.studio/tw/contents/public.webp".to_owned(),
            square_url: "https://image.balcony.studio/tw/contents/public-square.webp".to_owned(),
            shared_url: "https://image.balcony.studio/tw/contents/shared.webp".to_owned(),
            shared_loading_url: "https://image.balcony.studio/tw/contents/shared-loading.webp"
                .to_owned(),
            protected_url: "https://image.balcony.studio/tw/contents/protected.webp".to_owned(),
            public_cover_task: TaskId(90),
            protected_cover_task: TaskId(91),
            public_homepage_task: TaskId(92),
            wallet_task: TaskId(93),
            reader_task: TaskId(94),
            shared_cover_task: TaskId(95),
            shared_picture: TilePicture::new(PictureHandle(81), 10, 20),
            square_picture: TilePicture::new(PictureHandle(83), 20, 20),
            protected_picture: TilePicture::new(PictureHandle(82), 10, 20),
        }
    }

    fn seed_sign_out_public_data(app: &mut Bomtoon, scenario: &SignOutScenario) {
        let banners = vec![
            FeatureComic {
                title: "Public".to_owned(),
                alias: "public".to_owned(),
                creators: String::new(),
                view_count: None,
                vertical_url: Some(scenario.public_url.clone()),
                square_url: None,
            },
            FeatureComic {
                title: "Shared".to_owned(),
                alias: "shared".to_owned(),
                creators: String::new(),
                view_count: None,
                vertical_url: Some(scenario.shared_url.clone()),
                square_url: None,
            },
        ];
        let collection = model::FeatureCollection {
            id: "newest".to_owned(),
            label: "人氣新作".to_owned(),
            priority: 2,
            order: 0,
            comics: vec![
                FeatureComic {
                    title: "Recommended".to_owned(),
                    alias: "recommended".to_owned(),
                    creators: String::new(),
                    view_count: None,
                    vertical_url: Some(scenario.shared_url.clone()),
                    square_url: Some(scenario.square_url.clone()),
                },
                FeatureComic {
                    title: "Shared loading".to_owned(),
                    alias: "shared-loading".to_owned(),
                    creators: String::new(),
                    view_count: None,
                    vertical_url: Some(scenario.shared_loading_url.clone()),
                    square_url: None,
                },
            ],
        };
        let mut featured = FeaturedState {
            generation: 6,
            snapshot: Some(FeatureSnapshot {
                banners,
                collections: vec![collection.clone()],
                sources: BTreeMap::from([(FeatureSource::Homepage, vec![collection])]),
                failed_sources: BTreeSet::new(),
                warning: None,
            }),
            feed_page: 2,
            loaded_day: Some(local_day(30)),
            desired_day: Some(local_day(31)),
            local_day_pending: true,
            ..FeaturedState::default()
        };
        featured.begin_full_batch(Some(local_day(30)));
        featured.mark_source_pending(featured.generation, FeatureSource::Homepage);
        featured.desired_day = Some(local_day(31));
        app.featured = featured;
    }

    fn seed_sign_out_protected_data(app: &mut Bomtoon, scenario: &SignOutScenario) {
        app.comics = vec![Comic {
            alias: "protected".to_owned(),
            title: "Protected".to_owned(),
            creators: String::new(),
            cover_url: Some(scenario.protected_url.clone()),
            owned_episodes: 1,
            total_episodes: 2,
        }];
        app.recent = vec![RecentEntry {
            content_alias: "shared".to_owned(),
            content_title: "Shared protected placement".to_owned(),
            creators: String::new(),
            cover_url: Some(scenario.shared_loading_url.clone()),
            episode_alias: "ep-1".to_owned(),
            episode_title: "Episode One".to_owned(),
        }];
        app.episodes = vec![Episode {
            id: 101,
            alias: "ep-1".to_owned(),
            title: "Episode One".to_owned(),
            opened_at: 1_709_136_000_000,
            thumbnail_url: None,
            purchase: model::PurchaseState::Owned,
            rent_expires_at: None,
            rent_coin: None,
            purchase_coin: None,
            gift_eligible: false,
        }];
        app.library_load.loaded = true;
        app.recent_load.loaded = true;
        app.selected_content_alias = "protected".to_owned();
        app.selected_title = "Protected".to_owned();
        app.selected_creators = "Protected Creator".to_owned();
        app.selected_synopsis = "Protected synopsis".to_owned();
        app.wallet = WalletState {
            summary: Some(test_wallet_summary()),
            summary_task: Some(scenario.wallet_task),
            tasks: BTreeMap::from([(
                scenario.wallet_task,
                WalletTaskPurpose::Summary { generation: 1 },
            )]),
            coin_history: vec![expiration_row(
                AssetKind::Coin,
                AssetSubtype::Standard,
                1,
                None,
            )],
            ticket_history: vec![expiration_row(
                AssetKind::Ticket,
                AssetSubtype::Bonus,
                1,
                None,
            )],
            summary_generation: 1,
            ..WalletState::default()
        };
        app.reader_tasks.insert(
            scenario.reader_task,
            ReaderTaskEntry {
                generation: app.reader_generation,
                purpose: ReaderTaskPurpose::Maintenance,
            },
        );
    }

    fn seed_sign_out_cover_cache(app: &mut Bomtoon, scenario: &SignOutScenario) {
        app.covers.generation = 11;
        app.covers.visible_urls = vec![
            scenario.public_url.clone(),
            scenario.square_url.clone(),
            scenario.shared_loading_url.clone(),
            scenario.protected_url.clone(),
        ];
        app.covers.entries = BTreeMap::from([
            (
                scenario.public_url.clone(),
                CoverState::Loading(scenario.public_cover_task),
            ),
            (
                scenario.shared_url.clone(),
                CoverState::Ready(scenario.shared_picture),
            ),
            (
                scenario.square_url.clone(),
                CoverState::Ready(scenario.square_picture),
            ),
            (
                scenario.shared_loading_url.clone(),
                CoverState::Loading(scenario.shared_cover_task),
            ),
            (
                scenario.protected_url.clone(),
                CoverState::Loading(scenario.protected_cover_task),
            ),
            (
                "https://image.balcony.studio/tw/contents/protected-ready.webp".to_owned(),
                CoverState::Ready(scenario.protected_picture),
            ),
        ]);
        app.covers.tasks = BTreeMap::from([
            (
                scenario.public_cover_task,
                CoverTask {
                    generation: 11,
                    url: scenario.public_url.clone(),
                    source: CoverSource::Public,
                },
            ),
            (
                scenario.shared_cover_task,
                CoverTask {
                    generation: 11,
                    url: scenario.shared_loading_url.clone(),
                    source: CoverSource::Protected,
                },
            ),
            (
                scenario.protected_cover_task,
                CoverTask {
                    generation: 11,
                    url: scenario.protected_url.clone(),
                    source: CoverSource::Protected,
                },
            ),
        ]);
    }

    fn sign_out_runner(scenario: &SignOutScenario) -> AppRunner<Bomtoon> {
        let mut runner = seeded_reader(1, 0, false);
        let app = runner.app_mut();
        app.view = View::Main;
        app.destination = MainDestination::Library;
        app.page = 4;
        seed_sign_out_public_data(app, scenario);
        seed_sign_out_protected_data(app, scenario);
        app.feature_tasks.insert(
            scenario.public_homepage_task,
            FeatureTaskPurpose::Source {
                generation: app.featured.generation,
                source: FeatureSource::Homepage,
            },
        );
        seed_sign_out_cover_cache(app, scenario);
        runner
    }

    fn assert_signed_out_public_state(
        app: &Bomtoon,
        scenario: &SignOutScenario,
        featured: &FeaturedState,
    ) {
        assert_eq!(app.account, AccountState::SignedOut);
        assert_eq!(app.view, View::Main);
        assert_eq!(app.destination, MainDestination::Featured);
        assert_eq!(app.page, 0);
        assert_eq!(app.featured.feed_page, 0);
        assert_eq!(app.featured.snapshot, featured.snapshot);
        assert_eq!(app.featured.generation, featured.generation);
        assert_eq!(app.featured.loaded_day, featured.loaded_day);
        assert_eq!(app.featured.desired_day, featured.desired_day);
        assert_eq!(
            app.covers.entries.get(&scenario.shared_url),
            Some(&CoverState::Ready(scenario.shared_picture))
        );
        assert_eq!(
            app.covers.entries.get(&scenario.square_url),
            Some(&CoverState::Ready(scenario.square_picture))
        );
        assert_eq!(
            app.covers.entries.get(&scenario.public_url),
            Some(&CoverState::Loading(scenario.public_cover_task))
        );
        assert_eq!(
            app.covers.entries.get(&scenario.shared_loading_url),
            Some(&CoverState::Loading(scenario.shared_cover_task))
        );
        assert!(app
            .feature_tasks
            .contains_key(&scenario.public_homepage_task));
        assert!(app.covers.tasks.contains_key(&scenario.public_cover_task));
        assert_eq!(
            app.covers
                .tasks
                .get(&scenario.shared_cover_task)
                .map(|task| task.source),
            Some(CoverSource::Public)
        );
    }

    fn assert_signed_out_protected_state_cleared(app: &Bomtoon, scenario: &SignOutScenario) {
        assert!(!app.covers.entries.contains_key(&scenario.protected_url));
        assert!(app.comics.is_empty());
        assert!(app.recent.is_empty());
        assert!(app.episodes.is_empty());
        assert!(app.selected_content_alias.is_empty());
        assert!(app.selected_title.is_empty());
        assert!(app.selected_creators.is_empty());
        assert!(app.selected_synopsis.is_empty());
        assert!(app.reader_selection.is_none());
        assert!(app.reader.is_none());
        assert!(app.reader_tasks.is_empty());
        assert!(app.wallet.summary.is_none());
        assert!(app.wallet.coin_history.is_empty());
        assert!(app.wallet.ticket_history.is_empty());
        assert!(app.wallet.tasks.is_empty());
        assert!(!app
            .covers
            .tasks
            .contains_key(&scenario.protected_cover_task));
    }

    fn assert_sign_out_cleanup_commands(commands: &[Command], scenario: &SignOutScenario) {
        assert!(commands.contains(&Command::Cancel(scenario.protected_cover_task)));
        assert!(commands.contains(&Command::Cancel(scenario.wallet_task)));
        assert!(commands.contains(&Command::Cancel(scenario.reader_task)));
        assert!(!commands.contains(&Command::Cancel(scenario.public_homepage_task)));
        assert!(!commands.contains(&Command::Cancel(scenario.public_cover_task)));
        assert!(!commands.contains(&Command::Cancel(scenario.shared_cover_task)));
        assert!(commands.contains(&Command::DropPicture(PictureHandle(7))));
        assert!(commands.contains(&Command::DropPicture(scenario.protected_picture.handle)));
        assert!(!commands.contains(&Command::DropPicture(scenario.shared_picture.handle)));
        assert!(!commands.contains(&Command::DropPicture(scenario.square_picture.handle)));
    }

    fn assert_protected_destinations_require_sign_in(runner: &mut AppRunner<Bomtoon>) {
        for action in [RECENT, LIBRARY] {
            runner.app_mut().view = View::Main;
            runner.app_mut().destination = MainDestination::Featured;
            let commands = runner.action(action_id(action));
            assert_eq!(runner.app().view, View::Status);
            assert!(spawns(&commands).is_empty());
        }
    }

    struct FullExitScenario {
        pending_task: TaskId,
        shelf_task: TaskId,
        public_cover_task: TaskId,
        protected_cover_task: TaskId,
        wallet_task: TaskId,
        reader_task: TaskId,
        public_url: String,
        protected_url: String,
        ready_url: String,
        ready_picture: TilePicture,
    }

    fn full_exit_scenario() -> FullExitScenario {
        FullExitScenario {
            pending_task: TaskId(70),
            shelf_task: TaskId(71),
            public_cover_task: TaskId(72),
            protected_cover_task: TaskId(73),
            wallet_task: TaskId(74),
            reader_task: TaskId(75),
            public_url: "https://image.balcony.studio/tw/contents/public-exit.webp".to_owned(),
            protected_url: "https://image.balcony.studio/tw/contents/protected-exit.webp"
                .to_owned(),
            ready_url: "https://image.balcony.studio/tw/contents/ready-exit.webp".to_owned(),
            ready_picture: TilePicture::new(PictureHandle(83), 10, 20),
        }
    }

    fn full_exit_cover_cache(scenario: &FullExitScenario) -> CoverCache {
        CoverCache {
            generation: 6,
            entries: BTreeMap::from([
                (
                    scenario.public_url.clone(),
                    CoverState::Loading(scenario.public_cover_task),
                ),
                (
                    scenario.protected_url.clone(),
                    CoverState::Loading(scenario.protected_cover_task),
                ),
                (
                    scenario.ready_url.clone(),
                    CoverState::Ready(scenario.ready_picture),
                ),
            ]),
            tasks: BTreeMap::from([
                (
                    scenario.public_cover_task,
                    CoverTask {
                        generation: 6,
                        url: scenario.public_url.clone(),
                        source: CoverSource::Public,
                    },
                ),
                (
                    scenario.protected_cover_task,
                    CoverTask {
                        generation: 6,
                        url: scenario.protected_url.clone(),
                        source: CoverSource::Protected,
                    },
                ),
            ]),
            ..CoverCache::default()
        }
    }

    fn full_exit_runner(scenario: &FullExitScenario) -> AppRunner<Bomtoon> {
        let collection = model::FeatureCollection {
            id: "newest".to_owned(),
            label: "人氣新作".to_owned(),
            priority: 2,
            order: 0,
            comics: vec![FeatureComic {
                title: "Ready".to_owned(),
                alias: "ready".to_owned(),
                creators: String::new(),
                view_count: None,
                vertical_url: Some(scenario.ready_url.clone()),
                square_url: None,
            }],
        };
        let mut featured = FeaturedState {
            generation: 3,
            snapshot: Some(FeatureSnapshot {
                banners: Vec::new(),
                collections: vec![collection.clone()],
                sources: BTreeMap::from([(FeatureSource::Homepage, vec![collection])]),
                failed_sources: BTreeSet::new(),
                warning: None,
            }),
            desired_day: Some(local_day(31)),
            local_day_pending: true,
            ..FeaturedState::default()
        };
        featured.begin_full_batch(Some(local_day(30)));
        featured.mark_source_pending(featured.generation, FeatureSource::Homepage);
        AppRunner::new(Bomtoon {
            pending: Some(Pending::Content(0)),
            task: Some(scenario.pending_task),
            featured,
            feature_tasks: BTreeMap::from([(
                scenario.shelf_task,
                FeatureTaskPurpose::Source {
                    generation: 4,
                    source: FeatureSource::Homepage,
                },
            )]),
            covers: full_exit_cover_cache(scenario),
            wallet: WalletState {
                summary_task: Some(scenario.wallet_task),
                tasks: BTreeMap::from([(
                    scenario.wallet_task,
                    WalletTaskPurpose::Summary { generation: 1 },
                )]),
                ..WalletState::default()
            },
            reader_tasks: BTreeMap::from([(
                scenario.reader_task,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: ReaderTaskPurpose::Manifest,
                },
            )]),
            reader_generation: 1,
            ..Bomtoon::default()
        })
    }

    fn assert_full_exit_cleanup(
        runner: &AppRunner<Bomtoon>,
        commands: &[Command],
        scenario: &FullExitScenario,
    ) {
        assert!(commands.contains(&Command::DropPicture(scenario.ready_picture.handle)));
        assert!(runner.app().feature_tasks.is_empty());
        assert!(runner.app().covers.tasks.is_empty());
        assert!(runner.app().covers.entries.is_empty());
        assert!(runner.app().wallet.tasks.is_empty());
        assert!(runner.app().reader_tasks.is_empty());
        assert!(!runner.app().featured.local_day_pending);
        assert_eq!(runner.app().featured.batch, None);
        assert_eq!(runner.app().featured.desired_day, None);
        assert_eq!(runner.app().featured.snapshot, None);
    }

    #[test]
    fn scope_credential_failure_keeps_public_feature_loading_and_ignores_source_outcome() {
        let (mut runner, commands) = started();
        let homepage_task = fetch_task_with(&commands, "/comic/main").0;
        let scope = scope_task(&commands);
        runner.task_outcome(scope, TaskOutcome::Failed(TaskError::NoCredential));
        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);

        runner.task_outcome(
            homepage_task,
            TaskOutcome::Completed(b"<html></html>".to_vec()),
        );
        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_eq!(runner.app().view, View::Main);
        assert!(runner.app().comics.is_empty());
    }

    #[test]
    fn sign_out_retains_both_public_artwork_urls_and_tasks_while_clearing_protected_state() {
        let scenario = sign_out_scenario();
        let mut runner = sign_out_runner(&scenario);
        let public_featured = runner.app().featured.clone();
        let public_urls = runner.app().public_cover_urls();
        assert!(public_urls.contains(&scenario.shared_url));
        assert!(public_urls.contains(&scenario.square_url));

        let account_commands = runner.action(action_id(ACCOUNT));
        assert_eq!(runner.app().view, View::Account);
        for task in [
            scenario.public_cover_task,
            scenario.shared_cover_task,
            scenario.protected_cover_task,
        ] {
            assert!(!account_commands.contains(&Command::Cancel(task)));
            assert!(runner.app().covers.tasks.contains_key(&task));
        }

        let commands = runner.action(action_id(SIGN_OUT));
        let (logout_task, work) = only_spawn(&commands);
        assert_eq!(
            work,
            Task::RevokeCredential {
                credential: "bomtoon-access-token".to_owned(),
            }
        );
        assert!(!commands.contains(&Command::Cancel(scenario.public_homepage_task)));
        assert!(
            !commands.contains(&Command::Cancel(scenario.public_cover_task)),
            "public cover cancellation leaked into sign-out: {commands:?}"
        );

        let commands = runner.task_outcome(logout_task, TaskOutcome::Completed(Vec::new()));
        assert_signed_out_public_state(runner.app(), &scenario, &public_featured);
        assert_signed_out_protected_state_cleared(runner.app(), &scenario);
        assert_sign_out_cleanup_commands(&commands, &scenario);
        assert_eq!(
            last_screen(&commands)
                .top_bar
                .expect("signed-out Featured top bar")
                .actions[0]
                .label,
            "Sign in"
        );
        assert_protected_destinations_require_sign_in(&mut runner);
    }

    #[test]
    fn full_exit_cancels_public_and_protected_work_and_drops_all_session_pictures() {
        let scenario = full_exit_scenario();
        let mut runner = full_exit_runner(&scenario);

        let commands = runner.exit();
        assert_eq!(
            cancelled_tasks(&commands),
            BTreeSet::from([
                scenario.pending_task,
                scenario.shelf_task,
                scenario.public_cover_task,
                scenario.protected_cover_task,
                scenario.wallet_task,
                scenario.reader_task,
            ])
        );
        assert_full_exit_cleanup(&runner, &commands, &scenario);
    }

    #[test]
    fn sign_out_clears_wallet_and_ignores_late_summary() {
        let (mut runner, _) = loaded_library();
        let summary_task = runner
            .app()
            .wallet
            .summary_task
            .expect("startup summary task");
        let logout_task = begin_logout(&mut runner);
        runner.task_outcome(logout_task, TaskOutcome::Completed(Vec::new()));
        assert!(runner.app().wallet.summary.is_none());
        assert!(runner.app().wallet.tasks.is_empty());

        runner.task_outcome(
            summary_task,
            TaskOutcome::Completed(ASSET_RESPONSE.to_vec()),
        );
        assert!(runner.app().wallet.summary.is_none());
    }

    #[test]
    fn startup_requests_public_homepage_without_a_credential() {
        let (_, commands) = started();
        let (_, work) = fetch_task_with(&commands, "/comic/main");
        let Task::Fetch {
            url, credential, ..
        } = work
        else {
            panic!("startup did not request the homepage");
        };
        assert_eq!(url, "https://www.bomtoon.tw/comic/main");
        assert_eq!(credential, None);
    }

    #[test]
    fn missing_credentials_leave_featured_available_and_sign_in_retries_cleanly() {
        let (mut runner, commands) = started();
        let scope = scope_task(&commands);
        runner.task_outcome(scope, TaskOutcome::Failed(TaskError::NoCredential));

        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_eq!(runner.app().view, View::Main);
        assert!(format!("{:?}", runner.app().screen()).contains("Sign in"));

        let commands = runner.action(action_id(SIGN_IN));
        assert_login_instructions(&last_screen(&commands));
        let old_feature_tasks = runner
            .app()
            .feature_tasks
            .keys()
            .copied()
            .collect::<Vec<_>>();

        let commands = runner.action(action_id(RETRY));
        assert!(old_feature_tasks
            .iter()
            .all(|task| commands.contains(&Command::Cancel(*task))));
        assert!(!commands.iter().any(is_homepage_fetch));

        let commands = runner.task_outcome(old_feature_tasks[0], TaskOutcome::Cancelled);
        let scope = scope_task(&commands);
        for task in old_feature_tasks.into_iter().skip(1) {
            runner.task_outcome(task, TaskOutcome::Cancelled);
        }
        runner.task_outcome(
            scope,
            TaskOutcome::Completed(b"00112233445566778899aabbccddeeff".to_vec()),
        );
        assert_eq!(runner.app().account, AccountState::Active);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
    }

    #[test]
    fn sign_out_is_owned_only_by_account() {
        let (mut runner, _) = loaded_library();
        let main = runner.app().screen();
        assert!(main
            .top_bar
            .expect("Library top bar")
            .actions
            .iter()
            .all(|action| action.action != action_id(SIGN_OUT)));

        let commands = runner.action(action_id(SIGN_OUT));
        assert!(spawns(&commands).is_empty());
        assert_eq!(runner.app().pending, None);

        let commands = runner.action(action_id(ACCOUNT));
        let account = last_screen(&commands);
        assert!(account
            .top_bar
            .expect("Account top bar")
            .actions
            .iter()
            .any(|action| action.action == action_id(SIGN_OUT)));

        let _ = begin_logout(&mut runner);
        assert_eq!(runner.app().pending, Some(Pending::Logout));
        let commands = runner.action(action_id(SIGN_OUT));
        assert!(spawns(&commands).is_empty());
    }

    #[test]
    fn a_full_middle_library_page_fits_and_loads_remote_only_at_the_boundary() {
        let (mut runner, _) = loaded_library();
        for index in 1..REMOTE_LIBRARY_PAGE_SIZE {
            runner.app_mut().comics.push(Comic {
                alias: format!("comic-{index}"),
                title: format!("Comic {index}"),
                creators: String::new(),
                cover_url: None,
                owned_episodes: index,
                total_episodes: REMOTE_LIBRARY_PAGE_SIZE + 1,
            });
        }
        runner.app_mut().total_library_titles = REMOTE_LIBRARY_PAGE_SIZE + 1;
        runner.app_mut().next_library_page = Some(1);

        let commands = runner.action(action_id(NEXT_PAGE));
        assert_eq!(runner.app().page, 1);
        assert_eq!(runner.app().pending, None);
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert_eq!(
            screen.page_turns.as_ref().and_then(|turns| turns.position),
            Some((2, 6))
        );
        assert!(drawn.contains("Coins"), "missing coin action: {drawn}");
        assert_fits(&screen);

        for expected_page in 2..(REMOTE_LIBRARY_PAGE_SIZE / LIBRARY_ITEMS_PER_PAGE) {
            let commands = runner.action(action_id(NEXT_PAGE));
            assert_eq!(runner.app().page, expected_page);
            assert_eq!(runner.app().pending, None);
            assert!(
                commands
                    .iter()
                    .all(|command| !matches!(command, Command::Spawn { .. })),
                "remote page loaded before the local boundary"
            );
        }

        let commands = runner.action(action_id(NEXT_PAGE));
        let (task, work) = only_spawn(&commands);
        assert!(matches!(
            work,
            Task::Fetch { ref url, .. } if url.contains("page=1")
        ));
        assert_eq!(
            runner.app().page,
            REMOTE_LIBRARY_PAGE_SIZE / LIBRARY_ITEMS_PER_PAGE - 1
        );
        assert_eq!(runner.app().pending, Some(Pending::Library(1)));

        let commands = runner.task_outcome(
            task,
            TaskOutcome::Completed(REMOTE_LIBRARY_RESPONSE.to_vec()),
        );
        assert_eq!(
            runner.app().page,
            REMOTE_LIBRARY_PAGE_SIZE / LIBRARY_ITEMS_PER_PAGE
        );
        assert_eq!(
            page_bounds(
                runner.app().page,
                runner.app().comics.len(),
                LIBRARY_ITEMS_PER_PAGE,
            ),
            (30, 31)
        );
        let drawn = format!("{:?}", last_screen(&commands));
        assert!(
            drawn.contains("Remote First"),
            "first appended title is not visible: {drawn}"
        );
    }

    #[test]
    fn a_hidden_stale_retry_keeps_the_loaded_library() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id(RETRY));

        assert!(
            commands
                .iter()
                .all(|command| !matches!(command, Command::Spawn { .. })),
            "a hidden retry started work"
        );
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().comics.len(), 1);
        assert_eq!(runner.app().total_library_titles, 1);
        assert!(runner.app().library_load.loaded);
    }

    #[test]
    fn successful_logout_clears_every_account_collection() {
        let (mut runner, _) = loaded_library();
        seed_all_account_data(&mut runner);
        let task = begin_logout(&mut runner);
        let commands = runner.task_outcome(task, TaskOutcome::Completed(Vec::new()));

        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_all_account_data_cleared(runner.app());
        let screen = last_screen(&commands);
        assert_eq!(runner.app().view, View::Main);
        assert!(screen
            .top_bar
            .expect("signed-out Featured top bar")
            .actions
            .iter()
            .any(|action| action.action == action_id(SIGN_IN)));
    }

    #[test]
    fn unconfirmed_remote_logout_is_signed_out_with_a_warning() {
        let (mut runner, _) = loaded_library();
        seed_all_account_data(&mut runner);
        let task = begin_logout(&mut runner);
        let commands =
            runner.task_outcome(task, TaskOutcome::Failed(TaskError::RevocationUnconfirmed));

        assert_eq!(runner.app().account, AccountState::RevocationUnconfirmed);
        assert_all_account_data_cleared(runner.app());
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(
            drawn.contains("Signed out here, but BOMTOON did not confirm remote sign-out."),
            "missing remote-revocation warning: {drawn}"
        );
        assert_login_instructions(&screen);
    }

    #[test]
    fn local_storage_logout_failure_keeps_loaded_account_data() {
        let (mut runner, _) = loaded_library();
        seed_all_account_data(&mut runner);
        let task = begin_logout(&mut runner);
        let commands = runner.task_outcome(task, TaskOutcome::Failed(TaskError::LocalStorage));

        assert_eq!(runner.app().account, AccountState::Active);
        assert_seeded_account_data_is_kept(runner.app());
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(
            drawn.contains("Could not remove the local BOMTOON sign-in data."),
            "missing local-storage failure: {drawn}"
        );
        assert_fits(&screen);

        let commands = runner.action(action_id(RETRY));
        let mut resumed = Vec::new();
        for cancelled in cancelled_tasks(&commands) {
            resumed.extend(runner.task_outcome(cancelled, TaskOutcome::Cancelled));
        }
        let (_, work) = fetch_task_with(&resumed, "/comic/main");
        assert_eq!(work, api::homepage());
        assert_eq!(runner.app().account, AccountState::Checking);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert_all_account_data_cleared(runner.app());
    }

    #[test]
    fn expired_session_returns_to_login_instructions() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (task, work) = only_spawn(&commands);
        let Task::Fetch {
            url, credential, ..
        } = work
        else {
            panic!("opening a comic did not request content");
        };
        assert!(url.contains("/detail/hunter_q"));
        assert!(matches!(
            credential,
            Some(value)
                if value.secret == "bomtoon-session"
                    && value.header == SecretHeader::Named("Cookie".to_owned())
        ));

        let commands = runner.task_outcome(task, TaskOutcome::Failed(TaskError::Unauthorized));
        assert_eq!(runner.app().account, AccountState::Expired);
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(
            drawn.contains("BOMTOON sign-in has expired"),
            "missing expiration warning: {drawn}"
        );
        assert_login_instructions(&screen);
    }

    #[test]
    fn library_recent_and_content_credential_errors_show_account_states() {
        for (error, expected) in [
            (TaskError::NoCredential, AccountState::SignedOut),
            (TaskError::Unauthorized, AccountState::Expired),
        ] {
            let (runner, _commands) = failed_start(error);
            assert_eq!(runner.app().account, expected);
            let screen = runner.app().screen();
            if expected == AccountState::SignedOut {
                assert_eq!(runner.app().view, View::Main);
                assert!(format!("{screen:?}").contains("Sign in"));
            } else {
                assert_login_instructions(&screen);
            }

            for (runner, commands) in [
                failed_library_action(RECENT, error),
                failed_library_action("comic-0", error),
            ] {
                assert_eq!(runner.app().account, expected);
                if expected == AccountState::SignedOut {
                    assert_eq!(runner.app().view, View::Main);
                    assert!(format!("{:?}", last_screen(&commands)).contains("Sign in"));
                } else {
                    assert_login_instructions(&last_screen(&commands));
                }
            }
        }
    }

    #[test]
    fn comic_selection_uses_personalized_detail_and_back_returns_to_the_library() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (task, work) = only_spawn(&commands);
        assert!(matches!(
            work,
            Task::Fetch { ref url, .. }
                if url.contains("/detail/hunter_q")
        ));

        let commands = runner.task_outcome(task, TaskOutcome::Completed(content_response()));
        let screen = last_screen(&commands);
        assert_eq!(runner.app().view, View::Episodes);
        assert!(screen.owns_back);
        assert_fits(&screen);

        let commands = runner.action(ActionId::BACK);
        let screen = last_screen(&commands);
        assert_eq!(runner.app().view, View::Main);
        assert!(!screen.owns_back);
        assert_fits(&screen);
    }

    #[test]
    fn content_detail_replaces_the_provisional_shelf_title() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (task, _) = only_spawn(&commands);

        runner.task_outcome(task, TaskOutcome::Completed(content_response()));

        assert_eq!(runner.app().selected_title, "Localized Title");
        assert_eq!(runner.app().selected_creators, "Writer");
        assert_eq!(runner.app().selected_synopsis, "Synopsis");
    }

    #[test]
    fn logout_rejects_a_nonempty_completion_and_keeps_account_data() {
        let (mut runner, _) = loaded_library();
        seed_all_account_data(&mut runner);
        let task = begin_logout(&mut runner);
        let commands = runner.task_outcome(task, TaskOutcome::Completed(b"unexpected".to_vec()));

        assert_eq!(runner.app().account, AccountState::Active);
        assert_seeded_account_data_is_kept(runner.app());
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(
            drawn.contains("BOMTOON returned unexpected sign-out data."),
            "missing strict revoke-result failure: {drawn}"
        );
        assert_fits(&screen);
    }

    #[test]
    fn episode_pagination_keeps_six_items_per_page_and_stops_at_the_final_page() {
        assert_eq!(EPISODE_ITEMS_PER_PAGE, 6);
        assert_eq!(
            page_bounds(0, EPISODE_ITEMS_PER_PAGE + 1, EPISODE_ITEMS_PER_PAGE),
            (0, 6)
        );
        assert_eq!(
            page_bounds(1, EPISODE_ITEMS_PER_PAGE + 1, EPISODE_ITEMS_PER_PAGE),
            (6, 7)
        );
        let episodes = (0..=EPISODE_ITEMS_PER_PAGE)
            .map(|index| Episode {
                id: index,
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
        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::Active,
            view: View::Episodes,
            episodes,
            ..Bomtoon::default()
        });
        runner.app_mut().prepare_episode_layout();

        let commands = runner.action(action_id(NEXT_PAGE));
        assert_eq!(runner.app().page, 1);
        assert_eq!(
            last_screen(&commands)
                .page_turns
                .as_ref()
                .and_then(|turns| turns.position),
            Some((2, 2))
        );

        let commands = runner.action(action_id(NEXT_PAGE));
        assert_eq!(runner.app().page, 1);
        assert!(commands.is_empty());
    }

    #[test]
    fn modeled_reader_memory_bounds_are_byte_exact() {
        assert_eq!(gray8_conservative_bytes(), 96_079_168);
        assert_eq!(rgb8_conservative_bytes(), 93_935_424);
        assert!(gray8_conservative_bytes() <= 96 * 1024 * 1024);
        assert!(rgb8_conservative_bytes() <= 96 * 1024 * 1024);
        assert_eq!(LARGEST_RGB8_PAGE_BYTES, 1_264 * 1_680 * 3);
        assert_eq!(LARGEST_RGB8_PAGE_BYTES, 6_370_560);
    }

    #[test]
    fn format_bounded_reader_window_limits_are_exact() {
        assert_eq!(
            reader_limits(PictureFormat::Gray8),
            ReaderLimits {
                pages: 3,
                source_slots: 2,
                fetches: 2,
                tasks: 4,
            }
        );
        assert_eq!(
            reader_limits(PictureFormat::Rgb8),
            ReaderLimits {
                pages: 2,
                source_slots: 1,
                fetches: 1,
                tasks: 2,
            }
        );
    }
    #[test]
    fn format_bounded_reader_window_reverse_completions() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let metrics = reader_metrics(format, 1);
            let (mut runner, manifest_task, _) = reader_waiting_for_manifest_with_metrics(metrics);
            runner.task_outcome(
                manifest_task,
                TaskOutcome::Completed(image_manifest_sources(8)),
            );
            assert_reader_bounds(runner.app());
            let mut maximum_window = 0;
            let mut maximum_combined_sources = 0;
            let mut maximum_fetches = 0;
            let mut callbacks = 0;
            while let Some((&task, entry)) = runner.app().reader_tasks.iter().next_back() {
                let purpose = entry.purpose;
                let bytes = match purpose {
                    ReaderTaskPurpose::ForegroundSource { .. }
                    | ReaderTaskPurpose::PrefetchSource { .. } => TINY_WEBP.to_vec(),
                    ReaderTaskPurpose::Manifest
                    | ReaderTaskPurpose::ManifestRefresh
                    | ReaderTaskPurpose::Maintenance => Vec::new(),
                };
                runner.task_outcome(task, TaskOutcome::Completed(bytes));
                assert_reader_bounds(runner.app());
                let reader = runner.app().reader.as_ref().expect("reader");
                maximum_window = maximum_window.max(reader.window.len());
                maximum_combined_sources = maximum_combined_sources
                    .max(reader.source_cache.len() + reader.source_fetches.len());
                maximum_fetches = maximum_fetches.max(reader.source_fetches.len());
                callbacks += 1;
                assert!(callbacks < 32, "reader scheduling did not settle");
            }
            let limits = reader_limits(format);
            assert_eq!(maximum_window, limits.pages);
            assert_eq!(maximum_combined_sources, limits.source_slots);
            assert_eq!(maximum_fetches, limits.fetches);
        }
    }

    #[test]
    fn first_page_put_precedes_screen_and_zero_second_maintenance() {
        let metrics = reader_metrics(PictureFormat::Gray8, 1);
        let (mut runner, manifest_task, _) = reader_waiting_for_manifest_with_metrics(metrics);
        let commands = runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest_sources(1)),
        );
        let (source_task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(source_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        assert_reader_bounds(runner.app());

        let put_index = commands
            .iter()
            .position(|command| matches!(command, Command::PutPicture { .. }))
            .expect("first page put");
        let screen_index = commands
            .iter()
            .position(|command| matches!(command, Command::SetScreen(_)))
            .expect("first page screen");
        let maintenance_spawn_index = commands
            .iter()
            .position(|command| {
                matches!(
                    command,
                    Command::Spawn {
                        work: Task::Sleep { seconds: 0 },
                        ..
                    }
                )
            })
            .expect("zero-second maintenance spawn");
        assert!(put_index < screen_index);
        assert!(screen_index < maintenance_spawn_index);
        assert_eq!(
            runner
                .app()
                .reader
                .as_ref()
                .expect("reader")
                .source_cache
                .len(),
            0
        );
    }

    #[test]
    fn rgb_reader_source_bound_copies_four_one_row_sources_sequentially() {
        let metrics = reader_metrics(PictureFormat::Rgb8, 4);
        let (mut runner, manifest_task, _) = reader_waiting_for_manifest_with_metrics(metrics);
        runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest_sources(4)),
        );
        assert_reader_bounds(runner.app());

        let mut observed_sources = Vec::new();
        let mut final_commands = Vec::new();
        for expected_source in 0..4 {
            let (&task, entry) = runner
                .app()
                .reader_tasks
                .iter()
                .next_back()
                .expect("one source task");
            let ReaderTaskPurpose::ForegroundSource {
                source,
                offset_rows: 0,
            } = entry.purpose
            else {
                panic!("expected foreground offset-zero source");
            };
            assert_eq!(source, expected_source);
            observed_sources.push(source);
            final_commands = runner.task_outcome(task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
            assert_reader_bounds(runner.app());
        }
        assert_eq!(observed_sources, [0, 1, 2, 3]);

        let expected_source =
            decode_reader_source(TINY_WEBP, &episode_image(0, 1, 1), PictureFormat::Rgb8, 1)
                .expect("RGB source");
        let PicturePixels::Rgb8(source_pixels) = expected_source.into_pixels() else {
            panic!("expected RGB source pixels");
        };
        let expected_page = source_pixels.repeat(4);
        let uploaded = final_commands
            .iter()
            .find_map(|command| match command {
                Command::PutPicture {
                    pixels: PicturePixels::Rgb8(pixels),
                    ..
                } => Some(pixels.as_slice()),
                _ => None,
            })
            .expect("assembled RGB page upload");
        assert_eq!(uploaded, expected_page);
        let reader = runner.app().reader.as_ref().expect("reader");
        assert!(reader.window.is_empty());
        assert!(reader.source_cache.is_empty());
        assert!(reader.source_fetches.is_empty());
    }
    #[test]
    fn rapid_ready_page_turns_coalesce_rgb_maintenance() {
        let metrics = reader_metrics(PictureFormat::Rgb8, 1);
        let (mut runner, manifest_task, _) = reader_waiting_for_manifest_with_metrics(metrics);
        let commands = runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest_sources(5)),
        );
        let (first_source, _) = only_spawn(&commands);
        runner.task_outcome(first_source, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        let first_maintenance = runner
            .app()
            .reader
            .as_ref()
            .expect("reader")
            .maintenance_task
            .expect("first maintenance");
        runner.task_outcome(first_maintenance, TaskOutcome::Completed(Vec::new()));

        let page_one_source = runner
            .app()
            .reader_tasks
            .iter()
            .find_map(|(task, entry)| {
                matches!(
                    entry.purpose,
                    ReaderTaskPurpose::PrefetchSource { source: 1 }
                )
                .then_some(*task)
            })
            .expect("page one source");
        runner.task_outcome(page_one_source, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        let page_two_source = runner
            .app()
            .reader_tasks
            .iter()
            .find_map(|(task, entry)| {
                matches!(
                    entry.purpose,
                    ReaderTaskPurpose::PrefetchSource { source: 2 }
                )
                .then_some(*task)
            })
            .expect("page two source");

        runner.action(action_id(READER_NEXT));
        assert_eq!(runner.app().reader.as_ref().expect("reader").offset_rows, 1);
        runner.task_outcome(page_two_source, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        let existing_maintenance = runner
            .app()
            .reader
            .as_ref()
            .expect("reader")
            .maintenance_task
            .expect("pending maintenance");
        assert_eq!(runner.app().reader_tasks.len(), 2);

        runner.action(action_id(READER_NEXT));
        let app = runner.app();
        let reader = app.reader.as_ref().expect("reader");
        assert_eq!(reader.offset_rows, 2);
        assert!(app.problem.is_none());
        assert_eq!(reader.maintenance_task, Some(existing_maintenance));
        assert_eq!(
            app.reader_tasks
                .values()
                .filter(|entry| entry.purpose == ReaderTaskPurpose::Maintenance)
                .count(),
            1
        );
        assert_reader_bounds(app);

        runner.task_outcome(existing_maintenance, TaskOutcome::Completed(Vec::new()));
        let app = runner.app();
        let reader = app.reader.as_ref().expect("reader");
        assert!(app.problem.is_none());
        assert_eq!(reader.offset_rows, 2);
        assert_eq!(reader.maintenance_task, None);
        assert_eq!(reader.window.len(), reader.limits.pages);
        assert_reader_bounds(app);
    }
    fn reader_with_source_task(
        format: PictureFormat,
        intent: FetchIntent,
    ) -> (AppRunner<Bomtoon>, TaskId) {
        let mut runner = seeded_reader_with_metrics(reader_metrics(format, 1), 5, 0, false);
        let source_task = TaskId(41);
        {
            let app = runner.app_mut();
            let reader = app.reader.as_mut().expect("reader");
            reader.window.clear();
            for offset_rows in 1..=u64::try_from(reader.limits.pages).expect("viewport limit") {
                let plan =
                    viewport_plan(&reader.strip, offset_rows, 1).expect("source viewport plan");
                reader.window.push_back(ViewportEntry::Building(
                    ViewportBuild::new(offset_rows, plan, format, 1, 1)
                        .expect("source viewport build"),
                ));
            }
            reader.source_fetches.insert(1, source_task);
            app.reader_tasks.insert(
                source_task,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: match intent {
                        FetchIntent::Foreground { offset_rows } => {
                            ReaderTaskPurpose::ForegroundSource {
                                source: 1,
                                offset_rows,
                            }
                        }
                        FetchIntent::Prefetch => ReaderTaskPurpose::PrefetchSource { source: 1 },
                    },
                },
            );
            if matches!(intent, FetchIntent::Foreground { .. }) {
                app.foreground_reader_task = Some(source_task);
            }
        }
        (runner, source_task)
    }

    fn reader_with_reused_source_task(format: PictureFormat) -> (AppRunner<Bomtoon>, TaskId) {
        let mut runner = seeded_reader_with_metrics(reader_metrics(format, 1), 3, 0, false);
        let source_task = TaskId(41);
        {
            let app = runner.app_mut();
            app.retry = Retry::Viewport(1);
            app.foreground_reader_task = Some(source_task);
            app.reader_tasks.insert(
                source_task,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: ReaderTaskPurpose::ForegroundSource {
                        source: 0,
                        offset_rows: 1,
                    },
                },
            );
            let reader = app.reader.as_mut().expect("reader");
            reader.images = vec![episode_image(0, 1, 3)];
            reader.strip = episode_strip(&reader.images, 1, 1).expect("reused source strip");
            let plan = viewport_plan(&reader.strip, 1, 1).expect("reused viewport plan");
            reader.window = VecDeque::from([ViewportEntry::Building(
                ViewportBuild::new(1, plan, format, 1, 1).expect("offset-one build"),
            )]);
            reader.source_fetches = BTreeMap::from([(0, source_task)]);
        }
        (runner, source_task)
    }

    fn finish_reused_source_refresh(
        runner: &mut AppRunner<Bomtoon>,
        source_task: TaskId,
        policy: &str,
    ) {
        runner.task_outcome(source_task, TaskOutcome::Failed(TaskError::Unauthorized));
        let refresh = runner
            .app()
            .reader
            .as_ref()
            .and_then(|reader| reader.refresh_task)
            .expect("foreground refresh");
        let manifest = format!(
            "{{\"result\":\"SUCCESS\",\"data\":[{{\"orderNo\":1,\"width\":1,\"height\":3,\"imagePath\":\"https://image.balcony.studio/tw/ep/0.webp?Policy={policy}&Signature=s&Key-Pair-Id=k\",\"line\":null,\"point\":null}}]}}"
        )
        .into_bytes();
        runner.task_outcome(refresh, TaskOutcome::Completed(manifest));
        let refreshed_source = active_reader_source(runner.app(), 0)
            .map(|(task, intent)| {
                assert_eq!(intent, FetchIntent::Foreground { offset_rows: 1 });
                task
            })
            .expect("refreshed reused source");
        runner.task_outcome(
            refreshed_source,
            TaskOutcome::Completed(BLACK_1X3_WEBP.to_vec()),
        );
        let maintenance = runner
            .app()
            .reader
            .as_ref()
            .and_then(|reader| reader.maintenance_task)
            .expect("reused source maintenance");
        runner.task_outcome(maintenance, TaskOutcome::Completed(Vec::new()));
        let reader = runner.app().reader.as_ref().expect("reader");
        assert_eq!(reader.offset_rows, 1);
        assert!(reader.source_cache.is_empty());
        assert_eq!(
            reader.refresh_attempted.get(&0),
            Some(&FetchIntent::Foreground { offset_rows: 1 })
        );
        assert_reader_bounds(runner.app());
    }

    fn active_reader_source(app: &Bomtoon, source: usize) -> Option<(TaskId, FetchIntent)> {
        app.reader_tasks.iter().find_map(|(task, entry)| {
            let intent = match entry.purpose {
                ReaderTaskPurpose::ForegroundSource {
                    source: task_source,
                    offset_rows,
                } if task_source == source => FetchIntent::Foreground { offset_rows },
                ReaderTaskPurpose::PrefetchSource {
                    source: task_source,
                } if task_source == source => FetchIntent::Prefetch,
                ReaderTaskPurpose::Manifest
                | ReaderTaskPurpose::ManifestRefresh
                | ReaderTaskPurpose::Maintenance
                | ReaderTaskPurpose::ForegroundSource { .. }
                | ReaderTaskPurpose::PrefetchSource { .. } => return None,
            };
            Some((*task, intent))
        })
    }

    #[test]
    fn prefetch_failure_stays_interactive_and_retry_resumes_the_exact_requested_page() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, prefetch) = reader_with_source_task(format, FetchIntent::Prefetch);

            let commands = runner.task_outcome(prefetch, TaskOutcome::Failed(TaskError::TimedOut));
            let app = runner.app();
            let reader = app.reader.as_ref().expect("reader");
            assert!(
                app.problem.is_none(),
                "{format:?} exposed a background error"
            );
            assert!(app.foreground_reader_task.is_none());
            assert_eq!(reader.offset_rows, 0);
            assert_eq!(
                reader.picture.map(|picture| picture.handle),
                Some(PictureHandle(7))
            );
            assert!(reader.source_failures.contains_key(&1));
            assert!(last_screen(&commands).reading_surface.is_some());
            assert_reader_bounds(app);

            let commands = runner.action(action_id(READER_NEXT));
            let app = runner.app();
            assert!(app.problem.is_some());
            assert_eq!(app.retry, Retry::Viewport(1));
            assert_eq!(
                app.reader
                    .as_ref()
                    .and_then(|reader| reader.picture)
                    .map(|picture| picture.handle),
                Some(PictureHandle(7))
            );
            assert!(last_screen(&commands).reading_surface.is_none());

            let commands = runner.action(action_id(RETRY));
            let (retry_task, work) = only_spawn(&commands);
            assert!(matches!(
                work,
                Task::Fetch {
                    credential: None,
                    ..
                }
            ));
            let app = runner.app();
            let reader = app.reader.as_ref().expect("reader");
            assert!(app.problem.is_none());
            assert_eq!(app.retry, Retry::Viewport(1));
            assert_eq!(
                app.reader_tasks.get(&retry_task).map(|entry| entry.purpose),
                Some(ReaderTaskPurpose::ForegroundSource {
                    source: 1,
                    offset_rows: 1,
                })
            );
            assert!(!reader.source_failures.contains_key(&1));
            assert_reader_bounds(app);
        }
    }

    fn concurrent_refresh_runner(format: PictureFormat) -> (AppRunner<Bomtoon>, TaskId) {
        let (mut runner, first_source_task) =
            reader_with_source_task(format, FetchIntent::Prefetch);
        let first_commands = runner.task_outcome(
            first_source_task,
            TaskOutcome::Failed(TaskError::Unauthorized),
        );
        let refresh = runner
            .app()
            .reader
            .as_ref()
            .and_then(|reader| reader.refresh_task)
            .expect("shared refresh");
        assert!(runner.app().problem.is_none());
        assert!(runner.app().foreground_reader_task.is_none());
        assert_eq!(runner.app().account, AccountState::Active);
        assert!(runner.app().screen().reading_surface.is_some());
        let manifest_spawns = first_commands
            .iter()
            .filter(|command| {
                matches!(
                    command,
                    Command::Spawn {
                        work: Task::Fetch {
                            credential: Some(credential),
                            ..
                        },
                        ..
                    } if credential.header == SecretHeader::Bearer
                )
            })
            .count();
        assert_eq!(manifest_spawns, 1);
        let second_source_task = active_reader_source(runner.app(), 2)
            .map(|(task, _)| task)
            .expect("later source scheduled while refresh is pending");
        let second_commands = runner.task_outcome(
            second_source_task,
            TaskOutcome::Failed(TaskError::Unauthorized),
        );
        let reader = runner.app().reader.as_ref().expect("reader");
        assert_eq!(reader.refresh_task, Some(refresh));
        assert_eq!(
            reader.refresh_waiters.keys().copied().collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            reader.refresh_attempted.keys().copied().collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(!second_commands.iter().any(|command| {
            matches!(
                command,
                Command::Spawn {
                    work: Task::Fetch {
                        credential: Some(_),
                        ..
                    },
                    ..
                }
            )
        }));
        assert_reader_bounds(runner.app());
        (runner, refresh)
    }

    #[test]
    fn concurrent_unauthorized_sources_share_one_bounded_manifest_refresh() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, refresh) = concurrent_refresh_runner(format);

            runner.task_outcome(
                refresh,
                TaskOutcome::Completed(image_manifest_sources_with_policy(5, "r")),
            );
            let reader = runner.app().reader.as_ref().expect("reader");
            assert!(reader
                .images
                .iter()
                .enumerate()
                .all(|(source, image)| image.url.contains(&format!("Policy=r{source}"))));
            assert_eq!(
                active_reader_source(runner.app(), 1).map(|(_, intent)| intent),
                Some(FetchIntent::Prefetch),
                "{format:?} did not retry the lowest source first"
            );
            assert!(reader.refresh_waiters.contains_key(&2));
            assert_reader_bounds(runner.app());

            let retry = active_reader_source(runner.app(), 1)
                .map(|(task, _)| task)
                .expect("source one retry");
            let commands = runner.task_outcome(retry, TaskOutcome::Failed(TaskError::Unauthorized));
            let reader = runner.app().reader.as_ref().expect("reader");
            assert!(reader.refresh_task.is_none());
            assert!(reader.source_failures.contains_key(&1));
            assert!(!commands.iter().any(|command| {
                matches!(
                    command,
                    Command::Spawn {
                        work: Task::Fetch {
                            credential: Some(_),
                            ..
                        },
                        ..
                    }
                )
            }));
            assert_reader_bounds(runner.app());

            runner.action(action_id(READER_NEXT));
            assert!(runner.app().problem.is_some());
            assert_eq!(runner.app().retry, Retry::Viewport(1));
            runner.action(action_id(RETRY));
            let reader = runner.app().reader.as_ref().expect("reader");
            assert_eq!(
                reader.refresh_attempted.get(&1),
                Some(&FetchIntent::Prefetch)
            );
            assert_eq!(
                active_reader_source(runner.app(), 1).map(|(_, intent)| intent),
                Some(FetchIntent::Foreground { offset_rows: 1 })
            );
            assert!(reader.refresh_task.is_none());
            assert_reader_bounds(runner.app());
        }
    }

    #[test]
    fn promoted_prefetch_refresh_preserves_original_intent_after_second_unauthorized() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, source_task) = reader_with_source_task(format, FetchIntent::Prefetch);
            runner.task_outcome(source_task, TaskOutcome::Failed(TaskError::Unauthorized));
            let refresh = runner
                .app()
                .reader
                .as_ref()
                .and_then(|reader| reader.refresh_task)
                .expect("background refresh");

            runner.action(action_id(READER_NEXT));
            let reader = runner.app().reader.as_ref().expect("reader");
            assert_eq!(runner.app().retry, Retry::Viewport(1));
            assert_eq!(
                reader.refresh_waiters.get(&1),
                Some(&FetchIntent::Foreground { offset_rows: 1 })
            );
            assert_eq!(
                reader.refresh_attempted.get(&1),
                Some(&FetchIntent::Prefetch)
            );

            runner.task_outcome(
                refresh,
                TaskOutcome::Completed(image_manifest_sources_with_policy(5, "i")),
            );
            let retried = active_reader_source(runner.app(), 1)
                .map(|(task, intent)| {
                    assert_eq!(intent, FetchIntent::Foreground { offset_rows: 1 });
                    task
                })
                .expect("promoted refreshed source");
            runner.task_outcome(retried, TaskOutcome::Failed(TaskError::Unauthorized));

            let app = runner.app();
            let reader = app.reader.as_ref().expect("reader");
            assert!(app.problem.is_none());
            assert!(app.foreground_reader_task.is_none());
            assert!(reader.refresh_task.is_none());
            assert!(reader.source_failures.contains_key(&1));
            assert_eq!(
                reader.refresh_attempted.get(&1),
                Some(&FetchIntent::Prefetch)
            );
            assert!(app.screen().reading_surface.is_some());
            assert_reader_bounds(app);
        }
    }

    #[test]
    fn stale_original_foreground_uses_current_foreground_for_terminal_unauthorized() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, source_task) = reader_with_reused_source_task(format);
            finish_reused_source_refresh(&mut runner, source_task, "foreground");

            runner.action(action_id(READER_PREVIOUS));
            let second_fetch = active_reader_source(runner.app(), 0)
                .map(|(task, intent)| {
                    assert_eq!(intent, FetchIntent::Foreground { offset_rows: 0 });
                    task
                })
                .expect("page-zero reused source");
            let commands =
                runner.task_outcome(second_fetch, TaskOutcome::Failed(TaskError::Unauthorized));

            let app = runner.app();
            let reader = app.reader.as_ref().expect("reader");
            assert!(app.problem.is_some());
            assert_eq!(app.retry, Retry::Viewport(0));
            assert!(reader.refresh_task.is_none());
            assert_eq!(
                reader.refresh_attempted.get(&0),
                Some(&FetchIntent::Foreground { offset_rows: 1 })
            );
            assert!(!commands
                .iter()
                .any(|command| matches!(command, Command::Spawn { .. })));
            assert_reader_bounds(app);
        }
    }

    #[test]
    fn stale_original_foreground_uses_current_prefetch_for_terminal_unauthorized() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, source_task) = reader_with_reused_source_task(format);
            finish_reused_source_refresh(&mut runner, source_task, "prefetch");
            let prefetch = TaskId(90);
            {
                let app = runner.app_mut();
                app.retry = Retry::Viewport(2);
                app.foreground_reader_task = None;
                app.reader_tasks.clear();
                app.reader_tasks.insert(
                    prefetch,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::PrefetchSource { source: 0 },
                    },
                );
                let reader = app.reader.as_mut().expect("reader");
                reader.window = VecDeque::from([ViewportEntry::Building(
                    ViewportBuild::new(
                        2,
                        viewport_plan(&reader.strip, 2, 1).expect("offset-two plan"),
                        format,
                        1,
                        1,
                    )
                    .expect("offset-two build"),
                )]);
                reader.source_fetches = BTreeMap::from([(0, prefetch)]);
                reader.maintenance_task = None;
            }

            let commands =
                runner.task_outcome(prefetch, TaskOutcome::Failed(TaskError::Unauthorized));
            let app = runner.app();
            let reader = app.reader.as_ref().expect("reader");
            assert!(app.problem.is_none());
            assert!(app.foreground_reader_task.is_none());
            assert!(reader.refresh_task.is_none());
            assert!(reader.source_failures.contains_key(&0));
            assert_eq!(
                reader.refresh_attempted.get(&0),
                Some(&FetchIntent::Foreground { offset_rows: 1 })
            );
            assert!(!commands
                .iter()
                .any(|command| matches!(command, Command::Spawn { .. })));
            assert!(app.screen().reading_surface.is_some());
            assert_reader_bounds(app);
        }
    }

    #[test]
    fn queued_foreground_refresh_waiter_is_dropped_after_page_turn_retargets() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, _) = reader_with_source_task(format, FetchIntent::Prefetch);
            let refresh = TaskId(42);
            let first_source = TaskId(43);
            {
                let app = runner.app_mut();
                app.reader_tasks.clear();
                app.foreground_reader_task = Some(refresh);
                app.retry = Retry::Viewport(2);
                app.reader_tasks.insert(
                    refresh,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::ManifestRefresh,
                    },
                );
                app.reader_tasks.insert(
                    first_source,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::PrefetchSource { source: 1 },
                    },
                );
                let mut source_fetches = BTreeMap::from([(1, first_source)]);
                if format == PictureFormat::Gray8 {
                    let second_source = TaskId(44);
                    source_fetches.insert(3, second_source);
                    app.reader_tasks.insert(
                        second_source,
                        ReaderTaskEntry {
                            generation: 1,
                            purpose: ReaderTaskPurpose::PrefetchSource { source: 3 },
                        },
                    );
                }
                let reader = app.reader.as_mut().expect("reader");
                reader.source_fetches = source_fetches;
                reader.refresh_task = Some(refresh);
                reader
                    .refresh_waiters
                    .insert(2, FetchIntent::Foreground { offset_rows: 2 });
                reader
                    .refresh_attempted
                    .insert(2, FetchIntent::Foreground { offset_rows: 2 });
            }

            runner.task_outcome(
                refresh,
                TaskOutcome::Completed(image_manifest_sources_with_policy(5, "t")),
            );
            assert!(runner
                .app()
                .reader
                .as_ref()
                .expect("reader")
                .refresh_waiters
                .contains_key(&2));

            runner.action(action_id(READER_NEXT));
            let reader = runner.app().reader.as_ref().expect("reader");
            assert_eq!(runner.app().retry, Retry::Viewport(1));
            assert!(!reader.refresh_waiters.contains_key(&2));
            assert_eq!(
                active_reader_source(runner.app(), 1).map(|(_, intent)| intent),
                Some(FetchIntent::Foreground { offset_rows: 1 })
            );

            runner.task_outcome(first_source, TaskOutcome::Completed(TINY_WEBP.to_vec()));
            let maintenance = runner
                .app()
                .reader
                .as_ref()
                .and_then(|reader| reader.maintenance_task)
                .expect("page-one maintenance");
            runner.task_outcome(maintenance, TaskOutcome::Completed(Vec::new()));
            let app = runner.app();
            let reader = app.reader.as_ref().expect("reader");
            assert_eq!(reader.offset_rows, 1);
            assert!(app.problem.is_none());
            assert_ne!(
                active_reader_source(app, 2).map(|(_, intent)| intent),
                Some(FetchIntent::Foreground { offset_rows: 2 })
            );
            assert!(app.foreground_reader_task.is_none());
            assert!(!reader.refresh_waiters.contains_key(&2));
            assert_reader_bounds(app);
        }
    }

    #[test]
    fn manifest_refresh_drains_foreground_before_lower_prefetch_with_format_bounds() {
        for format in [PictureFormat::Gray8, PictureFormat::Rgb8] {
            let (mut runner, _) = reader_with_source_task(format, FetchIntent::Prefetch);
            let refresh = TaskId(42);
            {
                let app = runner.app_mut();
                app.reader_tasks.clear();
                app.foreground_reader_task = Some(refresh);
                app.retry = Retry::Viewport(2);
                app.reader_tasks.insert(
                    refresh,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::ManifestRefresh,
                    },
                );
                let reader = app.reader.as_mut().expect("reader");
                reader.source_fetches.clear();
                reader.refresh_task = Some(refresh);
                reader.refresh_waiters = BTreeMap::from([
                    (1, FetchIntent::Prefetch),
                    (2, FetchIntent::Foreground { offset_rows: 2 }),
                ]);
                reader.refresh_attempted = reader.refresh_waiters.clone();
            }

            runner.task_outcome(
                refresh,
                TaskOutcome::Completed(image_manifest_sources_with_policy(5, "f")),
            );
            let app = runner.app();
            let reader = app.reader.as_ref().expect("reader");
            assert_eq!(
                active_reader_source(app, 2).map(|(_, intent)| intent),
                Some(FetchIntent::Foreground { offset_rows: 2 })
            );
            if format == PictureFormat::Gray8 {
                assert_eq!(
                    active_reader_source(app, 1).map(|(_, intent)| intent),
                    Some(FetchIntent::Prefetch)
                );
            } else {
                assert!(reader.refresh_waiters.contains_key(&1));
            }
            assert_eq!(
                app.foreground_reader_task,
                active_reader_source(app, 2).map(|pair| pair.0)
            );
            assert_reader_bounds(app);
        }
    }

    #[test]
    fn manifest_refresh_does_not_drain_prefetch_ahead_of_a_busy_foreground() {
        let format = PictureFormat::Gray8;
        let (mut runner, _) = reader_with_source_task(format, FetchIntent::Prefetch);
        let refresh = TaskId(42);
        let active_foreground = TaskId(43);
        {
            let app = runner.app_mut();
            app.reader_tasks.clear();
            app.foreground_reader_task = Some(active_foreground);
            app.retry = Retry::Viewport(2);
            app.reader_tasks.insert(
                refresh,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: ReaderTaskPurpose::ManifestRefresh,
                },
            );
            app.reader_tasks.insert(
                active_foreground,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: ReaderTaskPurpose::ForegroundSource {
                        source: 3,
                        offset_rows: 3,
                    },
                },
            );
            let reader = app.reader.as_mut().expect("reader");
            reader.source_fetches = BTreeMap::from([(3, active_foreground)]);
            reader.refresh_task = Some(refresh);
            reader.refresh_waiters = BTreeMap::from([
                (1, FetchIntent::Prefetch),
                (2, FetchIntent::Foreground { offset_rows: 2 }),
            ]);
            reader.refresh_attempted = reader.refresh_waiters.clone();
        }

        runner.task_outcome(
            refresh,
            TaskOutcome::Completed(image_manifest_sources_with_policy(5, "q")),
        );
        let app = runner.app();
        let reader = app.reader.as_ref().expect("reader");
        assert!(active_reader_source(app, 1).is_none());
        assert_eq!(
            reader.refresh_waiters.keys().copied().collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(app.foreground_reader_task, Some(active_foreground));
        assert_reader_bounds(app);
    }

    #[test]
    fn manifest_refresh_drain_respects_a_saturated_rgb_source_bound() {
        let format = PictureFormat::Rgb8;
        let (mut runner, _) = reader_with_source_task(format, FetchIntent::Prefetch);
        let refresh = TaskId(42);
        {
            let app = runner.app_mut();
            app.reader_tasks.clear();
            app.foreground_reader_task = Some(refresh);
            app.retry = Retry::Viewport(2);
            app.reader_tasks.insert(
                refresh,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: ReaderTaskPurpose::ManifestRefresh,
                },
            );
            let active_source = TaskId(43);
            app.reader_tasks.insert(
                active_source,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: ReaderTaskPurpose::PrefetchSource { source: 1 },
                },
            );
            let reader = app.reader.as_mut().expect("reader");
            reader.source_fetches = BTreeMap::from([(1, active_source)]);
            reader.refresh_task = Some(refresh);
            reader
                .refresh_waiters
                .insert(2, FetchIntent::Foreground { offset_rows: 2 });
            reader
                .refresh_attempted
                .insert(2, FetchIntent::Foreground { offset_rows: 2 });
        }

        runner.task_outcome(
            refresh,
            TaskOutcome::Completed(image_manifest_sources_with_policy(5, "b")),
        );
        let app = runner.app();
        let reader = app.reader.as_ref().expect("reader");
        assert!(active_reader_source(app, 2).is_none());
        assert_eq!(
            reader.refresh_waiters.get(&2),
            Some(&FetchIntent::Foreground { offset_rows: 2 })
        );
        assert_reader_bounds(app);
    }

    #[test]
    fn refresh_failure_maps_foreground_to_page_and_prefetch_to_source_failure() {
        let (mut background, source_task) =
            reader_with_source_task(PictureFormat::Gray8, FetchIntent::Prefetch);
        background.task_outcome(source_task, TaskOutcome::Failed(TaskError::Unauthorized));
        let refresh = background
            .app()
            .reader
            .as_ref()
            .and_then(|reader| reader.refresh_task)
            .expect("background refresh");
        let commands = background.task_outcome(refresh, TaskOutcome::Failed(TaskError::TimedOut));
        let app = background.app();
        assert!(app.problem.is_none());
        assert!(app
            .reader
            .as_ref()
            .expect("reader")
            .source_failures
            .contains_key(&1));
        assert!(app.screen().reading_surface.is_some());
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::SetScreen(_))));

        let (mut foreground, source_task) = reader_with_source_task(
            PictureFormat::Gray8,
            FetchIntent::Foreground { offset_rows: 1 },
        );
        foreground.task_outcome(source_task, TaskOutcome::Failed(TaskError::Unauthorized));
        let refresh = foreground
            .app()
            .reader
            .as_ref()
            .and_then(|reader| reader.refresh_task)
            .expect("foreground refresh");
        let commands = foreground.task_outcome(refresh, TaskOutcome::Failed(TaskError::TimedOut));
        assert!(foreground.app().problem.is_some());
        assert_eq!(foreground.app().retry, Retry::Viewport(1));
        assert!(last_screen(&commands).reading_surface.is_none());
    }

    #[test]
    fn same_assets_accepts_url_changes_only_and_rejects_every_identity_change() {
        let current = vec![episode_image(0, 1, 2), episode_image(1, 3, 4)];
        let mut refreshed = current.clone();
        refreshed[0].url.push_str("?Policy=new");
        refreshed[1].url.push_str("?Policy=new");
        assert!(same_assets(&current, &refreshed));

        let mut changed_count = refreshed.clone();
        changed_count.pop();
        assert!(!same_assets(&current, &changed_count));
        for mutate in [
            |image: &mut EpisodeImage| image.order += 1,
            |image: &mut EpisodeImage| image.width += 1,
            |image: &mut EpisodeImage| image.height += 1,
            |image: &mut EpisodeImage| image.path.push_str(".changed"),
        ] {
            let mut changed = refreshed.clone();
            mutate(&mut changed[0]);
            assert!(!same_assets(&current, &changed));
        }
        let mut reordered = refreshed;
        reordered.swap(0, 1);
        assert!(!same_assets(&current, &reordered));
    }

    #[test]
    fn manifest_credentials_on_refresh_preserve_account_transitions() {
        for (error, expected) in [
            (TaskError::NoCredential, AccountState::SignedOut),
            (TaskError::Unauthorized, AccountState::Expired),
        ] {
            let (mut runner, _) =
                reader_with_source_task(PictureFormat::Gray8, FetchIntent::Prefetch);
            let refresh = TaskId(42);
            {
                let app = runner.app_mut();
                app.reader_tasks.clear();
                app.reader_tasks.insert(
                    refresh,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::ManifestRefresh,
                    },
                );
                app.view = View::Episodes;
                app.reader.as_mut().expect("reader").refresh_task = Some(refresh);
            }

            runner.task_outcome(refresh, TaskOutcome::Failed(error));
            assert_eq!(runner.app().account, expected);
            assert_all_account_data_cleared(runner.app());
        }
    }

    fn shelf(alias: &str, title: &str) -> model::FeatureComic {
        model::FeatureComic {
            alias: alias.to_owned(),
            title: title.to_owned(),
            creators: "Creator".to_owned(),
            view_count: None,
            vertical_url: Some(format!(
                "https://image.balcony.studio/tw/contents/{alias}.webp"
            )),
            square_url: None,
        }
    }

    fn feed_with_recommendations(count: usize) -> FeaturedState {
        let banners = ["feature-a", "feature-b", "feature-c"]
            .into_iter()
            .map(|alias| shelf(alias, alias))
            .collect::<Vec<_>>();
        let collection = model::FeatureCollection {
            id: "newest".to_owned(),
            label: "人氣新作".to_owned(),
            priority: 2,
            order: 0,
            comics: (0..count)
                .map(|index| shelf(&format!("rec-{index}"), &format!("Recommended {index}")))
                .collect(),
        };
        FeaturedState {
            generation: 1,
            snapshot: Some(FeatureSnapshot {
                banners,
                collections: vec![collection.clone()],
                sources: BTreeMap::from([(FeatureSource::Homepage, vec![collection])]),
                failed_sources: BTreeSet::new(),
                warning: None,
            }),
            ..FeaturedState::default()
        }
    }

    fn featured_with_public_covers(urls: &[String]) -> FeaturedState {
        FeaturedState {
            generation: 1,
            snapshot: Some(FeatureSnapshot {
                banners: urls
                    .iter()
                    .enumerate()
                    .map(|(index, url)| model::FeatureComic {
                        alias: format!("public-shared-{index}"),
                        title: format!("Public shared {index}"),
                        creators: "Creator".to_owned(),
                        view_count: None,
                        vertical_url: Some(url.clone()),
                        square_url: None,
                    })
                    .collect(),
                collections: Vec::new(),
                sources: BTreeMap::new(),
                failed_sources: BTreeSet::new(),
                warning: None,
            }),
            ..FeaturedState::default()
        }
    }

    fn grouped_feature_state(warning: Option<&str>) -> FeaturedState {
        let collection =
            |id: &str, label: &str, priority: u8, order: usize| model::FeatureCollection {
                id: id.to_owned(),
                label: label.to_owned(),
                priority,
                order,
                comics: (0..8)
                    .map(|index| shelf(&format!("{id}-{index}"), &format!("{label} title {index}")))
                    .collect(),
            };
        let collections = vec![
            collection("freetime", "免費看", 10, 0),
            collection("theme-20", "Theme First", 9, 0),
            collection("only-in-bomtoon", "只在 Bomtoon", 8, 0),
            collection("theme-10", "Theme Second", 9, 1),
            collection("newest", "人氣新作", 2, 0),
        ];
        FeaturedState {
            generation: 1,
            snapshot: Some(FeatureSnapshot {
                banners: (0..4)
                    .map(|index| shelf(&format!("banner-{index}"), "unused banner label"))
                    .collect(),
                collections,
                sources: BTreeMap::new(),
                failed_sources: BTreeSet::new(),
                warning: warning.map(str::to_owned),
            }),
            ..FeaturedState::default()
        }
    }

    fn grouped_feature_app(warning: Option<&str>) -> Bomtoon {
        Bomtoon {
            account: AccountState::Active,
            view: View::Main,
            destination: MainDestination::Featured,
            featured: grouped_feature_state(warning),
            page: 7,
            ..Bomtoon::default()
        }
    }

    #[test]
    fn feature_feed_pages_never_split_duplicate_or_strand_warning() {
        for warning in [None, Some("Some Featured collections could not be loaded.")] {
            let featured = grouped_feature_state(warning);
            let snapshot = featured.snapshot().expect("grouped snapshot");
            let pages = feed_pages(snapshot, &CLARA_BW_METRICS);
            assert!(!pages.is_empty());
            assert!(pages.iter().all(|page| !page.blocks.is_empty()));
            let collection_blocks = pages
                .iter()
                .flat_map(|page| &page.blocks)
                .filter(|block| {
                    matches!(
                        block,
                        feature::FeedBlock::Collection(_) | feature::FeedBlock::ThemeWithHeading(_)
                    )
                })
                .count();
            assert_eq!(collection_blocks, snapshot.collections.len());
            assert!(pages
                .iter()
                .all(|page| page_fits(page, snapshot, &CLARA_BW_METRICS)));
        }
    }

    type GroupedFeatureSections = Vec<(String, Option<ActionId>)>;
    type GroupedFeatureGrids = Vec<Vec<kobo_sdk::Tile>>;

    fn grouped_feature_page_evidence(
        app: &mut Bomtoon,
        pages: &[FeedPage],
        page_index: usize,
    ) -> (GroupedFeatureSections, GroupedFeatureGrids) {
        let page = &pages[page_index];
        app.featured.feed_page = page_index;
        let screen = app.main_screen();
        let diagnostics = screen.diagnostics(&CLARA_BW_METRICS, &Chrome::measuring(true));
        assert!(
            !diagnostics.has_errors(),
            "page {page_index}: {:?}",
            diagnostics.issues
        );
        let turns = screen.page_turns.as_ref().expect("Feature page turns");
        assert_eq!(
            turns.position,
            Some((
                u16::try_from(page_index + 1).expect("small page"),
                u16::try_from(pages.len()).expect("small page count")
            ))
        );
        let mut sections = Vec::new();
        let mut grids = Vec::new();
        for node in &screen.nodes {
            match node {
                Node::ImageStrip { tiles, .. } => {
                    assert_eq!(page_index, 0);
                    assert_eq!(tiles.len(), 3);
                    assert!(tiles.iter().enumerate().all(|(index, tile)| {
                        tile.label.is_empty()
                            && tile.action == action_id(&format!("feature-banner-{index}"))
                    }));
                }
                Node::Section { title, action, .. } => sections.push((title.clone(), *action)),
                Node::MediaGrid { tiles, .. } => grids.push(tiles.clone()),
                _ => {}
            }
        }
        assert_eq!(
            page.blocks
                .iter()
                .filter(|block| matches!(block, feature::FeedBlock::Banners))
                .count(),
            screen
                .nodes
                .iter()
                .filter(|node| matches!(node, Node::ImageStrip { .. }))
                .count()
        );
        (sections, grids)
    }

    #[test]
    fn grouped_feature_feed_renders_exact_banners_collections_and_theme_group() {
        let mut app = grouped_feature_app(None);
        let snapshot = app.featured.snapshot().expect("snapshot").clone();
        let pages = feed_pages(&snapshot, &CLARA_BW_METRICS);
        let mut sections = Vec::new();
        let mut grids = Vec::new();
        for page_index in 0..pages.len() {
            let (page_sections, page_grids) =
                grouped_feature_page_evidence(&mut app, &pages, page_index);
            sections.extend(page_sections);
            grids.extend(page_grids);
        }
        let titles = sections
            .iter()
            .map(|(title, _)| title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            [
                "人氣新作",
                "只在 Bomtoon",
                "編輯精選",
                "Theme First",
                "Theme Second",
                "免費看"
            ]
        );
        assert_eq!(
            sections
                .iter()
                .filter(|(title, action)| title == "編輯精選" && action.is_none())
                .count(),
            1
        );
        for (title, action) in sections.iter().filter(|(title, _)| title != "編輯精選") {
            let collection = snapshot
                .collections
                .iter()
                .find(|collection| collection.label == *title)
                .expect("section collection");
            assert_eq!(*action, Some(action_id(&collection_action(&collection.id))));
        }
        assert_eq!(grids.len(), snapshot.collections.len());
        for (grid, collection) in grids.iter().zip(
            [
                "newest",
                "only-in-bomtoon",
                "theme-20",
                "theme-10",
                "freetime",
            ]
            .into_iter()
            .map(|id| snapshot.collection(id).expect(id)),
        ) {
            assert_eq!(grid.len(), 6);
            assert!(grid.iter().enumerate().all(|(index, tile)| {
                tile.action == action_id(&comic_action(&collection.id, index))
                    && tile.label
                        == display_content_title(
                            &collection.comics[index].title,
                            &collection.comics[index].alias,
                        )
            }));
        }
    }

    #[test]
    fn grouped_feature_feed_keeps_drawable_traditional_chinese_title() {
        let mut app = grouped_feature_app(None);
        let snapshot = app.featured.snapshot.as_mut().expect("snapshot");
        let comic = &mut snapshot.collections[0].comics[0];
        comic.alias = "365".to_owned();
        comic.title = "36.5度的晚餐".to_owned();
        let collection_id = snapshot.collections[0].id.clone();
        let snapshot = snapshot.clone();
        let pages = feed_pages(&snapshot, &CLARA_BW_METRICS);
        let action = action_id(&comic_action(&collection_id, 0));

        let label = (0..pages.len()).find_map(|page_index| {
            let (_, grids) = grouped_feature_page_evidence(&mut app, &pages, page_index);
            grids
                .into_iter()
                .flatten()
                .find(|tile| tile.action == action)
                .map(|tile| tile.label)
        });

        assert_eq!(label.as_deref(), Some("36.5度的晚餐"));
    }

    #[test]
    fn grouped_feature_feed_routes_exact_placement_and_collection_origin() {
        let mut app = grouped_feature_app(None);
        let urls = app
            .featured
            .snapshot()
            .expect("snapshot")
            .banners
            .iter()
            .chain(
                app.featured
                    .snapshot()
                    .expect("snapshot")
                    .collections
                    .iter()
                    .flat_map(|collection| collection.comics.iter()),
            )
            .filter_map(|comic| comic.vertical_url.clone())
            .collect::<Vec<_>>();
        for url in urls {
            app.covers.entries.insert(
                url,
                CoverState::Ready(TilePicture::new(PictureHandle(41), 60, 80)),
            );
        }
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        let pages = feed_pages(
            runner.app().featured.snapshot().expect("snapshot"),
            &CLARA_BW_METRICS,
        );
        runner.app_mut().featured.feed_page = pages.len().saturating_sub(1);
        let origin = runner.app().featured.feed_page;

        let commands = runner.action(action_id(&collection_action("theme-10")));
        assert!(!spawns(&commands).is_empty());
        let collection = runner
            .app()
            .featured
            .collection
            .as_ref()
            .expect("open collection");
        assert_eq!(collection.collection_id, "theme-10");
        assert_eq!(collection.origin_feed_page, origin);
        settle_open_collection_window(&mut runner, None);
        runner.action(ActionId::BACK);

        let commands = runner.action(action_id(&comic_action("theme-10", 4)));
        assert!(spawns(&commands)
            .iter()
            .any(|(_, task)| *task == api::detail("theme-10-4")));
        assert_eq!(runner.app().pending, Some(Pending::Content(4)));
        assert_eq!(runner.app().selected_content_alias, "theme-10-4");
        assert_eq!(runner.app().featured.feed_page, origin);
    }

    #[test]
    fn grouped_feature_feed_banner_action_opens_exact_banner_placement() {
        let mut runner = AppRunner::with_metrics(grouped_feature_app(None), CLARA_BW_METRICS);

        let commands = runner.action(action_id("feature-banner-1"));

        assert!(spawns(&commands)
            .iter()
            .any(|(_, task)| *task == api::detail("banner-1")));
        assert_eq!(runner.app().pending, Some(Pending::Content(1)));
        assert_eq!(runner.app().selected_content_alias, "banner-1");
    }

    #[test]
    fn grouped_feature_feed_warning_shares_content_page_and_retry_keeps_position() {
        let mut app = grouped_feature_app(Some("Some Featured collections could not be loaded."));
        let snapshot = app.featured.snapshot.as_mut().expect("snapshot");
        snapshot.failed_sources.insert(FeatureSource::Ranking);
        app.featured.feed_page = 1;
        let screen = app.main_screen();
        assert_eq!(retry_button_count(&screen), 1);
        assert!(screen
            .nodes
            .iter()
            .any(|node| matches!(node, Node::Banner { .. })));
        assert!(screen
            .nodes
            .iter()
            .any(|node| { matches!(node, Node::ImageStrip { .. } | Node::MediaGrid { .. }) }));
        assert_fits(&screen);

        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        let commands = runner.action(action_id(RETRY));
        assert!(spawns(&commands)
            .iter()
            .any(|(_, task)| *task == api::ranking()));
        assert_eq!(runner.app().featured.feed_page, 1);
        assert!(runner.app().featured.snapshot().is_some());
        let screen = last_screen(&commands);
        assert_eq!(retry_button_count(&screen), 1);
        assert!(screen
            .nodes
            .iter()
            .any(|node| { matches!(node, Node::ImageStrip { .. } | Node::MediaGrid { .. }) }));
    }

    #[test]
    fn grouped_feature_feed_signed_out_card_preserves_feed_origin_state() {
        let mut app = grouped_feature_app(None);
        app.account = AccountState::SignedOut;
        app.featured.feed_page = 2;
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);

        runner.action(action_id(&comic_action("theme-20", 1)));

        assert_eq!(runner.app().view, View::Status);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert_eq!(runner.app().featured.feed_page, 2);
        assert_eq!(runner.app().page, 0);
        assert!(runner.app().pending.is_none());
    }

    #[test]
    fn visible_feature_covers_cancel_obsolete_page_tasks() {
        let mut runner = AppRunner::with_metrics(grouped_feature_app(None), CLARA_BW_METRICS);
        let commands = runner.action(action_id("refresh-layout"));
        let first_tasks = cover_fetches(&commands)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        assert!(!first_tasks.is_empty());

        let commands = runner.action(action_id(NEXT_PAGE));

        let cancelled = cancelled_tasks(&commands);
        assert!(!cancelled.is_empty());
        assert!(cancelled.is_subset(&first_tasks));
        let expected = runner.app().visible_cover_urls();
        assert_eq!(runner.app().covers.visible_urls, expected);
    }
    #[test]
    fn feature_feed_pages_turn_without_mutating_shelf_page() {
        let mut runner = AppRunner::with_metrics(grouped_feature_app(None), CLARA_BW_METRICS);
        let page_count = feed_pages(
            runner.app().featured.snapshot().expect("snapshot"),
            &CLARA_BW_METRICS,
        )
        .len();
        assert!(page_count > 1);
        runner.action(action_id(NEXT_PAGE));
        assert_eq!(runner.app().featured.feed_page, 1);
        assert_eq!(runner.app().page, 7);
        runner.app_mut().featured.feed_page = page_count - 1;
        runner.action(action_id(NEXT_PAGE));
        assert_eq!(runner.app().featured.feed_page, page_count - 1);
        assert_eq!(runner.app().page, 7);
        runner.action(action_id(PREVIOUS_PAGE));
        assert_eq!(runner.app().featured.feed_page, page_count - 2);
        assert_eq!(runner.app().page, 7);
        runner.app_mut().featured.feed_page = usize::MAX;
        runner.action(action_id(PREVIOUS_PAGE));
        assert_eq!(runner.app().featured.feed_page, page_count - 2);
        assert_eq!(runner.app().page, 7);
    }

    #[test]
    fn visible_feature_covers_follow_current_blocks_and_deduplicate_only_urls() {
        let mut app = grouped_feature_app(None);
        let snapshot = app.featured.snapshot.as_mut().expect("snapshot");
        let shared = snapshot.banners[0].vertical_url.clone();
        snapshot.collections[0].comics[0]
            .vertical_url
            .clone_from(&shared);
        snapshot.collections[4].comics[0]
            .vertical_url
            .clone_from(&shared);
        let snapshot = snapshot.clone();
        let pages = feed_pages(&snapshot, &CLARA_BW_METRICS);
        let first_blocks = pages[0].blocks.clone();
        let mut expected = Vec::new();
        let mut seen = BTreeSet::new();
        let mut add = |url: Option<&String>| {
            if let Some(url) = url {
                if seen.insert(url.clone()) {
                    expected.push(url.clone());
                }
            }
        };
        for block in first_blocks {
            match block {
                feature::FeedBlock::Banners => {
                    for comic in snapshot.banners.iter().take(3) {
                        add(comic.vertical_url.as_ref().or(comic.square_url.as_ref()));
                    }
                }
                feature::FeedBlock::Collection(index)
                | feature::FeedBlock::ThemeWithHeading(index) => {
                    for comic in snapshot.collections[index].comics.iter().take(6) {
                        add(comic.vertical_url.as_ref());
                    }
                }
            }
        }
        assert_eq!(app.visible_cover_urls(), expected);
        let placements = snapshot
            .collections
            .iter()
            .flat_map(|collection| collection.comics.iter())
            .filter(|comic| comic.vertical_url == shared)
            .count();
        assert_eq!(placements, 2);
    }

    fn local_day(day: u8) -> LocalDay {
        LocalDay::new(2026, 8, day).expect("valid local day")
    }

    fn homepage_response(banners: &[&str], alias: &str) -> Vec<u8> {
        let banners = banners
            .iter()
            .map(|alias| {
                format!(
                    r#"{{"bannerDetailInfo":[{{"linkInfo":{{"target":"CONTENTS","subTarget":"COMIC","params":"{alias}"}}}}]}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"<script id="__NEXT_DATA__" type="application/json">{{"props":{{"pageProps":{{"main":{{"banners":[{banners}],"newest":[{{"alias":"{alias}","title":"Title {alias}","creators":"Creator","adult":false,"viewCount":1,"thumbnailVertical":"https://image.balcony.studio/tw/contents/{alias}.webp"}}],"weekDay":[],"onlyBom":[]}}}}}}}}</script>"#
        )
        .into_bytes()
    }

    fn collection_response(alias: &str) -> Vec<u8> {
        format!(
            r#"{{"result":"SUCCESS","data":[{{"alias":"{alias}","title":"Title {alias}","creators":"Creator","isAdult":false,"viewCount":1,"thumbnails":[{{"type":"VERTICAL_NON_ADULT","imagePath":"https://image.balcony.studio/tw/contents/{alias}.webp"}}]}}]}}"#
        )
        .into_bytes()
    }

    fn themes_response(alias: &str) -> Vec<u8> {
        format!(
            r#"{{"result":"SUCCESS","data":[{{"id":1785,"title":"Theme","contentsInfo":[{{"alias":"{alias}","title":"Title {alias}","creators":"Creator","badgeAdult":false,"viewCount":1,"thumbnails":[{{"type":"VERTICAL_NON_ADULT","imagePath":"https://image.balcony.studio/tw/contents/{alias}.webp"}}]}}]}}]}}"#
        )
        .into_bytes()
    }

    fn feature_detail_response(alias: &str, title: &str) -> Vec<u8> {
        format!(
            r#"<meta property="og:title" content="{title} - 漫畫 - BOMTOON"><meta property="og:description" content="Synopsis for {alias}">"#
        )
        .into_bytes()
    }

    fn empty_source_response(source: FeatureSource) -> Vec<u8> {
        match source {
            FeatureSource::Homepage => {
                br#"<script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"main":{"banners":[],"newest":[],"weekDay":[],"onlyBom":[]}}}}</script>"#.to_vec()
            }
            FeatureSource::Ranking
            | FeatureSource::MostFavorited
            | FeatureSource::Themes
            | FeatureSource::Freetime => br#"{"result":"SUCCESS","data":[]}"#.to_vec(),
        }
    }

    fn source_response(source: FeatureSource, prefix: &str) -> Vec<u8> {
        let suffix = match source {
            FeatureSource::Homepage => "homepage",
            FeatureSource::Ranking => "ranking",
            FeatureSource::MostFavorited => "favorite",
            FeatureSource::Themes => "theme",
            FeatureSource::Freetime => "free",
        };
        let alias = format!("{prefix}-{suffix}");
        match source {
            FeatureSource::Homepage => homepage_response(&[], &alias),
            FeatureSource::Themes => themes_response(&alias),
            FeatureSource::Ranking | FeatureSource::MostFavorited | FeatureSource::Freetime => {
                collection_response(&alias)
            }
        }
    }

    fn feature_task(runner: &AppRunner<Bomtoon>, source: FeatureSource) -> TaskId {
        runner
            .app()
            .feature_tasks
            .iter()
            .find_map(|(task, purpose)| {
                matches!(
                    purpose,
                    FeatureTaskPurpose::Source {
                        source: task_source,
                        ..
                    } if *task_source == source
                )
                .then_some(*task)
            })
            .unwrap_or_else(|| panic!("missing {source:?} task"))
    }

    fn settle_source(
        runner: &mut AppRunner<Bomtoon>,
        source: FeatureSource,
        outcome: TaskOutcome,
    ) -> Vec<Command> {
        let task = feature_task(runner, source);
        runner.task_outcome(task, outcome)
    }

    fn complete_feature_batch(
        runner: &mut AppRunner<Bomtoon>,
        prefix: &str,
        failed: Option<FeatureSource>,
    ) -> Vec<Command> {
        let mut last = Vec::new();
        for source in FEATURE_SOURCES {
            let outcome = if failed == Some(source) {
                TaskOutcome::Failed(TaskError::Offline)
            } else {
                TaskOutcome::Completed(source_response(source, prefix))
            };
            last = settle_source(runner, source, outcome);
        }
        last
    }

    fn ready_featured(day: LocalDay, prefix: &str) -> Bomtoon {
        let collection = model::FeatureCollection {
            id: "ranking".to_owned(),
            label: "排行榜".to_owned(),
            priority: 5,
            order: 0,
            comics: vec![shelf(&format!("{prefix}-ranking"), prefix)],
        };
        let featured = FeaturedState {
            generation: 1,
            snapshot: Some(FeatureSnapshot {
                banners: vec![shelf(&format!("{prefix}-banner"), prefix)],
                collections: vec![collection.clone()],
                sources: BTreeMap::from([(FeatureSource::Ranking, vec![collection])]),
                failed_sources: BTreeSet::new(),
                warning: None,
            }),
            loaded_day: Some(day),
            ..FeaturedState::default()
        };
        Bomtoon {
            account: AccountState::SignedOut,
            view: View::Main,
            destination: MainDestination::Featured,
            featured,
            ..Bomtoon::default()
        }
    }

    fn observe_day_with_runner(
        runner: &mut AppRunner<Bomtoon>,
        observed: LocalDay,
    ) -> Vec<Command> {
        let resumed = runner.resume();
        if let Some(scope) = spawns(&resumed)
            .into_iter()
            .find_map(|(task, work)| (work == api::account_scope()).then_some(task))
        {
            runner.task_outcome(scope, TaskOutcome::Failed(TaskError::NoCredential));
        }
        assert!(resumed
            .iter()
            .any(|command| matches!(command, Command::Device(DeviceRequest::ReadLocalDay))));
        runner.device_result(DeviceResult::LocalDay(Some(observed)))
    }

    #[test]
    fn late_first_local_day_labels_published_startup_without_duplicate_source_tasks() {
        let mut app = ready_featured(local_day(30), "initial");
        app.featured.loaded_day = None;
        let before = app.featured.snapshot().expect("startup snapshot").clone();
        let generation = app.featured.generation;
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);

        let commands = observe_day_with_runner(&mut runner, local_day(30));

        assert!(spawns(&commands).is_empty());
        assert!(runner.app().feature_tasks.is_empty());
        assert!(runner.app().featured.batch.is_none());
        assert_eq!(runner.app().featured.generation, generation);
        assert_eq!(runner.app().featured.loaded_day, Some(local_day(30)));
        assert_eq!(runner.app().featured.snapshot(), Some(&before));
    }

    #[test]
    fn feature_startup_fills_four_source_slots_then_starts_the_fifth_after_one_outcome() {
        let (mut runner, commands) = started();
        assert_eq!(runner.app().feature_tasks.len(), 1);
        assert!(runner.app().feature_tasks.len() <= 4);
        let scope = scope_task(&commands);

        let commands = runner.task_outcome(scope, TaskOutcome::Failed(TaskError::NoCredential));
        let protected = cancelled_tasks(&commands);
        assert_eq!(protected.len(), 2);
        for task in protected {
            runner.task_outcome(task, TaskOutcome::Cancelled);
        }
        assert_eq!(runner.app().feature_tasks.len(), 4);
        assert!(runner
            .app()
            .featured
            .batch
            .as_ref()
            .expect("batch")
            .queued
            .contains(&FeatureSource::Freetime));

        settle_source(
            &mut runner,
            FeatureSource::Homepage,
            TaskOutcome::Completed(homepage_response(&[], "home")),
        );
        assert_eq!(runner.app().feature_tasks.len(), 4);
        assert!(runner.app().feature_tasks.values().any(|purpose| matches!(
            purpose,
            FeatureTaskPurpose::Source {
                source: FeatureSource::Freetime,
                ..
            }
        )));
    }

    #[test]
    fn scheduler_released_slot_prefers_queued_foreground_over_feature_source() {
        let (mut runner, commands) = started();
        let library = fetch_task_with(&commands, "/library?").0;
        runner.app_mut().queued_foreground = Some(Pending::Recent(0));

        let commands =
            runner.task_outcome(library, TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()));

        let (_, work) = only_spawn(&commands);
        assert_eq!(work, api::recent(0));
        assert!(!spawns(&commands)
            .iter()
            .any(|(_, work)| *work == api::ranking()));
    }

    #[test]
    fn scheduler_source_outcome_prefers_deferred_wallet_over_feature_refill() {
        let (mut runner, commands) = started();
        let summary = fetch_task_with(&commands, "/asset/user").0;
        runner.task_outcome(summary, TaskOutcome::Completed(ASSET_RESPONSE.to_vec()));
        runner.app_mut().wallet.summary_refresh_queued = true;
        let homepage = feature_task(&runner, FeatureSource::Homepage);

        let commands = runner.task_outcome(
            homepage,
            TaskOutcome::Completed(homepage_response(&[], "home")),
        );

        let (_, work) = only_spawn(&commands);
        assert_eq!(work, api::asset_summary());
        assert!(!spawns(&commands)
            .iter()
            .any(|(_, work)| *work == api::most_favorited()));
    }

    #[test]
    fn scheduler_retry_enters_shared_foreground_priority() {
        let mut app = ready_featured(local_day(30), "old");
        let snapshot = app.featured.snapshot.as_mut().expect("snapshot");
        snapshot.failed_sources.insert(FeatureSource::Ranking);
        snapshot.warning = Some("Some Featured collections could not be loaded.".to_owned());
        app.queued_foreground = Some(Pending::Recent(0));
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);

        let commands = runner.action(action_id(RETRY));

        let spawned = spawns(&commands);
        assert_eq!(spawned.first().map(|(_, work)| work), Some(&api::recent(0)));
    }

    #[test]
    fn scheduler_daily_refresh_enters_shared_foreground_priority() {
        let mut app = ready_featured(local_day(30), "old");
        app.queued_foreground = Some(Pending::Recent(0));
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.resume();

        let commands = runner.device_result(DeviceResult::LocalDay(Some(local_day(31))));

        let spawned = spawns(&commands);
        assert_eq!(spawned.first().map(|(_, work)| work), Some(&api::recent(0)));
    }

    #[test]
    fn commerce_preempts_feature_sources_instead_of_failing_a_valid_quote() {
        let mut app = episode_thumbnail_app(1);
        let account_scope = app.account_scope.expect("account scope");
        let _ = app.commerce.marker_loaded(None);
        let _ = app.commerce.safety_changed(
            commerce::Authentication::Authenticated(account_scope),
            commerce::Connectivity::Online,
        );
        app.featured.begin_full_batch(None);
        let mut context = Context::default();
        assert!(app.resume_feature_capacity(&mut context));
        let feature_tasks = app.feature_tasks.keys().copied().collect::<BTreeSet<_>>();
        assert_eq!(feature_tasks.len(), 4);
        let _ = context.take_commands();

        let effects = app.commerce.begin_quote(
            commerce::Selection {
                title_id: 41,
                title_alias: "hunter_q".to_owned(),
                episode_id: 101,
                episode_alias: "episode-0".to_owned(),
            },
            model::PurchaseType::Possession,
        );
        app.apply_commerce_effects(&mut context, effects);
        let commands = context.take_commands();

        assert_eq!(cancelled_tasks(&commands), feature_tasks);
        assert!(app.queued_commerce_command.is_some());
        assert_eq!(app.commerce.state(), commerce::CommerceState::Quoting);
        assert!(app.featured.queued_source().is_some());
    }

    #[test]
    fn cancelling_commerce_resumes_a_paused_feature_batch() {
        let mut app = episode_thumbnail_app(1);
        let account_scope = app.account_scope.expect("account scope");
        let _ = app.commerce.marker_loaded(None);
        let _ = app.commerce.safety_changed(
            commerce::Authentication::Authenticated(account_scope),
            commerce::Connectivity::Online,
        );
        app.featured.begin_full_batch(None);
        let _ = app.commerce.begin_quote(
            commerce::Selection {
                title_id: 41,
                title_alias: "hunter_q".to_owned(),
                episode_id: 101,
                episode_alias: "episode-0".to_owned(),
            },
            model::PurchaseType::Possession,
        );
        let mut context = Context::default();
        app.resume_capacity_work(&mut context);
        assert!(context.take_commands().is_empty());
        let effects = app.commerce.cancel_unpersisted();

        app.apply_commerce_effects(&mut context, effects);

        assert_eq!(app.commerce.state(), commerce::CommerceState::Idle);
        assert_eq!(app.feature_tasks.len(), 4);
        assert_eq!(spawns(&context.take_commands()).len(), 4);
    }

    #[test]
    fn feature_initial_feed_remains_loading_until_sources_and_banner_details_settle() {
        let (mut runner, commands) = started();
        let scope = scope_task(&commands);
        runner.task_outcome(scope, TaskOutcome::Failed(TaskError::NoCredential));

        settle_source(
            &mut runner,
            FeatureSource::Homepage,
            TaskOutcome::Completed(homepage_response(&["unresolved"], "home")),
        );
        for source in [
            FeatureSource::Ranking,
            FeatureSource::MostFavorited,
            FeatureSource::Themes,
            FeatureSource::Freetime,
        ] {
            settle_source(
                &mut runner,
                source,
                TaskOutcome::Completed(source_response(source, "fresh")),
            );
        }
        assert!(runner.app().featured.snapshot().is_none());
        assert!(runner.app().featured.is_loading());
        let detail = runner
            .app()
            .feature_tasks
            .iter()
            .find_map(|(task, purpose)| {
                matches!(
                    purpose,
                    FeatureTaskPurpose::BannerDetail { alias, .. } if alias == "unresolved"
                )
                .then_some(*task)
            })
            .expect("banner detail");

        runner.task_outcome(
            detail,
            TaskOutcome::Completed(feature_detail_response("unresolved", "Recovered")),
        );
        assert!(runner.app().featured.snapshot().is_some());
        assert!(!runner.app().featured.is_loading());
    }

    #[test]
    fn feature_initial_total_failure_has_one_dedicated_retry_page() {
        let (mut runner, commands) = started();
        let scope = scope_task(&commands);
        runner.task_outcome(scope, TaskOutcome::Failed(TaskError::NoCredential));

        let mut commands = Vec::new();
        for source in FEATURE_SOURCES {
            commands = settle_source(&mut runner, source, TaskOutcome::Failed(TaskError::Offline));
        }

        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(drawn.contains("Featured could not be loaded."));
        assert!(!drawn.contains("Some Featured collections could not be loaded."));
        assert_eq!(retry_button_count(&screen), 1);
        assert!(screen.page_turns.is_none());
    }

    #[test]
    fn feature_source_batch_partial_failure_publishes_one_retry_action() {
        let mut runner =
            AppRunner::with_metrics(ready_featured(local_day(30), "old"), CLARA_BW_METRICS);
        observe_day_with_runner(&mut runner, local_day(31));
        let commands = complete_feature_batch(&mut runner, "fresh", Some(FeatureSource::Ranking));
        let snapshot = runner.app().featured.snapshot().expect("partial snapshot");
        assert_eq!(
            snapshot.failed_sources,
            BTreeSet::from([FeatureSource::Ranking])
        );
        assert_eq!(retry_button_count(&last_screen(&commands)), 1);
    }

    #[test]
    fn failed_source_retry_emits_only_the_failed_public_endpoint() {
        let mut runner =
            AppRunner::with_metrics(ready_featured(local_day(30), "old"), CLARA_BW_METRICS);
        observe_day_with_runner(&mut runner, local_day(31));
        complete_feature_batch(&mut runner, "fresh", Some(FeatureSource::Ranking));

        let commands = runner.action(action_id(RETRY));
        let spawned = spawns(&commands);
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0].1, api::ranking());
        assert!(matches!(
            runner.app().feature_tasks.get(&spawned[0].0),
            Some(FeatureTaskPurpose::Source {
                source: FeatureSource::Ranking,
                ..
            })
        ));
    }

    #[test]
    fn daily_refresh_keeps_old_aliases_until_the_last_new_source_result() {
        let mut runner =
            AppRunner::with_metrics(ready_featured(local_day(30), "old"), CLARA_BW_METRICS);
        let before = runner.app().featured.snapshot().expect("old").clone();
        observe_day_with_runner(&mut runner, local_day(31));
        for source in FEATURE_SOURCES.into_iter().take(4) {
            settle_source(
                &mut runner,
                source,
                TaskOutcome::Completed(source_response(source, "fresh")),
            );
            assert_eq!(runner.app().featured.snapshot(), Some(&before));
        }
        settle_source(
            &mut runner,
            FeatureSource::Freetime,
            TaskOutcome::Completed(source_response(FeatureSource::Freetime, "fresh")),
        );
        assert_ne!(runner.app().featured.snapshot(), Some(&before));
    }

    #[test]
    fn daily_refresh_total_failure_keeps_old_feed_with_one_retry_action() {
        let mut runner =
            AppRunner::with_metrics(ready_featured(local_day(30), "old"), CLARA_BW_METRICS);
        let before = runner.app().featured.snapshot().expect("old").clone();
        observe_day_with_runner(&mut runner, local_day(31));

        let mut commands = Vec::new();
        for source in FEATURE_SOURCES {
            commands = settle_source(&mut runner, source, TaskOutcome::Failed(TaskError::Offline));
        }

        assert_eq!(runner.app().featured.snapshot(), Some(&before));
        assert!(runner
            .app()
            .featured
            .batch
            .as_ref()
            .is_some_and(feature::FeatureBatch::settled));
        assert_eq!(retry_button_count(&last_screen(&commands)), 1);
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(drawn.contains("Some Featured collections could not be loaded."));
        assert!(!drawn.contains("Featured could not be loaded."));
        assert_eq!(
            screen.page_turns.as_ref().and_then(|turns| turns.position),
            Some((1, 1))
        );
        assert!(screen
            .nodes
            .iter()
            .any(|node| { matches!(node, Node::ImageStrip { .. } | Node::MediaGrid { .. }) }));
    }

    #[test]
    fn newer_local_day_starts_after_the_older_atomic_refresh_settles() {
        let mut runner =
            AppRunner::with_metrics(ready_featured(local_day(29), "old"), CLARA_BW_METRICS);
        observe_day_with_runner(&mut runner, local_day(30));
        observe_day_with_runner(&mut runner, local_day(31));
        assert_eq!(runner.app().featured.desired_day, Some(local_day(31)));

        let commands = complete_feature_batch(&mut runner, "day30", None);
        assert_eq!(runner.app().featured.loaded_day, Some(local_day(30)));
        assert_eq!(
            runner
                .app()
                .featured
                .batch
                .as_ref()
                .and_then(|batch| batch.refresh_day),
            Some(local_day(31))
        );
        assert!(spawns(&commands)
            .iter()
            .any(|(_, work)| *work == api::homepage()));
    }

    #[test]
    fn feature_exit_cancellation_and_stale_outcomes_never_publish() {
        let mut runner =
            AppRunner::with_metrics(ready_featured(local_day(30), "old"), CLARA_BW_METRICS);
        observe_day_with_runner(&mut runner, local_day(31));
        let stale = feature_task(&runner, FeatureSource::Homepage);
        let cancelled = runner.exit();
        assert!(cancelled.contains(&Command::Cancel(stale)));
        assert!(runner.app().featured.snapshot().is_none());

        let commands = runner.task_outcome(
            stale,
            TaskOutcome::Completed(homepage_response(&[], "stale")),
        );
        assert!(commands.is_empty());
        assert!(runner.app().featured.snapshot().is_none());
    }

    #[test]
    fn signed_out_feature_sources_never_emit_credentials() {
        let (mut runner, commands) = started();
        let scope = scope_task(&commands);
        let mut command_batches = vec![commands];
        command_batches
            .push(runner.task_outcome(scope, TaskOutcome::Failed(TaskError::NoCredential)));
        for source in FEATURE_SOURCES {
            command_batches.push(settle_source(
                &mut runner,
                source,
                TaskOutcome::Completed(source_response(source, "public")),
            ));
        }
        let feature_requests = command_batches
            .iter()
            .flat_map(|commands| spawns(commands))
            .filter(|(_, work)| {
                [
                    api::homepage(),
                    api::ranking(),
                    api::most_favorited(),
                    api::themes(),
                    api::freetime(),
                ]
                .contains(work)
            })
            .collect::<Vec<_>>();
        assert_eq!(feature_requests.len(), FEATURE_SOURCES.len());
        for (_, task) in feature_requests {
            let Task::Fetch { credential, .. } = task else {
                panic!("Feature source was not a fetch");
            };
            assert_eq!(credential, None);
        }
    }

    fn shelf_cover_url(index: usize) -> String {
        format!("https://image.balcony.studio/tw/co_thumbnail/cover-{index}.webp")
    }

    fn recent_shelf_entry(index: usize, cover_url: Option<String>) -> RecentEntry {
        RecentEntry {
            content_alias: format!("recent-{index}"),
            content_title: format!("Recent {index}"),
            creators: format!("Creators {index}"),
            cover_url,
            episode_alias: format!("episode-{index}"),
            episode_title: format!("Episode title {index}"),
        }
    }

    fn library_shelf_comic(index: usize, cover_url: Option<String>) -> Comic {
        Comic {
            alias: format!("library-{index}"),
            title: format!("Library {index}"),
            creators: format!("Creators {index}"),
            cover_url,
            owned_episodes: index + 1,
            total_episodes: index + 7,
        }
    }

    fn recent_cover_runner(count: usize) -> AppRunner<Bomtoon> {
        AppRunner::with_metrics(
            Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                destination: MainDestination::Featured,
                recent: (0..count)
                    .map(|index| recent_shelf_entry(index, Some(shelf_cover_url(index))))
                    .collect(),
                recent_load: loaded_shelf(),
                total_recent_titles: count,
                ..Bomtoon::default()
            },
            CLARA_BW_METRICS,
        )
    }

    fn library_cover_runner(count: usize) -> AppRunner<Bomtoon> {
        AppRunner::with_metrics(
            Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                destination: MainDestination::Featured,
                comics: (0..count)
                    .map(|index| library_shelf_comic(index, Some(shelf_cover_url(index))))
                    .collect(),
                library_load: loaded_shelf(),
                total_library_titles: count,
                ..Bomtoon::default()
            },
            CLARA_BW_METRICS,
        )
    }

    #[test]
    fn recent_row_shows_cover_title_creators_and_episode() {
        let url = shelf_cover_url(0);
        let picture = TilePicture::new(PictureHandle(7), 60, 60);
        let mut recent = recent_shelf_entry(0, Some(url.clone()));
        recent.content_title = "近似嚮導".to_owned();
        let app = Bomtoon {
            account: AccountState::Active,
            view: View::Main,
            destination: MainDestination::Recent,
            recent: vec![recent],
            recent_load: loaded_shelf(),
            total_recent_titles: 1,
            covers: CoverCache {
                entries: BTreeMap::from([(url, CoverState::Ready(picture))]),
                ..CoverCache::default()
            },
            ..Bomtoon::default()
        };

        let screen = app.main_screen();
        let row = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => rows.first(),
                _ => None,
            })
            .expect("recent row");
        assert_eq!(row.title, "近似嚮導");
        assert_eq!(row.summary, "Creators 0");
        assert_eq!(row.trailing.as_deref(), Some("Episode title 0"));
        assert!(matches!(
            row.lead,
            RowLead::Picture(lead, Glyph::Book) if lead == picture
        ));
        assert_fits(&screen);
    }

    #[test]
    fn library_row_shows_cover_title_creators_and_owned_total_count() {
        let url = shelf_cover_url(0);
        let picture = TilePicture::new(PictureHandle(8), 60, 60);
        let mut comic = library_shelf_comic(0, Some(url.clone()));
        comic.title = "戀愛漫畫".to_owned();
        let app = Bomtoon {
            account: AccountState::Active,
            view: View::Main,
            destination: MainDestination::Library,
            comics: vec![comic],
            library_load: loaded_shelf(),
            total_library_titles: 1,
            covers: CoverCache {
                entries: BTreeMap::from([(url, CoverState::Ready(picture))]),
                ..CoverCache::default()
            },
            ..Bomtoon::default()
        };

        let screen = app.main_screen();
        let row = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => rows.first(),
                _ => None,
            })
            .expect("library row");
        assert_eq!(row.title, "戀愛漫畫");
        assert_eq!(row.summary, "Creators 0");
        assert_eq!(row.trailing.as_deref(), Some("1 / 7"));
        assert!(matches!(
            row.lead,
            RowLead::Picture(lead, Glyph::Book) if lead == picture
        ));
        assert_fits(&screen);
    }

    fn four_cover_task_ids(commands: &[Command]) -> Vec<TaskId> {
        let tasks = cover_fetches(commands)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<Vec<_>>();
        assert_eq!(tasks.len(), 4);
        tasks
    }

    fn cover_fetches(commands: &[Command]) -> Vec<(TaskId, String)> {
        spawns(commands)
            .into_iter()
            .filter_map(|(task, work)| match work {
                Task::Fetch { url, .. } if url.starts_with("https://image.balcony.studio/tw/") => {
                    Some((task, url))
                }
                _ => None,
            })
            .collect()
    }

    fn cancelled_tasks(commands: &[Command]) -> BTreeSet<TaskId> {
        commands
            .iter()
            .filter_map(|command| match command {
                Command::Cancel(task) => Some(*task),
                _ => None,
            })
            .collect()
    }

    fn episode_thumbnail_url(index: usize) -> String {
        format!("https://image.balcony.studio/tw/ep_thumbnail/episode-{index}.webp")
    }

    fn thumbnail_episode(index: usize, thumbnail_url: String) -> Episode {
        Episode {
            id: 1_000 + index,
            alias: format!("thumbnail-{index}"),
            title: format!("Thumbnail episode {index}"),
            opened_at: 1_709_136_000_000 + i64::try_from(index).expect("small index") * 1_000,
            thumbnail_url: Some(thumbnail_url),
            purchase: model::PurchaseState::Owned,
            rent_expires_at: None,
            rent_coin: None,
            purchase_coin: None,
            gift_eligible: false,
        }
    }

    fn episode_thumbnail_app(count: usize) -> Bomtoon {
        let mut app = episode_metadata_app("Synopsis".to_owned());
        app.episodes = (0..count)
            .map(|index| thumbnail_episode(index, episode_thumbnail_url(index)))
            .collect();
        app.episode_items_per_page = None;
        app.prepare_episode_layout();
        app
    }

    fn episode_thumbnail_runner(count: usize) -> AppRunner<Bomtoon> {
        AppRunner::with_metrics(episode_thumbnail_app(count), CLARA_BW_METRICS)
    }

    fn first_episode_lead(screen: &Screen) -> RowLead {
        screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => rows.first().map(|row| row.lead),
                _ => None,
            })
            .expect("episode row")
    }

    fn put_picture_handles(commands: &[Command]) -> BTreeSet<PictureHandle> {
        commands
            .iter()
            .filter_map(|command| match command {
                Command::PutPicture { handle, .. } => Some(*handle),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn episode_thumbnails_request_only_the_visible_page() {
        let mut runner = episode_thumbnail_runner(12);
        let rows = runner.app().episode_rows_per_page();
        assert!(runner.app().episodes.len() > rows);
        let first_page = (0..rows)
            .map(episode_thumbnail_url)
            .collect::<BTreeSet<_>>();
        let second_page = (rows..rows.saturating_mul(2).min(12))
            .map(episode_thumbnail_url)
            .collect::<BTreeSet<_>>();

        let commands = runner.action(action_id("refresh-layout"));
        let fetches = cover_fetches(&commands);
        let requested = fetches
            .iter()
            .map(|(_, url)| url.clone())
            .collect::<BTreeSet<_>>();
        assert!(!requested.is_empty());
        assert!(requested.is_subset(&first_page));
        assert!(requested.is_disjoint(&second_page));
        assert!(fetches.len() <= 4);
        assert_eq!(
            first_episode_lead(&last_screen(&commands)),
            RowLead::Icon(Glyph::Book)
        );

        let (ready_task, ready_url) = fetches[0].clone();
        let commands = runner.task_outcome(ready_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        assert_eq!(put_picture_handles(&commands).len(), 1);
        assert!(matches!(
            first_episode_lead(&last_screen(&commands)),
            RowLead::Picture(_, Glyph::Book)
        ));
        let ready_picture =
            ready_cover(&runner.app().covers, Some(&ready_url)).expect("visible thumbnail picture");
        let stale = runner
            .app()
            .covers
            .tasks
            .iter()
            .filter(|(_, cover)| first_page.contains(&cover.url))
            .map(|(task, cover)| (*task, cover.url.clone()))
            .collect::<Vec<_>>();
        assert!(stale.len() >= 2, "two stale outcomes must be exercised");

        let commands = runner.action(action_id(NEXT_PAGE));
        let cancelled = cancelled_tasks(&commands);
        assert_eq!(runner.app().page, 1);
        assert!(stale.iter().all(|(task, _)| cancelled.contains(task)));
        assert!(commands.contains(&Command::DropPicture(ready_picture.handle)));
        assert!(cover_fetches(&commands)
            .iter()
            .all(|(_, url)| second_page.contains(url)));

        for (index, (task, url)) in stale.into_iter().take(2).enumerate() {
            let outcome = if index == 0 {
                TaskOutcome::Completed(TINY_WEBP.to_vec())
            } else {
                TaskOutcome::Failed(TaskError::TimedOut)
            };
            let commands = runner.task_outcome(task, outcome);
            assert!(put_picture_handles(&commands).is_empty());
            assert!(runner.app().problem.is_none());
            assert!(!runner.app().covers.entries.contains_key(&url));
            assert!(cover_fetches(&commands)
                .iter()
                .all(|(_, url)| second_page.contains(url)));
        }
        assert!(!runner.app().covers.entries.contains_key(&ready_url));
    }

    #[test]
    fn repeated_episode_thumbnail_url_remains_owned_across_pages() {
        let mut app = episode_thumbnail_app(12);
        let rows = app.episode_rows_per_page();
        let shared = episode_thumbnail_url(0);
        app.episodes[rows].thumbnail_url = Some(shared.clone());
        app.episode_items_per_page = None;
        app.prepare_episode_layout();
        let rows = app.episode_rows_per_page();
        app.episodes[rows].thumbnail_url = Some(shared.clone());
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);

        let commands = runner.action(action_id("refresh-layout"));
        let shared_task = cover_fetches(&commands)
            .into_iter()
            .find_map(|(task, url)| (url == shared).then_some(task))
            .expect("shared visible thumbnail task");
        let commands = runner.action(action_id(NEXT_PAGE));
        assert!(!cancelled_tasks(&commands).contains(&shared_task));

        let commands = runner.task_outcome(shared_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        assert_eq!(put_picture_handles(&commands).len(), 1);
        assert!(matches!(
            first_episode_lead(&last_screen(&commands)),
            RowLead::Picture(_, Glyph::Book)
        ));
    }

    #[test]
    fn episode_thumbnail_failure_keeps_glyph_and_commerce_online_without_page_error() {
        let mut app = episode_thumbnail_app(1);
        app.episodes[0].purchase = model::PurchaseState::NotOwned;
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        let commands = runner.action(action_id("refresh-layout"));
        let thumbnail = cover_fetches(&commands)[0].0;

        let commands = runner.task_outcome(thumbnail, TaskOutcome::Failed(TaskError::Offline));

        assert!(put_picture_handles(&commands).is_empty());
        assert!(runner.app().problem.is_none());
        assert_eq!(runner.app().connection, ConnectionState::Online);
        assert_eq!(
            first_episode_lead(&runner.app().screen()),
            RowLead::Icon(Glyph::Book)
        );
        assert!(cover_fetches(&runner.action(action_id("refresh-layout"))).is_empty());
    }

    fn cached_episode_thumbnail_runner(
    ) -> (AppRunner<Bomtoon>, String, TilePicture, String, TilePicture) {
        let shared = episode_thumbnail_url(0);
        let episode_only = episode_thumbnail_url(1);
        let shared_picture = TilePicture::new(PictureHandle(301), 1, 1);
        let episode_picture = TilePicture::new(PictureHandle(302), 1, 1);
        let mut app = episode_thumbnail_app(2);
        app.episodes[0].thumbnail_url = Some(shared.clone());
        app.episodes[1].thumbnail_url = Some(episode_only.clone());
        app.featured = featured_with_public_covers(std::slice::from_ref(&shared));
        app.covers.entries = BTreeMap::from([
            (shared.clone(), CoverState::Ready(shared_picture)),
            (episode_only.clone(), CoverState::Ready(episode_picture)),
        ]);
        app.covers.visible_urls = vec![shared.clone(), episode_only.clone()];
        app.covers.visible_source = Some(CoverSource::Protected);
        (
            AppRunner::with_metrics(app, CLARA_BW_METRICS),
            shared,
            shared_picture,
            episode_only,
            episode_picture,
        )
    }

    #[test]
    fn leaving_episode_view_drops_episode_only_pictures_but_preserves_public_shelf_covers() {
        let (mut reader, shared, shared_picture, episode_only, episode_picture) =
            cached_episode_thumbnail_runner();
        let commands = reader.action(action_id("episode-0"));
        assert_eq!(reader.app().view, View::Reader);
        assert_eq!(
            reader.app().covers.entries.get(&shared),
            Some(&CoverState::Ready(shared_picture))
        );
        assert!(!reader.app().covers.entries.contains_key(&episode_only));
        assert!(!commands.contains(&Command::DropPicture(shared_picture.handle)));
        assert!(commands.contains(&Command::DropPicture(episode_picture.handle)));

        let (mut main, shared, shared_picture, episode_only, episode_picture) =
            cached_episode_thumbnail_runner();
        let commands = main.action(ActionId::BACK);
        assert_eq!(main.app().view, View::Main);
        assert_eq!(
            main.app().covers.entries.get(&shared),
            Some(&CoverState::Ready(shared_picture))
        );
        assert!(!main.app().covers.entries.contains_key(&episode_only));
        assert!(!commands.contains(&Command::DropPicture(shared_picture.handle)));
        assert!(commands.contains(&Command::DropPicture(episode_picture.handle)));

        let (mut replacement, shared, shared_picture, episode_only, episode_picture) =
            cached_episode_thumbnail_runner();
        replacement.app_mut().view = View::Main;
        let commands = replacement.action(action_id("feature-banner-0"));
        assert_eq!(replacement.app().pending, Some(Pending::Content(0)));
        assert_eq!(
            replacement.app().covers.entries.get(&shared),
            Some(&CoverState::Ready(shared_picture))
        );
        assert!(!replacement.app().covers.entries.contains_key(&episode_only));
        assert!(!commands.contains(&Command::DropPicture(shared_picture.handle)));
        assert!(commands.contains(&Command::DropPicture(episode_picture.handle)));

        let mut eviction = episode_thumbnail_app(12);
        let shared = episode_thumbnail_url(0);
        let shared_picture = TilePicture::new(PictureHandle(303), 1, 1);
        eviction.featured = featured_with_public_covers(std::slice::from_ref(&shared));
        eviction.covers.entries =
            BTreeMap::from([(shared.clone(), CoverState::Ready(shared_picture))]);
        eviction.covers.visible_urls = eviction.visible_cover_urls();
        eviction.covers.visible_source = Some(CoverSource::Protected);
        let mut eviction = AppRunner::with_metrics(eviction, CLARA_BW_METRICS);
        let commands = eviction.action(action_id(NEXT_PAGE));
        assert_eq!(
            eviction.app().covers.entries.get(&shared),
            Some(&CoverState::Ready(shared_picture))
        );
        assert!(!commands.contains(&Command::DropPicture(shared_picture.handle)));
    }

    fn seed_episode_thumbnail_cache(
        app: &mut Bomtoon,
        loading_task: TaskId,
        ready_picture: TilePicture,
    ) -> (String, String) {
        let loading_url = episode_thumbnail_url(0);
        let ready_url = episode_thumbnail_url(1);
        app.covers.generation = 17;
        app.covers.visible_urls = vec![loading_url.clone(), ready_url.clone()];
        app.covers.visible_source = Some(CoverSource::Protected);
        app.covers.entries = BTreeMap::from([
            (loading_url.clone(), CoverState::Loading(loading_task)),
            (ready_url.clone(), CoverState::Ready(ready_picture)),
        ]);
        app.covers.tasks = BTreeMap::from([(
            loading_task,
            CoverTask {
                generation: 17,
                url: loading_url.clone(),
                source: CoverSource::Protected,
            },
        )]);
        (loading_url, ready_url)
    }

    fn assert_episode_thumbnail_cache_cleared(app: &Bomtoon, loading_url: &str, ready_url: &str) {
        assert!(!app.covers.entries.contains_key(loading_url));
        assert!(!app.covers.entries.contains_key(ready_url));
        assert!(app.covers.tasks.is_empty());
        assert!(!app.covers.visible_urls.contains(&loading_url.to_owned()));
        assert!(!app.covers.visible_urls.contains(&ready_url.to_owned()));
    }

    #[test]
    fn protected_episode_thumbnails_clear_on_suspend_credential_loss_sign_out_and_exit() {
        let loading = TaskId(801);
        let picture = TilePicture::new(PictureHandle(401), 1, 1);
        let mut suspend = episode_thumbnail_runner(2);
        let (loading_url, ready_url) =
            seed_episode_thumbnail_cache(suspend.app_mut(), loading, picture);
        let commands = suspend.suspend();
        assert!(commands.contains(&Command::Cancel(loading)));
        assert!(commands.contains(&Command::DropPicture(picture.handle)));
        assert_episode_thumbnail_cache_cleared(suspend.app(), &loading_url, &ready_url);

        let loading = TaskId(802);
        let picture = TilePicture::new(PictureHandle(402), 1, 1);
        let mut credential = episode_thumbnail_runner(2);
        let (loading_url, ready_url) =
            seed_episode_thumbnail_cache(credential.app_mut(), loading, picture);
        let scope = TaskId(803);
        credential.app_mut().scope_task = Some(scope);
        let commands = credential.task_outcome(scope, TaskOutcome::Failed(TaskError::NoCredential));
        assert!(commands.contains(&Command::Cancel(loading)));
        assert!(commands.contains(&Command::DropPicture(picture.handle)));
        assert_episode_thumbnail_cache_cleared(credential.app(), &loading_url, &ready_url);

        let loading = TaskId(804);
        let picture = TilePicture::new(PictureHandle(404), 1, 1);
        let mut sign_out = episode_thumbnail_runner(2);
        sign_out.app_mut().view = View::Account;
        let (loading_url, ready_url) =
            seed_episode_thumbnail_cache(sign_out.app_mut(), loading, picture);
        let commands = sign_out.action(action_id(SIGN_OUT));
        let (logout, work) = only_spawn(&commands);
        assert_eq!(work, api::logout());
        let commands = sign_out.task_outcome(logout, TaskOutcome::Completed(Vec::new()));
        assert!(commands.contains(&Command::Cancel(loading)));
        assert!(commands.contains(&Command::DropPicture(picture.handle)));
        assert_episode_thumbnail_cache_cleared(sign_out.app(), &loading_url, &ready_url);

        let loading = TaskId(805);
        let picture = TilePicture::new(PictureHandle(405), 1, 1);
        let mut exit = episode_thumbnail_runner(2);
        let (loading_url, ready_url) =
            seed_episode_thumbnail_cache(exit.app_mut(), loading, picture);
        let commands = exit.exit();
        assert!(commands.contains(&Command::Cancel(loading)));
        assert!(commands.contains(&Command::DropPicture(picture.handle)));
        assert_episode_thumbnail_cache_cleared(exit.app(), &loading_url, &ready_url);
    }

    #[test]
    fn reader_and_content_foreground_work_preempt_episode_thumbnails() {
        let mut reader = episode_thumbnail_runner(12);
        let commands = reader.action(action_id("refresh-layout"));
        let reader_covers = cover_fetches(&commands)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        assert!(!reader_covers.is_empty());
        assert_eq!(reader_covers.len(), 4);
        let commands = reader.action(action_id("episode-0"));
        assert_eq!(cancelled_tasks(&commands), reader_covers);
        assert!(cover_fetches(&commands).is_empty());
        let mut manifest = spawns(&commands)
            .into_iter()
            .find(|(_, work)| *work == api::images("hunter_q", "thumbnail-0", 1072));
        for task in reader_covers.iter().copied() {
            if manifest.is_some() {
                break;
            }
            let commands = reader.task_outcome(task, TaskOutcome::Cancelled);
            assert!(cover_fetches(&commands).is_empty());
            manifest = spawns(&commands)
                .into_iter()
                .find(|(_, work)| *work == api::images("hunter_q", "thumbnail-0", 1072));
        }
        assert!(manifest.is_some(), "reader manifest did not resume first");

        let mut content = episode_thumbnail_runner(12);
        content.app_mut().episodes[0].purchase = model::PurchaseState::Rented;
        content.app_mut().episodes[0].rent_expires_at = Some(1);
        let commands = content.action(action_id("refresh-layout"));
        let content_covers = cover_fetches(&commands)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        assert_eq!(content_covers.len(), 4);

        let commands = content.action(action_id("episode-0"));
        assert_eq!(cancelled_tasks(&commands), content_covers);
        assert!(cover_fetches(&commands).is_empty());
        let mut detail = spawns(&commands)
            .into_iter()
            .find(|(_, work)| *work == api::detail("hunter_q"));
        for task in content_covers.iter().copied() {
            if detail.is_none() {
                let commands = content.task_outcome(task, TaskOutcome::Cancelled);
                assert!(cover_fetches(&commands).is_empty());
                detail = spawns(&commands)
                    .into_iter()
                    .find(|(_, work)| *work == api::detail("hunter_q"));
            } else {
                let commands = content.task_outcome(task, TaskOutcome::Cancelled);
                assert!(cover_fetches(&commands).is_empty());
            }
        }
        let (detail, _) = detail.expect("content refresh did not resume first");
        let refreshed_thumbnail = episode_thumbnail_url(50);
        let refreshed_episode = format!(
            concat!(
                "{{\"id\":105,\"alias\":\"thumbnail-0\",\"title\":\"Thumbnail episode 0\",",
                "\"openedAt\":1709136000000,\"isSample\":false,",
                "\"purchaseStatus\":\"POSSESSION\",",
                "\"thumbnails\":[{{\"type\":\"COMMON\",\"imagePath\":\"{}\"}}]}}"
            ),
            refreshed_thumbnail
        );
        let commands = content.task_outcome(
            detail,
            TaskOutcome::Completed(detail_response(
                41,
                "hunter_q",
                "Hunter Q",
                &refreshed_episode,
            )),
        );
        assert!(cover_fetches(&commands)
            .iter()
            .any(|(_, url)| url == &refreshed_thumbnail));
    }

    #[test]
    fn commerce_foreground_preempts_episode_thumbnails_and_resumes_after_cancellation() {
        let mut app = episode_thumbnail_app(12);
        app.episodes[0].purchase = model::PurchaseState::NotOwned;
        app.episodes[0].purchase_coin = Some(7);
        let account_scope = app.account_scope.expect("account scope");
        let _ = app.commerce.marker_loaded(None);
        let _ = app.commerce.safety_changed(
            commerce::Authentication::Authenticated(account_scope),
            commerce::Connectivity::Online,
        );
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        let commands = runner.action(action_id("refresh-layout"));
        let covers = cover_fetches(&commands)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        assert_eq!(covers.len(), 4);

        let commands = runner.action(action_id("episode-0"));
        assert_eq!(cancelled_tasks(&commands), covers);
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::Quoting
        );
        assert!(cover_fetches(&commands).is_empty());
        let quote_work = api::quote("hunter_q", "thumbnail-0", model::PurchaseType::Possession);
        let mut quote = spawns(&commands)
            .into_iter()
            .find(|(_, work)| *work == quote_work);
        for task in covers {
            let commands = runner.task_outcome(task, TaskOutcome::Cancelled);
            assert!(cover_fetches(&commands).is_empty());
            if quote.is_none() {
                quote = spawns(&commands)
                    .into_iter()
                    .find(|(_, work)| *work == quote_work);
            }
        }
        assert!(quote.is_some(), "quote did not resume before thumbnails");
    }

    fn add_public_episode_covers(app: &mut Bomtoon, urls: &[String]) {
        app.featured = featured_with_public_covers(urls);
    }

    #[test]
    fn shared_public_loading_episode_tasks_yield_to_reader_and_next_page() {
        let mut reader_app = episode_thumbnail_app(12);
        let public_urls = (0..4).map(episode_thumbnail_url).collect::<Vec<_>>();
        add_public_episode_covers(&mut reader_app, &public_urls);
        let mut reader = AppRunner::with_metrics(reader_app, CLARA_BW_METRICS);
        let commands = reader.action(action_id("refresh-layout"));
        let reader_thumbnails = cover_fetches(&commands)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        assert_eq!(reader_thumbnails.len(), 4);

        let commands = reader.action(action_id("episode-0"));
        assert_eq!(cancelled_tasks(&commands), reader_thumbnails);
        assert!(cover_fetches(&commands).is_empty());
        let commands = reader.task_outcome(
            *reader_thumbnails.iter().next().expect("thumbnail task"),
            TaskOutcome::Cancelled,
        );
        assert!(spawns(&commands)
            .iter()
            .any(|(_, work)| *work == api::images("hunter_q", "thumbnail-0", 1072)));
        assert!(cover_fetches(&commands).is_empty());

        let mut page_app = episode_thumbnail_app(12);
        let rows = page_app.episode_rows_per_page();
        let public_urls = (0..4).map(episode_thumbnail_url).collect::<Vec<_>>();
        add_public_episode_covers(&mut page_app, &public_urls);
        let second_page_urls = (rows..rows.saturating_mul(2).min(page_app.episodes.len()))
            .map(episode_thumbnail_url)
            .collect::<BTreeSet<_>>();
        let mut page = AppRunner::with_metrics(page_app, CLARA_BW_METRICS);
        let commands = page.action(action_id("refresh-layout"));
        let first_page_tasks = cover_fetches(&commands)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        assert_eq!(first_page_tasks.len(), 4);

        let commands = page.action(action_id(NEXT_PAGE));
        assert_eq!(cancelled_tasks(&commands), first_page_tasks);
        assert!(cover_fetches(&commands).is_empty());
        let first_cancelled = *first_page_tasks.iter().next().expect("thumbnail task");
        let commands = page.task_outcome(first_cancelled, TaskOutcome::Cancelled);
        let resumed = cover_fetches(&commands);
        assert_eq!(resumed.len(), 1);
        assert!(second_page_urls.contains(&resumed[0].1));
        assert!(!public_urls.contains(&resumed[0].1));
        assert_eq!(
            page.app()
                .covers
                .tasks
                .get(&resumed[0].0)
                .map(|task| task.source),
            Some(CoverSource::Protected)
        );
        let mut second_page_tasks = BTreeSet::from([resumed[0].0]);
        for task in first_page_tasks
            .iter()
            .copied()
            .filter(|task| *task != first_cancelled)
        {
            let commands = page.task_outcome(task, TaskOutcome::Cancelled);
            let resumed = cover_fetches(&commands);
            assert_eq!(resumed.len(), 1);
            assert!(second_page_urls.contains(&resumed[0].1));
            assert!(!public_urls.contains(&resumed[0].1));
            assert_eq!(
                page.app()
                    .covers
                    .tasks
                    .get(&resumed[0].0)
                    .map(|cover| cover.source),
                Some(CoverSource::Protected)
            );
            second_page_tasks.insert(resumed[0].0);
        }
        assert_eq!(second_page_tasks.len(), 4);

        let commands = page.action(ActionId::BACK);
        assert_eq!(cancelled_tasks(&commands), second_page_tasks);
        assert!(cover_fetches(&commands).is_empty());
        let commands = page.task_outcome(
            *second_page_tasks.iter().next().expect("second-page task"),
            TaskOutcome::Cancelled,
        );
        let shelf = cover_fetches(&commands);
        assert_eq!(shelf.len(), 1);
        assert!(public_urls.contains(&shelf[0].1));
        assert_eq!(
            page.app()
                .covers
                .tasks
                .get(&shelf[0].0)
                .map(|cover| cover.source),
            Some(CoverSource::Public)
        );
    }

    #[test]
    fn rejected_shared_public_outcome_removes_loading_entry_and_retries_when_visible() {
        let shared = episode_thumbnail_url(0);
        let stale = TaskId(901);
        let mut app = episode_thumbnail_app(1);
        app.view = View::Main;
        add_public_episode_covers(&mut app, std::slice::from_ref(&shared));
        app.covers = CoverCache {
            generation: 23,
            entries: BTreeMap::from([(shared.clone(), CoverState::Loading(stale))]),
            tasks: BTreeMap::from([(
                stale,
                CoverTask {
                    generation: 23,
                    url: shared.clone(),
                    source: CoverSource::Public,
                },
            )]),
            visible_urls: vec![shared.clone()],
            visible_source: Some(CoverSource::Public),
        };
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.app_mut().featured.snapshot = None;

        let commands = runner.task_outcome(stale, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        assert!(put_picture_handles(&commands).is_empty());
        assert!(cover_fetches(&commands).is_empty());
        assert!(!runner.app().covers.entries.contains_key(&shared));

        runner.app_mut().view = View::Episodes;
        let commands = runner.action(action_id("refresh-layout"));
        let retry = cover_fetches(&commands);
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].1, shared);
        assert_eq!(
            runner
                .app()
                .covers
                .tasks
                .get(&retry[0].0)
                .map(|task| task.source),
            Some(CoverSource::Protected)
        );
    }

    #[test]
    fn four_active_covers_preempt_for_recent_and_resume_once_after_cancel_settles() {
        let mut runner = library_cover_runner(6);
        let covers = four_cover_task_ids(&runner.action(action_id(LIBRARY)));

        let queued = runner.action(action_id(RECENT));
        assert!(spawns(&queued).is_empty());
        assert_eq!(
            cancelled_tasks(&queued),
            covers.iter().copied().collect::<BTreeSet<_>>()
        );
        assert_eq!(runner.app().queued_foreground, Some(Pending::Recent(0)));
        assert!(runner.app().problem.is_none());

        let resumed = runner.task_outcome(covers[0], TaskOutcome::Cancelled);
        let (recent, work) = only_spawn(&resumed);
        assert_eq!(work, api::recent(0));
        assert_eq!(runner.app().pending, Some(Pending::Recent(0)));
        assert_eq!(runner.app().queued_foreground, None);

        for cover in covers.into_iter().skip(1) {
            let commands = runner.task_outcome(cover, TaskOutcome::Cancelled);
            assert!(spawns(&commands)
                .iter()
                .all(|(_, work)| *work != api::recent(0)));
            assert_eq!(runner.app().task, Some(recent));
        }

        runner.task_outcome(recent, TaskOutcome::Completed(RECENT_RESPONSE.to_vec()));
        assert!(runner.app().recent_load.loaded);
        assert_eq!(runner.app().destination, MainDestination::Recent);
        assert!(runner.app().problem.is_none());
    }

    #[test]
    fn four_active_covers_preempt_for_comic_and_do_not_duplicate_queued_content() {
        let mut runner = recent_cover_runner(6);
        let covers = four_cover_task_ids(&runner.action(action_id(RECENT)));

        let queued = runner.action(action_id("comic-0"));
        assert!(spawns(&queued).is_empty());
        assert_eq!(
            cancelled_tasks(&queued),
            covers.iter().copied().collect::<BTreeSet<_>>()
        );
        assert_eq!(runner.app().queued_foreground, Some(Pending::Content(0)));
        assert!(runner.app().problem.is_none());

        let resumed = runner.task_outcome(covers[0], TaskOutcome::Cancelled);
        let (content, work) = only_spawn(&resumed);
        assert_eq!(work, api::detail("recent-0"));

        for cover in covers.into_iter().skip(1) {
            let commands = runner.task_outcome(cover, TaskOutcome::Cancelled);
            assert!(spawns(&commands)
                .iter()
                .all(|(_, work)| *work != api::detail("recent-0")));
            assert_eq!(runner.app().task, Some(content));
        }

        runner.task_outcome(
            content,
            TaskOutcome::Completed(detail_response(
                41,
                "recent-0",
                "Recent Zero",
                "{\"id\":101,\"alias\":\"ep-1\",\"title\":\"Episode One\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"POSSESSION\"}",
            )),
        );
        assert_eq!(runner.app().view, View::Episodes);
        assert!(runner.app().problem.is_none());
    }

    #[test]
    fn four_active_covers_preempt_for_logout_and_resume_before_wallet_refills() {
        let mut runner = recent_cover_runner(6);
        let covers = four_cover_task_ids(&runner.action(action_id(RECENT)));
        runner.action(action_id(ACCOUNT));
        assert_eq!(runner.app().view, View::Account);

        let queued = runner.action(action_id(SIGN_OUT));
        assert!(spawns(&queued).is_empty());
        assert_eq!(
            cancelled_tasks(&queued),
            covers.iter().copied().collect::<BTreeSet<_>>()
        );
        assert_eq!(runner.app().queued_foreground, Some(Pending::Logout));
        assert!(runner.app().problem.is_none());

        let resumed = runner.task_outcome(covers[0], TaskOutcome::Cancelled);
        let (logout, work) = only_spawn(&resumed);
        assert_eq!(work, api::logout());

        for cover in covers.into_iter().skip(1) {
            let commands = runner.task_outcome(cover, TaskOutcome::Cancelled);
            assert!(spawns(&commands)
                .iter()
                .all(|(_, work)| *work != api::logout()));
            assert_eq!(runner.app().task, Some(logout));
        }

        runner.task_outcome(logout, TaskOutcome::Completed(Vec::new()));
        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_eq!(runner.app().destination, MainDestination::Featured);
    }

    #[test]
    fn coin_account_action_remains_reachable_on_both_compact_shelves() {
        for destination in [MainDestination::Recent, MainDestination::Library] {
            let app = Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                destination,
                recent: vec![recent_shelf_entry(0, None)],
                comics: vec![library_shelf_comic(0, None)],
                recent_load: loaded_shelf(),
                library_load: loaded_shelf(),
                total_recent_titles: 1,
                total_library_titles: 1,
                ..Bomtoon::default()
            };
            let screen = app.screen();
            let actions = &screen.top_bar.as_ref().expect("protected top bar").actions;
            assert_eq!(actions.len(), 1);
            assert_eq!(actions[0].action, action_id(ACCOUNT));
            assert_eq!(actions[0].label, "Coins unavailable");
            assert_fits(&screen);

            let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
            runner.action(action_id(ACCOUNT));
            assert_eq!(runner.app().view, View::Account);
        }
    }

    #[test]
    fn compact_shelf_recent_uses_creator_rows_with_episode_trailing_and_ready_picture() {
        let mut runner = recent_cover_runner(6);
        let commands = runner.action(action_id(RECENT));
        let first_screen = last_screen(&commands);
        let first_rows = first_screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("Recent compact rows");
        assert_eq!(first_rows.len(), 6);
        assert!(first_rows.iter().enumerate().all(|(index, row)| {
            row.title == format!("Recent {index}")
                && row.summary == format!("Creators {index}")
                && row.trailing.as_deref() == Some(format!("Episode title {index}").as_str())
                && row.lead == kobo_sdk::RowLead::Icon(Glyph::Book)
        }));
        assert!(first_screen.nav_bar.is_some());
        assert_fits(&first_screen);

        let (cover_task, _) = cover_fetches(&commands)
            .into_iter()
            .next()
            .expect("first visible cover fetch");
        let commands = runner.task_outcome(cover_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        let ready_screen = last_screen(&commands);
        let ready_rows = ready_screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("Recent compact rows");
        assert_eq!(ready_rows[0].title, "Recent 0");
        assert_eq!(ready_rows[0].summary, "Creators 0");
        assert_eq!(ready_rows[0].trailing.as_deref(), Some("Episode title 0"));
        assert!(matches!(
            ready_rows[0].lead,
            kobo_sdk::RowLead::Picture(TilePicture { source: (1, 1), .. }, Glyph::Book)
        ));
        assert!(ready_rows[1..]
            .iter()
            .all(|row| row.lead == kobo_sdk::RowLead::Icon(Glyph::Book)));
        assert!(ready_screen.nav_bar.is_some());
        assert_fits(&ready_screen);
    }

    #[test]
    fn compact_shelf_library_uses_creator_rows_with_owned_total_trailing_and_fixed_nav() {
        let mut runner = AppRunner::with_metrics(
            Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                destination: MainDestination::Featured,
                comics: (0..6)
                    .map(|index| library_shelf_comic(index, None))
                    .collect(),
                library_load: loaded_shelf(),
                total_library_titles: 6,
                ..Bomtoon::default()
            },
            CLARA_BW_METRICS,
        );

        let screen = last_screen(&runner.action(action_id(LIBRARY)));
        let rows = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("Library compact rows");
        assert_eq!(rows.len(), 6);
        assert!(rows.iter().enumerate().all(|(index, row)| {
            row.title == format!("Library {index}")
                && row.summary == format!("Creators {index}")
                && row.trailing.as_deref()
                    == Some(format!("{} / {}", index + 1, index + 7).as_str())
                && row.lead == kobo_sdk::RowLead::Icon(Glyph::Book)
        }));
        assert!(screen.nav_bar.is_some());
        assert_fits(&screen);
    }

    #[test]
    fn cover_only_visible_page_urls_are_requested() {
        let mut runner = recent_cover_runner(7);
        let commands = runner.action(action_id(RECENT));
        let visible = (0..6).map(shelf_cover_url).collect::<BTreeSet<_>>();
        let requested = cover_fetches(&commands)
            .into_iter()
            .map(|(_, url)| url)
            .collect::<BTreeSet<_>>();

        assert!(!requested.is_empty());
        assert!(requested.is_subset(&visible));
        assert!(!requested.contains(&shelf_cover_url(6)));
    }

    #[test]
    fn cover_duplicate_visible_url_fetches_and_installs_once() {
        let shared = shelf_cover_url(0);
        let mut runner = AppRunner::with_metrics(
            Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                recent: vec![
                    recent_shelf_entry(0, Some(shared.clone())),
                    recent_shelf_entry(1, Some(shared.clone())),
                ],
                recent_load: loaded_shelf(),
                total_recent_titles: 2,
                ..Bomtoon::default()
            },
            CLARA_BW_METRICS,
        );

        let commands = runner.action(action_id(RECENT));
        let fetches = cover_fetches(&commands);
        assert_eq!(fetches.len(), 1);
        let commands =
            runner.task_outcome(fetches[0].0, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, Command::PutPicture { .. }))
                .count(),
            1
        );
        let screen = last_screen(&commands);
        let rows = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("Recent rows");
        let handles = rows
            .iter()
            .map(|row| match row.lead {
                kobo_sdk::RowLead::Picture(picture, Glyph::Book) => picture.handle,
                _ => panic!("duplicate rows did not reuse the ready cover"),
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(handles.len(), 1);
    }

    #[test]
    fn cover_obsolete_page_tasks_are_cancelled() {
        let mut runner = recent_cover_runner(12);
        let commands = runner.action(action_id(RECENT));
        let first_page_tasks = cover_fetches(&commands)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        assert!(!first_page_tasks.is_empty());

        let commands = runner.action(action_id(NEXT_PAGE));

        assert_eq!(runner.app().page, 1);
        assert_eq!(cancelled_tasks(&commands), first_page_tasks);
    }

    #[test]
    fn cover_stale_generation_completion_is_ignored() {
        let mut runner = recent_cover_runner(7);
        let commands = runner.action(action_id(RECENT));
        let stale = cover_fetches(&commands)[0].0;
        runner.action(action_id(NEXT_PAGE));

        let commands = runner.task_outcome(stale, TaskOutcome::Completed(TINY_WEBP.to_vec()));

        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::PutPicture { .. })));
    }

    #[test]
    fn cover_failure_keeps_placeholder_without_error_or_same_generation_retry() {
        let mut runner = recent_cover_runner(1);
        let commands = runner.action(action_id(RECENT));
        let cover = cover_fetches(&commands)[0].0;

        let commands = runner.task_outcome(cover, TaskOutcome::Failed(TaskError::Offline));
        assert!(cover_fetches(&commands).is_empty());
        assert!(runner.app().problem.is_none());
        let screen = runner.app().screen();
        let row = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => rows.first(),
                _ => None,
            })
            .expect("Recent row");
        assert_eq!(row.lead, kobo_sdk::RowLead::Icon(Glyph::Book));

        let commands = runner.action(action_id("refresh-layout"));
        assert!(cover_fetches(&commands).is_empty());
    }

    #[test]
    fn cover_cached_picture_is_reused_across_feature_banner_and_collection() {
        let shared = shelf("shared-cover", "Shared cover");
        let mut runner = AppRunner::with_metrics(
            Bomtoon {
                account: AccountState::SignedOut,
                view: View::Main,
                destination: MainDestination::Recent,
                featured: {
                    let collection = model::FeatureCollection {
                        id: "newest".to_owned(),
                        label: "人氣新作".to_owned(),
                        priority: 2,
                        order: 0,
                        comics: vec![shared.clone(); 6],
                    };
                    FeaturedState {
                        generation: 1,
                        snapshot: Some(FeatureSnapshot {
                            banners: vec![shared],
                            collections: vec![collection.clone()],
                            sources: BTreeMap::from([(FeatureSource::Homepage, vec![collection])]),
                            failed_sources: BTreeSet::new(),
                            warning: None,
                        }),
                        ..FeaturedState::default()
                    }
                },
                ..Bomtoon::default()
            },
            CLARA_BW_METRICS,
        );

        let commands = runner.action(action_id(FEATURED));
        let fetches = cover_fetches(&commands);
        assert_eq!(fetches.len(), 1);
        let commands =
            runner.task_outcome(fetches[0].0, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        let screen = last_screen(&commands);
        let banner_picture = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::ImageStrip { tiles, .. } => tiles.first().and_then(|tile| tile.picture),
                _ => None,
            })
            .expect("Feature banner picture");
        let collection_picture = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::MediaGrid { tiles, .. } => tiles.first().and_then(|tile| tile.picture),
                _ => None,
            })
            .or_else(|| {
                let commands = runner.action(action_id(NEXT_PAGE));
                assert!(cover_fetches(&commands).is_empty());
                last_screen(&commands)
                    .nodes
                    .iter()
                    .find_map(|node| match node {
                        Node::MediaGrid { tiles, .. } => {
                            tiles.first().and_then(|tile| tile.picture)
                        }
                        _ => None,
                    })
            })
            .expect("Feature collection picture");
        assert_eq!(banner_picture, collection_picture);
    }

    #[test]
    fn cover_spawn_resumes_in_visible_order_after_capacity_release() {
        let mut runner = recent_cover_runner(6);
        let commands = runner.action(action_id(RECENT));
        let initial = cover_fetches(&commands);
        assert_eq!(initial.len(), 4);
        assert_eq!(runner.tasks_in_flight(), 4);

        let commands = runner.task_outcome(initial[0].0, TaskOutcome::Failed(TaskError::TimedOut));
        let resumed = cover_fetches(&commands);
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].1, shelf_cover_url(4));
        assert_eq!(runner.tasks_in_flight(), 4);
    }

    #[test]
    fn cover_scheduler_shares_the_runner_four_task_cap() {
        let (mut runner, commands) = started();
        let scope = scope_task(&commands);
        let library = fetch_task_with(&commands, "/library?").0;
        runner.task_outcome(
            scope,
            TaskOutcome::Completed(b"00112233445566778899aabbccddeeff".to_vec()),
        );
        runner.task_outcome(library, TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()));
        runner.app_mut().featured = feed_with_recommendations(6);

        let commands = runner.action(action_id(FEATURED));
        assert!(cover_fetches(&commands).is_empty());
        assert_eq!(runner.tasks_in_flight(), 4);

        let sources = runner
            .app()
            .feature_tasks
            .keys()
            .copied()
            .take(2)
            .collect::<Vec<_>>();
        assert_eq!(sources.len(), 2);
        for source in sources {
            let commands = runner.task_outcome(source, TaskOutcome::Cancelled);
            assert_eq!(cover_fetches(&commands).len(), 1);
            assert_eq!(runner.tasks_in_flight(), 4);
        }
    }
    #[test]
    fn episode_commerce_summary_and_actions_fit_clara() {
        let episode = |id, purchase| Episode {
            id,
            alias: format!("episode-{id}"),
            title: format!("Episode {id}"),
            opened_at: 1_709_136_000_000,
            thumbnail_url: None,
            purchase,
            rent_expires_at: None,
            rent_coin: Some(usize::MAX),
            purchase_coin: Some(usize::MAX),
            gift_eligible: true,
        };
        let mut app = Bomtoon {
            view: View::Episodes,
            selected_title: "Maximum commerce".to_owned(),
            selected_content_id: Some(41),
            wallet: WalletState {
                summary: Some(WalletSummary {
                    coins: model::AssetAmounts {
                        standard: usize::MAX,
                        bonus: 0,
                        free: 0,
                    },
                    tickets: model::AssetAmounts {
                        standard: usize::MAX,
                        bonus: 0,
                        free: 0,
                    },
                }),
                ..WalletState::default()
            },
            episodes: vec![
                episode(1, model::PurchaseState::Owned),
                episode(2, model::PurchaseState::Sample),
                episode(3, model::PurchaseState::Free),
                episode(4, model::PurchaseState::NotOwned),
                episode(5, model::PurchaseState::Other("REMOTE".to_owned())),
                episode(6, model::PurchaseState::NotOwned),
            ],
            ..Bomtoon::default()
        };
        app.prepare_episode_layout();

        let screen = app.episode_screen();
        let drawn = format!("{screen:?}");
        assert!(
            drawn.contains(&format!("Coins {} · Gifts unavailable", usize::MAX)),
            "missing independent balances: {drawn}"
        );
        assert!(!drawn.contains("Tickets"));
        let rows = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("commerce episode rows");
        assert!(rows
            .iter()
            .any(|row| row.trailing.as_deref() == Some("Owned")));
        assert!(rows
            .iter()
            .any(|row| { row.action == action_id("episode-3") && row.trailing.is_none() }));
        assert_fits(&screen);
    }

    #[test]
    fn choosing_commerce_is_a_retained_four_action_surface() {
        const SCOPE: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        let scope = test_scope(SCOPE);
        let app = Bomtoon {
            account: AccountState::Active,
            connection: ConnectionState::Online,
            account_scope: Some(scope),
            commerce: choosing_commerce(scope),
            view: View::Episodes,
            selected_content_id: Some(41),
            selected_content_alias: "hunter_q".to_owned(),
            selected_title: "Hunter Q".to_owned(),
            commerce_episode: Some(0),
            episodes: vec![Episode {
                id: 105,
                alias: "paid".to_owned(),
                title: "Paid Episode".to_owned(),
                opened_at: 1_709_136_000_000,
                thumbnail_url: None,
                purchase: model::PurchaseState::NotOwned,
                rent_expires_at: None,
                rent_coin: Some(1),
                purchase_coin: Some(2),
                gift_eligible: true,
            }],
            ..Bomtoon::default()
        };

        let screen = app.screen();
        let drawn = format!("{screen:?}");
        for expected in [
            "Paid Episode",
            "Use Gift",
            "Rent · 1 coins",
            "Buy · 2 coins",
            "Cancel",
            "No Gifts for this title",
        ] {
            assert!(drawn.contains(expected), "missing {expected}: {drawn}");
        }
        assert_eq!(
            screen
                .nodes
                .iter()
                .filter(|node| matches!(node, Node::Button { .. }))
                .count(),
            4
        );
        assert!(screen.owns_back);
        assert_fits(&screen);
    }
    #[test]
    fn content_detail_loads_the_title_gift_balance_independently() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);

        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);
        let commands =
            runner.task_outcome(content_task, TaskOutcome::Completed(content_response()));
        let (gift_task, gift_work) = only_spawn(&commands);
        assert_eq!(gift_work, api::title_gifts(41));
        let loading = format!("{:?}", last_screen(&commands));
        assert!(loading.contains("Coins 10 · Gifts…"), "{loading}");

        let commands = runner.task_outcome(
            gift_task,
            TaskOutcome::Completed(
                br#"{"result":"SUCCESS","data":{"receivedGifts":[{"giftType":"RENT","isReceived":true,"issuedCount":3,"usedCount":1}],"receivableGifts":[]}}"#
                    .to_vec(),
            ),
        );
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(drawn.contains("Coins 10 · Gifts 2"), "{drawn}");
        assert_fits(&screen);
    }
    #[test]
    fn gift_failure_exposes_only_a_gift_retry_and_preserves_episode_actions() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands = runner.task_outcome(content, TaskOutcome::Completed(content_response()));
        let (gift, _) = only_spawn(&commands);

        let commands = runner.task_outcome(gift, TaskOutcome::Failed(TaskError::TimedOut));
        let screen = last_screen(&commands);
        assert!(screen.nodes.iter().any(|node| matches!(
            node,
            Node::Button { action, label, .. }
                if *action == action_id(RETRY_GIFTS) && label.contains("Retry Gift")
        )));
        assert!(screen.nodes.iter().any(|node| matches!(
            node,
            Node::Rows { rows, .. }
                if rows.iter().any(|row| row.action == action_id("episode-0"))
        )));
        assert_fits(&screen);

        let commands = runner.action(action_id(RETRY_GIFTS));
        let (_, work) = only_spawn(&commands);
        assert_eq!(work, api::title_gifts(41));
        assert!(spawns(&commands).iter().all(
            |(_, work)| !matches!(work, Task::Fetch { url, .. } if url.contains("/asset/user"))
        ));
    }
    #[test]
    fn quote_requote_and_marker_acknowledgement_order_the_purchase_post() {
        const GIFT_RESPONSE: &[u8] = br#"{"result":"SUCCESS","data":{"receivedGifts":[{"giftType":"RENT","isReceived":true,"issuedCount":1,"usedCount":0}],"receivableGifts":[]}}"#;
        const QUOTE_RESPONSE: &[u8] = br#"{"result":"SUCCESS","data":{"contentsId":41,"episodeId":105,"contentsAlias":"hunter_q","episodeAlias":"paid","isAvailable":false,"coinKind":"COIN","rentCoin":1,"possessionCoin":2,"permanentCoin":2,"isRentGift":true,"isPossessionGift":false}}"#;
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });

        let (content_task, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands =
            runner.task_outcome(content_task, TaskOutcome::Completed(content_response()));
        let (gift_task, _) = only_spawn(&commands);
        runner.task_outcome(gift_task, TaskOutcome::Completed(GIFT_RESPONSE.to_vec()));

        let commands = runner.action(action_id("episode-4"));
        let (quote_task, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::quote("hunter_q", "paid", model::PurchaseType::Possession)
        );
        let commands =
            runner.task_outcome(quote_task, TaskOutcome::Completed(QUOTE_RESPONSE.to_vec()));
        let quote_screen = last_screen(&commands);
        assert!(format!("{quote_screen:?}").contains("Buy · 2 coins"));

        let commands = runner.action(action_id(RENT));
        let (requote_task, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::quote("hunter_q", "paid", model::PurchaseType::Rent)
        );
        let commands = runner.task_outcome(
            requote_task,
            TaskOutcome::Completed(QUOTE_RESPONSE.to_vec()),
        );
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::Store(StoreRequest::Save { key, .. }) if key == commerce::MARKER_KEY
        )));
        assert!(
            spawns(&commands)
                .iter()
                .all(|(_, work)| !matches!(work, Task::Post { .. })),
            "POST preceded Saved: {commands:?}"
        );

        let commands = runner.store_result(StoreResult::Saved {
            key: commerce::MARKER_KEY.to_owned(),
        });
        let (_, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::purchase("hunter_q", 105, model::PurchaseType::Rent)
        );
    }
    fn runner_choosing_paid_episode() -> AppRunner<Bomtoon> {
        const GIFT_RESPONSE: &[u8] = br#"{"result":"SUCCESS","data":{"receivedGifts":[{"giftType":"RENT","isReceived":true,"issuedCount":1,"usedCount":0}],"receivableGifts":[]}}"#;
        const QUOTE_RESPONSE: &[u8] = br#"{"result":"SUCCESS","data":{"contentsId":41,"episodeId":105,"contentsAlias":"hunter_q","episodeAlias":"paid","isAvailable":false,"coinKind":"COIN","rentCoin":1,"possessionCoin":2,"permanentCoin":2,"isRentGift":true,"isPossessionGift":false}}"#;
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });
        let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands = runner.task_outcome(content, TaskOutcome::Completed(content_response()));
        let (gift, _) = only_spawn(&commands);
        runner.task_outcome(gift, TaskOutcome::Completed(GIFT_RESPONSE.to_vec()));
        let (quote, _) = only_spawn(&runner.action(action_id("episode-4")));
        runner.task_outcome(quote, TaskOutcome::Completed(QUOTE_RESPONSE.to_vec()));
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::Choosing
        );
        runner
    }

    fn runner_waiting_for_buy_post() -> (AppRunner<Bomtoon>, TaskId) {
        const QUOTE_RESPONSE: &[u8] = br#"{"result":"SUCCESS","data":{"contentsId":41,"episodeId":105,"contentsAlias":"hunter_q","episodeAlias":"paid","isAvailable":false,"coinKind":"COIN","rentCoin":1,"possessionCoin":2,"permanentCoin":2,"isRentGift":true,"isPossessionGift":false}}"#;
        let mut runner = runner_choosing_paid_episode();
        let (requote, work) = only_spawn(&runner.action(action_id(BUY)));
        assert_eq!(
            work,
            api::quote("hunter_q", "paid", model::PurchaseType::Possession)
        );
        runner.task_outcome(requote, TaskOutcome::Completed(QUOTE_RESPONSE.to_vec()));
        let commands = runner.store_result(StoreResult::Saved {
            key: commerce::MARKER_KEY.to_owned(),
        });
        let (post, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::purchase("hunter_q", 105, model::PurchaseType::Possession)
        );
        (runner, post)
    }

    fn runner_waiting_for_paid_rent_post() -> (AppRunner<Bomtoon>, TaskId) {
        const GIFT_RESPONSE: &[u8] = br#"{"result":"SUCCESS","data":{"receivedGifts":[{"giftType":"RENT","isReceived":true,"issuedCount":1,"usedCount":0}],"receivableGifts":[]}}"#;
        const QUOTE_RESPONSE: &[u8] = br#"{"result":"SUCCESS","data":{"contentsId":41,"episodeId":105,"contentsAlias":"hunter_q","episodeAlias":"paid","isAvailable":false,"coinKind":"COIN","rentCoin":1,"possessionCoin":2,"permanentCoin":2,"isRentGift":true,"isPossessionGift":false}}"#;
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });
        let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands = runner.task_outcome(content, TaskOutcome::Completed(content_response()));
        let (gift, _) = only_spawn(&commands);
        runner.task_outcome(gift, TaskOutcome::Completed(GIFT_RESPONSE.to_vec()));
        let (quote, _) = only_spawn(&runner.action(action_id("episode-4")));
        runner.task_outcome(quote, TaskOutcome::Completed(QUOTE_RESPONSE.to_vec()));
        let (requote, _) = only_spawn(&runner.action(action_id(RENT)));
        runner.task_outcome(requote, TaskOutcome::Completed(QUOTE_RESPONSE.to_vec()));
        let commands = runner.store_result(StoreResult::Saved {
            key: commerce::MARKER_KEY.to_owned(),
        });
        let (post, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::purchase("hunter_q", 105, model::PurchaseType::Rent)
        );
        (runner, post)
    }

    fn runner_waiting_for_gift_post() -> (AppRunner<Bomtoon>, TaskId) {
        const GIFT_RESPONSE: &[u8] = br#"{"result":"SUCCESS","data":{"receivedGifts":[{"giftType":"RENT","isReceived":true,"issuedCount":1,"usedCount":0}],"receivableGifts":[]}}"#;
        const QUOTE_RESPONSE: &[u8] = br#"{"result":"SUCCESS","data":{"contentsId":41,"episodeId":105,"contentsAlias":"hunter_q","episodeAlias":"paid","isAvailable":false,"coinKind":"COIN","rentCoin":1,"possessionCoin":2,"permanentCoin":2,"isRentGift":true,"isPossessionGift":false}}"#;
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });
        let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands = runner.task_outcome(content, TaskOutcome::Completed(content_response()));
        let (gift, _) = only_spawn(&commands);
        runner.task_outcome(gift, TaskOutcome::Completed(GIFT_RESPONSE.to_vec()));
        let (quote, _) = only_spawn(&runner.action(action_id("episode-4")));
        runner.task_outcome(quote, TaskOutcome::Completed(QUOTE_RESPONSE.to_vec()));
        let (requote, _) = only_spawn(&runner.action(action_id(USE_GIFT)));
        runner.task_outcome(requote, TaskOutcome::Completed(QUOTE_RESPONSE.to_vec()));
        let commands = runner.store_result(StoreResult::Saved {
            key: commerce::MARKER_KEY.to_owned(),
        });
        let (post, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::purchase("hunter_q", 105, model::PurchaseType::RentGift)
        );
        (runner, post)
    }
    #[test]
    fn accepted_coin_rent_reconciles_exact_content_and_wallet_before_forget() {
        const RECEIPT: &[u8] = br#"{"result":"SUCCESS","data":{"purchaseType":"RENT","contentsAlias":"hunter_q","episodeAlias":"paid","useCoin":1,"useGoldCoin":1,"useBonusCoin":0,"useFreeCoin":0}}"#;
        const NINE_COINS: &[u8] = br#"{"result":"SUCCESS","data":{"coinBalance":{"coin":6,"bonusCoin":2,"freeCoin":1},"ticketBalance":{"ticket":3,"bonusTicket":1,"freeTicket":0}}}"#;
        let rented_content = || {
            detail_response(
                41,
                "hunter_q",
                "Localized Title",
                "{\"id\":105,\"alias\":\"paid\",\"title\":\"Paid Episode\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"RENT\",\"rentExpiredAt\":1819728000000,\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2,\"isRentGift\":true}",
            )
        };

        let (mut runner, post) = runner_waiting_for_paid_rent_post();
        let title = runner.app().selected_title.clone();
        let page = runner.app().page;

        let commands = runner.task_outcome(post, TaskOutcome::Completed(RECEIPT.to_vec()));
        let (content, _) = fetch_task_with(&commands, "/detail/hunter_q");
        let (wallet, _) = fetch_task_with(&commands, "/asset/user");
        assert_eq!(spawns(&commands).len(), 2);
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::Store(StoreRequest::Forget { .. }))));

        let commands = runner.task_outcome(content, TaskOutcome::Completed(rented_content()));
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::Store(StoreRequest::Forget { .. }))));
        let commands = runner.task_outcome(wallet, TaskOutcome::Completed(NINE_COINS.to_vec()));
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::Store(StoreRequest::Forget { key }) if key == commerce::MARKER_KEY
        )));
        assert_eq!(runner.app().view, View::Episodes);
        assert_eq!(runner.app().selected_title, title);
        assert_eq!(runner.app().page, page);
        assert_eq!(
            runner.app().episodes[0].purchase,
            model::PurchaseState::Rented
        );
        assert_eq!(
            runner
                .app()
                .wallet
                .summary
                .and_then(|summary| summary.coins.total()),
            Some(9)
        );

        runner.store_result(StoreResult::Forgotten {
            key: commerce::MARKER_KEY.to_owned(),
        });
        assert_eq!(runner.app().commerce.state(), commerce::CommerceState::Idle);
        assert_eq!(runner.app().view, View::Episodes);
    }
    #[test]
    fn accepted_gift_rent_requires_entitlement_and_exact_gift_decrement() {
        const RECEIPT: &[u8] = br#"{"result":"SUCCESS","data":{"purchaseType":"RENT_GIFT","contentsAlias":"hunter_q","episodeAlias":"paid","useCoin":0}}"#;
        const ZERO_GIFTS: &[u8] = br#"{"result":"SUCCESS","data":{"receivedGifts":[{"giftType":"RENT","isReceived":true,"issuedCount":1,"usedCount":1}],"receivableGifts":[]}}"#;
        let rented_content = || {
            detail_response(
                41,
                "hunter_q",
                "Localized Title",
                "{\"id\":105,\"alias\":\"paid\",\"title\":\"Paid Episode\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"RENT\",\"rentExpiredAt\":1819728000000,\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2,\"isRentGift\":true}",
            )
        };
        let (mut runner, post) = runner_waiting_for_gift_post();

        let commands = runner.task_outcome(post, TaskOutcome::Completed(RECEIPT.to_vec()));
        let (content, _) = fetch_task_with(&commands, "/detail/hunter_q");
        let (gift, _) = fetch_task_with(&commands, "/gift/contents/detail?");
        assert_eq!(spawns(&commands).len(), 2, "{commands:?}");

        runner.task_outcome(content, TaskOutcome::Completed(rented_content()));
        let commands = runner.task_outcome(gift, TaskOutcome::Completed(ZERO_GIFTS.to_vec()));
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::Store(StoreRequest::Forget { key }) if key == commerce::MARKER_KEY
        )));
        assert_eq!(
            runner.app().episodes[0].purchase,
            model::PurchaseState::Rented
        );
        assert_eq!(runner.app().gifts.available, Some(0));
        assert_eq!(runner.app().view, View::Episodes);
    }

    #[test]
    fn explicit_backend_rejection_waits_for_matching_forget_before_safe_notice() {
        let (mut runner, post) = runner_waiting_for_paid_rent_post();

        let commands = runner.task_outcome(
            post,
            TaskOutcome::Completed(
                br#"{"result":"FAIL","data":{"message":"not accepted"}}"#.to_vec(),
            ),
        );

        assert!(spawns(&commands).is_empty(), "{commands:?}");
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::Store(StoreRequest::Forget { key }) if key == commerce::MARKER_KEY
        )));
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::ClearingIntent
        );
        assert_eq!(runner.app().pending_purchase_rejection, Some("FAIL"));
        assert_eq!(runner.app().purchase_rejection_notice, None);
        assert!(!format!("{:?}", runner.app().screen()).contains("Purchase rejected"));

        let commands = runner.store_result(StoreResult::Forgotten {
            key: "other.marker".to_owned(),
        });
        assert!(commands.is_empty());
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::ClearingIntent
        );
        assert_eq!(runner.app().pending_purchase_rejection, Some("FAIL"));
        assert_eq!(runner.app().purchase_rejection_notice, None);

        let commands = runner.store_result(StoreResult::Forgotten {
            key: commerce::MARKER_KEY.to_owned(),
        });
        assert_no_post_or_marker_forget(&commands);
        assert_eq!(runner.app().commerce.state(), commerce::CommerceState::Idle);
        assert_eq!(runner.app().pending_purchase_rejection, None);
        assert_eq!(runner.app().purchase_rejection_notice, Some("FAIL"));
        let drawn = format!("{:?}", last_screen(&commands));
        assert!(drawn.contains("Purchase rejected: FAIL"), "{drawn}");
        assert!(!drawn.contains("not accepted"), "{drawn}");

        let commands = runner.action(action_id("episode-4"));
        let (_, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::quote("hunter_q", "paid", model::PurchaseType::Possession)
        );
        assert_eq!(runner.app().pending_purchase_rejection, None);
        assert_eq!(runner.app().purchase_rejection_notice, None);
        assert!(commands
            .iter()
            .all(|command| !matches!(command, Command::Store(_))));
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn processing_purchase_response_reconciles_without_forget_or_repost() {
        let (mut runner, post) = runner_waiting_for_paid_rent_post();

        let commands = runner.task_outcome(
            post,
            TaskOutcome::Completed(
                br#"{"result":"PROCESSING","data":{"message":"wait"}}"#.to_vec(),
            ),
        );

        fetch_task_with(&commands, "/detail/hunter_q");
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::Reconciling
        );
        assert_eq!(runner.app().pending_purchase_rejection, None);
        assert_no_post_or_marker_forget(&commands);

        let commands = runner.store_result(StoreResult::Forgotten {
            key: commerce::MARKER_KEY.to_owned(),
        });
        assert!(commands.is_empty());
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::Reconciling
        );
        assert_no_post_or_marker_forget(&commands);

        let commands = runner.action(action_id("episode-4"));
        assert!(spawns(&commands).is_empty(), "{commands:?}");
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn marker_reconciliation_content_credential_loss_uses_protected_transition() {
        const SCOPE: &[u8; 32] = b"00112233445566778899aabbccddeeff";
        for (error, expected) in [
            (TaskError::NoCredential, AccountState::SignedOut),
            (TaskError::Unauthorized, AccountState::Expired),
        ] {
            let (mut runner, scope) = startup_with_marker(Some(marker_for(test_scope(SCOPE))));
            let commands = runner.task_outcome(scope, TaskOutcome::Completed(SCOPE.to_vec()));
            let (content, _) = fetch_task_with(&commands, "/detail/hunter_q");
            seed_all_account_data(&mut runner);
            runner.app_mut().pending_purchase_rejection = Some("FAIL");
            runner.app_mut().purchase_rejection_notice = Some("FAIL");

            let commands = runner.task_outcome(content, TaskOutcome::Failed(error));
            assert_eq!(
                runner.app().commerce.state(),
                commerce::CommerceState::AcceptedButStale
            );
            assert_eq!(runner.app().account, expected);
            assert_eq!(runner.app().account_scope, None);
            assert!(runner.app().reconciliation.is_none());
            assert_all_account_data_cleared(runner.app());
            assert!(runner.app().wallet.summary.is_none());
            assert_eq!(runner.app().gifts.title_id, None);
            assert_eq!(runner.app().pending_purchase_rejection, None);
            assert_eq!(runner.app().purchase_rejection_notice, None);
            assert_no_post_or_marker_forget(&commands);
        }
    }
    #[test]
    fn failed_authoritative_refresh_locks_and_only_refresh_status_retries() {
        const RECEIPT: &[u8] = br#"{"result":"SUCCESS","data":{"purchaseType":"RENT","contentsAlias":"hunter_q","episodeAlias":"paid","useCoin":1}}"#;
        const NINE_COINS: &[u8] = br#"{"result":"SUCCESS","data":{"coinBalance":{"coin":6,"bonusCoin":2,"freeCoin":1},"ticketBalance":{"ticket":3,"bonusTicket":1,"freeTicket":0}}}"#;
        let (mut runner, post) = runner_waiting_for_paid_rent_post();
        let commands = runner.task_outcome(post, TaskOutcome::Completed(RECEIPT.to_vec()));
        let (content, _) = fetch_task_with(&commands, "/detail/hunter_q");
        let (wallet, _) = fetch_task_with(&commands, "/asset/user");

        runner.task_outcome(content, TaskOutcome::Failed(TaskError::TimedOut));
        let commands = runner.task_outcome(wallet, TaskOutcome::Completed(NINE_COINS.to_vec()));
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::AcceptedButStale
        );
        let screen = last_screen(&commands);
        let buttons = screen
            .nodes
            .iter()
            .filter_map(|node| match node {
                Node::Button { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(buttons, ["Refresh status"]);
        assert!(format!("{screen:?}").contains("Accepted, refresh needed"));
        assert_fits(&screen);
        assert!(screen.owns_back);
        assert!(!runner.app().commerce.marker_belongs_to_another_account());
        let commands = runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Episodes);
        assert_no_post_or_marker_forget(&commands);

        let commands = runner.action(action_id(REFRESH_COMMERCE));
        assert_eq!(spawns(&commands).len(), 3, "{commands:?}");
        assert!(
            spawns(&commands)
                .iter()
                .all(|(_, work)| !matches!(work, Task::Post { .. })),
            "Refresh duplicated POST: {commands:?}"
        );
    }
    #[test]
    fn ambiguous_transport_forgets_after_unchanged_authoritative_state_without_reposting() {
        const ONE_GIFT: &[u8] = br#"{"result":"SUCCESS","data":{"receivedGifts":[{"giftType":"RENT","isReceived":true,"issuedCount":1,"usedCount":0}],"receivableGifts":[]}}"#;
        let (mut runner, post) = runner_waiting_for_paid_rent_post();

        let commands = runner.task_outcome(post, TaskOutcome::Failed(TaskError::TimedOut));
        let (content, _) = fetch_task_with(&commands, "/detail/hunter_q");
        let (wallet, _) = fetch_task_with(&commands, "/asset/user");
        let (gift, _) = fetch_task_with(&commands, "/gift/contents/detail?");
        assert_eq!(spawns(&commands).len(), 3);
        assert!(spawns(&commands)
            .iter()
            .all(|(_, work)| !matches!(work, Task::Post { .. })));

        runner.task_outcome(gift, TaskOutcome::Completed(ONE_GIFT.to_vec()));
        runner.task_outcome(content, TaskOutcome::Completed(content_response()));
        let commands = runner.task_outcome(wallet, TaskOutcome::Completed(ASSET_RESPONSE.to_vec()));
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::Store(StoreRequest::Forget { key }) if key == commerce::MARKER_KEY
        )));
        assert_eq!(
            runner
                .app()
                .episodes
                .iter()
                .find(|episode| episode.id == 105)
                .map(|episode| episode.purchase.clone()),
            Some(model::PurchaseState::NotOwned)
        );
        assert_eq!(
            runner
                .app()
                .wallet
                .summary
                .and_then(|summary| summary.coins.total()),
            Some(10)
        );
    }
    #[test]
    fn zero_hour_rental_refreshes_content_before_starting_reader() {
        const NO_GIFTS: &[u8] =
            br#"{"result":"SUCCESS","data":{"receivedGifts":[],"receivableGifts":[]}}"#;
        let expired_rent = || {
            detail_response(
                41,
                "hunter_q",
                "Localized Title",
                "{\"id\":104,\"alias\":\"rented\",\"title\":\"Rented Episode\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"RENT\",\"rentExpiredAt\":1,\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2,\"isRentGift\":true}",
            )
        };
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands = runner.task_outcome(content, TaskOutcome::Completed(expired_rent()));
        let (gift, _) = only_spawn(&commands);
        runner.task_outcome(gift, TaskOutcome::Completed(NO_GIFTS.to_vec()));

        let commands = runner.action(action_id("episode-0"));
        let (refresh, work) = only_spawn(&commands);
        assert_eq!(work, api::detail("hunter_q"));
        assert_eq!(runner.app().view, View::Episodes);

        let commands = runner.task_outcome(refresh, TaskOutcome::Completed(expired_rent()));
        assert!(spawns(&commands).iter().any(
            |(_, work)| matches!(work, Task::Fetch { url, .. } if url.contains("/contents/images/"))
        ));
        assert!(spawns(&commands)
            .iter()
            .all(|(_, work)| !matches!(work, Task::Fetch { url, .. } if url.contains("/gift/"))));
        assert_eq!(runner.app().view, View::Reader);
    }
    #[test]
    fn back_cancels_requote_before_save_and_is_consumed_after_save() {
        const QUOTE: &[u8] = br#"{"result":"SUCCESS","data":{"contentsId":41,"episodeId":105,"contentsAlias":"hunter_q","episodeAlias":"paid","isAvailable":false,"coinKind":"COIN","rentCoin":1,"possessionCoin":2,"permanentCoin":2,"isRentGift":true,"isPossessionGift":false}}"#;
        let mut before_save = runner_choosing_paid_episode();
        let (requote, _) = only_spawn(&before_save.action(action_id(RENT)));
        let commands = before_save.action(ActionId::BACK);
        assert!(commands.contains(&Command::Cancel(requote)));
        assert_eq!(
            before_save.app().commerce.state(),
            commerce::CommerceState::Idle
        );
        assert_eq!(before_save.app().view, View::Episodes);
        assert_no_post_or_marker_forget(&commands);
        let commands = before_save.task_outcome(requote, TaskOutcome::Completed(QUOTE.to_vec()));
        assert_no_post_or_marker_forget(&commands);
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::Store(StoreRequest::Save { .. }))));

        let mut persisting = runner_choosing_paid_episode();
        let (requote, _) = only_spawn(&persisting.action(action_id(RENT)));
        let commands = persisting.task_outcome(requote, TaskOutcome::Completed(QUOTE.to_vec()));
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::Store(StoreRequest::Save { .. }))));
        let commands = persisting.action(ActionId::BACK);
        assert_eq!(
            persisting.app().commerce.state(),
            commerce::CommerceState::PersistingIntent
        );
        assert_eq!(persisting.app().view, View::Episodes);
        assert_no_post_or_marker_forget(&commands);

        let (mut mutating, _) = runner_waiting_for_paid_rent_post();
        let commands = mutating.action(ActionId::BACK);
        assert_eq!(
            mutating.app().commerce.state(),
            commerce::CommerceState::Mutating
        );
        assert_eq!(mutating.app().view, View::Episodes);
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn back_is_consumed_during_reconciliation_and_marker_clearing() {
        const RECEIPT: &[u8] = br#"{"result":"SUCCESS","data":{"purchaseType":"RENT","contentsAlias":"hunter_q","episodeAlias":"paid","useCoin":1}}"#;
        const NINE_COINS: &[u8] = br#"{"result":"SUCCESS","data":{"coinBalance":{"coin":6,"bonusCoin":2,"freeCoin":1},"ticketBalance":{"ticket":3,"bonusTicket":1,"freeTicket":0}}}"#;
        let rented_content = || {
            detail_response(
                41,
                "hunter_q",
                "Localized Title",
                "{\"id\":105,\"alias\":\"paid\",\"title\":\"Paid Episode\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"RENT\",\"rentExpiredAt\":1819728000000,\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2,\"isRentGift\":true}",
            )
        };
        let (mut runner, post) = runner_waiting_for_paid_rent_post();
        let commands = runner.task_outcome(post, TaskOutcome::Completed(RECEIPT.to_vec()));
        let (content, _) = fetch_task_with(&commands, "/detail/hunter_q");
        let (wallet, _) = fetch_task_with(&commands, "/asset/user");
        let commands = runner.action(ActionId::BACK);
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::Reconciling
        );
        assert_eq!(runner.app().view, View::Episodes);
        assert_no_post_or_marker_forget(&commands);

        runner.task_outcome(content, TaskOutcome::Completed(rented_content()));
        let commands = runner.task_outcome(wallet, TaskOutcome::Completed(NINE_COINS.to_vec()));
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::ClearingIntent
        );
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::Store(StoreRequest::Forget { .. }))));
        let commands = runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Episodes);
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn failed_requote_drops_retained_surface_and_returns_to_episode_actions() {
        let mut runner = runner_choosing_paid_episode();
        let (requote, _) = only_spawn(&runner.action(action_id(RENT)));

        let commands = runner.task_outcome(requote, TaskOutcome::Failed(TaskError::TimedOut));

        assert_eq!(runner.app().commerce.state(), commerce::CommerceState::Idle);
        assert!(runner.app().retained_quote.is_none());
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(screen.nodes.iter().any(|node| matches!(
            node,
            Node::Rows { rows, .. }
                if rows.iter().any(|row| row.action == action_id("episode-0"))
        )));
        assert!(!drawn.contains("Use Gift"), "{drawn}");
    }

    #[test]
    fn stale_episode_action_during_quote_keeps_original_episode_bound() {
        const NO_GIFTS: &[u8] =
            br#"{"result":"SUCCESS","data":{"receivedGifts":[],"receivableGifts":[]}}"#;
        let two_unowned = || {
            detail_response(
                41,
                "hunter_q",
                "Localized Title",
                concat!(
                    "{\"id\":105,\"alias\":\"first\",\"title\":\"First Paid\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"NONE\",\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2,\"isRentGift\":true},",
                    "{\"id\":106,\"alias\":\"second\",\"title\":\"Second Paid\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"NONE\",\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2,\"isRentGift\":true}"
                ),
            )
        };
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });
        let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands = runner.task_outcome(content, TaskOutcome::Completed(two_unowned()));
        let (gift, _) = only_spawn(&commands);
        runner.task_outcome(gift, TaskOutcome::Completed(NO_GIFTS.to_vec()));

        let commands = runner.action(action_id("episode-0"));
        let (quote, work) = only_spawn(&commands);
        assert_eq!(
            work,
            api::quote("hunter_q", "first", model::PurchaseType::Possession)
        );
        assert_eq!(spawns(&commands).len(), 1);
        assert!(commands
            .iter()
            .all(|command| !matches!(command, Command::Store(_))));
        assert_no_post_or_marker_forget(&commands);

        let commands = runner.action(action_id("episode-1"));

        assert!(spawns(&commands).is_empty(), "{commands:?}");
        assert!(commands
            .iter()
            .all(|command| !matches!(command, Command::Store(_))));
        assert_no_post_or_marker_forget(&commands);
        assert_eq!(runner.app().commerce_episode, Some(0));
        assert_eq!(runner.app().quote_episode_title(), "First Paid");
        assert!(runner.app().commerce_task.as_ref().is_some_and(|task| {
            task.id == quote
                && matches!(
                    &task.purpose,
                    CommerceTaskPurpose::Quote { selection, .. }
                        if selection.episode_id == 105 && selection.episode_alias == "first"
                )
        }));
    }

    #[test]
    fn quote_heading_names_the_selected_unowned_episode() {
        const NO_GIFTS: &[u8] =
            br#"{"result":"SUCCESS","data":{"receivedGifts":[],"receivableGifts":[]}}"#;
        const SECOND_QUOTE: &[u8] = br#"{"result":"SUCCESS","data":{"contentsId":41,"episodeId":106,"contentsAlias":"hunter_q","episodeAlias":"second","isAvailable":false,"coinKind":"COIN","rentCoin":1,"possessionCoin":2,"permanentCoin":2,"isRentGift":true,"isPossessionGift":false}}"#;
        let two_unowned = || {
            detail_response(
                41,
                "hunter_q",
                "Localized Title",
                concat!(
                    "{\"id\":105,\"alias\":\"first\",\"title\":\"First Paid\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"NONE\",\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2,\"isRentGift\":true},",
                    "{\"id\":106,\"alias\":\"second\",\"title\":\"Second Paid\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"NONE\",\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2,\"isRentGift\":true}"
                ),
            )
        };
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });
        let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands = runner.task_outcome(content, TaskOutcome::Completed(two_unowned()));
        let (gift, _) = only_spawn(&commands);
        runner.task_outcome(gift, TaskOutcome::Completed(NO_GIFTS.to_vec()));
        let (quote, _) = only_spawn(&runner.action(action_id("episode-1")));

        let commands = runner.task_outcome(quote, TaskOutcome::Completed(SECOND_QUOTE.to_vec()));
        let drawn = format!("{:?}", last_screen(&commands));
        assert!(drawn.contains("Second Paid"), "{drawn}");
        assert!(!drawn.contains("heading: \"First Paid\""), "{drawn}");
    }

    #[test]
    fn leaving_reader_restores_the_title_gift_balance() {
        const TWO_GIFTS: &[u8] = br#"{"result":"SUCCESS","data":{"receivedGifts":[{"giftType":"RENT","isReceived":true,"issuedCount":2,"usedCount":0}],"receivableGifts":[]}}"#;
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands = runner.task_outcome(content, TaskOutcome::Completed(content_response()));
        let (gift, _) = only_spawn(&commands);
        runner.task_outcome(gift, TaskOutcome::Completed(TWO_GIFTS.to_vec()));
        assert_eq!(runner.app().gifts.available, Some(2));

        runner.action(action_id("episode-0"));
        assert_eq!(runner.app().gifts.available, Some(2));
        let commands = runner.action(ActionId::BACK);
        let (_, work) = only_spawn(&commands);
        assert_eq!(work, api::title_gifts(41));
        assert_eq!(runner.app().view, View::Episodes);
        assert_eq!(runner.app().gifts.available, Some(2));
    }

    #[test]
    fn leaving_account_cancels_only_history_tasks_and_clears_the_queue() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        let commands = runner.action(action_id(ACCOUNT));
        let (summary, _) = fetch_task_with(&commands, "/asset/user");
        let (coin, _) = fetch_task_with(&commands, "coinKind=COIN");
        let (ticket, _) = fetch_task_with(&commands, "coinKind=TICKET");
        let generation = runner.app().wallet.detail_generation;

        let commands = runner.action(ActionId::BACK);

        assert!(commands.contains(&Command::Cancel(coin)));
        assert!(commands.contains(&Command::Cancel(ticket)));
        assert!(!commands.contains(&Command::Cancel(summary)));
        assert_eq!(
            runner.app().wallet.detail_generation,
            generation.wrapping_add(1)
        );
        assert!(runner.app().wallet.detail_queue.is_empty());
        assert_eq!(runner.app().wallet.summary_task, Some(summary));
        assert!(runner
            .app()
            .wallet
            .tasks
            .values()
            .all(|purpose| matches!(purpose, WalletTaskPurpose::Summary { .. })));
    }

    #[test]
    fn failed_or_in_progress_gift_refresh_disables_only_gift_purchase() {
        const QUOTE: &[u8] = br#"{"result":"SUCCESS","data":{"contentsId":41,"episodeId":105,"contentsAlias":"hunter_q","episodeAlias":"paid","isAvailable":false,"coinKind":"COIN","rentCoin":1,"possessionCoin":2,"permanentCoin":2,"isRentGift":true,"isPossessionGift":false}}"#;
        let assert_only_gift_disabled = |screen: &Screen| {
            assert!(screen.nodes.iter().any(|node| matches!(
                node,
                Node::Button { label, state: ControlState::Disabled, .. } if label == "Use Gift"
            )));
            for expected in ["Rent · 1 coins", "Buy · 2 coins"] {
                assert!(screen.nodes.iter().any(|node| matches!(
                    node,
                    Node::Button { label, state: ControlState::Enabled, .. }
                        if label == expected
                )));
            }
        };
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });
        let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands = runner.task_outcome(content, TaskOutcome::Completed(content_response()));
        let (gift, _) = only_spawn(&commands);
        let (quote, _) = only_spawn(&runner.action(action_id("episode-4")));
        runner.task_outcome(quote, TaskOutcome::Completed(QUOTE.to_vec()));

        assert_eq!(
            runner.app().gifts.task.as_ref().map(|task| task.id),
            Some(gift)
        );
        assert_only_gift_disabled(&runner.app().screen());

        runner.task_outcome(gift, TaskOutcome::Failed(TaskError::TimedOut));

        assert!(runner.app().gifts.error);
        assert!(runner.app().gifts.task.is_none());
        assert_only_gift_disabled(&runner.app().screen());
    }
    #[test]
    fn accepted_coin_possession_reconciles_and_back_reloads_library() {
        const RECEIPT: &[u8] = br#"{"result":"SUCCESS","data":{"purchaseType":"POSSESSION","contentsAlias":"hunter_q","episodeAlias":"paid","useCoin":2,"useGoldCoin":2,"useBonusCoin":0,"useFreeCoin":0}}"#;
        const EIGHT_COINS: &[u8] = br#"{"result":"SUCCESS","data":{"coinBalance":{"coin":5,"bonusCoin":2,"freeCoin":1},"ticketBalance":{"ticket":3,"bonusTicket":1,"freeTicket":0}}}"#;
        let owned_content = || {
            detail_response(
                41,
                "hunter_q",
                "Localized Title",
                "{\"id\":105,\"alias\":\"paid\",\"title\":\"Paid Episode\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"POSSESSION\",\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2,\"isRentGift\":true}",
            )
        };
        let (mut runner, post) = runner_waiting_for_buy_post();

        let commands = runner.task_outcome(post, TaskOutcome::Completed(RECEIPT.to_vec()));
        let (content, _) = fetch_task_with(&commands, "/detail/hunter_q");
        let (wallet, _) = fetch_task_with(&commands, "/asset/user");
        assert_eq!(spawns(&commands).len(), 2);
        let commands = runner.task_outcome(content, TaskOutcome::Completed(owned_content()));
        assert_no_post_or_marker_forget(&commands);
        let commands = runner.task_outcome(wallet, TaskOutcome::Completed(EIGHT_COINS.to_vec()));
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::Store(StoreRequest::Forget { .. }))));
        runner.store_result(StoreResult::Forgotten {
            key: commerce::MARKER_KEY.to_owned(),
        });
        assert!(!runner.app().library_load.loaded);
        assert_eq!(runner.app().view, View::Episodes);

        let commands = runner.action(ActionId::BACK);
        let (_, work) = only_spawn(&commands);
        assert_eq!(work, api::library(0));
        assert_eq!(runner.app().view, View::Main);
        assert!(runner.app().comics.is_empty());
    }

    #[test]
    fn ambiguous_purchase_with_entitlement_and_exact_delta_clears_without_repost() {
        const EIGHT_COINS: &[u8] = br#"{"result":"SUCCESS","data":{"coinBalance":{"coin":5,"bonusCoin":2,"freeCoin":1},"ticketBalance":{"ticket":3,"bonusTicket":1,"freeTicket":0}}}"#;
        const ONE_GIFT: &[u8] = br#"{"result":"SUCCESS","data":{"receivedGifts":[{"giftType":"RENT","isReceived":true,"issuedCount":1,"usedCount":0}],"receivableGifts":[]}}"#;
        let owned_content = || {
            detail_response(
                41,
                "hunter_q",
                "Localized Title",
                "{\"id\":105,\"alias\":\"paid\",\"title\":\"Paid Episode\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"POSSESSION\",\"paid\":true,\"coinKind\":\"COIN\",\"rentCoin\":1,\"possessionCoin\":2,\"isRentGift\":true}",
            )
        };
        let (mut runner, post) = runner_waiting_for_buy_post();

        let commands = runner.task_outcome(post, TaskOutcome::Failed(TaskError::TimedOut));
        let (content, _) = fetch_task_with(&commands, "/detail/hunter_q");
        let (wallet, _) = fetch_task_with(&commands, "/asset/user");
        let (gift, _) = fetch_task_with(&commands, "/gift/contents/detail?");
        assert_eq!(spawns(&commands).len(), 3);
        assert!(spawns(&commands)
            .iter()
            .all(|(_, work)| !matches!(work, Task::Post { .. })));
        let commands = runner.task_outcome(gift, TaskOutcome::Completed(ONE_GIFT.to_vec()));
        assert_no_post_or_marker_forget(&commands);
        let commands = runner.task_outcome(content, TaskOutcome::Completed(owned_content()));
        assert_no_post_or_marker_forget(&commands);
        let commands = runner.task_outcome(wallet, TaskOutcome::Completed(EIGHT_COINS.to_vec()));
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::Store(StoreRequest::Forget { .. }))));
        assert!(spawns(&commands)
            .iter()
            .all(|(_, work)| !matches!(work, Task::Post { .. })));
    }

    #[test]
    fn mismatched_receipt_total_is_ambiguous_and_contradiction_stays_locked() {
        const MISMATCHED: &[u8] = br#"{"result":"SUCCESS","data":{"purchaseType":"POSSESSION","contentsAlias":"hunter_q","episodeAlias":"paid","useCoin":1,"useGoldCoin":1,"useBonusCoin":0,"useFreeCoin":0}}"#;
        const EIGHT_COINS: &[u8] = br#"{"result":"SUCCESS","data":{"coinBalance":{"coin":5,"bonusCoin":2,"freeCoin":1},"ticketBalance":{"ticket":3,"bonusTicket":1,"freeTicket":0}}}"#;
        const ONE_GIFT: &[u8] = br#"{"result":"SUCCESS","data":{"receivedGifts":[{"giftType":"RENT","isReceived":true,"issuedCount":1,"usedCount":0}],"receivableGifts":[]}}"#;
        let (mut runner, post) = runner_waiting_for_buy_post();

        let commands = runner.task_outcome(post, TaskOutcome::Completed(MISMATCHED.to_vec()));
        let (content, _) = fetch_task_with(&commands, "/detail/hunter_q");
        let (wallet, _) = fetch_task_with(&commands, "/asset/user");
        let (gift, _) = fetch_task_with(&commands, "/gift/contents/detail?");
        assert_eq!(spawns(&commands).len(), 3);
        assert_no_post_or_marker_forget(&commands);
        let commands = runner.task_outcome(gift, TaskOutcome::Completed(ONE_GIFT.to_vec()));
        assert_no_post_or_marker_forget(&commands);
        let commands = runner.task_outcome(content, TaskOutcome::Completed(content_response()));
        assert_no_post_or_marker_forget(&commands);
        let commands = runner.task_outcome(wallet, TaskOutcome::Completed(EIGHT_COINS.to_vec()));
        assert_eq!(
            runner.app().commerce.state(),
            commerce::CommerceState::AcceptedButStale
        );
        assert_no_post_or_marker_forget(&commands);
    }

    #[test]
    fn offline_and_different_account_leave_accepted_mutations_locked() {
        let (mut offline, post) = runner_waiting_for_paid_rent_post();
        let commands = offline.task_outcome(post, TaskOutcome::Failed(TaskError::Offline));
        assert_eq!(
            offline.app().commerce.state(),
            commerce::CommerceState::AcceptedButStale
        );
        assert_no_post_or_marker_forget(&commands);
        assert!(spawns(&commands).is_empty());

        let (mut switched, post) = runner_waiting_for_paid_rent_post();
        let commands = switched.lifecycle(Lifecycle::Background);
        assert!(commands.contains(&Command::Cancel(post)));
        let commands = switched.lifecycle(Lifecycle::Foreground);
        let (scope_task, work) = only_spawn(&commands);
        assert_eq!(work, api::account_scope());
        let commands = switched.task_outcome(
            scope_task,
            TaskOutcome::Completed(b"ffeeddccbbaa99887766554433221100".to_vec()),
        );
        assert_no_post_or_marker_forget(&commands);
        assert_eq!(
            switched.app().commerce.state(),
            commerce::CommerceState::AcceptedButStale
        );
        assert_eq!(
            switched.app().account_scope,
            Some(test_scope(b"ffeeddccbbaa99887766554433221100"))
        );
        assert_eq!(switched.app().view, View::Episodes);
    }

    #[test]
    fn saturated_task_capacity_fails_quote_closed_without_save_or_post() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.store_result(StoreResult::Loaded {
            key: commerce::MARKER_KEY.to_owned(),
            value: None,
        });
        let (content, _) = only_spawn(&runner.action(action_id("comic-0")));
        let commands = runner.task_outcome(content, TaskOutcome::Completed(content_response()));
        fetch_task_with(&commands, "/gift/contents/detail?");
        runner.app_mut().view = View::Main;
        runner.action(action_id(ACCOUNT));
        assert_eq!(runner.tasks_in_flight(), 4);
        let app = runner.app_mut();
        app.view = View::Episodes;
        app.problem = None;

        let commands = runner.action(action_id("episode-4"));

        assert_eq!(runner.app().commerce.state(), commerce::CommerceState::Idle);
        assert_eq!(runner.app().view, View::Episodes);
        assert!(spawns(&commands).is_empty());
        assert_no_post_or_marker_forget(&commands);
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::Store(StoreRequest::Save { .. }))));
    }

    fn full_collection_comic(index: usize) -> model::FeatureComic {
        model::FeatureComic {
            alias: format!("full-{index}"),
            title: format!(
                "A deliberately long collection title {index} that must stay on one rendered line"
            ),
            creators: format!(
                "A deliberately long creator credit {index} that must stay on one rendered line"
            ),
            view_count: match index {
                0 => Some(1_200),
                1 => Some(0),
                _ => Some(u64::try_from(index + 1).expect("small fixture")),
            },
            vertical_url: Some(format!(
                "https://image.balcony.studio/tw/contents/full-{index}-vertical.webp"
            )),
            square_url: Some(format!(
                "https://image.balcony.studio/tw/contents/full-{index}-square.webp"
            )),
        }
    }

    fn full_collection_app(account: AccountState, count: usize) -> Bomtoon {
        let comics = (0..count).map(full_collection_comic).collect::<Vec<_>>();
        let shared = comics.first().cloned().into_iter().collect::<Vec<_>>();
        let collections = vec![
            model::FeatureCollection {
                id: "ranking".to_owned(),
                label: "排行榜".to_owned(),
                priority: 5,
                order: 0,
                comics: comics.clone(),
            },
            model::FeatureCollection {
                id: "theme-shared".to_owned(),
                label: "Shared alias".to_owned(),
                priority: 9,
                order: 0,
                comics: shared,
            },
        ];
        Bomtoon {
            account,
            view: View::Main,
            destination: MainDestination::Featured,
            featured: FeaturedState {
                generation: 7,
                snapshot_generation: 7,
                snapshot: Some(FeatureSnapshot {
                    banners: Vec::new(),
                    collections,
                    sources: BTreeMap::new(),
                    failed_sources: BTreeSet::new(),
                    warning: None,
                }),
                ..FeaturedState::default()
            },
            ..Bomtoon::default()
        }
    }

    fn cache_all_feature_covers(app: &mut Bomtoon) {
        let urls = app
            .featured
            .snapshot()
            .into_iter()
            .flat_map(|snapshot| {
                snapshot
                    .banners
                    .iter()
                    .chain(
                        snapshot
                            .collections
                            .iter()
                            .flat_map(|collection| collection.comics.iter()),
                    )
                    .flat_map(|comic| {
                        [comic.square_url.clone(), comic.vertical_url.clone()]
                            .into_iter()
                            .flatten()
                    })
            })
            .collect::<BTreeSet<_>>();
        for (index, url) in urls.into_iter().enumerate() {
            let index = u32::try_from(index).expect("cover fixture index fits u32");
            let handle = 1_000_u32
                .checked_add(index)
                .expect("cover fixture handle fits u32");
            app.covers.entries.insert(
                url,
                CoverState::Ready(TilePicture::new(PictureHandle(handle), 300, 300)),
            );
        }
    }

    fn collection_detail_tasks(runner: &AppRunner<Bomtoon>) -> Vec<(TaskId, String)> {
        runner
            .app()
            .feature_tasks
            .iter()
            .filter_map(|(task, purpose)| match purpose {
                FeatureTaskPurpose::CollectionDetail { alias, .. } => Some((*task, alias.clone())),
                FeatureTaskPurpose::Source { .. } | FeatureTaskPurpose::BannerDetail { .. } => None,
            })
            .collect()
    }

    fn long_detail_response(alias: &str) -> Vec<u8> {
        let synopsis = format!(
            "Synopsis for {alias}. {}",
            "Long bounded synopsis text ".repeat(30)
        );
        format!(
            r#"<meta property="og:title" content="Detail {alias} - 漫畫 - BOMTOON"><meta property="og:description" content="{synopsis}">"#
        )
        .into_bytes()
    }

    fn settle_open_collection_window(
        runner: &mut AppRunner<Bomtoon>,
        failed_alias: Option<&str>,
    ) -> Vec<Command> {
        let mut last = Vec::new();
        loop {
            let unsettled = runner
                .app()
                .featured
                .collection
                .as_ref()
                .is_some_and(|view| {
                    !view.queued_aliases.is_empty() || !view.pending_aliases.is_empty()
                });
            if !unsettled {
                break;
            }
            let (task, alias) = collection_detail_tasks(runner)
                .into_iter()
                .next()
                .expect("unsettled collection has an active detail task");
            let outcome = if failed_alias == Some(alias.as_str()) {
                TaskOutcome::Failed(TaskError::Offline)
            } else {
                TaskOutcome::Completed(long_detail_response(&alias))
            };
            last = runner.task_outcome(task, outcome);
        }
        last
    }

    fn collection_rows(screen: &Screen) -> &[kobo_sdk::Row] {
        screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows.as_slice()),
                _ => None,
            })
            .expect("collection rows")
    }

    #[test]
    fn collection_detail_requests_begin_only_after_heading_tap_and_fill_bounded_capacity() {
        let mut runner = AppRunner::with_metrics(
            full_collection_app(AccountState::SignedOut, 14),
            CLARA_BW_METRICS,
        );
        assert!(collection_detail_tasks(&runner).is_empty());

        let commands = runner.action(action_id(&collection_action("ranking")));
        let active = collection_detail_tasks(&runner);

        assert_eq!(runner.app().view, View::FeatureCollection);
        assert_eq!(active.len(), 4);
        assert_eq!(
            runner
                .app()
                .featured
                .collection
                .as_ref()
                .expect("collection")
                .queued_aliases
                .len(),
            2
        );
        assert!(active.iter().all(|(_, alias)| spawns(&commands)
            .iter()
            .any(|(_, work)| { work == &api::public_detail(alias) })));
        assert!(spawns(&commands).len() <= 4);
    }

    #[test]
    fn scheduler_collection_details_enter_shared_wallet_priority() {
        let mut app = full_collection_app(AccountState::Active, 14);
        app.wallet.summary_refresh_queued = true;
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);

        let commands = runner.action(action_id(&collection_action("ranking")));
        let spawned = spawns(&commands);

        assert_eq!(
            spawned.first().map(|(_, work)| work),
            Some(&api::asset_summary())
        );
        assert_eq!(collection_detail_tasks(&runner).len(), 3);
        let summary = spawned[0].0;

        let commands =
            runner.task_outcome(summary, TaskOutcome::Completed(ASSET_RESPONSE.to_vec()));

        assert_eq!(collection_detail_tasks(&runner).len(), 4);
        assert!(spawns(&commands)
            .iter()
            .all(|(_, work)| matches!(work, Task::Fetch { url, .. } if url.contains("/detail/"))));
    }

    #[test]
    fn collection_detail_window_requests_only_uncached_aliases_and_shares_alias_cache() {
        let mut app = full_collection_app(AccountState::SignedOut, 8);
        app.featured.detail_cache.insert(
            "full-0".to_owned(),
            feature::DetailState::Ready(model::PublicDetail {
                alias: "full-0".to_owned(),
                title: "Cached".to_owned(),
                synopsis: Some("Cached synopsis".to_owned()),
            }),
        );
        app.featured
            .detail_cache
            .insert("full-1".to_owned(), feature::DetailState::Failed);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);

        runner.action(action_id(&collection_action("ranking")));
        assert_eq!(
            collection_detail_tasks(&runner)
                .iter()
                .map(|(_, alias)| alias.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["full-2", "full-3", "full-4", "full-5"])
        );
        settle_open_collection_window(&mut runner, None);
        runner.action(ActionId::BACK);
        let commands = runner.action(action_id(&collection_action("theme-shared")));

        assert!(spawns(&commands)
            .iter()
            .all(|(_, work)| work != &api::public_detail("full-0")));
        assert!(collection_detail_tasks(&runner).is_empty());
    }

    #[test]
    fn collection_boundary_waits_for_every_queued_and_pending_detail() {
        let mut runner = AppRunner::with_metrics(
            full_collection_app(AccountState::SignedOut, 6),
            CLARA_BW_METRICS,
        );
        runner.action(action_id(&collection_action("ranking")));

        for settled in 0..6 {
            let (task, alias) = collection_detail_tasks(&runner)[0].clone();
            runner.task_outcome(task, TaskOutcome::Completed(long_detail_response(&alias)));
            let pages = &runner
                .app()
                .featured
                .collection
                .as_ref()
                .expect("collection")
                .pages;
            assert_eq!(pages.is_empty(), settled < 5);
        }
        let range = runner
            .app()
            .featured
            .collection
            .as_ref()
            .expect("collection")
            .pages[0]
            .clone();
        assert!(range.start == 0 && range.end > 0 && range.end < 6);
    }

    #[test]
    fn adaptive_collection_screen_renders_exact_bounded_rows_counts_actions_and_square_crops() {
        let mut app = full_collection_app(AccountState::SignedOut, 6);
        for index in 0..6 {
            let index_u32 = u32::try_from(index).expect("cover fixture index fits u32");
            let handle = 70_u32
                .checked_add(index_u32)
                .expect("cover fixture handle fits u32");
            let url = format!("https://image.balcony.studio/tw/contents/full-{index}-square.webp");
            app.covers.entries.insert(
                url,
                CoverState::Ready(TilePicture::new(PictureHandle(handle), 300, 180)),
            );
        }
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.action(action_id(&collection_action("ranking")));
        let commands = settle_open_collection_window(&mut runner, Some("full-1"));
        let screen = last_screen(&commands);
        let rows = collection_rows(&screen);

        assert!(!rows.is_empty());
        assert_eq!(rows[0].title, full_collection_comic(0).title);
        assert_eq!(rows[0].summary, full_collection_comic(0).creators);
        assert!(rows[0].description.starts_with("Synopsis for full-0"));
        assert_eq!(rows[0].trailing.as_deref(), Some("1.2K"));
        assert_eq!(rows[0].action, action_id(&comic_action("ranking", 0)));
        assert_eq!(rows[0].line_limits, kobo_sdk::RowLineLimits::new(1, 1, 2));
        assert!(matches!(
            rows[0].lead,
            RowLead::Picture(
                TilePicture {
                    fit: kobo_sdk::PictureFit::Cover,
                    ..
                },
                Glyph::Book
            )
        ));
        if let Some(failed) = rows
            .iter()
            .find(|row| row.action == action_id(&comic_action("ranking", 1)))
        {
            assert!(failed.description.is_empty());
            assert_eq!(failed.trailing, None);
            assert_eq!(failed.state, kobo_sdk::RowState::Open);
        }
        let visible_range = runner
            .app()
            .featured
            .collection
            .as_ref()
            .expect("collection")
            .pages[0]
            .clone();
        assert_eq!(
            runner.app().covers.visible_urls,
            visible_range
                .map(|index| {
                    format!("https://image.balcony.studio/tw/contents/full-{index}-square.webp")
                })
                .collect::<Vec<_>>()
        );
        assert!(screen.nav_bar.is_none());
        assert!(screen.owns_back);
        assert_fits(&screen);
    }

    #[test]
    fn unknown_collection_total_shows_current_page_and_discovered_turns() {
        let mut app = full_collection_app(AccountState::SignedOut, 12);
        cache_all_feature_covers(&mut app);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.action(action_id(&collection_action("ranking")));
        let commands = settle_open_collection_window(&mut runner, None);
        let first = last_screen(&commands);

        assert_eq!(
            first.page_turns.as_ref().and_then(|turns| turns.position),
            Some((1, 0))
        );
        let first_layout = first.layout_with(&CLARA_BW_METRICS, &Chrome::default());
        let first_position = first_layout
            .nodes
            .iter()
            .find(|node| {
                node.text_lines.as_slice() == ["1"]
                    && node.rect.y >= first_layout.content.y + first_layout.content.height
            })
            .expect("unknown-total page position");
        assert_eq!(first_position.text_lines, ["1"]);
        assert!(!first_layout
            .nodes
            .iter()
            .any(|node| { node.kind.acts_on() == Some(action_id(PREVIOUS_PAGE)) }));
        assert!(first_layout
            .nodes
            .iter()
            .any(|node| { node.kind.acts_on() == Some(action_id(NEXT_PAGE)) }));

        runner.action(action_id(NEXT_PAGE));
        let commands = settle_open_collection_window(&mut runner, None);
        let second = last_screen(&commands);
        assert_eq!(
            second.page_turns.as_ref().and_then(|turns| turns.position),
            Some((2, 0))
        );
        let second_layout = second.layout_with(&CLARA_BW_METRICS, &Chrome::default());
        assert!(second_layout
            .nodes
            .iter()
            .any(|node| { node.kind.acts_on() == Some(action_id(PREVIOUS_PAGE)) }));
        assert!(second_layout
            .nodes
            .iter()
            .any(|node| { node.kind.acts_on() == Some(action_id(NEXT_PAGE)) }));
        assert_fits(&second);
    }

    #[test]
    fn collection_failed_synopses_remain_actionable_and_zero_or_missing_counts_have_no_trailing() {
        let mut app = full_collection_app(AccountState::SignedOut, 3);
        let comics = &mut app
            .featured
            .snapshot
            .as_mut()
            .expect("snapshot")
            .collections[0]
            .comics;
        for (index, comic) in comics.iter_mut().enumerate() {
            comic.title = format!("Title {index}");
            comic.creators = "Creator".to_owned();
            comic.view_count = match index {
                0 => None,
                1 => Some(0),
                _ => Some(999),
            };
            app.featured
                .detail_cache
                .insert(comic.alias.clone(), feature::DetailState::Failed);
        }
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);

        let commands = runner.action(action_id(&collection_action("ranking")));
        let screen = last_screen(&commands);
        let rows = collection_rows(&screen);

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].trailing, None);
        assert_eq!(rows[1].trailing, None);
        assert_eq!(rows[2].trailing.as_deref(), Some("999"));
        assert!(rows
            .iter()
            .all(|row| { row.description.is_empty() && row.state == kobo_sdk::RowState::Open }));
        assert!(collection_detail_tasks(&runner).is_empty());
        assert_fits(&screen);
    }

    #[test]
    fn collection_cover_fallback_retains_the_picture_slot_for_every_unready_state() {
        let url = "https://image.balcony.studio/tw/contents/fallback.webp";
        let mut covers = CoverCache::default();

        assert_eq!(
            collection_cover_lead(&covers, None),
            RowLead::CoverSlot(Glyph::Book)
        );
        covers
            .entries
            .insert(url.to_owned(), CoverState::Loading(TaskId(1)));
        assert_eq!(
            collection_cover_lead(&covers, Some(url)),
            RowLead::CoverSlot(Glyph::Book)
        );
        covers.entries.insert(url.to_owned(), CoverState::Failed);
        assert_eq!(
            collection_cover_lead(&covers, Some(url)),
            RowLead::CoverSlot(Glyph::Book)
        );

        let picture = TilePicture::new(PictureHandle(9), 300, 180);
        covers
            .entries
            .insert(url.to_owned(), CoverState::Ready(picture));
        assert_eq!(
            collection_cover_lead(&covers, Some(url)),
            RowLead::Picture(picture.with_fit(PictureFit::Cover), Glyph::Book)
        );
    }

    #[test]
    fn collection_next_reuses_cached_overflow_before_requesting_only_new_aliases() {
        let mut app = full_collection_app(AccountState::SignedOut, 12);
        cache_all_feature_covers(&mut app);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.action(action_id(&collection_action("ranking")));
        settle_open_collection_window(&mut runner, None);
        let first_range = runner
            .app()
            .featured
            .collection
            .as_ref()
            .expect("collection")
            .pages[0]
            .clone();
        let first_end = first_range.end;
        assert!(first_end < 6);

        runner.action(action_id(NEXT_PAGE));
        let mut aliases = collection_detail_tasks(&runner)
            .into_iter()
            .map(|(_, alias)| alias)
            .collect::<BTreeSet<_>>();
        aliases.extend(
            runner
                .app()
                .featured
                .collection
                .as_ref()
                .expect("collection")
                .queued_aliases
                .iter()
                .cloned(),
        );
        let expected = (6..first_end.saturating_add(6).min(12))
            .map(|index| format!("full-{index}"))
            .collect::<BTreeSet<_>>();
        assert_eq!(aliases, expected);
        settle_open_collection_window(&mut runner, None);
        assert!(
            runner
                .app()
                .featured
                .collection
                .as_ref()
                .expect("collection")
                .pages
                .len()
                > 1
        );
        runner.action(action_id(PREVIOUS_PAGE));
        let view = runner
            .app()
            .featured
            .collection
            .as_ref()
            .expect("collection");
        assert_eq!(view.page, 0);
        assert_eq!(view.pages[0], first_range);
    }

    #[test]
    fn collection_async_boundary_settle_preserves_page_changed_during_discovery() {
        let mut app = full_collection_app(AccountState::SignedOut, 12);
        cache_all_feature_covers(&mut app);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.action(action_id(&collection_action("ranking")));
        settle_open_collection_window(&mut runner, None);
        runner.action(action_id(NEXT_PAGE));
        settle_open_collection_window(&mut runner, None);
        let settled_pages = runner
            .app()
            .featured
            .collection
            .as_ref()
            .expect("collection")
            .pages
            .len();
        assert_eq!(
            runner
                .app()
                .featured
                .collection
                .as_ref()
                .expect("collection")
                .page,
            settled_pages - 1
        );

        runner.action(action_id(NEXT_PAGE));
        assert!(!collection_detail_tasks(&runner).is_empty());
        runner.action(action_id(PREVIOUS_PAGE));
        assert_eq!(
            runner
                .app()
                .featured
                .collection
                .as_ref()
                .expect("collection")
                .page,
            settled_pages.saturating_sub(2)
        );
        settle_open_collection_window(&mut runner, None);
        let view = runner
            .app()
            .featured
            .collection
            .as_ref()
            .expect("collection");

        assert!(view.pages.len() > settled_pages);
        assert_eq!(view.page, settled_pages.saturating_sub(2));
    }

    #[test]
    fn collection_back_is_owned_signed_out_cancels_details_and_restores_exact_feed_page() {
        let mut app = full_collection_app(AccountState::SignedOut, 12);
        app.featured.feed_page = 4;
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        let open = runner.action(action_id(&collection_action("ranking")));
        let details = collection_detail_tasks(&runner)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        let screen = last_screen(&open);
        assert!(screen.owns_back);
        assert!(screen.nav_bar.is_none());

        let commands = runner.action(ActionId::BACK);

        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert_eq!(runner.app().featured.feed_page, 4);
        assert_eq!(runner.app().featured.collection, None);
        assert_eq!(cancelled_tasks(&commands), details);
    }

    #[test]
    fn collection_row_leave_cancels_unresolved_next_window_before_opening_comic() {
        let mut app = full_collection_app(AccountState::Active, 12);
        cache_all_feature_covers(&mut app);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.action(action_id(&collection_action("ranking")));
        settle_open_collection_window(&mut runner, None);
        runner.action(action_id(NEXT_PAGE));
        let details = collection_detail_tasks(&runner)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();

        let commands = runner.action(action_id(&comic_action("ranking", 0)));

        assert!(details.is_subset(&cancelled_tasks(&commands)));
        assert!(
            spawns(&commands)
                .iter()
                .any(|(_, work)| work == &api::detail("full-0"))
                || runner.app().queued_foreground == Some(Pending::Content(0))
        );
        assert_eq!(runner.app().featured.collection, None);
    }

    #[test]
    fn collection_pending_comic_request_leaves_collection_before_back_and_late_outcome() {
        let mut app = full_collection_app(AccountState::Active, 6);
        cache_all_feature_covers(&mut app);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        runner.action(action_id(&collection_action("ranking")));
        settle_open_collection_window(&mut runner, None);

        let commands = runner.action(action_id(&comic_action("ranking", 0)));
        let (content, work) = only_spawn(&commands);
        assert_eq!(work, api::detail("full-0"));
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().featured.collection, None);
        assert_eq!(runner.app().pending, Some(Pending::Content(0)));

        let commands = runner.action(ActionId::BACK);
        assert!(cancelled_tasks(&commands).is_empty());
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().pending, Some(Pending::Content(0)));
        assert_eq!(runner.app().task, Some(content));

        runner.task_outcome(
            content,
            TaskOutcome::Completed(detail_response(
                1,
                "full-0",
                "Full 0",
                "{\"id\":1,\"alias\":\"episode-0\",\"title\":\"Episode 0\",\"openedAt\":1709136000000,\"isSample\":false,\"purchaseStatus\":\"POSSESSION\"}",
            )),
        );
        assert_eq!(runner.app().view, View::Episodes);
    }

    #[test]
    fn collection_suspend_and_exit_cancel_detail_work_and_reject_late_outcomes() {
        for exit in [false, true] {
            let mut runner = AppRunner::with_metrics(
                full_collection_app(AccountState::SignedOut, 8),
                CLARA_BW_METRICS,
            );
            runner.action(action_id(&collection_action("ranking")));
            let (late, alias) = collection_detail_tasks(&runner)[0].clone();
            let active = collection_detail_tasks(&runner)
                .into_iter()
                .map(|(task, _)| task)
                .collect::<BTreeSet<_>>();

            let commands = if exit {
                runner.exit()
            } else {
                runner.suspend()
            };
            assert!(active.is_subset(&cancelled_tasks(&commands)));
            let generation = runner.app().featured.detail_generation;
            runner.task_outcome(late, TaskOutcome::Completed(long_detail_response(&alias)));
            assert_eq!(runner.app().featured.detail_generation, generation);
            assert!(!matches!(
                runner.app().featured.detail_cache.get(&alias),
                Some(feature::DetailState::Ready(_))
            ));
        }
    }

    #[test]
    fn collection_sign_out_transition_cancels_loading_details_and_retains_settled_public_cache() {
        let mut runner = AppRunner::with_metrics(
            full_collection_app(AccountState::Active, 8),
            CLARA_BW_METRICS,
        );
        runner.action(action_id(&collection_action("ranking")));
        runner.resume();
        let mut scope = None;
        for _ in 0..6 {
            let (task, alias) = collection_detail_tasks(&runner)[0].clone();
            let commands =
                runner.task_outcome(task, TaskOutcome::Completed(long_detail_response(&alias)));
            scope = spawns(&commands)
                .into_iter()
                .find_map(|(task, work)| (work == api::account_scope()).then_some(task));
            if scope.is_some() {
                break;
            }
        }
        let scope = scope.expect("scope request resumes after a detail slot opens");
        let active = collection_detail_tasks(&runner)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        let settled = runner
            .app()
            .featured
            .detail_cache
            .values()
            .filter(|state| matches!(state, feature::DetailState::Ready(_)))
            .count();
        assert!(!active.is_empty());
        assert!(settled > 0);

        let commands = runner.task_outcome(scope, TaskOutcome::Failed(TaskError::NoCredential));

        assert!(active.is_subset(&cancelled_tasks(&commands)));
        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().featured.collection, None);
        assert_eq!(
            runner
                .app()
                .featured
                .detail_cache
                .values()
                .filter(|state| matches!(state, feature::DetailState::Ready(_)))
                .count(),
            settled
        );
        assert!(runner
            .app()
            .featured
            .detail_cache
            .values()
            .all(|state| !matches!(state, feature::DetailState::Loading(_))));
    }

    #[test]
    fn collection_detail_outcomes_require_snapshot_collection_generation_and_id() {
        for mismatch in ["snapshot", "collection-generation", "collection-id"] {
            let mut runner = AppRunner::with_metrics(
                full_collection_app(AccountState::SignedOut, 6),
                CLARA_BW_METRICS,
            );
            runner.action(action_id(&collection_action("ranking")));
            let (task, alias) = collection_detail_tasks(&runner)[0].clone();
            match mismatch {
                "snapshot" => {
                    let generation = runner.app().featured.snapshot_generation;
                    runner.app_mut().featured.snapshot_generation = generation.wrapping_add(1);
                }
                "collection-generation" => {
                    let collection = runner
                        .app_mut()
                        .featured
                        .collection
                        .as_mut()
                        .expect("collection");
                    collection.generation = collection.generation.wrapping_add(1);
                }
                "collection-id" => {
                    runner
                        .app_mut()
                        .featured
                        .collection
                        .as_mut()
                        .expect("collection")
                        .collection_id = "another".to_owned();
                }
                _ => unreachable!(),
            }

            runner.task_outcome(task, TaskOutcome::Completed(long_detail_response(&alias)));

            assert!(!matches!(
                runner.app().featured.detail_cache.get(&alias),
                Some(feature::DetailState::Ready(_))
            ));
            assert!(runner
                .app()
                .featured
                .collection
                .as_ref()
                .expect("collection")
                .pages
                .is_empty());
        }
    }

    #[test]
    fn published_snapshot_clears_ready_and_failed_detail_cache_before_reopen() {
        let mut app = ready_featured(local_day(30), "old");
        app.featured.snapshot_generation = app.featured.generation;
        app.featured.detail_cache.insert(
            "new-ranking".to_owned(),
            feature::DetailState::Ready(model::PublicDetail {
                alias: "new-ranking".to_owned(),
                title: "Stale ready".to_owned(),
                synopsis: Some("Stale ready synopsis".to_owned()),
            }),
        );
        app.featured
            .detail_cache
            .insert("new-free".to_owned(), feature::DetailState::Failed);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        observe_day_with_runner(&mut runner, local_day(31));

        complete_feature_batch(&mut runner, "new", None);

        assert!(runner.app().featured.detail_cache.is_empty());
        runner.action(action_id(&collection_action("ranking")));
        let mut requested = collection_detail_tasks(&runner)
            .into_iter()
            .map(|(_, alias)| alias)
            .collect::<BTreeSet<_>>();
        requested.extend(
            runner
                .app()
                .featured
                .collection
                .as_ref()
                .expect("ranking collection")
                .queued_aliases
                .iter()
                .cloned(),
        );
        assert!(requested.contains("new-ranking"));
        runner.action(ActionId::BACK);
        runner.action(action_id(&collection_action("freetime")));
        let mut requested = collection_detail_tasks(&runner)
            .into_iter()
            .map(|(_, alias)| alias)
            .collect::<BTreeSet<_>>();
        requested.extend(
            runner
                .app()
                .featured
                .collection
                .as_ref()
                .expect("freetime collection")
                .queued_aliases
                .iter()
                .cloned(),
        );
        assert!(requested.contains("new-free"));
    }

    #[test]
    fn published_snapshot_clamps_stable_collection_origin_before_back() {
        let mut app = grouped_feature_app(None);
        app.account = AccountState::SignedOut;
        app.featured.loaded_day = Some(local_day(30));
        app.featured.snapshot_generation = app.featured.generation;
        let old_pages = featured_feed_pages(&app.featured, &CLARA_BW_METRICS).len();
        assert!(old_pages > 1);
        let origin = old_pages - 1;
        app.featured.feed_page = origin;
        cache_all_feature_covers(&mut app);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        observe_day_with_runner(&mut runner, local_day(31));
        runner.app_mut().featured.feed_page = origin;
        runner.action(action_id(&collection_action("freetime")));

        complete_feature_batch(&mut runner, "replacement", None);
        let new_pages = featured_feed_pages(&runner.app().featured, &CLARA_BW_METRICS).len();
        assert!(new_pages < old_pages);
        let expected_origin = origin.min(new_pages.saturating_sub(1));
        assert_eq!(
            runner
                .app()
                .featured
                .collection
                .as_ref()
                .expect("stable collection remains open")
                .origin_feed_page,
            expected_origin
        );

        runner.action(ActionId::BACK);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().featured.feed_page, expected_origin);
    }

    #[test]
    fn published_snapshot_resets_stable_open_collection_and_cancels_old_details() {
        let mut app = ready_featured(local_day(30), "old");
        app.featured.snapshot_generation = app.featured.generation;
        app.featured
            .snapshot
            .as_mut()
            .expect("snapshot")
            .collections
            .iter_mut()
            .find(|collection| collection.id == "ranking")
            .expect("ranking")
            .comics = (0..8).map(full_collection_comic).collect();
        cache_all_feature_covers(&mut app);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        observe_day_with_runner(&mut runner, local_day(31));
        runner.action(action_id(&collection_action("ranking")));
        let old_collection_generation = runner
            .app()
            .featured
            .collection
            .as_ref()
            .expect("collection")
            .generation;
        for source in FEATURE_SOURCES.into_iter().take(4) {
            settle_source(
                &mut runner,
                source,
                TaskOutcome::Completed(source_response(source, "new")),
            );
        }
        let old_details = collection_detail_tasks(&runner)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<BTreeSet<_>>();
        assert!(!old_details.is_empty());

        let commands = settle_source(
            &mut runner,
            FeatureSource::Freetime,
            TaskOutcome::Completed(source_response(FeatureSource::Freetime, "new")),
        );
        let collection = runner
            .app()
            .featured
            .collection
            .as_ref()
            .expect("stable collection remains open");

        assert_eq!(runner.app().view, View::FeatureCollection);
        assert_eq!(collection.collection_id, "ranking");
        assert_ne!(collection.generation, old_collection_generation);
        assert!(collection.pages.is_empty());
        assert_eq!(collection.window_start, 0);
        assert_eq!(collection.window_end, 1);
        assert!(old_details.is_subset(&cancelled_tasks(&commands)));
        let mut aliases = collection_detail_tasks(&runner)
            .into_iter()
            .map(|(_, alias)| alias)
            .collect::<BTreeSet<_>>();
        aliases.extend(collection.queued_aliases.iter().cloned());
        assert!(aliases.contains("new-ranking"));
    }

    #[test]
    fn published_snapshot_closes_missing_open_collection_to_clamped_origin_feed_page() {
        let mut app = grouped_feature_app(None);
        app.account = AccountState::SignedOut;
        app.featured.loaded_day = Some(local_day(30));
        app.featured.snapshot_generation = app.featured.generation;
        let old_pages = featured_feed_pages(&app.featured, &CLARA_BW_METRICS).len();
        app.featured.feed_page = old_pages.saturating_sub(1);
        let origin = app.featured.feed_page;
        cache_all_feature_covers(&mut app);
        let mut runner = AppRunner::with_metrics(app, CLARA_BW_METRICS);
        observe_day_with_runner(&mut runner, local_day(31));
        runner.app_mut().featured.feed_page = origin;
        runner.action(action_id(&collection_action("theme-20")));
        for source in FEATURE_SOURCES {
            settle_source(
                &mut runner,
                source,
                TaskOutcome::Completed(source_response(source, "replacement")),
            );
        }
        let new_pages = featured_feed_pages(&runner.app().featured, &CLARA_BW_METRICS).len();

        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert_eq!(runner.app().featured.collection, None);
        assert_eq!(
            runner.app().featured.feed_page,
            origin.min(new_pages.saturating_sub(1))
        );
    }
}
