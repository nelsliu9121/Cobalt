use kobo_json::Value;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

pub const MAX_BODY_BYTES: usize = 16 * 1024;
pub const MAX_COOKIES: usize = 16;
pub const MAX_REJECTED_REQUESTS: usize = 32;
pub const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(2);
pub const HOST_LOCK_PORT: u16 = 53_941;

const MAX_HEADER_BYTES: usize = 8 * 1024;
const BAD_REQUEST_RESPONSE: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
const SUCCESS_RESPONSE: &[u8] =
    b"HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
const FAILURE_RESPONSE: &[u8] = b"HTTP/1.1 422 Unprocessable Content\r\nConnection: close\r\n\
Content-Type: text/plain\r\nContent-Length: 24\r\n\r\nBOMTOON login rejected.\n";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffPayload {
    pub version: u32,
    pub fingerprint: String,
    pub cookies: Vec<HandoffCookie>,
}

pub struct Challenge {
    port: u16,
    nonce: String,
}

pub struct PendingHandoff {
    payload: HandoffPayload,
    stream: TcpStream,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandoffError {
    Timeout,
    Listener,
}

pub struct HostLock {
    _listener: TcpListener,
}

impl Challenge {
    pub fn new() -> io::Result<(Self, TcpListener)> {
        let mut random = File::open("/dev/urandom")?;
        let nonce = nonce_from(&mut random)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        Ok((Self { port, nonce }, listener))
    }

    #[must_use]
    pub fn fragment(&self) -> String {
        format!("#cobalt-login=v1.{}.{}", self.port, self.nonce)
    }
}

impl PendingHandoff {
    #[must_use]
    pub fn payload(&self) -> &HandoffPayload {
        &self.payload
    }

    pub fn succeed(self) -> io::Result<()> {
        finish_response(self.stream, SUCCESS_RESPONSE)
    }

    pub fn fail(self) -> io::Result<()> {
        finish_response(self.stream, FAILURE_RESPONSE)
    }
}

impl HostLock {
    pub fn acquire() -> io::Result<Self> {
        Self::acquire_at(HOST_LOCK_PORT)
    }

    fn acquire_at(port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        Ok(Self {
            _listener: listener,
        })
    }
}

fn nonce_from(source: &mut impl Read) -> io::Result<String> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut entropy = [0_u8; 32];
    source.read_exact(&mut entropy)?;
    let mut encoded = String::with_capacity(43);
    let mut chunks = entropy.chunks_exact(3);
    for chunk in &mut chunks {
        encoded.push(char::from(ALPHABET[usize::from(chunk[0] >> 2)]));
        encoded.push(char::from(
            ALPHABET[usize::from(((chunk[0] & 0x03) << 4) | (chunk[1] >> 4))],
        ));
        encoded.push(char::from(
            ALPHABET[usize::from(((chunk[1] & 0x0f) << 2) | (chunk[2] >> 6))],
        ));
        encoded.push(char::from(ALPHABET[usize::from(chunk[2] & 0x3f)]));
    }
    let remainder = chunks.remainder();
    if let [first, second] = remainder {
        encoded.push(char::from(ALPHABET[usize::from(first >> 2)]));
        encoded.push(char::from(
            ALPHABET[usize::from(((first & 0x03) << 4) | (second >> 4))],
        ));
        encoded.push(char::from(ALPHABET[usize::from((second & 0x0f) << 2)]));
    }
    Ok(encoded)
}

