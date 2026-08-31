use crate::model::{AssetKind, PurchaseType};
use kobo_json::ObjectBuilder;
use kobo_sdk::{Credential, Header, Task};

const HOMEPAGE_URL: &str = "https://www.bomtoon.tw/comic/main";
const DETAIL_URL: &str = "https://www.bomtoon.tw/detail/";
const MAIN_API: &str = "https://www.bomtoon.tw/api/balcony-api-v2/contents/main/";
const THEME_API: &str = "https://www.bomtoon.tw/api/balcony-api-v2/theme";
const IMAGES_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/";
const LIBRARY_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/library";
const RECENT_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/library/recent";
const ASSET_URL: &str = "https://www.bomtoon.tw/api/balcony-api/asset/user";
const CHARGE_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge";
const GIFTS_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail";
const QUOTE_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/";
const PURCHASE_URL: &str = "https://www.bomtoon.tw/api/balcony-api/purchase";
const COMMENTS_URL: &str = "https://www.bomtoon.tw/api/balcony-api/comment/contents/";
const REPLIES_URL: &str = "https://www.bomtoon.tw/api/balcony-api/comment/reply/CONTENTS/";
const PUBLIC_HTML_BYTES: u32 = 512 * 1024;
const PUBLIC_COLLECTION_BYTES: u32 = 512 * 1024;
const IMAGE_MANIFEST_BYTES: u32 = 512 * 1024;
const LIBRARY_BYTES: u32 = 2 * 1024 * 1024;
const ASSET_SUMMARY_BYTES: u32 = 64 * 1024;
const ASSET_HISTORY_BYTES: u32 = 512 * 1024;
const COMMERCE_BYTES: u32 = 64 * 1024;
const COMMENT_BYTES: u32 = 512 * 1024;
const ACCEPT_LANGUAGE: &str = "zh-TW,zh;q=0.9,en-US;q=0.8,en;q=0.7";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommentOrder {
    Hot,
    Newest,
}

impl CommentOrder {
    const fn is_best(self) -> bool {
        matches!(self, Self::Hot)
    }
}

pub fn homepage() -> Task {
    public_fetch(HOMEPAGE_URL.to_owned())
}

pub fn public_detail(alias: &str) -> Task {
    public_fetch(format!("{DETAIL_URL}{alias}"))
}

pub fn ranking() -> Task {
    public_json_fetch(format!("{MAIN_API}ranking/COMIC?adultToggle=true&contentsThumbnailType=VERTICAL,MAIN,SQUARE,DETAIL,HORIZONTAL_TYPE_A&mainGenre=ALL"))
}

pub fn most_favorited() -> Task {
    public_json_fetch(format!("{MAIN_API}favorite/COMIC?adultToggle=true&contentsThumbnailType=VERTICAL,MAIN,SQUARE,VERTICAL_NON_ADULT&mainGenre=ALL"))
}

pub fn themes() -> Task {
    public_json_fetch(format!(
        "{THEME_API}?isIncludeAdult=true&displayRange=COMIC&displayPosition="
    ))
}

pub fn freetime() -> Task {
    public_json_fetch(format!("{MAIN_API}free/COMIC?adultToggle=true&contentsFreeFilter=FREETIME&contentsThumbnailType=VERTICAL,MAIN,SQUARE,VERTICAL_NON_ADULT&mainGenre=ALL"))
}

pub fn asset_summary() -> Task {
    fetch(
        ASSET_URL.to_owned(),
        ASSET_SUMMARY_BYTES,
        Credential::bearer("bomtoon-access-token"),
        balcony_headers(),
    )
}

pub fn expiration_history(kind: AssetKind, created_at: i64) -> Task {
    let coin_kind = match kind {
        AssetKind::Coin => "COIN",
        AssetKind::Ticket => "TICKET",
    };
    fetch(
        format!("{CHARGE_URL}?createdAt={created_at}&sort=EXPIRE&coinKind={coin_kind}"),
        ASSET_HISTORY_BYTES,
        Credential::bearer("bomtoon-access-token"),
        balcony_headers(),
    )
}

pub fn library(page: usize) -> Task {
    fetch(
        format!(
            "{LIBRARY_URL}?sort=CREATE&page={page}&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE"
        ),
        LIBRARY_BYTES,
        Credential::bearer("bomtoon-access-token"),
        balcony_headers(),
    )
}

