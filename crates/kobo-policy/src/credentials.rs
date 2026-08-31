//! Platform-owned allowlists for attaching stored credentials to requests.
//!
//! [`kobo_net`] supplies generic HTTPS transport and URL primitives. This
//! module owns the shipped applications' identities and provider contracts so
//! an application cannot broaden the destinations, methods, or headers that a
//! stored secret may use.

use kobo_net::{has_origin, parse};
use kobo_protocol::{Credential, CredentialUse, SecretHeader};

/// The narration voices the audiobook application may spend its `ElevenLabs`
/// key on: one per offered language, native accents. The application holds
/// the same list in its pipeline; a voice added there must be added here.
const AUDIOBOOK_VOICES: [&str; 6] = [
    "JBFqnCBsd6RMkjVDRZzb", // George, English
    "1qEiC6qsybMkmnNdVMbK", // Monika Sogam, Hindi
    "l1zE9xgNpUTaQCZzpNJa", // Alberto Rodríguez, Spanish
    "aQROLel5sQbj1vuIVi6B", // Nicolas, French
    "7eVMgwCnXydb3CikjV7a", // Lea, German
    "4VZIsMPtgggwNg7OXbPY", // James Gao, Chinese
];

/// Whether a shipped application may attach one named secret to this request.
///
/// The runtime calls this immediately before resolving the secret. Policies
/// are default-deny and bind a runtime-verified app ID to the credential name,
/// header convention, request kind, exact HTTPS origin, path, and query.
#[must_use]
pub fn allowed(app: &str, credential: &Credential, url: &str, usage: CredentialUse) -> bool {
    if app == "bomtoon" {
        return bomtoon_credential_allowed(credential, url, usage);
    }
    if app == "zotero-reader" {
        return usage == CredentialUse::Fetch && zotero_credential_allowed(credential, url);
    }
    if app == "audiobook" {
        return match (&*credential.secret, &credential.header) {
            ("exa", SecretHeader::Named(header)) => {
                header.eq_ignore_ascii_case("x-api-key")
                    && url == "https://api.exa.ai/agent/runs"
                    && has_origin(url, "api.exa.ai", 443)
            }
            ("openai", SecretHeader::Bearer) => {
                url == "https://api.openai.com/v1/responses"
                    && has_origin(url, "api.openai.com", 443)
            }
            ("elevenlabs", SecretHeader::Named(header)) => {
                header.eq_ignore_ascii_case("xi-api-key")
                    && AUDIOBOOK_VOICES.iter().any(|voice| {
                        url == format!(
                            "https://api.elevenlabs.io/v1/text-to-speech/{voice}?output_format=mp3_44100_128"
                        )
                    })
                    && has_origin(url, "api.elevenlabs.io", 443)
            }
            _ => false,
        };
    }
    if app != "chat" {
        return false;
    }
    match (&*credential.secret, &credential.header) {
        ("openai", SecretHeader::Bearer) => {
            url == "https://api.openai.com/v1/chat/completions"
                && has_origin(url, "api.openai.com", 443)
        }
        ("anthropic", SecretHeader::Named(header)) => {
            header.eq_ignore_ascii_case("x-api-key")
                && url == "https://api.anthropic.com/v1/messages"
                && has_origin(url, "api.anthropic.com", 443)
        }
        ("gemini", SecretHeader::Named(header)) => {
            header.eq_ignore_ascii_case("x-goog-api-key")
                && url
                    == "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent"
                && has_origin(url, "generativelanguage.googleapis.com", 443)
        }
        _ => false,
    }
}

/// Binds a dedicated Zotero key to the exact read endpoints used by Zotero
/// Reader. The app cannot send it to group libraries, key-management routes,
/// file downloads, arbitrary queries, or a lookalike origin.
fn bomtoon_credential_allowed(credential: &Credential, url: &str, usage: CredentialUse) -> bool {
    if credential.secret == "bomtoon-session" {
        return usage == CredentialUse::Fetch
            && matches!(
                &credential.header,
                SecretHeader::Named(header) if header.eq_ignore_ascii_case("cookie")
            )
            && bomtoon_detail_url(url);
    }
    if !matches!(
        (&*credential.secret, &credential.header),
        ("bomtoon-access-token", SecretHeader::Bearer)
    ) || url.contains('#')
    {
        return false;
    }
    let Ok(address) = parse(url) else {
        return false;
    };
    let Some(authority) = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/').map(|(authority, _)| authority))
    else {
        return false;
    };
    if !authority.eq_ignore_ascii_case("www.bomtoon.tw") {
        return false;
    }
    match usage {
        CredentialUse::Fetch => {
            bomtoon_existing_get(url)
                || bomtoon_asset_url(&address.path)
                || bomtoon_history_url(&address.path)
                || bomtoon_gift_url(&address.path)
                || bomtoon_quote_url(&address.path)
        }
        CredentialUse::Post => bomtoon_purchase_url(&address.path),
    }
}

