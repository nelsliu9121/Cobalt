use crate::model::{
    AssetAmounts, AssetKind, AssetSubtype, Comic, Episode, EpisodeAvailability, EpisodeImage,
    ExpirationRow, PurchaseState, RecentEntry, WalletSummary,
};
use http::Uri;
use kobo_json::Value;
use std::{error::Error, fmt, str};

const MAX_LIBRARY_PAGES: usize = 100;
const MAX_IMAGES: usize = 256;
const MAX_SIGNED_URL_BYTES: usize = 1024;
const MAX_HISTORY_ENTRIES: usize = 256;
const MAX_HISTORY_ROWS: usize = 256;
const MAX_HISTORY_DESCRIPTION_BYTES: usize = 256;

#[derive(Debug)]
pub enum ParseError {
    Utf8(str::Utf8Error),
    Json(kobo_json::ParseError),
    Missing(&'static str),
    WrongType(&'static str),
    InvalidValue(&'static str),
    UnsupportedScramble,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Utf8(_) => formatter.write_str("response is not UTF-8"),
            Self::Json(_) => formatter.write_str("response contains invalid JSON"),
            Self::Missing(field) => write!(formatter, "response is missing {field}"),
            Self::WrongType(field) => write!(formatter, "response has the wrong type for {field}"),
            Self::InvalidValue(field) => write!(formatter, "response has an invalid {field}"),
            Self::UnsupportedScramble => {
                formatter.write_str("this episode uses unsupported scrambled images")
            }
        }
    }
}

impl Error for ParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Utf8(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Missing(_)
            | Self::WrongType(_)
            | Self::InvalidValue(_)
            | Self::UnsupportedScramble => None,
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

pub fn asset_summary(bytes: &[u8]) -> Result<WalletSummary, ParseError> {
    let root = parse_json(bytes)?;
    if string(&root, "result", "result")? != "SUCCESS" {
        return Err(ParseError::InvalidValue("result"));
    }
    let data = field(&root, "data", "data")?;
    let coin_balance = field(data, "coinBalance", "data.coinBalance")?;
    let ticket_balance = field(data, "ticketBalance", "data.ticketBalance")?;
    Ok(WalletSummary {
        coins: asset_amounts(
            coin_balance,
            ("coin", "coinBalance.coin"),
            ("bonusCoin", "coinBalance.bonusCoin"),
            ("freeCoin", "coinBalance.freeCoin"),
            "coin total",
        )?,
        tickets: asset_amounts(
            ticket_balance,
            ("ticket", "ticketBalance.ticket"),
            ("bonusTicket", "ticketBalance.bonusTicket"),
            ("freeTicket", "ticketBalance.freeTicket"),
            "ticket total",
        )?,
    })
}

pub fn expiration_history(
    bytes: &[u8],
    kind: AssetKind,
) -> Result<Vec<ExpirationRow>, ParseError> {
    let root = parse_json(bytes)?;
    if string(&root, "result", "result")? != "SUCCESS" {
        return Err(ParseError::InvalidValue("result"));
    }
    let entries = field(&root, "data", "data")?
        .as_array()
        .ok_or(ParseError::WrongType("data"))?;
    if entries.len() > MAX_HISTORY_ENTRIES {
        return Err(ParseError::InvalidValue("history entry count"));
    }
    let components = match kind {
        AssetKind::Coin => [
            (
                AssetSubtype::Standard,
                "coin",
                "history.coin",
                "coinExpiredAt",
                "history.coinExpiredAt",
            ),
            (
                AssetSubtype::Bonus,
                "bonusCoin",
                "history.bonusCoin",
                "bonusCoinExpiredAt",
                "history.bonusCoinExpiredAt",
            ),
            (
                AssetSubtype::Free,
                "freeCoin",
                "history.freeCoin",
                "freeCoinExpiredAt",
                "history.freeCoinExpiredAt",
            ),
        ],
        AssetKind::Ticket => [
            (
                AssetSubtype::Standard,
                "ticket",
                "history.ticket",
                "ticketExpiredAt",
                "history.ticketExpiredAt",
            ),
            (
                AssetSubtype::Bonus,
                "bonusTicket",
                "history.bonusTicket",
                "bonusTicketExpiredAt",
                "history.bonusTicketExpiredAt",
            ),
            (
                AssetSubtype::Free,
                "freeTicket",
                "history.freeTicket",
                "freeTicketExpiredAt",
                "history.freeTicketExpiredAt",
            ),
        ],
    };
    let capacity = entries
        .len()
        .saturating_mul(components.len())
        .min(MAX_HISTORY_ROWS);
    let mut rows = Vec::with_capacity(capacity);
    for entry in entries {
        let description = optional_string(entry, "title", "history.title")?;
        if description.is_some_and(|text| text.len() > MAX_HISTORY_DESCRIPTION_BYTES) {
            return Err(ParseError::InvalidValue("history.title"));
        }
        for &(subtype, amount_key, amount_name, expiry_key, expiry_name) in &components {
            let quantity = unsigned(entry, amount_key, amount_name)?;
            let expires_at = optional_timestamp(entry, expiry_key, expiry_name)?;
            if quantity == 0 {
                continue;
            }
            if rows.len() == MAX_HISTORY_ROWS {
                return Err(ParseError::InvalidValue("history row count"));
            }
            rows.push(ExpirationRow {
                kind,
                subtype,
                quantity,
                expires_at,
                description: description.map(str::to_owned),
            });
        }
    }
    Ok(rows)
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
            let coin_kind = optional_string(item, "coinKind", "episode.coinKind")?;
            let availability = EpisodeAvailability {
                status: nullable_string(item, "purchaseStatus", "episode.purchaseStatus")?,
                episode_type: optional_string(item, "type", "episode.type")?,
                is_sample: boolean(item, "isSample", "episode.isSample")?,
                paid: optional_boolean(item, "paid", "episode.paid")?,
                possession_coin: optional_unsigned(
                    item,
                    "possessionCoin",
                    "episode.possessionCoin",
                )?,
                rent_coin: optional_unsigned(item, "rentCoin", "episode.rentCoin")?,
            };
            let ticket_quantity = if coin_kind == Some("TICKET") {
                Some(
                    availability
                        .rent_coin
                        .or(availability.possession_coin)
                        .ok_or(ParseError::Missing("episode ticket quantity"))?,
                )
            } else {
                None
            };
            Ok(Episode {
                alias: string(item, "alias", "episode.alias")?.to_owned(),
                title: string(item, "title", "episode.title")?.to_owned(),
                purchase: PurchaseState::from_remote(availability),
                ticket_quantity,
            })
        })
        .collect()
}

