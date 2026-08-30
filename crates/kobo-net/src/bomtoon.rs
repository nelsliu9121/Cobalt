use kobo_json::{ObjectBuilder, Value};
use kobo_policy::tasks::MAX_ACCOUNT_SUBJECT_BYTES;
use kobo_policy::{ManagedCredentialRecipe, ManagedTokenPair};
use kobo_protocol::TaskError;
use std::sync::Arc;

pub const ACCESS_CREDENTIAL: &str = "bomtoon-access-token";
pub const SESSION_SECRET: &str = "bomtoon-session";
pub const SESSION_COOKIE_MAX_BYTES: usize = kobo_policy::tasks::MAX_SECRET_BYTES;
pub const SESSION_URL: &str = "https://www.bomtoon.tw/api/auth/session";
pub const IP_URL: &str = "https://www.bomtoon.tw/api/balcony/ip";
pub const REFRESH_URL: &str = "https://www.bomtoon.tw/api/balcony/auth/refresh";
pub const LOGOUT_URL: &str = "https://www.bomtoon.tw/api/balcony-api/auth/logout";

const SESSION_RESPONSE_MAX_BYTES: u32 = 128 * 1024;
const IP_RESPONSE_MAX_BYTES: u32 = 4 * 1024;
const REFRESH_RESPONSE_MAX_BYTES: u32 = 128 * 1024;
const LOGOUT_RESPONSE_MAX_BYTES: u32 = 16 * 1024;
const JSON_CONTENT_TYPE: &str = "application/json";
const MANAGED_SCOPE_KEY_DOMAIN: &[u8] = b"cobalt-managed-scope-key-v1\0";
const ACCOUNT_SCOPE_DOMAIN: &[u8] = b"bomtoon-account-scope-v1\0";
const BOMTOON_HEADERS: [(&str, &str); 4] = [
    ("Accept", "application/json"),
    ("x-balcony-id", "BOMTOON_TW"),
    ("x-balcony-timezone", "Asia/Taipei"),
    ("x-platform", "MOBILE_IOS"),
];

/// The bounded HTTP operations needed by the BOMTOON credential broker.
pub trait Transport: Send + Sync {
    /// Performs one bounded GET request.
    ///
    /// # Errors
    ///
    /// Returns a task error when the transport cannot complete the request.
    fn get(
        &self,
        url: &str,
        credential: Option<(&str, &str)>,
        headers: &[(&str, &str)],
        max_bytes: u32,
    ) -> Result<Vec<u8>, TaskError>;

    /// Performs one bounded JSON POST request.
    ///
    /// # Errors
    ///
    /// Returns a task error when the transport cannot complete the request.
    fn post_json(
        &self,
        url: &str,
        body: &[u8],
        bearer: &str,
        headers: &[(&str, &str)],
        max_bytes: u32,
    ) -> Result<Vec<u8>, TaskError>;

    /// Performs one bounded JSON PUT request.
    ///
    /// # Errors
    ///
    /// Returns a task error when the transport cannot complete the request.
    fn put_json(
        &self,
        url: &str,
        body: &[u8],
        bearer: &str,
        headers: &[(&str, &str)],
        max_bytes: u32,
    ) -> Result<Vec<u8>, TaskError>;
}

struct LiveTransport;

impl Transport for LiveTransport {
    fn get(
        &self,
        url: &str,
        credential: Option<(&str, &str)>,
        headers: &[(&str, &str)],
        max_bytes: u32,
    ) -> Result<Vec<u8>, TaskError> {
        crate::fetch_from(url, 0, max_bytes, credential, headers)
    }

    fn post_json(
        &self,
        url: &str,
        body: &[u8],
        bearer: &str,
        headers: &[(&str, &str)],
        max_bytes: u32,
    ) -> Result<Vec<u8>, TaskError> {
        send_json_with_bearer(url, body, bearer, headers, max_bytes, crate::post)
    }

    fn put_json(
        &self,
        url: &str,
        body: &[u8],
        bearer: &str,
        headers: &[(&str, &str)],
        max_bytes: u32,
    ) -> Result<Vec<u8>, TaskError> {
        send_json_with_bearer(url, body, bearer, headers, max_bytes, crate::put)
    }
}

