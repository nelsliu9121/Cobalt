use kobo_json::{ObjectBuilder, Value};
use kobo_sim::SimulatorAuthPaths;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const USAGE: &str = "usage: kobo bomtoon login (--device IP | --sim)";
const LOGIN_URL: &str = "https://www.bomtoon.tw/user/login";
const COOKIE_URL: &str = "https://www.bomtoon.tw/";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const LOGIN_POLL_INTERVAL: Duration = Duration::from_millis(500);
const DEVICE_INSTALL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CDP_FRAME_BYTES: usize = 1024 * 1024;
const UNSUPPORTED_HOST: &str = "BOMTOON login is supported only on macOS";
const BROWSER_LAUNCH_FAILED: &str = "browser launch failed";
const LOGIN_TIMED_OUT: &str = "login timeout";
const SESSION_VALIDATION_FAILED: &str = "session validation failed";
const COOKIE_SELECTION_FAILED: &str = "cookie selection failed";
const TARGET_INSTALLATION_FAILED: &str = "target installation failed";
const SECURE_COOKIE: &str = "__Secure-next-auth.session-token";
const INSECURE_COOKIE: &str = "next-auth.session-token";
const SESSION_SECRET: &str = "bomtoon-session";
const MANAGED_STATE: &str = "bomtoon-access-token.state";

const CHROME_LAUNCH_PROGRAM: &str = r#"exec 3<&0
exec 4>&1
browser=$1
shift
exec "$browser" "$@"
"#;

const DEVICE_INSTALL_PROGRAM: &str = r#"set -eu
umask 077
secrets=/mnt/onboard/.adds/cobalt/secrets
state=/mnt/onboard/.adds/cobalt/state
cookie="$secrets/bomtoon-session"
managed="$state/bomtoon-access-token.state"
temporary="$secrets/.bomtoon-session.login.$$"
backup="$secrets/.bomtoon-session.backup.$$"
state_backup="$state/.bomtoon-access-token.state.backup.$$"
had_cookie=0
had_state=0
installed=0
complete=0
rollback() {
    status=$?
    set +e
    trap - EXIT HUP INT TERM
    if [ "$complete" -ne 1 ]; then
        rm -f "$temporary"
        if [ "$had_state" -eq 1 ] && [ -e "$state_backup" ]; then
            rm -f "$managed"
            mv "$state_backup" "$managed" || true
        fi
        if [ "$installed" -eq 1 ]; then
            rm -f "$cookie"
        fi
        if [ "$had_cookie" -eq 1 ] && [ -e "$backup" ]; then
            mv "$backup" "$cookie" || true
        fi
    fi
    exit "$status"
}
trap rollback EXIT HUP INT TERM
mkdir -p "$secrets" "$state"
chmod 700 "$secrets" "$state"
set -C
: > "$temporary"
set +C
chmod 600 "$temporary"
cat > "$temporary"
if [ -e "$cookie" ]; then
    mv "$cookie" "$backup"
    had_cookie=1
fi
mv "$temporary" "$cookie"
installed=1
if [ -e "$managed" ]; then
    mv "$managed" "$state_backup"
    had_state=1
fi
if [ "$had_cookie" -eq 1 ]; then
    rm -f "$backup"
fi
if [ "$had_state" -eq 1 ]; then
    rm -f "$state_backup"
fi
complete=1
trap - EXIT HUP INT TERM
exit 0
"#;

const BROWSER_SESSION_EXPRESSION: &str = r#"(async () => {
    try {
        const response = await fetch('/api/auth/session', {
            credentials: 'same-origin',
            cache: 'no-store',
            headers: { Accept: 'application/json' }
        });
        if (!response.ok) return { authenticated: false };
        const session = await response.json();
        const user = session && session.user;
        if (!user || typeof user !== 'object' || Array.isArray(user)) {
            return { authenticated: false };
        }
        const validToken = (value) => value
            && typeof value === 'object'
            && !Array.isArray(value)
            && typeof value.token === 'string'
            && value.token.length > 0
            && !/[\u0000-\u001f\u007f]/.test(value.token)
            && Number.isSafeInteger(value.createdAt)
            && value.createdAt >= 0
            && Number.isSafeInteger(value.expiredAt)
            && value.expiredAt > value.createdAt;
        if (!validToken(user.accessToken) || !validToken(user.refreshToken)) {
            return { authenticated: false };
        }
        const encoder = new TextEncoder();
        const access = encoder.encode(user.accessToken.token);
        const refresh = encoder.encode(user.refreshToken.token);
        const material = new Uint8Array(access.length + 1 + refresh.length);
        material.set(access, 0);
        material[access.length] = 0;
        material.set(refresh, access.length + 1);
        const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', material));
        const tokenFingerprint = Array.from(digest, byte => byte.toString(16).padStart(2, '0')).join('');
        return { authenticated: true, tokenFingerprint };
    } catch (_) {
        return { authenticated: false };
    }
})()"#;

static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
enum LoginTarget {
    Device(String),
    Simulator,
}

pub fn command(arguments: &[String]) -> Result<(), String> {
    let target = parse_target(arguments)?;
    if !cfg!(target_os = "macos") {
        return Err(UNSUPPORTED_HOST.to_owned());
    }

    #[cfg(target_os = "macos")]
    {
        login(target)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = target;
        Err(UNSUPPORTED_HOST.to_owned())
    }
}

