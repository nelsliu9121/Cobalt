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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentEntry {
    pub content_alias: String,
    pub content_title: String,
    pub episode_alias: String,
    pub episode_title: String,
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
    pub fn from_remote(status: Option<&str>, is_sample: bool, paid: Option<bool>) -> Self {
        if status == Some("POSSESSION") {
            Self::Owned
        } else if is_sample {
            Self::Sample
        } else if paid == Some(false) {
            Self::Free
        } else if status.is_none() || status == Some("NONE") {
            Self::NotOwned
        } else {
            Self::Other(status.unwrap_or_default().to_owned())
        }
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
    use super::{display_text, PurchaseState};

    #[test]
    fn purchase_state_uses_remote_precedence_and_nullable_status() {
        let cases = [
            (
                Some("POSSESSION"),
                true,
                Some(false),
                PurchaseState::Owned,
            ),
            (None, true, None, PurchaseState::Sample),
            (
                Some("NONE"),
                true,
                Some(false),
                PurchaseState::Sample,
            ),
            (None, false, Some(false), PurchaseState::Free),
            (
                Some("NONE"),
                false,
                Some(false),
                PurchaseState::Free,
            ),
            (
                Some("RENTAL"),
                false,
                Some(false),
                PurchaseState::Free,
            ),
            (None, false, None, PurchaseState::NotOwned),
            (None, false, Some(true), PurchaseState::NotOwned),
            (Some("NONE"), false, None, PurchaseState::NotOwned),
            (
                Some("RENTAL"),
                false,
                Some(true),
                PurchaseState::Other("RENTAL".to_owned()),
            ),
        ];

        for (status, is_sample, paid, expected) in cases {
            assert_eq!(
                PurchaseState::from_remote(status, is_sample, paid),
                expected
            );
        }
    }

    #[test]
    fn remote_text_with_unsupported_glyphs_uses_the_fallback() {
        assert_eq!(display_text("近似嚮導", "Title hunter_q"), "Title hunter_q");
        assert_eq!(display_text("Dinner", "Title 365"), "Dinner");
    }
}