fn send_json_with_bearer(
    url: &str,
    body: &[u8],
    bearer: &str,
    headers: &[(&str, &str)],
    max_bytes: u32,
    send: impl FnOnce(
        &str,
        &[u8],
        &str,
        Option<(&str, &str)>,
        &[(&str, &str)],
        u32,
    ) -> Result<Vec<u8>, TaskError>,
) -> Result<Vec<u8>, TaskError> {
    let authorization = format!("Bearer {bearer}");
    send(
        url,
        body,
        JSON_CONTENT_TYPE,
        Some(("Authorization", authorization.as_str())),
        headers,
        max_bytes,
    )
}

/// BOMTOON's cookie-bound access-token lifecycle.
pub struct Recipe {
    transport: Arc<dyn Transport>,
}

impl Recipe {
    #[must_use]
    pub fn live() -> Self {
        Self {
            transport: Arc::new(LiveTransport),
        }
    }

    #[cfg(test)]
    fn with_transport(transport: Arc<dyn Transport>) -> Self {
        Self { transport }
    }
}

impl ManagedCredentialRecipe for Recipe {
    fn credential_name(&self) -> &'static str {
        ACCESS_CREDENTIAL
    }

    fn binding_secret_name(&self) -> &'static str {
        SESSION_SECRET
    }

    fn binding_digest(&self, secret: &str) -> String {
        crate::sha256::hex_digest(secret.as_bytes())
    }

    fn derive_scope_key(&self, binding_secret: &str) -> String {
        let mut material =
            Vec::with_capacity(MANAGED_SCOPE_KEY_DOMAIN.len() + binding_secret.len());
        material.extend_from_slice(MANAGED_SCOPE_KEY_DOMAIN);
        material.extend_from_slice(binding_secret.as_bytes());
        crate::sha256::hex_digest(&material)
    }

    fn derive_account_scope(&self, scope_key: &str, account_subject: &str) -> String {
        let mut message = Vec::with_capacity(ACCOUNT_SCOPE_DOMAIN.len() + account_subject.len());
        message.extend_from_slice(ACCOUNT_SCOPE_DOMAIN);
        message.extend_from_slice(account_subject.as_bytes());
        crate::sha256::hmac_hex(scope_key.as_bytes(), &message)[..32].to_owned()
    }

    fn bootstrap(&self, binding_secret: &str) -> Result<ManagedTokenPair, TaskError> {
        validate_cookie(binding_secret)?;
        let response = self.transport.get(
            SESSION_URL,
            Some(("Cookie", binding_secret)),
            &BOMTOON_HEADERS,
            SESSION_RESPONSE_MAX_BYTES,
        )?;
        parse_session_tokens(&response)
    }

    fn refresh(
        &self,
        binding_secret: &str,
        pair: &ManagedTokenPair,
    ) -> Result<ManagedTokenPair, TaskError> {
        validate_cookie(binding_secret)?;
        let ip_response = self.transport.get(
            IP_URL,
            Some(("Cookie", binding_secret)),
            &BOMTOON_HEADERS,
            IP_RESPONSE_MAX_BYTES,
        )?;
        let client_ip = parse_ip(&ip_response)?;
        let body = ObjectBuilder::new()
            .set("refreshToken", pair.refresh_token.as_str())
            .set("clientIp", client_ip)
            .build()
            .to_json();
        let headers = headers_with_cookie(binding_secret);
        let response = self.transport.post_json(
            REFRESH_URL,
            body.as_bytes(),
            &pair.access_token,
            &headers,
            REFRESH_RESPONSE_MAX_BYTES,
        )?;
        parse_refresh_tokens(&response, pair.account_subject.as_deref())
    }

    fn revoke(&self, binding_secret: &str, pair: &ManagedTokenPair) -> Result<(), TaskError> {
        validate_cookie(binding_secret)?;
        let body = ObjectBuilder::new()
            .set("refreshToken", pair.refresh_token.as_str())
            .build()
            .to_json();
        let headers = headers_with_cookie(binding_secret);
        let response = self.transport.put_json(
            LOGOUT_URL,
            body.as_bytes(),
            &pair.access_token,
            &headers,
            LOGOUT_RESPONSE_MAX_BYTES,
        )?;
        parse_logout(&response)
    }
}