pub fn recent(page: usize) -> Task {
    let mut headers = balcony_headers();
    headers.push(Header::new(
        "x-referer",
        "https://www.bomtoon.tw/my/library/recent",
    ));
    fetch(
        format!(
            "{RECENT_URL}?sort=CREATE&page={page}&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE"
        ),
        LIBRARY_BYTES,
        Credential::bearer("bomtoon-access-token"),
        headers,
    )
}

pub fn detail(alias: &str) -> Task {
    fetch(
        format!("{DETAIL_URL}{alias}"),
        PUBLIC_HTML_BYTES,
        Credential::in_header("bomtoon-session", "Cookie"),
        response_headers("text/html"),
    )
}

pub fn images(content: &str, episode: &str, panel_width: u32) -> Task {
    let mut headers = balcony_headers();
    headers.push(Header::new(
        "x-referer",
        format!("https://www.bomtoon.tw/viewer/{content}/{episode}"),
    ));
    fetch(
        format!("{IMAGES_URL}{content}/{episode}?imageWidth={panel_width}"),
        IMAGE_MANIFEST_BYTES,
        Credential::bearer("bomtoon-access-token"),
        headers,
    )
}

pub fn comments(content: &str, episode: &str, order: CommentOrder, page: usize) -> Task {
    let mut headers = balcony_headers();
    headers.push(Header::new(
        "x-referer",
        format!("https://www.bomtoon.tw/comment/{content}/{episode}"),
    ));
    fetch(
        format!(
            "{COMMENTS_URL}{content}/{episode}?isBest={}&page={page}",
            order.is_best()
        ),
        COMMENT_BYTES,
        Credential::bearer("bomtoon-access-token"),
        headers,
    )
}

pub fn replies(comment_id: usize, order: CommentOrder, page: usize) -> Task {
    let mut headers = balcony_headers();
    headers.push(Header::new(
        "x-referer",
        format!("https://www.bomtoon.tw/comment/reply/{comment_id}"),
    ));
    fetch(
        format!(
            "{REPLIES_URL}{comment_id}?isBest={}&page={page}",
            order.is_best()
        ),
        COMMENT_BYTES,
        Credential::bearer("bomtoon-access-token"),
        headers,
    )
}

pub fn account_scope() -> Task {
    Task::CredentialScope {
        credential: "bomtoon-access-token".to_owned(),
    }
}

pub fn title_gifts(content_id: usize) -> Task {
    fetch(
        format!("{GIFTS_URL}?contentsId={content_id}"),
        COMMERCE_BYTES,
        Credential::bearer("bomtoon-access-token"),
        balcony_headers(),
    )
}

pub fn quote(content_alias: &str, episode_alias: &str, purchase: PurchaseType) -> Task {
    let purchase_type = match purchase {
        PurchaseType::RentGift | PurchaseType::Rent => PurchaseType::Rent.as_remote(),
        PurchaseType::Possession => PurchaseType::Possession.as_remote(),
    };
    fetch(
        format!("{QUOTE_URL}{content_alias}/{episode_alias}?purchaseType={purchase_type}"),
        COMMERCE_BYTES,
        Credential::bearer("bomtoon-access-token"),
        balcony_headers(),
    )
}

pub fn purchase(content_alias: &str, episode_id: usize, purchase: PurchaseType) -> Task {
    let episode_id_text = episode_id.to_string();
    let episode_id = kobo_json::parse(&episode_id_text).expect("a usize is always a JSON integer");
    let body = ObjectBuilder::new()
        .set("id", episode_id)
        .set("purchaseType", purchase.as_remote())
        .set("isMobile", false)
        .build()
        .to_json();
    let mut headers = balcony_headers();
    headers.push(Header::new(
        "x-referer",
        format!("{DETAIL_URL}{content_alias}"),
    ));
    Task::Post {
        url: PURCHASE_URL.to_owned(),
        body,
        content_type: "application/json".to_owned(),
        credential: Some(Credential::bearer("bomtoon-access-token")),
        headers,
        max_bytes: COMMERCE_BYTES,
    }
}

