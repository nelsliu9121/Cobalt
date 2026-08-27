mod api;
mod model;
mod parse;

use kobo_sdk::{
    action_id, ActionId, BannerLevel, Context, Failure, KoboApp, Screen, ScreenBuilder, TaskError,
    TaskId, TaskOutcome,
};
use model::{display_text, Comic, Episode, RecentEntry};
use std::process::ExitCode;

const TITLE: &str = "BOMTOON";
const RETRY: &str = "retry";
const PREVIOUS: &str = "previous";
const NEXT: &str = "next";
const LIBRARY_SHELF: &str = "library-shelf";
const RECENT_SHELF: &str = "recent-shelf";
const ITEMS_PER_PAGE: usize = 6;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum View {
    #[default]
    Status,
    Library,
    Episodes,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Shelf {
    #[default]
    Library,
    Recent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pending {
    Session,
    Library(usize),
    Recent(usize),
    Detail(usize),
}

#[derive(Default)]
struct Bomtoon {
    view: View,
    pending: Option<Pending>,
    task: Option<TaskId>,
    comics: Vec<Comic>,
    recent: Vec<RecentEntry>,
    episodes: Vec<Episode>,
    selected_title: String,
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
        let owns_back = self.view == View::Episodes && self.pending.is_none();
        context.set_screen(self.screen().with_own_back(owns_back));
    }

    fn screen(&self) -> Screen {
        if let Some(pending) = self.pending {
            let message = match pending {
                Pending::Session => "Checking the stored session",
                Pending::Library(_) => "Loading your library",
                Pending::Recent(_) => "Loading recent reading",
                Pending::Detail(_) => "Loading episode purchase status",
            };
            return ScreenBuilder::new("bomtoon-loading")
                .top_bar(TITLE)
                .activity(message, None)
                .build();
        }
        if let Some(problem) = &self.problem {
            return ScreenBuilder::new("bomtoon-error")
                .top_bar(TITLE)
                .banner(BannerLevel::Attention, problem.clone())
                .primary_button(RETRY, "Try again")
                .build();
        }
        match self.view {
            View::Status => ScreenBuilder::new("bomtoon-status")
                .top_bar(TITLE)
                .text("No request has started.")
                .primary_button(RETRY, "Connect")
                .build(),
            View::Library => self.library_screen(),
            View::Episodes => self.episode_screen(),
        }
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
        let (start, end) = page_bounds(self.page, count);
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
        if library_has_next_page(
            self.page,
            count,
            self.shelf_next_page().is_some(),
        ) {
            screen = screen.button(NEXT, "Next page");
        }
        screen.build()
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

    fn shelf_is_loaded(&self, shelf: Shelf) -> bool {
        match shelf {
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

    fn switch_shelf(&mut self, context: &mut Context, shelf: Shelf) {
        if self.shelf == shelf {
            return;
        }
        self.remember_shelf_page();
        self.shelf = shelf;
        self.page = match shelf {
            Shelf::Library => self.library_view_page,
            Shelf::Recent => self.recent_view_page,
        };
        self.problem = None;
        if !self.shelf_is_loaded(shelf) {
            let (pending, work) = match shelf {
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
        let (start, end) = page_bounds(self.page, self.episodes.len());
        for episode in &self.episodes[start..end] {
            let title_fallback = format!("Episode {}", episode.alias);
            let status = display_text(episode.purchase.label(), "Other status");
            screen = screen.text(format!(
                "{} [{}] - {}",
                display_text(&episode.title, &title_fallback),
                episode.alias,
                status
            ));
        }
        page_controls(screen, self.page, self.episodes.len())
    }

    fn restart(&mut self, context: &mut Context) {
        self.problem = None;
        self.comics.clear();
        self.recent.clear();
        self.episodes.clear();
        self.page = 0;
        self.library_view_page = 0;
        self.recent_view_page = 0;
        self.next_library_page = None;
        self.next_recent_page = None;
        self.total_library_titles = 0;
        self.total_recent_titles = 0;
        self.library_loaded = false;
        self.recent_loaded = false;
        self.shelf = Shelf::Library;
        self.view = View::Status;
        self.spawn(context, Pending::Session, api::session());
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

    fn accept(&mut self, context: &mut Context, pending: Pending, bytes: &[u8]) {
        match pending {
            Pending::Session => match parse::session_is_authenticated(bytes) {
                Ok(true) => self.spawn(context, Pending::Library(0), api::library(0)),
                Ok(false) => {
                    self.problem = Some("The stored BOMTOON session is not signed in.".to_owned());
                }
                Err(error) => self.problem = Some(error.to_string()),
            },
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
                Ok(_) => self.problem = Some("BOMTOON returned a different library page.".to_owned()),
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
                Ok(_) => self.problem = Some("BOMTOON returned a different recent page.".to_owned()),
                Err(error) => self.problem = Some(error.to_string()),
            },
            Pending::Detail(_index) => match parse::episodes(bytes) {
                Ok(episodes) => {
                    self.episodes = episodes;
                    self.page = 0;
                    self.view = View::Episodes;
                }
                Err(error) => self.problem = Some(error.to_string()),
            },
        }
    }

    fn open_comic(&mut self, context: &mut Context, index: usize) {
        let selected = match self.shelf {
            Shelf::Library => self
                .comics
                .get(index)
                .map(|comic| (comic.alias.clone(), comic.title.clone())),
            Shelf::Recent => self.recent.get(index).map(|recent| {
                (recent.content_alias.clone(), recent.content_title.clone())
            }),
        };
        let Some((alias, title)) = selected else {
            return;
        };
        self.remember_shelf_page();
        self.selected_title = display_text(&title, &format!("BOMTOON {alias}"));
        self.problem = None;
        self.spawn(context, Pending::Detail(index), api::detail(&alias));
        self.show(context);
    }
}

impl KoboApp for Bomtoon {
    fn on_start(&mut self, context: &mut Context) {
        self.restart(context);
        self.show(context);
    }

    fn on_action(&mut self, context: &mut Context, action: ActionId) {
        if action == ActionId::BACK && self.view == View::Episodes {
            self.view = View::Library;
            self.page = match self.shelf {
                Shelf::Library => self.library_view_page,
                Shelf::Recent => self.recent_view_page,
            };
            self.show(context);
            return;
        }
        if action == action_id(RETRY) {
            self.restart(context);
        } else if self.view == View::Library && action == action_id(LIBRARY_SHELF) {
            self.switch_shelf(context, Shelf::Library);
        } else if self.view == View::Library && action == action_id(RECENT_SHELF) {
            self.switch_shelf(context, Shelf::Recent);
        } else if action == action_id(PREVIOUS) {
            self.page = self.page.saturating_sub(1);
        } else if action == action_id(NEXT) {
            let next_start = self
                .page
                .saturating_add(1)
                .saturating_mul(ITEMS_PER_PAGE);
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
        match outcome {
            TaskOutcome::Completed(bytes) => self.accept(context, pending, &bytes),
            TaskOutcome::Failed(TaskError::NoCredential) => {
                let name = match pending {
                    Pending::Library(_) | Pending::Recent(_) => "bomtoon-access-token",
                    Pending::Session | Pending::Detail(_) => "bomtoon-session",
                };
                self.problem = Some(format!("Stored credential '{name}' is missing."));
            }
            TaskOutcome::Failed(error) => {
                self.problem = Some(Failure::of(error).advice.to_owned());
            }
            TaskOutcome::Cancelled => self.problem = Some("The request was cancelled.".to_owned()),
        }
        self.show(context);
    }
}

fn page_bounds(page: usize, count: usize) -> (usize, usize) {
    let start = page.saturating_mul(ITEMS_PER_PAGE).min(count);
    (start, start.saturating_add(ITEMS_PER_PAGE).min(count))
}

fn library_has_next_page(page: usize, loaded: usize, remote_more: bool) -> bool {
    page.saturating_add(1).saturating_mul(ITEMS_PER_PAGE) < loaded || remote_more
}

fn should_load_recent(page: usize, total_items: usize) -> bool {
    page == 0 && total_items == 0
}

fn page_controls(mut screen: ScreenBuilder, page: usize, count: usize) -> Screen {
    if page > 0 {
        screen = screen.button(PREVIOUS, "Previous page");
    }
    if page.saturating_add(1).saturating_mul(ITEMS_PER_PAGE) < count {
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
    use super::{library_has_next_page, should_load_recent};

    #[test]
    fn a_remote_library_page_is_loaded_only_at_the_local_boundary() {
        assert!(library_has_next_page(0, 30, true));
        assert!(library_has_next_page(4, 30, true));
        assert!(!library_has_next_page(4, 30, false));
    }

    #[test]
    fn an_empty_owned_library_uses_recent_reading() {
        assert!(should_load_recent(0, 0));
        assert!(!should_load_recent(0, 1));
        assert!(!should_load_recent(1, 0));
    }
}
