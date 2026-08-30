#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Comic {
    pub alias: String,
    pub title: String,
    pub cover_url: Option<String>,
    pub owned_episodes: usize,
    pub total_episodes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShelfComic {
    pub alias: String,
    pub title: String,
    pub cover_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BannerComic {
    pub alias: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Homepage {
    pub banners: Vec<BannerComic>,
    pub newest: Vec<ShelfComic>,
    pub week_day: Vec<ShelfComic>,
    pub only_bom: Vec<ShelfComic>,
}

const HOUR_MS: i64 = 60 * 60 * 1_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentDetail {
    pub id: usize,
    pub episodes: Vec<Episode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Episode {
    pub id: usize,
    pub alias: String,
    pub title: String,
    pub purchase: PurchaseState,
    pub rent_expires_at: Option<i64>,
    pub rent_coin: Option<usize>,
    pub purchase_coin: Option<usize>,
    pub gift_eligible: bool,
}

impl Episode {
    #[must_use]
    pub fn remaining_rental_hours(&self, now_ms: i64) -> Option<usize> {
        if self.purchase != PurchaseState::Rented {
            return None;
        }
        let expiry = self.rent_expires_at?;
        let remaining = expiry.saturating_sub(now_ms).max(0);
        let hours = remaining / HOUR_MS + i64::from(remaining % HOUR_MS != 0);
        usize::try_from(hours).ok()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpisodeImage {
    pub order: usize,
    pub width: u32,
    pub height: u32,
    pub path: String,
    pub url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentEntry {
    pub content_alias: String,
    pub content_title: String,
    pub cover_url: Option<String>,
    pub episode_alias: String,
    pub episode_title: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetKind {
    Coin,
    Ticket,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetSubtype {
    Standard,
    Bonus,
    Free,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AssetAmounts {
    pub standard: usize,
    pub bonus: usize,
    pub free: usize,
}

impl AssetAmounts {
    pub fn total(self) -> Option<usize> {
        self.standard
            .checked_add(self.bonus)?
            .checked_add(self.free)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalletSummary {
    pub coins: AssetAmounts,
    pub tickets: AssetAmounts,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpirationRow {
    pub kind: AssetKind,
    pub subtype: AssetSubtype,
    pub quantity: usize,
    pub expires_at: Option<i64>,
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EpisodeAvailability<'a> {
    pub status: Option<&'a str>,
    pub episode_type: Option<&'a str>,
    pub is_sample: bool,
    pub paid: Option<bool>,
    pub possession_coin: Option<usize>,
    pub rent_coin: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PurchaseState {
    Owned,
    Rented,
    Sample,
    Free,
    NotOwned,
    Other(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PurchaseType {
    RentGift,
    Rent,
    Possession,
}

impl PurchaseType {
    #[must_use]
    pub const fn as_remote(self) -> &'static str {
        match self {
            Self::RentGift => "RENT_GIFT",
            Self::Rent => "RENT",
            Self::Possession => "POSSESSION",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GiftBalance {
    pub available: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Quote {
    pub content_id: usize,
    pub episode_id: usize,
    pub content_alias: String,
    pub episode_alias: String,
    pub is_available: bool,
    pub coin_kind: String,
    pub rent_coin: usize,
    pub possession_coin: usize,
    pub permanent_coin: Option<usize>,
    pub is_rent_gift: bool,
    pub is_possession_gift: bool,
}

impl Quote {
    #[must_use]
    pub fn rent_price(&self) -> Option<usize> {
        (self.coin_kind == "COIN").then_some(self.rent_coin)
    }

    #[must_use]
    pub fn purchase_price(&self) -> Option<usize> {
        if self.coin_kind != "COIN"
            || self
                .permanent_coin
                .is_some_and(|coin| coin != self.possession_coin)
        {
            None
        } else {
            Some(self.possession_coin)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoinUse {
    pub aggregate: usize,
    pub standard: usize,
    pub bonus: usize,
    pub free: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PurchaseReceipt {
    pub purchase_type: PurchaseType,
    pub content_alias: String,
    pub episode_alias: String,
    pub coin_use: CoinUse,
}

pub fn display_text(text: &str, fallback: &str) -> String {
    if !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte == b' ' || byte.is_ascii_graphic())
    {
        text.to_owned()
    } else {
        fallback.to_owned()
    }
}

impl PurchaseState {
    pub fn from_remote(availability: EpisodeAvailability<'_>) -> Self {
        match availability.status {
            Some("POSSESSION") => Self::Owned,
            Some("RENT") => Self::Rented,
            Some(status) if !status.is_empty() && status != "NONE" => {
                Self::Other(status.to_owned())
            }
            _ if availability.episode_type == Some("PREVIEW") || availability.is_sample => {
                Self::Sample
            }
            _ if availability.possession_coin == Some(0)
                || availability.rent_coin == Some(0)
                || availability.paid == Some(false) =>
            {
                Self::Free
            }
            _ => Self::NotOwned,
        }
    }

    #[must_use]
    pub const fn is_readable(&self) -> bool {
        matches!(self, Self::Owned | Self::Rented | Self::Sample | Self::Free)
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Owned => "Owned",
            Self::Rented => "Rented",
            Self::Sample => "Free sample",
            Self::Free => "Free",
            Self::NotOwned => "Not owned",
            Self::Other(status) => status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        display_text, AssetAmounts, Episode, EpisodeAvailability, PurchaseState, PurchaseType, Quote,
    };

    #[test]
    fn asset_amounts_total_is_checked() {
        assert_eq!(
            AssetAmounts {
                standard: 7,
                bonus: 2,
                free: 1,
            }
            .total(),
            Some(10)
        );
        assert_eq!(
            AssetAmounts {
                standard: usize::MAX,
                bonus: 1,
                free: 0,
            }
            .total(),
            None
        );
    }

    const HOUR_MS: i64 = 60 * 60 * 1_000;

    fn rented(rent_expires_at: Option<i64>) -> Episode {
        Episode {
            id: 17,
            alias: "episode-17".to_owned(),
            title: "Episode 17".to_owned(),
            purchase: PurchaseState::Rented,
            rent_expires_at,
            rent_coin: Some(2),
            purchase_coin: Some(3),
            gift_eligible: true,
        }
    }

    #[test]
    fn remaining_rental_hours_use_a_whole_hour_ceiling() {
        assert_eq!(rented(Some(HOUR_MS)).remaining_rental_hours(0), Some(1));
        assert_eq!(
            rented(Some(HOUR_MS + 1)).remaining_rental_hours(0),
            Some(2)
        );
        assert_eq!(
            rented(Some(48 * HOUR_MS)).remaining_rental_hours(0),
            Some(48)
        );
    }

    #[test]
    fn elapsed_and_missing_rentals_have_explicit_remaining_time() {
        assert_eq!(rented(Some(HOUR_MS)).remaining_rental_hours(HOUR_MS), Some(0));
        assert_eq!(
            rented(Some(HOUR_MS)).remaining_rental_hours(2 * HOUR_MS),
            Some(0)
        );
        assert_eq!(rented(None).remaining_rental_hours(0), None);
    }

    #[test]
    fn purchase_state_uses_live_availability_with_fail_closed_precedence() {
        let cases = [
            (
                "sample",
                EpisodeAvailability {
                    status: Some("NONE"),
                    episode_type: Some("PREVIEW"),
                    is_sample: false,
                    paid: None,
                    possession_coin: Some(0),
                    rent_coin: Some(0),
                },
                PurchaseState::Sample,
            ),
            (
                "free",
                EpisodeAvailability {
                    status: Some("NONE"),
                    episode_type: Some("GENERAL"),
                    is_sample: false,
                    paid: None,
                    possession_coin: Some(0),
                    rent_coin: Some(0),
                },
                PurchaseState::Free,
            ),
            (
                "not owned",
                EpisodeAvailability {
                    status: Some("NONE"),
                    episode_type: Some("GENERAL"),
                    is_sample: false,
                    paid: None,
                    possession_coin: Some(3),
                    rent_coin: Some(2),
                },
                PurchaseState::NotOwned,
            ),
            (
                "rent precedence",
                EpisodeAvailability {
                    status: Some("RENT"),
                    episode_type: Some("PREVIEW"),
                    is_sample: true,
                    paid: Some(false),
                    possession_coin: Some(0),
                    rent_coin: Some(0),
                },
                PurchaseState::Rented,
            ),
            (
                "possession precedence",
                EpisodeAvailability {
                    status: Some("POSSESSION"),
                    episode_type: Some("PREVIEW"),
                    is_sample: true,
                    paid: Some(false),
                    possession_coin: Some(0),
                    rent_coin: Some(0),
                },
                PurchaseState::Owned,
            ),
            (
                "unknown status",
                EpisodeAvailability {
                    status: Some("FUTURE"),
                    episode_type: Some("PREVIEW"),
                    is_sample: true,
                    paid: Some(false),
                    possession_coin: Some(0),
                    rent_coin: Some(0),
                },
                PurchaseState::Other("FUTURE".to_owned()),
            ),
        ];

        for (name, availability, expected) in cases {
            assert_eq!(PurchaseState::from_remote(availability), expected, "{name}");
        }
    }

    #[test]
    fn server_granted_and_free_episode_states_are_readable() {
        assert!(PurchaseState::Owned.is_readable());
        assert!(PurchaseState::Rented.is_readable());
        assert!(PurchaseState::Sample.is_readable());
        assert!(PurchaseState::Free.is_readable());
        assert!(!PurchaseState::NotOwned.is_readable());
        assert!(!PurchaseState::Other("FUTURE".to_owned()).is_readable());
    }

    #[test]
    fn purchase_types_have_exact_remote_values() {
        assert_eq!(PurchaseType::RentGift.as_remote(), "RENT_GIFT");
        assert_eq!(PurchaseType::Rent.as_remote(), "RENT");
        assert_eq!(PurchaseType::Possession.as_remote(), "POSSESSION");
    }

    #[test]
    fn quote_prices_require_coin_and_conflicts_disable_only_purchase() {
        let mut quote = Quote {
            content_id: 41,
            episode_id: 17,
            content_alias: "comic-41".to_owned(),
            episode_alias: "episode-17".to_owned(),
            is_available: true,
            coin_kind: "COIN".to_owned(),
            rent_coin: 2,
            possession_coin: 3,
            permanent_coin: Some(3),
            is_rent_gift: true,
            is_possession_gift: false,
        };
        assert_eq!(quote.rent_price(), Some(2));
        assert_eq!(quote.purchase_price(), Some(3));

        quote.permanent_coin = Some(4);
        assert_eq!(quote.rent_price(), Some(2));
        assert_eq!(quote.purchase_price(), None);

        quote.coin_kind = "TICKET".to_owned();
        assert_eq!(quote.rent_price(), None);
        assert_eq!(quote.purchase_price(), None);
    }

    #[test]
    fn remote_text_with_unsupported_glyphs_uses_the_fallback() {
        assert_eq!(display_text("近似嚮導", "Title hunter_q"), "Title hunter_q");
        assert_eq!(display_text("Dinner", "Title 365"), "Dinner");
    }
}
