mod api;
mod model;
mod parse;

use kobo_image::{Picture, PictureFormat, PicturePixels, PicturePixelsRef, PANEL_GREYS};
use kobo_sdk::{
    action_id, ActionId, BannerLevel, Chrome, Context, DeviceRequest, DeviceResult, Failure, Glyph,
    KoboApp, LocalDay, PictureHandle, ReadingChrome, RowLead, Screen, ScreenBuilder, TaskError,
    TaskId, TaskOutcome, TilePicture, TileShape, CLARA_BW_METRICS,
};
use model::{
    display_text, AssetKind, AssetSubtype, Comic, Episode, EpisodeImage, ExpirationRow, Homepage,
    RecentEntry, ShelfComic, WalletSummary,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
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
const LIBRARY_ITEMS_PER_PAGE: usize = 6;
const EPISODE_ITEMS_PER_PAGE: usize = 6;
const ACCOUNT_HISTORY_ITEMS_PER_PAGE: usize = 3;
const HISTORY_WINDOW_MS: i64 = 90 * 24 * 60 * 60 * 1_000;
const READER_PREVIOUS: &str = "reader-previous";
const READER_NEXT: &str = "reader-next";
const READER_CHROME: &str = "reader-chrome";
const MAIN_DESTINATIONS: [(&str, &str); 3] =
    [(FEATURED, "Featured"), (RECENT, "Recent"), (LIBRARY, "Library")];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum View {
    #[default]
    Status,
    Main,
    Account,
    Episodes,
    Reader,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pending {
    Library(usize),
    Recent(usize),
    Content(usize),
    Logout,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AccountState {
    #[default]
    Active,
    SignedOut,
    Expired,
    RevocationUnconfirmed,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum FeaturedStatus {
    #[default]
    Unloaded,
    Loading,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct FeaturedRefresh {
    loaded_day: Option<LocalDay>,
    desired_day: Option<LocalDay>,
    active_day: Option<LocalDay>,
    local_day_pending: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct StagedFeatured {
    featured: Vec<ShelfComic>,
    recommended: Vec<ShelfComic>,
    pending_details: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct FeaturedState {
    status: FeaturedStatus,
    generation: u64,
    featured: Vec<ShelfComic>,
    recommended: Vec<ShelfComic>,
    pending_details: usize,
    page: usize,
    stale_warning: Option<String>,
    refresh: FeaturedRefresh,
    staged: Option<StagedFeatured>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ShelfTaskPurpose {
    Homepage {
        generation: u64,
        refresh_day: Option<LocalDay>,
    },
    BannerDetail {
        generation: u64,
        slot: usize,
        alias: String,
    },
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct BannerDetailPlan {
    slot: usize,
    alias: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FeaturedPlan {
    featured: Vec<ShelfComic>,
    recommended: Vec<ShelfComic>,
    details: Vec<BannerDetailPlan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FeaturedPage {
    featured: std::ops::Range<usize>,
    recommended: std::ops::Range<usize>,
}

fn plan_featured(homepage: Homepage) -> FeaturedPlan {
    let Homepage {
        banners,
        newest,
        week_day,
        only_bom,
    } = homepage;
    let mut aliases = BTreeSet::new();
    let mut recommended = Vec::new();
    for comic in newest
        .into_iter()
        .chain(week_day)
        .chain(only_bom)
    {
        if aliases.insert(comic.alias.clone()) {
            recommended.push(comic);
        }
    }

    let mut featured = Vec::new();
    let mut details = Vec::new();
    for banner in banners.into_iter().take(3) {
        if let Some(comic) = recommended
            .iter()
            .find(|comic| comic.alias == banner.alias)
        {
            featured.push(comic.clone());
        } else {
            let slot = featured.len();
            details.push(BannerDetailPlan {
                slot,
                alias: banner.alias.clone(),
            });
            featured.push(ShelfComic {
                title: banner.alias.clone(),
                alias: banner.alias,
                cover_url: None,
            });
        }
    }
    FeaturedPlan {
        featured,
        recommended,
        details,
    }
}

fn featured_page(
    featured: &FeaturedState,
    page: usize,
    first_page_rows: usize,
) -> FeaturedPage {
    let first_page_rows = first_page_rows.max(1);
    if page == 0 {
        return FeaturedPage {
            featured: 0..featured.featured.len().min(3),
            recommended: 0..featured.recommended.len().min(first_page_rows),
        };
    }
    let continuation_rows = if featured.stale_warning.is_some() {
        first_page_rows
    } else {
        6
    };
    let start = first_page_rows
        .saturating_add(
            page.saturating_sub(1)
                .saturating_mul(continuation_rows),
        )
        .min(featured.recommended.len());
    FeaturedPage {
        featured: 0..0,
        recommended: start
            ..start
                .saturating_add(continuation_rows)
                .min(featured.recommended.len()),
    }
}

fn featured_page_count(featured: &FeaturedState, first_page_rows: usize) -> usize {
    let first_page_rows = first_page_rows.max(1);
    let continuation_rows = if featured.stale_warning.is_some() {
        first_page_rows
    } else {
        6
    };
    if featured.recommended.len() <= first_page_rows {
        1
    } else {
        1 + featured
            .recommended
            .len()
            .saturating_sub(first_page_rows)
            .div_ceil(continuation_rows)
    }
}
fn ready_cover(covers: &CoverCache, url: Option<&str>) -> Option<TilePicture> {
    match url.and_then(|url| covers.entries.get(url)) {
        Some(CoverState::Ready(picture)) => Some(*picture),
        Some(CoverState::Loading(_) | CoverState::Failed) | None => None,
    }
}

fn cover_lead(covers: &CoverCache, url: Option<&str>) -> RowLead {
    ready_cover(covers, url).map_or(
        RowLead::Icon(Glyph::Book),
        |picture| RowLead::Picture(picture, Glyph::Book),
    )
}

fn add_featured_feed(
    mut screen: ScreenBuilder,
    featured: &FeaturedState,
    covers: &CoverCache,
    first_page_rows: usize,
) -> ScreenBuilder {
    let page = featured_page(featured, featured.page, first_page_rows);
    if !page.featured.is_empty() {
        screen = screen.picture_tiles(
            TileShape::Portrait,
            page.featured.map(|index| {
                let comic = &featured.featured[index];
                (
                    format!("comic-{index}"),
                    display_text(&comic.title, &comic.alias),
                    Glyph::Book,
                    ready_cover(covers, comic.cover_url.as_deref()),
                )
            }),
        );
    }
    let recommended_offset = featured.featured.len();
    screen = screen
        .section("Recommended")
        .rows(page.recommended.map(|index| {
            let comic = &featured.recommended[index];
            (
                format!("comic-{}", recommended_offset.saturating_add(index)),
                display_text(&comic.title, &comic.alias),
                "",
                cover_lead(covers, comic.cover_url.as_deref()),
            )
        }));
    let pages = featured_page_count(featured, first_page_rows);
    screen
        .page_turns(PREVIOUS_PAGE, NEXT_PAGE)
        .page_position(
            u16::try_from(featured.page.saturating_add(1)).unwrap_or(u16::MAX),
            u16::try_from(pages).unwrap_or(u16::MAX),
        )
}

fn add_featured_content(
    mut screen: ScreenBuilder,
    featured: &FeaturedState,
    covers: &CoverCache,
    first_page_rows: usize,
) -> ScreenBuilder {
    if let Some(warning) = &featured.stale_warning {
        screen = screen
            .banner(BannerLevel::Attention, warning.clone())
            .button(RETRY, "Try again");
    }
    match featured.status {
        FeaturedStatus::Unloaded => screen.text("Featured has not loaded yet."),
        FeaturedStatus::Loading if featured.featured.is_empty() => {
            screen.activity("Loading Featured", None)
        }
        FeaturedStatus::Loading | FeaturedStatus::Ready => {
            add_featured_feed(screen, featured, covers, first_page_rows)
        }
        FeaturedStatus::Failed => screen
            .banner(BannerLevel::Attention, "Featured could not be loaded.")
            .primary_button(RETRY, "Try again"),
    }
}

fn featured_page_zero_rows(featured: &FeaturedState) -> usize {
    featured_page_zero_rows_with_covers(featured, &CoverCache::default())
}

fn featured_page_zero_rows_with_covers(
    featured: &FeaturedState,
    covers: &CoverCache,
) -> usize {
    let mut measuring = featured.clone();
    measuring.page = 0;
    for rows in (1..=6).rev() {
        let mut builder = ScreenBuilder::new("bomtoon-featured-measure")
            .top_bar("Featured")
            .top_bar_action(SIGN_IN, "Sign in");
        if let Some(warning) = &measuring.stale_warning {
            builder = builder
                .banner(BannerLevel::Attention, warning.clone())
                .button(RETRY, "Try again");
        }
        let screen = add_featured_feed(builder, &measuring, covers, rows)
        .nav_bar(MainDestination::Featured.index(), MAIN_DESTINATIONS)
        .build();
        if !screen
            .diagnostics(&CLARA_BW_METRICS, &Chrome::measuring(true))
            .has_errors()
        {
            return rows;
        }
    }
    1
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PageSegment {
    source: usize,
    source_row: u32,
    rows: u32,
    destination_row: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PagePlan {
    segments: Vec<PageSegment>,
    content_rows: u32,
}

#[derive(Debug, Eq, PartialEq)]
struct PageBuild {
    page: usize,
    format: PictureFormat,
    bytes: Vec<u8>,
    next_segment: usize,
}

impl PageBuild {
    fn new(
        page: usize,
        format: PictureFormat,
        panel_width: u32,
        panel_height: u32,
    ) -> Result<Self, String> {
        let byte_len = format
            .byte_len(panel_width, panel_height)
            .ok_or_else(|| "The comic page byte length is not supported.".to_owned())?;
        Ok(Self {
            page,
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
    Foreground { page: usize },
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
    ForegroundSource { source: usize, page: usize },
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

enum PageEntry {
    Building(PageBuild),
    Ready { page: usize, picture: Picture },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Retry {
    #[default]
    Restart,
    Manifest,
    Page(usize),
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
    plans: Vec<PagePlan>,
    page: usize,
    total_pages: u16,
    window: VecDeque<PageEntry>,
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

struct PlannedReaderSpawn {
    source: usize,
    purpose: ReaderTaskPurpose,
    foreground: bool,
    url: String,
}

#[derive(Default)]
struct ReaderMaintenancePlan {
    spawns: Vec<PlannedReaderSpawn>,
    promotion: Option<(TaskId, usize, usize)>,
    refresh_promotion: Option<TaskId>,
    ready: Option<(usize, Picture)>,
}

struct RebasedReaderWindow {
    build: PageBuild,
    cached_sources: BTreeSet<usize>,
    fetches: BTreeMap<usize, TaskId>,
}

#[derive(Default)]
struct Bomtoon {
    account: AccountState,
    view: View,
    destination: MainDestination,
    pending: Option<Pending>,
    task: Option<TaskId>,
    wallet: WalletState,
    featured: FeaturedState,
    shelf_tasks: BTreeMap<TaskId, ShelfTaskPurpose>,
    superseded_shelf_tasks: BTreeSet<TaskId>,
    queued_homepage: Option<(u64, Option<LocalDay>)>,
    covers: CoverCache,
    comics: Vec<Comic>,
    recent: Vec<RecentEntry>,
    episodes: Vec<Episode>,
    selected_content_alias: String,
    selected_title: String,
    reader_selection: Option<EpisodeSelection>,
    reader: Option<ReaderState>,
    reader_generation: u64,
    reader_tasks: BTreeMap<TaskId, ReaderTaskEntry>,
    foreground_reader_task: Option<TaskId>,
    retry: Retry,
    next_picture_handle: u32,
    page: usize,
    next_library_page: Option<usize>,
    next_recent_page: Option<usize>,
    total_library_titles: usize,
    total_recent_titles: usize,
    library_loaded: bool,
    recent_loaded: bool,
    problem: Option<String>,
}

impl Bomtoon {
    fn show(&mut self, context: &mut Context) {
        self.sync_visible_covers(context);
        let owns_back = self.account == AccountState::Active
            && match self.view {
                View::Account | View::Episodes => self.pending.is_none() && self.problem.is_none(),
                View::Reader => true,
                View::Status | View::Main => false,
            };
        context.set_screen(self.screen().with_own_back(owns_back));
    }

    fn visible_cover_urls(&self) -> Vec<String> {
        if self.view != View::Main
            || self.pending.is_some()
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
        match self.destination {
            MainDestination::Featured
                if matches!(
                    self.featured.status,
                    FeaturedStatus::Loading | FeaturedStatus::Ready
                ) =>
            {
                let first_page_rows = featured_page_zero_rows(&self.featured);
                let page = featured_page(&self.featured, self.featured.page, first_page_rows);
                for index in page.featured {
                    push(self.featured.featured[index].cover_url.as_ref());
                }
                for index in page.recommended {
                    push(self.featured.recommended[index].cover_url.as_ref());
                }
            }
            MainDestination::Recent if self.recent_loaded => {
                let (start, end) =
                    page_bounds(self.page, self.recent.len(), LIBRARY_ITEMS_PER_PAGE);
                for entry in &self.recent[start..end] {
                    push(entry.cover_url.as_ref());
                }
            }
            MainDestination::Library if self.library_loaded => {
                let (start, end) =
                    page_bounds(self.page, self.comics.len(), LIBRARY_ITEMS_PER_PAGE);
                for comic in &self.comics[start..end] {
                    push(comic.cover_url.as_ref());
                }
            }
            MainDestination::Featured
            | MainDestination::Recent
            | MainDestination::Library => {}
        }
        visible
    }

    fn visible_cover_source(&self) -> Option<CoverSource> {
        if self.view != View::Main
            || self.pending.is_some()
            || self.problem.is_some()
            || self.foreground_reader_task.is_some()
        {
            return None;
        }
        Some(match self.destination {
            MainDestination::Featured => CoverSource::Public,
            MainDestination::Recent | MainDestination::Library => CoverSource::Protected,
        })
    }

    fn sync_visible_covers(&mut self, context: &mut Context) {
        if self.pending == Some(Pending::Logout) {
            return;
        }
        let visible = self.visible_cover_urls();
        let visible_source = self.visible_cover_source();
        if visible != self.covers.visible_urls || visible_source != self.covers.visible_source {
            self.covers.generation = self.covers.generation.wrapping_add(1);
            let generation = self.covers.generation;
            let visible_set = visible.iter().cloned().collect::<BTreeSet<_>>();
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
        let Some(source) = self.covers.visible_source else {
            return;
        };
        for url in self.covers.visible_urls.clone() {
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

    fn screen(&self) -> Screen {
        if let Some(pending) = self.pending {
            let (title, message) = match pending {
                Pending::Library(_) => (TITLE, "Loading your library"),
                Pending::Recent(_) => (TITLE, "Loading recent reading"),
                Pending::Content(_) => (TITLE, "Loading episode purchase status"),
                Pending::Logout => (TITLE, "Signing out"),
            };
            return ScreenBuilder::new("bomtoon-loading")
                .top_bar(title)
                .activity(message, None)
                .build();
        }
        if let Some(entry) = self
            .foreground_reader_task
            .and_then(|task| self.reader_tasks.get(&task))
        {
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
        if let Some(problem) = &self.problem {
            let title = if self.view == View::Reader {
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
        match self.view {
            View::Status if self.account != AccountState::Active => self.signed_out_screen(),
            View::Status => ScreenBuilder::new("bomtoon-status")
                .top_bar(TITLE)
                .text("No request has started.")
                .primary_button(RETRY, "Connect")
                .build(),
            View::Main => self.main_screen(),
            View::Account => self.account_screen(),
            View::Episodes => self.episode_screen(),
            View::Reader => self.reader_screen(),
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
            AccountState::SignedOut | AccountState::Active => screen,
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
                screen = add_featured_content(
                    screen,
                    &self.featured,
                    &self.covers,
                    featured_page_zero_rows(&self.featured),
                );
            }
            MainDestination::Recent => {
                let (start, end) =
                    page_bounds(self.page, self.recent.len(), LIBRARY_ITEMS_PER_PAGE);
                screen = screen.rows((start..end).map(|index| {
                    let recent = &self.recent[index];
                    let title_fallback = format!("Title {}", recent.content_alias);
                    let episode_fallback = format!("Episode {}", recent.episode_alias);
                    (
                        format!("comic-{index}"),
                        display_text(&recent.content_title, &title_fallback),
                        display_text(&recent.episode_title, &episode_fallback),
                        cover_lead(&self.covers, recent.cover_url.as_deref()),
                    )
                }));
                let pages = self
                    .destination_total()
                    .max(self.recent.len())
                    .div_ceil(LIBRARY_ITEMS_PER_PAGE)
                    .max(1);
                screen = screen
                    .page_turns(PREVIOUS_PAGE, NEXT_PAGE)
                    .page_position(
                        u16::try_from(self.page.saturating_add(1)).unwrap_or(u16::MAX),
                        u16::try_from(pages).unwrap_or(u16::MAX),
                    );
            }
            MainDestination::Library => {
                let (start, end) =
                    page_bounds(self.page, self.comics.len(), LIBRARY_ITEMS_PER_PAGE);
                screen = screen.rows((start..end).map(|index| {
                    let comic = &self.comics[index];
                    let fallback = format!("Title {}", comic.alias);
                    (
                        format!("comic-{index}"),
                        display_text(&comic.title, &fallback),
                        format!("{} / {}", comic.owned_episodes, comic.total_episodes),
                        cover_lead(&self.covers, comic.cover_url.as_deref()),
                    )
                }));
                let pages = self
                    .destination_total()
                    .max(self.comics.len())
                    .div_ceil(LIBRARY_ITEMS_PER_PAGE)
                    .max(1);
                screen = screen
                    .page_turns(PREVIOUS_PAGE, NEXT_PAGE)
                    .page_position(
                        u16::try_from(self.page.saturating_add(1)).unwrap_or(u16::MAX),
                        u16::try_from(pages).unwrap_or(u16::MAX),
                    );
            }
        }

        screen
            .nav_bar(self.destination.index(), MAIN_DESTINATIONS)
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
        screen = screen
            .page_turns(PREVIOUS_PAGE, NEXT_PAGE)
            .page_position(
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
            MainDestination::Featured => self
                .featured
                .featured
                .len()
                .saturating_add(self.featured.recommended.len()),
            MainDestination::Recent => self.recent.len(),
            MainDestination::Library => self.comics.len(),
        }
    }

    fn destination_total(&self) -> usize {
        match self.destination {
            MainDestination::Featured => self.destination_len(),
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

    fn destination_is_loaded(&self, target: MainDestination) -> bool {
        match target {
            MainDestination::Featured => self.featured.status != FeaturedStatus::Unloaded,
            MainDestination::Recent => self.recent_loaded,
            MainDestination::Library => self.library_loaded,
        }
    }

    fn select_destination(&mut self, context: &mut Context, target: MainDestination) {
        self.page = 0;
        if target == MainDestination::Featured {
            self.featured.page = 0;
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
        if !self.destination_is_loaded(target) {
            let (pending, work) = match target {
                MainDestination::Recent => (Pending::Recent(0), api::recent(0)),
                MainDestination::Library => (Pending::Library(0), api::library(0)),
                MainDestination::Featured => return,
            };
            self.spawn(context, pending, work);
        }
    }

    fn episode_screen(&self) -> Screen {
        let mut screen = ScreenBuilder::new("bomtoon-episodes")
            .top_bar(self.selected_title.clone())
            .text(format!("{} episodes", self.episodes.len()));
        if self.episodes.iter().any(Episode::uses_ticket) {
            let ticket_text = self
                .wallet
                .summary
                .and_then(|summary| summary.tickets.total())
                .map_or_else(
                    || "Tickets unavailable".to_owned(),
                    |total| format!("Tickets {total}"),
                );
            screen = screen.text(ticket_text);
        }
        let (start, end) = page_bounds(self.page, self.episodes.len(), EPISODE_ITEMS_PER_PAGE);
        for (index, episode) in self.episodes[start..end].iter().enumerate() {
            let index = start + index;
            let title_fallback = format!("Episode {}", episode.alias);
            let mut status = display_text(episode.purchase.label(), "Other status");
            if let Some(quantity) = episode.ticket_quantity {
                write!(status, " · Ticket · {quantity}").expect("writing to a String cannot fail");
            }
            let label = format!(
                "{} [{}] - {}",
                display_text(&episode.title, &title_fallback),
                episode.alias,
                status
            );
            if episode.purchase.is_readable() {
                screen = screen.button(format!("episode-{index}"), label);
            } else {
                screen = screen.text(label);
            }
        }
        let pages = self
            .episodes
            .len()
            .div_ceil(EPISODE_ITEMS_PER_PAGE)
            .max(1);
        screen
            .page_turns(PREVIOUS_PAGE, NEXT_PAGE)
            .page_position(
                u16::try_from(self.page.saturating_add(1)).unwrap_or(u16::MAX),
                u16::try_from(pages).unwrap_or(u16::MAX),
            )
            .build()
    }

    fn reader_screen(&self) -> Screen {
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
            .reading_surface(
                picture,
                if reader.chrome_visible {
                    ReadingChrome::Overlay
                } else {
                    ReadingChrome::Hidden
                },
            )
            .page_turns(READER_PREVIOUS, READER_NEXT)
            .reading_menu(READER_CHROME)
            .page_position(
                u16::try_from(reader.page.saturating_add(1)).unwrap_or(reader.total_pages),
                reader.total_pages,
            )
            .build()
    }

    fn cancel_reader(&mut self, context: &mut Context) {
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
        if self.view == View::Reader {
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
        if self.view != View::Reader && self.wallet.summary_refresh_queued {
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

    fn public_cover_urls(&self) -> BTreeSet<String> {
        self.featured
            .featured
            .iter()
            .chain(&self.featured.recommended)
            .chain(
                self.featured
                    .staged
                    .iter()
                    .flat_map(|staged| staged.featured.iter().chain(&staged.recommended)),
            )
            .filter_map(|comic| comic.cover_url.clone())
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
        self.covers.visible_source = (!self.covers.visible_urls.is_empty())
            .then_some(CoverSource::Public);
    }

    fn clear_protected_state(&mut self, context: &mut Context) {
        if let Some(task) = self.task.take() {
            context.cancel(task);
        }
        self.pending = None;
        self.cancel_reader(context);
        self.cancel_wallet(context);
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
        self.episodes.clear();
        self.selected_content_alias.clear();
        self.selected_title.clear();
        self.reader_selection = None;
        self.problem = None;
        self.retry = Retry::Restart;
        self.page = 0;
        self.next_library_page = None;
        self.next_recent_page = None;
        self.total_library_titles = 0;
        self.total_recent_titles = 0;
        self.library_loaded = false;
        self.recent_loaded = false;
    }

    fn transition_after_credential_loss(
        &mut self,
        context: &mut Context,
        account: AccountState,
    ) {
        self.clear_protected_state(context);
        self.account = account;
        self.destination = MainDestination::Featured;
        self.featured.page = 0;
        self.view = if account == AccountState::SignedOut {
            View::Main
        } else {
            View::Status
        };
        self.problem = None;
    }

    fn clear_all_state(&mut self, context: &mut Context) {
        if self.view == View::Reader {
            self.view = View::Episodes;
        }
        self.clear_protected_state(context);
        self.featured.generation = self.featured.generation.wrapping_add(1);
        for task in std::mem::take(&mut self.shelf_tasks).into_keys() {
            context.cancel(task);
        }
        self.superseded_shelf_tasks.clear();
        self.queued_homepage = None;
        self.featured.refresh.local_day_pending = false;
        self.featured.refresh.active_day = None;
        self.featured.refresh.desired_day = None;
        self.featured.staged = None;
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
        if self.featured.refresh.local_day_pending {
            return;
        }
        self.featured.refresh.local_day_pending = true;
        context.device().read_local_day();
    }

    fn observe_local_day(&mut self, context: &mut Context, observed: Option<LocalDay>) {
        let Some(day) = observed else {
            return;
        };
        let refresh = &mut self.featured.refresh;
        let Some(loaded_day) = refresh.loaded_day else {
            refresh.loaded_day = Some(day);
            return;
        };
        if day == loaded_day || refresh.active_day == Some(day) || refresh.desired_day == Some(day) {
            return;
        }
        refresh.desired_day = Some(day);
        self.start_desired_refresh(context);
    }

    fn start_desired_refresh(&mut self, context: &mut Context) {
        if self.featured.refresh.active_day.is_some() {
            return;
        }
        let Some(day) = self.featured.refresh.desired_day.take() else {
            return;
        };
        if self.featured.refresh.loaded_day == Some(day) {
            return;
        }
        self.featured.refresh.active_day = Some(day);
        self.start_homepage(context, Some(day));
    }

    fn spawn_homepage_task(
        &mut self,
        context: &mut Context,
        generation: u64,
        refresh_day: Option<LocalDay>,
    ) {
        if let Some(task) = context.spawn(api::homepage()) {
            self.shelf_tasks.insert(
                task,
                ShelfTaskPurpose::Homepage {
                    generation,
                    refresh_day,
                },
            );
        } else if refresh_day.is_some() {
            self.fail_featured_refresh(context);
        } else {
            self.featured.status = FeaturedStatus::Failed;
        }
    }

    fn resume_queued_homepage(&mut self, context: &mut Context) -> bool {
        if !self.superseded_shelf_tasks.is_empty() {
            return false;
        }
        let Some((generation, refresh_day)) = self.queued_homepage.take() else {
            return false;
        };
        self.spawn_homepage_task(context, generation, refresh_day);
        true
    }

    fn start_homepage(&mut self, context: &mut Context, refresh_day: Option<LocalDay>) {
        if refresh_day.is_some() && self.featured.status != FeaturedStatus::Ready {
            self.featured.status = FeaturedStatus::Loading;
            self.featured.stale_warning = None;
        }
        if let Some((_, queued_day)) = self.queued_homepage.as_mut() {
            *queued_day = refresh_day;
            if refresh_day.is_none() {
                self.featured.status = FeaturedStatus::Loading;
                self.featured.refresh.active_day = None;
                self.featured.refresh.desired_day = None;
                self.featured.staged = None;
            }
            return;
        }
        for &task in self.shelf_tasks.keys() {
            if self.superseded_shelf_tasks.insert(task) {
                context.cancel(task);
            }
        }
        self.featured.generation = self.featured.generation.wrapping_add(1);
        self.featured.staged = None;
        if refresh_day.is_none() {
            self.featured.status = FeaturedStatus::Loading;
            self.featured.featured.clear();
            self.featured.recommended.clear();
            self.featured.pending_details = 0;
            self.featured.page = 0;
            self.featured.stale_warning = None;
            self.featured.refresh.active_day = None;
            self.featured.refresh.desired_day = None;
        }
        let generation = self.featured.generation;
        if self.superseded_shelf_tasks.is_empty() {
            self.spawn_homepage_task(context, generation, refresh_day);
        } else {
            self.queued_homepage = Some((generation, refresh_day));
        }
    }

    fn complete_featured_refresh(&mut self, context: &mut Context) -> bool {
        let Some(staged) = self.featured.staged.take() else {
            return false;
        };
        let Some(day) = self.featured.refresh.active_day.take() else {
            return false;
        };
        self.featured.featured = staged.featured;
        self.featured.recommended = staged.recommended;
        self.featured.pending_details = 0;
        self.featured.page = 0;
        self.featured.status = FeaturedStatus::Ready;
        self.featured.stale_warning = None;
        self.featured.refresh.loaded_day = Some(day);
        if self.featured.refresh.desired_day == Some(day) {
            self.featured.refresh.desired_day = None;
        }
        self.start_desired_refresh(context);
        true
    }

    fn fail_featured_refresh(&mut self, context: &mut Context) -> bool {
        self.featured.staged = None;
        let Some(failed_day) = self.featured.refresh.active_day.take() else {
            return false;
        };
        let preserves_ready_feed = self.featured.status == FeaturedStatus::Ready;
        if self
            .featured
            .refresh
            .desired_day
            .is_some_and(|desired| desired != failed_day)
        {
            if !preserves_ready_feed {
                self.featured.status = FeaturedStatus::Loading;
                self.featured.stale_warning = None;
            }
            self.start_desired_refresh(context);
            return true;
        }
        self.featured.refresh.desired_day = Some(failed_day);
        if preserves_ready_feed {
            self.featured.stale_warning =
                Some("Featured could not be refreshed. Showing the previous feed.".to_owned());
        } else {
            self.featured.status = FeaturedStatus::Failed;
            self.featured.pending_details = 0;
            self.featured.stale_warning = None;
        }
        true
    }

    fn settle_banner_detail(
        &mut self,
        context: &mut Context,
        generation: u64,
        slot: usize,
        alias: &str,
        bytes: Option<&[u8]>,
    ) -> bool {
        if generation != self.featured.generation {
            return false;
        }
        let comic = bytes.and_then(|bytes| parse::public_detail(bytes, alias).ok());
        if self.featured.staged.is_some() {
            let finished = {
                let staged = self.featured.staged.as_mut().expect("checked staged feed");
                if let Some(comic) = comic {
                    if staged
                        .featured
                        .get(slot)
                        .is_some_and(|selected| selected.alias == alias)
                    {
                        staged.featured[slot] = comic;
                    }
                }
                staged.pending_details = staged.pending_details.saturating_sub(1);
                staged.pending_details == 0
            };
            if finished {
                self.complete_featured_refresh(context);
            }
            return true;
        }
        if let Some(comic) = comic {
            if self
                .featured
                .featured
                .get(slot)
                .is_some_and(|selected| selected.alias == alias)
            {
                self.featured.featured[slot] = comic;
            }
        }
        self.featured.pending_details = self.featured.pending_details.saturating_sub(1);
        if self.featured.pending_details == 0 {
            self.featured.status = FeaturedStatus::Ready;
        }
        true
    }

    fn accept_homepage(
        &mut self,
        context: &mut Context,
        generation: u64,
        bytes: &[u8],
    ) -> bool {
        if generation != self.featured.generation {
            return false;
        }
        let Ok(homepage) = parse::homepage(bytes) else {
            self.featured.status = FeaturedStatus::Failed;
            self.featured.pending_details = 0;
            return true;
        };
        let plan = plan_featured(homepage);
        self.featured.featured = plan.featured;
        self.featured.recommended = plan.recommended;
        self.featured.page = 0;
        self.featured.pending_details = plan.details.len();
        self.featured.status = if plan.details.is_empty() {
            FeaturedStatus::Ready
        } else {
            FeaturedStatus::Loading
        };
        for detail in plan.details {
            if let Some(task) = context.spawn(api::public_detail(&detail.alias)) {
                self.shelf_tasks.insert(
                    task,
                    ShelfTaskPurpose::BannerDetail {
                        generation,
                        slot: detail.slot,
                        alias: detail.alias,
                    },
                );
            } else {
                self.featured.pending_details =
                    self.featured.pending_details.saturating_sub(1);
            }
        }
        if self.featured.pending_details == 0 {
            self.featured.status = FeaturedStatus::Ready;
        }
        true
    }

    fn accept_refresh_homepage(
        &mut self,
        context: &mut Context,
        generation: u64,
        refresh_day: LocalDay,
        bytes: &[u8],
    ) -> bool {
        if generation != self.featured.generation
            || self.featured.refresh.active_day != Some(refresh_day)
        {
            return false;
        }
        let Ok(homepage) = parse::homepage(bytes) else {
            return self.fail_featured_refresh(context);
        };
        let plan = plan_featured(homepage);
        self.featured.staged = Some(StagedFeatured {
            featured: plan.featured,
            recommended: plan.recommended,
            pending_details: plan.details.len(),
        });
        for detail in plan.details {
            if let Some(task) = context.spawn(api::public_detail(&detail.alias)) {
                self.shelf_tasks.insert(
                    task,
                    ShelfTaskPurpose::BannerDetail {
                        generation,
                        slot: detail.slot,
                        alias: detail.alias,
                    },
                );
            } else if let Some(staged) = self.featured.staged.as_mut() {
                staged.pending_details = staged.pending_details.saturating_sub(1);
            }
        }
        if self
            .featured
            .staged
            .as_ref()
            .is_some_and(|staged| staged.pending_details == 0)
        {
            self.complete_featured_refresh(context);
        }
        true
    }

    fn handle_shelf_outcome(
        &mut self,
        context: &mut Context,
        purpose: ShelfTaskPurpose,
        outcome: TaskOutcome,
    ) -> bool {
        match purpose {
            ShelfTaskPurpose::Homepage {
                generation,
                refresh_day,
            } => {
                if generation != self.featured.generation {
                    return false;
                }
                match (refresh_day, outcome) {
                    (Some(day), TaskOutcome::Completed(bytes)) => {
                        self.accept_refresh_homepage(context, generation, day, &bytes)
                    }
                    (Some(day), TaskOutcome::Failed(_) | TaskOutcome::Cancelled)
                        if self.featured.refresh.active_day == Some(day) =>
                    {
                        self.fail_featured_refresh(context)
                    }
                    (Some(_), TaskOutcome::Failed(_) | TaskOutcome::Cancelled) => false,
                    (None, TaskOutcome::Completed(bytes)) => {
                        self.accept_homepage(context, generation, &bytes)
                    }
                    (None, TaskOutcome::Failed(_) | TaskOutcome::Cancelled) => {
                        self.featured.status = FeaturedStatus::Failed;
                        self.featured.pending_details = 0;
                        true
                    }
                }
            }
            ShelfTaskPurpose::BannerDetail {
                generation,
                slot,
                alias,
            } => {
                if generation != self.featured.generation {
                    return false;
                }
                let bytes = match &outcome {
                    TaskOutcome::Completed(bytes) => Some(bytes.as_slice()),
                    TaskOutcome::Failed(_) | TaskOutcome::Cancelled => None,
                };
                self.settle_banner_detail(context, generation, slot, &alias, bytes)
            }
        }
    }

    fn open_public_main(&mut self, context: &mut Context) {
        self.destination = MainDestination::Featured;
        self.view = View::Main;
        self.page = 0;
        self.request_local_day(context);
        self.start_homepage(context, None);
        self.refresh_asset_summary(context);
    }

    fn restart(&mut self, context: &mut Context) {
        self.problem = None;
        self.account = AccountState::Active;
        self.clear_protected_state(context);
        self.open_public_main(context);
    }

    fn spawn(&mut self, context: &mut Context, pending: Pending, work: kobo_sdk::Task) {
        match context.spawn(work) {
            Some(task) => {
                self.task = Some(task);
                self.pending = Some(pending);
            }
            None => self.problem = Some("Another network request is still active.".to_owned()),
        }
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
            self.fail_reader(Retry::Manifest, "Another reader request is still active.");
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
            FetchIntent::Foreground { page } => {
                self.retry == Retry::Page(page)
                    && reader.plans.get(page).is_some_and(|plan| {
                        plan.segments.iter().any(|segment| segment.source == source)
                    })
            }
            FetchIntent::Prefetch => {
                source_relevant_to_window(source, &reader.plans, &reader.window)
                    || match self.retry {
                        Retry::Page(page) => source_relevant_to_page_window(
                            source,
                            &reader.plans,
                            page,
                            reader.limits.pages,
                        ),
                        Retry::Restart | Retry::Manifest => false,
                    }
            }
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
            FetchIntent::Foreground { page } => self.fail_reader(Retry::Page(page), advice),
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
                FetchIntent::Foreground { page } if foreground.is_none() => {
                    foreground = Some(page);
                }
                FetchIntent::Foreground { .. } | FetchIntent::Prefetch => {
                    self.record_source_failure(source, FetchIntent::Prefetch, advice.clone());
                }
            }
        }
        if let Some(page) = foreground {
            self.fail_reader(Retry::Page(page), advice);
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
                FetchIntent::Foreground { page } if self.retry == Retry::Page(page) => original,
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
        let desired_page = match self.retry {
            Retry::Page(page) => Some(page),
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
            refreshed_source_candidates(reader, desired_page, foreground_available, available_tasks)
        };
        for (source, intent) in candidates {
            let purpose = match intent {
                FetchIntent::Foreground { page } => {
                    ReaderTaskPurpose::ForegroundSource { source, page }
                }
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

    fn install_page(
        &mut self,
        context: &mut Context,
        page: usize,
        picture: Picture,
    ) -> Result<(), String> {
        let handle = self.next_handle()?;
        let width = picture.width();
        let height = picture.height();
        let uploaded = context
            .put_picture(handle, width, height, picture.into_pixels())
            .ok_or_else(|| "The comic page could not be uploaded.".to_owned())?;
        let old = {
            let reader = self
                .reader
                .as_mut()
                .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
            reader.page = page;
            reader.chrome_visible = false;
            reader.picture.replace(uploaded)
        };
        self.problem = None;
        self.retry = Retry::Page(page);
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
                    page_plan(&images, width, height)
                        .map(|(plans, total_pages)| (images, plans, total_pages, width, height))
                }),
            _ => Err("The panel dimensions are not supported.".to_owned()),
        };
        match planned {
            Ok((images, plans, total_pages, panel_width, panel_height)) => {
                let first = plans
                    .first()
                    .and_then(|plan| plan.segments.first())
                    .map(|segment| segment.source);
                let Some(first_source) = first else {
                    self.fail_reader(Retry::Manifest, "The comic has no readable pages.");
                    return;
                };
                let format = metrics.picture_format;
                let first_build = match PageBuild::new(0, format, panel_width, panel_height) {
                    Ok(build) => build,
                    Err(error) => {
                        self.fail_reader(Retry::Manifest, error);
                        return;
                    }
                };
                let mut window = VecDeque::new();
                window.push_back(PageEntry::Building(first_build));
                self.reader = Some(ReaderState {
                    generation: self.reader_generation,
                    format,
                    limits: reader_limits(format),
                    panel_width,
                    panel_height,
                    images,
                    plans,
                    page: 0,
                    total_pages,
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
                self.retry = Retry::Page(0);
                if self
                    .start_reader_source(
                        context,
                        first_source,
                        ReaderTaskPurpose::ForegroundSource {
                            source: first_source,
                            page: 0,
                        },
                        true,
                    )
                    .is_none()
                {
                    self.fail_reader(
                        Retry::Page(0),
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
        install_target: Option<usize>,
    ) -> Result<bool, String> {
        self.drain_refresh_waiters(context)?;
        let available_tasks = self
            .reader
            .as_ref()
            .map_or(0, |reader| reader.limits.tasks)
            .saturating_sub(self.reader_tasks.len());
        let desired_page = match self.retry {
            Retry::Page(page) => Some(page),
            Retry::Restart | Retry::Manifest => None,
        };
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
            plan_reader_maintenance(
                reader,
                available_tasks,
                extend_window,
                install_target,
                desired_page,
            )?
        };
        if let Some((task, source, page)) = promotion {
            let entry = self.reader_tasks.get_mut(&task).ok_or_else(|| {
                "The comic image request registry changed unexpectedly.".to_owned()
            })?;
            if entry.generation != self.reader_generation {
                return Err("The comic image request generation changed unexpectedly.".to_owned());
            }
            entry.purpose = ReaderTaskPurpose::ForegroundSource { source, page };
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
        if let Some((page, picture)) = ready {
            self.install_page(context, page, picture)?;
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
        let desired_page = match self.retry {
            Retry::Page(page) => Some(page),
            Retry::Restart | Retry::Manifest => None,
        };
        let install_target = match intent {
            FetchIntent::Foreground { page } => Some(page),
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
            let relevant_to_window =
                source_relevant_to_window(source, &reader.plans, &reader.window);
            let relevant_to_desired = desired_page.is_some_and(|target| {
                source_relevant_to_page_window(source, &reader.plans, target, reader.limits.pages)
            });
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
        match pending {
            Pending::Logout => {
                if bytes.is_empty() {
                    self.clear_protected_state(context);
                    self.account = AccountState::SignedOut;
                    self.destination = MainDestination::Featured;
                    self.featured.page = 0;
                    self.view = View::Main;
                } else {
                    self.problem = Some("BOMTOON returned unexpected sign-out data.".to_owned());
                }
            }
            Pending::Library(expected) => match parse::library(bytes) {
                Ok(page) if page.number == expected => {
                    let first_page = !self.library_loaded;
                    self.library_loaded = true;
                    self.total_library_titles = page.total_items;
                    self.comics.extend(page.comics);
                    self.next_library_page =
                        (page.number + 1 < page.total_pages).then_some(page.number + 1);
                    if !first_page {
                        self.page = self.page.saturating_add(1);
                    }
                    self.destination = MainDestination::Library;
                    self.view = View::Main;
                }
                Ok(_) => {
                    self.problem = Some("BOMTOON returned a different library page.".to_owned());
                }
                Err(error) => self.problem = Some(error.to_string()),
            },
            Pending::Recent(expected) => match parse::recent(bytes) {
                Ok(page) if page.number == expected => {
                    let first_page = !self.recent_loaded;
                    self.recent_loaded = true;
                    self.total_recent_titles = page.total_items;
                    self.recent.extend(page.entries);
                    self.next_recent_page =
                        (page.number + 1 < page.total_pages).then_some(page.number + 1);
                    if !first_page {
                        self.page = self.page.saturating_add(1);
                    }
                    self.destination = MainDestination::Recent;
                    self.view = View::Main;
                }
                Ok(_) => {
                    self.problem = Some("BOMTOON returned a different recent page.".to_owned());
                }
                Err(error) => self.problem = Some(error.to_string()),
            },
            Pending::Content(_index) => match parse::episodes(bytes) {
                Ok(episodes) => {
                    self.episodes = episodes;
                    self.page = 0;
                    self.view = View::Episodes;
                }
                Err(error) => self.problem = Some(error.to_string()),
            },
        }
        false
    }

    fn open_comic(&mut self, context: &mut Context, index: usize) {
        if self.account != AccountState::Active {
            self.destination = MainDestination::Featured;
            self.view = View::Status;
            self.page = 0;
            self.problem = None;
            self.show(context);
            return;
        }
        let selected = match self.destination {
            MainDestination::Featured => self
                .featured
                .featured
                .get(index)
                .or_else(|| {
                    index
                        .checked_sub(self.featured.featured.len())
                        .and_then(|index| self.featured.recommended.get(index))
                })
                .map(|comic| (comic.alias.clone(), comic.title.clone())),
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
        self.page = 0;
        self.selected_content_alias.clone_from(&alias);
        self.selected_title = display_text(&title, &format!("BOMTOON {alias}"));
        self.problem = None;
        self.retry = Retry::Restart;
        self.spawn(context, Pending::Content(index), api::content(&alias));
        self.show(context);
    }

    fn open_episode(&mut self, context: &mut Context, index: usize) {
        let Some((episode_alias, title)) = self.episodes.get(index).and_then(|episode| {
            episode.purchase.is_readable().then(|| {
                (
                    episode.alias.clone(),
                    display_text(&episode.title, &format!("Episode {}", episode.alias)),
                )
            })
        }) else {
            return;
        };
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

    fn leave_reader(&mut self, context: &mut Context) {
        self.reader_selection = None;
        self.problem = None;
        self.retry = Retry::Restart;
        self.view = View::Episodes;
        self.show(context);
        self.cancel_reader(context);
        self.resume_deferred_summary(context);
    }

    fn take_ready_page(&mut self, page: usize) -> Option<Picture> {
        let reader = self.reader.as_mut()?;
        if !matches!(
            reader.window.front(),
            Some(PageEntry::Ready {
                page: ready_page,
                ..
            }) if *ready_page == page
        ) {
            return None;
        }
        match reader.window.pop_front() {
            Some(PageEntry::Ready { picture, .. }) => Some(picture),
            Some(PageEntry::Building(_)) | None => None,
        }
    }

    fn prepare_rebased_window(&self, page: usize) -> Result<RebasedReaderWindow, String> {
        let reader = self
            .reader
            .as_ref()
            .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
        let plan = reader
            .plans
            .get(page)
            .ok_or_else(|| "The selected comic page is no longer available.".to_owned())?;
        let required_source = plan
            .segments
            .first()
            .ok_or_else(|| "The selected comic page is empty.".to_owned())?
            .source;
        let build = PageBuild::new(page, reader.format, reader.panel_width, reader.panel_height)?;
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
            if source_relevant_to_page_window(source, &reader.plans, page, reader.limits.pages)
                && kept_sources.insert(source)
            {
                kept_cache_sources.insert(source);
            }
        }
        for (&source, &task) in &reader.source_fetches {
            if kept_sources.len() == retained_capacity
                || kept_fetches.len() == reader.limits.fetches
            {
                break;
            }
            if source_relevant_to_page_window(source, &reader.plans, page, reader.limits.pages)
                && is_active_source(source, task)
                && kept_sources.insert(source)
            {
                kept_fetches.insert(source, task);
            }
        }
        Ok(RebasedReaderWindow {
            build,
            cached_sources: kept_cache_sources,
            fetches: kept_fetches,
        })
    }

    fn rebase_window(&mut self, context: &mut Context, page: usize) {
        let RebasedReaderWindow {
            build,
            cached_sources: kept_cache_sources,
            fetches: kept_fetches,
        } = match self.prepare_rebased_window(page) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_reader(Retry::Page(page), error);
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
                    Retry::Page(page),
                    "The comic image request registry changed unexpectedly.",
                );
                return;
            };
            entry.purpose = ReaderTaskPurpose::PrefetchSource { source };
        }
        self.foreground_reader_task = None;
        let Some(reader) = self.reader.as_mut() else {
            self.fail_reader(
                Retry::Page(page),
                "The selected episode is no longer available.",
            );
            return;
        };
        reader.window.clear();
        reader.window.push_back(PageEntry::Building(build));
        reader
            .source_cache
            .retain(|source, _| kept_cache_sources.contains(source));
        reader.source_fetches = kept_fetches;
        reader.maintenance_task = None;
        self.problem = None;
        self.retry = Retry::Page(page);

        match self.maintain_reader(context, false, Some(page)) {
            Ok(_) => {}
            Err(error) => self.fail_reader(Retry::Page(page), error),
        }
    }

    fn request_reader_page(&mut self, context: &mut Context, page: usize) {
        if let Some(picture) = self.take_ready_page(page) {
            if let Err(error) = self.install_page(context, page, picture) {
                self.fail_reader(Retry::Page(page), error);
                self.show(context);
            }
            return;
        }
        self.rebase_window(context, page);
        self.show(context);
    }

    fn retry(&mut self, context: &mut Context) -> bool {
        let retry = self.retry;
        self.problem = None;
        match retry {
            Retry::Restart => self.restart(context),
            Retry::Manifest => self.start_manifest(context),
            Retry::Page(page) => {
                if let Some(reader) = self.reader.as_mut() {
                    let failed = reader
                        .plans
                        .get(page)
                        .into_iter()
                        .flat_map(|plan| &plan.segments)
                        .map(|segment| segment.source)
                        .collect::<BTreeSet<_>>();
                    reader
                        .source_failures
                        .retain(|source, _| !failed.contains(source));
                }
                self.rebase_window(context, page);
            }
        }
        false
    }

    fn fail_task(&mut self, context: &mut Context, pending: Pending, error: TaskError) {
        match (pending, error) {
            (Pending::Logout, TaskError::RevocationUnconfirmed) => {
                self.clear_protected_state(context);
                self.account = AccountState::RevocationUnconfirmed;
                self.destination = MainDestination::Featured;
                self.featured.page = 0;
                self.view = View::Status;
                self.problem = None;
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
            (_, error) => {
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

    fn handle_reader_action(&mut self, context: &mut Context, action: ActionId) {
        if action == action_id(READER_CHROME) {
            if let Some(reader) = self.reader.as_mut() {
                reader.chrome_visible = !reader.chrome_visible;
            }
            self.show(context);
            return;
        }
        let target = self.reader.as_ref().and_then(|reader| {
            if action == action_id(READER_PREVIOUS) {
                reader.page.checked_sub(1)
            } else if action == action_id(READER_NEXT) {
                reader
                    .page
                    .checked_add(1)
                    .filter(|page| *page < reader.plans.len())
            } else {
                None
            }
        });
        if let Some(target) = target {
            self.request_reader_page(context, target);
        } else if action != action_id(READER_PREVIOUS) && action != action_id(READER_NEXT) {
            self.show(context);
        }
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
                        self.transition_after_credential_loss(
                            context,
                            AccountState::SignedOut,
                        );
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
        let page = self.reader.as_ref().map_or(0, |reader| reader.page);
        if let Some(reader) = self.reader.as_mut() {
            if reader.maintenance_task == Some(task) {
                reader.maintenance_task = None;
            }
        }
        match outcome {
            TaskOutcome::Completed(_) => {
                if let Err(error) = self.maintain_reader(context, true, None) {
                    self.fail_reader(Retry::Page(page), error);
                }
            }
            TaskOutcome::Failed(error) => {
                self.fail_reader(Retry::Page(page), Failure::of(*error).advice);
            }
            TaskOutcome::Cancelled => {
                self.fail_reader(Retry::Page(page), "The request was cancelled.");
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
            ReaderTaskPurpose::ForegroundSource { source, page } => self
                .handle_reader_source_outcome(
                    context,
                    task,
                    source,
                    FetchIntent::Foreground { page },
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
        if cover.generation != self.covers.generation
            || self.covers.entries.get(&cover.url) != Some(&CoverState::Loading(task))
        {
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

    fn cancel_task(&mut self, _pending: Pending) {
        self.problem = Some("The request was cancelled.".to_owned());
        self.retry = Retry::Restart;
    }
}

impl KoboApp for Bomtoon {
    fn on_start(&mut self, context: &mut Context) {
        self.problem = None;
        self.open_public_main(context);
        self.show(context);
    }

    fn on_resume(&mut self, context: &mut Context) {
        self.request_local_day(context);
    }

    fn on_suspend(&mut self, context: &mut Context) {
        if self.view == View::Reader
            || self.reader_selection.is_some()
            || self.reader.is_some()
            || !self.reader_tasks.is_empty()
        {
            self.reader_selection = None;
            self.problem = None;
            self.retry = Retry::Restart;
            if self.view == View::Reader {
                self.view = View::Episodes;
            }
            self.cancel_reader(context);
            self.resume_deferred_summary(context);
        }
    }

    fn on_foreground(&mut self, context: &mut Context) {
        self.request_local_day(context);
    }

    fn on_device_result(
        &mut self,
        context: &mut Context,
        request: DeviceRequest,
        result: DeviceResult,
    ) {
        if !self.featured.refresh.local_day_pending {
            return;
        }
        if let (DeviceRequest::ReadLocalDay, DeviceResult::LocalDay(observed)) = (request, result) {
            self.featured.refresh.local_day_pending = false;
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
        let can_leave_reader = self.account == AccountState::Active && self.view == View::Reader;
        if action == ActionId::BACK && can_leave_reader {
            self.leave_reader(context);
            return;
        }
        let ready = self.account == AccountState::Active
            && self.problem.is_none()
            && self.pending.is_none()
            && self.foreground_reader_task.is_none();
        if action == ActionId::BACK && ready && matches!(self.view, View::Account | View::Episodes)
        {
            self.view = View::Main;
            self.page = 0;
            self.show(context);
            return;
        }
        if action == action_id(RETRY)
            && self.problem.is_none()
            && self.view == View::Main
            && self.destination == MainDestination::Featured
            && self.featured.stale_warning.is_some()
            && self.featured.refresh.active_day.is_none()
        {
            self.start_desired_refresh(context);
            self.show(context);
            return;
        }
        if action == action_id(RETRY)
            && self.problem.is_none()
            && self.view == View::Main
            && self.destination == MainDestination::Featured
            && self.featured.status == FeaturedStatus::Failed
            && self.featured.refresh.active_day.is_none()
        {
            if self.featured.refresh.desired_day.is_some() {
                self.start_desired_refresh(context);
            } else {
                self.start_homepage(context, None);
            }
            self.show(context);
            return;
        }
        let retry_visible = self.problem.is_some() || self.view == View::Status;
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
        if self.view == View::Main && self.pending.is_none() {
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
        if self.view == View::Main && self.destination == MainDestination::Featured {
            if action == action_id(PREVIOUS_PAGE) {
                self.featured.page = self.featured.page.saturating_sub(1);
                self.show(context);
                return;
            }
            if action == action_id(NEXT_PAGE) {
                let first_page_rows = featured_page_zero_rows(&self.featured);
                let pages = featured_page_count(&self.featured, first_page_rows);
                if self.featured.page.saturating_add(1) < pages {
                    self.featured.page = self.featured.page.saturating_add(1);
                }
                self.show(context);
                return;
            }
            for index in 0..self.destination_len() {
                if action == action_id(&format!("comic-{index}")) {
                    self.open_comic(context, index);
                    return;
                }
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
                self.spawn(context, Pending::Logout, api::logout());
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
            let items_per_page = if self.view == View::Main {
                LIBRARY_ITEMS_PER_PAGE
            } else {
                EPISODE_ITEMS_PER_PAGE
            };
            let next_start = self.page.saturating_add(1).saturating_mul(items_per_page);
            if self.view != View::Main || next_start < self.destination_len() {
                self.page = self.page.saturating_add(1);
            } else if let Some(next) = self.destination_next_page() {
                let (pending, work) = match self.destination {
                    MainDestination::Recent => (Pending::Recent(next), api::recent(next)),
                    MainDestination::Library => (Pending::Library(next), api::library(next)),
                    MainDestination::Featured => {
                        self.show(context);
                        return;
                    }
                };
                self.spawn(context, pending, work);
            }
        } else if self.view == View::Main {
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
        if let Some(cover) = self.covers.tasks.remove(&task) {
            let changed = self.handle_cover_outcome(context, task, cover, outcome);
            self.resume_deferred_wallet(context);
            if changed {
                self.show(context);
            } else {
                self.spawn_visible_covers(context);
            }
            return;
        }
        if let Some(purpose) = self.shelf_tasks.remove(&task) {
            if self.superseded_shelf_tasks.remove(&task) {
                let changed = self.resume_queued_homepage(context);
                if changed
                    && self.view == View::Main
                    && self.destination == MainDestination::Featured
                {
                    self.show(context);
                }
                self.resume_deferred_wallet(context);
                self.spawn_visible_covers(context);
                return;
            }
            let changed = self.handle_shelf_outcome(context, purpose, outcome);
            if changed
                && self.view == View::Main
                && self.destination == MainDestination::Featured
            {
                self.show(context);
            }
            self.resume_deferred_wallet(context);
            self.spawn_visible_covers(context);
            return;
        }
        if let Some(purpose) = self.wallet.tasks.remove(&task) {
            self.handle_wallet_outcome(context, task, purpose, outcome);
            self.resume_deferred_wallet(context);
            self.spawn_visible_covers(context);
            return;
        }
        if let Some(entry) = self.reader_tasks.remove(&task) {
            if self.foreground_reader_task == Some(task) {
                self.foreground_reader_task = None;
            }
            self.handle_reader_outcome(context, task, entry, outcome);
            self.resume_deferred_wallet(context);
            self.spawn_visible_covers(context);
            return;
        }
        if self.task != Some(task) {
            self.resume_deferred_wallet(context);
            self.spawn_visible_covers(context);
            return;
        }
        self.task = None;
        let Some(pending) = self.pending.take() else {
            self.resume_deferred_wallet(context);
            self.spawn_visible_covers(context);
            return;
        };
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
        self.resume_deferred_wallet(context);
        self.spawn_visible_covers(context);
    }
}

fn extend_reader_window(reader: &mut ReaderState) -> Result<(), String> {
    while reader.window.len() < reader.limits.pages {
        let next_page = reader.window.back().map(entry_page).map_or_else(
            || reader.page.saturating_add(1),
            |page| page.saturating_add(1),
        );
        if next_page >= reader.plans.len() {
            break;
        }
        reader.window.push_back(PageEntry::Building(PageBuild::new(
            next_page,
            reader.format,
            reader.panel_width,
            reader.panel_height,
        )?));
    }
    Ok(())
}

fn discard_stale_reader_failures(reader: &mut ReaderState, desired_page: Option<usize>) {
    let ReaderState {
        source_failures,
        plans,
        window,
        limits,
        ..
    } = reader;
    source_failures.retain(|source, _| {
        source_relevant_to_window(*source, plans, window)
            || desired_page.is_some_and(|page| {
                source_relevant_to_page_window(*source, plans, page, limits.pages)
            })
    });
}

fn update_reader_builds(reader: &mut ReaderState) -> Result<(), String> {
    {
        let ReaderState {
            source_cache,
            plans,
            window,
            panel_width,
            panel_height,
            ..
        } = reader;
        for (&source, picture) in source_cache.iter() {
            for entry in window.iter_mut() {
                if let PageEntry::Building(build) = entry {
                    copy_source_into_builds(
                        source,
                        picture,
                        plans,
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
            Some(PageEntry::Ready { .. }) => {
                index += 1;
                continue;
            }
            Some(PageEntry::Building(build)) => {
                let plan = reader
                    .plans
                    .get(build.page)
                    .ok_or_else(|| "The comic page build has no plan.".to_owned())?;
                build.next_segment == plan.segments.len()
            }
            None => break,
        };
        if !complete {
            break;
        }
        let Some(PageEntry::Building(build)) = reader.window.remove(index) else {
            return Err("The comic page window changed unexpectedly.".to_owned());
        };
        let page = build.page;
        let plan = reader
            .plans
            .get(page)
            .ok_or_else(|| "The comic page build has no plan.".to_owned())?;
        let picture = finish_build(build, plan, reader.panel_width, reader.panel_height)?;
        reader
            .window
            .insert(index, PageEntry::Ready { page, picture });
        index += 1;
    }
    Ok(())
}

fn retain_reader_sources(reader: &mut ReaderState, install_target: Option<usize>) {
    let ReaderState {
        source_cache,
        plans,
        window,
        limits,
        ..
    } = reader;
    source_cache.retain(|source, _| {
        source_relevant_to_window(*source, plans, window)
            || install_target.is_some_and(|target| {
                source_relevant_to_following_page_window(*source, plans, target, limits.pages)
            })
    });
}

fn take_installable_reader_page(
    reader: &mut ReaderState,
    target: usize,
) -> Result<Option<(usize, Picture)>, String> {
    let installable = matches!(
        reader.window.front(),
        Some(PageEntry::Ready { page, .. }) if *page == target
    );
    if !installable {
        return Ok(None);
    }
    let Some(PageEntry::Ready { page, picture }) = reader.window.pop_front() else {
        return Err("The ready comic page changed unexpectedly.".to_owned());
    };
    Ok(Some((page, picture)))
}

fn plan_foreground_reader_source(
    reader: &mut ReaderState,
    page: usize,
    spawn_limit: usize,
    plan: &mut ReaderMaintenancePlan,
) -> Result<(), String> {
    let missing = reader.window.iter().find_map(|entry| match entry {
        PageEntry::Building(build) if build.page == page => reader
            .plans
            .get(build.page)
            .and_then(|page_plan| page_plan.segments.get(build.next_segment))
            .map(|segment| segment.source),
        PageEntry::Building(_) | PageEntry::Ready { .. } => None,
    });
    let Some(source) = missing else {
        return Ok(());
    };
    if let Some(failure) = reader.source_failures.get(&source) {
        return Err(failure.advice.clone());
    }
    if let Some(intent) = reader.refresh_waiters.get_mut(&source) {
        *intent = FetchIntent::Foreground { page };
        plan.refresh_promotion = reader.refresh_task;
    } else if let Some(&task) = reader.source_fetches.get(&source) {
        plan.promotion = Some((task, source, page));
    } else if !reader.source_cache.contains_key(&source) && spawn_limit > 0 {
        let url = reader
            .images
            .get(source)
            .ok_or_else(|| "The selected comic image is no longer available.".to_owned())?
            .url
            .clone();
        plan.spawns.push(PlannedReaderSpawn {
            source,
            purpose: ReaderTaskPurpose::ForegroundSource { source, page },
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
    'pages: for entry in &reader.window {
        let PageEntry::Building(build) = entry else {
            continue;
        };
        let page_plan = reader
            .plans
            .get(build.page)
            .ok_or_else(|| "The comic page build has no plan.".to_owned())?;
        for segment in &page_plan.segments[build.next_segment..] {
            let source = segment.source;
            if reader.source_failures.contains_key(&source) {
                break 'pages;
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
                break 'pages;
            }
        }
    }
    Ok(())
}

fn plan_reader_maintenance(
    reader: &mut ReaderState,
    available_tasks: usize,
    extend_window: bool,
    install_target: Option<usize>,
    desired_page: Option<usize>,
) -> Result<ReaderMaintenancePlan, String> {
    if extend_window {
        extend_reader_window(reader)?;
    }
    discard_stale_reader_failures(reader, desired_page);
    update_reader_builds(reader)?;
    retain_reader_sources(reader, install_target);
    let ready = match install_target {
        Some(target) => take_installable_reader_page(reader, target)?,
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
    if let Some(page) = install_target {
        plan_foreground_reader_source(reader, page, spawn_limit, &mut plan)?;
    } else if spawn_limit > 0
        && (reader.refresh_task.is_some() || reader.refresh_waiters.is_empty())
    {
        plan_prefetch_reader_sources(reader, spawn_limit, &mut plan)?;
    }
    Ok(plan)
}

fn refreshed_source_candidates(
    reader: &mut ReaderState,
    desired_page: Option<usize>,
    foreground_available: bool,
    available_tasks: usize,
) -> Vec<(usize, FetchIntent)> {
    let stale = reader
        .refresh_waiters
        .iter()
        .filter_map(|(&source, &intent)| {
            let relevant = match intent {
                FetchIntent::Foreground { page } => {
                    desired_page == Some(page)
                        && reader.plans.get(page).is_some_and(|plan| {
                            plan.segments.iter().any(|segment| segment.source == source)
                        })
                }
                FetchIntent::Prefetch => {
                    source_relevant_to_window(source, &reader.plans, &reader.window)
                        || desired_page.is_some_and(|page| {
                            source_relevant_to_page_window(
                                source,
                                &reader.plans,
                                page,
                                reader.limits.pages,
                            )
                        })
                }
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

fn entry_page(entry: &PageEntry) -> usize {
    match entry {
        PageEntry::Building(build) => build.page,
        PageEntry::Ready { page, .. } => *page,
    }
}

fn source_relevant_to_window(
    source: usize,
    plans: &[PagePlan],
    window: &VecDeque<PageEntry>,
) -> bool {
    window.iter().any(|entry| {
        let PageEntry::Building(build) = entry else {
            return false;
        };
        plans
            .get(build.page)
            .and_then(|plan| plan.segments.get(build.next_segment..))
            .is_some_and(|segments| segments.iter().any(|segment| segment.source == source))
    })
}

fn source_relevant_to_page_window(
    source: usize,
    plans: &[PagePlan],
    page: usize,
    following_pages: usize,
) -> bool {
    if page >= plans.len() {
        return false;
    }
    let end = page
        .checked_add(following_pages)
        .and_then(|last| last.checked_add(1))
        .unwrap_or(plans.len())
        .min(plans.len());
    plans[page..end]
        .iter()
        .flat_map(|plan| &plan.segments)
        .any(|segment| segment.source == source)
}

fn source_relevant_to_following_page_window(
    source: usize,
    plans: &[PagePlan],
    page: usize,
    following_pages: usize,
) -> bool {
    let Some(first_following) = page.checked_add(1) else {
        return false;
    };
    source_relevant_to_page_window(
        source,
        plans,
        first_following,
        following_pages.saturating_sub(1),
    )
}

fn validate_continuous_page(plan: &PagePlan) -> Result<(), String> {
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

fn page_plan(
    images: &[EpisodeImage],
    panel_width: u32,
    panel_height: u32,
) -> Result<(Vec<PagePlan>, u16), String> {
    if panel_width == 0 || panel_height == 0 {
        return Err("The comic page dimensions are not supported.".to_owned());
    }

    let panel_rows = u64::from(panel_height);
    let mut total_rows = 0_u64;
    for image in images {
        let (_, scaled_height) =
            kobo_image::width_scaled_size((image.width, image.height), panel_width)
                .map_err(|error| error.to_string())?;
        total_rows = total_rows
            .checked_add(u64::from(scaled_height))
            .ok_or_else(|| "The comic height is not supported.".to_owned())?;
    }

    let page_count = total_rows.div_ceil(panel_rows);
    let total_pages =
        u16::try_from(page_count).map_err(|_| "The comic has too many pages.".to_owned())?;
    let mut plans = Vec::with_capacity(usize::from(total_pages));
    for page in 0..page_count {
        let page_start = page
            .checked_mul(panel_rows)
            .ok_or_else(|| "The comic page interval is not supported.".to_owned())?;
        let remaining = total_rows
            .checked_sub(page_start)
            .ok_or_else(|| "The comic page interval is not supported.".to_owned())?;
        let content_rows = u32::try_from(remaining.min(panel_rows))
            .map_err(|_| "The comic page height is not supported.".to_owned())?;
        plans.push(PagePlan {
            segments: Vec::new(),
            content_rows,
        });
    }

    let mut source_start = 0_u64;
    for (source, image) in images.iter().enumerate() {
        let (_, scaled_height) =
            kobo_image::width_scaled_size((image.width, image.height), panel_width)
                .map_err(|error| error.to_string())?;
        let source_end = source_start
            .checked_add(u64::from(scaled_height))
            .ok_or_else(|| "The comic source interval is not supported.".to_owned())?;
        let mut page = source_start / panel_rows;
        loop {
            let page_start = page
                .checked_mul(panel_rows)
                .ok_or_else(|| "The comic page interval is not supported.".to_owned())?;
            if page_start >= source_end {
                break;
            }
            let plan_index = usize::try_from(page)
                .map_err(|_| "The comic page index is not supported.".to_owned())?;
            let plan = plans
                .get_mut(plan_index)
                .ok_or_else(|| "The comic page index is not supported.".to_owned())?;
            let page_end = page_start
                .checked_add(u64::from(plan.content_rows))
                .ok_or_else(|| "The comic page interval is not supported.".to_owned())?;
            let overlap_start = page_start.max(source_start);
            let overlap_end = page_end.min(source_end);
            if overlap_start < overlap_end {
                let source_row = overlap_start
                    .checked_sub(source_start)
                    .ok_or_else(|| "The comic source row is not supported.".to_owned())?;
                let rows = overlap_end
                    .checked_sub(overlap_start)
                    .ok_or_else(|| "The comic segment height is not supported.".to_owned())?;
                let destination_row = overlap_start
                    .checked_sub(page_start)
                    .ok_or_else(|| "The comic destination row is not supported.".to_owned())?;
                plan.segments.push(PageSegment {
                    source,
                    source_row: u32::try_from(source_row)
                        .map_err(|_| "The comic source row is not supported.".to_owned())?,
                    rows: u32::try_from(rows)
                        .map_err(|_| "The comic segment height is not supported.".to_owned())?,
                    destination_row: u32::try_from(destination_row)
                        .map_err(|_| "The comic destination row is not supported.".to_owned())?,
                });
            }
            page = page
                .checked_add(1)
                .ok_or_else(|| "The comic page index is not supported.".to_owned())?;
        }
        source_start = source_end;
    }

    if source_start != total_rows {
        return Err("The comic source intervals are not contiguous.".to_owned());
    }
    for plan in &plans {
        validate_continuous_page(plan)?;
    }
    Ok((plans, total_pages))
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
    plans: &[PagePlan],
    builds: &mut [PageBuild],
    panel_width: u32,
    panel_height: u32,
) -> Result<(), String> {
    for build in builds {
        let plan = plans
            .get(build.page)
            .ok_or_else(|| "The comic page build has no plan.".to_owned())?;
        let Some(segment) = plan.segments.get(build.next_segment) else {
            continue;
        };
        if segment.source != source_index {
            continue;
        }
        if source.format() != build.format {
            return Err("The comic source format does not match the page build.".to_owned());
        }
        if source.width() != panel_width {
            return Err("The scaled comic image width does not match the panel.".to_owned());
        }
        let expected_build_len = build
            .format
            .byte_len(panel_width, panel_height)
            .ok_or_else(|| "The comic page byte length is not supported.".to_owned())?;
        if build.bytes.len() != expected_build_len {
            return Err("The comic page pixels do not match their dimensions.".to_owned());
        }

        let source_bytes = match source.pixels() {
            PicturePixelsRef::Gray8(bytes) if build.format == PictureFormat::Gray8 => bytes,
            PicturePixelsRef::Rgb8(bytes) if build.format == PictureFormat::Rgb8 => bytes,
            _ => {
                return Err("The comic source format does not match the page build.".to_owned());
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
            .ok_or_else(|| "The comic page byte offset is not supported.".to_owned())?;
        let destination_end = destination_start
            .checked_add(copied_len)
            .ok_or_else(|| "The comic page byte interval is not supported.".to_owned())?;
        let source_rows = source_bytes.get(source_start..source_end).ok_or_else(|| {
            "The comic source pixels do not cover the planned segment.".to_owned()
        })?;
        let destination = build
            .bytes
            .get_mut(destination_start..destination_end)
            .ok_or_else(|| "The comic page pixels do not cover the planned segment.".to_owned())?;
        destination.copy_from_slice(source_rows);
        build.next_segment = build
            .next_segment
            .checked_add(1)
            .ok_or_else(|| "The comic page has too many segments.".to_owned())?;
    }
    Ok(())
}

fn finish_build(
    build: PageBuild,
    plan: &PagePlan,
    panel_width: u32,
    panel_height: u32,
) -> Result<Picture, String> {
    if build.next_segment != plan.segments.len() {
        return Err("The comic page build is incomplete.".to_owned());
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
        AppRunner, Chrome, Command, DisplayMetrics, Node, PictureHandle, ReadingChrome,
        SecretHeader, Task, TilePicture, CLARA_BW_METRICS,
    };

    const LIBRARY_RESPONSE: &[u8] = br#"{
        "result":"SUCCESS",
        "data":{
            "content":[{
                "alias":"hunter_q",
                "title":"Hunter Q",
                "collectionCount":1,
                "episodeCount":2
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
                "collectionCount":1,
                "episodeCount":1
            }],
            "number":1,
            "totalPages":2,
            "totalElements":31
        }
    }"#;
    const REMOTE_LIBRARY_PAGE_SIZE: usize = 30;
    const CONTENT_RESPONSE: &[u8] = br#"{
        "result":"SUCCESS",
        "data":{"episodes":[
            {"alias":"ep-1","title":"Episode One","isSample":false,"purchaseStatus":"POSSESSION"},
            {"alias":"ep-2","title":"Episode Two","isSample":false,"purchaseStatus":null,"paid":false},
            {"alias":"sample","title":"Sample","isSample":true,"purchaseStatus":null},
            {"alias":"ticket","title":"Ticket Episode","isSample":false,"purchaseStatus":"NONE","paid":true,"coinKind":"TICKET","rentCoin":1}
        ]}
    }"#;
    const COIN_ONLY_CONTENT_RESPONSE: &[u8] = br#"{
        "result":"SUCCESS",
        "data":{"episodes":[
            {"alias":"owned","title":"Owned Episode","isSample":false,"purchaseStatus":"POSSESSION"},
            {"alias":"coin","title":"Coin Episode","isSample":false,"purchaseStatus":"NONE","paid":true,"coinKind":"COIN","rentCoin":1}
        ]}
    }"#;
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

    fn plan_for_heights(heights: &[u32]) -> (Vec<PagePlan>, u16) {
        let images = heights
            .iter()
            .enumerate()
            .map(|(source, height)| episode_image(source, 2, *height))
            .collect::<Vec<_>>();
        page_plan(&images, 2, 4).expect("continuous page plan")
    }

    #[test]
    fn short_sources_share_a_page_without_seam_padding() {
        let (plans, total_pages) = plan_for_heights(&[2, 2]);
        assert_eq!(total_pages, 1);
        assert_eq!(
            plans,
            vec![PagePlan {
                segments: vec![
                    PageSegment {
                        source: 0,
                        source_row: 0,
                        rows: 2,
                        destination_row: 0,
                    },
                    PageSegment {
                        source: 1,
                        source_row: 0,
                        rows: 2,
                        destination_row: 2,
                    },
                ],
                content_rows: 4,
            }]
        );
    }

    #[test]
    fn page_plan_handles_seams_at_global_rows_3_4_and_5() {
        let (row_3, _) = plan_for_heights(&[3, 2]);
        assert_eq!(
            row_3,
            vec![
                PagePlan {
                    segments: vec![
                        PageSegment {
                            source: 0,
                            source_row: 0,
                            rows: 3,
                            destination_row: 0,
                        },
                        PageSegment {
                            source: 1,
                            source_row: 0,
                            rows: 1,
                            destination_row: 3,
                        },
                    ],
                    content_rows: 4,
                },
                PagePlan {
                    segments: vec![PageSegment {
                        source: 1,
                        source_row: 1,
                        rows: 1,
                        destination_row: 0,
                    }],
                    content_rows: 1,
                },
            ]
        );

        let (row_4, _) = plan_for_heights(&[4, 2]);
        assert_eq!(
            row_4,
            vec![
                PagePlan {
                    segments: vec![PageSegment {
                        source: 0,
                        source_row: 0,
                        rows: 4,
                        destination_row: 0,
                    }],
                    content_rows: 4,
                },
                PagePlan {
                    segments: vec![PageSegment {
                        source: 1,
                        source_row: 0,
                        rows: 2,
                        destination_row: 0,
                    }],
                    content_rows: 2,
                },
            ]
        );

        let (row_5, _) = plan_for_heights(&[5, 2]);
        assert_eq!(
            row_5,
            vec![
                PagePlan {
                    segments: vec![PageSegment {
                        source: 0,
                        source_row: 0,
                        rows: 4,
                        destination_row: 0,
                    }],
                    content_rows: 4,
                },
                PagePlan {
                    segments: vec![
                        PageSegment {
                            source: 0,
                            source_row: 4,
                            rows: 1,
                            destination_row: 0,
                        },
                        PageSegment {
                            source: 1,
                            source_row: 0,
                            rows: 2,
                            destination_row: 1,
                        },
                    ],
                    content_rows: 3,
                },
            ]
        );
    }

    #[test]
    fn page_plan_packs_one_row_sources_in_source_order() {
        let (plans, total_pages) = plan_for_heights(&[1, 1, 1, 1]);
        assert_eq!(total_pages, 1);
        assert_eq!(
            plans,
            vec![PagePlan {
                segments: (0..4)
                    .map(|source| PageSegment {
                        source,
                        source_row: 0,
                        rows: 1,
                        destination_row: u32::try_from(source).expect("destination row"),
                    })
                    .collect(),
                content_rows: 4,
            }]
        );
    }

    #[test]
    fn page_plan_keeps_the_final_partial_page() {
        let (plans, total_pages) = plan_for_heights(&[5]);
        assert_eq!(total_pages, 2);
        assert_eq!(
            plans,
            vec![
                PagePlan {
                    segments: vec![PageSegment {
                        source: 0,
                        source_row: 0,
                        rows: 4,
                        destination_row: 0,
                    }],
                    content_rows: 4,
                },
                PagePlan {
                    segments: vec![PageSegment {
                        source: 0,
                        source_row: 4,
                        rows: 1,
                        destination_row: 0,
                    }],
                    content_rows: 1,
                },
            ]
        );
    }

    #[test]
    fn page_plan_rejects_zero_dimensions() {
        let valid = episode_image(0, 2, 2);
        assert!(page_plan(std::slice::from_ref(&valid), 0, 4).is_err());
        assert!(page_plan(std::slice::from_ref(&valid), 2, 0).is_err());
        assert!(page_plan(&[episode_image(0, 0, 2)], 2, 4).is_err());
        assert!(page_plan(&[episode_image(0, 2, 0)], 2, 4).is_err());
    }

    #[test]
    fn cumulative_height_above_u32_uses_checked_global_intervals() {
        let source_height = 1_u32 << 22;
        let source_count = usize::try_from(u64::from(u32::MAX) / u64::from(source_height) + 1)
            .expect("source count");
        let images = (0..source_count)
            .map(|source| episode_image(source, 1, source_height))
            .collect::<Vec<_>>();

        let (plans, total_pages) = page_plan(&images, 1, u32::MAX).expect("u64 global plan");

        assert_eq!(total_pages, 2);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].content_rows, u32::MAX);
        assert_eq!(
            plans[1],
            PagePlan {
                segments: vec![PageSegment {
                    source: source_count - 1,
                    source_row: source_height - 1,
                    rows: 1,
                    destination_row: 0,
                }],
                content_rows: 1,
            }
        );
        assert_eq!(
            plans
                .iter()
                .map(|plan| u64::from(plan.content_rows))
                .sum::<u64>(),
            u64::from(u32::MAX) + 1
        );
    }

    #[test]
    fn page_plan_rejects_more_than_u16_pages() {
        let height = u32::from(u16::MAX) + 1;
        let error = page_plan(&[episode_image(0, 1, height)], 1, 1)
            .expect_err("page count above u16 must fail");
        assert_eq!(error, "The comic has too many pages.");
    }

    fn seam_plan() -> PagePlan {
        PagePlan {
            segments: vec![
                PageSegment {
                    source: 0,
                    source_row: 0,
                    rows: 1,
                    destination_row: 0,
                },
                PageSegment {
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
    fn typed_page_assembly_gray8_dithers_once_after_the_source_seam() {
        let plans = vec![seam_plan()];
        let mut builds = vec![PageBuild::new(0, PictureFormat::Gray8, 2, 2).expect("Gray8 build")];
        let first = Picture::from_grey(2, 1, vec![10, 10]).expect("first source");
        let second = Picture::from_grey(2, 1, vec![20, 20]).expect("second source");

        copy_source_into_builds(0, &first, &plans, &mut builds, 2, 2).expect("first segment");
        assert_eq!(builds[0].bytes, [10, 10, 255, 255]);
        assert_eq!(builds[0].next_segment, 1);
        copy_source_into_builds(0, &first, &plans, &mut builds, 2, 2)
            .expect("duplicate source is ignored");
        assert_eq!(builds[0].next_segment, 1);
        copy_source_into_builds(1, &second, &plans, &mut builds, 2, 2).expect("second segment");
        assert_eq!(builds[0].bytes, [10, 10, 20, 20]);

        let mut expected = Picture::from_grey(2, 2, vec![10, 10, 20, 20]).expect("undithered page");
        expected.dither(PANEL_GREYS).expect("whole-page dither");
        let picture = finish_build(builds.pop().expect("build"), &plans[0], 2, 2)
            .expect("finished Gray8 page");

        assert_eq!(picture.pixels(), expected.pixels());
    }

    #[test]
    fn typed_page_assembly_rgb8_preserves_exact_colors_across_the_source_seam() {
        let plans = vec![seam_plan()];
        let mut builds = vec![PageBuild::new(0, PictureFormat::Rgb8, 2, 2).expect("RGB8 build")];
        let red = Picture::from_pixels(2, 1, PicturePixels::Rgb8(vec![255, 0, 0, 255, 0, 0]))
            .expect("red source");
        let blue = Picture::from_pixels(2, 1, PicturePixels::Rgb8(vec![0, 0, 255, 0, 0, 255]))
            .expect("blue source");

        copy_source_into_builds(0, &red, &plans, &mut builds, 2, 2).expect("red segment");
        copy_source_into_builds(1, &blue, &plans, &mut builds, 2, 2).expect("blue segment");
        let picture = finish_build(builds.pop().expect("build"), &plans[0], 2, 2)
            .expect("finished RGB8 page");

        assert_eq!(
            picture.pixels(),
            PicturePixelsRef::Rgb8(&[255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255,])
        );
    }

    #[test]
    fn page_assembly_final_padding_is_white_and_bytes_per_pixel_correct() {
        let plan = PagePlan {
            segments: vec![PageSegment {
                source: 0,
                source_row: 0,
                rows: 1,
                destination_row: 0,
            }],
            content_rows: 1,
        };
        let plans = vec![plan.clone()];

        let grey = Picture::from_grey(2, 1, vec![0, 0]).expect("Gray8 source");
        let mut grey_builds =
            vec![PageBuild::new(0, PictureFormat::Gray8, 2, 2).expect("Gray8 build")];
        copy_source_into_builds(0, &grey, &plans, &mut grey_builds, 2, 2).expect("Gray8 segment");
        let grey_page =
            finish_build(grey_builds.pop().expect("Gray8 build"), &plan, 2, 2).expect("Gray8 page");
        assert_eq!(
            grey_page.pixels(),
            PicturePixelsRef::Gray8(&[0, 0, 255, 255])
        );

        let rgb = Picture::from_pixels(2, 1, PicturePixels::Rgb8(vec![1, 2, 3, 4, 5, 6]))
            .expect("RGB8 source");
        let mut rgb_builds =
            vec![PageBuild::new(0, PictureFormat::Rgb8, 2, 2).expect("RGB8 build")];
        copy_source_into_builds(0, &rgb, &plans, &mut rgb_builds, 2, 2).expect("RGB8 segment");
        let rgb_page =
            finish_build(rgb_builds.pop().expect("RGB8 build"), &plan, 2, 2).expect("RGB8 page");
        assert_eq!(
            rgb_page.pixels(),
            PicturePixelsRef::Rgb8(&[1, 2, 3, 4, 5, 6, 255, 255, 255, 255, 255, 255])
        );
    }

    #[test]
    fn page_assembly_format_mismatch_is_refused() {
        let plans = vec![PagePlan {
            segments: vec![PageSegment {
                source: 0,
                source_row: 0,
                rows: 1,
                destination_row: 0,
            }],
            content_rows: 1,
        }];
        let source = Picture::from_grey(2, 1, vec![0, 0]).expect("Gray8 source");
        let mut builds = vec![PageBuild::new(0, PictureFormat::Rgb8, 2, 1).expect("RGB8 build")];

        assert!(
            copy_source_into_builds(0, &source, &plans, &mut builds, 2, 1).is_err(),
            "a Gray8 source must not enter an RGB8 build"
        );
    }

    #[test]
    fn page_assembly_wrong_scaled_width_is_refused() {
        let plans = vec![PagePlan {
            segments: vec![PageSegment {
                source: 0,
                source_row: 0,
                rows: 1,
                destination_row: 0,
            }],
            content_rows: 1,
        }];
        let source = Picture::from_grey(1, 1, vec![0]).expect("narrow source");
        let mut builds = vec![PageBuild::new(0, PictureFormat::Gray8, 2, 1).expect("build")];

        assert!(copy_source_into_builds(0, &source, &plans, &mut builds, 2, 1).is_err());
    }

    #[test]
    fn page_assembly_truncated_source_rows_are_refused() {
        let plans = vec![PagePlan {
            segments: vec![PageSegment {
                source: 0,
                source_row: 0,
                rows: 2,
                destination_row: 0,
            }],
            content_rows: 2,
        }];
        let source = Picture::from_grey(2, 1, vec![0, 0]).expect("one-row source");
        let mut builds = vec![PageBuild::new(0, PictureFormat::Gray8, 2, 2).expect("build")];

        assert!(copy_source_into_builds(0, &source, &plans, &mut builds, 2, 2).is_err());
    }

    #[test]
    fn page_assembly_incomplete_build_is_refused() {
        let plan = seam_plan();
        let mut builds = vec![PageBuild::new(0, PictureFormat::Gray8, 2, 2).expect("build")];
        let source = Picture::from_grey(2, 1, vec![0, 0]).expect("first source");
        copy_source_into_builds(0, &source, std::slice::from_ref(&plan), &mut builds, 2, 2)
            .expect("first segment");

        assert!(finish_build(builds.pop().expect("build"), &plan, 2, 2).is_err());
    }

    #[test]
    fn page_assembly_rejects_unrepresentable_page_buffer() {
        assert!(PageBuild::new(0, PictureFormat::Rgb8, u32::MAX, u32::MAX).is_err());
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

    fn spawns(commands: &[Command]) -> Vec<(TaskId, Task)> {
        commands
            .iter()
            .filter_map(|command| match command {
                Command::Spawn { task, work } => Some((*task, work.clone())),
                _ => None,
            })
            .collect()
    }

    fn fetch_task_with(commands: &[Command], needle: &str) -> (TaskId, Task) {
        spawns(commands)
            .into_iter()
            .find(|(_, work)| matches!(work, Task::Fetch { url, .. } if url.contains(needle)))
            .expect("matching fetch task")
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
        let (homepage_task, _) = fetch_task_with(&commands, "/comic/main");
        runner.task_outcome(
            homepage_task,
            TaskOutcome::Completed(b"<html></html>".to_vec()),
        );
        let commands = runner.action(action_id(LIBRARY));
        let (task, _) = fetch_task_with(&commands, "/library?");
        let commands = runner.task_outcome(task, TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()));
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
        runner.task_outcome(
            content_task,
            TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
        );
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
        page_count: usize,
        current_page: usize,
        chrome_visible: bool,
    ) -> AppRunner<Bomtoon> {
        let width = u32::try_from(metrics.width).expect("positive panel width");
        let panel_height = u32::try_from(metrics.height).expect("positive panel height");
        let format = metrics.picture_format;
        let images = (0..page_count)
            .map(|source| episode_image(source, width, panel_height))
            .collect::<Vec<_>>();
        let (plans, total_pages) =
            page_plan(&images, width, panel_height).expect("seeded reader plans");
        let limits = reader_limits(format);
        let mut window = VecDeque::new();
        for page in current_page.saturating_add(1)..page_count {
            if window.len() == limits.pages {
                break;
            }
            let byte_len = format
                .byte_len(width, panel_height)
                .expect("seeded page byte length");
            let pixels = match format {
                PictureFormat::Gray8 => PicturePixels::Gray8(vec![127; byte_len]),
                PictureFormat::Rgb8 => PicturePixels::Rgb8(vec![127; byte_len]),
            };
            let picture = Picture::from_pixels(width, panel_height, pixels).expect("ready page");
            window.push_back(PageEntry::Ready { page, picture });
        }
        AppRunner::with_metrics(
            Bomtoon {
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
                    plans,
                    page: current_page,
                    total_pages,
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
            app.retry = Retry::Page(0);
            app.reader_tasks = BTreeMap::from([
                (
                    foreground,
                    ReaderTaskEntry {
                        generation: 1,
                        purpose: ReaderTaskPurpose::ForegroundSource { source: 0, page: 0 },
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
                .insert(3, FetchIntent::Foreground { page: 3 });
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
            reader.window = VecDeque::from([
                PageEntry::Ready {
                    page: 1,
                    picture: Picture::from_pixels(1, 2, ready_pixels).expect("ready page"),
                },
                PageEntry::Building(PageBuild::new(2, format, 1, 2).expect("building page")),
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
                        .all(|entry| matches!(entry, PageEntry::Ready { .. }))
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
        let (plans, total_pages) = page_plan(&images, 1, 2).expect("seam plans");
        assert_eq!(total_pages, 3);
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
        let window = VecDeque::from([PageEntry::Ready {
            page: 0,
            picture: backward_picture,
        }]);
        let picture = TilePicture::new(PictureHandle(7), 1, 2);
        let runner = AppRunner::with_metrics(
            Bomtoon {
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
                    plans,
                    page: 2,
                    total_pages,
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
            cover_url: None,
            episode_alias: "ep-1".to_owned(),
            episode_title: "Episode One".to_owned(),
        });
        app.episodes.push(Episode {
            alias: "ep-1".to_owned(),
            title: "Episode One".to_owned(),
            purchase: model::PurchaseState::Owned,
            ticket_quantity: None,
        });
        app.selected_title = "Hunter Q".to_owned();
        app.page = 3;
        app.next_library_page = Some(4);
        app.next_recent_page = Some(5);
        app.total_library_titles = 91;
        app.total_recent_titles = 62;
        app.library_loaded = true;
        app.recent_loaded = true;
    }

    fn assert_all_account_data_cleared(app: &Bomtoon) {
        assert!(app.comics.is_empty());
        assert!(app.recent.is_empty());
        assert!(app.episodes.is_empty());
        assert!(app.selected_title.is_empty());
        assert_eq!(app.page, 0);
        assert_eq!(app.next_library_page, None);
        assert_eq!(app.next_recent_page, None);
        assert_eq!(app.total_library_titles, 0);
        assert_eq!(app.total_recent_titles, 0);
        assert!(!app.library_loaded);
        assert!(!app.recent_loaded);
    }

    fn assert_seeded_account_data_is_kept(app: &Bomtoon) {
        assert_eq!(app.comics.len(), 1);
        assert_eq!(app.recent.len(), 1);
        assert_eq!(app.episodes.len(), 1);
        assert_eq!(app.selected_title, "Hunter Q");
        assert_eq!(app.page, 3);
        assert_eq!(app.next_library_page, Some(4));
        assert_eq!(app.next_recent_page, Some(5));
        assert_eq!(app.total_library_titles, 91);
        assert_eq!(app.total_recent_titles, 62);
        assert!(app.library_loaded);
        assert!(app.recent_loaded);
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
        let (task, _) = fetch_task_with(&commands, "/asset/user");
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
                view: View::Main,
                destination,
                library_loaded: true,
                recent_loaded: true,
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
    fn signed_out_start_opens_public_featured_without_protected_shelves() {
        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::SignedOut,
            ..Bomtoon::default()
        });

        let commands = runner.start();

        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert!(commands.iter().any(is_homepage_fetch));
        assert!(!commands.iter().any(is_library_or_recent_fetch));
    }

    #[test]
    fn signed_in_start_opens_public_featured_with_independent_account_probe() {
        let (runner, commands) = started();

        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert!(commands.iter().any(is_homepage_fetch));
        assert!(!commands.iter().any(is_library_or_recent_fetch));
        assert!(spawns(&commands)
            .iter()
            .any(|(_, work)| *work == api::asset_summary()));
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
                library_loaded: true,
                recent_loaded: true,
                ..Bomtoon::default()
            }
            .screen();
            let bar = screen
                .nav_bar
                .as_ref()
                .expect("one fixed destination bar");

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
            let layout =
                screen.layout_with(&CLARA_BW_METRICS, &Chrome::measuring(true));
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
            (
                RECENT,
                MainDestination::Recent,
                "/recent?",
                "/library?",
            ),
            (
                LIBRARY,
                MainDestination::Library,
                "/library?",
                "/recent?",
            ),
        ] {
            let mut runner = AppRunner::new(Bomtoon {
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
            view: View::Main,
            destination: MainDestination::Recent,
            recent_loaded: true,
            page: 4,
            ..Bomtoon::default()
        });
        let commands = runner.action(action_id(RECENT));
        assert_eq!(runner.app().page, 0);
        assert!(spawns(&commands).is_empty());
    }

    #[test]
    fn navigation_pending_protected_shelf_ignores_destination_taps() {
        let empty_recent_response = br#"{
            "result":"SUCCESS",
            "data":{
                "content":[],
                "number":0,
                "totalPages":1,
                "totalElements":0
            }
        }"#;
        for (open, queued, destination, pending, response) in [
            (
                RECENT,
                LIBRARY,
                MainDestination::Recent,
                Pending::Recent(0),
                empty_recent_response.as_slice(),
            ),
            (
                LIBRARY,
                RECENT,
                MainDestination::Library,
                Pending::Library(0),
                LIBRARY_RESPONSE,
            ),
        ] {
            let mut runner = AppRunner::new(Bomtoon {
                view: View::Main,
                destination: MainDestination::Featured,
                ..Bomtoon::default()
            });
            let commands = runner.action(action_id(open));
            let (task, _) = only_spawn(&commands);
            assert_eq!(runner.app().pending, Some(pending));

            let commands = runner.action(action_id(queued));

            assert_eq!(runner.app().view, View::Main);
            assert_eq!(runner.app().destination, destination);
            assert_eq!(runner.app().pending, Some(pending));
            assert_eq!(runner.app().task, Some(task));
            assert!(spawns(&commands).is_empty());

            runner.task_outcome(task, TaskOutcome::Completed(response.to_vec()));
            assert_eq!(runner.app().view, View::Main);
            assert_eq!(runner.app().destination, destination);
            assert_eq!(runner.app().pending, None);
            assert_eq!(runner.app().task, None);
        }
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

        runner.task_outcome(task, TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()));
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
        app.open_comic(&mut context, 0);
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
        assert!(!commands.iter().any(is_library_or_recent_fetch));
        assert!(!spawns(&commands)
            .iter()
            .any(|(_, work)| matches!(work, Task::Fetch { url, .. } if url.contains("/contents/"))));
    }

    #[test]
    fn back_returns_destination_page_one_after_account_and_episodes() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.app_mut().destination = MainDestination::Recent;
        runner.app_mut().recent_loaded = true;
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
    }

    #[test]
    fn owned_sample_and_free_episode_rows_are_actions() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(
            content_task,
            TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
        );
        let screen = last_screen(&commands);
        let actions = screen
            .nodes
            .iter()
            .filter_map(|node| match node {
                Node::Button { action, .. } => Some(*action),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(actions.contains(&action_id("episode-0")));
        assert!(actions.contains(&action_id("episode-1")));
        assert!(actions.contains(&action_id("episode-2")));
        assert!(!actions.contains(&action_id("episode-3")));
    }

    #[test]
    fn ticket_comic_shows_cached_ticket_total_and_episode_quantity() {
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
        let commands = runner.task_outcome(
            content_task,
            TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
        );
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(drawn.contains("Tickets 4"));
        assert!(drawn.contains("Ticket · 1"));
        assert_eq!(drawn.matches("Ticket ·").count(), 1);
        assert!(!screen.nodes.iter().any(
            |node| matches!(node, Node::Button { action, .. } if *action == action_id("episode-3"))
        ));
        assert_fits(&screen);
    }

    #[test]
    fn coin_only_comic_has_no_ticket_ui() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(
            content_task,
            TaskOutcome::Completed(COIN_ONLY_CONTENT_RESPONSE.to_vec()),
        );
        let drawn = format!("{:?}", last_screen(&commands));
        assert!(!drawn.contains("Tickets"));
        assert!(!drawn.contains("Ticket ·"));
    }

    #[test]
    fn ticket_comic_without_cached_summary_does_not_fetch_wallet() {
        let (mut runner, _) = loaded_library();
        complete_initial_summary(&mut runner);
        runner.app_mut().wallet.summary = None;

        let commands = runner.action(action_id("comic-0"));
        let (content_task, work) = only_spawn(&commands);
        assert!(matches!(
            work,
            Task::Fetch { ref url, .. }
                if url.contains("/api/balcony-api-v2/contents/hunter_q?")
        ));
        let commands = runner.task_outcome(
            content_task,
            TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
        );
        assert!(spawns(&commands).is_empty());
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(drawn.contains("Tickets unavailable"));
        assert!(drawn.contains("Ticket · 1"));
        assert_fits(&screen);
    }

    #[test]
    fn ticket_comic_maximum_quantity_fits_clara_bw() {
        let mut app = Bomtoon {
            view: View::Episodes,
            selected_title: "Maximum Tickets".to_owned(),
            wallet: WalletState {
                summary: Some(WalletSummary {
                    coins: model::AssetAmounts::default(),
                    tickets: model::AssetAmounts {
                        standard: usize::MAX,
                        bonus: 0,
                        free: 0,
                    },
                }),
                ..WalletState::default()
            },
            ..Bomtoon::default()
        };
        app.episodes.push(Episode {
            alias: "maximum-ticket".to_owned(),
            title: "Maximum Ticket Episode".to_owned(),
            purchase: model::PurchaseState::NotOwned,
            ticket_quantity: Some(usize::MAX),
        });

        let screen = app.episode_screen();
        let drawn = format!("{screen:?}");
        assert!(drawn.contains(&format!("Tickets {}", usize::MAX)));
        assert!(drawn.contains(&format!("Ticket · {}", usize::MAX)));
        assert!(!screen
            .nodes
            .iter()
            .any(|node| matches!(node, Node::Button { .. })));
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
        let commands = runner.task_outcome(
            content_task,
            TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
        );
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
    fn center_toggles_chrome_and_boundary_noop_preserves_it() {
        let mut runner = seeded_reader(1, 0, false);
        let commands = runner.action(action_id(READER_CHROME));
        assert_eq!(
            last_screen(&commands)
                .reading_surface
                .expect("surface")
                .chrome,
            ReadingChrome::Overlay
        );
        assert!(runner.action(action_id(READER_NEXT)).is_empty());
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
    fn successful_page_turn_hides_chrome_and_replaces_handle_in_order() {
        let mut runner = seeded_reader(2, 0, true);
        let commands = runner.action(action_id(READER_NEXT));
        let reader = runner.app().reader.as_ref().expect("reader");
        assert_eq!(reader.page, 1);
        assert!(!reader.chrome_visible);
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
            .position(|command| matches!(command, Command::DropPicture(_)))
            .expect("DropPicture");
        assert!(put < set && set < drop);
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
        assert_eq!(set_indices.len(), 1);
        assert_eq!(drop_indices.len(), 1);
        assert!(put_indices[0] < set_indices[0] && set_indices[0] < drop_indices[0]);
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
        let Command::SetScreen(screen) = &commands[set_indices[0]] else {
            unreachable!();
        };
        assert!(
            screen.reading_surface.is_some(),
            "prepared turn showed loading"
        );
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
                .map(entry_page)
                .collect::<Vec<_>>();
            assert_eq!(initial_pages, (1..=limits.pages).collect::<Vec<_>>());
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
            assert_eq!(reader.page, 1);
            assert_eq!(
                reader.window.iter().map(entry_page).collect::<Vec<_>>(),
                (2..=limits.pages).collect::<Vec<_>>()
            );
            assert!(reader
                .window
                .iter()
                .all(|entry| matches!(entry, PageEntry::Ready { .. })));
            let maintenance = reader.maintenance_task.expect("turn maintenance");

            let commands = runner.task_outcome(maintenance, TaskOutcome::Completed(Vec::new()));
            assert!(!commands.iter().any(|command| matches!(
                command,
                Command::PutPicture { .. } | Command::SetScreen(_) | Command::DropPicture(_)
            )));
            let (far_edge_task, work) = only_spawn(&commands);
            assert!(matches!(work, Task::Fetch { .. }));
            let far_edge = limits.pages + 1;
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
                reader.window.iter().map(entry_page).collect::<Vec<_>>(),
                (2..=far_edge).collect::<Vec<_>>()
            );
            assert!(matches!(
                reader.window.back(),
                Some(PageEntry::Building(build)) if build.page == far_edge
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
        assert_eq!(reader.page, 0);
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
                let expected = limits.pages.min(reader.plans.len().saturating_sub(1));
                reader.window.len() == expected
                    && reader
                        .window
                        .iter()
                        .all(|entry| matches!(entry, PageEntry::Ready { .. }))
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
            .all(|entry| matches!(entry, PageEntry::Ready { .. })));
        assert!(runner.app().reader_tasks.is_empty());
    }

    fn task7_seam_next(
        runner: &mut AppRunner<Bomtoon>,
        format: PictureFormat,
        page_width: u32,
        page_height: u32,
    ) -> Vec<u8> {
        let next_commands = runner.action(action_id(READER_NEXT));
        assert_eq!(runner.app().reader.as_ref().expect("next reader").page, 1);
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
        assert_eq!(runner.app().reader.as_ref().expect("chrome reader").page, 1);
        next_png
    }

    fn task7_retry_previous(
        runner: &mut AppRunner<Bomtoon>,
        page_width: u32,
        page_height: u32,
        first_pixels: &PicturePixels,
    ) -> Vec<u8> {
        let previous_commands = runner.action(action_id(READER_PREVIOUS));
        assert!(last_screen(&previous_commands).reading_surface.is_none());
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
        assert_eq!(runner.app().retry, Retry::Page(0));

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
            runner.app().reader.as_ref().expect("Previous reader").page,
            0
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
                reader.window.push_back(PageEntry::Building(
                    PageBuild::new(1, format, 1, 1).expect("page one build"),
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
            assert!(screen.reading_surface.is_none());
            let app = runner.app();
            let reader = app.reader.as_ref().expect("reader");
            assert_eq!(reader.page, 0);
            assert_eq!(
                reader.picture.map(|picture| picture.handle),
                Some(PictureHandle(7))
            );
            assert_eq!(app.retry, Retry::Page(1));
            assert_eq!(app.foreground_reader_task, Some(prefetch));
            assert_eq!(
                app.reader_tasks.get(&prefetch).map(|entry| entry.purpose),
                Some(ReaderTaskPurpose::ForegroundSource { source: 1, page: 1 })
            );
            assert_eq!(reader.source_fetches.get(&1), Some(&prefetch));
            assert_reader_bounds(app);
        }
    }

    #[test]
    fn previous_rebase_caches_retained_future_prefetch_until_maintenance_uses_it() {
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
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::Spawn { .. } | Command::Cancel(_))));
        let reader = runner.app().reader.as_ref().expect("reader");
        assert_eq!(
            reader.window.iter().map(entry_page).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(runner.app().foreground_reader_task, Some(target_task));
        assert_eq!(
            runner
                .app()
                .reader_tasks
                .get(&future_task)
                .map(|entry| entry.purpose),
            Some(ReaderTaskPurpose::PrefetchSource { source: 4 })
        );

        let commands = runner.task_outcome(future_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::Spawn { .. })));
        let reader = runner.app().reader.as_ref().expect("reader");
        assert!(reader.source_cache.contains_key(&4));
        assert!(!reader.source_fetches.contains_key(&4));
        assert_eq!(
            reader.window.iter().map(entry_page).collect::<Vec<_>>(),
            vec![2]
        );
        assert_reader_bounds(runner.app());

        runner.task_outcome(target_task, TaskOutcome::Failed(TaskError::TimedOut));
        let reader = runner.app().reader.as_ref().expect("reader");
        assert!(runner.app().problem.is_some());
        assert!(reader.source_cache.contains_key(&4));
        assert_reader_bounds(runner.app());

        let commands = runner.action(action_id(RETRY));
        let (retry_task, retry_work) = only_spawn(&commands);
        assert!(matches!(retry_work, Task::Fetch { .. }));
        let reader = runner.app().reader.as_ref().expect("reader");
        assert!(runner.app().problem.is_none());
        assert_eq!(runner.app().foreground_reader_task, Some(retry_task));
        assert!(reader.source_cache.contains_key(&4));
        assert_eq!(
            runner
                .app()
                .reader_tasks
                .get(&retry_task)
                .map(|entry| entry.purpose),
            Some(ReaderTaskPurpose::ForegroundSource { source: 2, page: 2 })
        );
        assert_reader_bounds(runner.app());

        runner.task_outcome(retry_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
        let reader = runner.app().reader.as_ref().expect("reader");
        assert_eq!(reader.page, 2);
        assert!(reader.source_cache.contains_key(&4));
        let maintenance = reader.maintenance_task.expect("rebase maintenance");
        assert_reader_bounds(runner.app());

        runner.task_outcome(maintenance, TaskOutcome::Completed(Vec::new()));
        let app = runner.app();
        let reader = app.reader.as_ref().expect("reader");
        assert_eq!(
            reader.window.iter().map(entry_page).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert!(matches!(
            reader.window.get(1),
            Some(PageEntry::Building(build)) if build.page == 4 && build.next_segment == 1
        ));
        assert!(!reader.source_fetches.contains_key(&4));
        assert!(!app.reader_tasks.values().any(|entry| matches!(
            entry.purpose,
            ReaderTaskPurpose::PrefetchSource { source: 4 }
                | ReaderTaskPurpose::ForegroundSource { source: 4, .. }
        )));
        assert_reader_bounds(app);
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
            reader.window.iter().map(entry_page).collect::<Vec<_>>(),
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
        assert_eq!(reader.page, 1);
        assert!(reader.window.iter().all(|entry| entry_page(entry) >= 1));
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
            assert!(last_screen(&commands).reading_surface.is_none());
            assert_eq!(
                runner
                    .app()
                    .reader_tasks
                    .get(&first_task)
                    .map(|entry| entry.purpose),
                Some(ReaderTaskPurpose::ForegroundSource { source: 0, page: 1 })
            );
            let reader = runner.app().reader.as_ref().expect("reader");
            assert_eq!(
                reader.window.iter().map(entry_page).collect::<Vec<_>>(),
                vec![1]
            );
            assert!(reader.window.iter().all(|entry| entry_page(entry) >= 1));

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
                Some(ReaderTaskPurpose::ForegroundSource { source: 1, page: 1 })
            );

            let commands =
                runner.task_outcome(second_task, TaskOutcome::Completed(WHITE_1X2_WEBP.to_vec()));
            let maintenance = assert_previous_seam_upload(&runner, &commands, format);

            runner.task_outcome(maintenance, TaskOutcome::Completed(Vec::new()));
            let reader = runner.app().reader.as_ref().expect("reader");
            assert!(reader.window.iter().all(|entry| entry_page(entry) >= 1));
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
        assert_eq!(reader.page, 0);
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
    fn startup_loads_homepage_and_asset_summary_independently() {
        let (_, commands) = started();
        let spawned = spawns(&commands);
        assert_eq!(spawned.len(), 2);
        assert!(spawned
            .iter()
            .any(|(_, work)| *work == api::homepage()));
        assert!(spawned
            .iter()
            .any(|(_, work)| *work == api::asset_summary()));
        assert!(spawned.iter().all(
            |(_, work)| !matches!(work, Task::Fetch { url, .. } if url.contains("/library?") || url.contains("/recent?"))
        ));
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
        runner.app_mut().wallet.summary_refresh_queued = true;
        runner.action(ActionId::BACK);

        let commands = runner.action(action_id("comic-0"));
        let (content_task, _) = only_spawn(&commands);
        runner.task_outcome(
            content_task,
            TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
        );
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
    fn account_saturated_reentry_defers_ticket_and_preserves_server_order() {
        let (mut runner, _) = loaded_library();
        let first_open = runner.action(action_id(ACCOUNT));
        let (old_coin, _) = fetch_task_with(&first_open, "coinKind=COIN");
        let (old_ticket, _) = fetch_task_with(&first_open, "coinKind=TICKET");

        runner.action(ActionId::BACK);
        let second_open = runner.action(action_id(ACCOUNT));
        assert!(second_open.contains(&Command::Cancel(old_coin)));
        assert!(second_open.contains(&Command::Cancel(old_ticket)));
        let (current_coin, _) = fetch_task_with(&second_open, "coinKind=COIN");
        assert!(
            spawns(&second_open)
                .iter()
                .all(|(_, work)| !matches!(work, Task::Fetch { url, .. } if url.contains("coinKind=TICKET"))),
            "the fourth occupied slot must defer the replacement ticket"
        );
        assert!(!runner.app().wallet.coin_history_error);
        assert!(!runner.app().wallet.ticket_history_error);
        let loading = format!("{:?}", last_screen(&second_open));
        assert!(loading.contains("Coin history"));
        assert!(loading.contains("Ticket history"));
        assert!(loading.matches("Loading…").count() >= 2);

        let resumed = runner.task_outcome(old_coin, TaskOutcome::Cancelled);
        let (current_ticket, work) = only_spawn(&resumed);
        assert!(matches!(
            work,
            Task::Fetch { ref url, .. } if url.contains("coinKind=TICKET")
        ));
        assert_ne!(current_coin, current_ticket);
        assert!(!runner.app().wallet.ticket_history_error);

        runner.task_outcome(old_ticket, TaskOutcome::Cancelled);
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
                .map(|row| (row.subtype, row.quantity))
                .collect::<Vec<_>>(),
            [
                (model::AssetSubtype::Standard, 11),
                (model::AssetSubtype::Bonus, 12),
                (model::AssetSubtype::Standard, 21),
                (model::AssetSubtype::Free, 22),
            ]
        );
    }

    #[test]
    fn account_open_uses_capacity_left_by_cancelled_reader_work() {
        let (mut runner, image_task) = reader_waiting_for_first_image();
        let commands = runner.action(ActionId::BACK);
        assert!(commands.contains(&Command::Cancel(image_task)));
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

    #[test]
    fn wallet_credential_failure_keeps_public_featured_and_ignores_homepage_outcome() {
        let (mut runner, commands) = started();
        let (homepage_task, _) = fetch_task_with(&commands, "/comic/main");
        let (summary_task, _) = fetch_task_with(&commands, "/asset/user");
        runner.task_outcome(summary_task, TaskOutcome::Failed(TaskError::NoCredential));
        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert_eq!(runner.app().task, None);
        assert_eq!(runner.app().pending, None);

        runner.task_outcome(
            homepage_task,
            TaskOutcome::Completed(b"<html></html>".to_vec()),
        );
        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_eq!(runner.app().view, View::Main);
        assert!(runner.app().comics.is_empty());
    }

    #[test]
    fn sign_out_retains_public_feed_pictures_and_tasks_while_clearing_protected_state() {
        let public_url = "https://image.balcony.studio/tw/contents/public.webp".to_owned();
        let shared_url = "https://image.balcony.studio/tw/contents/shared.webp".to_owned();
        let shared_loading_url =
            "https://image.balcony.studio/tw/contents/shared-loading.webp".to_owned();
        let protected_url =
            "https://image.balcony.studio/tw/contents/protected.webp".to_owned();
        let public_cover_task = TaskId(90);
        let protected_cover_task = TaskId(91);
        let public_homepage_task = TaskId(92);
        let wallet_task = TaskId(93);
        let reader_task = TaskId(94);
        let shared_cover_task = TaskId(95);
        let shared_picture = TilePicture::new(PictureHandle(81), 10, 20);
        let protected_picture = TilePicture::new(PictureHandle(82), 10, 20);
        let mut runner = seeded_reader(1, 0, false);
        {
            let app = runner.app_mut();
            app.view = View::Account;
            app.destination = MainDestination::Library;
            app.page = 4;
            app.featured = FeaturedState {
                status: FeaturedStatus::Ready,
                generation: 7,
                featured: vec![
                    ShelfComic {
                        title: "Public".to_owned(),
                        alias: "public".to_owned(),
                        cover_url: Some(public_url.clone()),
                    },
                    ShelfComic {
                        title: "Shared".to_owned(),
                        alias: "shared".to_owned(),
                        cover_url: Some(shared_url.clone()),
                    },
                ],
                recommended: vec![
                    ShelfComic {
                        title: "Recommended".to_owned(),
                        alias: "recommended".to_owned(),
                        cover_url: Some(shared_url.clone()),
                    },
                    ShelfComic {
                        title: "Shared loading".to_owned(),
                        alias: "shared-loading".to_owned(),
                        cover_url: Some(shared_loading_url.clone()),
                    },
                ],
                page: 2,
                refresh: FeaturedRefresh {
                    loaded_day: Some(local_day(30)),
                    desired_day: Some(local_day(31)),
                    active_day: Some(local_day(30)),
                    local_day_pending: true,
                },
                ..FeaturedState::default()
            };
            app.comics = vec![Comic {
                alias: "protected".to_owned(),
                title: "Protected".to_owned(),
                cover_url: Some(protected_url.clone()),
                owned_episodes: 1,
                total_episodes: 2,
            }];
            app.recent = vec![RecentEntry {
                content_alias: "shared".to_owned(),
                content_title: "Shared protected placement".to_owned(),
                cover_url: Some(shared_loading_url.clone()),
                episode_alias: "ep-1".to_owned(),
                episode_title: "Episode One".to_owned(),
            }];
            app.episodes = vec![Episode {
                alias: "ep-1".to_owned(),
                title: "Episode One".to_owned(),
                purchase: model::PurchaseState::Owned,
                ticket_quantity: None,
            }];
            app.library_loaded = true;
            app.recent_loaded = true;
            app.selected_content_alias = "protected".to_owned();
            app.selected_title = "Protected".to_owned();
            app.wallet = WalletState {
                summary: Some(test_wallet_summary()),
                summary_task: Some(wallet_task),
                tasks: BTreeMap::from([(
                    wallet_task,
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
                reader_task,
                ReaderTaskEntry {
                    generation: app.reader_generation,
                    purpose: ReaderTaskPurpose::Maintenance,
                },
            );
            app.shelf_tasks.insert(
                public_homepage_task,
                ShelfTaskPurpose::Homepage {
                    generation: app.featured.generation,
                    refresh_day: Some(local_day(30)),
                },
            );
            app.covers.generation = 11;
            app.covers.visible_urls = vec![
                public_url.clone(),
                shared_loading_url.clone(),
                protected_url.clone(),
            ];
            app.covers.entries = BTreeMap::from([
                (
                    public_url.clone(),
                    CoverState::Loading(public_cover_task),
                ),
                (shared_url.clone(), CoverState::Ready(shared_picture)),
                (
                    shared_loading_url.clone(),
                    CoverState::Loading(shared_cover_task),
                ),
                (
                    protected_url.clone(),
                    CoverState::Loading(protected_cover_task),
                ),
                (
                    "https://image.balcony.studio/tw/contents/protected-ready.webp".to_owned(),
                    CoverState::Ready(protected_picture),
                ),
            ]);
            app.covers.tasks = BTreeMap::from([
                (
                    public_cover_task,
                    CoverTask {
                        generation: 11,
                        url: public_url.clone(),
                        source: CoverSource::Public,
                    },
                ),
                (
                    shared_cover_task,
                    CoverTask {
                        generation: 11,
                        url: shared_loading_url.clone(),
                        source: CoverSource::Protected,
                    },
                ),
                (
                    protected_cover_task,
                    CoverTask {
                        generation: 11,
                        url: protected_url.clone(),
                        source: CoverSource::Protected,
                    },
                ),
            ]);
        }
        let public_featured = runner.app().featured.featured.clone();
        let public_recommended = runner.app().featured.recommended.clone();
        let public_refresh = runner.app().featured.refresh.clone();

        let commands = runner.action(action_id(SIGN_OUT));
        let (logout_task, work) = only_spawn(&commands);
        assert_eq!(
            work,
            Task::RevokeCredential {
                credential: "bomtoon-access-token".to_owned(),
            }
        );
        assert!(!commands.contains(&Command::Cancel(public_homepage_task)));
        assert!(!commands.contains(&Command::Cancel(public_cover_task)));

        let commands = runner.task_outcome(logout_task, TaskOutcome::Completed(Vec::new()));
        let app = runner.app();
        assert_eq!(app.account, AccountState::SignedOut);
        assert_eq!(app.view, View::Main);
        assert_eq!(app.destination, MainDestination::Featured);
        assert_eq!(app.page, 0);
        assert_eq!(app.featured.page, 0);
        assert_eq!(app.featured.featured, public_featured);
        assert_eq!(app.featured.recommended, public_recommended);
        assert_eq!(app.featured.refresh, public_refresh);
        assert_eq!(
            app.covers.entries.get(&shared_url),
            Some(&CoverState::Ready(shared_picture))
        );
        assert_eq!(
            app.covers.entries.get(&public_url),
            Some(&CoverState::Loading(public_cover_task))
        );
        assert_eq!(
            app.covers.entries.get(&shared_loading_url),
            Some(&CoverState::Loading(shared_cover_task))
        );
        assert!(!app.covers.entries.contains_key(&protected_url));
        assert!(app.comics.is_empty());
        assert!(app.recent.is_empty());
        assert!(app.episodes.is_empty());
        assert!(app.selected_content_alias.is_empty());
        assert!(app.selected_title.is_empty());
        assert!(app.reader_selection.is_none());
        assert!(app.reader.is_none());
        assert!(app.reader_tasks.is_empty());
        assert!(app.wallet.summary.is_none());
        assert!(app.wallet.coin_history.is_empty());
        assert!(app.wallet.ticket_history.is_empty());
        assert!(app.wallet.tasks.is_empty());
        assert!(app.shelf_tasks.contains_key(&public_homepage_task));
        assert!(app.covers.tasks.contains_key(&public_cover_task));
        assert_eq!(
            app.covers.tasks
                .get(&shared_cover_task)
                .map(|task| task.source),
            Some(CoverSource::Public)
        );
        assert!(!app.covers.tasks.contains_key(&protected_cover_task));
        assert!(commands.contains(&Command::Cancel(protected_cover_task)));
        assert!(commands.contains(&Command::Cancel(wallet_task)));
        assert!(commands.contains(&Command::Cancel(reader_task)));
        assert!(!commands.contains(&Command::Cancel(public_homepage_task)));
        assert!(!commands.contains(&Command::Cancel(public_cover_task)));
        assert!(!commands.contains(&Command::Cancel(shared_cover_task)));
        assert!(commands.contains(&Command::DropPicture(PictureHandle(7))));
        assert!(commands.contains(&Command::DropPicture(PictureHandle(82))));
        assert!(!commands.contains(&Command::DropPicture(shared_picture.handle)));
        assert_eq!(
            last_screen(&commands)
                .top_bar
                .expect("signed-out Featured top bar")
                .actions[0]
                .label,
            "Sign in"
        );

        for action in [RECENT, LIBRARY] {
            runner.app_mut().view = View::Main;
            runner.app_mut().destination = MainDestination::Featured;
            let commands = runner.action(action_id(action));
            assert_eq!(runner.app().view, View::Status);
            assert!(spawns(&commands).is_empty());
        }
    }

    #[test]
    fn full_exit_cancels_public_and_protected_work_and_drops_all_session_pictures() {
        let pending_task = TaskId(70);
        let shelf_task = TaskId(71);
        let public_cover_task = TaskId(72);
        let protected_cover_task = TaskId(73);
        let wallet_task = TaskId(74);
        let reader_task = TaskId(75);
        let public_url = "https://image.balcony.studio/tw/contents/public-exit.webp".to_owned();
        let protected_url =
            "https://image.balcony.studio/tw/contents/protected-exit.webp".to_owned();
        let ready_url = "https://image.balcony.studio/tw/contents/ready-exit.webp".to_owned();
        let ready_picture = TilePicture::new(PictureHandle(83), 10, 20);
        let mut runner = AppRunner::new(Bomtoon {
            pending: Some(Pending::Content(0)),
            task: Some(pending_task),
            featured: FeaturedState {
                generation: 4,
                featured: vec![ShelfComic {
                    title: "Ready".to_owned(),
                    alias: "ready".to_owned(),
                    cover_url: Some(ready_url.clone()),
                }],
                refresh: FeaturedRefresh {
                    local_day_pending: true,
                    active_day: Some(local_day(30)),
                    desired_day: Some(local_day(31)),
                    ..FeaturedRefresh::default()
                },
                ..FeaturedState::default()
            },
            shelf_tasks: BTreeMap::from([(
                shelf_task,
                ShelfTaskPurpose::Homepage {
                    generation: 4,
                    refresh_day: None,
                },
            )]),
            covers: CoverCache {
                generation: 6,
                entries: BTreeMap::from([
                    (
                        public_url.clone(),
                        CoverState::Loading(public_cover_task),
                    ),
                    (
                        protected_url.clone(),
                        CoverState::Loading(protected_cover_task),
                    ),
                    (ready_url, CoverState::Ready(ready_picture)),
                ]),
                tasks: BTreeMap::from([
                    (
                        public_cover_task,
                        CoverTask {
                            generation: 6,
                            url: public_url,
                            source: CoverSource::Public,
                        },
                    ),
                    (
                        protected_cover_task,
                        CoverTask {
                            generation: 6,
                            url: protected_url,
                            source: CoverSource::Protected,
                        },
                    ),
                ]),
                ..CoverCache::default()
            },
            wallet: WalletState {
                summary_task: Some(wallet_task),
                tasks: BTreeMap::from([(
                    wallet_task,
                    WalletTaskPurpose::Summary { generation: 1 },
                )]),
                ..WalletState::default()
            },
            reader_tasks: BTreeMap::from([(
                reader_task,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: ReaderTaskPurpose::Manifest,
                },
            )]),
            reader_generation: 1,
            ..Bomtoon::default()
        });

        let commands = runner.exit();
        let cancelled = cancelled_tasks(&commands);
        assert_eq!(
            cancelled,
            BTreeSet::from([
                pending_task,
                shelf_task,
                public_cover_task,
                protected_cover_task,
                wallet_task,
                reader_task,
            ])
        );
        assert!(commands.contains(&Command::DropPicture(ready_picture.handle)));
        assert!(runner.app().shelf_tasks.is_empty());
        assert!(runner.app().covers.tasks.is_empty());
        assert!(runner.app().covers.entries.is_empty());
        assert!(runner.app().wallet.tasks.is_empty());
        assert!(runner.app().reader_tasks.is_empty());
        assert!(!runner.app().featured.refresh.local_day_pending);
        assert_eq!(runner.app().featured.refresh.active_day, None);
        assert_eq!(runner.app().featured.refresh.desired_day, None);
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
        let (homepage, _) = fetch_task_with(&commands, "/comic/main");
        let (task, _) = fetch_task_with(&commands, "/asset/user");
        let commands = runner.task_outcome(task, TaskOutcome::Failed(TaskError::NoCredential));

        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_eq!(runner.app().view, View::Main);
        assert!(format!("{:?}", last_screen(&commands)).contains("Sign in"));

        let commands = runner.action(action_id(SIGN_IN));
        assert_login_instructions(&last_screen(&commands));

        let commands = runner.action(action_id(RETRY));
        assert!(commands.contains(&Command::Cancel(homepage)));
        assert!(!commands.iter().any(is_homepage_fetch));
        let commands = runner.task_outcome(homepage, TaskOutcome::Cancelled);
        let (_, work) = fetch_task_with(&commands, "/comic/main");
        assert_eq!(work, api::homepage());
        assert_eq!(runner.app().account, AccountState::Active);
        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().destination, MainDestination::Featured);
        assert!(!commands.iter().any(is_library_or_recent_fetch));
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
            screen
                .page_turns
                .as_ref()
                .and_then(|turns| turns.position),
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
        assert!(runner.app().library_loaded);
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
        let (_, work) = fetch_task_with(&commands, "/comic/main");
        assert_eq!(work, api::homepage());
        assert_eq!(runner.app().account, AccountState::Active);
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
        assert!(url.contains("/api/balcony-api-v2/contents/hunter_q?"));
        assert!(matches!(
            credential,
            Some(value)
                if value.secret == "bomtoon-access-token"
                    && value.header == SecretHeader::Bearer
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
            let (runner, commands) = failed_start(error);
            assert_eq!(runner.app().account, expected);
            if expected == AccountState::SignedOut {
                assert_eq!(runner.app().view, View::Main);
                assert!(format!("{:?}", last_screen(&commands)).contains("Sign in"));
            } else {
                assert_login_instructions(&last_screen(&commands));
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
    fn comic_selection_uses_json_content_and_back_returns_to_the_library() {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id("comic-0"));
        let (task, work) = only_spawn(&commands);
        assert!(matches!(
            work,
            Task::Fetch { ref url, .. }
                if url.contains("/api/balcony-api-v2/contents/hunter_q?")
        ));

        let commands = runner.task_outcome(task, TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()));
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
    fn episode_pagination_keeps_six_items_per_page() {
        assert_eq!(EPISODE_ITEMS_PER_PAGE, 6);
        assert_eq!(
            page_bounds(0, EPISODE_ITEMS_PER_PAGE + 1, EPISODE_ITEMS_PER_PAGE),
            (0, 6)
        );
        assert_eq!(
            page_bounds(1, EPISODE_ITEMS_PER_PAGE + 1, EPISODE_ITEMS_PER_PAGE),
            (6, 7)
        );
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
            let ReaderTaskPurpose::ForegroundSource { source, page: 0 } = entry.purpose else {
                panic!("expected foreground page-zero source");
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
        assert_eq!(runner.app().reader.as_ref().expect("reader").page, 1);
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
        assert_eq!(reader.page, 2);
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
        assert_eq!(reader.page, 2);
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
            for page in 1..=reader.limits.pages {
                reader.window.push_back(PageEntry::Building(
                    PageBuild::new(page, format, 1, 1).expect("source page build"),
                ));
            }
            reader.source_fetches.insert(1, source_task);
            app.reader_tasks.insert(
                source_task,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: match intent {
                        FetchIntent::Foreground { page } => {
                            ReaderTaskPurpose::ForegroundSource { source: 1, page }
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
            app.retry = Retry::Page(1);
            app.foreground_reader_task = Some(source_task);
            app.reader_tasks.insert(
                source_task,
                ReaderTaskEntry {
                    generation: 1,
                    purpose: ReaderTaskPurpose::ForegroundSource { source: 0, page: 1 },
                },
            );
            let reader = app.reader.as_mut().expect("reader");
            reader.images = vec![episode_image(0, 1, 3)];
            (reader.plans, reader.total_pages) =
                page_plan(&reader.images, 1, 1).expect("reused source plan");
            reader.window = VecDeque::from([PageEntry::Building(
                PageBuild::new(1, format, 1, 1).expect("page-one build"),
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
                assert_eq!(intent, FetchIntent::Foreground { page: 1 });
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
        assert_eq!(reader.page, 1);
        assert!(reader.source_cache.is_empty());
        assert_eq!(
            reader.refresh_attempted.get(&0),
            Some(&FetchIntent::Foreground { page: 1 })
        );
        assert_reader_bounds(runner.app());
    }

    fn active_reader_source(app: &Bomtoon, source: usize) -> Option<(TaskId, FetchIntent)> {
        app.reader_tasks.iter().find_map(|(task, entry)| {
            let intent = match entry.purpose {
                ReaderTaskPurpose::ForegroundSource {
                    source: task_source,
                    page,
                } if task_source == source => FetchIntent::Foreground { page },
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
            assert_eq!(reader.page, 0);
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
            assert_eq!(app.retry, Retry::Page(1));
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
            assert_eq!(app.retry, Retry::Page(1));
            assert_eq!(
                app.reader_tasks.get(&retry_task).map(|entry| entry.purpose),
                Some(ReaderTaskPurpose::ForegroundSource { source: 1, page: 1 })
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
            assert_eq!(runner.app().retry, Retry::Page(1));
            runner.action(action_id(RETRY));
            let reader = runner.app().reader.as_ref().expect("reader");
            assert_eq!(
                reader.refresh_attempted.get(&1),
                Some(&FetchIntent::Prefetch)
            );
            assert_eq!(
                active_reader_source(runner.app(), 1).map(|(_, intent)| intent),
                Some(FetchIntent::Foreground { page: 1 })
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
            assert_eq!(runner.app().retry, Retry::Page(1));
            assert_eq!(
                reader.refresh_waiters.get(&1),
                Some(&FetchIntent::Foreground { page: 1 })
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
                    assert_eq!(intent, FetchIntent::Foreground { page: 1 });
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
                    assert_eq!(intent, FetchIntent::Foreground { page: 0 });
                    task
                })
                .expect("page-zero reused source");
            let commands =
                runner.task_outcome(second_fetch, TaskOutcome::Failed(TaskError::Unauthorized));

            let app = runner.app();
            let reader = app.reader.as_ref().expect("reader");
            assert!(app.problem.is_some());
            assert_eq!(app.retry, Retry::Page(0));
            assert!(reader.refresh_task.is_none());
            assert_eq!(
                reader.refresh_attempted.get(&0),
                Some(&FetchIntent::Foreground { page: 1 })
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
                app.retry = Retry::Page(2);
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
                reader.window = VecDeque::from([PageEntry::Building(
                    PageBuild::new(2, format, 1, 1).expect("page-two build"),
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
                Some(&FetchIntent::Foreground { page: 1 })
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
                app.retry = Retry::Page(2);
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
                    .insert(2, FetchIntent::Foreground { page: 2 });
                reader
                    .refresh_attempted
                    .insert(2, FetchIntent::Foreground { page: 2 });
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
            assert_eq!(runner.app().retry, Retry::Page(1));
            assert!(!reader.refresh_waiters.contains_key(&2));
            assert_eq!(
                active_reader_source(runner.app(), 1).map(|(_, intent)| intent),
                Some(FetchIntent::Foreground { page: 1 })
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
            assert_eq!(reader.page, 1);
            assert!(app.problem.is_none());
            assert_ne!(
                active_reader_source(app, 2).map(|(_, intent)| intent),
                Some(FetchIntent::Foreground { page: 2 })
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
                app.retry = Retry::Page(2);
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
                    (2, FetchIntent::Foreground { page: 2 }),
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
                Some(FetchIntent::Foreground { page: 2 })
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
            app.retry = Retry::Page(2);
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
                    purpose: ReaderTaskPurpose::ForegroundSource { source: 3, page: 3 },
                },
            );
            let reader = app.reader.as_mut().expect("reader");
            reader.source_fetches = BTreeMap::from([(3, active_foreground)]);
            reader.refresh_task = Some(refresh);
            reader.refresh_waiters = BTreeMap::from([
                (1, FetchIntent::Prefetch),
                (2, FetchIntent::Foreground { page: 2 }),
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
            app.retry = Retry::Page(2);
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
                .insert(2, FetchIntent::Foreground { page: 2 });
            reader
                .refresh_attempted
                .insert(2, FetchIntent::Foreground { page: 2 });
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
            Some(&FetchIntent::Foreground { page: 2 })
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

        let (mut foreground, source_task) =
            reader_with_source_task(PictureFormat::Gray8, FetchIntent::Foreground { page: 1 });
        foreground.task_outcome(source_task, TaskOutcome::Failed(TaskError::Unauthorized));
        let refresh = foreground
            .app()
            .reader
            .as_ref()
            .and_then(|reader| reader.refresh_task)
            .expect("foreground refresh");
        let commands = foreground.task_outcome(refresh, TaskOutcome::Failed(TaskError::TimedOut));
        assert!(foreground.app().problem.is_some());
        assert_eq!(foreground.app().retry, Retry::Page(1));
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
    fn shelf(alias: &str, title: &str) -> model::ShelfComic {
        model::ShelfComic {
            alias: alias.to_owned(),
            title: title.to_owned(),
            cover_url: Some(format!(
                "https://image.balcony.studio/tw/contents/{alias}.webp"
            )),
        }
    }

    fn homepage_fixture() -> model::Homepage {
        model::Homepage {
            banners: ["shared", "banner_b", "banner_c", "banner_d"]
                .into_iter()
                .map(|alias| model::BannerComic {
                    alias: alias.to_owned(),
                })
                .collect(),
            newest: vec![shelf("shared", "Shared first"), shelf("new_b", "New B")],
            week_day: vec![
                shelf("new_b", "New B later"),
                shelf("weekday_a", "Weekday A"),
            ],
            only_bom: vec![
                shelf("shared", "Shared later"),
                shelf("only_a", "Only A"),
            ],
        }
    }

    fn aliases(comics: &[model::ShelfComic]) -> Vec<&str> {
        comics.iter().map(|comic| comic.alias.as_str()).collect()
    }

    fn feed_with_recommendations(count: usize) -> FeaturedState {
        FeaturedState {
            status: FeaturedStatus::Ready,
            generation: 1,
            featured: ["feature-a", "feature-b", "feature-c"]
                .into_iter()
                .map(|alias| shelf(alias, alias))
                .collect(),
            recommended: (0..count)
                .map(|index| shelf(&format!("rec-{index}"), &format!("Recommended {index}")))
                .collect(),
            pending_details: 0,
            page: 0,
            stale_warning: None,
            ..FeaturedState::default()
        }
    }

    fn homepage_response(banners: &[&str], recommendations: &[(&str, &str)]) -> Vec<u8> {
        let banners = banners
            .iter()
            .map(|alias| {
                format!(
                    "{{\"bannerDetailInfo\":[{{\"linkInfo\":{{\"target\":\"CONTENTS\",\"subTarget\":\"COMIC\",\"params\":\"{alias}\"}}}}]}}"
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let recommendations = recommendations
            .iter()
            .map(|(alias, title)| {
                format!(
                    "{{\"alias\":\"{alias}\",\"title\":\"{title}\",\"isAdult\":false,\"thumbnails\":[]}}"
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "<script id=\"__NEXT_DATA__\">{{\"props\":{{\"pageProps\":{{\"main\":{{\"banners\":[{banners}],\"newest\":[{recommendations}],\"weekDay\":[],\"onlyBom\":[]}}}}}}}}</script>"
        )
        .into_bytes()
    }

    fn local_day(day: u8) -> LocalDay {
        LocalDay::new(2026, 8, day).expect("valid local day")
    }

    fn ready_featured(day: Option<LocalDay>) -> Bomtoon {
        let mut featured = feed_with_recommendations(1);
        featured.refresh.loaded_day = day;
        Bomtoon {
            account: AccountState::SignedOut,
            view: View::Main,
            destination: MainDestination::Featured,
            featured,
            ..Bomtoon::default()
        }
    }

    fn ready_featured_runner(day: Option<LocalDay>) -> AppRunner<Bomtoon> {
        AppRunner::with_metrics(ready_featured(day), CLARA_BW_METRICS)
    }

    fn homepage_fetches(commands: &[Command]) -> usize {
        commands.iter().filter(|command| is_homepage_fetch(command)).count()
    }

    fn observe_local_day(
        runner: &mut AppRunner<Bomtoon>,
        observed: Option<LocalDay>,
    ) -> Vec<Command> {
        let requests = runner.resume();
        assert_eq!(
            requests
                .iter()
                .filter(|command| {
                    matches!(command, Command::Device(DeviceRequest::ReadLocalDay))
                })
                .count(),
            1
        );
        runner.device_result(DeviceResult::LocalDay(observed))
    }

    fn detail_response(alias: &str, title: &str) -> Vec<u8> {
        format!(
            r#"
                <meta property="og:title" content="{title} - 漫畫 - BOMTOON">
                <meta property="og:image" content="https://image.balcony.studio/tw/contents/{alias}.webp">
            "#
        )
        .into_bytes()
    }

    #[test]
    fn first_known_local_day_establishes_baseline_without_refresh() {
        let mut runner = ready_featured_runner(None);

        let commands = observe_local_day(&mut runner, Some(local_day(30)));

        assert_eq!(homepage_fetches(&commands), 0);
        assert_eq!(
            runner.app().featured.refresh.loaded_day,
            Some(local_day(30))
        );
        assert_eq!(runner.app().featured.refresh.active_day, None);
        assert_eq!(runner.app().featured.refresh.desired_day, None);
    }

    #[test]
    fn repeated_or_unknown_local_day_preserves_known_baseline() {
        let mut runner = ready_featured_runner(Some(local_day(30)));

        let unknown = observe_local_day(&mut runner, None);
        let repeated = observe_local_day(&mut runner, Some(local_day(30)));

        assert_eq!(homepage_fetches(&unknown), 0);
        assert_eq!(homepage_fetches(&repeated), 0);
        assert_eq!(
            runner.app().featured.refresh.loaded_day,
            Some(local_day(30))
        );
        assert_eq!(runner.app().featured.refresh.active_day, None);
        assert_eq!(runner.app().featured.refresh.desired_day, None);
    }

    #[test]
    fn later_different_local_day_refreshes_exactly_once() {
        let mut runner = ready_featured_runner(Some(local_day(30)));

        let first = observe_local_day(&mut runner, Some(local_day(31)));
        let repeated = observe_local_day(&mut runner, Some(local_day(31)));

        assert_eq!(homepage_fetches(&first), 1);
        assert_eq!(homepage_fetches(&repeated), 0);
        assert_eq!(
            runner.app().featured.refresh.active_day,
            Some(local_day(31))
        );
        assert_eq!(runner.app().featured.refresh.desired_day, None);
    }

    #[test]
    fn local_day_device_results_require_an_exact_pending_pair() {
        let mut app = ready_featured(Some(local_day(30)));
        let mut context = Context::default();
        app.request_local_day(&mut context);
        let _ = context.take_commands();
        let pending = app.featured.refresh.clone();

        app.on_device_result(
            &mut context,
            DeviceRequest::ReadBattery,
            DeviceResult::LocalDay(Some(local_day(31))),
        );
        assert_eq!(app.featured.refresh, pending);

        app.on_device_result(
            &mut context,
            DeviceRequest::ReadLocalDay,
            DeviceResult::Done,
        );
        assert_eq!(app.featured.refresh, pending);

        app.on_device_result(
            &mut context,
            DeviceRequest::ReadLocalDay,
            DeviceResult::LocalDay(Some(local_day(31))),
        );
        assert_eq!(app.featured.refresh.loaded_day, Some(local_day(30)));
        assert_eq!(app.featured.refresh.active_day, Some(local_day(31)));
        assert!(!app.featured.refresh.local_day_pending);

        let after_exact = app.featured.refresh.clone();
        app.on_device_result(
            &mut context,
            DeviceRequest::ReadLocalDay,
            DeviceResult::LocalDay(Some(local_day(30))),
        );
        assert_eq!(app.featured.refresh, after_exact);
    }

    #[test]
    fn overlapping_local_day_boundaries_emit_one_request() {
        let mut runner = AppRunner::new(Bomtoon::default());

        let started = runner.start();
        let resumed = runner.resume();
        let foregrounded = runner.lifecycle(kobo_sdk::Lifecycle::Foreground);
        let entered = runner.action(action_id(FEATURED));

        assert_eq!(
            [&started, &resumed, &foregrounded, &entered]
                .into_iter()
                .flat_map(|commands| commands.iter())
                .filter(|command| {
                    matches!(command, Command::Device(DeviceRequest::ReadLocalDay))
                })
                .count(),
            1
        );
        assert!(runner.app().featured.refresh.local_day_pending);
    }

    #[test]
    fn featured_entry_requests_local_day_after_prior_observation_settles() {
        let mut runner = ready_featured_runner(Some(local_day(30)));
        runner.app_mut().destination = MainDestination::Recent;

        let commands = runner.action(action_id(FEATURED));

        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::Device(DeviceRequest::ReadLocalDay))));
        assert!(runner.app().featured.refresh.local_day_pending);
    }

    #[test]
    fn newer_desired_day_chains_after_active_refresh_settles() {
        let mut runner = ready_featured_runner(Some(local_day(29)));
        let first = observe_local_day(&mut runner, Some(local_day(30)));
        let (first_homepage, _) = fetch_task_with(&first, "/comic/main");

        let newer = observe_local_day(&mut runner, Some(local_day(31)));
        assert_eq!(homepage_fetches(&newer), 0);
        assert_eq!(
            runner.app().featured.refresh.desired_day,
            Some(local_day(31))
        );

        let commands = runner.task_outcome(
            first_homepage,
            TaskOutcome::Completed(homepage_response(&[], &[("day-30", "Day 30")])),
        );

        assert_eq!(homepage_fetches(&commands), 1);
        assert_eq!(
            runner.app().featured.refresh.loaded_day,
            Some(local_day(30))
        );
        assert_eq!(
            runner.app().featured.refresh.active_day,
            Some(local_day(31))
        );
        assert_eq!(runner.app().featured.refresh.desired_day, None);
        assert_eq!(aliases(&runner.app().featured.recommended), ["day-30"]);
    }

    #[test]
    fn refresh_failure_chains_the_latest_desired_day() {
        let mut runner = ready_featured_runner(Some(local_day(29)));
        let first = observe_local_day(&mut runner, Some(local_day(30)));
        let (first_homepage, _) = fetch_task_with(&first, "/comic/main");
        let newer = observe_local_day(&mut runner, Some(local_day(31)));
        assert_eq!(homepage_fetches(&newer), 0);

        let commands =
            runner.task_outcome(first_homepage, TaskOutcome::Failed(TaskError::Offline));

        assert_eq!(homepage_fetches(&commands), 1);
        assert_eq!(
            runner.app().featured.refresh.loaded_day,
            Some(local_day(29))
        );
        assert_eq!(
            runner.app().featured.refresh.active_day,
            Some(local_day(31))
        );
        assert_eq!(runner.app().featured.refresh.desired_day, None);
        assert!(runner.app().featured.stale_warning.is_none());
    }

    #[test]
    fn refresh_success_is_atomic_and_reprioritizes_visible_covers() {
        let mut runner = ready_featured_runner(Some(local_day(30)));
        let cover_commands = runner.action(action_id(PREVIOUS_PAGE));
        let cover_tasks = spawns(&cover_commands)
            .into_iter()
            .filter_map(|(task, work)| match work {
                Task::Fetch { url, .. } => Some((url, task)),
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let shared_url = "https://image.balcony.studio/tw/contents/feature-a.webp";
        let obsolete_urls = [
            "https://image.balcony.studio/tw/contents/feature-c.webp",
            "https://image.balcony.studio/tw/contents/rec-0.webp",
        ];
        for url in [
            shared_url,
            "https://image.balcony.studio/tw/contents/feature-b.webp",
        ] {
            runner.task_outcome(
                *cover_tasks.get(url).expect("old cover task"),
                TaskOutcome::Completed(TINY_WEBP.to_vec()),
            );
        }
        let old_featured = runner.app().featured.featured.clone();
        let old_recommended = runner.app().featured.recommended.clone();
        let shared_picture = ready_cover(&runner.app().covers, Some(shared_url));

        let commands = observe_local_day(&mut runner, Some(local_day(31)));
        let (homepage, _) = fetch_task_with(&commands, "/comic/main");
        let commands = runner.task_outcome(
            homepage,
            TaskOutcome::Completed(homepage_response(&["feature-a", "fresh-b"], &[])),
        );
        let (shared_detail, _) = fetch_task_with(&commands, "/detail/feature-a");
        let (fresh_detail, _) = fetch_task_with(&commands, "/detail/fresh-b");

        assert_eq!(runner.app().featured.featured, old_featured);
        assert_eq!(runner.app().featured.recommended, old_recommended);
        assert_eq!(runner.app().featured.page, 0);

        runner.task_outcome(
            shared_detail,
            TaskOutcome::Completed(detail_response("feature-a", "Shared refreshed")),
        );
        assert_eq!(runner.app().featured.featured, old_featured);
        assert_eq!(runner.app().featured.recommended, old_recommended);
        runner.app_mut().featured.page = 7;

        let commands = runner.task_outcome(
            fresh_detail,
            TaskOutcome::Completed(detail_response("fresh-b", "Fresh B")),
        );

        assert_eq!(
            aliases(&runner.app().featured.featured),
            ["feature-a", "fresh-b"]
        );
        assert!(runner.app().featured.recommended.is_empty());
        assert_eq!(runner.app().featured.page, 0);
        assert_eq!(runner.app().featured.status, FeaturedStatus::Ready);
        assert_eq!(runner.app().featured.stale_warning, None);
        assert_eq!(
            runner.app().featured.refresh.loaded_day,
            Some(local_day(31))
        );
        assert_eq!(runner.app().featured.refresh.active_day, None);
        assert_eq!(runner.app().featured.refresh.desired_day, None);
        assert_eq!(
            ready_cover(&runner.app().covers, Some(shared_url)),
            shared_picture
        );
        let cancelled = commands
            .iter()
            .filter_map(|command| match command {
                Command::Cancel(task) => Some(*task),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert!(obsolete_urls.into_iter().all(|url| {
            cancelled.contains(cover_tasks.get(url).expect("obsolete cover task"))
        }));
        assert!(runner
            .app()
            .covers
            .visible_urls
            .iter()
            .any(|url| url.ends_with("/fresh-b.webp")));
    }

    #[test]
    fn refresh_failure_preserves_feed_pictures_baseline_and_has_one_retry() {
        let mut runner = ready_featured_runner(Some(local_day(30)));
        runner.app_mut().featured = {
            let mut featured = feed_with_recommendations(8);
            featured.page = 1;
            featured.refresh.loaded_day = Some(local_day(30));
            featured
        };
        let shared_url = "https://image.balcony.studio/tw/contents/feature-a.webp".to_owned();
        let picture = TilePicture {
            handle: PictureHandle(77),
            source: (1, 1),
        };
        runner
            .app_mut()
            .covers
            .entries
            .insert(shared_url.clone(), CoverState::Ready(picture));
        let old_featured = runner.app().featured.featured.clone();
        let old_recommended = runner.app().featured.recommended.clone();

        let cached_urls = runner
            .app()
            .featured
            .featured
            .iter()
            .chain(&runner.app().featured.recommended)
            .filter_map(|comic| comic.cover_url.clone())
            .collect::<Vec<_>>();
        for (index, url) in cached_urls.into_iter().enumerate() {
            runner.app_mut().covers.entries.entry(url).or_insert_with(|| {
                CoverState::Ready(TilePicture {
                    handle: PictureHandle(
                        100_u32.saturating_add(u32::try_from(index).expect("cover index")),
                    ),
                    source: (1, 1),
                })
            });
        }
        let commands = observe_local_day(&mut runner, Some(local_day(31)));
        let (homepage, _) = fetch_task_with(&commands, "/comic/main");
        let commands =
            runner.task_outcome(homepage, TaskOutcome::Failed(TaskError::Offline));
        let screen = last_screen(&commands);

        assert_eq!(runner.app().featured.featured, old_featured);
        assert_eq!(runner.app().featured.recommended, old_recommended);
        assert_eq!(runner.app().featured.page, 1);
        assert_eq!(
            runner.app().covers.entries.get(&shared_url),
            Some(&CoverState::Ready(picture))
        );
        assert_eq!(
            runner.app().featured.refresh.loaded_day,
            Some(local_day(30))
        );
        assert_eq!(runner.app().featured.refresh.active_day, None);
        assert_eq!(
            runner.app().featured.refresh.desired_day,
            Some(local_day(31))
        );
        assert!(runner.app().featured.stale_warning.is_some());
        assert_eq!(
            screen
                .nodes
                .iter()
                .filter(|node| {
                    matches!(node, Node::Button { action, .. } if *action == action_id(RETRY))
                })
                .count(),
            1
        );
        assert_fits(&screen);
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::DropPicture(PictureHandle(77)))));

        let retry = runner.action(action_id(RETRY));
        assert_eq!(homepage_fetches(&retry), 1);
        assert_eq!(
            runner.app().featured.refresh.active_day,
            Some(local_day(31))
        );
        assert_eq!(runner.app().featured.refresh.desired_day, None);
        assert_eq!(runner.app().featured.featured, old_featured);
        assert_eq!(runner.app().featured.recommended, old_recommended);
    }

    fn assert_non_ready_refresh_failure_is_terminal(status: FeaturedStatus) {
        let mut runner = ready_featured_runner(Some(local_day(30)));
        {
            let featured = &mut runner.app_mut().featured;
            featured.status = status;
            featured.featured.clear();
            featured.recommended.clear();
        }
        let commands = observe_local_day(&mut runner, Some(local_day(31)));
        let (homepage, _) = fetch_task_with(&commands, "/comic/main");

        let commands =
            runner.task_outcome(homepage, TaskOutcome::Failed(TaskError::Offline));
        let screen = last_screen(&commands);

        assert_eq!(runner.app().featured.status, FeaturedStatus::Failed);
        assert!(runner.app().featured.stale_warning.is_none());
        assert_eq!(
            runner.app().featured.refresh.loaded_day,
            Some(local_day(30))
        );
        assert_eq!(runner.app().featured.refresh.active_day, None);
        assert_eq!(
            runner.app().featured.refresh.desired_day,
            Some(local_day(31))
        );
        assert_eq!(
            screen
                .nodes
                .iter()
                .filter(|node| {
                    matches!(node, Node::Button { action, .. } if *action == action_id(RETRY))
                })
                .count(),
            1
        );

        let retry = runner.action(action_id(RETRY));
        let (homepage, _) = fetch_task_with(&retry, "/comic/main");
        assert_eq!(runner.app().featured.status, FeaturedStatus::Loading);
        assert_eq!(
            runner.app().featured.refresh.active_day,
            Some(local_day(31))
        );
        assert!(matches!(
            runner.app().shelf_tasks.get(&homepage),
            Some(ShelfTaskPurpose::Homepage {
                refresh_day: Some(day),
                ..
            }) if *day == local_day(31)
        ));
    }

    #[test]
    fn refresh_failure_from_loading_without_a_ready_feed_is_terminal() {
        assert_non_ready_refresh_failure_is_terminal(FeaturedStatus::Loading);
    }

    #[test]
    fn refresh_failure_from_failed_without_a_ready_feed_is_terminal() {
        assert_non_ready_refresh_failure_is_terminal(FeaturedStatus::Failed);
    }

    #[test]
    fn featured_problem_retry_precedes_stale_refresh_retry() {
        let mut app = ready_featured(Some(local_day(30)));
        app.featured.stale_warning = Some("Showing an older Featured feed.".to_owned());
        app.featured.refresh.desired_day = Some(local_day(31));
        app.problem = Some("The protected request failed.".to_owned());
        app.retry = Retry::Restart;
        let mut runner = AppRunner::new(app);

        let commands = runner.action(action_id(RETRY));
        let (homepage, _) = fetch_task_with(&commands, "/comic/main");

        assert!(runner.app().problem.is_none());
        assert!(runner.app().featured.stale_warning.is_none());
        assert_eq!(runner.app().featured.refresh.active_day, None);
        assert_eq!(runner.app().featured.refresh.desired_day, None);
        assert!(matches!(
            runner.app().shelf_tasks.get(&homepage),
            Some(ShelfTaskPurpose::Homepage {
                refresh_day: None,
                ..
            })
        ));
    }

    #[test]
    fn recommended_deduplicates_within_lists_but_not_against_featured() {
        let plan = plan_featured(homepage_fixture());

        assert_eq!(
            aliases(&plan.featured),
            ["shared", "banner_b", "banner_c"]
        );
        assert_eq!(
            aliases(&plan.recommended),
            ["shared", "new_b", "weekday_a", "only_a"]
        );
    }

    #[test]
    fn featured_uses_first_three_banners_and_reuses_first_list_metadata() {
        let plan = plan_featured(homepage_fixture());

        assert_eq!(plan.featured[0], shelf("shared", "Shared first"));
        assert_eq!(
            plan.details
                .iter()
                .map(|detail| (detail.slot, detail.alias.as_str()))
                .collect::<Vec<_>>(),
            [(1, "banner_b"), (2, "banner_c")]
        );
        assert_eq!(plan.featured[1].title, "banner_b");
        assert_eq!(plan.featured[1].cover_url, None);
    }

    #[test]
    fn featured_unresolved_selection_never_plans_more_than_three_details() {
        let plan = plan_featured(model::Homepage {
            banners: ["a", "b", "c", "d", "e"]
                .into_iter()
                .map(|alias| model::BannerComic {
                    alias: alias.to_owned(),
                })
                .collect(),
            newest: Vec::new(),
            week_day: Vec::new(),
            only_bom: Vec::new(),
        });

        assert_eq!(aliases(&plan.featured), ["a", "b", "c"]);
        assert_eq!(plan.details.len(), 3);
    }

    #[test]
    fn featured_page_one_has_tiles_then_measured_recommendations() {
        let feed = feed_with_recommendations(20);
        let page = featured_page(&feed, 0, 6);

        assert_eq!(page.featured, 0..3);
        assert_eq!(page.recommended, 0..6);
    }

    #[test]
    fn featured_page_continuations_do_not_repeat_tiles() {
        let feed = feed_with_recommendations(20);
        let page = featured_page(&feed, 1, 6);

        assert!(page.featured.is_empty());
        assert_eq!(page.recommended, 6..12);
        assert_eq!(featured_page_count(&feed, 6), 4);
    }

    #[test]
    fn featured_homepage_spawns_only_bounded_public_detail_tasks_with_runner_cap() {
        let (mut runner, commands) = started();
        let (homepage, _) = fetch_task_with(&commands, "/comic/main");
        let response = homepage_response(
            &["banner-a", "banner-b", "banner-c", "banner-d"],
            &[("recommended", "Recommended")],
        );

        let commands = runner.task_outcome(homepage, TaskOutcome::Completed(response));

        assert_eq!(runner.app().featured.pending_details, 3);
        assert_eq!(runner.app().featured.status, FeaturedStatus::Loading);
        assert_eq!(aliases(&runner.app().featured.recommended), ["recommended"]);
        assert_eq!(spawns(&commands).len(), 3);
        assert!(spawns(&commands).iter().all(
            |(_, work)| matches!(work, Task::Fetch { url, credential: None, .. } if url.contains("/detail/"))
        ));
        assert_eq!(runner.tasks_in_flight(), 4);
    }

    #[test]
    fn banner_detail_success_enriches_exact_slot_and_waits_for_every_detail() {
        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::SignedOut,
            ..Bomtoon::default()
        });
        let commands = runner.start();
        let (homepage, _) = fetch_task_with(&commands, "/comic/main");
        let commands = runner.task_outcome(
            homepage,
            TaskOutcome::Completed(homepage_response(&["first", "second"], &[])),
        );
        let (first, _) = fetch_task_with(&commands, "/detail/first");
        let (second, _) = fetch_task_with(&commands, "/detail/second");
        let detail = r#"
            <meta property="og:title" content="First title - 漫畫 - BOMTOON">
            <meta property="og:image" content="https://image.balcony.studio/tw/contents/first.webp">
        "#;

        runner.task_outcome(
            first,
            TaskOutcome::Completed(detail.as_bytes().to_vec()),
        );

        assert_eq!(runner.app().featured.status, FeaturedStatus::Loading);
        assert_eq!(runner.app().featured.pending_details, 1);
        assert_eq!(runner.app().featured.featured[0].title, "First title");
        assert_eq!(
            runner.app().featured.featured[0].cover_url.as_deref(),
            Some("https://image.balcony.studio/tw/contents/first.webp")
        );
        assert_eq!(runner.app().featured.featured[1].title, "second");

        runner.task_outcome(second, TaskOutcome::Failed(TaskError::TimedOut));
        assert_eq!(runner.app().featured.status, FeaturedStatus::Ready);
        assert_eq!(runner.app().featured.pending_details, 0);
    }

    #[test]
    fn banner_detail_failure_keeps_validated_alias_fallback_and_finishes_featured() {
        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::SignedOut,
            ..Bomtoon::default()
        });
        let commands = runner.start();
        let (homepage, _) = fetch_task_with(&commands, "/comic/main");
        let commands = runner.task_outcome(
            homepage,
            TaskOutcome::Completed(homepage_response(&["fallback"], &[])),
        );
        let (detail, _) = fetch_task_with(&commands, "/detail/fallback");

        let commands = runner.task_outcome(detail, TaskOutcome::Failed(TaskError::TimedOut));

        assert_eq!(runner.app().featured.status, FeaturedStatus::Ready);
        assert_eq!(
            runner.app().featured.featured,
            [model::ShelfComic {
                alias: "fallback".to_owned(),
                title: "fallback".to_owned(),
                cover_url: None,
            }]
        );
        assert!(spawns(&commands).is_empty());
        assert!(runner.app().problem.is_none());
    }

    #[test]
    fn featured_stale_generation_and_unknown_outcomes_are_no_ops() {
        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::SignedOut,
            ..Bomtoon::default()
        });
        let commands = runner.start();
        let (homepage, _) = fetch_task_with(&commands, "/comic/main");
        runner.app_mut().featured.generation =
            runner.app().featured.generation.wrapping_add(1);
        let before = runner.app().featured.clone();

        let commands = runner.task_outcome(
            homepage,
            TaskOutcome::Completed(homepage_response(&["stale"], &[])),
        );
        assert!(commands.is_empty());
        assert_eq!(runner.app().featured, before);

        let commands =
            runner.task_outcome(TaskId(9_999), TaskOutcome::Completed(Vec::new()));
        assert!(commands.is_empty());
        assert_eq!(runner.app().featured, before);
    }

    #[test]
    fn featured_initial_failure_is_local_retryable_and_keeps_navigation() {
        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::SignedOut,
            ..Bomtoon::default()
        });
        let commands = runner.start();
        let (homepage, _) = fetch_task_with(&commands, "/comic/main");
        let generation = runner.app().featured.generation;

        let commands = runner.task_outcome(homepage, TaskOutcome::Failed(TaskError::Offline));
        let screen = last_screen(&commands);

        assert_eq!(runner.app().view, View::Main);
        assert_eq!(runner.app().featured.status, FeaturedStatus::Failed);
        assert!(runner.app().problem.is_none());
        assert!(screen.top_bar.is_some());
        assert!(screen.nav_bar.is_some());
        assert!(screen.nodes.iter().any(
            |node| matches!(node, Node::Button { action, .. } if *action == action_id(RETRY))
        ));
        assert_fits(&screen);

        let commands = runner.action(action_id(RETRY));
        assert_eq!(runner.app().featured.status, FeaturedStatus::Loading);
        assert_eq!(runner.app().featured.generation, generation.wrapping_add(1));
        assert_eq!(spawns(&commands), [(runner.app().shelf_tasks.keys().copied().next().expect("retry task"), api::homepage())]);
    }

    #[test]
    fn featured_metadata_layout_measures_page_zero_and_uses_turns_without_buttons() {
        let featured = feed_with_recommendations(20);
        let measured = featured_page_zero_rows(&featured);
        assert!(measured > 0);
        assert!(measured <= 6);
        let page_count = featured_page_count(&featured, measured);
        let screen = Bomtoon {
            account: AccountState::SignedOut,
            view: View::Main,
            destination: MainDestination::Featured,
            featured,
            ..Bomtoon::default()
        }
        .screen();

        let tiles = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::TileGrid {
                    tiles,
                    shape: kobo_sdk::TileShape::Portrait,
                    ..
                } => Some(tiles),
                _ => None,
            })
            .expect("portrait Featured tiles");
        assert_eq!(tiles.len(), 3);
        assert!(tiles.iter().all(|tile| tile.picture.is_none()));
        let rows = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("Recommended rows");
        assert_eq!(rows.len(), measured);
        assert_eq!(
            screen.page_turns.expect("Featured page turns").position,
            Some((
                1,
                u16::try_from(page_count).expect("bounded test page count")
            ))
        );
        assert!(screen.nodes.iter().all(|node| {
            !matches!(
                node,
                Node::Button { action, .. }
                    if *action == action_id(PREVIOUS_PAGE) || *action == action_id(NEXT_PAGE)
            )
        }));
        assert_fits(&screen);
    }

    #[test]
    fn featured_continuation_has_six_recommendations_and_no_tiles() {
        let mut featured = feed_with_recommendations(20);
        let first_page_rows = featured_page_zero_rows(&featured);
        featured.page = 1;
        let screen = Bomtoon {
            account: AccountState::SignedOut,
            view: View::Main,
            destination: MainDestination::Featured,
            featured,
            ..Bomtoon::default()
        }
        .screen();

        assert!(!screen
            .nodes
            .iter()
            .any(|node| matches!(node, Node::TileGrid { .. })));
        let rows = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("Recommended rows");
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0].title, format!("Recommended {first_page_rows}"));
        assert_eq!(
            screen.page_turns.expect("Featured page turns").position,
            Some((
                2,
                u16::try_from(featured_page_count(
                    &feed_with_recommendations(20),
                    first_page_rows,
                ))
                .expect("bounded test page count"),
            ))
        );
        assert_fits(&screen);
    }
    #[test]
    fn featured_page_turn_actions_change_only_the_public_feed_position() {
        let mut runner = AppRunner::new(Bomtoon {
            account: AccountState::SignedOut,
            view: View::Main,
            destination: MainDestination::Featured,
            featured: feed_with_recommendations(20),
            page: 7,
            ..Bomtoon::default()
        });

        let commands = runner.action(action_id(NEXT_PAGE));

        assert_eq!(runner.app().featured.page, 1);
        assert_eq!(runner.app().page, 7);
        assert_eq!(
            last_screen(&commands)
                .page_turns
                .expect("Featured page turns")
                .position
                .expect("Featured page position")
                .0,
            2
        );

        runner.action(action_id(PREVIOUS_PAGE));
        assert_eq!(runner.app().featured.page, 0);
        assert_eq!(runner.app().page, 7);
    }

    #[test]
    fn featured_tiles_and_rows_use_the_existing_protected_content_route() {
        for (index, alias) in [(0, "feature-a"), (3, "rec-0")] {
            let mut runner = AppRunner::new(Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                destination: MainDestination::Featured,
                featured: feed_with_recommendations(2),
                ..Bomtoon::default()
            });

            let commands = runner.action(action_id(&format!("comic-{index}")));
            let (_, work) = only_spawn(&commands);

            assert_eq!(work, api::content(alias));
            assert_eq!(runner.app().pending, Some(Pending::Content(index)));
            assert_eq!(runner.app().selected_content_alias, alias);
        }
    }

    #[test]
    fn featured_restart_defers_and_coalesces_replacement_until_every_cancellation_settles() {
        let (mut runner, commands) = started();
        let (old_homepage, _) = fetch_task_with(&commands, "/comic/main");
        let (old_summary, _) = fetch_task_with(&commands, "/asset/user");
        let commands = runner.task_outcome(
            old_homepage,
            TaskOutcome::Completed(homepage_response(&["old-a", "old-b"], &[])),
        );
        let (old_a, _) = fetch_task_with(&commands, "/detail/old-a");
        let (old_b, _) = fetch_task_with(&commands, "/detail/old-b");
        runner.task_outcome(
            old_summary,
            TaskOutcome::Failed(TaskError::NoCredential),
        );
        runner.action(action_id(SIGN_IN));

        let commands = runner.action(action_id(RETRY));
        let cancelled = commands
            .iter()
            .filter_map(|command| match command {
                Command::Cancel(task) => Some(*task),
                _ => None,
            })
            .collect::<BTreeSet<_>>();

        assert_eq!(cancelled, BTreeSet::from([old_a, old_b]));
        assert!(!commands.iter().any(is_homepage_fetch));
        assert_eq!(
            runner.app().shelf_tasks.keys().copied().collect::<Vec<_>>(),
            [old_a, old_b]
        );
        let generation = runner.app().featured.generation;

        runner.app_mut().featured.status = FeaturedStatus::Failed;
        let repeated = runner.action(action_id(RETRY));
        assert!(!repeated
            .iter()
            .any(|command| matches!(command, Command::Cancel(_))));
        assert!(!repeated.iter().any(is_homepage_fetch));
        assert_eq!(runner.app().featured.generation, generation);

        let commands = runner.task_outcome(old_a, TaskOutcome::Cancelled);
        assert!(!commands.iter().any(is_homepage_fetch));
        assert_eq!(
            runner.app().shelf_tasks.keys().copied().collect::<Vec<_>>(),
            [old_b]
        );

        let commands = runner.task_outcome(old_b, TaskOutcome::Completed(Vec::new()));
        let (replacement_homepage, _) = fetch_task_with(&commands, "/comic/main");
        assert_eq!(runner.app().shelf_tasks.len(), 1);
        assert!(matches!(
            runner.app().shelf_tasks.get(&replacement_homepage),
            Some(ShelfTaskPurpose::Homepage {
                generation: replacement_generation,
                ..
            }) if *replacement_generation == generation
        ));

        let commands = runner.task_outcome(
            replacement_homepage,
            TaskOutcome::Completed(homepage_response(&["a", "b", "c", "d"], &[])),
        );

        assert_eq!(spawns(&commands).len(), 3);
        assert!(["/detail/a", "/detail/b", "/detail/c"]
            .into_iter()
            .all(|path| spawns(&commands).iter().any(
                |(_, work)| matches!(work, Task::Fetch { url, .. } if url.ends_with(path))
            )));
        assert_eq!(runner.app().featured.pending_details, 3);
        assert_eq!(runner.tasks_in_flight(), 4);
    }

    fn shelf_cover_url(index: usize) -> String {
        format!("https://image.balcony.studio/tw/co_thumbnail/cover-{index}.webp")
    }

    fn recent_shelf_entry(index: usize, cover_url: Option<String>) -> RecentEntry {
        RecentEntry {
            content_alias: format!("recent-{index}"),
            content_title: format!("Recent {index}"),
            cover_url,
            episode_alias: format!("episode-{index}"),
            episode_title: format!("Episode title {index}"),
        }
    }

    fn library_shelf_comic(index: usize, cover_url: Option<String>) -> Comic {
        Comic {
            alias: format!("library-{index}"),
            title: format!("Library {index}"),
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
                recent_loaded: true,
                total_recent_titles: count,
                ..Bomtoon::default()
            },
            CLARA_BW_METRICS,
        )
    }

    fn cover_fetches(commands: &[Command]) -> Vec<(TaskId, String)> {
        spawns(commands)
            .into_iter()
            .filter_map(|(task, work)| match work {
                Task::Fetch { url, .. }
                    if url.starts_with("https://image.balcony.studio/tw/") =>
                {
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

    #[test]
    fn coin_account_action_remains_reachable_on_both_compact_shelves() {
        for destination in [MainDestination::Recent, MainDestination::Library] {
            let app = Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                destination,
                recent: vec![recent_shelf_entry(0, None)],
                comics: vec![library_shelf_comic(0, None)],
                recent_loaded: true,
                library_loaded: true,
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
    fn compact_shelf_recent_uses_six_episode_summary_rows_and_ready_picture() {
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
                && row.summary == format!("Episode title {index}")
                && row.lead == kobo_sdk::RowLead::Icon(Glyph::Book)
        }));
        assert!(first_screen.nav_bar.is_some());
        assert_fits(&first_screen);

        let (cover_task, _) = cover_fetches(&commands)
            .into_iter()
            .next()
            .expect("first visible cover fetch");
        let commands =
            runner.task_outcome(cover_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
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
        assert_eq!(ready_rows[0].summary, "Episode title 0");
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
    fn compact_shelf_library_uses_six_owned_total_rows_and_fixed_nav() {
        let mut runner = AppRunner::with_metrics(
            Bomtoon {
                account: AccountState::Active,
                view: View::Main,
                destination: MainDestination::Featured,
                comics: (0..6)
                    .map(|index| library_shelf_comic(index, None))
                    .collect(),
                library_loaded: true,
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
                && row.summary == format!("{} / {}", index + 1, index + 7)
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
                recent_loaded: true,
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
    fn cover_cached_picture_is_reused_across_featured_and_recommended() {
        let shared = shelf("shared-cover", "Shared cover");
        let mut runner = AppRunner::with_metrics(
            Bomtoon {
                account: AccountState::SignedOut,
                view: View::Main,
                destination: MainDestination::Recent,
                featured: FeaturedState {
                    status: FeaturedStatus::Ready,
                    generation: 1,
                    featured: vec![shared.clone()],
                    recommended: vec![shared],
                    pending_details: 0,
                    page: 0,
                    stale_warning: None,
                    ..FeaturedState::default()
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
        let tile_picture = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::TileGrid { tiles, .. } => tiles.first().and_then(|tile| tile.picture),
                _ => None,
            })
            .expect("Featured tile picture");
        let row_picture = screen
            .nodes
            .iter()
            .find_map(|node| match node {
                Node::Rows { rows, .. } => rows.first().and_then(|row| match row.lead {
                    kobo_sdk::RowLead::Picture(picture, Glyph::Book) => Some(picture),
                    _ => None,
                }),
                _ => None,
            })
            .expect("Recommended row picture");
        assert_eq!(tile_picture, row_picture);
    }

    #[test]
    fn cover_spawn_resumes_in_visible_order_after_capacity_release() {
        let mut runner = recent_cover_runner(6);
        let commands = runner.action(action_id(RECENT));
        let initial = cover_fetches(&commands);
        assert_eq!(initial.len(), 4);
        assert_eq!(runner.tasks_in_flight(), 4);

        let commands =
            runner.task_outcome(initial[0].0, TaskOutcome::Failed(TaskError::TimedOut));
        let resumed = cover_fetches(&commands);
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].1, shelf_cover_url(4));
        assert_eq!(runner.tasks_in_flight(), 4);
    }

    #[test]
    fn cover_scheduler_shares_the_runner_four_task_cap() {
        let (mut runner, _) = started();
        runner.app_mut().featured = feed_with_recommendations(6);

        let commands = runner.action(action_id(FEATURED));
        let covers = cover_fetches(&commands);
        assert_eq!(covers.len(), 2);
        assert_eq!(runner.tasks_in_flight(), 4);

        let commands =
            runner.task_outcome(covers[0].0, TaskOutcome::Failed(TaskError::TimedOut));
        assert_eq!(cover_fetches(&commands).len(), 1);
        assert_eq!(runner.tasks_in_flight(), 4);
    }

}