pub fn image(url: &str) -> Task {
    Task::Fetch {
        url: url.to_owned(),
        offset: 0,
        max_bytes: u32::try_from(kobo_image::MAX_SOURCE_BYTES)
            .expect("the image source byte limit must fit in u32"),
        credential: None,
        headers: response_headers("image/webp"),
    }
}

pub fn logout() -> Task {
    Task::RevokeCredential {
        credential: "bomtoon-access-token".to_owned(),
    }
}

fn response_headers(accept: &str) -> Vec<Header> {
    vec![
        Header::new("Accept", accept),
        Header::new("Accept-Language", ACCEPT_LANGUAGE),
    ]
}

fn balcony_headers() -> Vec<Header> {
    let mut headers = response_headers("application/json");
    headers.extend([
        Header::new("x-balcony-id", "BOMTOON_TW"),
        Header::new("x-balcony-timezone", "Asia/Taipei"),
        Header::new("x-platform", "MOBILE_IOS"),
    ]);
    headers
}

fn public_json_fetch(url: String) -> Task {
    Task::Fetch {
        url,
        offset: 0,
        max_bytes: PUBLIC_COLLECTION_BYTES,
        credential: None,
        headers: balcony_headers(),
    }
}

fn public_fetch(url: String) -> Task {
    Task::Fetch {
        url,
        offset: 0,
        max_bytes: PUBLIC_HTML_BYTES,
        credential: None,
        headers: response_headers("text/html"),
    }
}

