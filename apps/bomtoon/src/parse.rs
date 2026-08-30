use crate::model::{
    AssetAmounts, AssetKind, AssetSubtype, BannerComic, Comic, Episode, EpisodeAvailability,
    EpisodeImage, ExpirationRow, Homepage, PurchaseState, RecentEntry, ShelfComic, WalletSummary,
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
const MAX_HOMEPAGE_BANNERS: usize = 64;
const MAX_HOMEPAGE_LIST: usize = 64;
const MAX_ALIAS_BYTES: usize = 96;
const MAX_TITLE_BYTES: usize = 256;
const MAX_COVER_URL_BYTES: usize = 2048;
const BOMTOON_TITLE_SUFFIX: &str = " - 漫畫 - BOMTOON";
const HTML_ENTITIES: &[(&str, &str)] = &[
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    ("nbsp", " "),
    ("hellip", "\u{2026}"),
    ("mdash", "\u{2014}"),
    ("ndash", "\u{2013}"),
    ("lsquo", "\u{2018}"),
    ("rsquo", "\u{2019}"),
    ("ldquo", "\u{201c}"),
    ("rdquo", "\u{201d}"),
    ("sbquo", "\u{201a}"),
    ("bdquo", "\u{201e}"),
    ("laquo", "\u{ab}"),
    ("raquo", "\u{bb}"),
    ("lsaquo", "\u{2039}"),
    ("rsaquo", "\u{203a}"),
    ("bull", "\u{2022}"),
    ("middot", "\u{b7}"),
    ("deg", "\u{b0}"),
    ("copy", "\u{a9}"),
    ("reg", "\u{ae}"),
    ("trade", "\u{2122}"),
    ("sect", "\u{a7}"),
    ("para", "\u{b6}"),
    ("pound", "\u{a3}"),
    ("euro", "\u{20ac}"),
    ("times", "\u{d7}"),
    ("frac12", "\u{bd}"),
    ("shy", ""),
    ("ensp", " "),
    ("emsp", " "),
];
const PUBLIC_IMAGE_PATHS: &[&str] = &[
    "/tw/contents/",
    "/BOMTOON_TW/contents/",
    "/tw/co_thumbnail/",
    "/BOMTOON_TW/co_thumbnail/",
];
const PUBLIC_SQUARE_IMAGE_PATHS: &[&str] = &["/tw/co_thumbnail/", "/BOMTOON_TW/co_thumbnail/"];

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

pub fn homepage(bytes: &[u8]) -> Result<Homepage, ParseError> {
    let html = str::from_utf8(bytes).map_err(ParseError::Utf8)?;
    let payload = next_data_payload(html).ok_or(ParseError::Missing("__NEXT_DATA__"))?;
    let root = kobo_json::parse(payload).map_err(ParseError::Json)?;
    let props = field(&root, "props", "props")?;
    let page_props = field(props, "pageProps", "props.pageProps")?;
    let main = field(page_props, "main", "props.pageProps.main")?;
    let banner_values = bounded_array(
        main,
        "banners",
        "props.pageProps.main.banners",
        MAX_HOMEPAGE_BANNERS,
    )?;
    let newest = homepage_comics(main, "newest", "props.pageProps.main.newest")?;
    let week_day = homepage_comics(main, "weekDay", "props.pageProps.main.weekDay")?;
    let only_bom = homepage_comics(main, "onlyBom", "props.pageProps.main.onlyBom")?;

    let mut banners = Vec::with_capacity(banner_values.len());
    for banner in banner_values {
        let Some(alias) = banner_alias(banner) else {
            continue;
        };
        banners.push(BannerComic {
            alias: alias.to_owned(),
        });
    }

    Ok(Homepage {
        banners,
        newest,
        week_day,
        only_bom,
    })
}

pub fn public_detail(bytes: &[u8], expected_alias: &str) -> Result<ShelfComic, ParseError> {
    if !valid_alias(expected_alias) {
        return Err(ParseError::InvalidValue("detail alias"));
    }
    let html = str::from_utf8(bytes).map_err(ParseError::Utf8)?;
    let raw_title = open_graph_content(html, "og:title").ok_or(ParseError::Missing("og:title"))?;
    let mut title = decode_entities_bounded(
        raw_title,
        MAX_TITLE_BYTES + BOMTOON_TITLE_SUFFIX.len(),
        "detail title",
    )?;
    if title.ends_with(BOMTOON_TITLE_SUFFIX) {
        title.truncate(title.len() - BOMTOON_TITLE_SUFFIX.len());
    }
    if title.trim().is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err(ParseError::InvalidValue("detail title"));
    }
    let cover_url = open_graph_content(html, "og:image")
        .filter(|raw| !raw.chars().any(char::is_control))
        .and_then(|raw| decode_entities_bounded(raw, MAX_COVER_URL_BYTES, "cover URL").ok())
        .and_then(|url| public_image_url(&url, PUBLIC_IMAGE_PATHS).then_some(url));

    Ok(ShelfComic {
        alias: expected_alias.to_owned(),
        title,
        cover_url,
    })
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
                cover_url: public_thumbnail(item, &["MAIN_NON_ADULT"], PUBLIC_SQUARE_IMAGE_PATHS),
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
                cover_url: public_thumbnail(item, &["MAIN_NON_ADULT"], PUBLIC_SQUARE_IMAGE_PATHS),
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

pub fn expiration_history(bytes: &[u8], kind: AssetKind) -> Result<Vec<ExpirationRow>, ParseError> {
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
    let text = value.as_integer_str().ok_or(ParseError::WrongType(name))?;
    let timestamp = text
        .parse::<i64>()
        .map_err(|_| ParseError::InvalidValue(name))?;
    match timestamp {
        0 => Ok(None),
        1.. => Ok(Some(timestamp)),
        _ => Err(ParseError::InvalidValue(name)),
    }
}

fn bounded_array<'a>(
    value: &'a Value,
    key: &str,
    name: &'static str,
    limit: usize,
) -> Result<&'a [Value], ParseError> {
    let values = array(value, key, name)?;
    if values.len() > limit {
        return Err(ParseError::InvalidValue(name));
    }
    Ok(values)
}

