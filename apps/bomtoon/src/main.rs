mod api;
mod model;
mod parse;

use kobo_image::{Picture, PANEL_GREYS};
use kobo_sdk::{
    action_id, ActionId, BannerLevel, Context, Failure, KoboApp, PictureHandle, PicturePixelsRef,
    ReadingChrome, Screen, ScreenBuilder, TaskError, TaskId, TaskOutcome, TilePicture,
};
use model::{display_text, Comic, Episode, EpisodeImage, RecentEntry};
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
    Manifest,
    ManifestRefresh(PageLocation),
    Image(PageLocation),
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
struct PageLocation {
    source: usize,
    slice: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Retry {
    #[default]
    Restart,
    Manifest,
    Image(PageLocation),
    Slice,
}

struct EpisodeSelection {
    content_alias: String,
    episode_alias: String,
    title: String,
}

struct ReaderState {
    images: Vec<EpisodeImage>,
    pages_per_source: Vec<usize>,
    location: PageLocation,
    total_pages: u16,
    source: Option<Picture>,
    picture: Option<TilePicture>,
    chrome_visible: bool,
    refreshed_current_image: bool,
}

impl ReaderState {
    fn global_page(&self) -> u16 {
        let before = self.pages_per_source[..self.location.source]
            .iter()
            .sum::<usize>();
        u16::try_from(before + self.location.slice + 1).unwrap_or(self.total_pages)
    }
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
                Pending::Manifest | Pending::ManifestRefresh(_) => {
                    (self.reader_title(), "Loading comic pages")
                }
                Pending::Image(_) => (self.reader_title(), "Loading comic image"),
                Pending::Logout => (TITLE, "Signing out"),
            };
            return ScreenBuilder::new("bomtoon-loading")
                .top_bar(title)
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
            .page_position(reader.global_page(), reader.total_pages)
            .build()
    }

    fn clear_account_data(&mut self, context: &mut Context) {
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
        self.retry = Retry::Manifest;
        self.spawn(
            context,
            Pending::Manifest,
            api::images(&content_alias, &episode_alias),
        );
    }

    fn start_manifest_refresh(&mut self, context: &mut Context, target: PageLocation) {
        let Some((content_alias, episode_alias)) =
            self.reader_selection.as_ref().map(|selection| {
                (
                    selection.content_alias.clone(),
                    selection.episode_alias.clone(),
                )
            })
        else {
            self.fail_reader(
                Retry::Image(target),
                "The selected episode is no longer available.",
            );
            return;
        };
        self.retry = Retry::Image(target);
        self.spawn(
            context,
            Pending::ManifestRefresh(target),
            api::images(&content_alias, &episode_alias),
        );
    }

    fn start_image(&mut self, context: &mut Context, target: PageLocation, reset_refresh: bool) {
        let Some(url) = self
            .reader
            .as_ref()
            .and_then(|reader| reader.images.get(target.source))
            .map(|image| image.url.clone())
        else {
            self.fail_reader(
                Retry::Manifest,
                "The selected comic image is no longer available.",
            );
            return;
        };
        if reset_refresh {
            if let Some(reader) = self.reader.as_mut() {
                reader.refreshed_current_image = false;
            }
        }
        self.retry = Retry::Image(target);
        self.spawn(context, Pending::Image(target), api::image(&url));
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

    fn install_slice(&mut self, context: &mut Context, target: PageLocation) -> Result<(), String> {
        let panel_height = u32::try_from(context.metrics().height)
            .map_err(|_| "The panel height is not supported.".to_owned())?;
        let slice = {
            let reader = self
                .reader
                .as_ref()
                .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
            let source = reader
                .source
                .as_ref()
                .ok_or_else(|| "The comic image needs to be loaded again.".to_owned())?;
            slice_rows(source, target.slice, panel_height)?
        };
        let handle = self.next_handle()?;
        let width = slice.width();
        let height = slice.height();
        let picture = context
            .put_picture(handle, width, height, slice.into_pixels())
            .ok_or_else(|| "The comic slice could not be uploaded.".to_owned())?;
        let old = {
            let reader = self
                .reader
                .as_mut()
                .ok_or_else(|| "The selected episode is no longer available.".to_owned())?;
            reader.location = target;
            reader.chrome_visible = false;
            reader.picture.replace(picture)
        };
        self.problem = None;
        self.show(context);
        if let Some(old) = old {
            context.drop_picture(old.handle);
        }
        Ok(())
    }

    fn accept_manifest(&mut self, context: &mut Context, bytes: &[u8]) {
        let panel_width = u32::try_from(context.metrics().width);
        let panel_height = u32::try_from(context.metrics().height);
        let planned = match (panel_width, panel_height) {
            (Ok(width), Ok(height)) => parse::images(bytes)
                .map_err(|error| error.to_string())
                .and_then(|images| {
                    page_plan(&images, width, height).map(|(pages_per_source, total_pages)| {
                        (images, pages_per_source, total_pages)
                    })
                }),
            _ => Err("The panel dimensions are not supported.".to_owned()),
        };
        match planned {
            Ok((images, pages_per_source, total_pages)) => {
                let target = PageLocation {
                    source: 0,
                    slice: 0,
                };
                self.reader = Some(ReaderState {
                    images,
                    pages_per_source,
                    location: target,
                    total_pages,
                    source: None,
                    picture: None,
                    chrome_visible: false,
                    refreshed_current_image: false,
                });
                self.start_image(context, target, true);
            }
            Err(error) => self.fail_reader(Retry::Manifest, error),
        }
    }

    fn accept_manifest_refresh(
        &mut self,
        context: &mut Context,
        target: PageLocation,
        bytes: &[u8],
    ) {
        match parse::images(bytes) {
            Ok(refreshed)
                if self
                    .reader
                    .as_ref()
                    .is_some_and(|reader| same_assets(&reader.images, &refreshed)) =>
            {
                if let Some(reader) = self.reader.as_mut() {
                    for (current, replacement) in reader.images.iter_mut().zip(refreshed) {
                        current.url = replacement.url;
                    }
                }
                self.start_image(context, target, false);
            }
            Ok(_) => self.fail_reader(
                Retry::Image(target),
                "BOMTOON returned different comic image metadata.",
            ),
            Err(error) => self.fail_reader(Retry::Image(target), error.to_string()),
        }
    }

    fn accept_image(&mut self, context: &mut Context, target: PageLocation, bytes: &[u8]) -> bool {
        let Ok(panel_width) = u32::try_from(context.metrics().width) else {
            self.fail_reader(Retry::Image(target), "The panel width is not supported.");
            return false;
        };
        let scaled = (|| {
            let expected = self
                .reader
                .as_ref()
                .and_then(|reader| reader.images.get(target.source))
                .ok_or_else(|| "The selected comic image is no longer available.".to_owned())?;
            let decoded = kobo_image::decode(bytes).map_err(|error| error.to_string())?;
            if (decoded.width(), decoded.height()) != (expected.width, expected.height) {
                return Err("BOMTOON returned different comic image dimensions.".to_owned());
            }
            let mut scaled = decoded
                .scale_to_width(panel_width)
                .map_err(|error| error.to_string())?;
            scaled
                .dither(PANEL_GREYS)
                .map_err(|error| error.to_string())?;
            Ok(scaled)
        })();
        match scaled {
            Ok(source) => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.source = Some(source);
                    reader.location = target;
                    reader.chrome_visible = false;
                }
                self.retry = Retry::Slice;
                match self.install_slice(context, target) {
                    Ok(()) => return true,
                    Err(error) => self.fail_reader(Retry::Slice, error),
                }
            }
            Err(error) => self.fail_reader(Retry::Image(target), error),
        }
        false
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
            Pending::Manifest => self.accept_manifest(context, bytes),
            Pending::ManifestRefresh(target) => {
                self.accept_manifest_refresh(context, target, bytes);
            }
            Pending::Image(target) => return self.accept_image(context, target, bytes),
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
        if let Some(task) = self.task.take() {
            context.cancel(task);
        }
        self.pending = None;
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

    fn turn_reader(&mut self, context: &mut Context, target: PageLocation) {
        let same_source = self
            .reader
            .as_ref()
            .is_some_and(|reader| reader.location.source == target.source);
        if let Some(reader) = self.reader.as_mut() {
            reader.location = target;
            reader.chrome_visible = false;
            if !same_source {
                reader.source = None;
                reader.refreshed_current_image = false;
            }
        }
        if same_source {
            self.retry = Retry::Slice;
            if let Err(error) = self.install_slice(context, target) {
                self.fail_reader(Retry::Slice, error);
                self.show(context);
            }
        } else {
            self.start_image(context, target, true);
            self.show(context);
        }
    }

    fn retry(&mut self, context: &mut Context) -> bool {
        let retry = self.retry;
        self.problem = None;
        match retry {
            Retry::Restart => self.restart(context),
            Retry::Manifest => self.start_manifest(context),
            Retry::Image(target) => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.location = target;
                    reader.source = None;
                    reader.refreshed_current_image = false;
                }
                self.start_image(context, target, true);
            }
            Retry::Slice => {
                let Some(target) = self.reader.as_ref().map(|reader| reader.location) else {
                    self.fail_reader(
                        Retry::Restart,
                        "The selected episode is no longer available.",
                    );
                    return false;
                };
                match self.install_slice(context, target) {
                    Ok(()) => return true,
                    Err(error) => self.fail_reader(Retry::Slice, error),
                }
            }
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
                Pending::Library(_)
                | Pending::Recent(_)
                | Pending::Content(_)
                | Pending::Manifest
                | Pending::ManifestRefresh(_),
                TaskError::NoCredential,
            ) => {
                self.clear_account_data(context);
                self.account = AccountState::SignedOut;
                self.problem = None;
            }
            (
                Pending::Library(_)
                | Pending::Recent(_)
                | Pending::Content(_)
                | Pending::Manifest
                | Pending::ManifestRefresh(_),
                TaskError::Unauthorized,
            ) => {
                self.clear_account_data(context);
                self.account = AccountState::Expired;
                self.problem = None;
            }
            (Pending::Image(target), TaskError::Unauthorized) => {
                let can_refresh = self
                    .reader
                    .as_ref()
                    .is_some_and(|reader| !reader.refreshed_current_image);
                if can_refresh {
                    if let Some(reader) = self.reader.as_mut() {
                        reader.refreshed_current_image = true;
                    }
                    self.start_manifest_refresh(context, target);
                } else {
                    self.fail_reader(
                        Retry::Image(target),
                        "BOMTOON did not authorize the selected comic image.",
                    );
                }
            }
            (Pending::Image(target) | Pending::ManifestRefresh(target), error) => {
                self.fail_reader(Retry::Image(target), Failure::of(error).advice);
            }
            (Pending::Manifest, error) => {
                self.fail_reader(Retry::Manifest, Failure::of(error).advice);
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
                previous_location(&reader.pages_per_source, reader.location)
            } else if action == action_id(READER_NEXT) {
                next_location(&reader.pages_per_source, reader.location)
            } else {
                None
            }
        });
        if let Some(target) = target {
            self.turn_reader(context, target);
        } else if action != action_id(READER_PREVIOUS) && action != action_id(READER_NEXT) {
            self.show(context);
        }
    }

    fn cancel_task(&mut self, pending: Pending) {
        match pending {
            Pending::Manifest => {
                self.fail_reader(Retry::Manifest, "The request was cancelled.");
            }
            Pending::ManifestRefresh(target) | Pending::Image(target) => {
                self.fail_reader(Retry::Image(target), "The request was cancelled.");
            }
            _ => {
                self.problem = Some("The request was cancelled.".to_owned());
                self.retry = Retry::Restart;
            }
        }
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
            && self.pending.is_none();
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

fn slices_for(height: u32, panel_height: u32) -> Option<usize> {
    if height == 0 || panel_height == 0 {
        return None;
    }
    let height = usize::try_from(height).ok()?;
    let panel = usize::try_from(panel_height).ok()?;
    Some(height.div_ceil(panel))
}

fn page_plan(
    images: &[EpisodeImage],
    panel_width: u32,
    panel_height: u32,
) -> Result<(Vec<usize>, u16), String> {
    let mut pages = Vec::with_capacity(images.len());
    let mut total = 0_usize;
    for image in images {
        let (_, scaled_height) =
            kobo_image::width_scaled_size((image.width, image.height), panel_width)
                .map_err(|error| error.to_string())?;
        let count = slices_for(scaled_height, panel_height)
            .ok_or_else(|| "The comic page dimensions are not supported.".to_owned())?;
        total = total
            .checked_add(count)
            .ok_or_else(|| "The comic has too many pages.".to_owned())?;
        pages.push(count);
    }
    let total = u16::try_from(total).map_err(|_| "The comic has too many pages.".to_owned())?;
    Ok((pages, total))
}

fn previous_location(pages: &[usize], current: PageLocation) -> Option<PageLocation> {
    if current.slice > 0 {
        return Some(PageLocation {
            source: current.source,
            slice: current.slice - 1,
        });
    }
    let source = current.source.checked_sub(1)?;
    Some(PageLocation {
        source,
        slice: pages.get(source)?.checked_sub(1)?,
    })
}

fn next_location(pages: &[usize], current: PageLocation) -> Option<PageLocation> {
    let count = *pages.get(current.source)?;
    let slice = current.slice.checked_add(1)?;
    if slice < count {
        return Some(PageLocation {
            source: current.source,
            slice,
        });
    }
    let source = current.source.checked_add(1)?;
    pages.get(source).map(|_| PageLocation { source, slice: 0 })
}

fn slice_rows(source: &Picture, slice: usize, panel_height: u32) -> Result<Picture, String> {
    let width = usize::try_from(source.width())
        .map_err(|_| "The comic width is not supported.".to_owned())?;
    let panel = usize::try_from(panel_height)
        .map_err(|_| "The panel height is not supported.".to_owned())?;
    if panel == 0 {
        return Err("The panel height is not supported.".to_owned());
    }
    let start_row = slice
        .checked_mul(panel)
        .ok_or_else(|| "The comic page offset is too large.".to_owned())?;
    let source_height = usize::try_from(source.height())
        .map_err(|_| "The comic height is not supported.".to_owned())?;
    if start_row >= source_height {
        return Err("The comic page offset is outside the image.".to_owned());
    }
    let copied_rows = (source_height - start_row).min(panel);
    let output_len = width
        .checked_mul(panel)
        .ok_or_else(|| "The comic slice is too large.".to_owned())?;
    let copied_len = width
        .checked_mul(copied_rows)
        .ok_or_else(|| "The comic slice is too large.".to_owned())?;
    let source_start = width
        .checked_mul(start_row)
        .ok_or_else(|| "The comic page offset is too large.".to_owned())?;
    let source_end = source_start
        .checked_add(copied_len)
        .ok_or_else(|| "The comic page offset is too large.".to_owned())?;
    let PicturePixelsRef::Gray8(grey) = source.pixels() else {
        return Err("this operation requires a grayscale picture".to_owned());
    };
    let source_rows = grey
        .get(source_start..source_end)
        .ok_or_else(|| "The comic pixels do not match their dimensions.".to_owned())?;
    let mut grey = vec![255; output_len];
    grey[..copied_len].copy_from_slice(source_rows);
    Picture::from_grey(source.width(), panel_height, grey).map_err(|error| error.to_string())
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
        AppRunner, Chrome, Command, Credential, Node, PictureHandle, ReadingChrome, SecretHeader,
        Task, TilePicture, CLARA_BW_METRICS,
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

    fn image_manifest(path: &str, policy: &str) -> Vec<u8> {
        format!(
            "{{\"result\":\"SUCCESS\",\"data\":[{{\"orderNo\":1,\"width\":1,\"height\":1,\"imagePath\":\"https://image.balcony.studio{path}?Policy={policy}&Signature=s&Key-Pair-Id=k\",\"line\":null,\"point\":null}}]}}"
        )
        .into_bytes()
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

    fn loaded_library() -> (AppRunner<Bomtoon>, Vec<Command>) {
        let (mut runner, commands) = started();
        let (task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(task, TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()));
        assert_eq!(runner.app().view, View::Library);
        (runner, commands)
    }

    fn reader_waiting_for_manifest() -> (AppRunner<Bomtoon>, TaskId, Vec<Command>) {
        let (mut runner, _) = loaded_library();
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

    fn reader_waiting_for_first_image() -> (AppRunner<Bomtoon>, TaskId) {
        let (mut runner, manifest_task, _) = reader_waiting_for_manifest();
        let commands = runner.task_outcome(
            manifest_task,
            TaskOutcome::Completed(image_manifest("/tw/ep/one.webp", "p1")),
        );
        let (image_task, _) = only_spawn(&commands);
        (runner, image_task)
    }

    fn seeded_reader(
        pages_per_source: Vec<usize>,
        location: PageLocation,
        chrome_visible: bool,
    ) -> AppRunner<Bomtoon> {
        let width = CLARA_BW_METRICS.width as u32;
        let panel_height = CLARA_BW_METRICS.height as u32;
        let source_pages = u32::try_from(pages_per_source[location.source])
            .expect("seeded reader source page count must fit in u32");
        let source_height = panel_height * source_pages;
        let images = pages_per_source
            .iter()
            .enumerate()
            .map(|(index, pages)| EpisodeImage {
                order: index + 1,
                width,
                height: panel_height
                    * u32::try_from(*pages)
                        .expect("seeded reader image page count must fit in u32"),
                path: format!("/tw/ep/{index}.webp"),
                url: format!(
                    "https://image.balcony.studio/tw/ep/{index}.webp?Policy=p&Signature=s&Key-Pair-Id=k"
                ),
            })
            .collect();
        let total_pages = u16::try_from(pages_per_source.iter().sum::<usize>())
            .expect("seeded reader total page count must fit in u16");
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
                    images,
                    pages_per_source,
                    location,
                    total_pages,
                    source: Some(
                        Picture::from_grey(
                            width,
                            source_height,
                            vec![127; width as usize * source_height as usize],
                        )
                        .expect("scaled source"),
                    ),
                    picture: Some(TilePicture::new(PictureHandle(7), width, panel_height)),
                    chrome_visible,
                    refreshed_current_image: false,
                }),
                ..Bomtoon::default()
            },
            CLARA_BW_METRICS,
        )
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
        assert_eq!(manifest_work, api::images("hunter_q", "ep-1"));
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
        assert!(commands
            .iter()
            .any(|command| matches!(command, Command::PutPicture { .. })));
        assert_fits(&screen);
    }

    #[test]
    fn cdn_unauthorized_refreshes_once_without_expiring_account() {
        let (mut runner, image_task) = reader_waiting_for_first_image();
        let commands =
            runner.task_outcome(image_task, TaskOutcome::Failed(TaskError::Unauthorized));
        assert_eq!(runner.app().account, AccountState::Active);
        let (refresh_task, refresh_work) = only_spawn(&commands);
        assert_eq!(refresh_work, api::images("hunter_q", "ep-1"));

        let commands = runner.task_outcome(
            refresh_task,
            TaskOutcome::Completed(image_manifest("/tw/ep/one.webp", "p2")),
        );
        let (retry_task, retry_work) = only_spawn(&commands);
        let Task::Fetch {
            url, credential, ..
        } = retry_work
        else {
            panic!("expected image retry");
        };
        assert!(url.contains("Policy=p2"));
        assert_eq!(credential, None);

        let commands =
            runner.task_outcome(retry_task, TaskOutcome::Failed(TaskError::Unauthorized));
        assert_eq!(runner.app().account, AccountState::Active);
        assert!(commands
            .iter()
            .all(|command| !matches!(command, Command::Spawn { .. })));
    }

    #[test]
    fn row_slices_cover_source_once_and_pad_only_the_final_page() {
        let source = Picture::from_grey(2, 5, (0..10).collect()).expect("source");
        let first = slice_rows(&source, 0, 3).expect("first");
        let second = slice_rows(&source, 1, 3).expect("second");
        assert_eq!(
            first.pixels(),
            PicturePixelsRef::Gray8(&[0, 1, 2, 3, 4, 5])
        );
        assert_eq!(
            second.pixels(),
            PicturePixelsRef::Gray8(&[6, 7, 8, 9, 255, 255])
        );
    }

    #[test]
    fn center_toggles_chrome_and_boundary_noop_preserves_it() {
        let mut runner = seeded_reader(
            vec![1],
            PageLocation {
                source: 0,
                slice: 0,
            },
            false,
        );
        let commands = runner.action(action_id(READER_CHROME));
        assert_eq!(
            last_screen(&commands)
                .reading_surface
                .expect("surface")
                .chrome,
            ReadingChrome::Overlay
        );
        runner.action(action_id(READER_NEXT));
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
        let mut runner = seeded_reader(
            vec![2],
            PageLocation {
                source: 0,
                slice: 0,
            },
            true,
        );
        let commands = runner.action(action_id(READER_NEXT));
        let reader = runner.app().reader.as_ref().expect("reader");
        assert_eq!(
            reader.location,
            PageLocation {
                source: 0,
                slice: 1
            }
        );
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
    fn source_boundary_targets_are_exact_and_reversible() {
        let pages = [2, 3];
        assert_eq!(
            next_location(
                &pages,
                PageLocation {
                    source: 0,
                    slice: 1
                }
            ),
            Some(PageLocation {
                source: 1,
                slice: 0
            })
        );
        assert_eq!(
            previous_location(
                &pages,
                PageLocation {
                    source: 1,
                    slice: 0
                }
            ),
            Some(PageLocation {
                source: 0,
                slice: 1
            })
        );
        assert_eq!(
            previous_location(
                &pages,
                PageLocation {
                    source: 0,
                    slice: 0
                }
            ),
            None
        );
        assert_eq!(
            next_location(
                &pages,
                PageLocation {
                    source: 1,
                    slice: 2
                }
            ),
            None
        );
    }

    #[test]
    fn refreshed_manifest_rejects_changed_asset_identity() {
        for refreshed in [
            image_manifest("/tw/ep/different.webp", "p2"),
            String::from_utf8(image_manifest("/tw/ep/one.webp", "p2"))
                .expect("JSON")
                .replace("\"height\":1", "\"height\":2")
                .into_bytes(),
        ] {
            let (mut runner, image_task) = reader_waiting_for_first_image();
            let commands =
                runner.task_outcome(image_task, TaskOutcome::Failed(TaskError::Unauthorized));
            let (refresh_task, _) = only_spawn(&commands);
            let commands = runner.task_outcome(refresh_task, TaskOutcome::Completed(refreshed));
            assert!(runner.app().problem.is_some());
            assert!(commands
                .iter()
                .all(|command| !matches!(command, Command::Spawn { .. })));
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
        let before = runner.app().pending;
        let commands = runner.task_outcome(
            TaskId(image_task.0 + 1),
            TaskOutcome::Completed(TINY_WEBP.to_vec()),
        );
        assert!(commands.is_empty());
        assert_eq!(runner.app().pending, before);
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
        let mut runner = seeded_reader(
            vec![1],
            PageLocation {
                source: 0,
                slice: 0,
            },
            true,
        );
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

        let mut runner = seeded_reader(
            vec![1],
            PageLocation {
                source: 0,
                slice: 0,
            },
            false,
        );
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
}