pub fn wait_for_payload(
    listener: &TcpListener,
    challenge: &Challenge,
    deadline: Instant,
) -> Result<PendingHandoff, HandoffError> {
    listener
        .set_nonblocking(true)
        .map_err(|_| HandoffError::Listener)?;
    let mut rejected = 0_usize;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(HandoffError::Timeout);
        }
        match listener.accept() {
            Ok((mut stream, peer)) => {
                let payload = if stream.set_nonblocking(false).is_ok() && is_ipv4_loopback(peer) {
                    read_stream_payload(&mut stream, challenge, deadline)
                } else {
                    Err(())
                };
                match payload {
                    Ok(payload) if Instant::now() < deadline => {
                        return Ok(PendingHandoff { payload, stream });
                    }
                    Ok(_) | Err(()) => {
                        let _ = finish_response(stream, BAD_REQUEST_RESPONSE);
                        rejected += 1;
                        if rejected >= MAX_REJECTED_REQUESTS {
                            return Err(HandoffError::Listener);
                        }
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(remaining.min(Duration::from_millis(5)));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(HandoffError::Listener),
        }
    }
}

fn is_ipv4_loopback(peer: SocketAddr) -> bool {
    matches!(peer, SocketAddr::V4(address) if address.ip().is_loopback())
}

fn read_stream_payload(
    stream: &mut TcpStream,
    challenge: &Challenge,
    deadline: Instant,
) -> Result<HandoffPayload, ()> {
    let started = Instant::now();
    let remaining = deadline
        .checked_duration_since(started)
        .filter(|remaining| !remaining.is_zero())
        .ok_or(())?;
    let connection_deadline = started
        .checked_add(remaining.min(CONNECTION_READ_TIMEOUT))
        .ok_or(())?;

    let mut request = Vec::with_capacity(1_024);
    let header_end = loop {
        if let Some(header_end) = find_header_end(&request) {
            break header_end;
        }
        if request.len() >= MAX_HEADER_BYTES {
            return Err(());
        }
        let mut buffer = [0_u8; 1_024];
        let limit = buffer.len().min(MAX_HEADER_BYTES - request.len());
        let read = read_with_deadline(stream, &mut buffer[..limit], connection_deadline)?;
        if read == 0 {
            return Err(());
        }
        request.extend_from_slice(&buffer[..read]);
    };

    let head = parse_http_head(
        request.get(..header_end).ok_or(())?,
        challenge.port,
        &challenge.nonce,
    )?;
    let request_end = header_end.checked_add(head.content_length).ok_or(())?;
    if request.len() > request_end {
        return Err(());
    }
    while request.len() < request_end {
        let mut buffer = [0_u8; 4_096];
        let limit = buffer
            .len()
            .min(request_end.saturating_add(1).saturating_sub(request.len()));
        let read = read_with_deadline(stream, &mut buffer[..limit], connection_deadline)?;
        if read == 0 {
            return Err(());
        }
        request.extend_from_slice(&buffer[..read]);
        if request.len() > request_end {
            return Err(());
        }
    }
    if has_immediate_trailing_bytes(stream)? || Instant::now() >= connection_deadline {
        return Err(());
    }
    parse_payload(request.get(header_end..request_end).ok_or(())?)
}

fn read_with_deadline(
    stream: &mut TcpStream,
    buffer: &mut [u8],
    deadline: Instant,
) -> Result<usize, ()> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(())?;
        stream.set_read_timeout(Some(remaining)).map_err(|_| ())?;
        match stream.read(buffer) {
            Ok(read) => return Ok(read),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(()),
        }
    }
}

fn has_immediate_trailing_bytes(stream: &TcpStream) -> Result<bool, ()> {
    stream.set_nonblocking(true).map_err(|_| ())?;
    let mut byte = [0_u8; 1];
    let peek = stream.peek(&mut byte);
    let restore = stream.set_nonblocking(false);
    restore.map_err(|_| ())?;
    match peek {
        Ok(0) => Ok(false),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(_) => Err(()),
    }
}

#[cfg(test)]
fn parse_request(request: &[u8], port: u16, nonce: &str) -> Result<HandoffPayload, ()> {
    let header_end = find_header_end(request).ok_or(())?;
    if header_end > MAX_HEADER_BYTES {
        return Err(());
    }
    let head = parse_http_head(request.get(..header_end).ok_or(())?, port, nonce)?;
    let request_end = header_end.checked_add(head.content_length).ok_or(())?;
    if request_end != request.len() {
        return Err(());
    }
    parse_payload(request.get(header_end..request_end).ok_or(())?)
}

fn find_header_end(request: &[u8]) -> Option<usize> {
    request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|offset| offset + 4)
}

struct HttpHead {
    content_length: usize,
}