pub fn images(bytes: &[u8]) -> Result<Vec<EpisodeImage>, ParseError> {
    let root = parse_json(bytes)?;
    if string(&root, "result", "result")? != "SUCCESS" {
        return Err(ParseError::InvalidValue("result"));
    }
    let values = field(&root, "data", "data")?
        .as_array()
        .ok_or(ParseError::WrongType("data"))?;
    if values.is_empty() || values.len() > MAX_IMAGES {
        return Err(ParseError::InvalidValue("image count"));
    }

    values
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let order = unsigned(item, "orderNo", "image.orderNo")?;
            if order != index + 1 {
                return Err(ParseError::InvalidValue("image ordering"));
            }
            let width = positive_u32(item, "width", "image.width")?;
            let height = positive_u32(item, "height", "image.height")?;
            let pixels = u64::from(width) * u64::from(height);
            if pixels > kobo_image::MAX_PIXELS {
                return Err(ParseError::InvalidValue("image dimensions"));
            }
            require_null(item, "line", "image.line")?;
            require_null(item, "point", "image.point")?;
            let url = string(item, "imagePath", "image.imagePath")?;
            let path = signed_image_path(url)?;
            Ok(EpisodeImage {
                order,
                width,
                height,
                path,
                url: url.to_owned(),
            })
        })
        .collect()
}

