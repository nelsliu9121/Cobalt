use crate::model::{Comic, Episode, PurchaseState, RecentEntry};
use kobo_json::Value;
use std::{error::Error, fmt, str};

const NEXT_DATA_ID: &str = "id=\"__NEXT_DATA__\"";
const MAX_LIBRARY_PAGES: usize = 100;

#[derive(Debug)]
pub enum ParseError {
    Utf8(str::Utf8Error),
    Json(kobo_json::ParseError),
    Missing(&'static str),
    WrongType(&'static str),
    InvalidValue(&'static str),
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Utf8(_) => formatter.write_str("response is not UTF-8"),
            Self::Json(_) => formatter.write_str("response contains invalid JSON"),
            Self::Missing(field) => write!(formatter, "response is missing {field}"),
            Self::WrongType(field) => write!(formatter, "response has the wrong type for {field}"),
            Self::InvalidValue(field) => write!(formatter, "response has an invalid {field}"),
        }
    }
}

impl Error for ParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Utf8(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Missing(_) | Self::WrongType(_) | Self::InvalidValue(_) => None,
        }
    }
}

pub struct LibraryPage {
    pub comics: Vec<Comic>,
    pub number: usize,
    pub total_pages: usize,
    pub total_items: usize,
}

pub struct RecentPage {
    pub entries: Vec<RecentEntry>,
    pub number: usize,
    pub total_pages: usize,
    pub total_items: usize,
}

pub fn session_is_authenticated(bytes: &[u8]) -> Result<bool, ParseError> {
    let root = parse_json(bytes)?;
    Ok(matches!(root.get("user"), Some(Value::Object(_))))
}

pub fn library(bytes: &[u8]) -> Result<LibraryPage, ParseError> {
    let root = parse_json(bytes)?;
    if string(&root, "result", "result")? != "SUCCESS" {
        return Err(ParseError::InvalidValue("result"));
    }
    let data = field(&root, "data", "data")?;
    let content = array(data, "content", "data.content")?;
    let number = unsigned(data, "number", "data.number")?;
    let total_pages = unsigned(data, "totalPages", "data.totalPages")?;
    let total_items = unsigned(data, "totalElements", "data.totalElements")?;
    if total_pages > MAX_LIBRARY_PAGES || number >= total_pages.max(1) {
        return Err(ParseError::InvalidValue("library pagination"));
    }
    let comics = content
        .iter()
        .map(|item| {
            Ok(Comic {
                alias: string(item, "alias", "comic.alias")?.to_owned(),
                title: string(item, "title", "comic.title")?.to_owned(),
                owned_episodes: unsigned(item, "collectionCount", "comic.collectionCount")?,
                total_episodes: unsigned(item, "episodeCount", "comic.episodeCount")?,
            })
        })
        .collect::<Result<Vec<_>, ParseError>>()?;
    Ok(LibraryPage {
        comics,
        number,
        total_pages,
        total_items,
    })
}