fn parse_http_head(head: &[u8], port: u16, nonce: &str) -> Result<HttpHead, ()> {
    if head.len() > MAX_HEADER_BYTES || !head.ends_with(b"\r\n\r\n") {
        return Err(());
    }
    let text = std::str::from_utf8(head).map_err(|_| ())?;
    let mut lines = text[..text.len() - 4].split("\r\n");
    let request_line = lines.next().ok_or(())?;
    let request_nonce = request_line
        .strip_prefix("POST /bomtoon-login/")
        .and_then(|line| line.strip_suffix(" HTTP/1.1"));
    if request_nonce != Some(nonce) {
        return Err(());
    }
    let mut host = None;
    let mut content_type = None;
    let mut content_length = None;
    for line in lines {
        let (name, raw_value) = line.split_once(':').ok_or(())?;
        if !valid_header_name(name) {
            return Err(());
        }
        let value = raw_value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("host") {
            if host.replace(value).is_some() {
                return Err(());
            }
        } else if name.eq_ignore_ascii_case("content-type") {
            if content_type.replace(value).is_some() {
                return Err(());
            }
        } else if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() || !canonical_decimal(value) {
                return Err(());
            }
            let length = value.parse::<usize>().map_err(|_| ())?;
            if length > MAX_BODY_BYTES {
                return Err(());
            }
            content_length = Some(length);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(());
        }
    }
    if !host.is_some_and(|value| exact_loopback_host(value, port))
        || content_type != Some("application/json")
    {
        return Err(());
    }
    Ok(HttpHead {
        content_length: content_length.ok_or(())?,
    })
}

fn exact_loopback_host(value: &str, port: u16) -> bool {
    let Some(port_text) = value.strip_prefix("127.0.0.1:") else {
        return false;
    };
    canonical_decimal(port_text) && port_text.parse::<u16>() == Ok(port)
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn canonical_decimal(value: &str) -> bool {
    value == "0"
        || value
            .strip_prefix(|byte: char| matches!(byte, '1'..='9'))
            .is_some_and(|rest| rest.bytes().all(|byte| byte.is_ascii_digit()))
}

fn parse_payload(body: &[u8]) -> Result<HandoffPayload, ()> {
    if body.len() > MAX_BODY_BYTES {
        return Err(());
    }
    let body = std::str::from_utf8(body).map_err(|_| ())?;
    let value = kobo_json::parse(body).map_err(|_| ())?;
    let object = StrictObject::new(&value, &["version", "fingerprint", "cookies"])?;
    if object.value("version")?.as_integer_str() != Some("1") {
        return Err(());
    }
    let fingerprint = object.value("fingerprint")?.as_str().ok_or(())?;
    if fingerprint.len() != 64
        || !fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(());
    }
    let cookies = object.value("cookies")?.as_array().ok_or(())?;
    if cookies.len() > MAX_COOKIES {
        return Err(());
    }
    let cookies = cookies
        .iter()
        .map(parse_cookie)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(HandoffPayload {
        version: 1,
        fingerprint: fingerprint.to_owned(),
        cookies,
    })
}

fn parse_cookie(value: &Value) -> Result<HandoffCookie, ()> {
    let object = StrictObject::new(value, &["name", "value", "domain", "path", "secure"])?;
    let name = bounded_string(object.value("name")?, 128)?;
    let cookie_value = bounded_string(object.value("value")?, 4_096)?;
    let domain = bounded_string(object.value("domain")?, 255)?;
    let path = bounded_string(object.value("path")?, 1_024)?;
    let secure = object.value("secure")?.as_bool().ok_or(())?;
    Ok(HandoffCookie {
        name: name.to_owned(),
        value: cookie_value.to_owned(),
        domain: domain.to_owned(),
        path: path.to_owned(),
        secure,
    })
}

fn bounded_string(value: &Value, maximum: usize) -> Result<&str, ()> {
    let value = value.as_str().ok_or(())?;
    if value.len() <= maximum {
        Ok(value)
    } else {
        Err(())
    }
}

struct StrictObject<'a> {
    fields: &'a [(String, Value)],
}

impl<'a> StrictObject<'a> {
    fn new(value: &'a Value, allowed: &[&str]) -> Result<Self, ()> {
        let Value::Object(fields) = value else {
            return Err(());
        };
        let mut seen = 0_u8;
        for (name, _) in fields {
            let index = allowed
                .iter()
                .position(|allowed_name| name == allowed_name)
                .ok_or(())?;
            let bit = 1_u8
                .checked_shl(u32::try_from(index).map_err(|_| ())?)
                .ok_or(())?;
            if seen & bit != 0 {
                return Err(());
            }
            seen |= bit;
        }
        let expected = 1_u8
            .checked_shl(u32::try_from(allowed.len()).map_err(|_| ())?)
            .ok_or(())?
            - 1;
        if seen != expected {
            return Err(());
        }
        Ok(Self { fields })
    }

