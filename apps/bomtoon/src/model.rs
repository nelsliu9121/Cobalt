#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Comic {
    pub alias: String,
    pub title: String,
    pub owned_episodes: usize,
    pub total_episodes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Episode {
    pub alias: String,
    pub title: String,
    pub purchase: PurchaseState,
    pub ticket_quantity: Option<usize>,
}

impl Episode {
    #[must_use]
    pub const fn uses_ticket(&self) -> bool {
        self.ticket_quantity.is_some()
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
    Sample,
    Free,
    NotOwned,
    Other(String),
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
        if availability.status == Some("POSSESSION") {
            Self::Owned
        } else if availability.episode_type == Some("PREVIEW") || availability.is_sample {
            Self::Sample
        } else if availability.possession_coin == Some(0)
            || availability.rent_coin == Some(0)
            || availability.paid == Some(false)
        {
            Self::Free
        } else if availability.status.is_none() || availability.status == Some("NONE") {
            Self::NotOwned
        } else {
            Self::Other(availability.status.unwrap_or_default().to_owned())
        }
    }

    #[must_use]
    pub const fn is_readable(&self) -> bool {
        matches!(self, Self::Owned | Self::Sample | Self::Free)
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Owned => "Owned",
            Self::Sample => "Free sample",
            Self::Free => "Free",
            Self::NotOwned => "Not owned",
            Self::Other(status) => status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{display_text, AssetAmounts, Episode, EpisodeAvailability, PurchaseState};

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

    #[test]
    fn only_episodes_with_a_ticket_quantity_use_tickets() {
        let ticket = Episode {
            alias: "ticket".to_owned(),
            title: "Ticket episode".to_owned(),
            purchase: PurchaseState::NotOwned,
            ticket_quantity: Some(1),
        };
        let coin = Episode {
            alias: "coin".to_owned(),
            title: "Coin episode".to_owned(),
            purchase: PurchaseState::NotOwned,
            ticket_quantity: None,
        };
        assert!(ticket.uses_ticket());
        assert!(!coin.uses_ticket());
    }

    #[test]
    fn purchase_state_uses_live_availability_with_fail_closed_precedence() {
        let cases = [
            (
                "hunter f1",
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
                "hunter episode 1",
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
                "hunter episode 2",
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
                "owned precedence",
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
                "legacy sample",
                EpisodeAvailability {
                    status: Some("NONE"),
                    episode_type: None,
                    is_sample: true,
                    paid: None,
                    possession_coin: None,
                    rent_coin: None,
                },
                PurchaseState::Sample,
            ),
            (
                "legacy free",
                EpisodeAvailability {
                    status: Some("NONE"),
                    episode_type: None,
                    is_sample: false,
                    paid: Some(false),
                    possession_coin: None,
                    rent_coin: None,
                },
                PurchaseState::Free,
            ),
            (
                "omitted prices and type",
                EpisodeAvailability {
                    status: Some("NONE"),
                    episode_type: None,
                    is_sample: false,
                    paid: None,
                    possession_coin: None,
                    rent_coin: None,
                },
                PurchaseState::NotOwned,
            ),
        ];

        for (name, availability, expected) in cases {
            assert_eq!(PurchaseState::from_remote(availability), expected, "{name}");
        }
    }

    #[test]
    fn owned_sample_and_free_episodes_are_readable() {
        assert!(PurchaseState::Owned.is_readable());
        assert!(PurchaseState::Sample.is_readable());
        assert!(PurchaseState::Free.is_readable());
        assert!(!PurchaseState::NotOwned.is_readable());
        assert!(!PurchaseState::Other("RENTAL".to_owned()).is_readable());
    }

    #[test]
    fn remote_text_with_unsupported_glyphs_uses_the_fallback() {
        assert_eq!(display_text("近似嚮導", "Title hunter_q"), "Title hunter_q");
        assert_eq!(display_text("Dinner", "Title 365"), "Dinner");
    }
}
