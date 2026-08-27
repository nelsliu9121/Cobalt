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
const SIGN_OUT: &str = "sign-out";
const PREVIOUS: &str = "previous";
const NEXT: &str = "next";
const LIBRARY_SHELF: &str = "library-shelf";
const RECENT_SHELF: &str = "recent-shelf";
const LIBRARY_ITEMS_PER_PAGE: usize = 3;
const EPISODE_ITEMS_PER_PAGE: usize = 6;

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

#[derive(Default)]
struct Bomtoon {
    account: AccountState,
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
        let owns_back = self.account == AccountState::Active
            && self.problem.is_none()
            && self.view == View::Episodes
            && self.pending.is_none();
        context.set_screen(self.screen().with_own_back(owns_back));
    }

    fn screen(&self) -> Screen {
        if let Some(pending) = self.pending {
            let message = match pending {
                Pending::Library(_) => "Loading your library",
                Pending::Recent(_) => "Loading recent reading",
                Pending::Content(_) => "Loading episode purchase status",
                Pending::Logout => "Signing out",
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
        }
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
        if library_has_next_page(
            self.page,
            count,
            self.shelf_next_page().is_some(),
        ) {
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
        let (start, end) = page_bounds(
            self.page,
            self.episodes.len(),
            EPISODE_ITEMS_PER_PAGE,
        );
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

    fn clear_account_data(&mut self) {
        self.comics.clear();
        self.recent.clear();
        self.episodes.clear();
        self.selected_title.clear();
        self.page = 0;
        self.library_view_page = 0;
        self.recent_view_page = 0;
        self.next_library_page = None;
        self.next_recent_page = None;
        self.total_library_titles = 0;
        self.total_recent_titles = 0;
        self.library_loaded = false;
        self.recent_loaded = false;
    }

    fn restart(&mut self, context: &mut Context) {
        self.problem = None;
        self.account = AccountState::Active;
        self.clear_account_data();
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

    fn accept(&mut self, context: &mut Context, pending: Pending, bytes: &[u8]) {
        match pending {
            Pending::Logout => {
                if bytes.is_empty() {
                    self.clear_account_data();
                    self.account = AccountState::SignedOut;
                } else {
                    self.problem =
                        Some("BOMTOON returned unexpected sign-out data.".to_owned());
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
            Pending::Content(_index) => match parse::episodes(bytes) {
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
        self.spawn(context, Pending::Content(index), api::content(&alias));
        self.show(context);
    }
}

impl KoboApp for Bomtoon {
    fn on_start(&mut self, context: &mut Context) {
        self.restart(context);
        self.show(context);
    }

    fn on_action(&mut self, context: &mut Context, action: ActionId) {
        let ready = self.account == AccountState::Active
            && self.problem.is_none()
            && self.pending.is_none();
        let retry_visible = self.problem.is_some()
            || self.account != AccountState::Active
            || self.view == View::Status;
        if action == ActionId::BACK && ready && self.view == View::Episodes {
            self.view = View::Library;
            self.page = match self.shelf {
                Shelf::Library => self.library_view_page,
                Shelf::Recent => self.recent_view_page,
            };
            self.show(context);
            return;
        }
        if action == action_id(RETRY) && self.pending.is_none() && retry_visible {
            self.restart(context);
        } else if !ready {
            self.show(context);
            return;
        } else if self.view == View::Library && action == action_id(SIGN_OUT) {
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
            let next_start = self
                .page
                .saturating_add(1)
                .saturating_mul(items_per_page);
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
        match (pending, outcome) {
            (pending, TaskOutcome::Completed(bytes)) => {
                self.accept(context, pending, &bytes);
            }
            (
                Pending::Logout,
                TaskOutcome::Failed(TaskError::RevocationUnconfirmed),
            ) => {
                self.clear_account_data();
                self.account = AccountState::RevocationUnconfirmed;
                self.problem = None;
            }
            (Pending::Logout, TaskOutcome::Failed(TaskError::LocalStorage)) => {
                self.problem =
                    Some("Could not remove the local BOMTOON sign-in data.".to_owned());
            }
            (
                Pending::Library(_) | Pending::Recent(_) | Pending::Content(_),
                TaskOutcome::Failed(TaskError::NoCredential),
            ) => {
                self.account = AccountState::SignedOut;
                self.problem = None;
            }
            (
                Pending::Library(_) | Pending::Recent(_) | Pending::Content(_),
                TaskOutcome::Failed(TaskError::Unauthorized),
            ) => {
                self.account = AccountState::Expired;
                self.problem = None;
            }
            (_, TaskOutcome::Failed(error)) => {
                self.problem = Some(Failure::of(error).advice.to_owned());
            }
            (_, TaskOutcome::Cancelled) => {
                self.problem = Some("The request was cancelled.".to_owned());
            }
        }
        self.show(context);
    }
}

fn page_bounds(page: usize, count: usize, items_per_page: usize) -> (usize, usize) {
    let start = page.saturating_mul(items_per_page).min(count);
    (start, start.saturating_add(items_per_page).min(count))
}

fn library_has_next_page(page: usize, loaded: usize, remote_more: bool) -> bool {
    page
        .saturating_add(1)
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
        AppRunner, Chrome, Command, Credential, SecretHeader, Task, CLARA_BW_METRICS,
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
        "data":{"episodes":[{
            "alias":"ep-1",
            "title":"Episode One",
            "isSample":false,
            "purchaseStatus":"POSSESSION"
        }]}
    }"#;

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
        let commands = runner.task_outcome(
            task,
            TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()),
        );
        assert_eq!(runner.app().view, View::Library);
        (runner, commands)
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

    fn failed_library_action(
        action: &str,
        error: TaskError,
    ) -> (AppRunner<Bomtoon>, Vec<Command>) {
        let (mut runner, _) = loaded_library();
        let commands = runner.action(action_id(action));
        let (task, _) = only_spawn(&commands);
        let commands = runner.task_outcome(task, TaskOutcome::Failed(error));
        (runner, commands)
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
        let commands = runner.task_outcome(
            task,
            TaskOutcome::Failed(TaskError::NoCredential),
        );

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
        assert!(drawn.contains("Sign out"), "missing sign-out action: {drawn}");
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
        let commands = runner.task_outcome(
            task,
            TaskOutcome::Failed(TaskError::RevocationUnconfirmed),
        );

        assert_eq!(
            runner.app().account,
            AccountState::RevocationUnconfirmed
        );
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
        let commands = runner.task_outcome(
            task,
            TaskOutcome::Failed(TaskError::LocalStorage),
        );

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

        let commands = runner.task_outcome(
            task,
            TaskOutcome::Failed(TaskError::Unauthorized),
        );
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

        let commands = runner.task_outcome(
            task,
            TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
        );
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
        let commands = runner.task_outcome(
            task,
            TaskOutcome::Completed(b"unexpected".to_vec()),
        );

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