fn homepage_comics(
    main: &Value,
    key: &str,
    name: &'static str,
) -> Result<Vec<ShelfComic>, ParseError> {
    let values = bounded_array(main, key, name, MAX_HOMEPAGE_LIST)?;
    let mut comics = Vec::with_capacity(values.len());
    for value in values {
        let Some(alias) = value.get("alias").and_then(Value::as_str) else {
            continue;
        };
        if !valid_alias(alias) {
            continue;
        }
        let Some(title) = value.get("title").and_then(Value::as_str) else {
            continue;
        };
        if title.trim().is_empty() || title.len() > MAX_TITLE_BYTES {
            continue;
        }
        let thumbnail_types: &[&str] = if matches!(value.get("isAdult"), Some(Value::Bool(false))) {
            match key {
                "newest" => &["COVER"],
                "weekDay" => &["VERTICAL"],
                "onlyBom" => &["SQUARE"],
                _ => &[],
            }
        } else {
            &[]
        };
        comics.push(ShelfComic {
            alias: alias.to_owned(),
            title: title.to_owned(),
            cover_url: public_thumbnail(value, thumbnail_types, PUBLIC_IMAGE_PATHS),
        });
    }
    Ok(comics)
}

fn banner_alias(banner: &Value) -> Option<&str> {
    let details = banner.get("bannerDetailInfo")?.as_array()?;
    for detail in details {
        let Some(link) = detail.get("linkInfo") else {
            continue;
        };
        if link.get("target").and_then(Value::as_str) != Some("CONTENTS")
            || link.get("subTarget").and_then(Value::as_str) != Some("COMIC")
        {
            continue;
        }
        let Some(alias) = link.get("params").and_then(Value::as_str) else {
            continue;
        };
        if valid_alias(alias) {
            return Some(alias);
        }
    }
    None
}

fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= MAX_ALIAS_BYTES
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn public_thumbnail(
    value: &Value,
    accepted_types: &[&str],
    accepted_paths: &[&str],
) -> Option<String> {
    let thumbnails = value.get("thumbnails")?.as_array()?;
    for thumbnail in thumbnails {
        let Some(kind) = thumbnail.get("type").and_then(Value::as_str) else {
            continue;
        };
        if !accepted_types.contains(&kind) {
            continue;
        }
        let Some(url) = thumbnail.get("imagePath").and_then(Value::as_str) else {
            continue;
        };
        if public_image_url(url, accepted_paths) {
            return Some(url.to_owned());
        }
    }
    None
}

fn public_image_url(url: &str, accepted_paths: &[&str]) -> bool {
    if url.len() > MAX_COVER_URL_BYTES || url.contains('#') {
        return false;
    }
    let Ok(uri) = url.parse::<Uri>() else {
        return false;
    };
    if uri.scheme_str() != Some("https") || uri.query().is_some() {
        return false;
    }
    let Some(authority) = uri.authority() else {
        return false;
    };
    if !matches!(
        authority.as_str(),
        "image.balcony.studio" | "image.balcony.studio:443"
    ) {
        return false;
    }
    let path = uri.path();
    let relative = accepted_paths
        .iter()
        .find_map(|prefix| path.strip_prefix(prefix));
    let Some(stem) = relative.and_then(|path| path.strip_suffix(".webp")) else {
        return false;
    };
    !stem.is_empty()
        && stem.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
}

fn next_data_payload(html: &str) -> Option<&str> {
    let mut cursor = 0;
    while let Some((_, tag_end, tag)) = next_open_tag(html, cursor, "script") {
        cursor = tag_end + 1;
        let (close, after_close) = closing_element(html, cursor, "script")?;
        if html_attribute(tag, "id") != Some("__NEXT_DATA__") {
            cursor = after_close;
            continue;
        }
        return Some(&html[tag_end + 1..close]);
    }
    None
}

fn open_graph_content<'a>(html: &'a str, property: &str) -> Option<&'a str> {
    let mut cursor = 0;
    while let Some((_, tag_end, tag)) = next_open_tag(html, cursor, "meta") {
        cursor = tag_end + 1;
        if html_attribute(tag, "property") == Some(property) {
            if let Some(content) = html_attribute(tag, "content") {
                return Some(content);
            }
        }
    }
    None
}