fn bomtoon_alias(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn bomtoon_detail_url(url: &str) -> bool {
    const PREFIX: &str = "https://www.bomtoon.tw/detail/";
    const MAX_ALIAS_BYTES: usize = 96;

    url.strip_prefix(PREFIX)
        .is_some_and(|alias| alias.len() <= MAX_ALIAS_BYTES && bomtoon_alias(alias))
}

fn bomtoon_commerce_alias(value: &str) -> bool {
    value.len() <= 128 && bomtoon_alias(value)
}

fn bomtoon_existing_get(url: &str) -> bool {
    bomtoon_asset_summary_url(url)
        || bomtoon_library_url(url)
        || bomtoon_recent_url(url)
        || bomtoon_content_url(url)
        || bomtoon_images_url(url)
}

fn bomtoon_asset_url(path: &str) -> bool {
    path == "/api/balcony-api-v2/asset/user"
}

fn bomtoon_history_url(path: &str) -> bool {
    const PREFIX: &str = "/api/balcony-api-v2/payment/charge?createdAt=";
    const SUFFIX: &str = "&sort=EXPIRE&coinKind=";

    let Some((created_at, coin_kind)) = path
        .strip_prefix(PREFIX)
        .and_then(|rest| rest.split_once(SUFFIX))
    else {
        return false;
    };
    !created_at.is_empty()
        && created_at.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(coin_kind, "COIN" | "TICKET")
}

fn bomtoon_gift_url(path: &str) -> bool {
    const PREFIX: &str = "/api/balcony-api-v2/gift/contents/detail?contentsId=";

    path.strip_prefix(PREFIX).is_some_and(|content_id| {
        !content_id.is_empty() && content_id.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn bomtoon_quote_url(path: &str) -> bool {
    const PREFIX: &str = "/api/balcony-api-v2/contents/price/";

    let Some((aliases, purchase_type)) = path
        .strip_prefix(PREFIX)
        .and_then(|rest| rest.split_once("?purchaseType="))
    else {
        return false;
    };
    matches!(purchase_type, "RENT" | "POSSESSION")
        && aliases.split_once('/').is_some_and(|(content, episode)| {
            !episode.contains('/')
                && bomtoon_commerce_alias(content)
                && bomtoon_commerce_alias(episode)
        })
}

fn bomtoon_purchase_url(path: &str) -> bool {
    path == "/api/balcony-api/purchase"
}

fn bomtoon_content_url(url: &str) -> bool {
    const PREFIX: &str = "https://www.bomtoon.tw/api/balcony-api-v2/contents/";
    const SUFFIX: &str = "?isNotLoginAdult=false&isPorch=false";

    url.strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix(SUFFIX))
        .is_some_and(bomtoon_alias)
}

fn bomtoon_images_url(url: &str) -> bool {
    const PREFIX: &str = "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/";

    let Some((aliases, query)) = url
        .strip_prefix(PREFIX)
        .and_then(|rest| rest.split_once('?'))
    else {
        return false;
    };
    let Some(width) = query.strip_prefix("imageWidth=") else {
        return false;
    };
    if width.is_empty() || !width.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Ok(width) = width.parse::<u16>() else {
        return false;
    };
    if !(1..=4096).contains(&width) {
        return false;
    }

    aliases.split_once('/').is_some_and(|(content, episode)| {
        !episode.contains('/') && bomtoon_alias(content) && bomtoon_alias(episode)
    })
}

fn bomtoon_asset_summary_url(url: &str) -> bool {
    url == "https://www.bomtoon.tw/api/balcony-api/asset/user"
}

fn bomtoon_library_url(url: &str) -> bool {
    const PREFIX: &str = "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=";
    const SUFFIX: &str = "&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE";

    url.strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix(SUFFIX))
        .is_some_and(|page| !page.is_empty() && page.bytes().all(|byte| byte.is_ascii_digit()))
}

fn bomtoon_recent_url(url: &str) -> bool {
    const PREFIX: &str =
        "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=";
    const SUFFIX: &str =
        "&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE";

    url.strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix(SUFFIX))
        .is_some_and(|page| !page.is_empty() && page.bytes().all(|byte| byte.is_ascii_digit()))
}

fn zotero_credential_allowed(credential: &Credential, url: &str) -> bool {
    if credential.secret != "zotero" || credential.header != SecretHeader::Bearer {
        return false;
    }
    parse(url).is_ok_and(|target| {
        target.host.eq_ignore_ascii_case("api.zotero.org")
            && target.port == 443
            && zotero_read_api_path(&target.path)
    })
}

fn zotero_read_api_path(path_and_query: &str) -> bool {
    if path_and_query.contains(['%', '\\']) {
        return false;
    }
    let Some(path_and_query) = path_and_query.strip_prefix('/') else {
        return false;
    };
    if path_and_query.starts_with('/') {
        return false;
    }
    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 3
        || parts[0] != "users"
        || parts[1].is_empty()
        || parts[1].len() > 20
        || !parts[1].bytes().all(|byte| byte.is_ascii_digit())
        || parts.iter().any(|part| matches!(*part, "." | ".."))
    {
        return false;
    }
    match parts.as_slice() {
        ["users", _, "collections"] => {
            query == Some("format=json&limit=100&sort=title&direction=asc")
        }
        ["users", _, "collections", collection, "items", "top"] if zotero_key(collection) => {
            let Some(query) = query else {
                return false;
            };
            let fields: Vec<&str> = query.split('&').collect();
            if fields.len() != 6
                || fields[0] != "format=json"
                || fields[1] != "itemType=-attachment"
                || fields[4] != "sort=dateAdded"
                || fields[5] != "direction=desc"
            {
                return false;
            }
            let Some(limit) = fields[2].strip_prefix("limit=") else {
                return false;
            };
            let Some(start) = fields[3].strip_prefix("start=") else {
                return false;
            };
            let Ok(start) = start.parse::<usize>() else {
                return false;
            };
            (limit == "25" && start < 500 && start % 25 == 0) || (limit == "1" && start == 500)
        }
        ["users", _, "items", item] if zotero_key(item) => query == Some("format=json"),
        ["users", _, "items", item, "children"] if zotero_key(item) => {
            query == Some("format=json&itemType=attachment&limit=100")
        }
        ["users", _, "items", item, "fulltext"] if zotero_key(item) => query.is_none(),
        _ => false,
    }
}

fn zotero_key(value: &str) -> bool {
    value.len() == 8
        && value
            .bytes()
            .all(|byte| matches!(byte, b'2'..=b'9' | b'A'..=b'N' | b'P'..=b'Z'))
}

#[cfg(test)]
mod tests {
    use super::{allowed, AUDIOBOOK_VOICES};
    use kobo_protocol::{Credential, CredentialUse};

    #[derive(Clone, Copy, Debug)]
    enum RequestMethod {
        Get,
        Post,
    }

    fn credential_allowed(
        app: &str,
        credential: &Credential,
        method: RequestMethod,
        url: &str,
    ) -> bool {
        let usage = match method {
            RequestMethod::Get => CredentialUse::Fetch,
            RequestMethod::Post => CredentialUse::Post,
        };
        allowed(app, credential, url, usage)
    }

    #[test]
    fn bomtoon_session_cookie_is_limited_to_exact_detail_get() {
        use kobo_protocol::Credential;

        let session = Credential::in_header("bomtoon-session", "Cookie");
        let exact = "https://www.bomtoon.tw/detail/hunter_q";
        assert!(credential_allowed(
            "bomtoon",
            &session,
            RequestMethod::Get,
            exact
        ));
        for (method, url) in [
            (RequestMethod::Post, exact),
            (RequestMethod::Get, "https://www.bomtoon.tw/detail/"),
            (RequestMethod::Get, "https://www.bomtoon.tw/detail/hunter/q"),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/detail/hunter_q?preview=true",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/detail/hunter_q#episodes",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw:443/detail/hunter_q",
            ),
            (RequestMethod::Get, "http://www.bomtoon.tw/detail/hunter_q"),
            (
                RequestMethod::Get,
                "https://attacker.example/detail/hunter_q",
            ),
        ] {
            assert!(!credential_allowed("bomtoon", &session, method, url));
        }
        for credential in [
            Credential::bearer("bomtoon-session"),
            Credential::in_header("bomtoon-session", "Authorization"),
            Credential::in_header("another-secret", "Cookie"),
        ] {
            assert!(!credential_allowed(
                "bomtoon",
                &credential,
                RequestMethod::Get,
                exact
            ));
        }
    }

    #[test]
    fn authenticated_next_data_and_detail_html_are_denied() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        for url in [
            "https://www.bomtoon.tw/comic/main",
            "https://www.bomtoon.tw/api/auth/session",
            "https://www.bomtoon.tw/detail/hunter_q",
            "https://www.bomtoon.tw/_next/data/BUILD_ID/detail/hunter_q.json",
        ] {
            assert!(!credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                url
            ));
        }
    }

    #[test]
    fn content_json_requires_get_bearer_exact_alias_and_query() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        let content = "https://www.bomtoon.tw/api/balcony-api-v2/contents/hunter_q?isNotLoginAdult=false&isPorch=false";
        assert!(credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Get,
            content
        ));
        for alias in ["365", "hunter_q", "title-Z"] {
            assert!(credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                &format!(
                    "https://www.bomtoon.tw/api/balcony-api-v2/contents/{alias}?isNotLoginAdult=false&isPorch=false"
                )
            ));
        }
        for url in [
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/?isNotLoginAdult=false&isPorch=false",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/hunter/q?isNotLoginAdult=false&isPorch=false",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/hunter_q?isPorch=false&isNotLoginAdult=false",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/hunter_q?isNotLoginAdult=false",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/hunter_q?isNotLoginAdult=false&isPorch=false&extra=true",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/hunter_q?isNotLoginAdult=false&isPorch=false&isPorch=false",
            "https://www.bomtoon.tw:444/api/balcony-api-v2/contents/hunter_q?isNotLoginAdult=false&isPorch=false",
            "https://attacker.invalid/api/balcony-api-v2/contents/hunter_q?isNotLoginAdult=false&isPorch=false",
        ] {
            assert!(!credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                url
            ));
        }
        assert!(!credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Post,
            content
        ));
        assert!(!credential_allowed(
            "bomtoon",
            &Credential::in_header("bomtoon-access-token", "Authorization"),
            RequestMethod::Get,
            content
        ));
        for url in [
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
        ] {
            assert!(credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                url
            ));
        }
    }

    #[test]
    fn bomtoon_image_manifest_requires_exact_get_bearer_aliases_and_bounded_width() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        let exact = "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080";
        assert!(credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Get,
            exact
        ));
        for width in [1, 1072, 1264, 1404, 4096] {
            assert!(credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                &format!(
                    "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth={width}"
                )
            ));
        }

        for url in [
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images//ep-1?imageWidth=1080",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/?imageWidth=1080",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter/q/ep-1?imageWidth=1080",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep/1?imageWidth=1080",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/%2f?imageWidth=1080",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=0",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=4097",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=18446744073709551616",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=+1080",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=-1080",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth= 1080",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080%20",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=%31%30%38%30",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=10.80",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imagewidth=1080",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080&extra=true",
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080&imageWidth=1080",
            "http://www.bomtoon.tw/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080",
            "https://www.bomtoon.tw:444/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080",
            "https://attacker.invalid/api/balcony-api-v2/contents/images/hunter_q/ep-1?imageWidth=1080",
        ] {
            assert!(!credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                url
            ));
        }
        assert!(!credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Post,
            exact
        ));
    }

    #[test]
    fn library_json_rejects_every_query_and_origin_mutation() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        let exact = "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE";
        assert!(credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Get,
            exact
        ));
        for url in [
            "https://www.bomtoon.tw/api/balcony-api-v2/library",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=one&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?page=1&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?page=1&sort=CREATE&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&size=31&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&size=30&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&size=30&isIncludeAdult=false&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&size=30&isIncludeAdult=true",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&size=30&isIncludeAdult=true&contentsThumbnailType=RECTANGLE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&page=1&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE&extra=true",
            "https://www.bomtoon.tw:444/api/balcony-api-v2/library?sort=CREATE&page=1&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://attacker.invalid/api/balcony-api-v2/library?sort=CREATE&page=1&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "http://www.bomtoon.tw/api/balcony-api-v2/library?sort=CREATE&page=1&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
        ] {
            assert!(!credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                url
            ));
        }
        assert!(!credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Post,
            exact
        ));
    }

    #[test]
    fn recent_json_rejects_every_query_and_origin_mutation() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        let exact = "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE";
        assert!(credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Get,
            exact
        ));
        for url in [
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=one&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?page=0&sort=CREATE&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&size=30&contentsOrderNo=0&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=1&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&size=31&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&size=30&isIncludeAdult=false&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=RECTANGLE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&page=0&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE&extra=true",
            "https://www.bomtoon.tw:444/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "https://attacker.invalid/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
            "http://www.bomtoon.tw/api/balcony-api-v2/library/recent?sort=CREATE&page=0&contentsOrderNo=0&size=30&isIncludeAdult=true&contentsThumbnailType=SQUARE",
        ] {
            assert!(!credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                url
            ));
        }
        assert!(!credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Post,
            exact
        ));
    }

    #[test]
    fn bomtoon_credential_allows_approved_commerce_routes() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        let approved = [
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/asset/user",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind=COIN",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind=TICKET",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentsId=41",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType=RENT",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType=POSSESSION",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/api/balcony-api/purchase",
            ),
        ];
        for (method, url) in approved {
            assert!(
                credential_allowed("bomtoon", &access, method, url),
                "{method:?} {url}"
            );
        }
    }

    #[test]
    fn bomtoon_credential_denies_wrong_method_origin_and_gift_variants() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        let denied = [
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/api/balcony-api-v2/asset/user",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind=COIN",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentsId=41",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType=RENT",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api/purchase",
            ),
            (
                RequestMethod::Get,
                "https://attacker.invalid/api/balcony-api-v2/gift/contents/detail?contentsId=41",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw:443/api/balcony-api-v2/gift/contents/detail?contentsId=41",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw:444/api/balcony-api-v2/gift/contents/detail?contentsId=41",
            ),
            (
                RequestMethod::Get,
                "http://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentsId=41",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/details?contentsId=41",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/prefix/api/balcony-api-v2/gift/contents/detail?contentsId=41",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail/suffix?contentsId=41",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentsId=",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentsId=forty-one",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentsId=41&contentsId=41",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentsId=41&extra=true",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentId=41",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentsId=41#fragment",
            ),
        ];
        for (method, url) in denied {
            assert!(
                !credential_allowed("bomtoon", &access, method, url),
                "{method:?} {url}"
            );
        }
    }

    #[test]
    fn bomtoon_credential_denies_quote_and_purchase_variants() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        let denied = [
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price//ep-1?purchaseType=RENT",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/?purchaseType=RENT",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter%2fq/ep-1?purchaseType=RENT",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep.1?purchaseType=RENT",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType=",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType=RENT_GIFT",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType=RENT&purchaseType=RENT",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType=RENT&extra=true",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType=RENT#fragment",
            ),
            (
                RequestMethod::Post,
                "https://attacker.invalid/api/balcony-api/purchase",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw:443/api/balcony-api/purchase",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw:444/api/balcony-api/purchase",
            ),
            (
                RequestMethod::Post,
                "http://www.bomtoon.tw/api/balcony-api/purchase",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/api/balcony-api/purchases",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/prefix/api/balcony-api/purchase",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/api/balcony-api/purchase/suffix",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/api/balcony-api/purchase?extra=true",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/api/balcony-api/purchase#fragment",
            ),
        ];
        for (method, url) in denied {
            assert!(
                !credential_allowed("bomtoon", &access, method, url),
                "{method:?} {url}"
            );
        }
    }

    #[test]
    fn bomtoon_credential_enforces_quote_alias_length_boundary() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        let alias_at_limit = "a".repeat(128);
        let quote_at_limit = format!(
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/{alias_at_limit}/{alias_at_limit}?purchaseType=RENT"
        );
        assert!(credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Get,
            &quote_at_limit
        ));
        let alias_over_limit = "a".repeat(129);
        let quote_over_limit = format!(
            "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/{alias_over_limit}/ep-1?purchaseType=RENT"
        );
        assert!(!credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Get,
            &quote_over_limit
        ));
    }

    #[test]
    fn bomtoon_commerce_routes_reject_wrong_credential_kinds() {
        use kobo_protocol::Credential;

        let approved = [
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/asset/user",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind=COIN",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind=TICKET",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/gift/contents/detail?contentsId=41",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType=RENT",
            ),
            (
                RequestMethod::Get,
                "https://www.bomtoon.tw/api/balcony-api-v2/contents/price/hunter_q/ep-1?purchaseType=POSSESSION",
            ),
            (
                RequestMethod::Post,
                "https://www.bomtoon.tw/api/balcony-api/purchase",
            ),
        ];
        for credential in [
            Credential::in_header("bomtoon-access-token", "Authorization"),
            Credential::basic("bomtoon-access-token"),
            Credential::bearer("another-secret"),
        ] {
            for (method, url) in approved {
                assert!(!credential_allowed("bomtoon", &credential, method, url));
            }
        }
    }

    #[test]
    fn bomtoon_asset_summary_requires_exact_get_bearer_origin_path_and_no_query() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        let exact = "https://www.bomtoon.tw/api/balcony-api/asset/user";
        assert!(credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Get,
            exact
        ));
        for url in [
            "https://attacker.invalid/api/balcony-api/asset/user",
            "https://www.bomtoon.tw/api/balcony-api/assets/user",
            "https://www.bomtoon.tw/api/balcony-api/asset/user?extra=true",
        ] {
            assert!(!credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                url
            ));
        }
        assert!(!credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Post,
            exact
        ));
    }

    #[test]
    fn bomtoon_expiration_history_requires_exact_get_bearer_origin_path_and_query() {
        use kobo_protocol::Credential;

        let access = Credential::bearer("bomtoon-access-token");
        for kind in ["COIN", "TICKET"] {
            assert!(credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                &format!(
                    "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind={kind}"
                )
            ));
        }
        let exact = "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind=COIN";
        for url in [
            "https://attacker.invalid/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind=COIN",
            "https://www.bomtoon.tw/api/balcony-api-v2/payment/charges?createdAt=1725000000000&sort=EXPIRE&coinKind=COIN",
            "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?sort=EXPIRE&createdAt=1725000000000&coinKind=COIN",
            "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=&sort=EXPIRE&coinKind=COIN",
            "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=now&sort=EXPIRE&coinKind=COIN",
            "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=CREATE&coinKind=COIN",
            "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind=BONUS",
            "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind=COIN&extra=true",
        ] {
            assert!(!credential_allowed(
                "bomtoon",
                &access,
                RequestMethod::Get,
                url
            ));
        }
        assert!(!credential_allowed(
            "bomtoon",
            &access,
            RequestMethod::Post,
            exact
        ));
    }

    #[test]
    fn chat_credentials_are_bound_to_their_exact_service() {
        let openai = Credential::bearer("openai");
        assert!(allowed(
            "chat",
            &openai,
            "https://api.openai.com/v1/chat/completions",
            CredentialUse::Fetch
        ));
        for (app, url) in [
            ("other", "https://api.openai.com/v1/chat/completions"),
            (
                "chat",
                "https://api.openai.com.attacker.invalid/v1/chat/completions",
            ),
            ("chat", "https://attacker.invalid/collect"),
        ] {
            assert!(!allowed(app, &openai, url, CredentialUse::Fetch));
        }
    }

    #[test]
    fn audiobook_credentials_are_bound_to_exact_provider_requests() {
        let requests = [
            (
                Credential::in_header("exa", "x-api-key"),
                "https://api.exa.ai/agent/runs".to_owned(),
            ),
            (
                Credential::bearer("openai"),
                "https://api.openai.com/v1/responses".to_owned(),
            ),
        ];
        let voices = AUDIOBOOK_VOICES.map(|voice| {
            (
                Credential::in_header("elevenlabs", "xi-api-key"),
                format!(
                    "https://api.elevenlabs.io/v1/text-to-speech/{voice}?output_format=mp3_44100_128"
                ),
            )
        });
        for (credential, url) in requests.into_iter().chain(voices) {
            assert!(allowed(
                "audiobook",
                &credential,
                &url,
                CredentialUse::Fetch
            ));
            assert!(!allowed("chat", &credential, &url, CredentialUse::Fetch));
            assert!(!allowed(
                "audiobook",
                &credential,
                "https://attacker.invalid/collect",
                CredentialUse::Fetch
            ));
        }
        let elevenlabs = Credential::in_header("elevenlabs", "xi-api-key");
        for url in [
            "https://api.elevenlabs.io/v1/text-to-speech/AAAAAAAAAAAAAAAAAAAA?output_format=mp3_44100_128",
            "https://api.elevenlabs.io/v1/text-to-speech/JBFqnCBsd6RMkjVDRZzb?output_format=mp3_22050_32",
            "https://api.elevenlabs.io.attacker.invalid/v1/text-to-speech/JBFqnCBsd6RMkjVDRZzb?output_format=mp3_44100_128",
        ] {
            assert!(!allowed(
                "audiobook",
                &elevenlabs,
                url,
                CredentialUse::Fetch
            ));
        }
    }

    #[test]
    fn zotero_key_is_bound_to_exact_read_routes() {
        let key = Credential::bearer("zotero");
        for url in [
            "https://api.zotero.org/users/12345/collections?format=json&limit=100&sort=title&direction=asc",
            "https://api.zotero.org/users/12345/collections/ABCD2345/items/top?format=json&itemType=-attachment&limit=25&start=475&sort=dateAdded&direction=desc",
            "https://api.zotero.org/users/12345/collections/ABCD2345/items/top?format=json&itemType=-attachment&limit=1&start=500&sort=dateAdded&direction=desc",
            "https://api.zotero.org/users/12345/items/EFGH6789?format=json",
            "https://api.zotero.org/users/12345/items/EFGH6789/children?format=json&itemType=attachment&limit=100",
            "https://api.zotero.org/users/12345/items/JKLM2345/fulltext",
        ] {
            assert!(allowed(
                "zotero-reader",
                &key,
                url,
                CredentialUse::Fetch
            ));
            assert!(!allowed(
                "zotero-reader",
                &key,
                url,
                CredentialUse::Post
            ));
        }
    }

    #[test]
    fn zotero_key_refuses_other_apps_credentials_and_destinations() {
        let key = Credential::bearer("zotero");
        let item = "https://api.zotero.org/users/12345/items/PAPER001?format=json";
        assert!(!allowed("other", &key, item, CredentialUse::Fetch));
        assert!(!allowed(
            "zotero-reader",
            &Credential::bearer("other"),
            item,
            CredentialUse::Fetch
        ));
        for url in [
            "http://api.zotero.org/users/12345/items/PAPER001?format=json",
            "https://api.zotero.org:8443/users/12345/items/PAPER001?format=json",
            "https://user@api.zotero.org/users/12345/items/PAPER001?format=json",
            "https://api.zotero.org.attacker.invalid/users/12345/items/PAPER001?format=json",
            "https://api.zotero.org/groups/12345/items/PAPER001?format=json",
            "https://api.zotero.org/users/name/items/PAPER001?format=json",
            "https://api.zotero.org/users/12345/items",
            "https://api.zotero.org/users/12345/items/paper001?format=json",
            "https://api.zotero.org/users/12345/items/PAPER001/file",
            "https://api.zotero.org/users/12345/items/PAPER001?format=json&key=leak",
            "https://api.zotero.org/users/12345/items/%2e%2e/fulltext",
            "https://api.zotero.org/users/12345/collections/COLL1234/items/top?format=json&itemType=-attachment&limit=100&start=0&sort=dateAdded&direction=desc",
            "https://api.zotero.org/users/12345/items/ABCD0EFG?format=json",
            "https://api.zotero.org/users/12345/items/ABCD1EFG?format=json",
            "https://api.zotero.org/users/12345/items/ABCDOEFG?format=json",
            "https://api.zotero.org//users/12345/items/ABCD2345?format=json",
        ] {
            assert!(!allowed(
                "zotero-reader",
                &key,
                url,
                CredentialUse::Fetch
            ), "accepted {url}");
        }
    }
}