pub fn recent(bytes: &[u8]) -> Result<RecentPage, ParseError> {
    let root = parse_json(bytes)?;
    if string(&root, "result", "result")? != "SUCCESS" {
        return Err(ParseError::InvalidValue("result"));
    }
    let data = field(&root, "data", "data")?;
    let content = array(data, "content", "data.content")?;
    let number = unsigned(data, "number", "data.number")?;
    let total_pages = unsigned(data, "totalPages", "data.totalPages")?;
    let total_items = unsigned(data, "totalElements", "data.totalElements")?;
    if total_pages > MAX_LIBRARY_PAGES || number >= total_pages.max(1) {
        return Err(ParseError::InvalidValue("recent pagination"));
    }
    let entries = content
        .iter()
        .map(|item| {
            let episode = field(item, "episode", "recent.episode")?;
            Ok(RecentEntry {
                content_alias: string(item, "alias", "recent.alias")?.to_owned(),
                content_title: string(item, "title", "recent.title")?.to_owned(),
                episode_alias: string(episode, "alias", "recent.episode.alias")?.to_owned(),
                episode_title: string(episode, "title", "recent.episode.title")?.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, ParseError>>()?;
    Ok(RecentPage {
        entries,
        number,
        total_pages,
        total_items,
    })
}

pub fn episodes(bytes: &[u8]) -> Result<Vec<Episode>, ParseError> {
    let html = str::from_utf8(bytes).map_err(ParseError::Utf8)?;
    let marker = html.find(NEXT_DATA_ID).ok_or(ParseError::Missing("__NEXT_DATA__"))?;
    let body_start = html[marker..]
        .find('>')
        .map(|offset| marker + offset + 1)
        .ok_or(ParseError::Missing("__NEXT_DATA__ body"))?;
    let body_end = html[body_start..]
        .find("</script>")
        .map(|offset| body_start + offset)
        .ok_or(ParseError::Missing("__NEXT_DATA__ closing tag"))?;
    let root = kobo_json::parse(&html[body_start..body_end]).map_err(ParseError::Json)?;
    let values = root
        .get("props")
        .and_then(|value| value.get("pageProps"))
        .and_then(|value| value.get("ssrDetail"))
        .and_then(|value| value.get("episodes"))
        .and_then(Value::as_array)
        .ok_or(ParseError::Missing("props.pageProps.ssrDetail.episodes"))?;

    values
        .iter()
        .map(|item| {
            let status = string(item, "purchaseStatus", "episode.purchaseStatus")?;
            let is_sample = item.get("isSample").and_then(Value::as_bool).unwrap_or(false);
            let paid = item.get("paid").and_then(Value::as_bool);
            Ok(Episode {
                alias: string(item, "alias", "episode.alias")?.to_owned(),
                title: string(item, "title", "episode.title")?.to_owned(),
                purchase: PurchaseState::from_remote(status, is_sample, paid),
            })
        })
        .collect()
}

fn parse_json(bytes: &[u8]) -> Result<Value, ParseError> {
    let text = str::from_utf8(bytes).map_err(ParseError::Utf8)?;
    kobo_json::parse(text).map_err(ParseError::Json)
}

fn field<'a>(value: &'a Value, key: &str, name: &'static str) -> Result<&'a Value, ParseError> {
    value.get(key).ok_or(ParseError::Missing(name))
}

fn string<'a>(value: &'a Value, key: &str, name: &'static str) -> Result<&'a str, ParseError> {
    field(value, key, name)?
        .as_str()
        .ok_or(ParseError::WrongType(name))
}

fn array<'a>(value: &'a Value, key: &str, name: &'static str) -> Result<&'a [Value], ParseError> {
    field(value, key, name)?
        .as_array()
        .ok_or(ParseError::WrongType(name))
}

fn unsigned(value: &Value, key: &str, name: &'static str) -> Result<usize, ParseError> {
    let number = field(value, key, name)?
        .as_i64()
        .ok_or(ParseError::WrongType(name))?;
    usize::try_from(number).map_err(|_| ParseError::InvalidValue(name))
}

#[cfg(test)]
mod tests {
    use super::{episodes, library, recent, session_is_authenticated};
    use crate::model::PurchaseState;

    #[test]
    fn session_check_does_not_need_token_fields() {
        assert!(session_is_authenticated(br#"{"user":{"name":"Reader"},"expires":"tomorrow"}"#)
            .is_ok_and(|authenticated| authenticated));
        assert_eq!(session_is_authenticated(br#"{}"#).ok(), Some(false));
    }

    #[test]
    fn library_response_becomes_typed_comics() {
        let body = br#"{"result":"SUCCESS","data":{"content":[{"alias":"365","title":"Dinner","collectionCount":25,"episodeCount":25}],"number":0,"totalPages":1,"totalElements":117}}"#;
        let page = library(body).expect("valid library response");
        assert_eq!(page.comics[0].alias, "365");
        assert_eq!(page.comics[0].owned_episodes, 25);
        assert_eq!(page.total_items, 117);
    }

    #[test]
    fn next_data_distinguishes_owned_sample_and_unowned_episodes() {
        let html = br#"<html><script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"ssrDetail":{"episodes":[{"alias":"f1","title":"Sample","purchaseStatus":"NONE","isSample":true,"paid":false},{"alias":"59","title":"Episode 59","purchaseStatus":"POSSESSION","isSample":false,"paid":true},{"alias":"60","title":"Episode 60","purchaseStatus":"NONE","isSample":false,"paid":true}]}}}}</script></html>"#;
        let parsed = episodes(html).expect("valid detail HTML");
        assert_eq!(parsed[0].purchase, PurchaseState::Sample);
        assert_eq!(parsed[1].purchase, PurchaseState::Owned);
        assert_eq!(parsed[2].purchase, PurchaseState::NotOwned);
    }

    #[test]
    fn recent_response_keeps_the_content_and_episode_aliases() {
        let body = br#"{"result":"SUCCESS","data":{"content":[{"alias":"hunter_q","title":"Hunter","episode":{"alias":"60","title":"Episode 60"}}],"number":0,"totalPages":1,"totalElements":1}}"#;
        let page = recent(body).expect("valid recent response");
        assert_eq!(page.entries[0].content_alias, "hunter_q");
        assert_eq!(page.entries[0].episode_alias, "60");
        assert_eq!(page.total_items, 1);
    }
}