fn next_open_tag<'a>(
    html: &'a str,
    mut cursor: usize,
    expected_name: &str,
) -> Option<(usize, usize, &'a str)> {
    while cursor < html.len() {
        let start = cursor + html[cursor..].find('<')?;
        if html[start..].starts_with("<!--") {
            cursor = start + html[start + 4..].find("-->")? + 7;
            continue;
        }
        let bytes = html.as_bytes();
        let name_start = start + 1;
        if matches!(bytes.get(name_start), Some(b'/' | b'!' | b'?')) {
            cursor = find_tag_end(html, name_start + 1)? + 1;
            continue;
        }
        let mut name_end = name_start;
        while bytes
            .get(name_end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':'))
        {
            name_end += 1;
        }
        if name_start == name_end {
            cursor = start + 1;
            continue;
        }
        let tag_end = find_tag_end(html, name_end)?;
        if !bytes
            .get(name_end)
            .copied()
            .is_some_and(is_tag_name_delimiter)
        {
            cursor = tag_end + 1;
            continue;
        }
        let name = &html[name_start..name_end];
        if name.eq_ignore_ascii_case(expected_name) {
            return Some((start, tag_end, &html[start..=tag_end]));
        }
        cursor = if is_inert_html_element(name) {
            closing_element(html, tag_end + 1, name)?.1
        } else {
            tag_end + 1
        };
    }
    None
}

fn is_tag_name_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>')
}

fn is_inert_html_element(name: &str) -> bool {
    name.eq_ignore_ascii_case("template") || is_raw_text_html_element(name)
}

fn is_raw_text_html_element(name: &str) -> bool {
    [
        "script", "style", "noscript", "title", "textarea", "xmp", "iframe", "noembed",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn closing_element(html: &str, cursor: usize, name: &str) -> Option<(usize, usize)> {
    if name.eq_ignore_ascii_case("template") {
        closing_template_element(html, cursor)
    } else {
        closing_raw_element(html, cursor, name)
    }
}

fn closing_raw_element(html: &str, mut cursor: usize, name: &str) -> Option<(usize, usize)> {
    let needle = [
        ("script", "</script"),
        ("style", "</style"),
        ("noscript", "</noscript"),
        ("title", "</title"),
        ("textarea", "</textarea"),
        ("xmp", "</xmp"),
        ("iframe", "</iframe"),
        ("noembed", "</noembed"),
    ]
    .iter()
    .find_map(|(candidate, close)| name.eq_ignore_ascii_case(candidate).then_some(*close))?;
    loop {
        let close = cursor + find_ascii_case_insensitive(&html[cursor..], needle)?;
        let after_name = html.as_bytes().get(close + needle.len()).copied()?;
        if is_tag_name_delimiter(after_name) {
            let tag_end = find_tag_end(html, close + needle.len())?;
            return Some((close, tag_end + 1));
        }
        cursor = close + needle.len();
    }
}

fn closing_template_element(html: &str, mut cursor: usize) -> Option<(usize, usize)> {
    let mut depth = 1usize;
    while cursor < html.len() {
        let start = cursor + html[cursor..].find('<')?;
        if html[start..].starts_with("<!--") {
            cursor = start + html[start + 4..].find("-->")? + 7;
            continue;
        }
        let bytes = html.as_bytes();
        let mut name_start = start + 1;
        let closing = bytes.get(name_start) == Some(&b'/');
        if closing {
            name_start += 1;
        }
        let mut name_end = name_start;
        while bytes
            .get(name_end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':'))
        {
            name_end += 1;
        }
        if name_start == name_end {
            cursor = start + 1;
            continue;
        }
        let tag_end = find_tag_end(html, name_end)?;
        if !bytes
            .get(name_end)
            .copied()
            .is_some_and(is_tag_name_delimiter)
        {
            cursor = tag_end + 1;
            continue;
        }
        let name = &html[name_start..name_end];
        if name.eq_ignore_ascii_case("template") {
            if closing {
                depth -= 1;
                if depth == 0 {
                    return Some((start, tag_end + 1));
                }
            } else {
                depth = depth.checked_add(1)?;
            }
            cursor = tag_end + 1;
            continue;
        }
        cursor = if !closing && is_raw_text_html_element(name) {
            closing_raw_element(html, tag_end + 1, name)?.1
        } else {
            tag_end + 1
        };
    }
    None
}

fn find_tag_end(html: &str, mut cursor: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut quote = None;
    while let Some(&byte) = bytes.get(cursor) {
        match (quote, byte) {
            (Some(open), close) if open == close => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some(cursor),
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn html_attribute<'a>(tag: &'a str, expected_name: &str) -> Option<&'a str> {
    let bytes = tag.as_bytes();
    let mut cursor = 1;
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    while bytes
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':'))
    {
        cursor += 1;
    }
    loop {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if matches!(bytes.get(cursor), None | Some(b'/' | b'>')) {
            return None;
        }
        let name_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'=' | b'/' | b'>'))
        {
            cursor += 1;
        }
        let name_end = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            continue;
        }
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        let (value_start, value_end) = match bytes.get(cursor).copied() {
            Some(quote @ (b'\'' | b'"')) => {
                cursor += 1;
                let start = cursor;
                while bytes.get(cursor).is_some_and(|byte| *byte != quote) {
                    cursor += 1;
                }
                let end = cursor;
                bytes.get(cursor)?;
                cursor += 1;
                (start, end)
            }
            Some(_) => {
                let start = cursor;
                while bytes
                    .get(cursor)
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'/' | b'>'))
                {
                    cursor += 1;
                }
                (start, cursor)
            }
            None => return None,
        };
        if tag[name_start..name_end].eq_ignore_ascii_case(expected_name) {
            return Some(&tag[value_start..value_end]);
        }
    }
}