fn fetch(url: String, max_bytes: u32, credential: Credential, headers: Vec<Header>) -> Task {
    Task::Fetch {
        url,
        offset: 0,
        max_bytes,
        credential: Some(credential),
        headers,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        account_scope, asset_summary, comments, detail, expiration_history, freetime, homepage,
        image, images, library, most_favorited, public_detail, purchase, quote, ranking, recent,
        replies, themes, title_gifts, CommentOrder, ACCEPT_LANGUAGE, PUBLIC_COLLECTION_BYTES,
    };
    use crate::model::{AssetKind, PurchaseType};
    use kobo_sdk::{Credential, Header, SecretHeader, Task};

    #[test]
    fn homepage_is_public_and_bounded() {
        let Task::Fetch {
            url,
            offset,
            max_bytes,
            credential,
            headers,
        } = homepage()
        else {
            panic!("homepage must be a fetch");
        };
        assert_eq!(url, "https://www.bomtoon.tw/comic/main");
        assert_eq!(offset, 0);
        assert_eq!(max_bytes, 512 * 1024);
        assert_eq!(credential, None);
        assert_eq!(
            headers,
            vec![
                Header::new("Accept", "text/html"),
                Header::new("Accept-Language", ACCEPT_LANGUAGE),
            ]
        );
    }

    #[test]
    fn public_detail_is_public_and_exact() {
        let Task::Fetch {
            url,
            offset,
            max_bytes,
            credential,
            headers,
        } = public_detail("hunter_q")
        else {
            panic!("detail must be a fetch");
        };
        assert_eq!(url, "https://www.bomtoon.tw/detail/hunter_q");
        assert_eq!(offset, 0);
        assert_eq!(max_bytes, 512 * 1024);
        assert_eq!(credential, None);
        assert_eq!(
            headers,
            vec![
                Header::new("Accept", "text/html"),
                Header::new("Accept-Language", ACCEPT_LANGUAGE),
            ]
        );
    }

    #[test]
    fn feature_collection_requests_are_public_and_exact() {
        let expected_headers = vec![
            Header::new("Accept", "application/json"),
            Header::new("Accept-Language", ACCEPT_LANGUAGE),
            Header::new("x-balcony-id", "BOMTOON_TW"),
            Header::new("x-balcony-timezone", "Asia/Taipei"),
            Header::new("x-platform", "MOBILE_IOS"),
        ];
        for (task, expected_url) in [
            (
                ranking(),
                concat!(
                    "https://www.bomtoon.tw/api/balcony-api-v2/contents/main/ranking/COMIC",
                    "?adultToggle=true",
                    "&contentsThumbnailType=VERTICAL,MAIN,SQUARE,DETAIL,HORIZONTAL_TYPE_A",
                    "&mainGenre=ALL"
                ),
            ),
            (
                most_favorited(),
                concat!(
                    "https://www.bomtoon.tw/api/balcony-api-v2/contents/main/favorite/COMIC",
                    "?adultToggle=true",
                    "&contentsThumbnailType=VERTICAL,MAIN,SQUARE,VERTICAL_NON_ADULT",
                    "&mainGenre=ALL"
                ),
            ),
            (
                themes(),
                concat!(
                    "https://www.bomtoon.tw/api/balcony-api-v2/theme",
                    "?isIncludeAdult=true&displayRange=COMIC&displayPosition="
                ),
            ),
            (
                freetime(),
                concat!(
                    "https://www.bomtoon.tw/api/balcony-api-v2/contents/main/free/COMIC",
                    "?adultToggle=true&contentsFreeFilter=FREETIME",
                    "&contentsThumbnailType=VERTICAL,MAIN,SQUARE,VERTICAL_NON_ADULT",
                    "&mainGenre=ALL"
                ),
            ),
        ] {
            let Task::Fetch {
                url,
                offset,
                max_bytes,
                credential,
                headers,
            } = task
            else {
                panic!("feature collection request must be a fetch");
            };
            assert_eq!(url, expected_url);
            assert_eq!(offset, 0);
            assert_eq!(max_bytes, PUBLIC_COLLECTION_BYTES);
            assert_eq!(credential, None);
            assert_eq!(headers, expected_headers);
        }
    }

    #[test]
    fn detail_uses_managed_session_cookie_html_endpoint() {
        let Task::Fetch {
            url,
            offset,
            max_bytes,
            credential,
            headers,
        } = detail("365")
        else {
            panic!("detail must be a fetch");
        };
        assert_eq!(url, "https://www.bomtoon.tw/detail/365");
        assert_eq!(offset, 0);
        assert_eq!(max_bytes, 512 * 1024);
        assert_eq!(
            credential,
            Some(Credential::in_header("bomtoon-session", "Cookie"))
        );
        assert_eq!(
            headers,
            vec![
                Header::new("Accept", "text/html"),
                Header::new("Accept-Language", ACCEPT_LANGUAGE),
            ]
        );
        assert!(headers.iter().all(|header| {
            !header.name.eq_ignore_ascii_case("cookie")
                && !header.name.eq_ignore_ascii_case("authorization")
        }));
    }

    #[test]
    fn comments_use_observed_order_endpoint_and_referer() {
        for (order, flag) in [(CommentOrder::Hot, "true"), (CommentOrder::Newest, "false")] {
            let Task::Fetch {
                url,
                max_bytes,
                credential,
                headers,
                ..
            } = comments("365", "1", order, 2)
            else {
                panic!("expected fetch task");
            };
            assert_eq!(
                url,
                format!(
                    "https://www.bomtoon.tw/api/balcony-api/comment/contents/365/1?isBest={flag}&page=2"
                )
            );
            assert_eq!(max_bytes, 512 * 1024);
            assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
            assert!(headers.iter().any(|header| {
                header.name.eq_ignore_ascii_case("x-referer")
                    && header.value == "https://www.bomtoon.tw/comment/365/1"
            }));
        }
    }

    #[test]
    fn replies_use_observed_contents_route_and_referer() {
        let Task::Fetch {
            url,
            max_bytes,
            credential,
            headers,
            ..
        } = replies(354_980, CommentOrder::Hot, 3)
        else {
            panic!("expected fetch task");
        };
        assert_eq!(
            url,
            "https://www.bomtoon.tw/api/balcony-api/comment/reply/CONTENTS/354980?isBest=true&page=3"
        );
        assert_eq!(max_bytes, 512 * 1024);
        assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
        assert!(headers.iter().any(|header| {
            header.name.eq_ignore_ascii_case("x-referer")
                && header.value == "https://www.bomtoon.tw/comment/reply/354980"
        }));
    }

    #[test]
    fn asset_summary_uses_exact_bearer_endpoint_and_ceiling() {
        let Task::Fetch {
            url,
            offset,
            max_bytes,
            credential,
            headers,
        } = asset_summary()
        else {
            panic!("expected fetch task");
        };
        assert_eq!(url, "https://www.bomtoon.tw/api/balcony-api/asset/user");
        assert_eq!(offset, 0);
        assert_eq!(max_bytes, 64 * 1024);
        assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
        assert!(headers.iter().any(|header| {
            header.name.eq_ignore_ascii_case("accept") && header.value == "application/json"
        }));
    }

    #[test]
    fn expiration_history_fixes_kind_sort_window_and_ceiling() {
        for (kind, value) in [(AssetKind::Coin, "COIN"), (AssetKind::Ticket, "TICKET")] {
            let Task::Fetch {
                url,
                max_bytes,
                credential,
                ..
            } = expiration_history(kind, 1_725_000_000_000)
            else {
                panic!("expected fetch task");
            };
            assert_eq!(
                url,
                format!(
                    "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind={value}"
                )
            );
            assert_eq!(max_bytes, 512 * 1024);
            assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
        }
    }

    #[test]
    fn credentials_never_enter_urls_or_regular_headers() {
        for task in [
            detail("hunter_q"),
            library(2),
            recent(0),
            asset_summary(),
            expiration_history(AssetKind::Coin, 1_725_000_000_000),
            expiration_history(AssetKind::Ticket, 1_725_000_000_000),
        ] {
            let Task::Fetch {
                url,
                credential,
                headers,
                ..
            } = task
            else {
                panic!("expected fetch task");
            };
            assert!(!url.to_ascii_lowercase().contains("token"));
            assert!(headers
                .iter()
                .all(|header| !header.name.eq_ignore_ascii_case("authorization")));
            assert!(credential.is_some());
        }
    }

    #[test]
    fn requests_use_endpoint_headers_without_browser_fingerprints() {
        for task in [library(0), recent(0)] {
            let Task::Fetch { headers, .. } = task else {
                panic!("expected fetch task");
            };
            assert!(headers.iter().any(|header| {
                header.name.eq_ignore_ascii_case("accept") && header.value == "application/json"
            }));
            assert!(headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("accept-language")));
            assert!(headers.iter().all(|header| {
                let name = header.name.to_ascii_lowercase();
                name != "user-agent" && !name.starts_with("sec-")
            }));
        }
    }

    #[test]
    fn recent_reading_uses_the_observed_route_and_referer() {
        let Task::Fetch {
            url,
            credential,
            headers,
            ..
        } = recent(0)
        else {
            panic!("expected fetch task");
        };
        assert_eq!(
            url,
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE"
        );
        assert!(matches!(
            credential.map(|value| value.header),
            Some(SecretHeader::Bearer)
        ));
        assert!(headers.iter().any(|header| {
            header.name.eq_ignore_ascii_case("x-referer")
                && header.value == "https://www.bomtoon.tw/my/library/recent"
        }));
    }

    #[test]
    fn image_manifest_uses_exact_bearer_route_headers_and_ceiling() {
        let Task::Fetch {
            url,
            offset,
            max_bytes,
            credential,
            headers,
        } = images("hunter_q", "ep-1", 1072)
        else {
            panic!("expected manifest fetch");
        };
        assert_eq!(
            url,
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1072"
        );
        assert_eq!(offset, 0);
        assert_eq!(max_bytes, 512 * 1024);
        assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
        assert_eq!(
            headers,
            vec![
                Header::new("Accept", "application/json"),
                Header::new("Accept-Language", ACCEPT_LANGUAGE),
                Header::new("x-balcony-id", "BOMTOON_TW"),
                Header::new("x-balcony-timezone", "Asia/Taipei"),
                Header::new("x-platform", "MOBILE_IOS"),
                Header::new("x-referer", "https://www.bomtoon.tw/viewer/hunter_q/ep-1"),
            ]
        );
    }

    #[test]
    fn account_scope_uses_the_managed_bomtoon_credential() {
        assert_eq!(
            account_scope(),
            Task::CredentialScope {
                credential: "bomtoon-access-token".to_owned(),
            }
        );
    }

    #[test]
    fn title_gifts_uses_exact_bearer_route_headers_and_ceiling() {
        let Task::Fetch {
            url,
            offset,
            max_bytes,
            credential,
            headers,
        } = title_gifts(41)
        else {
            panic!("expected Gift fetch");
        };
        assert_eq!(
            url,
            "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentsId=41"
        );
        assert_eq!(offset, 0);
        assert_eq!(max_bytes, 64 * 1024);
        assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
        assert_eq!(
            headers,
            vec![
                Header::new("Accept", "application/json"),
                Header::new("Accept-Language", ACCEPT_LANGUAGE),
                Header::new("x-balcony-id", "BOMTOON_TW"),
                Header::new("x-balcony-timezone", "Asia/Taipei"),
                Header::new("x-platform", "MOBILE_IOS"),
            ]
        );
    }

    #[test]
    fn quotes_use_exact_action_specific_route_and_bounded_fetch() {
        for (purchase_type, remote) in [
            (PurchaseType::RentGift, "RENT"),
            (PurchaseType::Rent, "RENT"),
            (PurchaseType::Possession, "POSSESSION"),
        ] {
            let Task::Fetch {
                url,
                offset,
                max_bytes,
                credential,
                headers,
            } = quote("hunter_q", "ep-1", purchase_type)
            else {
                panic!("expected quote fetch");
            };
            assert_eq!(
                url,
                format!(
                    "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType={remote}"
                )
            );
            assert_eq!(offset, 0);
            assert_eq!(max_bytes, 64 * 1024);
            assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
            assert_eq!(
                headers,
                vec![
                    Header::new("Accept", "application/json"),
                    Header::new("Accept-Language", ACCEPT_LANGUAGE),
                    Header::new("x-balcony-id", "BOMTOON_TW"),
                    Header::new("x-balcony-timezone", "Asia/Taipei"),
                    Header::new("x-platform", "MOBILE_IOS"),
                ]
            );
        }
    }

    #[test]
    fn purchases_use_exact_json_mutation_and_no_application_secrets() {
        for (purchase_type, remote) in [
            (PurchaseType::RentGift, "RENT_GIFT"),
            (PurchaseType::Rent, "RENT"),
            (PurchaseType::Possession, "POSSESSION"),
        ] {
            let Task::Post {
                url,
                body,
                content_type,
                credential,
                headers,
                max_bytes,
            } = purchase("hunter_q", 6800, purchase_type)
            else {
                panic!("expected purchase POST");
            };
            assert_eq!(url, "https://www.bomtoon.tw/api/balcony-api/purchase");
            assert_eq!(
                body,
                format!(r#"{{"id":6800,"purchaseType":"{remote}","isMobile":false}}"#)
            );
            assert_eq!(content_type, "application/json");
            assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
            assert_eq!(max_bytes, 64 * 1024);
            assert_eq!(
                headers,
                vec![
                    Header::new("Accept", "application/json"),
                    Header::new("Accept-Language", ACCEPT_LANGUAGE),
                    Header::new("x-balcony-id", "BOMTOON_TW"),
                    Header::new("x-balcony-timezone", "Asia/Taipei"),
                    Header::new("x-platform", "MOBILE_IOS"),
                    Header::new("x-referer", "https://www.bomtoon.tw/detail/hunter_q"),
                ]
            );
            assert!(headers.iter().all(|header| {
                !header.name.eq_ignore_ascii_case("cookie")
                    && !header.name.eq_ignore_ascii_case("origin")
                    && !header.name.eq_ignore_ascii_case("authorization")
            }));
        }
    }

    #[test]
    fn signed_image_fetch_has_no_account_credential() {
        let signed_url =
            "https://image.balcony.studio/tw/ep/one.webp?Policy=p&Signature=s&Key-Pair-Id=k";
        let Task::Fetch {
            url,
            offset,
            max_bytes,
            credential,
            headers,
        } = image(signed_url)
        else {
            panic!("expected image fetch");
        };
        assert!(url == signed_url, "image URL mismatch");
        assert_eq!(offset, 0);
        assert_eq!(kobo_image::MAX_SOURCE_BYTES, 4 * 1024 * 1024);
        assert_eq!(
            max_bytes,
            u32::try_from(4 * 1024 * 1024).expect("the BOMTOON image limit fits in u32")
        );
        assert_eq!(credential, None);
        assert_eq!(
            headers,
            vec![
                Header::new("Accept", "image/webp"),
                Header::new("Accept-Language", ACCEPT_LANGUAGE),
            ]
        );
    }
}
