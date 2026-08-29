use kobo_sdk::{Credential, Header, Task};

const CONTENT_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/contents/";
const IMAGES_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/";
const LIBRARY_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/library";
const RECENT_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/library/recent";
const CONTENT_BYTES: u32 = 512 * 1024;
const IMAGE_MANIFEST_BYTES: u32 = 512 * 1024;
const LIBRARY_BYTES: u32 = 2 * 1024 * 1024;
const ACCEPT_LANGUAGE: &str = "zh-TW,zh;q=0.9,en-US;q=0.8,en;q=0.7";

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

pub fn content(alias: &str) -> Task {
    fetch(
        format!("{CONTENT_URL}{alias}?isNotLoginAdult=false&isPorch=false"),
        CONTENT_BYTES,
        Credential::bearer("bomtoon-access-token"),
        balcony_headers(),
    )
}

pub fn images(content: &str, episode: &str) -> Task {
    let mut headers = balcony_headers();
    headers.push(Header::new(
        "x-referer",
        format!("https://www.bomtoon.tw/viewer/{content}/{episode}"),
    ));
    fetch(
        format!("{IMAGES_URL}{content}/{episode}?imageWidth=1080"),
        IMAGE_MANIFEST_BYTES,
        Credential::bearer("bomtoon-access-token"),
        headers,
    )
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
    use super::{content, image, images, library, recent, ACCEPT_LANGUAGE};
    use kobo_sdk::{Credential, Header, SecretHeader, Task};

    #[test]
    fn content_uses_exact_bearer_json_endpoint() {
        let Task::Fetch {
            url,
            offset,
            max_bytes,
            credential,
            ..
        } = content("hunter_q")
        else {
            panic!("expected fetch task");
        };
        assert_eq!(
            url,
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/hunter_q?isNotLoginAdult=false&isPorch=false"
        );
        assert_eq!(offset, 0);
        assert_eq!(max_bytes, 512 * 1024);
        assert!(matches!(
            credential,
            Some(value)
                if value.secret == "bomtoon-access-token"
                    && value.header == SecretHeader::Bearer
        ));
    }

    #[test]
    fn content_request_uses_managed_bearer_and_json_headers() {
        let Task::Fetch {
            credential,
            headers,
            ..
        } = content("365")
        else {
            panic!("expected fetch task");
        };
        assert!(matches!(
            credential,
            Some(value)
                if value.secret == "bomtoon-access-token"
                    && value.header == SecretHeader::Bearer
        ));
        assert!(headers.iter().any(|header| {
            header.name.eq_ignore_ascii_case("accept") && header.value == "application/json"
        }));
        assert!(headers.iter().all(|header| {
            !header.name.eq_ignore_ascii_case("cookie")
                && !header.name.eq_ignore_ascii_case("authorization")
        }));
    }

    #[test]
    fn credentials_never_enter_urls_or_regular_headers() {
        for task in [content("hunter_q"), library(2), recent(0)] {
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
        for task in [content("365"), library(0), recent(0)] {
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
        } = images("hunter_q", "ep-1")
        else {
            panic!("expected manifest fetch");
        };
        assert_eq!(
            url,
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080"
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