fn asset_amounts(
    value: &Value,
    standard: (&str, &'static str),
    bonus: (&str, &'static str),
    free: (&str, &'static str),
    total_name: &'static str,
) -> Result<AssetAmounts, ParseError> {
    let amounts = AssetAmounts {
        standard: unsigned(value, standard.0, standard.1)?,
        bonus: unsigned(value, bonus.0, bonus.1)?,
        free: unsigned(value, free.0, free.1)?,
    };
    amounts
        .total()
        .ok_or(ParseError::InvalidValue(total_name))?;
    Ok(amounts)
}

fn optional_timestamp(
    value: &Value,
    key: &str,
    name: &'static str,
) -> Result<Option<i64>, ParseError> {
    let Some(value) = value.get(key) else {
        return Ok(None);
    };
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let text = value
        .as_integer_str()
        .ok_or(ParseError::WrongType(name))?;
    let timestamp = text
        .parse::<i64>()
        .map_err(|_| ParseError::InvalidValue(name))?;
    match timestamp {
        0 => Ok(None),
        1.. => Ok(Some(timestamp)),
        _ => Err(ParseError::InvalidValue(name)),
    }
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

fn optional_string<'a>(
    value: &'a Value,
    key: &str,
    name: &'static str,
) -> Result<Option<&'a str>, ParseError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(ParseError::WrongType(name)),
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
    let text = field(value, key, name)?
        .as_integer_str()
        .ok_or(ParseError::WrongType(name))?;
    text.parse::<usize>()
        .map_err(|_| ParseError::InvalidValue(name))
}

fn optional_unsigned(
    value: &Value,
    key: &str,
    name: &'static str,
) -> Result<Option<usize>, ParseError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_integer_str()
            .ok_or(ParseError::WrongType(name))?
            .parse::<usize>()
            .map(Some)
            .map_err(|_| ParseError::InvalidValue(name)),
    }
}

fn positive_u32(value: &Value, key: &str, name: &'static str) -> Result<u32, ParseError> {
    let number = field(value, key, name)?
        .as_i64()
        .ok_or(ParseError::WrongType(name))?;
    let number = u32::try_from(number).map_err(|_| ParseError::InvalidValue(name))?;
    if number == 0 {
        return Err(ParseError::InvalidValue(name));
    }
    Ok(number)
}

fn require_null(value: &Value, key: &str, name: &'static str) -> Result<(), ParseError> {
    match value.get(key) {
        None => Err(ParseError::Missing(name)),
        Some(Value::Null) => Ok(()),
        Some(_) => Err(ParseError::UnsupportedScramble),
    }
}