fn parse_target(arguments: &[String]) -> Result<LoginTarget, String> {
    match arguments {
        [verb, flag, host]
            if verb == "login" && flag == "--device" && super::valid_device_host(host) =>
        {
            Ok(LoginTarget::Device(host.clone()))
        }
        [verb, flag] if verb == "login" && flag == "--sim" => Ok(LoginTarget::Simulator),
        _ => Err(USAGE.to_owned()),
    }
}

#[cfg(target_os = "macos")]
fn login(target: LoginTarget) -> Result<(), String> {
    let browser = discover_chrome().ok_or_else(|| BROWSER_LAUNCH_FAILED.to_owned())?;
    let profile = create_private_profile_at(&std::env::temp_dir())
        .map_err(|_| BROWSER_LAUNCH_FAILED.to_owned())?;
    let child = match launch_chrome(&browser, &profile) {
        Ok(child) => child,
        Err(()) => {
            let _ = fs::remove_dir_all(&profile);
            return Err(BROWSER_LAUNCH_FAILED.to_owned());
        }
    };
    let mut guard = ChromeGuard::new(child, profile, RealProfileCleaner);
    let input = guard
        .child_mut()
        .stdin
        .take()
        .ok_or_else(|| BROWSER_LAUNCH_FAILED.to_owned())?;
    let output = guard
        .child_mut()
        .stdout
        .take()
        .ok_or_else(|| BROWSER_LAUNCH_FAILED.to_owned())?;
    let mut pipe = DevToolsPipe::new(output, input);
    let browser_result = browser_login(&mut pipe, LOGIN_TIMEOUT);
    drop(pipe);
    let cleanup_result = guard.close();
    let (cookie, browser_fingerprint) = match browser_result {
        Ok(value) if cleanup_result.is_ok() => value,
        Ok(_) => return Err(BROWSER_LAUNCH_FAILED.to_owned()),
        Err(error) => return Err(error.message().to_owned()),
    };

    validate_and_install(
        &cookie,
        &browser_fingerprint,
        |selected| {
            kobo_net::bomtoon::validate_session_cookie(selected)
                .map_err(|_| SESSION_VALIDATION_FAILED.to_owned())
        },
        |selected| install_target(&target, selected),
    )
}

fn chrome_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let system_chrome =
        PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    let system_chromium = PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium");
    let user_chrome = home.map(|path| {
        path.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
    });
    let user_chromium =
        home.map(|path| path.join("Applications/Chromium.app/Contents/MacOS/Chromium"));
    [
        Some(system_chrome),
        user_chrome,
        Some(system_chromium),
        user_chromium,
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn discover_chrome_with(
    home: Option<&Path>,
    mut is_file: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    chrome_candidates(home)
        .into_iter()
        .find(|candidate| is_file(candidate))
}

#[cfg(target_os = "macos")]
fn discover_chrome() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    discover_chrome_with(home.as_deref(), Path::is_file)
}

fn private_name(prefix: &str) -> String {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{epoch}-{counter}", std::process::id())
}

