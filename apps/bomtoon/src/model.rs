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
    pub fn from_remote(status: &str, is_sample: bool, paid: Option<bool>) -> Self {
        if status == "POSSESSION" {
            Self::Owned
        } else if is_sample {
            Self::Sample
        } else if paid == Some(false) {
            Self::Free
        } else if status == "NONE" {
            Self::NotOwned
        } else {
            Self::Other(status.to_owned())
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
    use super::display_text;

    #[test]
    fn remote_text_with_unsupported_glyphs_uses_the_fallback() {
        assert_eq!(display_text("近似嚮導", "Title hunter_q"), "Title hunter_q");
        assert_eq!(display_text("Dinner", "Title 365"), "Dinner");
    }
}
