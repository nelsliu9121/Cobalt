use kobo_sdk::{Credential, Header, Task};

const SESSION_URL: &str = "https://www.bomtoon.tw/api/auth/session";
const DETAIL_URL: &str = "https://www.bomtoon.tw/detail/";
const LIBRARY_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/library";
const RECENT_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/library/recent";
const SESSION_BYTES: u32 = 128 * 1024;
const LIBRARY_BYTES: u32 = 2 * 1024 * 1024;
const DETAIL_BYTES: u32 = 4 * 1024 * 1024;
const ACCEPT_LANGUAGE: &str = "zh-TW,zh;q=0.9,en-US;q=0.8,en;q=0.7";

pub fn session() -> Task {
    fetch(
        SESSION_URL.to_owned(),
        SESSION_BYTES,
        Credential::in_header("bomtoon-session", "Cookie"),
        response_headers("*/*"),
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
        DETAIL_BYTES,
        Credential::in_header("bomtoon-session", "Cookie"),
        response_headers(
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        ),
    )
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
    use super::{detail, library, recent, session};
    use kobo_sdk::{SecretHeader, Task};

    #[test]
    fn credentials_never_enter_urls_or_regular_headers() {
        for task in [session(), library(2), recent(0), detail("hunter_q")] {
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
    fn each_request_uses_the_expected_secret_kind() {
        let Task::Fetch { credential, .. } = session() else {
            panic!("expected fetch task");
        };
        assert!(matches!(
            credential.map(|value| value.header),
            Some(SecretHeader::Named(name)) if name.eq_ignore_ascii_case("cookie")
        ));

        let Task::Fetch { credential, .. } = library(0) else {
            panic!("expected fetch task");
        };
        assert!(matches!(
            credential.map(|value| value.header),
            Some(SecretHeader::Bearer)
        ));
    }

    #[test]
    fn requests_use_endpoint_headers_without_browser_fingerprints() {
        let cases = [
            (session(), "*/*"),
            (library(0), "application/json"),
            (recent(0), "application/json"),
            (
                detail("365"),
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            ),
        ];
        for (task, expected_accept) in cases {
            let Task::Fetch { headers, .. } = task else {
                panic!("expected fetch task");
            };
            assert!(headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("accept")
                    && header.value == expected_accept));
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
}