    fn value(&self, name: &str) -> Result<&'a Value, ()> {
        self.fields
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value)
            .ok_or(())
    }
}

fn finish_response(mut stream: TcpStream, response: &[u8]) -> io::Result<()> {
    let write_result = stream.write_all(response).and_then(|()| stream.flush());
    let shutdown_result = stream.shutdown(Shutdown::Both);
    match write_result {
        Ok(()) => shutdown_result,
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    const PORT: u16 = 43_125;
    const NONCE: &str = "nonce";
    const FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn request_bytes(
        method: &str,
        host: &str,
        path: &str,
        content_type: &str,
        body: &str,
    ) -> Vec<u8> {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn request_with_body(body: &str) -> Vec<u8> {
        request_bytes(
            "POST",
            "127.0.0.1:43125",
            "/bomtoon-login/nonce",
            "application/json",
            body,
        )
    }

    fn replace_once(input: &[u8], from: &str, to: &str) -> Vec<u8> {
        String::from_utf8(input.to_vec())
            .expect("ASCII request")
            .replacen(from, to, 1)
            .into_bytes()
    }

    fn valid_body() -> String {
        format!(r#"{{"version":1,"fingerprint":"{FINGERPRINT}","cookies":[]}}"#)
    }

    fn cookie(name: &str, value: &str, domain: &str, path: &str) -> String {
        format!(
            r#"{{"name":"{name}","value":"{value}","domain":"{domain}","path":"{path}","secure":true}}"#
        )
    }

    fn body_with_cookie(cookie: &str) -> String {
        format!(r#"{{"version":1,"fingerprint":"{FINGERPRINT}","cookies":[{cookie}]}}"#)
    }

    fn request_with_header_size(body: &str, header_size: usize) -> Vec<u8> {
        let fixed = format!(
            "POST /bomtoon-login/nonce HTTP/1.1\r\nHost: 127.0.0.1:43125\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Pad: \r\n\r\n",
            body.len()
        );
        assert!(fixed.len() <= header_size);
        let pad = "x".repeat(header_size - fixed.len());
        let head = fixed.replacen("X-Pad: ", &format!("X-Pad: {pad}"), 1);
        assert_eq!(head.len(), header_size);
        [head.as_bytes(), body.as_bytes()].concat()
    }

    fn live_pair() -> (TcpListener, Challenge, SocketAddrV4) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback");
        let address = match listener.local_addr().expect("listener address") {
            std::net::SocketAddr::V4(address) => address,
            std::net::SocketAddr::V6(_) => panic!("IPv4 listener"),
        };
        let challenge = Challenge {
            port: address.port(),
            nonce: NONCE.to_owned(),
        };
        (listener, challenge, address)
    }

    fn live_request(port: u16, method: &str, body: &str) -> Vec<u8> {
        request_bytes(
            method,
            &format!("127.0.0.1:{port}"),
            "/bomtoon-login/nonce",
            "application/json",
            body,
        )
    }

    fn read_response(mut stream: TcpStream) -> Vec<u8> {
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        response
    }

    fn pending_from(
        handle: thread::JoinHandle<Result<PendingHandoff, HandoffError>>,
    ) -> PendingHandoff {
        match handle.join().expect("wait thread") {
            Ok(pending) => pending,
            Err(error) => panic!("unexpected handoff error: {error:?}"),
        }
    }

    fn unused_loopback_port() -> u16 {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("select unused lock port");
        let port = listener.local_addr().expect("lock address").port();
        drop(listener);
        port
    }

    #[test]
    fn nonce_is_unpadded_base64url_with_full_entropy_length() {
        let source: Vec<u8> = (0_u8..32).collect();
        assert_eq!(
            nonce_from(&mut Cursor::new(source)).expect("nonce").len(),
            43
        );
        assert!(nonce_from(&mut Cursor::new(vec![0xff; 32]))
            .expect("nonce")
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
    }

    #[test]
    fn challenge_binds_ipv4_loopback_and_formats_exact_fragment() {
        let (challenge, listener) = Challenge::new().expect("challenge");
        let address = listener.local_addr().expect("listener address");
        assert!(
            matches!(address, std::net::SocketAddr::V4(address) if address.ip() == &Ipv4Addr::LOCALHOST)
        );
        assert_eq!(
            challenge.fragment(),
            format!("#cobalt-login=v1.{}.{}", address.port(), challenge.nonce)
        );
    }

    #[test]
    fn request_requires_exact_http_metadata_and_json_shape() {
        let valid = request_bytes(
            "POST",
            "127.0.0.1:43125",
            "/bomtoon-login/nonce",
            "application/json",
            &valid_body(),
        );
        assert!(parse_request(&valid, PORT, NONCE).is_ok());
        let lowercase_host = replace_once(&valid, "Host:", "host:");
        let lowercase_type = replace_once(&lowercase_host, "Content-Type:", "content-type:");
        let lowercase_length = replace_once(&lowercase_type, "Content-Length:", "content-length:");
        assert!(parse_request(&lowercase_length, PORT, NONCE).is_ok());
        for rejected in [
            replace_once(&valid, "POST", "GET"),
            replace_once(&valid, "127.0.0.1:43125", "localhost:43125"),
            replace_once(&valid, "/bomtoon-login/nonce", "/bomtoon-login/wrong"),
            replace_once(&valid, "application/json", "text/plain"),
            request_with_body(&format!(
                r#"{{"version":1,"version":1,"fingerprint":"{FINGERPRINT}","cookies":[]}}"#
            )),
            request_with_body(&format!(
                r#"{{"version":1,"fingerprint":"{FINGERPRINT}","cookies":[],"extra":true}}"#
            )),
            request_with_body(&valid_body().replace("\"version\":1", "\"version\":1.0")),
            request_with_body(&valid_body().replace(",\"cookies\":[]", "")),
        ] {
            assert!(parse_request(&rejected, PORT, NONCE).is_err());
        }
    }

    #[test]
    fn body_and_cookie_count_limits_are_inclusive() {
        let mut maximum = valid_body();
        maximum.push_str(&" ".repeat(MAX_BODY_BYTES - maximum.len()));
        assert_eq!(maximum.len(), MAX_BODY_BYTES);
        assert!(parse_request(&request_with_body(&maximum), PORT, NONCE).is_ok());
        assert!(parse_request(&request_with_body(&format!("{maximum} ")), PORT, NONCE).is_err());

        let cookies = (0..MAX_COOKIES)
            .map(|index| cookie(&format!("cookie-{index}"), "v", ".bomtoon.tw", "/"))
            .collect::<Vec<_>>();
        assert!(parse_request(
            &request_with_body(&body_with_cookie(&cookies.join(","))),
            PORT,
            NONCE
        )
        .is_ok());
        let seventeen = (0..=MAX_COOKIES)
            .map(|index| cookie(&format!("cookie-{index}"), "v", ".bomtoon.tw", "/"))
            .collect::<Vec<_>>();
        assert!(parse_request(
            &request_with_body(&body_with_cookie(&seventeen.join(","))),
            PORT,
            NONCE
        )
        .is_err());
    }

    #[test]
    fn cookie_string_byte_limits_are_inclusive() {
        for (field, limit) in [
            ("name", 128_usize),
            ("value", 4_096),
            ("domain", 255),
            ("path", 1_024),
        ] {
            for (size, accepted) in [(limit, true), (limit + 1, false)] {
                let bounded = "x".repeat(size);
                let candidate = match field {
                    "name" => cookie(&bounded, "v", "d", "/"),
                    "value" => cookie("n", &bounded, "d", "/"),
                    "domain" => cookie("n", "v", &bounded, "/"),
                    "path" => cookie("n", "v", "d", &bounded),
                    _ => unreachable!(),
                };
                assert_eq!(
                    parse_request(
                        &request_with_body(&body_with_cookie(&candidate)),
                        PORT,
                        NONCE
                    )
                    .is_ok(),
                    accepted,
                    "{field} length {size}"
                );
            }
        }
    }

    #[test]
    fn fingerprint_and_cookie_objects_are_strict() {
        let uppercase = valid_body().replacen(FINGERPRINT, &"A".repeat(64), 1);
        assert!(parse_request(&request_with_body(&uppercase), PORT, NONCE).is_err());
        let short = valid_body().replacen(FINGERPRINT, &"a".repeat(63), 1);
        assert!(parse_request(&request_with_body(&short), PORT, NONCE).is_err());
        let missing = cookie("n", "v", "d", "/").replace("\"path\":\"/\",", "");
        assert!(
            parse_request(&request_with_body(&body_with_cookie(&missing)), PORT, NONCE).is_err()
        );
        let unknown = cookie("n", "v", "d", "/").replacen(
            "\"secure\":true",
            "\"secure\":true,\"extra\":false",
            1,
        );
        assert!(
            parse_request(&request_with_body(&body_with_cookie(&unknown)), PORT, NONCE).is_err()
        );
        let duplicate = cookie("n", "v", "d", "/").replacen(
            "\"secure\":true",
            "\"secure\":true,\"name\":\"other\"",
            1,
        );
        assert!(parse_request(
            &request_with_body(&body_with_cookie(&duplicate)),
            PORT,
            NONCE
        )
        .is_err());
    }

    #[test]
    fn malformed_utf8_and_non_boolean_secure_are_rejected() {
        let mut invalid_body = request_with_body(&valid_body());
        let fingerprint_start = invalid_body
            .windows(FINGERPRINT.len())
            .position(|window| window == FINGERPRINT.as_bytes())
            .expect("fingerprint bytes");
        invalid_body[fingerprint_start] = 0xff;
        assert!(parse_request(&invalid_body, PORT, NONCE).is_err());

        let mut invalid_header = request_with_body(&valid_body());
        invalid_header[0] = 0xff;
        assert!(parse_request(&invalid_header, PORT, NONCE).is_err());

        let non_boolean = cookie("n", "v", "d", "/").replace("true", "1");
        assert!(parse_request(
            &request_with_body(&body_with_cookie(&non_boolean)),
            PORT,
            NONCE
        )
        .is_err());
    }

    #[test]
    fn framing_headers_are_unique_canonical_and_bounded() {
        let valid = request_with_body(&valid_body());
        let length = valid_body().len().to_string();
        let content_length = format!("Content-Length: {length}\r\n");
        for rejected in [
            replace_once(&valid, &content_length, ""),
            replace_once(
                &valid,
                &content_length,
                &format!("{content_length}{content_length}"),
            ),
            replace_once(
                &valid,
                "Host: 127.0.0.1:43125\r\n",
                "Host: 127.0.0.1:43125\r\nHOST: 127.0.0.1:43125\r\n",
            ),
            replace_once(
                &valid,
                "Content-Type: application/json\r\n",
                "Content-Type: application/json\r\ncontent-type: application/json\r\n",
            ),
            replace_once(
                &valid,
                &content_length,
                &format!("Transfer-Encoding: chunked\r\n{content_length}"),
            ),
            replace_once(
                &valid,
                &content_length,
                &format!("Content-Length: 0{length}\r\n"),
            ),
            [valid.as_slice(), b"x"].concat(),
        ] {
            assert!(parse_request(&rejected, PORT, NONCE).is_err());
        }

        assert!(parse_request(
            &request_with_header_size(&valid_body(), 8 * 1_024),
            PORT,
            NONCE
        )
        .is_ok());
        assert!(parse_request(
            &request_with_header_size(&valid_body(), 8 * 1_024 + 1),
            PORT,
            NONCE
        )
        .is_err());
    }

    #[test]
    fn production_host_lock_port_is_fixed() {
        assert_eq!(HOST_LOCK_PORT, 53_941);
    }

    #[test]
    fn host_lock_excludes_repeated_attempts_and_releases_on_drop() {
        let port = unused_loopback_port();
        let first = HostLock::acquire_at(port).expect("first lock");
        for probe in 0..256 {
            assert!(
                HostLock::acquire_at(port).is_err(),
                "probe {probe} bypassed active lock"
            );
        }
        drop(first);
        let second = HostLock::acquire_at(port).expect("reacquire after drop");
        drop(second);
    }

    #[test]
    fn host_lock_fails_closed_when_the_port_is_prebound() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("prebind lock port");
        let port = listener.local_addr().expect("lock address").port();
        assert!(HostLock::acquire_at(port).is_err());
    }

    #[test]
    fn invalid_request_is_retryable_then_success_is_terminal_and_secret_free() {
        let (listener, challenge, address) = live_pair();
        let port = address.port();
        let handle = thread::spawn(move || {
            wait_for_payload(
                &listener,
                &challenge,
                Instant::now() + Duration::from_secs(5),
            )
        });

        let mut invalid = TcpStream::connect(address).expect("invalid connection");
        invalid
            .write_all(&live_request(port, "GET", &valid_body()))
            .expect("send invalid request");
        let invalid_response = read_response(invalid);
        assert_eq!(
            invalid_response,
            b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
        );

        let mut valid = TcpStream::connect(address).expect("valid connection");
        valid
            .write_all(&live_request(port, "POST", &valid_body()))
            .expect("send valid request");
        let pending = pending_from(handle);
        assert_eq!(pending.payload().fingerprint, FINGERPRINT);
        pending.succeed().expect("accept handoff");
        let response = read_response(valid);
        assert_eq!(
            response,
            b"HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
        );
        assert!(TcpStream::connect(address).is_err());
    }

    #[test]
    fn schema_valid_payload_is_terminal_even_when_caller_rejects_it() {
        let secret_name = "credential-name";
        let secret_value = "credential-value";
        let secret_domain = "private.example";
        let body = body_with_cookie(&cookie(
            secret_name,
            secret_value,
            secret_domain,
            "/private",
        ));
        let (listener, challenge, address) = live_pair();
        let port = address.port();
        let handle = thread::spawn(move || {
            wait_for_payload(
                &listener,
                &challenge,
                Instant::now() + Duration::from_secs(5),
            )
        });
        let mut stream = TcpStream::connect(address).expect("connection");
        stream
            .write_all(&live_request(port, "POST", &body))
            .expect("send payload");
        let pending = pending_from(handle);
        assert_eq!(pending.payload().cookies[0].value, secret_value);
        pending.fail().expect("reject handoff");
        let response = read_response(stream);
        assert!(response.starts_with(b"HTTP/1.1 422 Unprocessable Content\r\n"));
        for secret in [
            FINGERPRINT,
            secret_name,
            secret_value,
            secret_domain,
            "/private",
        ] {
            assert!(!String::from_utf8_lossy(&response).contains(secret));
        }
    }

    #[test]
    fn one_slow_connection_gets_at_most_two_seconds() {
        let (listener, challenge, address) = live_pair();
        let port = address.port();
        let handle = thread::spawn(move || {
            wait_for_payload(
                &listener,
                &challenge,
                Instant::now() + Duration::from_secs(5),
            )
        });
        let mut slow = TcpStream::connect(address).expect("slow connection");
        slow.write_all(b"POST /bomtoon-login/nonce HTTP/1.1\r\n")
            .expect("partial request");
        let started = Instant::now();
        thread::sleep(Duration::from_secs(1));
        slow.write_all(b"x").expect("continue partial request");
        let response = read_response(slow);
        assert!(response.starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
        assert!(
            started.elapsed() >= CONNECTION_READ_TIMEOUT.saturating_sub(Duration::from_millis(500))
        );
        assert!(started.elapsed() < Duration::from_millis(2_800));

        let mut valid = TcpStream::connect(address).expect("valid connection");
        valid
            .write_all(&live_request(port, "POST", &valid_body()))
            .expect("send valid request");
        pending_from(handle).succeed().expect("finish handoff");
        assert!(read_response(valid).starts_with(b"HTTP/1.1 204 No Content\r\n"));
    }

    #[test]
    fn thirty_two_refused_requests_stop_the_listener_loop() {
        let (listener, challenge, address) = live_pair();
        let port = address.port();
        let handle = thread::spawn(move || {
            wait_for_payload(
                &listener,
                &challenge,
                Instant::now() + Duration::from_secs(10),
            )
        });
        for _ in 0..MAX_REJECTED_REQUESTS {
            let mut stream = TcpStream::connect(address).expect("rejected connection");
            stream
                .write_all(&live_request(port, "GET", &valid_body()))
                .expect("send rejected request");
            assert!(read_response(stream).starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
        }
        assert!(matches!(
            handle.join().expect("wait thread"),
            Err(HandoffError::Listener)
        ));
    }

    #[test]
    fn short_overall_deadline_returns_timeout() {
        let (listener, challenge, _) = live_pair();
        let started = Instant::now();
        assert!(matches!(
            wait_for_payload(
                &listener,
                &challenge,
                Instant::now() + Duration::from_millis(40)
            ),
            Err(HandoffError::Timeout)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
