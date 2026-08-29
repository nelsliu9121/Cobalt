mod api;
mod model;
mod parse;

use kobo_image::{Picture, PictureFormat, PicturePixels, PicturePixelsRef, PANEL_GREYS};
use kobo_sdk::{
    action_id, ActionId, BannerLevel, Context, Failure, KoboApp, PictureHandle, ReadingChrome,
    Screen, ScreenBuilder, TaskError, TaskId, TaskOutcome, TilePicture,
};
use model::{display_text, Comic, Episode, EpisodeImage, RecentEntry};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::process::ExitCode;

const TITLE: &str = "BOMTOON";
const RETRY: &str = "retry";
const SIGN_OUT: &str = "sign-out";
const PREVIOUS: &str = "previous";
const NEXT: &str = "next";
const LIBRARY_SHELF: &str = "library-shelf";
const RECENT_SHELF: &str = "recent-shelf";
const LIBRARY_ITEMS_PER_PAGE: usize = 3;
const EPISODE_ITEMS_PER_PAGE: usize = 6;
const READER_PREVIOUS: &str = "reader-previous";
const READER_NEXT: &str = "reader-next";
const READER_CHROME: &str = "reader-chrome";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum View {
    #[default]
    Status,
    Library,
    Episodes,
    Reader,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Shelf {
    #[default]
    Library,
    Recent,
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
enum ReaderTaskPurpose {
    Manifest,
    ForegroundSource { source: usize, page: usize },
    PrefetchSource { source: usize },
    Maintenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReaderTaskEntry {
    generation: u64,
    purpose: ReaderTaskPurpose,
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
    picture: Option<TilePicture>,
    chrome_visible: bool,
}

#[derive(Default)]
struct Bomtoon {
    account: AccountState,
    view: View,
    pending: Option<Pending>,
    task: Option<TaskId>,
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
    library_view_page: usize,
    recent_view_page: usize,
    next_library_page: Option<usize>,
    next_recent_page: Option<usize>,
    total_library_titles: usize,
    total_recent_titles: usize,
    library_loaded: bool,
    recent_loaded: bool,
    shelf: Shelf,
    problem: Option<String>,
}

impl Bomtoon {
    fn show(&self, context: &mut Context) {
        let owns_back = self.account == AccountState::Active
            && match self.view {
                View::Episodes => self.pending.is_none() && self.problem.is_none(),
                View::Reader => true,
                View::Status | View::Library => false,
            };
        context.set_screen(self.screen().with_own_back(owns_back));
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
                ReaderTaskPurpose::Manifest => "Loading comic pages",
                ReaderTaskPurpose::ForegroundSource { .. } => "Loading comic image",
                ReaderTaskPurpose::PrefetchSource { .. } | ReaderTaskPurpose::Maintenance => {
                    "Loading comic pages"
                }
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
        if self.account != AccountState::Active {
            return self.signed_out_screen();
        }
        match self.view {
            View::Status => ScreenBuilder::new("bomtoon-status")
                .top_bar(TITLE)
                .text("No request has started.")
                .primary_button(RETRY, "Connect")
                .build(),
            View::Library => self.library_screen(),
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

    fn library_screen(&self) -> Screen {
        let heading = if self.shelf == Shelf::Recent {
            "Recent Reading"
        } else {
            "BOMTOON Library"
        };
        let count = self.shelf_len();
        let mut screen = ScreenBuilder::new("bomtoon-library")
            .top_bar(heading)
            .text(format!(
                "{} of {} titles loaded.",
                count,
                self.shelf_total()
            ))
            .button(
                LIBRARY_SHELF,
                if self.shelf == Shelf::Library {
                    "Library (selected)"
                } else {
                    "Library"
                },
            )
            .button(
                RECENT_SHELF,
                if self.shelf == Shelf::Recent {
                    "Recent (selected)"
                } else {
                    "Recent"
                },
            );
        let (start, end) = page_bounds(self.page, count, LIBRARY_ITEMS_PER_PAGE);
        for index in start..end {
            let label = if self.shelf == Shelf::Recent {
                let recent = &self.recent[index];
                let fallback = format!("Title {}", recent.content_alias);
                let episode_fallback = format!("Episode {}", recent.episode_alias);
                format!(
                    "{} - {}",
                    display_text(&recent.content_title, &fallback),
                    display_text(&recent.episode_title, &episode_fallback)
                )
            } else {
                let comic = &self.comics[index];
                let fallback = format!("Title {}", comic.alias);
                format!(
                    "{}  ({}/{})",
                    display_text(&comic.title, &fallback),
                    comic.owned_episodes,
                    comic.total_episodes
                )
            };
            screen = screen.button(format!("comic-{index}"), label);
        }
        if self.page > 0 {
            screen = screen.button(PREVIOUS, "Previous page");
        }
        if library_has_next_page(self.page, count, self.shelf_next_page().is_some()) {
            screen = screen.button(NEXT, "Next page");
        }
        screen.button(SIGN_OUT, "Sign out").build()
    }

    fn shelf_len(&self) -> usize {
        match self.shelf {
            Shelf::Library => self.comics.len(),
            Shelf::Recent => self.recent.len(),
        }
    }

    fn shelf_total(&self) -> usize {
        match self.shelf {
            Shelf::Library => self.total_library_titles,
            Shelf::Recent => self.total_recent_titles,
        }
    }

    fn shelf_next_page(&self) -> Option<usize> {
        match self.shelf {
            Shelf::Library => self.next_library_page,
            Shelf::Recent => self.next_recent_page,
        }
    }

    fn shelf_is_loaded(&self, target: Shelf) -> bool {
        match target {
            Shelf::Library => self.library_loaded,
            Shelf::Recent => self.recent_loaded,
        }
    }

    fn remember_shelf_page(&mut self) {
        match self.shelf {
            Shelf::Library => self.library_view_page = self.page,
            Shelf::Recent => self.recent_view_page = self.page,
        }
    }

    fn switch_shelf(&mut self, context: &mut Context, target: Shelf) {
        if self.shelf == target {
            return;
        }
        self.remember_shelf_page();
        self.shelf = target;
        self.page = match target {
            Shelf::Library => self.library_view_page,
            Shelf::Recent => self.recent_view_page,
        };
        self.problem = None;
        if !self.shelf_is_loaded(target) {
            let (pending, work) = match target {
                Shelf::Library => (Pending::Library(0), api::library(0)),
                Shelf::Recent => (Pending::Recent(0), api::recent(0)),
            };
            self.spawn(context, pending, work);
        }
    }

    fn episode_screen(&self) -> Screen {
        let mut screen = ScreenBuilder::new("bomtoon-episodes")
            .top_bar(self.selected_title.clone())
            .text(format!("{} episodes", self.episodes.len()));
        let (start, end) = page_bounds(self.page, self.episodes.len(), EPISODE_ITEMS_PER_PAGE);
        for (index, episode) in self.episodes[start..end].iter().enumerate() {
            let index = start + index;
            let title_fallback = format!("Episode {}", episode.alias);
            let status = display_text(episode.purchase.label(), "Other status");
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
        page_controls(screen, self.page, self.episodes.len())
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

    fn invalidate_reader_tasks(&mut self, context: &mut Context) {
        self.reader_generation = self.reader_generation.wrapping_add(1);
        for task in std::mem::take(&mut self.reader_tasks).into_keys() {
            context.cancel(task);
        }
        self.foreground_reader_task = None;
        if let Some(reader) = self.reader.as_mut() {
            reader.generation = self.reader_generation;
            reader.source_fetches.clear();
            reader.maintenance_task = None;
        }
    }

    fn clear_account_data(&mut self, context: &mut Context) {
        self.invalidate_reader_tasks(context);
        let picture = self
            .reader
            .take()
            .and_then(|reader| reader.picture)
            .map(|picture| picture.handle);
        self.comics.clear();
        self.recent.clear();
        self.episodes.clear();
        self.selected_content_alias.clear();
        self.selected_title.clear();
        self.reader_selection = None;
        self.retry = Retry::Restart;
        self.page = 0;
        self.library_view_page = 0;
        self.recent_view_page = 0;
        self.next_library_page = None;
        self.next_recent_page = None;
        self.total_library_titles = 0;
        self.total_recent_titles = 0;
        self.library_loaded = false;
        self.recent_loaded = false;
        if let Some(handle) = picture {
            context.drop_picture(handle);
        }
    }

    fn restart(&mut self, context: &mut Context) {
        self.problem = None;
        self.account = AccountState::Active;
        self.clear_account_data(context);
        self.shelf = Shelf::Library;
        self.view = View::Status;
        self.spawn(context, Pending::Library(0), api::library(0));
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
        let task_limit = self
            .reader
            .as_ref()
            .map_or(1, |reader| reader.limits.tasks);
        if self.reader_tasks.len() >= task_limit
            || (foreground && self.foreground_reader_task.is_some())
        {
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
        self.invalidate_reader_tasks(context);
        let old = self
            .reader
            .take()
            .and_then(|reader| reader.picture)
            .map(|picture| picture.handle);
        if let Some(handle) = old {
            context.drop_picture(handle);
        }
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
            self.fail_reader(
                Retry::Manifest,
                "Another reader request is still active.",
            );
        }
    }

    fn start_reader_source(
        &mut self,
        context: &mut Context,
        source: usize,
        purpose: ReaderTaskPurpose,
        foreground: bool,
    ) -> Option<TaskId> {
        let url = self
            .reader
            .as_ref()?
            .images
            .get(source)?
            .url
            .clone();
        let task = self.spawn_reader(context, purpose, api::image(&url), foreground)?;
        let reader = self.reader.as_mut()?;
        reader.source_fetches.insert(source, task);
        Some(task)
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
                let first_build =
                    match PageBuild::new(0, format, panel_width, panel_height) {
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
        let available_tasks = self
            .reader
            .as_ref()
            .map_or(0, |reader| reader.limits.tasks)
            .saturating_sub(self.reader_tasks.len());
        let mut planned_spawns = Vec::new();
        let mut planned_promotion = None;
        let mut ready_to_install = None;
        {
            let reader = self
                .reader
                .as_mut()
                .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
            if extend_window {
                while reader.window.len() < reader.limits.pages {
                    let next_page = reader
                        .window
                        .back()
                        .map(entry_page)
                        .map_or_else(|| reader.page.saturating_add(1), |page| page.saturating_add(1));
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
            }

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
                let picture = finish_build(
                    build,
                    plan,
                    reader.panel_width,
                    reader.panel_height,
                )?;
                reader
                    .window
                    .insert(index, PageEntry::Ready { page, picture });
                index += 1;
            }

            {
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
                            source_relevant_to_following_page_window(
                                *source,
                                plans,
                                target,
                                limits.pages,
                            )
                        })
                });
            }

            if let Some(target) = install_target {
                let installable = matches!(
                    reader.window.front(),
                    Some(PageEntry::Ready { page, .. }) if *page == target
                );
                if installable {
                    let Some(PageEntry::Ready { page, picture }) = reader.window.pop_front() else {
                        return Err("The ready comic page changed unexpectedly.".to_owned());
                    };
                    ready_to_install = Some((page, picture));
                }
            }

            if ready_to_install.is_none() {
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
                    let missing = reader.window.iter().find_map(|entry| match entry {
                        PageEntry::Building(build) if build.page == page => reader
                            .plans
                            .get(build.page)
                            .and_then(|plan| plan.segments.get(build.next_segment))
                            .map(|segment| segment.source),
                        PageEntry::Building(_) | PageEntry::Ready { .. } => None,
                    });
                    if let Some(source) = missing {
                        if let Some(&task) = reader.source_fetches.get(&source) {
                            planned_promotion = Some((task, source, page));
                        } else if !reader.source_cache.contains_key(&source) && spawn_limit > 0 {
                            let url = reader
                                .images
                                .get(source)
                                .ok_or_else(|| {
                                    "The selected comic image is no longer available.".to_owned()
                                })?
                                .url
                                .clone();
                            planned_spawns.push((
                                source,
                                ReaderTaskPurpose::ForegroundSource { source, page },
                                true,
                                url,
                            ));
                        }
                    }
                } else if spawn_limit > 0 {
                    'pages: for entry in &reader.window {
                        let PageEntry::Building(build) = entry else {
                            continue;
                        };
                        let plan = reader
                            .plans
                            .get(build.page)
                            .ok_or_else(|| "The comic page build has no plan.".to_owned())?;
                        for segment in &plan.segments[build.next_segment..] {
                            let source = segment.source;
                            if reader.source_cache.contains_key(&source)
                                || reader.source_fetches.contains_key(&source)
                                || planned_spawns
                                    .iter()
                                    .any(|(planned, _, _, _)| *planned == source)
                            {
                                continue;
                            }
                            let url = reader
                                .images
                                .get(source)
                                .ok_or_else(|| {
                                    "The selected comic image is no longer available.".to_owned()
                                })?
                                .url
                                .clone();
                            planned_spawns.push((
                                source,
                                ReaderTaskPurpose::PrefetchSource { source },
                                false,
                                url,
                            ));
                            if planned_spawns.len() == spawn_limit {
                                break 'pages;
                            }
                        }
                    }
                }
            }
        }
        if let Some((task, source, page)) = planned_promotion {
            let entry = self
                .reader_tasks
                .get_mut(&task)
                .ok_or_else(|| "The comic image request registry changed unexpectedly.".to_owned())?;
            if entry.generation != self.reader_generation {
                return Err("The comic image request generation changed unexpectedly.".to_owned());
            }
            entry.purpose = ReaderTaskPurpose::ForegroundSource { source, page };
            self.foreground_reader_task = Some(task);
        }

        if let Some((page, picture)) = ready_to_install {
            self.install_page(context, page, picture)?;
            return Ok(true);
        }
        for (source, purpose, foreground, url) in planned_spawns {
            let Some(task) =
                self.spawn_reader(context, purpose, api::image(&url), foreground)
            else {
                return Err("The comic image request could not be started.".to_owned());
            };
            let reader = self
                .reader
                .as_mut()
                .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
            reader.source_fetches.insert(source, task);
        }
        Ok(false)
    }

    fn accept_reader_source(
        &mut self,
        context: &mut Context,
        task: TaskId,
        source: usize,
        install_target: Option<usize>,
        bytes: &[u8],
    ) -> bool {
        let desired_page = match self.retry {
            Retry::Page(page) => Some(page),
            Retry::Restart | Retry::Manifest => None,
        };
        let page = install_target.or(desired_page).unwrap_or_else(|| {
            self.reader.as_ref().map_or(0, |reader| {
                reader.page.checked_add(1).unwrap_or(reader.page)
            })
        });
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
                source_relevant_to_page_window(
                    source,
                    &reader.plans,
                    target,
                    reader.limits.pages,
                )
            });
            (
                relevant_to_window || relevant_to_desired,
                install_target.is_none() && (foreground_active || !relevant_to_window),
            )
        };
        if !relevant {
            return false;
        }
        let decoded = {
            let Some(reader) = self.reader.as_ref() else {
                return false;
            };
            let Some(expected) = reader.images.get(source) else {
                self.fail_reader(
                    Retry::Page(page),
                    "The selected comic image is no longer available.",
                );
                return false;
            };
            decode_reader_source(bytes, expected, reader.format, reader.panel_width)
        };
        let source_picture = match decoded {
            Ok(picture) => picture,
            Err(error) => {
                self.fail_reader(Retry::Page(page), error);
                return false;
            }
        };
        let Some(reader) = self.reader.as_mut() else {
            return false;
        };
        if reader
            .source_cache
            .len()
            .saturating_add(reader.source_fetches.len())
            >= reader.limits.source_slots
        {
            self.fail_reader(
                Retry::Page(page),
                "The comic source window exceeded its format limit.",
            );
            return false;
        }
        reader.source_cache.insert(source, source_picture);
        if defer_maintenance {
            return false;
        }
        match self.maintain_reader(
            context,
            install_target.is_none(),
            install_target,
        ) {
            Ok(shown) => shown,
            Err(error) => {
                self.fail_reader(Retry::Page(page), error);
                false
            }
        }
    }

    fn accept(&mut self, context: &mut Context, pending: Pending, bytes: &[u8]) -> bool {
        match pending {
            Pending::Logout => {
                if bytes.is_empty() {
                    self.clear_account_data(context);
                    self.account = AccountState::SignedOut;
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
                    self.library_view_page = self.page;
                    self.shelf = Shelf::Library;
                    self.view = View::Library;
                    if should_load_recent(page.number, page.total_items) {
                        self.switch_shelf(context, Shelf::Recent);
                    }
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
                    self.recent_view_page = self.page;
                    self.shelf = Shelf::Recent;
                    self.view = View::Library;
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
        let selected = match self.shelf {
            Shelf::Library => self
                .comics
                .get(index)
                .map(|comic| (comic.alias.clone(), comic.title.clone())),
            Shelf::Recent => self
                .recent
                .get(index)
                .map(|recent| (recent.content_alias.clone(), recent.content_title.clone())),
        };
        let Some((alias, title)) = selected else {
            return;
        };
        self.remember_shelf_page();
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
        self.invalidate_reader_tasks(context);
        let old = self
            .reader
            .take()
            .and_then(|reader| reader.picture)
            .map(|picture| picture.handle);
        self.reader_selection = None;
        self.problem = None;
        self.retry = Retry::Restart;
        self.view = View::Episodes;
        self.show(context);
        if let Some(handle) = old {
            context.drop_picture(handle);
        }
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

    fn rebase_window(&mut self, context: &mut Context, page: usize) {
        let prepared = (|| {
            let reader = self
                .reader
                .as_ref()
                .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
            let plan = reader
                .plans
                .get(page)
                .ok_or_else(|| "The selected comic page is no longer available.".to_owned())?;
            plan.segments
                .first()
                .ok_or_else(|| "The selected comic page is empty.".to_owned())?;
            let build =
                PageBuild::new(page, reader.format, reader.panel_width, reader.panel_height)?;
            let required_source = plan.segments[0].source;
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
            let active_required = reader
                .source_fetches
                .get(&required_source)
                .is_some_and(|task| is_active_source(required_source, *task));
            let fetch_capacity = reader.limits.fetches.min(reader.limits.source_slots);
            let keep_capacity = if active_required {
                fetch_capacity
            } else {
                fetch_capacity.saturating_sub(1)
            };
            let kept_fetches = reader
                .source_fetches
                .iter()
                .filter(|(source, task)| {
                    source_relevant_to_page_window(
                        **source,
                        &reader.plans,
                        page,
                        reader.limits.pages,
                    ) && is_active_source(**source, **task)
                })
                .take(keep_capacity)
                .map(|(&source, &task)| (source, task))
                .collect::<BTreeMap<_, _>>();
            Ok::<_, String>((build, kept_fetches))
        })();
        let (build, kept_fetches) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_reader(Retry::Page(page), error);
                return;
            }
        };

        let kept_tasks = kept_fetches.values().copied().collect::<BTreeSet<_>>();
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
        reader.source_cache.clear();
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
            Retry::Page(page) => self.rebase_window(context, page),
        }
        false
    }

    fn fail_task(&mut self, context: &mut Context, pending: Pending, error: TaskError) {
        match (pending, error) {
            (Pending::Logout, TaskError::RevocationUnconfirmed) => {
                self.clear_account_data(context);
                self.account = AccountState::RevocationUnconfirmed;
                self.problem = None;
            }
            (Pending::Logout, TaskError::LocalStorage) => {
                self.problem = Some("Could not remove the local BOMTOON sign-in data.".to_owned());
            }
            (
                Pending::Library(_) | Pending::Recent(_) | Pending::Content(_),
                TaskError::NoCredential,
            ) => {
                self.clear_account_data(context);
                self.account = AccountState::SignedOut;
                self.problem = None;
            }
            (
                Pending::Library(_) | Pending::Recent(_) | Pending::Content(_),
                TaskError::Unauthorized,
            ) => {
                self.clear_account_data(context);
                self.account = AccountState::Expired;
                self.problem = None;
            }
            (_, error) => {
                self.problem = Some(Failure::of(error).advice.to_owned());
                self.retry = Retry::Restart;
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

    fn cancel_task(&mut self, _pending: Pending) {
        self.problem = Some("The request was cancelled.".to_owned());
        self.retry = Retry::Restart;
    }
}

impl KoboApp for Bomtoon {
    fn on_start(&mut self, context: &mut Context) {
        self.restart(context);
        self.show(context);
    }

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
        if action == ActionId::BACK && ready && self.view == View::Episodes {
            self.view = View::Library;
            self.page = match self.shelf {
                Shelf::Library => self.library_view_page,
                Shelf::Recent => self.recent_view_page,
            };
            self.show(context);
            return;
        }
        let retry_visible = self.problem.is_some()
            || self.account != AccountState::Active
            || self.view == View::Status;
        if action == action_id(RETRY) && self.pending.is_none() && retry_visible {
            if !self.retry(context) {
                self.show(context);
            }
            return;
        }
        if !ready {
            self.show(context);
            return;
        }
        if self.view == View::Reader {
            self.handle_reader_action(context, action);
            return;
        }
        if self.view == View::Library && action == action_id(SIGN_OUT) {
            self.spawn(context, Pending::Logout, api::logout());
        } else if self.view == View::Library && action == action_id(LIBRARY_SHELF) {
            self.switch_shelf(context, Shelf::Library);
        } else if self.view == View::Library && action == action_id(RECENT_SHELF) {
            self.switch_shelf(context, Shelf::Recent);
        } else if action == action_id(PREVIOUS) {
            self.page = self.page.saturating_sub(1);
        } else if action == action_id(NEXT) {
            let items_per_page = if self.view == View::Library {
                LIBRARY_ITEMS_PER_PAGE
            } else {
                EPISODE_ITEMS_PER_PAGE
            };
            let next_start = self.page.saturating_add(1).saturating_mul(items_per_page);
            if self.view != View::Library || next_start < self.shelf_len() {
                self.page = self.page.saturating_add(1);
            } else if let Some(next) = self.shelf_next_page() {
                let pending = if self.shelf == Shelf::Recent {
                    Pending::Recent(next)
                } else {
                    Pending::Library(next)
                };
                let work = if self.shelf == Shelf::Recent {
                    api::recent(next)
                } else {
                    api::library(next)
                };
                self.spawn(context, pending, work);
            }
        } else if self.view == View::Library {
            for index in 0..self.shelf_len() {
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
        if let Some(entry) = self.reader_tasks.remove(&task) {
            if self.foreground_reader_task == Some(task) {
                self.foreground_reader_task = None;
            }
            if entry.generation != self.reader_generation {
                return;
            }
            if !matches!(entry.purpose, ReaderTaskPurpose::Manifest)
                && !self
                    .reader
                    .as_ref()
                    .is_some_and(|reader| reader.generation == entry.generation)
            {
                return;
            }
            let shown = match (entry.purpose, outcome) {
                (ReaderTaskPurpose::Manifest, TaskOutcome::Completed(bytes)) => {
                    self.accept_manifest(context, &bytes);
                    false
                }
                (
                    ReaderTaskPurpose::ForegroundSource { source, page },
                    TaskOutcome::Completed(bytes),
                ) => self.accept_reader_source(context, task, source, Some(page), &bytes),
                (
                    ReaderTaskPurpose::PrefetchSource { source },
                    TaskOutcome::Completed(bytes),
                ) => self.accept_reader_source(context, task, source, None, &bytes),
                (ReaderTaskPurpose::Maintenance, TaskOutcome::Completed(_)) => {
                    if let Some(reader) = self.reader.as_mut() {
                        if reader.maintenance_task == Some(task) {
                            reader.maintenance_task = None;
                        }
                    }
                    if let Err(error) = self.maintain_reader(context, true, None) {
                        let page = self.reader.as_ref().map_or(0, |reader| reader.page);
                        self.fail_reader(Retry::Page(page), error);
                    }
                    false
                }
                (ReaderTaskPurpose::Manifest, TaskOutcome::Failed(error)) => {
                    match error {
                        TaskError::NoCredential => {
                            self.clear_account_data(context);
                            self.account = AccountState::SignedOut;
                            self.problem = None;
                        }
                        TaskError::Unauthorized => {
                            self.clear_account_data(context);
                            self.account = AccountState::Expired;
                            self.problem = None;
                        }
                        error => self.fail_reader(Retry::Manifest, Failure::of(error).advice),
                    }
                    false
                }
                (
                    ReaderTaskPurpose::ForegroundSource { source, page },
                    TaskOutcome::Failed(error),
                ) => {
                    if let Some(reader) = self.reader.as_mut() {
                        if reader.source_fetches.get(&source) == Some(&task) {
                            reader.source_fetches.remove(&source);
                        }
                    }
                    self.fail_reader(Retry::Page(page), Failure::of(error).advice);
                    false
                }
                (
                    ReaderTaskPurpose::PrefetchSource { source },
                    TaskOutcome::Failed(error),
                ) => {
                    let page = self.reader.as_ref().map_or(0, |reader| reader.page);
                    if let Some(reader) = self.reader.as_mut() {
                        if reader.source_fetches.get(&source) == Some(&task) {
                            reader.source_fetches.remove(&source);
                        }
                    }
                    self.fail_reader(Retry::Page(page), Failure::of(error).advice);
                    false
                }
                (ReaderTaskPurpose::Maintenance, TaskOutcome::Failed(error)) => {
                    let page = self.reader.as_ref().map_or(0, |reader| reader.page);
                    if let Some(reader) = self.reader.as_mut() {
                        if reader.maintenance_task == Some(task) {
                            reader.maintenance_task = None;
                        }
                    }
                    self.fail_reader(Retry::Page(page), Failure::of(error).advice);
                    false
                }
                (ReaderTaskPurpose::Manifest, TaskOutcome::Cancelled) => {
                    self.fail_reader(Retry::Manifest, "The request was cancelled.");
                    false
                }
                (
                    ReaderTaskPurpose::ForegroundSource { source, page },
                    TaskOutcome::Cancelled,
                ) => {
                    if let Some(reader) = self.reader.as_mut() {
                        if reader.source_fetches.get(&source) == Some(&task) {
                            reader.source_fetches.remove(&source);
                        }
                    }
                    self.fail_reader(Retry::Page(page), "The request was cancelled.");
                    false
                }
                (
                    ReaderTaskPurpose::PrefetchSource { source },
                    TaskOutcome::Cancelled,
                ) => {
                    let page = self.reader.as_ref().map_or(0, |reader| reader.page);
                    if let Some(reader) = self.reader.as_mut() {
                        if reader.source_fetches.get(&source) == Some(&task) {
                            reader.source_fetches.remove(&source);
                        }
                    }
                    self.fail_reader(Retry::Page(page), "The request was cancelled.");
                    false
                }
                (ReaderTaskPurpose::Maintenance, TaskOutcome::Cancelled) => {
                    let page = self.reader.as_ref().map_or(0, |reader| reader.page);
                    if let Some(reader) = self.reader.as_mut() {
                        if reader.maintenance_task == Some(task) {
                            reader.maintenance_task = None;
                        }
                    }
                    self.fail_reader(Retry::Page(page), "The request was cancelled.");
                    false
                }
            };
            if !shown {
                self.show(context);
            }
            return;
        }
        if self.task != Some(task) {
            return;
        }
        self.task = None;
        let Some(pending) = self.pending.take() else {
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
    }
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
        let destination_start =
            row_byte_offset(segment.destination_row, panel_width, build.format)
                .ok_or_else(|| "The comic page byte offset is not supported.".to_owned())?;
        let destination_end = destination_start
            .checked_add(copied_len)
            .ok_or_else(|| "The comic page byte interval is not supported.".to_owned())?;
        let source_rows = source_bytes
            .get(source_start..source_end)
            .ok_or_else(|| "The comic source pixels do not cover the planned segment.".to_owned())?;
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
    let decoded =
        kobo_image::decode_webp(bytes, format).map_err(|error| error.to_string())?;
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


fn page_bounds(page: usize, count: usize, items_per_page: usize) -> (usize, usize) {
    let start = page.saturating_mul(items_per_page).min(count);
    (start, start.saturating_add(items_per_page).min(count))
}

fn library_has_next_page(page: usize, loaded: usize, remote_more: bool) -> bool {
    page.saturating_add(1)
        .saturating_mul(LIBRARY_ITEMS_PER_PAGE)
        < loaded
        || remote_more
}

fn should_load_recent(page: usize, total_items: usize) -> bool {
    page == 0 && total_items == 0
}

fn page_controls(mut screen: ScreenBuilder, page: usize, count: usize) -> Screen {
    if page > 0 {
        screen = screen.button(PREVIOUS, "Previous page");
    }
    if page
        .saturating_add(1)
        .saturating_mul(EPISODE_ITEMS_PER_PAGE)
        < count
    {
        screen = screen.button(NEXT, "Next page");
    }
    screen.build()
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
        AppRunner, Chrome, Command, Credential, DisplayMetrics, Node, PictureHandle, ReadingChrome,
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
            {"alias":"locked","title":"Locked","isSample":false,"purchaseStatus":null,"paid":true}
        ]}
    }"#;
    const TINY_WEBP: &[u8] = &[
        82, 73, 70, 70, 36, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 32, 24, 0, 0, 0, 48, 1, 0, 157, 1,
        42, 1, 0, 1, 0, 1, 64, 38, 37, 164, 0, 3, 112, 0, 254, 251, 148, 0, 0,
    ];
    const BLACK_1X3_WEBP: &[u8] = &[
        82, 73, 70, 70, 68, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 56, 0, 0, 0, 47, 0,
        128, 0, 16, 205, 85, 32, 34, 2, 30, 72, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
        0, 128, 136, 72, 1,
    ];
    const WHITE_1X2_WEBP: &[u8] = &[
        82, 73, 70, 70, 68, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 56, 0, 0, 0, 47, 0,
        64, 0, 16, 205, 85, 32, 34, 2, 30, 72, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 64,
        0, 0, 128, 136, 200, 0,
    ];

    fn image_manifest(path: &str, policy: &str) -> Vec<u8> {
        format!(
            "{{\"result\":\"SUCCESS\",\"data\":[{{\"orderNo\":1,\"width\":1,\"height\":1,\"imagePath\":\"https://image.balcony.studio{path}?Policy={policy}&Signature=s&Key-Pair-Id=k\",\"line\":null,\"point\":null}}]}}"
        )
        .into_bytes()
    }
    fn image_manifest_sources(count: usize) -> Vec<u8> {
        let images = (0..count)
            .map(|source| {
                format!(
                    "{{\"orderNo\":{},\"width\":1,\"height\":1,\"imagePath\":\"https://image.balcony.studio/tw/ep/{source}.webp?Policy=p{source}&Signature=s&Key-Pair-Id=k\",\"line\":null,\"point\":null}}",
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
            reader.source_cache.len() + reader.source_fetches.len()
                <= reader.limits.source_slots
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
        let source_count =
            usize::try_from(u64::from(u32::MAX) / u64::from(source_height) + 1)
                .expect("source count");
        let images = (0..source_count)
            .map(|source| episode_image(source, 1, source_height))
            .collect::<Vec<_>>();

        let (plans, total_pages) =
            page_plan(&images, 1, u32::MAX).expect("u64 global plan");

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
        let mut builds =
            vec![PageBuild::new(0, PictureFormat::Gray8, 2, 2).expect("Gray8 build")];
        let first = Picture::from_grey(2, 1, vec![10, 10]).expect("first source");
        let second = Picture::from_grey(2, 1, vec![20, 20]).expect("second source");

        copy_source_into_builds(0, &first, &plans, &mut builds, 2, 2)
            .expect("first segment");
        assert_eq!(builds[0].bytes, [10, 10, 255, 255]);
        assert_eq!(builds[0].next_segment, 1);
        copy_source_into_builds(0, &first, &plans, &mut builds, 2, 2)
            .expect("duplicate source is ignored");
        assert_eq!(builds[0].next_segment, 1);
        copy_source_into_builds(1, &second, &plans, &mut builds, 2, 2)
            .expect("second segment");
        assert_eq!(builds[0].bytes, [10, 10, 20, 20]);

        let mut expected =
            Picture::from_grey(2, 2, vec![10, 10, 20, 20]).expect("undithered page");
        expected.dither(PANEL_GREYS).expect("whole-page dither");
        let picture = finish_build(builds.pop().expect("build"), &plans[0], 2, 2)
            .expect("finished Gray8 page");

        assert_eq!(picture.pixels(), expected.pixels());
    }

    #[test]
    fn typed_page_assembly_rgb8_preserves_exact_colors_across_the_source_seam() {
        let plans = vec![seam_plan()];
        let mut builds =
            vec![PageBuild::new(0, PictureFormat::Rgb8, 2, 2).expect("RGB8 build")];
        let red = Picture::from_pixels(
            2,
            1,
            PicturePixels::Rgb8(vec![255, 0, 0, 255, 0, 0]),
        )
        .expect("red source");
        let blue = Picture::from_pixels(
            2,
            1,
            PicturePixels::Rgb8(vec![0, 0, 255, 0, 0, 255]),
        )
        .expect("blue source");

        copy_source_into_builds(0, &red, &plans, &mut builds, 2, 2).expect("red segment");
        copy_source_into_builds(1, &blue, &plans, &mut builds, 2, 2).expect("blue segment");
        let picture = finish_build(builds.pop().expect("build"), &plans[0], 2, 2)
            .expect("finished RGB8 page");

        assert_eq!(
            picture.pixels(),
            PicturePixelsRef::Rgb8(&[
                255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255,
            ])
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
        copy_source_into_builds(0, &grey, &plans, &mut grey_builds, 2, 2)
            .expect("Gray8 segment");
        let grey_page = finish_build(
            grey_builds.pop().expect("Gray8 build"),
            &plan,
            2,
            2,
        )
        .expect("Gray8 page");
        assert_eq!(
            grey_page.pixels(),
            PicturePixelsRef::Gray8(&[0, 0, 255, 255])
        );

        let rgb = Picture::from_pixels(
            2,
            1,
            PicturePixels::Rgb8(vec![1, 2, 3, 4, 5, 6]),
        )
        .expect("RGB8 source");
        let mut rgb_builds =
            vec![PageBuild::new(0, PictureFormat::Rgb8, 2, 2).expect("RGB8 build")];
        copy_source_into_builds(0, &rgb, &plans, &mut rgb_builds, 2, 2)
            .expect("RGB8 segment");
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
        let mut builds =
            vec![PageBuild::new(0, PictureFormat::Rgb8, 2, 1).expect("RGB8 build")];

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
        let mut builds =
            vec![PageBuild::new(0, PictureFormat::Gray8, 2, 1).expect("build")];

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
        let mut builds =
            vec![PageBuild::new(0, PictureFormat::Gray8, 2, 2).expect("build")];

        assert!(copy_source_into_builds(0, &source, &plans, &mut builds, 2, 2).is_err());
    }

    #[test]
    fn page_assembly_incomplete_build_is_refused() {
        let plan = seam_plan();
        let mut builds =
            vec![PageBuild::new(0, PictureFormat::Gray8, 2, 2).expect("build")];
        let source = Picture::from_grey(2, 1, vec![0, 0]).expect("first source");
        copy_source_into_builds(
            0,
            &source,
            std::slice::from_ref(&plan),
            &mut builds,
            2,
            2,
        )
        .expect("first segment");

        assert!(finish_build(builds.pop().expect("build"), &plan, 2, 2).is_err());
    }

    #[test]
    fn page_assembly_rejects_unrepresentable_page_buffer() {
        assert!(PageBuild::new(
            0,
            PictureFormat::Rgb8,
            u32::MAX,
            u32::MAX
        )
        .is_err());
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

        assert_eq!(
            error,
            "BOMTOON returned different comic image dimensions."
        );
    }

    #[test]
    fn webp_decode_boundary_refuses_non_webp_and_platform_oversize_sources() {
        let png = kobo_image::encode_png_grey(1, 1, &[0]).expect("valid non-WebP picture");
        assert!(
            decode_reader_source(
                &png,
                &episode_image(0, 1, 1),
                PictureFormat::Gray8,
                1,
            )
            .is_err()
        );
        let oversized = vec![0; 4 * 1024 * 1024 + 1];
        let error = decode_reader_source(
            &oversized,
            &episode_image(0, 1, 1),
            PictureFormat::Gray8,
            1,
        )
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

    fn only_spawn(commands: &[Command]) -> (TaskId, Task) {
        let mut spawned = commands.iter().filter_map(|command| match command {
            Command::Spawn { task, work } => Some((*task, work.clone())),
            _ => None,
        });
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

    fn loaded_library_with_metrics(
        metrics: DisplayMetrics,
    ) -> (AppRunner<Bomtoon>, Vec<Command>) {
        let mut runner = AppRunner::with_metrics(Bomtoon::default(), metrics);
        let commands = runner.start();
        let (task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(task, TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()));
        assert_eq!(runner.app().view, View::Library);
        (runner, commands)
    }

    fn loaded_library() -> (AppRunner<Bomtoon>, Vec<Command>) {
        loaded_library_with_metrics(CLARA_BW_METRICS)
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
        let width = metrics.width as u32;
        let panel_height = metrics.height as u32;
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
            let picture =
                Picture::from_pixels(width, panel_height, pixels).expect("ready page");
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
        seeded_reader_with_metrics(
            CLARA_BW_METRICS,
            page_count,
            current_page,
            chrome_visible,
        )
    }

    fn prepared_reader(format: PictureFormat) -> AppRunner<Bomtoon> {
        let metrics = reader_metrics(format, 1);
        let (mut runner, manifest_task, _) =
            reader_waiting_for_manifest_with_metrics(metrics);
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
                ReaderTaskPurpose::Manifest | ReaderTaskPurpose::Maintenance => Vec::new(),
            };
            runner.task_outcome(task, TaskOutcome::Completed(bytes));
            assert_reader_bounds(runner.app());
        }
        panic!("reader preparation did not settle");
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
            episode_alias: "ep-1".to_owned(),
            episode_title: "Episode One".to_owned(),
        });
        app.episodes.push(Episode {
            alias: "ep-1".to_owned(),
            title: "Episode One".to_owned(),
            purchase: model::PurchaseState::Owned,
        });
        app.selected_title = "Hunter Q".to_owned();
        app.page = 3;
        app.library_view_page = 2;
        app.recent_view_page = 1;
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
        assert_eq!(app.library_view_page, 0);
        assert_eq!(app.recent_view_page, 0);
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
        assert_eq!(app.library_view_page, 2);
        assert_eq!(app.recent_view_page, 1);
        assert_eq!(app.next_library_page, Some(4));
        assert_eq!(app.next_recent_page, Some(5));
        assert_eq!(app.total_library_titles, 91);
        assert_eq!(app.total_recent_titles, 62);
        assert!(app.library_loaded);
        assert!(app.recent_loaded);
    }

    fn begin_logout(runner: &mut AppRunner<Bomtoon>) -> TaskId {
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
        let (task, _) = only_spawn(&commands);
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

    #[test]
    fn only_owned_and_sample_episode_rows_are_actions() {
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
        assert!(!actions.contains(&action_id("episode-1")));
        assert!(actions.contains(&action_id("episode-2")));
        assert!(!actions.contains(&action_id("episode-3")));
    }

    #[test]
    fn image_manifest_uses_active_panel_width() {
        let libra_colour_metrics = DisplayMetrics {
            width: 1264,
            height: 1680,
            picture_format: PictureFormat::Rgb8,
            ..CLARA_BW_METRICS
        };
        for (metrics, panel_width) in [
            (CLARA_BW_METRICS, 1072),
            (libra_colour_metrics, 1264),
        ] {
            let (_, _, commands) = reader_waiting_for_manifest_with_metrics(metrics);
            let (_, manifest_work) = only_spawn(&commands);
            assert_eq!(
                manifest_work,
                api::images("hunter_q", "ep-1", panel_width)
            );
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
        assert!(screen.nodes.is_empty(), "reader must not expose scrolling controls");
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
            assert!(screen.reading_surface.is_some(), "prepared turn showed loading");
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

            let commands =
                runner.task_outcome(maintenance, TaskOutcome::Completed(Vec::new()));
            assert!(!commands.iter().any(|command| matches!(
                command,
                Command::PutPicture { .. }
                    | Command::SetScreen(_)
                    | Command::DropPicture(_)
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
            assert!(!commands.iter().any(|command| matches!(
                command,
                Command::Spawn { .. } | Command::Cancel(_)
            )));
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
        let mut runner =
            seeded_reader_with_metrics(reader_metrics(format, 1), 6, 3, false);
        let target_task = TaskId(51);
        let future_task = TaskId(52);
        {
            let app = runner.app_mut();
            let reader = app.reader.as_mut().expect("reader");
            reader.source_fetches =
                BTreeMap::from([(2, target_task), (4, future_task)]);
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
        assert!(!commands.iter().any(|command| matches!(
            command,
            Command::Spawn { .. } | Command::Cancel(_)
        )));
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

        let commands = runner.task_outcome(
            future_task,
            TaskOutcome::Completed(TINY_WEBP.to_vec()),
        );
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

        runner.task_outcome(
            target_task,
            TaskOutcome::Completed(TINY_WEBP.to_vec()),
        );
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

            let commands = runner.task_outcome(
                first_task,
                TaskOutcome::Completed(BLACK_1X3_WEBP.to_vec()),
            );
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

            let commands = runner.task_outcome(
                second_task,
                TaskOutcome::Completed(WHITE_1X2_WEBP.to_vec()),
            );
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
            let maintenance = reader.maintenance_task.expect("seam maintenance");

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
        assert_eq!(reader.picture.map(|picture| picture.handle), Some(PictureHandle(7)));
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
    fn startup_requests_library_without_a_session_task() {
        let (_, commands) = started();
        let (_, work) = only_spawn(&commands);
        let Task::Fetch {
            url, credential, ..
        } = work
        else {
            panic!("startup did not request the library");
        };
        assert!(url.starts_with("https://www.bomtoon.tw/api/balcony-api-v2/library?"));
        assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
    }

    #[test]
    fn missing_credentials_show_login_instructions_and_try_again() {
        let (mut runner, commands) = started();
        let (task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(task, TaskOutcome::Failed(TaskError::NoCredential));

        assert_eq!(runner.app().account, AccountState::SignedOut);
        assert_login_instructions(&last_screen(&commands));

        let commands = runner.action(action_id(RETRY));
        let (_, work) = only_spawn(&commands);
        assert!(matches!(work, Task::Fetch { .. }));
        assert_eq!(runner.app().account, AccountState::Active);
    }

    #[test]
    fn a_loaded_library_shows_sign_out() {
        let (mut runner, _) = loaded_library();
        for index in 1..LIBRARY_ITEMS_PER_PAGE {
            runner.app_mut().comics.push(Comic {
                alias: format!("comic-{index}"),
                title: format!("Comic {index}"),
                owned_episodes: index,
                total_episodes: LIBRARY_ITEMS_PER_PAGE,
            });
        }
        runner.app_mut().total_library_titles = 30;
        runner.app_mut().next_library_page = Some(1);
        let commands = runner.action(action_id("refresh-layout"));
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(
            drawn.contains("Sign out"),
            "missing sign-out action: {drawn}"
        );
        assert_fits(&screen);

        let _ = begin_logout(&mut runner);
        assert_eq!(runner.app().pending, Some(Pending::Logout));
        let commands = runner.action(action_id(SIGN_OUT));
        assert!(
            commands
                .iter()
                .all(|command| !matches!(command, Command::Spawn { .. })),
            "a stale sign-out action started overlapping work"
        );
    }

    #[test]
    fn a_full_middle_library_page_fits_and_loads_remote_only_at_the_boundary() {
        let (mut runner, _) = loaded_library();
        for index in 1..REMOTE_LIBRARY_PAGE_SIZE {
            runner.app_mut().comics.push(Comic {
                alias: format!("comic-{index}"),
                title: format!("Comic {index}"),
                owned_episodes: index,
                total_episodes: REMOTE_LIBRARY_PAGE_SIZE + 1,
            });
        }
        runner.app_mut().total_library_titles = REMOTE_LIBRARY_PAGE_SIZE + 1;
        runner.app_mut().next_library_page = Some(1);

        let commands = runner.action(action_id(NEXT));
        assert_eq!(runner.app().page, 1);
        assert_eq!(runner.app().pending, None);
        let screen = last_screen(&commands);
        let drawn = format!("{screen:?}");
        assert!(drawn.contains("Previous page"), "missing previous: {drawn}");
        assert!(drawn.contains("Next page"), "missing next: {drawn}");
        assert!(drawn.contains("Sign out"), "missing sign out: {drawn}");
        assert_fits(&screen);

        for expected_page in 2..(REMOTE_LIBRARY_PAGE_SIZE / LIBRARY_ITEMS_PER_PAGE) {
            let commands = runner.action(action_id(NEXT));
            assert_eq!(runner.app().page, expected_page);
            assert_eq!(runner.app().pending, None);
            assert!(
                commands
                    .iter()
                    .all(|command| !matches!(command, Command::Spawn { .. })),
                "remote page loaded before the local boundary"
            );
        }

        let commands = runner.action(action_id(NEXT));
        let (task, work) = only_spawn(&commands);
        assert!(matches!(
            work,
            Task::Fetch { ref url, .. } if url.contains("page=1")
        ));
        assert_eq!(runner.app().page, 9);
        assert_eq!(runner.app().pending, Some(Pending::Library(1)));

        let commands = runner.task_outcome(
            task,
            TaskOutcome::Completed(REMOTE_LIBRARY_RESPONSE.to_vec()),
        );
        assert_eq!(runner.app().page, 10);
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
        assert_eq!(runner.app().view, View::Library);
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
        assert_login_instructions(&last_screen(&commands));
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
        let (_, work) = only_spawn(&commands);
        assert!(matches!(work, Task::Fetch { .. }));
        assert_eq!(runner.app().account, AccountState::Active);
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
            for (runner, commands) in [
                failed_start(error),
                failed_library_action(RECENT_SHELF, error),
                failed_library_action("comic-0", error),
            ] {
                assert_eq!(runner.app().account, expected);
                assert_login_instructions(&last_screen(&commands));
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
        assert_eq!(runner.app().view, View::Library);
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
    fn library_pagination_uses_its_smaller_capacity_at_the_remote_boundary() {
        let page = 4;
        let loaded = (page + 1) * LIBRARY_ITEMS_PER_PAGE;
        assert!(library_has_next_page(page, loaded + 1, false));
        assert!(library_has_next_page(page, loaded, true));
        assert!(!library_has_next_page(page, loaded, false));
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
    fn an_empty_owned_library_uses_recent_reading() {
        assert!(should_load_recent(0, 0));
        assert!(!should_load_recent(0, 1));
        assert!(!should_load_recent(1, 0));
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
            let (mut runner, manifest_task, _) =
                reader_waiting_for_manifest_with_metrics(metrics);
            runner.task_outcome(
                manifest_task,
                TaskOutcome::Completed(image_manifest_sources(8)),
            );
            assert_reader_bounds(runner.app());
            let mut maximum_window = 0;
            let mut maximum_combined_sources = 0;
            let mut maximum_fetches = 0;
            let mut callbacks = 0;
            loop {
                let Some((&task, entry)) = runner.app().reader_tasks.iter().next_back() else {
                    break;
                };
                let purpose = entry.purpose;
                let bytes = match purpose {
                    ReaderTaskPurpose::ForegroundSource { .. }
                    | ReaderTaskPurpose::PrefetchSource { .. } => TINY_WEBP.to_vec(),
                    ReaderTaskPurpose::Manifest | ReaderTaskPurpose::Maintenance => Vec::new(),
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
        let (mut runner, manifest_task, _) =
            reader_waiting_for_manifest_with_metrics(metrics);
        let commands = runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest_sources(1)),
        );
        let (source_task, _) = only_spawn(&commands);
        let commands =
            runner.task_outcome(source_task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
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
        let (mut runner, manifest_task, _) =
            reader_waiting_for_manifest_with_metrics(metrics);
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
            final_commands =
                runner.task_outcome(task, TaskOutcome::Completed(TINY_WEBP.to_vec()));
            assert_reader_bounds(runner.app());
        }
        assert_eq!(observed_sources, [0, 1, 2, 3]);

        let expected_source = decode_reader_source(
            TINY_WEBP,
            &episode_image(0, 1, 1),
            PictureFormat::Rgb8,
            1,
        )
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
        let (mut runner, manifest_task, _) =
            reader_waiting_for_manifest_with_metrics(metrics);
        let commands = runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest_sources(5)),
        );
        let (first_source, _) = only_spawn(&commands);
        runner.task_outcome(
            first_source,
            TaskOutcome::Completed(TINY_WEBP.to_vec()),
        );
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
        runner.task_outcome(
            page_one_source,
            TaskOutcome::Completed(TINY_WEBP.to_vec()),
        );
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
        runner.task_outcome(
            page_two_source,
            TaskOutcome::Completed(TINY_WEBP.to_vec()),
        );
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
}
