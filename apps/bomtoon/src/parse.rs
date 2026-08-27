use crate::model::{Comic, Episode, PurchaseState, RecentEntry};
use kobo_json::Value;
use std::{error::Error, fmt, str};

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
    let root = parse_json(bytes)?;
    if string(&root, "result", "result")? != "SUCCESS" {
        return Err(ParseError::InvalidValue("result"));
    }
    let data = field(&root, "data", "data")?;
    let values = array(data, "episodes", "data.episodes")?;

    values
        .iter()
        .map(|item| {
            let status =
                nullable_string(item, "purchaseStatus", "episode.purchaseStatus")?;
            let is_sample = boolean(item, "isSample", "episode.isSample")?;
            let paid = optional_boolean(item, "paid", "episode.paid")?;
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

fn nullable_string<'a>(
    value: &'a Value,
    key: &str,
    name: &'static str,
) -> Result<Option<&'a str>, ParseError> {
    match field(value, key, name)? {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text)),
        _ => Err(ParseError::WrongType(name)),
    }
}

fn boolean(value: &Value, key: &str, name: &'static str) -> Result<bool, ParseError> {
    field(value, key, name)?
        .as_bool()
        .ok_or(ParseError::WrongType(name))
}

fn optional_boolean(
    value: &Value,
    key: &str,
    name: &'static str,
) -> Result<Option<bool>, ParseError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(ParseError::WrongType(name)),
    }
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
    use super::{episodes, library, recent, ParseError};
    use crate::model::PurchaseState;

    const CONTENT: &[u8] = br#"{
      "result":"SUCCESS",
      "data":{
        "episodes":[
          {"alias":"sample","title":"Sample","isSample":true,"purchaseStatus":null,"paid":null},
          {"alias":"owned","title":"Owned","isSample":false,"purchaseStatus":"POSSESSION","paid":true},
          {"alias":"locked","title":"Locked","isSample":false,"purchaseStatus":null,"paid":null}
        ]
      }
    }"#;

    #[test]
    fn library_response_becomes_typed_comics() {
        let body = br#"{"result":"SUCCESS","data":{"content":[{"alias":"365","title":"Dinner","collectionCount":25,"episodeCount":25}],"number":0,"totalPages":1,"totalElements":117}}"#;
        let page = library(body).expect("valid library response");
        assert_eq!(page.comics[0].alias, "365");
        assert_eq!(page.comics[0].owned_episodes, 25);
        assert_eq!(page.total_items, 117);
    }

    #[test]
    fn episodes_are_read_from_data_episodes() {
        let parsed = episodes(CONTENT).expect("valid content response");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].alias, "sample");
        assert_eq!(parsed[1].title, "Owned");
        assert_eq!(parsed[2].alias, "locked");
    }

    #[test]
    fn null_purchase_status_is_sample_when_sample_and_not_owned_otherwise() {
        let parsed = episodes(CONTENT).expect("valid content response");
        assert_eq!(parsed[0].purchase, PurchaseState::Sample);
        assert_eq!(parsed[1].purchase, PurchaseState::Owned);
        assert_eq!(parsed[2].purchase, PurchaseState::NotOwned);
    }

    #[test]
    fn episodes_rejects_non_json_content() {
        let body = br#"<html><script id="__NEXT_DATA__" type="application/json">{"result":"SUCCESS","data":{"episodes":[]}}</script></html>"#;
        assert!(matches!(episodes(body), Err(ParseError::Json(_))));
    }

    #[test]
    fn paid_may_be_absent_or_null() {
        let body = br#"{"result":"SUCCESS","data":{"episodes":[{"alias":"absent","title":"Absent","purchaseStatus":"NONE","isSample":false},{"alias":"null","title":"Null","purchaseStatus":"NONE","isSample":false,"paid":null}]}}"#;
        let parsed = episodes(body).expect("optional paid values");
        assert_eq!(parsed[0].purchase, PurchaseState::NotOwned);
        assert_eq!(parsed[1].purchase, PurchaseState::NotOwned);
    }

    #[test]
    fn episode_purchase_fields_require_observed_types() {
        for body in [
            br#"{"result":"SUCCESS","data":{"episodes":[{"alias":"1","title":"One","purchaseStatus":false,"isSample":false}]}}"#
                .as_slice(),
            br#"{"result":"SUCCESS","data":{"episodes":[{"alias":"1","title":"One","purchaseStatus":null,"isSample":"false"}]}}"#
                .as_slice(),
            br#"{"result":"SUCCESS","data":{"episodes":[{"alias":"1","title":"One","purchaseStatus":null,"isSample":false,"paid":"false"}]}}"#
                .as_slice(),
        ] {
            assert!(matches!(episodes(body), Err(ParseError::WrongType(_))));
        }
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
