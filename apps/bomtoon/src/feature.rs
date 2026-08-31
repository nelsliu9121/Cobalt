use crate::model::{
    BannerComic, FeatureCollection, FeatureComic, Homepage, PublicDetail, ThemeCollection,
};
use kobo_sdk::{drawable_text_in, Face, LocalDay};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
const PARTIAL_FAILURE_WARNING: &str = "Some Featured collections could not be loaded.";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FeatureSource {
    Homepage,
    Ranking,
    MostFavorited,
    Themes,
    Freetime,
}

pub const FEATURE_SOURCES: [FeatureSource; 5] = [
    FeatureSource::Homepage,
    FeatureSource::Ranking,
    FeatureSource::MostFavorited,
    FeatureSource::Themes,
    FeatureSource::Freetime,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceResult {
    Homepage(Homepage),
    Collection {
        source: FeatureSource,
        comics: Vec<FeatureComic>,
    },
    Themes(Vec<ThemeCollection>),
    Failure(FeatureSource),
}

impl SourceResult {
    pub fn homepage(homepage: Homepage) -> Self {
        Self::Homepage(homepage)
    }

    pub fn collection(source: FeatureSource, comics: Vec<FeatureComic>) -> Self {
        Self::Collection { source, comics }
    }

    pub fn themes(themes: Vec<ThemeCollection>) -> Self {
        Self::Themes(themes)
    }

    pub const fn failure(source: FeatureSource) -> Self {
        Self::Failure(source)
    }

    const fn source(&self) -> FeatureSource {
        match self {
            Self::Homepage(_) => FeatureSource::Homepage,
            Self::Collection { source, .. } | Self::Failure(source) => *source,
            Self::Themes(_) => FeatureSource::Themes,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureSnapshot {
    pub banners: Vec<FeatureComic>,
    pub collections: Vec<FeatureCollection>,
    pub sources: BTreeMap<FeatureSource, Vec<FeatureCollection>>,
    pub failed_sources: BTreeSet<FeatureSource>,
    pub warning: Option<String>,
}

impl FeatureSnapshot {
    #[allow(dead_code, reason = "the grouped Feature UI consumes collections by stable id")]
    pub fn collection(&self, id: &str) -> Option<&FeatureCollection> {
        self.collections
            .iter()
            .find(|collection| collection.id == id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceStatus {
    Queued,
    Pending,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureBatch {
    pub generation: u64,
    pub refresh_day: Option<LocalDay>,
    pub retry_only: bool,
    pub statuses: BTreeMap<FeatureSource, SourceStatus>,
    pub queued: VecDeque<FeatureSource>,
    pub collections: BTreeMap<FeatureSource, Vec<FeatureCollection>>,
    pub banners: Vec<BannerComic>,
    pub pending_banner_aliases: VecDeque<String>,
    pub resolved_banners: BTreeMap<String, FeatureComic>,
}

impl FeatureBatch {
    pub fn settled(&self) -> bool {
        self.sources_settled() && self.pending_banner_aliases.is_empty()
    }

    fn sources_settled(&self) -> bool {
        self.statuses
            .values()
            .all(|status| matches!(status, SourceStatus::Ready | SourceStatus::Failed))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeaturedState {
    pub generation: u64,
    pub snapshot: Option<FeatureSnapshot>,
    pub batch: Option<FeatureBatch>,
    pub feed_page: usize,
    pub loaded_day: Option<LocalDay>,
    pub desired_day: Option<LocalDay>,
    pub local_day_pending: bool,
}

impl FeaturedState {
    pub fn snapshot(&self) -> Option<&FeatureSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn warning(&self) -> Option<&str> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.warning.as_deref())
            .or_else(|| {
                self.batch
                    .as_ref()
                    .is_some_and(|batch| {
                        batch.settled()
                            && batch
                                .statuses
                                .values()
                                .any(|status| *status == SourceStatus::Failed)
                    })
                    .then_some(PARTIAL_FAILURE_WARNING)
            })
    }

    pub fn begin_full_batch(&mut self, refresh_day: Option<LocalDay>) -> Vec<FeatureSource> {
        self.generation = self.generation.wrapping_add(1);
        self.feed_page = 0;
        if refresh_day.is_none() {
            self.desired_day = None;
        }
        self.batch = Some(FeatureBatch {
            generation: self.generation,
            refresh_day,
            retry_only: false,
            statuses: FEATURE_SOURCES
                .into_iter()
                .map(|source| (source, SourceStatus::Queued))
                .collect(),
            queued: FEATURE_SOURCES.into(),
            collections: BTreeMap::new(),
            banners: Vec::new(),
            pending_banner_aliases: VecDeque::new(),
            resolved_banners: BTreeMap::new(),
        });
        FEATURE_SOURCES.to_vec()
    }

    pub fn begin_failed_retry(&mut self) -> Vec<FeatureSource> {
        let (failed_sources, collections, banners, resolved_banners) =
            if let Some(snapshot) = &self.snapshot {
                (
                    snapshot.failed_sources.clone(),
                    snapshot.sources.clone(),
                    snapshot
                        .banners
                        .iter()
                        .map(|comic| BannerComic {
                            alias: comic.alias.clone(),
                        })
                        .collect(),
                    snapshot
                        .banners
                        .iter()
                        .map(|comic| (comic.alias.clone(), comic.clone()))
                        .collect(),
                )
            } else if let Some(batch) = &self.batch {
                (
                    failed_sources(batch),
                    batch.collections.clone(),
                    batch.banners.clone(),
                    batch.resolved_banners.clone(),
                )
            } else {
                return Vec::new();
            };
        if failed_sources.is_empty() {
            return Vec::new();
        }

        self.generation = self.generation.wrapping_add(1);
        self.feed_page = 0;
        let queued = FEATURE_SOURCES
            .into_iter()
            .filter(|source| failed_sources.contains(source))
            .collect::<VecDeque<_>>();
        let statuses = FEATURE_SOURCES
            .into_iter()
            .map(|source| {
                let status = if failed_sources.contains(&source) {
                    SourceStatus::Queued
                } else {
                    SourceStatus::Ready
                };
                (source, status)
            })
            .collect();
        let retry_sources = queued.iter().copied().collect();
        self.batch = Some(FeatureBatch {
            generation: self.generation,
            refresh_day: self.loaded_day,
            retry_only: true,
            statuses,
            queued,
            collections,
            banners,
            pending_banner_aliases: VecDeque::new(),
            resolved_banners,
        });
        retry_sources
    }

    pub fn observe_day(&mut self, day: LocalDay) -> bool {
        if let Some(batch) = self
            .batch
            .as_mut()
            .filter(|batch| !batch.settled())
        {
            if batch.refresh_day.is_none() {
                batch.refresh_day = Some(day);
            } else if batch.refresh_day != Some(day) {
                self.desired_day = Some(day);
            }
            return false;
        }
        if self.loaded_day == Some(day) {
            if self.desired_day == Some(day) {
                self.desired_day = None;
            }
            return false;
        }
        self.desired_day = None;
        self.begin_full_batch(Some(day));
        true
    }

    pub fn queued_source(&self) -> Option<FeatureSource> {
        self.batch
            .as_ref()
            .and_then(|batch| batch.queued.front().copied())
    }

    pub fn mark_source_pending(
        &mut self,
        generation: u64,
        source: FeatureSource,
    ) -> bool {
        let Some(batch) = self
            .batch
            .as_mut()
            .filter(|batch| batch.generation == generation)
        else {
            return false;
        };
        if batch.queued.front() != Some(&source)
            || batch.statuses.get(&source) != Some(&SourceStatus::Queued)
        {
            return false;
        }
        batch.queued.pop_front();
        batch.statuses.insert(source, SourceStatus::Pending);
        true
    }

    #[allow(dead_code, reason = "the pure state API settles the active generation")]
    pub fn settle(&mut self, result: SourceResult) -> bool {
        self.settle_generation(self.generation, result)
    }

    pub fn settle_generation(&mut self, generation: u64, result: SourceResult) -> bool {
        if matches!(
            &result,
            SourceResult::Collection {
                source: FeatureSource::Homepage | FeatureSource::Themes,
                ..
            }
        ) {
            return false;
        }
        let source = result.source();
        let Some(batch) = self
            .batch
            .as_mut()
            .filter(|batch| batch.generation == generation)
        else {
            return false;
        };
        if !matches!(
            batch.statuses.get(&source),
            Some(SourceStatus::Queued | SourceStatus::Pending)
        ) {
            return false;
        }
        if let Some(index) = batch.queued.iter().position(|queued| *queued == source) {
            batch.queued.remove(index);
        }

        match result {
            SourceResult::Homepage(homepage) => {
                batch.banners = homepage.banners.iter().take(3).cloned().collect();
                batch
                    .collections
                    .insert(source, homepage_collections(homepage));
                batch.statuses.insert(source, SourceStatus::Ready);
            }
            SourceResult::Collection { source, comics } => {
                let Some(collections) = source_collection(source, comics) else {
                    return false;
                };
                batch.collections.insert(source, collections);
                batch.statuses.insert(source, SourceStatus::Ready);
            }
            SourceResult::Themes(themes) => {
                batch.collections.insert(source, theme_collections(themes));
                batch.statuses.insert(source, SourceStatus::Ready);
            }
            SourceResult::Failure(source) => {
                batch.collections.remove(&source);
                batch.statuses.insert(source, SourceStatus::Failed);
            }
        }
        self.prepare_banner_details();
        true
    }

    #[allow(dead_code, reason = "the pure state API exposes the next detail slot")]
    pub fn next_banner_alias(&self) -> Option<String> {
        self.batch
            .as_ref()
            .and_then(|batch| batch.pending_banner_aliases.front().cloned())
    }

    pub fn pending_banner_aliases(&self) -> impl Iterator<Item = &str> {
        self.batch.iter().flat_map(|batch| {
            batch
                .pending_banner_aliases
                .iter()
                .map(String::as_str)
        })
    }

    #[allow(dead_code, reason = "the pure state API settles the active generation")]
    pub fn settle_banner_detail(
        &mut self,
        alias: &str,
        detail: Option<PublicDetail>,
    ) -> bool {
        self.settle_banner_detail_generation(self.generation, alias, detail)
    }

    pub fn settle_banner_detail_generation(
        &mut self,
        generation: u64,
        alias: &str,
        detail: Option<PublicDetail>,
    ) -> bool {
        let Some(batch) = self
            .batch
            .as_mut()
            .filter(|batch| batch.generation == generation)
        else {
            return false;
        };
        let Some(index) = batch
            .pending_banner_aliases
            .iter()
            .position(|pending| pending == alias)
        else {
            return false;
        };
        batch.pending_banner_aliases.remove(index);
        let comic = detail.map_or_else(
            || banner_placeholder(alias),
            |detail| FeatureComic {
                alias: detail.alias,
                title: detail.title,
                creators: String::new(),
                view_count: None,
                vertical_url: None,
                square_url: None,
            },
        );
        batch.resolved_banners.insert(alias.to_owned(), comic);
        true
    }

    pub fn publish_ready_banner_details(&mut self) -> Option<&FeatureSnapshot> {
        self.prepare_banner_details();
        let ready = self
            .batch
            .as_ref()
            .is_some_and(FeatureBatch::settled);
        if !ready {
            return None;
        }
        let batch = self.batch.take().expect("checked settled Feature batch");
        let failures = failed_sources(&batch);
        let mut collections = batch
            .collections
            .values()
            .flat_map(|collections| collections.iter().cloned())
            .collect::<Vec<_>>();
        collections.sort_by_key(|collection| (collection.priority, collection.order));

        if collections.is_empty() && !failures.is_empty() {
            self.batch = Some(batch);
            self.start_desired_day_after_settlement();
            return self.snapshot.as_ref();
        }

        let banners = batch
            .banners
            .iter()
            .take(3)
            .map(|banner| {
                batch
                    .resolved_banners
                    .get(&banner.alias)
                    .cloned()
                    .unwrap_or_else(|| banner_placeholder(&banner.alias))
            })
            .collect();
        let warning = (!failures.is_empty()).then(|| PARTIAL_FAILURE_WARNING.to_owned());
        if let Some(day) = batch.refresh_day {
            self.loaded_day = Some(day);
        }
        self.feed_page = 0;
        self.snapshot = Some(FeatureSnapshot {
            banners,
            collections,
            sources: batch.collections,
            failed_sources: failures,
            warning,
        });
        self.start_desired_day_after_settlement();
        self.snapshot.as_ref()
    }

    pub fn is_loading(&self) -> bool {
        self.snapshot.is_none()
            && self
                .batch
                .as_ref()
                .is_some_and(|batch| !batch.settled())
    }

    pub fn is_failed(&self) -> bool {
        self.snapshot.is_none()
            && self.batch.as_ref().is_some_and(|batch| {
                batch.settled()
                    && batch
                        .statuses
                        .values()
                        .any(|status| *status == SourceStatus::Failed)
            })
    }

    pub fn has_failed_sources(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.failed_sources.is_empty())
            || self.batch.as_ref().is_some_and(|batch| {
                batch
                    .statuses
                    .values()
                    .any(|status| *status == SourceStatus::Failed)
            })
    }

    fn prepare_banner_details(&mut self) {
        let Some(batch) = self
            .batch
            .as_mut()
            .filter(|batch| batch.sources_settled())
        else {
            return;
        };
        for banner in batch.banners.iter().take(3) {
            if batch.resolved_banners.contains_key(&banner.alias)
                || batch
                    .pending_banner_aliases
                    .iter()
                    .any(|pending| pending == &banner.alias)
            {
                continue;
            }
            let matching = batch
                .collections
                .values()
                .flat_map(|collections| collections.iter())
                .flat_map(|collection| collection.comics.iter())
                .find(|comic| comic.alias == banner.alias)
                .cloned();
            if let Some(comic) = matching {
                batch.resolved_banners.insert(banner.alias.clone(), comic);
            } else {
                batch
                    .pending_banner_aliases
                    .push_back(banner.alias.clone());
            }
        }
    }

    fn start_desired_day_after_settlement(&mut self) {
        let Some(day) = self.desired_day.take() else {
            return;
        };
        if self.loaded_day != Some(day) {
            self.begin_full_batch(Some(day));
        }
    }
}

fn failed_sources(batch: &FeatureBatch) -> BTreeSet<FeatureSource> {
    batch
        .statuses
        .iter()
        .filter_map(|(source, status)| (*status == SourceStatus::Failed).then_some(*source))
        .collect()
}

fn banner_placeholder(alias: &str) -> FeatureComic {
    FeatureComic {
        alias: alias.to_owned(),
        title: alias.to_owned(),
        creators: String::new(),
        view_count: None,
        vertical_url: None,
        square_url: None,
    }
}

fn homepage_collections(homepage: Homepage) -> Vec<FeatureCollection> {
    [
        ("newest", "人氣新作", 2, homepage.newest),
        ("weekday", "連載作品", 3, homepage.week_day),
        ("only-in-bomtoon", "只在 Bomtoon", 8, homepage.only_bom),
    ]
    .into_iter()
    .filter(|(_, _, _, comics)| !comics.is_empty())
    .enumerate()
    .map(
        |(order, (id, label, priority, comics))| FeatureCollection {
            id: id.to_owned(),
            label: label.to_owned(),
            priority,
            order,
            comics,
        },
    )
    .collect()
}

fn source_collection(
    source: FeatureSource,
    comics: Vec<FeatureComic>,
) -> Option<Vec<FeatureCollection>> {
    let (id, label, priority) = match source {
        FeatureSource::Ranking => ("ranking", "排行榜", 5),
        FeatureSource::MostFavorited => ("most-favorited", "最多人收藏", 7),
        FeatureSource::Freetime => ("freetime", "免費看", 10),
        FeatureSource::Homepage | FeatureSource::Themes => return None,
    };
    Some(
        (!comics.is_empty())
            .then(|| {
                vec![FeatureCollection {
                    id: id.to_owned(),
                    label: label.to_owned(),
                    priority,
                    order: 0,
                    comics,
                }]
            })
            .unwrap_or_default(),
    )
}

fn theme_collections(themes: Vec<ThemeCollection>) -> Vec<FeatureCollection> {
    themes
        .into_iter()
        .enumerate()
        .filter_map(|(order, theme)| {
            let label = drawable_text_in(&theme.label, Face::Text);
            (!label.trim().is_empty() && !theme.comics.is_empty()).then(|| FeatureCollection {
                id: format!("theme-{}", theme.id),
                label,
                priority: 9,
                order,
                comics: theme.comics,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BannerComic, FeatureComic, Homepage, ThemeCollection};
    use kobo_sdk::LocalDay;
    use std::collections::BTreeSet;

    fn comic(alias: &str) -> FeatureComic {
        FeatureComic {
            alias: alias.to_owned(),
            title: format!("Title {alias}"),
            creators: "Creator".to_owned(),
            view_count: Some(1),
            vertical_url: Some(format!("https://image.balcony.studio/tw/contents/{alias}.webp")),
            square_url: None,
        }
    }

    fn comics(prefix: &str, count: usize) -> Vec<FeatureComic> {
        (0..count)
            .map(|index| comic(&format!("{prefix}-{index}")))
            .collect()
    }

    fn homepage_fixture() -> Homepage {
        Homepage {
            banners: ["favorite-0", "theme-0", "missing"]
                .into_iter()
                .map(|alias| BannerComic {
                    alias: alias.to_owned(),
                })
                .collect(),
            newest: comics("new", 2),
            week_day: comics("weekday", 2),
            only_bom: comics("only", 2),
        }
    }

    fn theme_fixture() -> Vec<ThemeCollection> {
        vec![ThemeCollection {
            id: 1785,
            label: "Theme Choice".to_owned(),
            comics: comics("theme", 2),
        }]
    }

    fn settle_all_sources(state: &mut FeaturedState, results: Vec<SourceResult>) {
        for result in results {
            assert!(state.settle(result));
        }
        while let Some(alias) = state.next_banner_alias() {
            assert!(state.settle_banner_detail(&alias, None));
        }
        state.publish_ready_banner_details();
    }

    fn successful_results(prefix: &str) -> Vec<SourceResult> {
        let mut homepage = homepage_fixture();
        homepage.banners.clear();
        homepage.newest = comics(&format!("{prefix}-new"), 1);
        homepage.week_day.clear();
        homepage.only_bom.clear();
        vec![
            SourceResult::homepage(homepage),
            SourceResult::collection(FeatureSource::Ranking, comics(&format!("{prefix}-rank"), 1)),
            SourceResult::collection(
                FeatureSource::MostFavorited,
                comics(&format!("{prefix}-favorite"), 1),
            ),
            SourceResult::themes(Vec::new()),
            SourceResult::collection(FeatureSource::Freetime, Vec::new()),
        ]
    }

    fn state_with_partial_snapshot() -> FeaturedState {
        let mut state = FeaturedState::default();
        state.begin_full_batch(None);
        state.settle(SourceResult::homepage(homepage_fixture()));
        state.settle(SourceResult::failure(FeatureSource::Ranking));
        state.settle(SourceResult::collection(
            FeatureSource::MostFavorited,
            comics("favorite", 2),
        ));
        state.settle(SourceResult::themes(theme_fixture()));
        state.settle(SourceResult::collection(
            FeatureSource::Freetime,
            comics("free", 2),
        ));
        while let Some(alias) = state.next_banner_alias() {
            state.settle_banner_detail(&alias, None);
        }
        state.publish_ready_banner_details();
        state
    }

    #[test]
    fn explicit_undated_full_batch_discards_an_obsolete_desired_day() {
        let mut state = FeaturedState {
            desired_day: Some(LocalDay::new(2026, 8, 31).expect("day")),
            ..FeaturedState::default()
        };

        state.begin_full_batch(None);

        assert_eq!(state.desired_day, None);
    }

    #[test]
    fn source_batch_publishes_successful_non_empty_groups_after_every_source_settles() {
        let mut state = FeaturedState::default();
        assert_eq!(state.begin_full_batch(None), FEATURE_SOURCES.to_vec());
        state.settle(SourceResult::homepage(homepage_fixture()));
        state.settle(SourceResult::failure(FeatureSource::Ranking));
        state.settle(SourceResult::collection(
            FeatureSource::MostFavorited,
            comics("favorite", 2),
        ));
        state.settle(SourceResult::themes(theme_fixture()));
        assert!(state.snapshot().is_none());
        state.settle(SourceResult::collection(
            FeatureSource::Freetime,
            comics("free", 2),
        ));
        assert_eq!(state.next_banner_alias().as_deref(), Some("missing"));
        state.settle_banner_detail("missing", None);
        let snapshot = state.publish_ready_banner_details().expect("snapshot");
        assert_eq!(
            snapshot
                .collections
                .iter()
                .map(|group| group.id.as_str())
                .collect::<Vec<_>>(),
            [
                "newest",
                "weekday",
                "most-favorited",
                "only-in-bomtoon",
                "theme-1785",
                "freetime",
            ]
        );
        assert_eq!(
            snapshot.failed_sources,
            BTreeSet::from([FeatureSource::Ranking])
        );
        assert!(snapshot.warning.is_some());
    }

    #[test]
    fn failed_source_retry_keeps_snapshot_and_replaces_only_failed_slots() {
        let mut state = state_with_partial_snapshot();
        let before = state.snapshot().expect("snapshot").clone();
        assert_eq!(state.begin_failed_retry(), vec![FeatureSource::Ranking]);
        assert_eq!(state.snapshot(), Some(&before));
        state.settle(SourceResult::collection(
            FeatureSource::Ranking,
            comics("rank", 2),
        ));
        let after = state.publish_ready_banner_details().expect("snapshot");
        assert!(after.collection("most-favorited").is_some());
        assert!(after.collection("ranking").is_some());
        assert!(after.failed_sources.is_empty());
    }

    #[test]
    fn first_observed_day_labels_an_undated_full_batch_without_duplicate_refresh() {
        let day = LocalDay::new(2026, 8, 31).expect("day");
        let mut state = FeaturedState {
            loaded_day: Some(day),
            ..FeaturedState::default()
        };
        state.begin_full_batch(None);

        assert!(!state.observe_day(day));
        assert_eq!(
            state.batch.as_ref().and_then(|batch| batch.refresh_day),
            Some(day)
        );
        assert_eq!(state.desired_day, None);
    }

    #[test]
    fn daily_refresh_retains_old_snapshot_until_the_new_batch_is_atomic() {
        let day = LocalDay::new(2026, 8, 31).expect("day");
        let mut state = FeaturedState::default();
        state.begin_full_batch(Some(day));
        settle_all_sources(&mut state, successful_results("old"));
        let before = state.snapshot().expect("snapshot").clone();

        let next_day = LocalDay::new(2026, 9, 1).expect("day");
        assert!(state.observe_day(next_day));
        assert_eq!(state.snapshot(), Some(&before));
        let results = successful_results("fresh");
        for result in results.into_iter().take(4) {
            state.settle(result);
            assert_eq!(state.snapshot(), Some(&before));
        }
        state.settle(SourceResult::collection(FeatureSource::Freetime, Vec::new()));
        state.publish_ready_banner_details();
        assert_ne!(state.snapshot(), Some(&before));
        assert_eq!(state.loaded_day, Some(next_day));
    }

    #[test]
    fn all_sources_fail_without_committing_an_empty_snapshot_and_are_retryable() {
        let mut state = FeaturedState::default();
        state.begin_full_batch(None);
        for source in FEATURE_SOURCES {
            state.settle(SourceResult::failure(source));
        }
        assert!(state.publish_ready_banner_details().is_none());
        assert!(state.snapshot().is_none());
        assert!(state.is_failed());
        assert_eq!(state.begin_failed_retry(), FEATURE_SOURCES.to_vec());
    }

    #[test]
    fn failed_refresh_keeps_the_committed_snapshot_visible_with_retry_warning() {
        let mut state = FeaturedState::default();
        state.begin_full_batch(None);
        settle_all_sources(&mut state, successful_results("old"));
        let before = state.snapshot().expect("snapshot").clone();

        state.begin_full_batch(None);
        for source in FEATURE_SOURCES {
            state.settle(SourceResult::failure(source));
        }
        state.publish_ready_banner_details();

        assert_eq!(state.snapshot(), Some(&before));
        assert_eq!(
            state.warning(),
            Some("Some Featured collections could not be loaded.")
        );
        assert!(state.batch.as_ref().is_some_and(FeatureBatch::settled));
    }

    #[test]
    fn successful_empty_sources_are_omitted_but_not_retryable_failures() {
        let mut state = FeaturedState::default();
        state.begin_full_batch(None);
        settle_all_sources(&mut state, successful_results("empty"));
        let snapshot = state.snapshot().expect("snapshot");
        assert!(snapshot.collection("weekday").is_none());
        assert!(snapshot.collection("freetime").is_none());
        assert!(snapshot.sources.contains_key(&FeatureSource::Freetime));
        assert!(snapshot.failed_sources.is_empty());
    }

    #[test]
    fn duplicate_aliases_remain_in_every_collection_placement() {
        let shared = comic("shared");
        let mut homepage = homepage_fixture();
        homepage.banners.clear();
        homepage.newest = vec![shared.clone(), shared.clone()];
        homepage.week_day = vec![shared.clone()];
        homepage.only_bom.clear();
        let mut state = FeaturedState::default();
        state.begin_full_batch(None);
        settle_all_sources(
            &mut state,
            vec![
                SourceResult::homepage(homepage),
                SourceResult::collection(FeatureSource::Ranking, vec![shared.clone()]),
                SourceResult::collection(FeatureSource::MostFavorited, vec![shared.clone()]),
                SourceResult::themes(Vec::new()),
                SourceResult::collection(FeatureSource::Freetime, vec![shared]),
            ],
        );
        let snapshot = state.snapshot().expect("snapshot");
        assert_eq!(snapshot.collection("newest").expect("newest").comics.len(), 2);
        for id in ["weekday", "ranking", "most-favorited", "freetime"] {
            assert_eq!(snapshot.collection(id).expect(id).comics.len(), 1);
        }
    }

    #[test]
    fn themes_share_priority_nine_and_preserve_response_order() {
        let mut state = FeaturedState::default();
        state.begin_full_batch(None);
        settle_all_sources(
            &mut state,
            vec![
                SourceResult::homepage(Homepage {
                    banners: Vec::new(),
                    newest: Vec::new(),
                    week_day: Vec::new(),
                    only_bom: Vec::new(),
                }),
                SourceResult::collection(FeatureSource::Ranking, Vec::new()),
                SourceResult::collection(FeatureSource::MostFavorited, Vec::new()),
                SourceResult::themes(vec![
                    ThemeCollection {
                        id: 20,
                        label: "First".to_owned(),
                        comics: comics("first", 1),
                    },
                    ThemeCollection {
                        id: 10,
                        label: "Second".to_owned(),
                        comics: comics("second", 1),
                    },
                ]),
                SourceResult::collection(FeatureSource::Freetime, Vec::new()),
            ],
        );
        let themes = state
            .snapshot()
            .expect("snapshot")
            .collections
            .iter()
            .filter(|group| group.priority == 9)
            .collect::<Vec<_>>();
        assert_eq!(themes.iter().map(|group| group.id.as_str()).collect::<Vec<_>>(), ["theme-20", "theme-10"]);
        assert_eq!(themes.iter().map(|group| group.order).collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn stale_or_already_settled_generation_is_a_no_op() {
        let mut state = FeaturedState::default();
        state.begin_full_batch(None);
        let generation = state.generation;
        let before = state.clone();
        assert!(!state.settle_generation(
            generation.wrapping_sub(1),
            SourceResult::failure(FeatureSource::Homepage),
        ));
        assert_eq!(state, before);
        assert!(state.settle_generation(
            generation,
            SourceResult::failure(FeatureSource::Homepage),
        ));
        let settled = state.clone();
        assert!(!state.settle_generation(
            generation,
            SourceResult::failure(FeatureSource::Homepage),
        ));
        assert_eq!(state, settled);
    }

    #[test]
    fn unresolved_banner_detail_never_supplies_artwork_and_failure_uses_placeholder() {
        let mut homepage = homepage_fixture();
        homepage.banners = vec![BannerComic {
            alias: "missing".to_owned(),
        }];
        let mut state = FeaturedState::default();
        state.begin_full_batch(None);
        state.settle(SourceResult::homepage(homepage));
        for source in [
            FeatureSource::Ranking,
            FeatureSource::MostFavorited,
            FeatureSource::Themes,
            FeatureSource::Freetime,
        ] {
            state.settle(SourceResult::failure(source));
        }
        assert_eq!(state.next_banner_alias().as_deref(), Some("missing"));
        state.settle_banner_detail(
            "missing",
            Some(crate::model::PublicDetail {
                alias: "missing".to_owned(),
                title: "Recovered".to_owned(),
                synopsis: Some("Synopsis".to_owned()),
            }),
        );
        let recovered = &state.publish_ready_banner_details().expect("snapshot").banners[0];
        assert_eq!(recovered.title, "Recovered");
        assert!(recovered.creators.is_empty());
        assert_eq!(recovered.vertical_url, None);
        assert_eq!(recovered.square_url, None);

        state.begin_full_batch(None);
        state.settle(SourceResult::homepage(Homepage {
            banners: vec![BannerComic { alias: "glyph".to_owned() }],
            newest: comics("new", 1),
            week_day: Vec::new(),
            only_bom: Vec::new(),
        }));
        for source in [FeatureSource::Ranking, FeatureSource::MostFavorited, FeatureSource::Themes, FeatureSource::Freetime] {
            state.settle(SourceResult::failure(source));
        }
        state.settle_banner_detail("glyph", None);
        let placeholder = &state.publish_ready_banner_details().expect("snapshot").banners[0];
        assert_eq!(placeholder.title, "glyph");
        assert_eq!(placeholder.vertical_url, None);
    }

    #[test]
    fn mismatched_collection_result_does_not_advance_the_source_slot() {
        let mut state = FeaturedState::default();
        state.begin_full_batch(None);
        let before = state.batch.clone();

        assert!(!state.settle(SourceResult::collection(
            FeatureSource::Homepage,
            comics("invalid", 1),
        )));
        assert_eq!(state.batch, before);
    }

    #[test]
    fn banner_alias_found_in_any_successful_collection_avoids_detail_fetch() {
        let mut homepage = homepage_fixture();
        homepage.banners = vec![BannerComic {
            alias: "rank-0".to_owned(),
        }];
        let mut state = FeaturedState::default();
        state.begin_full_batch(None);
        settle_all_sources(
            &mut state,
            vec![
                SourceResult::homepage(homepage),
                SourceResult::collection(FeatureSource::Ranking, comics("rank", 1)),
                SourceResult::collection(FeatureSource::MostFavorited, Vec::new()),
                SourceResult::themes(Vec::new()),
                SourceResult::collection(FeatureSource::Freetime, Vec::new()),
            ],
        );
        assert_eq!(state.next_banner_alias(), None);
        let banner = &state.snapshot().expect("snapshot").banners[0];
        assert_eq!(banner.alias, "rank-0");
        assert!(banner.vertical_url.is_some());
    }
}