/// Validates a candidate session and returns only a token-pair fingerprint.
///
/// # Errors
///
/// Returns a typed network or response-shape error without including any
/// provider response or credential text.
pub fn validate_session_cookie(cookie_header: &str) -> Result<String, TaskError> {
    validate_session_cookie_with(cookie_header, &LiveTransport)
}

fn validate_session_cookie_with(
    cookie_header: &str,
    transport: &dyn Transport,
) -> Result<String, TaskError> {
    validate_cookie(cookie_header)?;
    let response = transport.get(
        SESSION_URL,
        Some(("Cookie", cookie_header)),
        &BOMTOON_HEADERS,
        SESSION_RESPONSE_MAX_BYTES,
    )?;
    let pair = parse_session_tokens(&response)?;
    Ok(pair_fingerprint(&pair))
}

fn headers_with_cookie(cookie: &str) -> [(&str, &str); 5] {
    [
        ("Cookie", cookie),
        BOMTOON_HEADERS[0],
        BOMTOON_HEADERS[1],
        BOMTOON_HEADERS[2],
        BOMTOON_HEADERS[3],
    ]
}

fn validate_cookie(cookie: &str) -> Result<(), TaskError> {
    if cookie.len() > SESSION_COOKIE_MAX_BYTES {
        return Err(TaskError::TooLarge);
    }
    if cookie.is_empty() || cookie.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(TaskError::Denied);
    }
    Ok(())
}

fn parse_session_tokens(response: &[u8]) -> Result<ManagedTokenPair, TaskError> {
    let root = parse_bounded(response, SESSION_RESPONSE_MAX_BYTES)?;
    let container = unique_field(&root, "user")?;
    let account_subject = unique_field(container, "id")?
        .as_str()
        .ok_or(TaskError::Unreachable)?;
    let account_subject = validate_account_subject(account_subject)?;
    parse_token_pair(container, Some(account_subject))
}

fn parse_refresh_tokens(
    response: &[u8],
    account_subject: Option<&str>,
) -> Result<ManagedTokenPair, TaskError> {
    let root = parse_bounded(response, REFRESH_RESPONSE_MAX_BYTES)?;
    let container = unique_field(&root, "result")?;
    let account_subject = account_subject.map(validate_account_subject).transpose()?;
    parse_token_pair(container, account_subject)
}

fn parse_token_pair(
    container: &Value,
    account_subject: Option<String>,
) -> Result<ManagedTokenPair, TaskError> {
    let (access_token, access_expires_at_ms) = parse_token(container, "accessToken")?;
    let (refresh_token, refresh_expires_at_ms) = parse_token(container, "refreshToken")?;
    Ok(ManagedTokenPair {
        access_token,
        access_expires_at_ms,
        refresh_token,
        refresh_expires_at_ms,
        account_subject,
    })
}

fn validate_account_subject(account_subject: &str) -> Result<String, TaskError> {
    if account_subject.len() > MAX_ACCOUNT_SUBJECT_BYTES {
        return Err(TaskError::TooLarge);
    }
    if account_subject.is_empty() || account_subject.chars().any(char::is_control) {
        return Err(TaskError::Unreachable);
    }
    Ok(account_subject.to_owned())
}

fn parse_token(container: &Value, name: &str) -> Result<(String, u64), TaskError> {
    let token = unique_field(container, name)?;
    let text = unique_field(token, "token")?
        .as_str()
        .ok_or(TaskError::Unreachable)?;
    if text.len() > kobo_policy::tasks::MAX_SECRET_BYTES {
        return Err(TaskError::TooLarge);
    }
    if text.is_empty() || text.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(TaskError::Unreachable);
    }
    let created_at = epoch_milliseconds(unique_field(token, "createdAt")?)?;
    let expired_at = epoch_milliseconds(unique_field(token, "expiredAt")?)?;
    if expired_at <= created_at {
        return Err(TaskError::Unreachable);
    }
    Ok((text.to_owned(), expired_at))
}

fn epoch_milliseconds(value: &Value) -> Result<u64, TaskError> {
    value
        .as_integer_str()
        .and_then(|lexeme| lexeme.parse().ok())
        .ok_or(TaskError::Unreachable)
}

fn parse_ip(response: &[u8]) -> Result<String, TaskError> {
    let root = parse_bounded(response, IP_RESPONSE_MAX_BYTES)?;
    let ip = unique_field(&root, "ipAddress")?
        .as_str()
        .ok_or(TaskError::Unreachable)?;
    if ip.is_empty() || ip.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(TaskError::Unreachable);
    }
    Ok(ip.to_owned())
}