fn find_ascii_case_insensitive(text: &str, needle: &str) -> Option<usize> {
    text.as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn decode_entities_bounded(
    text: &str,
    limit: usize,
    name: &'static str,
) -> Result<String, ParseError> {
    let mut decoded = String::with_capacity(text.len().min(limit));
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        push_bounded(&mut decoded, &rest[..at], limit, name)?;
        rest = &rest[at..];
        let entity_end = rest
            .as_bytes()
            .iter()
            .take(13)
            .position(|byte| *byte == b';');
        let Some(entity_end) = entity_end else {
            push_bounded(&mut decoded, "&", limit, name)?;
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..entity_end];
        if let Some((_, replacement)) = HTML_ENTITIES
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(entity))
        {
            push_bounded(&mut decoded, replacement, limit, name)?;
            rest = &rest[entity_end + 1..];
            continue;
        }
        let Some(replacement) = numeric_entity(entity) else {
            push_bounded(&mut decoded, "&", limit, name)?;
            rest = &rest[1..];
            continue;
        };
        if decoded.len() + replacement.len_utf8() > limit {
            return Err(ParseError::InvalidValue(name));
        }
        decoded.push(replacement);
        rest = &rest[entity_end + 1..];
    }
    push_bounded(&mut decoded, rest, limit, name)?;
    Ok(decoded)
}

fn numeric_entity(entity: &str) -> Option<char> {
    let digits = entity.strip_prefix('#')?;
    let (digits, radix) = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16),
        None => (digits, 10),
    };
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    let number = u32::from_str_radix(digits, radix).ok()?;
    char::from_u32(number).filter(|character| !invalid_control(*character))
}

fn invalid_control(character: char) -> bool {
    character.is_control() && character != '\n' && character != '\t'
}