fn signed_image_path(url: &str) -> Result<String, ParseError> {
    if url.len() > MAX_SIGNED_URL_BYTES || url.contains('#') {
        return Err(ParseError::InvalidValue("image URL"));
    }
    let uri = url
        .parse::<Uri>()
        .map_err(|_| ParseError::InvalidValue("image URL"))?;
    if uri.scheme_str() != Some("https") {
        return Err(ParseError::InvalidValue("image URL"));
    }
    let authority = uri
        .authority()
        .ok_or(ParseError::InvalidValue("image URL"))?;
    if !matches!(
        authority.as_str(),
        "image.balcony.studio" | "image.balcony.studio:443"
    ) {
        return Err(ParseError::InvalidValue("image URL"));
    }
    let path = uri.path();
    if !path.starts_with("/tw/ep/") || !path.as_bytes().ends_with(b".webp") {
        return Err(ParseError::InvalidValue("image URL"));
    }

    let query = uri.query().ok_or(ParseError::InvalidValue("image URL"))?;
    let mut policy = None;
    let mut signature = None;
    let mut key_pair_id = None;
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or(ParseError::InvalidValue("image URL"))?;
        if value.is_empty() {
            return Err(ParseError::InvalidValue("image URL"));
        }
        let slot = match key {
            "Policy" => &mut policy,
            "Signature" => &mut signature,
            "Key-Pair-Id" => &mut key_pair_id,
            _ => return Err(ParseError::InvalidValue("image URL")),
        };
        if slot.replace(value).is_some() {
            return Err(ParseError::InvalidValue("image URL"));
        }
    }
    if policy.is_none() || signature.is_none() || key_pair_id.is_none() {
        return Err(ParseError::InvalidValue("image URL"));
    }

    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        asset_summary, episodes, expiration_history, images, library, recent, ParseError,
    };
    use crate::model::{AssetKind, AssetSubtype, PurchaseState};

    const CONTENT: &[u8] = br#"{
      "result":"SUCCESS",
      "data":{
        "episodes":[
          {"alias":"f1","title":"Free preview","type":"PREVIEW","isSample":false,"purchaseStatus":"NONE","paid":null,"possessionCoin":0,"rentCoin":0,"permanentCoin":3},
          {"alias":"1","title":"Episode 1","type":"GENERAL","isSample":false,"purchaseStatus":"NONE","paid":null,"possessionCoin":0,"rentCoin":0,"permanentCoin":3},
          {"alias":"2","title":"Episode 2","type":"GENERAL","isSample":false,"purchaseStatus":"NONE","paid":null,"possessionCoin":3,"rentCoin":2,"permanentCoin":3},
          {"alias":"owned","title":"Owned","type":"PREVIEW","isSample":true,"purchaseStatus":"POSSESSION","paid":false,"possessionCoin":0,"rentCoin":0},
          {"alias":"legacy-sample","title":"Legacy sample","isSample":true,"purchaseStatus":"NONE","paid":null},
          {"alias":"legacy-free","title":"Legacy free","isSample":false,"purchaseStatus":"NONE","paid":false},
          {"alias":"omitted","title":"Omitted prices and type","isSample":false,"purchaseStatus":"NONE","paid":null}
        ]
      }
    }"#;

    fn signed(path: &str, policy: &str, signature: &str, key: &str) -> String {
        format!(
            "https://image.balcony.studio{path}?Policy={policy}&Signature={signature}&Key-Pair-Id={key}"
        )
    }

    fn manifest(entries: &[String]) -> Vec<u8> {
        format!(
            "{{\"result\":\"SUCCESS\",\"data\":[{}]}}",
            entries.join(",")
        )
        .into_bytes()
    }

    fn image(order: usize, width: u32, height: u32, url: &str) -> String {
        format!(
            "{{\"orderNo\":{order},\"width\":{width},\"height\":{height},\"imagePath\":\"{url}\",\"line\":null,\"point\":null}}"
        )
    }

    fn signed_of_len(length: usize) -> String {
        let prefix = "https://image.balcony.studio/tw/ep/one.webp?Policy=";
        let suffix = "&Signature=s&Key-Pair-Id=k";
        assert!(length >= prefix.len() + suffix.len());
        format!(
            "{prefix}{}{suffix}",
            "p".repeat(length - prefix.len() - suffix.len())
        )
    }

    fn coin_history_entry(title: &str, coin: usize, bonus: usize, free: usize) -> String {
        format!(
            "{{\"title\":\"{title}\",\"coin\":{coin},\"bonusCoin\":{bonus},\"freeCoin\":{free}}}"
        )
    }

    fn coin_history(entries: &[String]) -> Vec<u8> {
        format!(
            "{{\"result\":\"SUCCESS\",\"data\":[{}]}}",
            entries.join(",")
        )
        .into_bytes()
    }

    #[test]
    fn asset_summary_parses_remote_buckets_and_checked_totals() {
        let summary = asset_summary(
            br#"{
              "result":"SUCCESS",
              "data":{
                "coinBalance":{"coin":7,"bonusCoin":2,"freeCoin":1},
                "ticketBalance":{"ticket":3,"bonusTicket":1,"freeTicket":0}
              }
            }"#,
        )
        .expect("summary");
        assert_eq!(summary.coins.total(), Some(10));
        assert_eq!(summary.tickets.total(), Some(4));
    }

    #[test]
    fn asset_summary_rejects_missing_wrong_and_negative_amounts() {
        for body in [
            br#"{"result":"SUCCESS","data":{"coinBalance":{},"ticketBalance":{"ticket":0,"bonusTicket":0,"freeTicket":0}}}"#.as_slice(),
            br#"{"result":"SUCCESS","data":{"coinBalance":{"coin":"7","bonusCoin":0,"freeCoin":0},"ticketBalance":{"ticket":0,"bonusTicket":0,"freeTicket":0}}}"#.as_slice(),
            br#"{"result":"SUCCESS","data":{"coinBalance":{"coin":-1,"bonusCoin":0,"freeCoin":0},"ticketBalance":{"ticket":0,"bonusTicket":0,"freeTicket":0}}}"#.as_slice(),
        ] {
            assert!(asset_summary(body).is_err());
        }
    }

    #[test]
    fn asset_summary_rejects_overflowing_totals() {
        let body = format!(
            concat!(
                "{{\"result\":\"SUCCESS\",\"data\":{{",
                "\"coinBalance\":{{\"coin\":{},\"bonusCoin\":1,\"freeCoin\":0}},",
                "\"ticketBalance\":{{\"ticket\":0,\"bonusTicket\":0,\"freeTicket\":0}}",
                "}}}}"
            ),
            usize::MAX,
        );
        assert!(matches!(
            asset_summary(body.as_bytes()),
            Err(ParseError::InvalidValue("coin total"))
        ));
    }

    #[test]
    fn expiration_history_flattens_nonzero_components_in_server_order() {
        let rows = expiration_history(
            br#"{
              "result":"SUCCESS",
              "data":[{
                "title":"Signup gift",
                "coin":2,
                "coinExpiredAt":1756684800000,
                "bonusCoin":1,
                "bonusCoinExpiredAt":0,
                "freeCoin":0
              }]
            }"#,
            AssetKind::Coin,
        )
        .expect("history");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].subtype, AssetSubtype::Standard);
        assert_eq!(rows[0].quantity, 2);
        assert_eq!(rows[0].expires_at, Some(1_756_684_800_000));
        assert_eq!(rows[0].description.as_deref(), Some("Signup gift"));
        assert_eq!(rows[1].subtype, AssetSubtype::Bonus);
        assert_eq!(rows[1].expires_at, None);
    }

    #[test]
    fn expiration_history_parses_ticket_fields() {
        let rows = expiration_history(
            br#"{"result":"SUCCESS","data":[{
              "ticket":2,"ticketExpiredAt":1,
              "bonusTicket":1,"bonusTicketExpiredAt":2,
              "freeTicket":3,"freeTicketExpiredAt":3
            }]}"#,
            AssetKind::Ticket,
        )
        .expect("ticket history");
        assert_eq!(
            rows.iter()
                .map(|row| (row.subtype, row.quantity, row.expires_at))
                .collect::<Vec<_>>(),
            [
                (AssetSubtype::Standard, 2, Some(1)),
                (AssetSubtype::Bonus, 1, Some(2)),
                (AssetSubtype::Free, 3, Some(3)),
            ]
        );
        assert!(rows.iter().all(|row| row.kind == AssetKind::Ticket));
        assert!(rows.iter().all(|row| row.description.is_none()));
    }

    #[test]
    fn expiration_history_rejects_more_than_256_remote_entries() {
        let accepted = (0..256)
            .map(|index| coin_history_entry(&index.to_string(), 0, 0, 0))
            .collect::<Vec<_>>();
        assert!(expiration_history(&coin_history(&accepted), AssetKind::Coin).is_ok());

        let rejected = (0..257)
            .map(|index| coin_history_entry(&index.to_string(), 0, 0, 0))
            .collect::<Vec<_>>();
        assert!(expiration_history(&coin_history(&rejected), AssetKind::Coin).is_err());
    }

    #[test]
    fn expiration_history_rejects_more_than_256_flattened_rows() {
        let mut entries = (0..85)
            .map(|index| coin_history_entry(&index.to_string(), 1, 1, 1))
            .collect::<Vec<_>>();
        entries.push(coin_history_entry("last", 1, 0, 0));
        assert_eq!(
            expiration_history(&coin_history(&entries), AssetKind::Coin)
                .expect("256 flattened rows")
                .len(),
            256
        );

        entries.pop();
        entries.push(coin_history_entry("last", 1, 1, 0));
        assert!(expiration_history(&coin_history(&entries), AssetKind::Coin).is_err());
    }

    #[test]
    fn expiration_history_bounds_descriptions_by_utf8_bytes() {
        let valid = coin_history(&[coin_history_entry(&"é".repeat(128), 1, 0, 0)]);
        assert_eq!(
            expiration_history(&valid, AssetKind::Coin)
                .expect("256-byte description")[0]
                .description
                .as_deref(),
            Some("é".repeat(128).as_str())
        );

        let too_long = coin_history(&[coin_history_entry(
            &format!("{}a", "é".repeat(128)),
            1,
            0,
            0,
        )]);
        assert!(expiration_history(&too_long, AssetKind::Coin).is_err());
    }

    #[test]
    fn expiration_history_rejects_invalid_timestamp_types() {
        let body = br#"{"result":"SUCCESS","data":[{
          "coin":1,"coinExpiredAt":"tomorrow","bonusCoin":0,"freeCoin":0
        }]}"#;
        assert!(matches!(
            expiration_history(body, AssetKind::Coin),
            Err(ParseError::WrongType("history.coinExpiredAt"))
        ));
    }

    #[test]
    fn expiration_history_rejects_negative_fractional_and_oversized_timestamps() {
        for timestamp in ["-1", "1.0", "9223372036854775808"] {
            let body = format!(
                concat!(
                    "{{\"result\":\"SUCCESS\",\"data\":[{{",
                    "\"coin\":1,\"coinExpiredAt\":{},\"bonusCoin\":0,\"freeCoin\":0",
                    "}}]}}"
                ),
                timestamp
            );
            assert!(
                expiration_history(body.as_bytes(), AssetKind::Coin).is_err(),
                "timestamp {timestamp}"
            );
        }
    }

    #[test]
    fn expiration_history_treats_missing_expiry_as_none() {
        let rows = expiration_history(
            br#"{"result":"SUCCESS","data":[{"coin":1,"bonusCoin":0,"freeCoin":0}]}"#,
            AssetKind::Coin,
        )
        .expect("missing expiry");
        assert_eq!(rows[0].expires_at, None);
    }

    #[test]
    fn expiration_history_omits_zero_quantities() {
        let rows = expiration_history(
            br#"{"result":"SUCCESS","data":[{
              "title":"empty","coin":0,"coinExpiredAt":0,"bonusCoin":0,"freeCoin":0
            }]}"#,
            AssetKind::Coin,
        )
        .expect("zero quantities");
        assert!(rows.is_empty());
    }

    #[test]
    fn ticket_coin_kind_requires_and_retains_effective_quantity() {
        let parsed = episodes(
            br#"{
              "result":"SUCCESS",
              "data":{"episodes":[
                {"alias":"rent","title":"Rent ticket","purchaseStatus":"NONE","isSample":false,"coinKind":"TICKET","rentCoin":1,"possessionCoin":4},
                {"alias":"possession","title":"Possession ticket","purchaseStatus":"NONE","isSample":false,"coinKind":"TICKET","possessionCoin":2},
                {"alias":"coin","title":"Coin","purchaseStatus":"NONE","isSample":false,"coinKind":"COIN","rentCoin":3},
                {"alias":"lower","title":"Lowercase","purchaseStatus":"NONE","isSample":false,"coinKind":"ticket","rentCoin":5}
              ]}
            }"#,
        )
        .expect("episodes");
        assert_eq!(parsed[0].ticket_quantity, Some(1));
        assert_eq!(parsed[1].ticket_quantity, Some(2));
        assert_eq!(parsed[2].ticket_quantity, None);
        assert_eq!(parsed[3].ticket_quantity, None);
    }

    #[test]
    fn ticket_coin_kind_rejects_missing_quantity() {
        let body = br#"{"result":"SUCCESS","data":{"episodes":[{
          "alias":"ticket","title":"Ticket","purchaseStatus":"NONE","isSample":false,
          "coinKind":"TICKET"
        }]}}"#;
        assert!(matches!(
            episodes(body),
            Err(ParseError::Missing("episode ticket quantity"))
        ));
    }

    #[test]
    fn coin_kind_requires_an_observed_type_when_present() {
        let body = br#"{"result":"SUCCESS","data":{"episodes":[{
          "alias":"ticket","title":"Ticket","purchaseStatus":"NONE","isSample":false,
          "coinKind":false
        }]}}"#;
        assert!(matches!(
            episodes(body),
            Err(ParseError::WrongType("episode.coinKind"))
        ));
    }

    #[test]
    fn plain_manifest_requires_contiguous_order_and_exact_signed_urls() {
        let bytes = manifest(&[
            image(1, 1280, 5000, &signed("/tw/ep/one.webp", "p1", "s1", "k1")),
            image(2, 1280, 5120, &signed("/tw/ep/two.webp", "p2", "s2", "k2")),
        ]);
        let parsed = images(&bytes).expect("plain manifest");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].order, 1);
        assert_eq!(parsed[1].path, "/tw/ep/two.webp");
    }

    #[test]
    fn non_null_line_is_explicitly_unsupported() {
        let bytes = br#"{"result":"SUCCESS","data":[{"orderNo":1,"width":1280,"height":5000,"imagePath":"https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k","line":4,"point":null}]}"#;
        assert!(matches!(
            images(bytes),
            Err(ParseError::UnsupportedScramble)
        ));
    }

    #[test]
    fn non_null_point_is_explicitly_unsupported() {
        let bytes = br#"{"result":"SUCCESS","data":[{"orderNo":1,"width":1280,"height":5000,"imagePath":"https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k","line":null,"point":"cipher"}]}"#;
        assert!(matches!(
            images(bytes),
            Err(ParseError::UnsupportedScramble)
        ));
    }

    #[test]
    fn manifest_count_and_url_length_are_bounded() {
        assert!(images(&manifest(&[])).is_err());
        let too_many = (1..=257)
            .map(|order| image(order, 1, 1, &signed("/tw/ep/one.webp", "p", "s", "k")))
            .collect::<Vec<_>>();
        assert!(images(&manifest(&too_many)).is_err());
        assert!(images(&manifest(&[image(1, 1, 1, &signed_of_len(1024))])).is_ok());
        assert!(images(&manifest(&[image(1, 1, 1, &signed_of_len(1025))])).is_err());
    }

    #[test]
    fn manifest_dimensions_and_order_are_strict() {
        let url = signed("/tw/ep/one.webp", "p", "s", "k");
        assert!(images(&manifest(&[image(1, 2_000, 3_500, &url)])).is_ok());
        assert!(images(&manifest(&[image(1, 1, 7_000_001, &url)])).is_err());
        assert!(images(&manifest(&[image(1, 0, 1, &url)])).is_err());
        for entries in [
            vec![image(0, 1, 1, &url)],
            vec![image(2, 1, 1, &url)],
            vec![image(1, 1, 1, &url), image(1, 1, 1, &url)],
            vec![image(1, 1, 1, &url), image(3, 1, 1, &url)],
        ] {
            assert!(images(&manifest(&entries)).is_err());
        }
    }

    #[test]
    fn signed_url_rejects_origin_path_and_unknown_query_mutations() {
        for (case, url) in [
            "http://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://attacker.invalid/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio:444/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://user@image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio/other/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio/tw/ep/one.png?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k#fragment",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k&extra=x",
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                images(&manifest(&[image(1, 1, 1, url)])).is_err(),
                "case {case}"
            );
        }
    }

    #[test]
    fn signed_url_rejects_malformed_present_ports() {
        for (case, url) in [
            "https://image.balcony.studio:/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio:not-a-port/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio:65536/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio:0443/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://IMAGE.BALCONY.STUDIO/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                images(&manifest(&[image(1, 1, 1, url)])).is_err(),
                "case {case}"
            );
        }
    }

    #[test]
    fn signed_url_accepts_implicit_and_explicit_default_ports() {
        for (case, url) in [
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio:443/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k",
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                images(&manifest(&[image(1, 1, 1, url)])).is_ok(),
                "case {case}"
            );
        }
    }

    #[test]
    fn signed_query_order_and_equals_in_values_are_supported() {
        for (case, url) in [
            "https://image.balcony.studio/tw/ep/one.webp?Signature=s&Key-Pair-Id=k&Policy=p",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s==&Key-Pair-Id=k",
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                images(&manifest(&[image(1, 1, 1, url)])).is_ok(),
                "case {case}"
            );
        }
    }

    #[test]
    fn signed_query_requires_each_key_once_with_a_value() {
        for (case, url) in [
            "https://image.balcony.studio/tw/ep/one.webp?Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Key-Pair-Id=k",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Policy=q&Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Signature=t&Key-Pair-Id=k",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k&Key-Pair-Id=l",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=&Signature=s&Key-Pair-Id=k",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=&Key-Pair-Id=k",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=",
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&&Signature=s&Key-Pair-Id=k",
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                matches!(
                    images(&manifest(&[image(1, 1, 1, url)])),
                    Err(ParseError::InvalidValue("image URL"))
                ),
                "case {case}"
            );
        }
    }

    #[test]
    fn image_fields_preserve_error_categories() {
        assert!(matches!(
            images(br#"{"result":"SUCCESS","data":[{}]}"#),
            Err(ParseError::Missing("image.orderNo"))
        ));
        assert!(matches!(
            images(br#"{"result":"SUCCESS","data":[{"orderNo":"1","width":1,"height":1,"imagePath":"x","line":null,"point":null}]}"#),
            Err(ParseError::WrongType("image.orderNo"))
        ));
        assert!(matches!(
            images(br#"{"result":"SUCCESS","data":[{"orderNo":1,"width":4294967296,"height":1,"imagePath":"x","line":null,"point":null}]}"#),
            Err(ParseError::InvalidValue("image.width"))
        ));
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
    fn episodes_are_read_from_data_episodes() {
        let parsed = episodes(CONTENT).expect("valid content response");
        assert_eq!(parsed.len(), 7);
        assert_eq!(parsed[0].alias, "f1");
        assert_eq!(parsed[1].title, "Episode 1");
        assert_eq!(parsed[2].alias, "2");
    }

    #[test]
    fn live_availability_fields_map_with_fail_closed_precedence() {
        let parsed = episodes(CONTENT).expect("valid content response");
        let purchases = parsed
            .iter()
            .map(|episode| episode.purchase.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            purchases,
            [
                PurchaseState::Sample,
                PurchaseState::Free,
                PurchaseState::NotOwned,
                PurchaseState::Owned,
                PurchaseState::Sample,
                PurchaseState::Free,
                PurchaseState::NotOwned,
            ]
        );
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
            br#"{"result":"SUCCESS","data":{"episodes":[{"alias":"1","title":"One","purchaseStatus":null,"isSample":false,"type":false}]}}"#
                .as_slice(),
            br#"{"result":"SUCCESS","data":{"episodes":[{"alias":"1","title":"One","purchaseStatus":null,"isSample":false,"possessionCoin":"0"}]}}"#
                .as_slice(),
            br#"{"result":"SUCCESS","data":{"episodes":[{"alias":"1","title":"One","purchaseStatus":null,"isSample":false,"rentCoin":false}]}}"#
                .as_slice(),
        ] {
            assert!(matches!(episodes(body), Err(ParseError::WrongType(_))));
        }

        let negative_price = br#"{"result":"SUCCESS","data":{"episodes":[{"alias":"1","title":"One","purchaseStatus":null,"isSample":false,"possessionCoin":-1}]}}"#;
        assert!(matches!(
            episodes(negative_price),
            Err(ParseError::InvalidValue("episode.possessionCoin"))
        ));
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