fn parse_logout(response: &[u8]) -> Result<(), TaskError> {
    let root = parse_bounded(response, LOGOUT_RESPONSE_MAX_BYTES)?;
    unique_field(&root, "result")?
        .as_str()
        .ok_or(TaskError::Unreachable)?;
    if !matches!(unique_field(&root, "data")?, Value::Object(_)) {
        return Err(TaskError::Unreachable);
    }
    Ok(())
}

fn parse_bounded(response: &[u8], max_bytes: u32) -> Result<Value, TaskError> {
    if response.len() > max_bytes as usize {
        return Err(TaskError::TooLarge);
    }
    let text = std::str::from_utf8(response).map_err(|_| TaskError::Unreachable)?;
    kobo_json::parse(text).map_err(|_| TaskError::Unreachable)
}

fn unique_field<'a>(object: &'a Value, name: &str) -> Result<&'a Value, TaskError> {
    let Value::Object(fields) = object else {
        return Err(TaskError::Unreachable);
    };
    let mut matches = fields
        .iter()
        .filter(|(field_name, _)| field_name == name)
        .map(|(_, value)| value);
    let value = matches.next().ok_or(TaskError::Unreachable)?;
    if matches.next().is_some() {
        return Err(TaskError::Unreachable);
    }
    Ok(value)
}