fn create_private_profile_at(root: &Path) -> io::Result<PathBuf> {
    for _ in 0..128 {
        let path = root.join(private_name("kobo-bomtoon-chrome"));
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&path) {
            Ok(()) => {
                if let Err(error) =
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                {
                    let _ = fs::remove_dir_all(&path);
                    return Err(error);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "temporary profile collision",
    ))
}

#[cfg(target_os = "macos")]
fn launch_chrome(browser: &Path, profile: &Path) -> Result<Child, ()> {
    let profile_argument = format!("--user-data-dir={}", profile.display());
    Command::new("/bin/sh")
        .args(["-c", CHROME_LAUNCH_PROGRAM, "kobo-chrome"])
        .arg(browser)
        .args([
            "--remote-debugging-pipe",
            profile_argument.as_str(),
            "--no-first-run",
            "--no-default-browser-check",
            LOGIN_URL,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())
}

trait BrowserProcess {
    fn stop(&mut self) -> io::Result<()>;
    fn wait_for_exit(&mut self) -> io::Result<()>;
}

impl BrowserProcess for Child {
    fn stop(&mut self) -> io::Result<()> {
        match self.kill() {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn wait_for_exit(&mut self) -> io::Result<()> {
        self.wait().map(|_| ())
    }
}

trait ProfileCleaner {
    fn remove_profile(&mut self, profile: &Path) -> io::Result<()>;
}

struct RealProfileCleaner;

impl ProfileCleaner for RealProfileCleaner {
    fn remove_profile(&mut self, profile: &Path) -> io::Result<()> {
        match fs::remove_dir_all(profile) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

struct ChromeGuard<C: BrowserProcess, F: ProfileCleaner> {
    child: C,
    profile: PathBuf,
    cleaner: F,
    cleaned: bool,
}

impl<C: BrowserProcess, F: ProfileCleaner> ChromeGuard<C, F> {
    fn new(child: C, profile: PathBuf, cleaner: F) -> Self {
        Self {
            child,
            profile,
            cleaner,
            cleaned: false,
        }
    }

    fn child_mut(&mut self) -> &mut C {
        &mut self.child
    }

    fn cleanup(&mut self) -> Result<(), ()> {
        if self.cleaned {
            return Ok(());
        }
        self.cleaned = true;
        let stopped = self.child.stop().is_ok();
        let waited = self.child.wait_for_exit().is_ok();
        let removed = self.cleaner.remove_profile(&self.profile).is_ok();
        (stopped && waited && removed).then_some(()).ok_or(())
    }

    fn close(mut self) -> Result<(), ()> {
        self.cleanup()
    }
}

impl<C: BrowserProcess, F: ProfileCleaner> Drop for ChromeGuard<C, F> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CdpError {
    Transport,
    Framing,
    Protocol,
}

struct DevToolsPipe<R: Read, W: Write> {
    reader: R,
    writer: W,
    next_id: u32,
    buffered: Vec<u8>,
}

impl<R: Read, W: Write> DevToolsPipe<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
            buffered: Vec::new(),
        }
    }

    fn call(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, CdpError> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or(CdpError::Protocol)?;
        let mut request = ObjectBuilder::new()
            .set("id", id)
            .set("method", method)
            .set("params", params);
        if let Some(session_id) = session_id {
            request = request.set("sessionId", session_id);
        }
        let encoded = request.build().to_json();
        self.writer
            .write_all(encoded.as_bytes())
            .and_then(|()| self.writer.write_all(&[0]))
            .and_then(|()| self.writer.flush())
            .map_err(|_| CdpError::Transport)?;

        loop {
            let frame = self.read_frame()?;
            let text = std::str::from_utf8(&frame).map_err(|_| CdpError::Framing)?;
            let response = kobo_json::parse(text).map_err(|_| CdpError::Framing)?;
            let Some(response_id) = response.get("id").and_then(Value::as_i64) else {
                continue;
            };
            if response_id != i64::from(id) {
                continue;
            }
            if response.get("error").is_some() {
                return Err(CdpError::Protocol);
            }
            return response
                .get("result")
                .cloned()
                .ok_or(CdpError::Protocol);
        }
    }

    fn read_frame(&mut self) -> Result<Vec<u8>, CdpError> {
        loop {
            if let Some(end) = self.buffered.iter().position(|byte| *byte == 0) {
                let mut frame = self.buffered.drain(..=end).collect::<Vec<_>>();
                frame.pop();
                if frame.is_empty() {
                    return Err(CdpError::Framing);
                }
                return Ok(frame);
            }
            if self.buffered.len() >= MAX_CDP_FRAME_BYTES {
                return Err(CdpError::Framing);
            }
            let mut chunk = [0_u8; 8192];
            let read = self
                .reader
                .read(&mut chunk)
                .map_err(|_| CdpError::Transport)?;
            if read == 0 {
                return Err(CdpError::Transport);
            }
            if self.buffered.len().saturating_add(read) > MAX_CDP_FRAME_BYTES {
                return Err(CdpError::Framing);
            }
            self.buffered.extend_from_slice(&chunk[..read]);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserFlowError {
    Launch,
    Timeout,
    CookieSelection,
}

impl BrowserFlowError {
    fn message(self) -> &'static str {
        match self {
            Self::Launch => BROWSER_LAUNCH_FAILED,
            Self::Timeout => LOGIN_TIMED_OUT,
            Self::CookieSelection => COOKIE_SELECTION_FAILED,
        }
    }
}

fn empty_object() -> Value {
    ObjectBuilder::new().build()
}

fn browser_login<R: Read, W: Write>(
    pipe: &mut DevToolsPipe<R, W>,
    timeout: Duration,
) -> Result<(String, String), BrowserFlowError> {
    let target_id = bomtoon_target(pipe)?;
    let attached = pipe
        .call(
            "Target.attachToTarget",
            ObjectBuilder::new()
                .set("targetId", target_id)
                .set("flatten", true)
                .build(),
            None,
        )
        .map_err(|_| BrowserFlowError::Launch)?;
    let session_id = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or(BrowserFlowError::Launch)?
        .to_owned();
    let deadline = Instant::now() + timeout;
    let browser_fingerprint = loop {
        if Instant::now() >= deadline {
            return Err(BrowserFlowError::Timeout);
        }
        if let Ok(Some(fingerprint)) = page_authentication(pipe, &session_id) {
            break fingerprint;
        }
        thread::sleep(LOGIN_POLL_INTERVAL);
    };
    let cookies = pipe
        .call(
            "Network.getCookies",
            ObjectBuilder::new()
                .set("urls", vec![COOKIE_URL])
                .build(),
            Some(&session_id),
        )
        .map_err(|_| BrowserFlowError::CookieSelection)?;
    let cookies = parse_network_cookies(&cookies).map_err(|_| BrowserFlowError::CookieSelection)?;
    let selected = select_session_cookie(&cookies).map_err(|_| BrowserFlowError::CookieSelection)?;
    Ok((selected, browser_fingerprint))
}

fn bomtoon_target<R: Read, W: Write>(
    pipe: &mut DevToolsPipe<R, W>,
) -> Result<String, BrowserFlowError> {
    let targets = pipe
        .call("Target.getTargets", empty_object(), None)
        .map_err(|_| BrowserFlowError::Launch)?;
    let infos = targets
        .get("targetInfos")
        .and_then(Value::as_array)
        .ok_or(BrowserFlowError::Launch)?;
    infos
        .iter()
        .find_map(|target| {
            let kind = target.get("type").and_then(Value::as_str)?;
            let url = target.get("url").and_then(Value::as_str)?;
            let target_id = target.get("targetId").and_then(Value::as_str)?;
            (kind == "page" && bomtoon_page_url(url)).then(|| target_id.to_owned())
        })
        .ok_or(BrowserFlowError::Launch)
}

fn bomtoon_page_url(url: &str) -> bool {
    url == "https://www.bomtoon.tw" || url.starts_with("https://www.bomtoon.tw/")
}

fn page_authentication<R: Read, W: Write>(
    pipe: &mut DevToolsPipe<R, W>,
    session_id: &str,
) -> Result<Option<String>, CdpError> {
    let evaluated = pipe.call(
        "Runtime.evaluate",
        ObjectBuilder::new()
            .set("expression", BROWSER_SESSION_EXPRESSION)
            .set("awaitPromise", true)
            .set("returnByValue", true)
            .build(),
        Some(session_id),
    )?;
    if evaluated.get("exceptionDetails").is_some() {
        return Err(CdpError::Protocol);
    }
    let value = evaluated
        .get("result")
        .and_then(|result| result.get("value"))
        .ok_or(CdpError::Protocol)?;
    match value.get("authenticated").and_then(Value::as_bool) {
        Some(false) => Ok(None),
        Some(true) => {
            let fingerprint = value
                .get("tokenFingerprint")
                .and_then(Value::as_str)
                .filter(|value| valid_fingerprint(value))
                .ok_or(CdpError::Protocol)?;
            Ok(Some(fingerprint.to_owned()))
        }
        None => Err(CdpError::Protocol),
    }
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BrowserCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
}

fn parse_network_cookies(result: &Value) -> Result<Vec<BrowserCookie>, ()> {
    result
        .get("cookies")
        .and_then(Value::as_array)
        .ok_or(())?
        .iter()
        .map(|cookie| {
            Ok(BrowserCookie {
                name: cookie.get("name").and_then(Value::as_str).ok_or(())?.to_owned(),
                value: cookie
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or(())?
                    .to_owned(),
                domain: cookie
                    .get("domain")
                    .and_then(Value::as_str)
                    .ok_or(())?
                    .to_owned(),
                path: cookie.get("path").and_then(Value::as_str).ok_or(())?.to_owned(),
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CookieMember {
    Base,
    Chunk(usize),
}

fn family_member(name: &str, family: &str) -> Result<Option<CookieMember>, ()> {
    if name == family {
        return Ok(Some(CookieMember::Base));
    }
    let Some(suffix) = name.strip_prefix(family).and_then(|name| name.strip_prefix('.')) else {
        return Ok(None);
    };
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    let index = suffix.parse::<usize>().map_err(|_| ())?;
    if suffix != index.to_string() {
        return Err(());
    }
    Ok(Some(CookieMember::Chunk(index)))
}

fn has_family(cookies: &[BrowserCookie], family: &str) -> bool {
    cookies.iter().any(|cookie| {
        cookie
            .name
            .strip_prefix(family)
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('.'))
    })
}

fn valid_cookie_scope(cookie: &BrowserCookie) -> bool {
    matches!(
        cookie.domain.strip_prefix('.').unwrap_or(&cookie.domain),
        "bomtoon.tw" | "www.bomtoon.tw"
    ) && cookie.path == "/"
}

fn select_session_cookie(cookies: &[BrowserCookie]) -> Result<String, ()> {
    let family = if has_family(cookies, SECURE_COOKIE) {
        SECURE_COOKIE
    } else if has_family(cookies, INSECURE_COOKIE) {
        INSECURE_COOKIE
    } else {
        return Err(());
    };
    select_cookie_family(cookies, family)
}

fn select_cookie_family(cookies: &[BrowserCookie], family: &str) -> Result<String, ()> {
    let mut base = None;
    let mut chunks = BTreeMap::new();
    for cookie in cookies {
        let Some(member) = family_member(&cookie.name, family)? else {
            continue;
        };
        if !valid_cookie_scope(cookie)
            || cookie.value.is_empty()
            || cookie.value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(());
        }
        match member {
            CookieMember::Base => {
                if base.replace(cookie).is_some() {
                    return Err(());
                }
            }
            CookieMember::Chunk(index) => {
                if chunks.insert(index, cookie).is_some() {
                    return Err(());
                }
            }
        }
    }
    if base.is_some() && !chunks.is_empty() {
        return Err(());
    }
    let selected = if let Some(cookie) = base {
        format!("{}={}", cookie.name, cookie.value)
    } else {
        if chunks.is_empty() || chunks.keys().copied().ne(0..chunks.len()) {
            return Err(());
        }
        let mut selected = String::new();
        for cookie in chunks.values() {
            if !selected.is_empty() {
                selected.push_str("; ");
            }
            write!(&mut selected, "{}={}", cookie.name, cookie.value).map_err(|_| ())?;
        }
        selected
    };
    if selected.len() > kobo_net::bomtoon::SESSION_COOKIE_MAX_BYTES {
        return Err(());
    }
    Ok(selected)
}

fn validate_and_install(
    cookie: &str,
    browser_fingerprint: &str,
    validate: impl FnOnce(&str) -> Result<String, String>,
    install: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    let validated_fingerprint =
        validate(cookie).map_err(|_| SESSION_VALIDATION_FAILED.to_owned())?;
    if !valid_fingerprint(&validated_fingerprint) || validated_fingerprint != browser_fingerprint {
        return Err(SESSION_VALIDATION_FAILED.to_owned());
    }
    install(cookie).map_err(|_| TARGET_INSTALLATION_FAILED.to_owned())
}

fn install_target(target: &LoginTarget, cookie: &str) -> Result<(), String> {
    match target {
        LoginTarget::Device(host) => install_device(host, cookie),
        LoginTarget::Simulator => install_simulator(cookie),
    }
    .map_err(|_| TARGET_INSTALLATION_FAILED.to_owned())
}

fn device_install_command(host: &str) -> Command {
    let mut command = super::remote_shell_command(&format!("root@{host}"));
    command.arg(DEVICE_INSTALL_PROGRAM);
    command
}

fn install_device_with(
    host: &str,
    cookie: &str,
    run: impl FnOnce(Command, &[u8]) -> Result<(), ()>,
) -> Result<(), ()> {
    if !super::valid_device_host(host) {
        return Err(());
    }
    run(device_install_command(host), cookie.as_bytes())
}

fn install_device(host: &str, cookie: &str) -> Result<(), ()> {
    install_device_with(host, cookie, run_device_install)
}

fn run_device_install(mut command: Command, cookie: &[u8]) -> Result<(), ()> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|_| ())?;
    let write_result = child
        .stdin
        .take()
        .ok_or(())
        .and_then(|mut input| input.write_all(cookie).map_err(|_| ()));
    if write_result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(());
    }
    let status = super::wait_for_remote_child(
        &mut child,
        "BOMTOON credential installation",
        DEVICE_INSTALL_TIMEOUT,
    )
    .map_err(|_| ())?;
    status.success().then_some(()).ok_or(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallPoint {
    CookieInstalled,
    StateDetached,
}

struct SimulatorTransaction {
    cookie: PathBuf,
    cookie_backup: PathBuf,
    temporary: PathBuf,
    state: PathBuf,
    state_backup: PathBuf,
    old_cookie: Option<(Vec<u8>, u32)>,
    old_state: Option<(Vec<u8>, u32)>,
    cookie_moved: bool,
    cookie_installed: bool,
    state_moved: bool,
    committed: bool,
}

impl SimulatorTransaction {
    fn new(paths: &SimulatorAuthPaths) -> Self {
        let cookie = paths.secrets.join(SESSION_SECRET);
        let state = paths.state.join(MANAGED_STATE);
        Self {
            cookie_backup: paths
                .secrets
                .join(private_name(".bomtoon-session.backup")),
            temporary: paths.secrets.join(private_name(".bomtoon-session.login")),
            state_backup: paths
                .state
                .join(private_name(".bomtoon-access-token.state.backup")),
            cookie,
            state,
            old_cookie: None,
            old_state: None,
            cookie_moved: false,
            cookie_installed: false,
            state_moved: false,
            committed: false,
        }
    }

    fn remember(path: &Path) -> io::Result<(Vec<u8>, u32)> {
        let bytes = fs::read(path)?;
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        Ok((bytes, mode))
    }

    fn restore(path: &Path, saved: &(Vec<u8>, u32)) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(saved.1)
            .open(path)?;
        file.write_all(&saved.0)?;
        file.set_permissions(fs::Permissions::from_mode(saved.1))
    }

    fn wipe_saved(&mut self) {
        if let Some((bytes, _)) = &mut self.old_cookie {
            bytes.fill(0);
        }
        if let Some((bytes, _)) = &mut self.old_state {
            bytes.fill(0);
        }
    }
}

impl Drop for SimulatorTransaction {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.temporary);
            if self.state_moved {
                let _ = fs::remove_file(&self.state);
                if self.state_backup.exists() {
                    let _ = fs::rename(&self.state_backup, &self.state);
                } else if let Some(saved) = &self.old_state {
                    let _ = Self::restore(&self.state, saved);
                }
            }
            if self.cookie_installed {
                let _ = fs::remove_file(&self.cookie);
            }
            if self.cookie_moved {
                if self.cookie_backup.exists() {
                    let _ = fs::rename(&self.cookie_backup, &self.cookie);
                } else if let Some(saved) = &self.old_cookie {
                    let _ = Self::restore(&self.cookie, saved);
                }
            }
        }
        let _ = fs::remove_file(&self.temporary);
        let _ = fs::remove_file(&self.cookie_backup);
        let _ = fs::remove_file(&self.state_backup);
        self.wipe_saved();
    }
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn install_simulator_at_with(
    cookie: &str,
    paths: &SimulatorAuthPaths,
    mut checkpoint: impl FnMut(InstallPoint) -> io::Result<()>,
) -> io::Result<()> {
    ensure_private_directory(&paths.secrets)?;
    ensure_private_directory(&paths.state)?;
    let mut transaction = SimulatorTransaction::new(paths);
    let mut temporary = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&transaction.temporary)?;
    temporary.write_all(cookie.as_bytes())?;
    temporary.set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.sync_all()?;
    drop(temporary);

    if transaction.cookie.exists() {
        transaction.old_cookie = Some(SimulatorTransaction::remember(&transaction.cookie)?);
        fs::rename(&transaction.cookie, &transaction.cookie_backup)?;
        transaction.cookie_moved = true;
    }
    fs::rename(&transaction.temporary, &transaction.cookie)?;
    transaction.cookie_installed = true;
    checkpoint(InstallPoint::CookieInstalled)?;

    if transaction.state.exists() {
        transaction.old_state = Some(SimulatorTransaction::remember(&transaction.state)?);
        fs::rename(&transaction.state, &transaction.state_backup)?;
        transaction.state_moved = true;
    }
    checkpoint(InstallPoint::StateDetached)?;

    if transaction.cookie_moved {
        fs::remove_file(&transaction.cookie_backup)?;
    }
    if transaction.state_moved {
        fs::remove_file(&transaction.state_backup)?;
    }
    transaction.committed = true;
    Ok(())
}

fn install_simulator_at(cookie: &str, paths: &SimulatorAuthPaths) -> io::Result<()> {
    install_simulator_at_with(cookie, paths, |_| Ok(()))
}

fn install_simulator(cookie: &str) -> Result<(), ()> {
    install_simulator_at(cookie, &kobo_sim::simulator_auth_paths()).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Cursor;
    use std::rc::Rc;

    fn argument_list(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn cookie(name: &str, value: &str) -> BrowserCookie {
        BrowserCookie {
            name: name.to_owned(),
            value: value.to_owned(),
            domain: ".bomtoon.tw".to_owned(),
            path: "/".to_owned(),
        }
    }

    fn fingerprint(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    struct TestDirectories {
        root: PathBuf,
        paths: SimulatorAuthPaths,
    }

    impl TestDirectories {
        fn new() -> Self {
            let root = create_private_profile_at(&std::env::temp_dir()).expect("test root");
            let paths = SimulatorAuthPaths {
                secrets: root.join("secrets"),
                state: root.join("state"),
            };
            Self { root, paths }
        }
    }

    impl Drop for TestDirectories {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.paths.secrets.join(SESSION_SECRET));
            let _ = fs::remove_file(self.paths.state.join(MANAGED_STATE));
            if let Ok(entries) = fs::read_dir(&self.paths.secrets) {
                for entry in entries.flatten() {
                    let _ = fs::remove_file(entry.path());
                }
            }
            if let Ok(entries) = fs::read_dir(&self.paths.state) {
                for entry in entries.flatten() {
                    let _ = fs::remove_file(entry.path());
                }
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn login_requires_exactly_one_device_or_simulator_target() {
        assert_eq!(
            parse_target(&argument_list(&["login", "--device", "192.0.2.1"])),
            Ok(LoginTarget::Device("192.0.2.1".to_owned()))
        );
        assert_eq!(
            parse_target(&argument_list(&["login", "--sim"])),
            Ok(LoginTarget::Simulator)
        );
        for rejected in [
            argument_list(&[]),
            argument_list(&["login"]),
            argument_list(&["logout", "--sim"]),
            argument_list(&["login", "-s", "192.0.2.1"]),
            argument_list(&["login", "--device"]),
            argument_list(&["login", "--device", "reader;reboot"]),
            argument_list(&["login", "--device", "192.0.2.1", "--sim"]),
            argument_list(&["login", "--sim", "extra"]),
        ] {
            assert_eq!(parse_target(&rejected), Err(USAGE.to_owned()));
        }
    }

    #[test]
    fn chrome_discovery_checks_system_and_user_application_directories() {
        let home = Path::new("/Users/browser-owner");
        let candidates = chrome_candidates(Some(home));
        assert_eq!(
            candidates,
            vec![
                PathBuf::from(
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
                ),
                home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
                PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
                home.join("Applications/Chromium.app/Contents/MacOS/Chromium"),
            ]
        );
        let checked = Rc::new(RefCell::new(Vec::new()));
        let observed = Rc::clone(&checked);
        let wanted = candidates[3].clone();
        let found = discover_chrome_with(Some(home), |path| {
            observed.borrow_mut().push(path.to_owned());
            path == wanted
        });
        assert_eq!(found, Some(wanted));
        assert_eq!(*checked.borrow(), candidates);
    }

    #[test]
    fn secure_nextauth_cookie_wins_without_combining_families() {
        let cookies = vec![
            cookie(INSECURE_COOKIE, "insecure-family"),
            cookie(SECURE_COOKIE, "secure-family"),
        ];
        assert_eq!(
            select_session_cookie(&cookies),
            Ok(format!("{SECURE_COOKIE}=secure-family"))
        );
        let invalid_secure = vec![
            cookie(INSECURE_COOKIE, "otherwise-valid"),
            cookie(&format!("{SECURE_COOKIE}.1"), "missing-zero"),
        ];
        assert!(select_session_cookie(&invalid_secure).is_err());
    }

    #[test]
    fn contiguous_cookie_chunks_form_one_cookie_header() {
        let cookies = vec![
            cookie(&format!("{SECURE_COOKIE}.2"), "third"),
            cookie(&format!("{SECURE_COOKIE}.0"), "first"),
            cookie(&format!("{SECURE_COOKIE}.1"), "second"),
        ];
        assert_eq!(
            select_session_cookie(&cookies),
            Ok(format!(
                "{SECURE_COOKIE}.0=first; {SECURE_COOKIE}.1=second; {SECURE_COOKIE}.2=third"
            ))
        );
    }

    #[test]
    fn a_chunk_gap_control_character_or_oversized_cookie_is_rejected() {
        assert!(select_session_cookie(&[
            cookie(&format!("{SECURE_COOKIE}.0"), "first"),
            cookie(&format!("{SECURE_COOKIE}.2"), "third"),
        ])
        .is_err());
        assert!(select_session_cookie(&[cookie(SECURE_COOKIE, "line\nbreak")]).is_err());
        let prefix_bytes = SECURE_COOKIE.len() + 1;
        let boundary =
            "x".repeat(kobo_net::bomtoon::SESSION_COOKIE_MAX_BYTES - prefix_bytes);
        assert_eq!(
            select_session_cookie(&[cookie(SECURE_COOKIE, &boundary)])
                .expect("exact ceiling")
                .len(),
            kobo_net::bomtoon::SESSION_COOKIE_MAX_BYTES
        );
        let oversized = format!("{boundary}x");
        assert!(select_session_cookie(&[cookie(SECURE_COOKIE, &oversized)]).is_err());
        assert!(select_session_cookie(&[
            cookie(SECURE_COOKIE, "base"),
            cookie(&format!("{SECURE_COOKIE}.0"), "chunk"),
        ])
        .is_err());
        let mut wrong_domain = cookie(SECURE_COOKIE, "scoped");
        wrong_domain.domain = "example.invalid".to_owned();
        assert!(select_session_cookie(&[wrong_domain]).is_err());
        let mut wrong_path = cookie(SECURE_COOKIE, "scoped");
        wrong_path.path = "/user".to_owned();
        assert!(select_session_cookie(&[wrong_path]).is_err());
        assert!(select_session_cookie(&[cookie(
            &format!("{SECURE_COOKIE}.01"),
            "ambiguous"
        )])
        .is_err());
    }

    struct PartialReader {
        inner: Cursor<Vec<u8>>,
        maximum: usize,
    }

    impl Read for PartialReader {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let end = output.len().min(self.maximum);
            self.inner.read(&mut output[..end])
        }
    }

    #[test]
    fn cdp_messages_are_nul_terminated_and_partial_reads_are_reassembled() {
        let mut incoming = br#"{"method":"Runtime.consoleAPICalled","params":{}}"#.to_vec();
        incoming.push(0);
        incoming.extend_from_slice(br#"{"id":77,"result":{"ignored":true}}"#);
        incoming.push(0);
        incoming.extend_from_slice(br#"{"id":1,"result":{"accepted":true}}"#);
        incoming.push(0);
        let reader = PartialReader {
            inner: Cursor::new(incoming),
            maximum: 3,
        };
        let mut pipe = DevToolsPipe::new(reader, Vec::new());
        let result = pipe
            .call("Runtime.evaluate", empty_object(), Some("flat-session"))
            .expect("matching result");
        assert_eq!(
            result.get("accepted").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(pipe.writer.last(), Some(&0));
        let request = std::str::from_utf8(&pipe.writer[..pipe.writer.len() - 1])
            .ok()
            .and_then(|json| kobo_json::parse(json).ok())
            .expect("request JSON");
        assert_eq!(request.get("id").and_then(Value::as_i64), Some(1));
        assert_eq!(
            request.get("sessionId").and_then(Value::as_str),
            Some("flat-session")
        );

        let mut rejected = br#"{"id":1,"error":{"message":"credential-material"}}"#.to_vec();
        rejected.push(0);
        let mut pipe = DevToolsPipe::new(Cursor::new(rejected), Vec::new());
        assert_eq!(
            pipe.call("Network.getCookies", empty_object(), None),
            Err(CdpError::Protocol)
        );
        assert!(!format!("{:?}", CdpError::Protocol).contains("credential-material"));
    }

    #[test]
    fn selected_cookie_is_validated_alone_before_installation() {
        let selected = select_session_cookie(&[
            cookie(INSECURE_COOKIE, "not-selected"),
            cookie(SECURE_COOKIE, "selected"),
        ])
        .expect("selection");
        let expected = format!("{SECURE_COOKIE}=selected");
        let seen = Rc::new(RefCell::new(Vec::new()));
        let validated = Rc::clone(&seen);
        let installed = Rc::clone(&seen);
        let browser_fingerprint = fingerprint('a');
        assert_eq!(
            validate_and_install(
                &selected,
                &browser_fingerprint,
                |candidate| {
                    validated
                        .borrow_mut()
                        .push(("validate", candidate.to_owned()));
                    Ok(browser_fingerprint.clone())
                },
                |candidate| {
                    installed
                        .borrow_mut()
                        .push(("install", candidate.to_owned()));
                    Ok(())
                },
            ),
            Ok(())
        );
        assert_eq!(
            *seen.borrow(),
            vec![("validate", expected.clone()), ("install", expected)]
        );
    }

    #[test]
    fn device_install_keeps_the_cookie_out_of_process_arguments_and_errors() {
        let cookie = format!("{SECURE_COOKIE}=stdin-only-material");
        let result = install_device_with("192.0.2.1", &cookie, |command, input| {
            let arguments = command
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(input, cookie.as_bytes());
            assert_eq!(arguments.last().map(String::as_str), Some(DEVICE_INSTALL_PROGRAM));
            assert!(arguments.iter().all(|argument| !argument.contains(&cookie)));
            assert!(DEVICE_INSTALL_PROGRAM.contains("trap rollback EXIT HUP INT TERM"));
            assert!(DEVICE_INSTALL_PROGRAM.contains("mv \"$backup\" \"$cookie\" || true"));
            assert!(DEVICE_INSTALL_PROGRAM.contains("mv \"$state_backup\" \"$managed\" || true"));
            Err(())
        });
        let error = result.map_err(|_| TARGET_INSTALLATION_FAILED.to_owned());
        assert_eq!(error, Err(TARGET_INSTALLATION_FAILED.to_owned()));
        assert!(!error.expect_err("fixed error").contains(&cookie));
    }

    #[test]
    fn simulator_install_replaces_cookie_and_removes_managed_state() {
        let directories = TestDirectories::new();
        ensure_private_directory(&directories.paths.secrets).expect("secrets");
        ensure_private_directory(&directories.paths.state).expect("state");
        let cookie_path = directories.paths.secrets.join(SESSION_SECRET);
        let state_path = directories.paths.state.join(MANAGED_STATE);
        fs::write(&cookie_path, "old-cookie").expect("old cookie");
        fs::write(&state_path, "old-managed-state").expect("old state");
        install_simulator_at("new-cookie", &directories.paths).expect("install");
        assert_eq!(fs::read_to_string(&cookie_path).expect("new cookie"), "new-cookie");
        assert!(!state_path.exists());
        assert_eq!(
            fs::metadata(&cookie_path)
                .expect("cookie metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        for directory in [&directories.paths.secrets, &directories.paths.state] {
            let names = fs::read_dir(directory)
                .expect("directory")
                .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert!(names.iter().all(|name| !name.contains(".login-") && !name.contains(".backup-")));
        }

        fs::write(&cookie_path, "rollback-cookie").expect("rollback cookie");
        fs::write(&state_path, "rollback-state").expect("rollback state");
        let failure = install_simulator_at_with(
            "replacement",
            &directories.paths,
            |point| {
                if point == InstallPoint::StateDetached {
                    Err(io::Error::other("injected failure"))
                } else {
                    Ok(())
                }
            },
        );
        assert!(failure.is_err());
        assert_eq!(
            fs::read_to_string(&cookie_path).expect("restored cookie"),
            "rollback-cookie"
        );
        assert_eq!(
            fs::read_to_string(&state_path).expect("restored state"),
            "rollback-state"
        );
    }

    #[derive(Clone)]
    struct FakeProcess {
        events: Rc<RefCell<Vec<&'static str>>>,
        stop_fails: bool,
    }

    impl BrowserProcess for FakeProcess {
        fn stop(&mut self) -> io::Result<()> {
            self.events.borrow_mut().push("stop");
            if self.stop_fails {
                Err(io::Error::other("injected stop failure"))
            } else {
                Ok(())
            }
        }

        fn wait_for_exit(&mut self) -> io::Result<()> {
            self.events.borrow_mut().push("wait");
            Ok(())
        }
    }

    struct FakeCleaner {
        events: Rc<RefCell<Vec<&'static str>>>,
    }

    impl ProfileCleaner for FakeCleaner {
        fn remove_profile(&mut self, _profile: &Path) -> io::Result<()> {
            self.events.borrow_mut().push("remove");
            Ok(())
        }
    }

    #[test]
    fn every_exit_path_stops_chrome_and_removes_the_profile() {
        let events = Rc::new(RefCell::new(Vec::new()));
        {
            let _guard = ChromeGuard::new(
                FakeProcess {
                    events: Rc::clone(&events),
                    stop_fails: false,
                },
                PathBuf::from("private-profile"),
                FakeCleaner {
                    events: Rc::clone(&events),
                },
            );
        }
        assert_eq!(*events.borrow(), ["stop", "wait", "remove"]);

        events.borrow_mut().clear();
        let guard = ChromeGuard::new(
            FakeProcess {
                events: Rc::clone(&events),
                stop_fails: true,
            },
            PathBuf::from("private-profile"),
            FakeCleaner {
                events: Rc::clone(&events),
            },
        );
        assert_eq!(guard.close(), Err(()));
        assert_eq!(*events.borrow(), ["stop", "wait", "remove"]);

        let root = create_private_profile_at(&std::env::temp_dir()).expect("private profile");
        assert_eq!(
            fs::metadata(&root)
                .expect("profile metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        fs::remove_dir_all(root).expect("remove test profile");
    }
}