fn push_bounded(
    output: &mut String,
    text: &str,
    limit: usize,
    name: &'static str,
) -> Result<(), ParseError> {
    for character in text
        .chars()
        .filter(|character| !invalid_control(*character))
    {
        if output.len() + character.len_utf8() > limit {
            return Err(ParseError::InvalidValue(name));
        }
        output.push(character);
    }
    Ok(())
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
        asset_summary, episodes, expiration_history, homepage, images, library, public_detail,
        recent, ParseError,
    };
    use crate::model::{AssetKind, AssetSubtype, BannerComic, PurchaseState};

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
            expiration_history(&valid, AssetKind::Coin).expect("256-byte description")[0]
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
    fn library_response_becomes_typed_comics_with_safe_public_covers() {
        let body = br#"{"result":"SUCCESS","data":{"content":[
          {"alias":"365","title":"Dinner","collectionCount":25,"episodeCount":25,"thumbnails":[
            {"type":"MAIN","imagePath":"https://image.balcony.studio/tw/co_thumbnail/365/adult.webp"},
            {"type":"MAIN_NON_ADULT","imagePath":"https://image.balcony.studio/tw/co_thumbnail/365/public.webp"}
          ]},
          {"alias":"plain","title":"No cover","collectionCount":1,"episodeCount":2},
          {"alias":"hostile","title":"Hostile cover","collectionCount":3,"episodeCount":4,"thumbnails":[
            {"type":"MAIN_NON_ADULT","imagePath":"https://image.balcony.studio/tw/contents/hostile/public.webp"}
          ]}
        ],"number":0,"totalPages":1,"totalElements":117}}"#;
        let page = library(body).expect("valid library response");
        assert_eq!(page.comics[0].alias, "365");
        assert_eq!(page.comics[0].owned_episodes, 25);
        assert_eq!(
            page.comics[0].cover_url.as_deref(),
            Some("https://image.balcony.studio/tw/co_thumbnail/365/public.webp")
        );
        assert_eq!(page.comics[1].title, "No cover");
        assert_eq!(page.comics[1].cover_url, None);
        assert_eq!(page.comics[2].owned_episodes, 3);
        assert_eq!(page.comics[2].cover_url, None);
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
    fn recent_response_keeps_aliases_and_safe_public_covers() {
        let body = br#"{"result":"SUCCESS","data":{"content":[
          {"alias":"hunter_q","title":"Hunter","thumbnails":[
            {"type":"MAIN","imagePath":"https://image.balcony.studio/tw/co_thumbnail/hunter_q/adult.webp"},
            {"type":"MAIN_NON_ADULT","imagePath":"https://image.balcony.studio/tw/co_thumbnail/hunter_q/public.webp"}
          ],"episode":{"alias":"60","title":"Episode 60"}},
          {"alias":"plain","title":"Plain","episode":{"alias":"1","title":"Episode 1"}},
          {"alias":"hostile","title":"Hostile","thumbnails":[
            {"type":"MAIN_NON_ADULT","imagePath":"https://image.balcony.studio/tw/contents/hostile/public.webp"}
          ],"episode":{"alias":"2","title":"Episode 2"}}
        ],"number":0,"totalPages":1,"totalElements":3}}"#;
        let page = recent(body).expect("valid recent response");
        assert_eq!(page.entries[0].content_alias, "hunter_q");
        assert_eq!(page.entries[0].episode_alias, "60");
        assert_eq!(
            page.entries[0].cover_url.as_deref(),
            Some("https://image.balcony.studio/tw/co_thumbnail/hunter_q/public.webp")
        );
        assert_eq!(page.entries[1].episode_title, "Episode 1");
        assert_eq!(page.entries[1].cover_url, None);
        assert_eq!(page.entries[2].content_title, "Hostile");
        assert_eq!(page.entries[2].cover_url, None);
        assert_eq!(page.total_items, 3);
    }

    fn homepage_document(main: &str) -> String {
        format!(
            concat!(
                "<!doctype html><html><head>",
                "<!--<script id=\"__NEXT_DATA__\">commented-out</script>-->",
                "<title><script id=\"__NEXT_DATA__\">title-text</script></title>",
                "<template><script id=\"__NEXT_DATA__\">template-content</script></template>",
                "<script type=\"application/json\">",
                "{{\"props\":{{\"pageProps\":{{\"main\":{{\"banners\":[],\"newest\":[],\"weekDay\":[],\"onlyBom\":[]}}}}}}}}",
                "</script>",
                "<script data-build=\"changed-build-id\" type=\"application/json\" id=\"__NEXT_DATA__\">",
                "{{\"props\":{{\"pageProps\":{{\"main\":{}}}}},",
                "\"main\":{{\"banners\":\"wrong\"}},\"buildId\":\"not-hard-coded\"}}",
                "</script>",
                "</head><body></body></html>"
            ),
            main
        )
    }

    fn homepage_main(banners: &str, newest: &str, week_day: &str, only_bom: &str) -> String {
        format!(
            "{{\"banners\":[{banners}],\"newest\":[{newest}],\"weekDay\":[{week_day}],\"onlyBom\":[{only_bom}]}}"
        )
    }

    fn shelf_json(alias: &str, title: &str, thumbnail: Option<(&str, &str)>) -> String {
        let thumbnails = thumbnail.map_or_else(
            || "[]".to_owned(),
            |(kind, url)| format!("[{{\"type\":\"{kind}\",\"imagePath\":\"{url}\"}}]"),
        );
        format!(
            "{{\"alias\":\"{alias}\",\"title\":\"{title}\",\"isAdult\":false,\"thumbnails\":{thumbnails}}}"
        )
    }

    fn banner_json(target: &str, sub_target: &str, alias: &str) -> String {
        format!(
            concat!(
                "{{\"bannerDetailInfo\":[{{\"linkInfo\":{{",
                "\"target\":\"{}\",\"subTarget\":\"{}\",\"params\":\"{}\",",
                "\"adultTarget\":\"CONTENTS\",\"adultSubTarget\":\"COMIC\",\"adultUrl\":\"adult_only\"",
                "}}}}]}}"
            ),
            target, sub_target, alias
        )
    }

    #[test]
    fn homepage_ignores_whitespace_prefixed_tag_text() {
        let body = br#"
          < script id="__NEXT_DATA__">not-json</script>
          <script id="__NEXT_DATA__">{"props":{"pageProps":{"main":{"banners":[],"newest":[],"weekDay":[],"onlyBom":[]}}}}</script>
        "#;

        let parsed = homepage(body).expect("whitespace-prefixed text is not a tag");

        assert!(parsed.banners.is_empty());
        assert!(parsed.newest.is_empty());
        assert!(parsed.week_day.is_empty());
        assert!(parsed.only_bom.is_empty());
    }

    #[test]
    fn homepage_ignores_complete_nested_template_content() {
        let body = br#"
          <template>
            <script>const close = '</template>';</script>
            <template></template>
            <script id="__NEXT_DATA__">not-json</script>
          </template>
          <script id="__NEXT_DATA__">{"props":{"pageProps":{"main":{"banners":[],"newest":[],"weekDay":[],"onlyBom":[]}}}}</script>
        "#;

        let parsed = homepage(body).expect("nested template content is inert");

        assert!(parsed.banners.is_empty());
        assert!(parsed.newest.is_empty());
        assert!(parsed.week_day.is_empty());
        assert!(parsed.only_bom.is_empty());
    }

    #[test]
    fn homepage_ignores_malformed_template_closer() {
        let body = br#"
          <template>
            </template=ignored>
            <script id="__NEXT_DATA__">not-json</script>
          </template>
          <script id="__NEXT_DATA__">{"props":{"pageProps":{"main":{"banners":[],"newest":[],"weekDay":[],"onlyBom":[]}}}}</script>
        "#;

        let parsed = homepage(body).expect("malformed closer does not end template");

        assert!(parsed.banners.is_empty());
        assert!(parsed.newest.is_empty());
        assert!(parsed.week_day.is_empty());
        assert!(parsed.only_bom.is_empty());
    }

    #[test]
    fn homepage_reads_only_next_data_main_and_filters_banner_targets() {
        let banners = [
            banner_json("CONTENTS", "COMIC", "featured_a"),
            banner_json("EVENT", "", "event"),
            banner_json("SHOP", "", "shop"),
            banner_json("GIFT", "", "gift"),
            banner_json("PICK", "", "pick"),
            banner_json("", "", ""),
            banner_json("CONTENTS", "EPISODE", "episode"),
            banner_json("CONTENTS", "COMIC", "featured_b"),
        ]
        .join(",");
        let newest = [
            shelf_json(
                "new_a",
                "New A",
                Some((
                    "COVER",
                    "https://image.balcony.studio:443/tw/contents/new_a.webp",
                )),
            ),
            shelf_json("new_none", "No artwork", None),
            shelf_json(
                "new_hostile",
                "Hostile artwork",
                Some((
                    "COVER",
                    "https://attacker.example/tw/contents/new_hostile.webp",
                )),
            ),
            "{\"alias\":\"missing_maturity\",\"title\":\"Missing maturity\",\"thumbnails\":[{\"type\":\"COVER\",\"imagePath\":\"https://image.balcony.studio/tw/contents/missing.webp\"}]}".to_owned(),
            "{\"alias\":\"adult_artwork\",\"title\":\"Adult artwork\",\"isAdult\":true,\"thumbnails\":[{\"type\":\"COVER\",\"imagePath\":\"https://image.balcony.studio/tw/contents/adult.webp\"}]}".to_owned(),
            "{\"alias\":\"bad/slash\",\"title\":\"Bad alias\",\"thumbnails\":[]}".to_owned(),
            "{\"alias\":\"missing_title\",\"thumbnails\":[]}".to_owned(),
            "{\"alias\":\"wrong_title\",\"title\":42,\"thumbnails\":[]}".to_owned(),
            "{\"alias\":\"blank_title\",\"title\":\"   \",\"thumbnails\":[]}".to_owned(),
        ]
        .join(",");
        let week_day = shelf_json(
            "weekday_a",
            "Weekday A",
            Some((
                "VERTICAL",
                "https://image.balcony.studio/BOMTOON_TW/co_thumbnail/weekday_a/1.webp",
            )),
        );
        let only_bom = concat!(
            "{\"alias\":\"only_a\",\"title\":\"Only A\",\"isAdult\":false,\"thumbnails\":[",
            "{\"type\":\"MAIN\",\"imagePath\":\"https://image.balcony.studio/tw/co_thumbnail/only_a/adult.webp\"},",
            "{\"type\":\"SQUARE\",\"imagePath\":\"https://image.balcony.studio/tw/co_thumbnail/only_a/public.webp\"}",
            "]}"
        )
        .to_owned();
        let main = homepage_main(&banners, &newest, &week_day, &only_bom);

        let parsed = homepage(homepage_document(&main).as_bytes()).expect("homepage");

        assert_eq!(
            parsed.banners,
            [
                BannerComic {
                    alias: "featured_a".into()
                },
                BannerComic {
                    alias: "featured_b".into()
                },
            ]
        );
        assert_eq!(
            parsed
                .newest
                .iter()
                .map(|comic| comic.alias.as_str())
                .collect::<Vec<_>>(),
            [
                "new_a",
                "new_none",
                "new_hostile",
                "missing_maturity",
                "adult_artwork",
            ]
        );
        assert_eq!(
            parsed.newest[0].cover_url.as_deref(),
            Some("https://image.balcony.studio:443/tw/contents/new_a.webp")
        );
        assert_eq!(parsed.newest[1].cover_url, None);
        assert_eq!(parsed.newest[2].cover_url, None);
        assert_eq!(parsed.newest[3].cover_url, None);
        assert_eq!(parsed.newest[4].cover_url, None);
        assert_eq!(parsed.week_day[0].alias, "weekday_a");
        assert_eq!(parsed.only_bom[0].alias, "only_a");
        assert_eq!(
            parsed.only_bom[0].cover_url.as_deref(),
            Some("https://image.balcony.studio/tw/co_thumbnail/only_a/public.webp")
        );
    }

    #[test]
    fn homepage_requires_each_exact_main_section() {
        for main in [
            r#"{"newest":[],"weekDay":[],"onlyBom":[]}"#,
            r#"{"banners":[],"weekDay":[],"onlyBom":[]}"#,
            r#"{"banners":[],"newest":[],"onlyBom":[]}"#,
            r#"{"banners":[],"newest":[],"weekDay":[]}"#,
            r#"{"banners":{},"newest":[],"weekDay":[],"onlyBom":[]}"#,
            r#"{"banners":[],"newest":{},"weekDay":[],"onlyBom":[]}"#,
            r#"{"banners":[],"newest":[],"weekDay":null,"onlyBom":[]}"#,
            r#"{"banners":[],"newest":[],"weekDay":[],"onlyBom":"wrong"}"#,
        ] {
            assert!(
                homepage(homepage_document(main).as_bytes()).is_err(),
                "{main}"
            );
        }
        assert!(homepage(br#"<script id="not-next-data">{}</script>"#).is_err());
    }

    #[test]
    fn homepage_rejects_remote_arrays_above_each_cap() {
        let banner = banner_json("CONTENTS", "COMIC", "safe");
        let accepted_banners = vec![banner.clone(); 64].join(",");
        let accepted = homepage_main(&accepted_banners, "", "", "");
        assert_eq!(
            homepage(homepage_document(&accepted).as_bytes())
                .expect("64 banners")
                .banners
                .len(),
            64
        );
        let rejected = homepage_main(&vec![banner; 65].join(","), "", "", "");
        assert!(homepage(homepage_document(&rejected).as_bytes()).is_err());

        let comic = shelf_json("safe", "Safe", None);
        let accepted_list = vec![comic.clone(); 64].join(",");
        let accepted = homepage_main("", &accepted_list, &accepted_list, &accepted_list);
        let accepted = homepage(homepage_document(&accepted).as_bytes()).expect("64 list items");
        assert_eq!(accepted.newest.len(), 64);
        assert_eq!(accepted.week_day.len(), 64);
        assert_eq!(accepted.only_bom.len(), 64);
        let rejected_list = vec![comic; 65].join(",");
        for main in [
            homepage_main("", &rejected_list, "", ""),
            homepage_main("", "", &rejected_list, ""),
            homepage_main("", "", "", &rejected_list),
        ] {
            assert!(homepage(homepage_document(&main).as_bytes()).is_err());
        }
    }

    #[test]
    fn homepage_bounds_retained_strings_by_utf8_bytes() {
        let alias_at_cap = "a".repeat(96);
        let title_at_cap = "é".repeat(128);
        let cover_prefix = "https://image.balcony.studio/tw/contents/";
        let cover_suffix = ".webp";
        let cover_at_cap = format!(
            "{cover_prefix}{}{cover_suffix}",
            "a".repeat(2048 - cover_prefix.len() - cover_suffix.len())
        );
        let entries = [
            shelf_json(&alias_at_cap, &title_at_cap, Some(("COVER", &cover_at_cap))),
            shelf_json(&"b".repeat(97), "Alias too long", None),
            shelf_json("title_too_long", &format!("{}a", "é".repeat(128)), None),
            shelf_json(
                "cover_too_long",
                "Cover too long",
                Some((
                    "COVER",
                    &format!(
                        "{cover_prefix}{}{cover_suffix}",
                        "a".repeat(2049 - cover_prefix.len() - cover_suffix.len())
                    ),
                )),
            ),
        ]
        .join(",");
        let main = homepage_main("", &entries, "", "");

        let parsed = homepage(homepage_document(&main).as_bytes()).expect("bounded homepage");

        assert_eq!(parsed.newest.len(), 2);
        assert_eq!(parsed.newest[0].alias.len(), 96);
        assert_eq!(parsed.newest[0].title.len(), 256);
        assert_eq!(
            parsed.newest[0].cover_url.as_deref(),
            Some(cover_at_cap.as_str())
        );
        assert_eq!(parsed.newest[1].alias, "cover_too_long");
        assert_eq!(parsed.newest[1].cover_url, None);
    }

    #[test]
    fn homepage_keeps_comics_but_rejects_hostile_public_image_urls() {
        let urls = [
            "http://image.balcony.studio/tw/contents/a.webp",
            "https://user@image.balcony.studio/tw/contents/a.webp",
            "https://image.balcony.studio.attacker.example/tw/contents/a.webp",
            "https://image.balcony.studio/tw/ep/a.webp",
            "https://image.balcony.studio/tw/contents/../a.webp",
            "https://image.balcony.studio/tw/contents/a.webp?token=secret",
            "https://image.balcony.studio/tw/contents/a.webp#fragment",
        ];
        let entries = urls
            .iter()
            .enumerate()
            .map(|(index, url)| {
                shelf_json(
                    &format!("comic_{index}"),
                    "Safe title",
                    Some(("COVER", url)),
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let main = homepage_main("", &entries, "", "");

        let parsed = homepage(homepage_document(&main).as_bytes()).expect("tolerant artwork");

        assert_eq!(parsed.newest.len(), urls.len());
        assert!(parsed.newest.iter().all(|comic| comic.cover_url.is_none()));
    }

    #[test]
    fn public_detail_ignores_whitespace_prefixed_tag_text() {
        let body = r#"
          < meta property="og:title" content="Fake - 漫畫 - BOMTOON">
          <meta property="og:title" content="Real - 漫畫 - BOMTOON">
        "#;

        let comic =
            public_detail(body.as_bytes(), "real").expect("whitespace-prefixed text is not a tag");
        assert_eq!(comic.title, "Real");
    }

    #[test]
    fn public_detail_ignores_complete_nested_template_content() {
        let body = r#"
          <template>
            <script>const close = '</template>';</script>
            <template></template>
            <meta property="og:title" content="Fake - 漫畫 - BOMTOON">
          </template>
          <meta property="og:title" content="Real - 漫畫 - BOMTOON">
        "#;

        let comic =
            public_detail(body.as_bytes(), "real").expect("nested template content is inert");
        assert_eq!(comic.title, "Real");
    }

    #[test]
    fn public_detail_ignores_malformed_template_closer() {
        let body = r#"
          <template>
            </template=ignored>
            <meta property="og:title" content="Fake - 漫畫 - BOMTOON">
          </template>
          <meta property="og:title" content="Real - 漫畫 - BOMTOON">
        "#;

        let comic =
            public_detail(body.as_bytes(), "real").expect("malformed closer does not end template");

        assert_eq!(comic.title, "Real");
    }

    #[test]
    fn public_detail_extracts_exact_open_graph_metadata_without_episodes() {
        let body = r#"<!doctype html><html><head>
          <!--<meta property="og:title" content="Commented - 漫畫 - BOMTOON">-->
          <title><meta property="og:title" content="Title text - 漫畫 - BOMTOON"></title>
          <textarea><meta property="og:title" content="Textarea - 漫畫 - BOMTOON"></textarea>
          <template><meta property="og:title" content="Template - 漫畫 - BOMTOON"></template>
          <script>const fake = '<meta property="og:title" content="Script - 漫畫 - BOMTOON">';</script>
          <meta content="ignored" name="og:title">
          <meta data-order="first" content="Hunter &amp; Q &mdash; Co. - 漫畫 - BOMTOON" property="og:title">
          <meta content="https://image.balcony.studio/tw/contents/hunter_q.webp" property="og:image">
          <script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"episodes":[{"title":"Not metadata"}],"title":"Wrong title"}}}</script>
        </head></html>"#;

        let comic = public_detail(body.as_bytes(), "hunter_q").expect("detail");

        assert_eq!(comic.alias, "hunter_q");
        assert_eq!(comic.title, "Hunter & Q — Co.");
        assert_eq!(
            comic.cover_url.as_deref(),
            Some("https://image.balcony.studio/tw/contents/hunter_q.webp")
        );
    }

    #[test]
    fn public_detail_preserves_alias_and_only_removes_the_exact_title_suffix() {
        let alias = "a".repeat(96);
        let body = r"<meta property='og:title' content='Hunter &quot;Q&quot; - BOMTOON'>";
        let comic = public_detail(body.as_bytes(), &alias).expect("detail");
        assert_eq!(comic.alias, alias);
        assert_eq!(comic.title, "Hunter \"Q\" - BOMTOON");

        let encoded_suffix = r#"<meta property="og:title" content="Hunter&#32;- 漫畫 - BOMTOON">"#;
        assert_eq!(
            public_detail(encoded_suffix.as_bytes(), "hunter_q")
                .expect("decoded suffix")
                .title,
            "Hunter"
        );
        let trailing_space = r#"<meta property="og:title" content="Hunter - 漫畫 - BOMTOON ">"#;
        assert_eq!(
            public_detail(trailing_space.as_bytes(), "hunter_q")
                .expect("non-exact suffix")
                .title,
            "Hunter - 漫畫 - BOMTOON "
        );
        let controls = format!(
            r#"<meta property="og:title" content="A&#27;B{}C &mdash; D - 漫畫 - BOMTOON">"#,
            '\u{1b}'
        );
        let controlled = public_detail(controls.as_bytes(), "hunter_q").expect("safe entities");
        assert_eq!(controlled.title, "A&#27;BC — D");
        assert!(!controlled.title.chars().any(super::invalid_control));

        assert!(public_detail(body.as_bytes(), &"a".repeat(97)).is_err());
        assert!(public_detail(body.as_bytes(), "bad/slash").is_err());
    }

    #[test]
    fn public_detail_requires_a_bounded_nonempty_title() {
        assert!(public_detail(
            br#"<meta property="og:image" content="https://image.balcony.studio/tw/contents/a.webp">"#,
            "safe",
        )
        .is_err());
        assert!(public_detail(
            r#"<meta property="og:title" content=" - 漫畫 - BOMTOON">"#.as_bytes(),
            "safe",
        )
        .is_err());

        let accepted = format!(
            r#"<meta property="og:title" content="{} - 漫畫 - BOMTOON">"#,
            "é".repeat(128)
        );
        assert_eq!(
            public_detail(accepted.as_bytes(), "safe")
                .expect("256-byte title")
                .title
                .len(),
            256
        );
        let rejected = format!(
            r#"<meta property="og:title" content="{}a - 漫畫 - BOMTOON">"#,
            "é".repeat(128)
        );
        assert!(public_detail(rejected.as_bytes(), "safe").is_err());
    }

    #[test]
    fn public_detail_tolerates_missing_or_hostile_images_as_none() {
        let title = r#"<meta property="og:title" content="Hunter Q - 漫畫 - BOMTOON">"#;
        assert_eq!(
            public_detail(title.as_bytes(), "hunter_q")
                .expect("missing image")
                .cover_url,
            None
        );

        for url in [
            "http://image.balcony.studio/tw/contents/hunter_q.webp",
            "https://attacker.example/tw/contents/hunter_q.webp",
            "https://image.balcony.studio/tw/ep/hunter_q.webp",
        ] {
            let body = format!(
                concat!(
                    "<meta content=\"Hunter Q - 漫畫 - BOMTOON\" property=\"og:title\">",
                    "<meta property=\"og:image\" content=\"{}\">"
                ),
                url
            );
            assert_eq!(
                public_detail(body.as_bytes(), "hunter_q")
                    .expect("hostile image is optional")
                    .cover_url,
                None
            );
        }
    }
}