fn pair_fingerprint(pair: &ManagedTokenPair) -> String {
    let mut material =
        Vec::with_capacity(pair.access_token.len() + 1_usize + pair.refresh_token.len());
    material.extend_from_slice(pair.access_token.as_bytes());
    material.push(0);
    material.extend_from_slice(pair.refresh_token.as_bytes());
    crate::sha256::hex_digest(&material)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    const SESSION: &str = r#"{
  "user": {
    "id": "account-a",
    "accessToken": {"token":"ACCESS_A","createdAt":1000,"expiredAt":86401000},
    "refreshToken": {"token":"REFRESH_A","createdAt":1000,"expiredAt":604801000}
  },
  "expires":"STRING_REDACTED"
}"#;

    const REFRESH: &str = r#"{
  "result": {
    "accessToken": {"token":"ACCESS_B","createdAt":2000,"expiredAt":86402000},
    "refreshToken": {"token":"REFRESH_B","createdAt":2000,"expiredAt":604802000},
    "email":"user@example.invalid",
    "ipAddress":"203.0.113.1"
  }
}"#;

    enum Call {
        Get {
            url: String,
            credential: Option<(String, String)>,
            headers: Vec<(String, String)>,
            max_bytes: u32,
        },
        Post {
            url: String,
            body: Vec<u8>,
            bearer: String,
            headers: Vec<(String, String)>,
            max_bytes: u32,
        },
        Put {
            url: String,
            body: Vec<u8>,
            bearer: String,
            headers: Vec<(String, String)>,
            max_bytes: u32,
        },
    }

    struct FakeTransport {
        responses: Mutex<VecDeque<Result<Vec<u8>, TaskError>>>,
        calls: Mutex<Vec<Call>>,
    }

    impl FakeTransport {
        fn new(responses: impl IntoIterator<Item = Result<Vec<u8>, TaskError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn answer(&self) -> Result<Vec<u8>, TaskError> {
            self.responses
                .lock()
                .map_err(|_| TaskError::Unreachable)?
                .pop_front()
                .unwrap_or(Err(TaskError::Unreachable))
        }
    }

    impl Transport for FakeTransport {
        fn get(
            &self,
            url: &str,
            credential: Option<(&str, &str)>,
            headers: &[(&str, &str)],
            max_bytes: u32,
        ) -> Result<Vec<u8>, TaskError> {
            self.calls
                .lock()
                .map_err(|_| TaskError::Unreachable)?
                .push(Call::Get {
                    url: url.to_owned(),
                    credential: credential.map(|(name, value)| (name.to_owned(), value.to_owned())),
                    headers: owned_headers(headers),
                    max_bytes,
                });
            self.answer()
        }

        fn post_json(
            &self,
            url: &str,
            body: &[u8],
            bearer: &str,
            headers: &[(&str, &str)],
            max_bytes: u32,
        ) -> Result<Vec<u8>, TaskError> {
            self.calls
                .lock()
                .map_err(|_| TaskError::Unreachable)?
                .push(Call::Post {
                    url: url.to_owned(),
                    body: body.to_vec(),
                    bearer: bearer.to_owned(),
                    headers: owned_headers(headers),
                    max_bytes,
                });
            self.answer()
        }

        fn put_json(
            &self,
            url: &str,
            body: &[u8],
            bearer: &str,
            headers: &[(&str, &str)],
            max_bytes: u32,
        ) -> Result<Vec<u8>, TaskError> {
            self.calls
                .lock()
                .map_err(|_| TaskError::Unreachable)?
                .push(Call::Put {
                    url: url.to_owned(),
                    body: body.to_vec(),
                    bearer: bearer.to_owned(),
                    headers: owned_headers(headers),
                    max_bytes,
                });
            self.answer()
        }
    }

    fn owned_headers(headers: &[(&str, &str)]) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn pair_a() -> ManagedTokenPair {
        ManagedTokenPair {
            access_token: "ACCESS_A".to_owned(),
            access_expires_at_ms: 86_401_000,
            refresh_token: "REFRESH_A".to_owned(),
            refresh_expires_at_ms: 604_801_000,
            account_subject: Some("account-a".to_owned()),
        }
    }

    fn common_headers() -> Vec<(String, String)> {
        owned_headers(&BOMTOON_HEADERS)
    }

    fn cookie_headers(cookie: &str) -> Vec<(String, String)> {
        owned_headers(&headers_with_cookie(cookie))
    }

    #[test]
    fn account_scope_is_stable_and_subject_specific() {
        let recipe = Recipe::with_transport(Arc::new(FakeTransport::new(std::iter::empty::<
            Result<Vec<u8>, TaskError>,
        >())));
        let key = recipe.derive_scope_key("high-entropy-cookie-a");
        assert_eq!(
            key,
            crate::sha256::hex_digest(b"cobalt-managed-scope-key-v1\0high-entropy-cookie-a")
        );
        let first = recipe.derive_account_scope(&key, "account-a");
        assert_eq!(
            first,
            crate::sha256::hmac_hex(key.as_bytes(), b"bomtoon-account-scope-v1\0account-a")[..32]
        );
        assert_eq!(first, recipe.derive_account_scope(&key, "account-a"));
        assert_ne!(first, recipe.derive_account_scope(&key, "account-b"));
        assert_eq!(first.len(), 32);
        assert!(first
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    }

    #[test]
    fn session_bootstrap_retains_bounded_account_subject() {
        let pair = parse_session_tokens(SESSION.as_bytes()).expect("session pair");
        assert_eq!(pair.account_subject.as_deref(), Some("account-a"));
    }

    #[test]
    fn refresh_preserves_bootstrap_account_subject() {
        let pair =
            parse_refresh_tokens(REFRESH.as_bytes(), Some("account-a")).expect("refresh pair");
        assert_eq!(pair.account_subject.as_deref(), Some("account-a"));
    }

    #[test]
    fn session_bootstrap_rejects_missing_account_subject() {
        let response = SESSION.replace("    \"id\": \"account-a\",\n", "");
        assert_eq!(
            parse_session_tokens(response.as_bytes()),
            Err(TaskError::Unreachable)
        );
    }

    #[test]
    fn session_bootstrap_rejects_wrong_account_subject_type() {
        let response = SESSION.replace("\"account-a\"", "42");
        assert_eq!(
            parse_session_tokens(response.as_bytes()),
            Err(TaskError::Unreachable)
        );
    }

    #[test]
    fn session_bootstrap_rejects_empty_account_subject() {
        let response = SESSION.replace("account-a", "");
        assert_eq!(
            parse_session_tokens(response.as_bytes()),
            Err(TaskError::Unreachable)
        );
    }

    #[test]
    fn session_bootstrap_rejects_oversized_account_subject() {
        let response = SESSION.replace("account-a", &"界".repeat(43));
        assert_eq!(
            parse_session_tokens(response.as_bytes()),
            Err(TaskError::TooLarge)
        );
    }

    #[test]
    fn session_bootstrap_rejects_control_character_account_subject() {
        let response = SESSION.replace("account-a", "account\\u000aa");
        assert_eq!(
            parse_session_tokens(response.as_bytes()),
            Err(TaskError::Unreachable)
        );
    }

    #[test]
    fn session_tokens_are_read_only_beneath_user() {
        let response = format!(
            r#"{{"accessToken":{{"token":"WRONG","createdAt":1,"expiredAt":2}},"refreshToken":{{"token":"WRONG","createdAt":1,"expiredAt":2}},"user":{}}}"#,
            unique_field(&kobo_json::parse(SESSION).expect("fixture"), "user")
                .expect("user")
                .to_json()
        );
        let pair = parse_session_tokens(response.as_bytes()).expect("session pair");
        assert_eq!(pair, pair_a());
        assert!(parse_session_tokens(REFRESH.as_bytes()).is_err());
    }

    #[test]
    fn refresh_tokens_are_read_only_beneath_result() {
        let response = format!(
            r#"{{"user":{{"accessToken":{{"token":"WRONG","createdAt":1,"expiredAt":2}},"refreshToken":{{"token":"WRONG","createdAt":1,"expiredAt":2}}}},"result":{}}}"#,
            unique_field(&kobo_json::parse(REFRESH).expect("fixture"), "result")
                .expect("result")
                .to_json()
        );
        let pair =
            parse_refresh_tokens(response.as_bytes(), Some("account-a")).expect("refresh pair");
        assert_eq!(pair.access_token, "ACCESS_B");
        assert_eq!(pair.refresh_token, "REFRESH_B");
        assert!(parse_refresh_tokens(SESSION.as_bytes(), Some("account-a")).is_err());
    }

    #[test]
    fn partial_or_oversized_token_pairs_are_rejected() {
        let partial =
            br#"{"user":{"id":"account-a","accessToken":{"token":"ACCESS_A","createdAt":1,"expiredAt":2}}}"#;
        assert!(matches!(
            parse_session_tokens(partial),
            Err(TaskError::Unreachable)
        ));

        let oversized_token = "X".repeat(kobo_policy::tasks::MAX_SECRET_BYTES + 1);
        let oversized = format!(
            r#"{{"user":{{"id":"account-a","accessToken":{{"token":"{oversized_token}","createdAt":1,"expiredAt":2}},"refreshToken":{{"token":"R","createdAt":1,"expiredAt":2}}}}}}"#
        );
        assert!(matches!(
            parse_session_tokens(oversized.as_bytes()),
            Err(TaskError::TooLarge)
        ));
        assert!(matches!(
            parse_session_tokens(&vec![b' '; SESSION_RESPONSE_MAX_BYTES as usize + 1]),
            Err(TaskError::TooLarge)
        ));

        for malformed in [
            SESSION.replace("86401000", "1000"),
            SESSION.replace("604801000", "1.5"),
            SESSION.replace("ACCESS_A", "ACCESS\\u000aA"),
            SESSION.replace("\"refreshToken\"", "\"accessToken\""),
        ] {
            assert!(matches!(
                parse_session_tokens(malformed.as_bytes()),
                Err(TaskError::Unreachable)
            ));
        }
    }

    #[test]
    fn epoch_milliseconds_require_exact_non_overflowing_integer_lexemes() {
        for malformed in [
            SESSION.replace("604801000", "604801000.0000000000000001"),
            SESSION.replace("604801000", "604801000e0"),
            SESSION.replace("604801000", "18446744073709551616"),
            SESSION.replace("1000", "-1"),
        ] {
            assert!(matches!(
                parse_session_tokens(malformed.as_bytes()),
                Err(TaskError::Unreachable)
            ));
        }

        let beyond_double_precision = br#"{
          "user": {
            "id": "account-a",
            "accessToken": {
              "token":"ACCESS_A",
              "createdAt":9007199254740993,
              "expiredAt":9007199254740994
            },
            "refreshToken": {
              "token":"REFRESH_A",
              "createdAt":9007199254740993,
              "expiredAt":9007199254740995
            }
          }
        }"#;
        let pair = parse_session_tokens(beyond_double_precision).expect("exact integer lexemes");
        assert_eq!(pair.access_expires_at_ms, 9_007_199_254_740_994);
        assert_eq!(pair.refresh_expires_at_ms, 9_007_199_254_740_995);
    }

    #[test]
    fn refresh_sends_cookie_ip_and_refresh_token_with_access_bearer() {
        let transport = Arc::new(FakeTransport::new([
            Ok(br#"{"ipAddress":"203.0.113.1"}"#.to_vec()),
            Ok(REFRESH.as_bytes().to_vec()),
        ]));
        let recipe = Recipe::with_transport(transport.clone());
        let rotated = recipe
            .refresh("SESSION_COOKIE_A", &pair_a())
            .expect("refresh");
        assert_eq!(rotated.access_token, "ACCESS_B");
        assert_eq!(rotated.refresh_token, "REFRESH_B");

        let calls = transport.calls.lock().expect("calls");
        assert_eq!(calls.len(), 2);
        match &calls[0] {
            Call::Get {
                url,
                credential,
                headers,
                max_bytes,
            } => {
                assert_eq!(url, IP_URL);
                assert_eq!(
                    credential
                        .as_ref()
                        .map(|value| (value.0.as_str(), value.1.as_str())),
                    Some(("Cookie", "SESSION_COOKIE_A"))
                );
                assert_eq!(*max_bytes, IP_RESPONSE_MAX_BYTES);
                assert_eq!(headers, &common_headers());
            }
            _ => panic!("expected IP GET"),
        }
        match &calls[1] {
            Call::Post {
                url,
                body,
                bearer,
                headers,
                max_bytes,
            } => {
                assert_eq!(url, REFRESH_URL);
                assert_eq!(
                    body,
                    br#"{"refreshToken":"REFRESH_A","clientIp":"203.0.113.1"}"#
                );
                assert_eq!(bearer, "ACCESS_A");
                assert_eq!(*max_bytes, REFRESH_RESPONSE_MAX_BYTES);
                assert_eq!(headers, &cookie_headers("SESSION_COOKIE_A"));
            }
            _ => panic!("expected refresh POST"),
        }
    }

    #[test]
    fn logout_sends_cookie_and_bearer_with_put_without_redirects() {
        let transport = Arc::new(FakeTransport::new([Ok(
            br#"{"result":"ok","data":{}}"#.to_vec()
        )]));
        let recipe = Recipe::with_transport(transport.clone());
        recipe
            .revoke("SESSION_COOKIE_A", &pair_a())
            .expect("logout");
        let calls = transport.calls.lock().expect("calls");
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            Call::Put {
                url,
                body,
                bearer,
                headers,
                max_bytes,
            } => {
                assert_eq!(url, LOGOUT_URL);
                assert_eq!(body, br#"{"refreshToken":"REFRESH_A"}"#);
                assert_eq!(bearer, "ACCESS_A");
                assert_eq!(*max_bytes, LOGOUT_RESPONSE_MAX_BYTES);
                assert_eq!(headers, &cookie_headers("SESSION_COOKIE_A"));
            }
            _ => panic!("expected logout PUT"),
        }
        drop(calls);

        let redirecting = Arc::new(FakeTransport::new([Err(TaskError::Denied)]));
        let recipe = Recipe::with_transport(redirecting.clone());
        assert_eq!(
            recipe.revoke("SESSION_COOKIE_A", &pair_a()),
            Err(TaskError::Denied)
        );
        assert_eq!(redirecting.calls.lock().expect("calls").len(), 1);
    }

    #[test]
    fn broker_gets_pass_cookie_credentials_separately() {
        let transport = Arc::new(FakeTransport::new([
            Ok(SESSION.as_bytes().to_vec()),
            Ok(SESSION.as_bytes().to_vec()),
        ]));
        let recipe = Recipe::with_transport(transport.clone());
        let bootstrapped = recipe.bootstrap("SESSION_COOKIE_A").expect("bootstrap");
        assert_eq!(bootstrapped, pair_a());
        let fingerprint = validate_session_cookie_with("SESSION_COOKIE_A", transport.as_ref())
            .expect("validation");
        assert_eq!(fingerprint.len(), 64);
        assert_eq!(
            fingerprint,
            crate::sha256::hex_digest(b"ACCESS_A\0REFRESH_A")
        );
        assert!(!fingerprint.contains("ACCESS_A"));
        assert!(!fingerprint.contains("REFRESH_A"));

        let calls = transport.calls.lock().expect("calls");
        assert_eq!(calls.len(), 2);
        for call in calls.iter() {
            let Call::Get {
                url,
                credential,
                headers,
                max_bytes,
            } = call
            else {
                panic!("expected session GET");
            };
            assert_eq!(url, SESSION_URL);
            assert_eq!(
                credential
                    .as_ref()
                    .map(|value| (value.0.as_str(), value.1.as_str())),
                Some(("Cookie", "SESSION_COOKIE_A"))
            );
            assert_eq!(*max_bytes, SESSION_RESPONSE_MAX_BYTES);
            assert_eq!(headers, &common_headers());
        }
    }

    #[test]
    fn malformed_broker_responses_return_only_typed_errors() {
        let malformed_ip = Arc::new(FakeTransport::new([Ok(
            br#"{"ipAddress":"203.0.113.1","ipAddress":"198.51.100.1"}"#.to_vec(),
        )]));
        assert_eq!(
            Recipe::with_transport(malformed_ip).refresh("SESSION_COOKIE_A", &pair_a()),
            Err(TaskError::Unreachable)
        );

        for response in [
            br#"{"result":{},"data":{}}"#.as_slice(),
            br#"{"result":"ok","data":[]}"#.as_slice(),
            br#"{"result":"ok"}"#.as_slice(),
        ] {
            let transport = Arc::new(FakeTransport::new([Ok(response.to_vec())]));
            assert_eq!(
                Recipe::with_transport(transport).revoke("SESSION_COOKIE_A", &pair_a()),
                Err(TaskError::Unreachable)
            );
        }

        let oversized = Arc::new(FakeTransport::new([Ok(vec![
            b' ';
            SESSION_RESPONSE_MAX_BYTES
                as usize
                + 1
        ])]));
        assert_eq!(
            Recipe::with_transport(oversized).bootstrap("SESSION_COOKIE_A"),
            Err(TaskError::TooLarge)
        );

        let invalid = Arc::new(FakeTransport::new(std::iter::empty::<
            Result<Vec<u8>, TaskError>,
        >()));
        assert_eq!(
            validate_session_cookie_with("", invalid.as_ref()),
            Err(TaskError::Denied)
        );
        assert_eq!(
            validate_session_cookie_with(
                &"X".repeat(SESSION_COOKIE_MAX_BYTES + 1),
                invalid.as_ref()
            ),
            Err(TaskError::TooLarge)
        );
        assert!(invalid.calls.lock().expect("calls").is_empty());
    }

    #[test]
    fn live_body_adapter_maps_bearer_to_exact_authorization_credential() {
        let headers = headers_with_cookie("SESSION_COOKIE_A");
        let mut called = false;
        let response = send_json_with_bearer(
            REFRESH_URL,
            br#"{"refreshToken":"REFRESH_A","clientIp":"203.0.113.1"}"#,
            "ACCESS_A",
            &headers,
            REFRESH_RESPONSE_MAX_BYTES,
            |url, body, content_type, credential, sent_headers, max_bytes| {
                called = true;
                assert_eq!(url, REFRESH_URL);
                assert_eq!(
                    body,
                    br#"{"refreshToken":"REFRESH_A","clientIp":"203.0.113.1"}"#
                );
                assert_eq!(content_type, "application/json");
                assert_eq!(credential, Some(("Authorization", "Bearer ACCESS_A")));
                assert_eq!(sent_headers, &headers);
                assert_eq!(max_bytes, REFRESH_RESPONSE_MAX_BYTES);
                Ok(b"ok".to_vec())
            },
        );
        assert!(called);
        assert_eq!(response, Ok(b"ok".to_vec()));
    }

    #[test]
    fn request_strings_are_json_escaped() {
        let transport = Arc::new(FakeTransport::new([
            Ok(br#"{"ipAddress":"203.0.113.1\"quoted"}"#.to_vec()),
            Ok(REFRESH.as_bytes().to_vec()),
        ]));
        let mut pair = pair_a();
        pair.refresh_token = "REFRESH_\"A".to_owned();
        Recipe::with_transport(transport.clone())
            .refresh("SESSION_COOKIE_A", &pair)
            .expect("refresh");
        let calls = transport.calls.lock().expect("calls");
        let Call::Post { body, .. } = &calls[1] else {
            panic!("expected refresh POST");
        };
        assert_eq!(
            body,
            br#"{"refreshToken":"REFRESH_\"A","clientIp":"203.0.113.1\"quoted"}"#
        );
    }
}
