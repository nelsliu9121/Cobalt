use kobo_json::{ObjectBuilder, Value};
use kobo_sim::SimulatorAuthPaths;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
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

const CHROME_LAUNCH_PROGRAM: &str = r#"set -u
owner=$1
browser=$2
profile=$3
shift 3
browser_pid=
stop_browser() {
    if [ -n "$browser_pid" ] && kill -0 "$browser_pid" 2>/dev/null; then
        kill -TERM "$browser_pid" 2>/dev/null || true
        attempt=0
        while kill -0 "$browser_pid" 2>/dev/null && [ "$attempt" -lt 100 ]; do
            sleep 0.05
            attempt=$((attempt + 1))
        done
        if kill -0 "$browser_pid" 2>/dev/null; then
            kill -KILL "$browser_pid" 2>/dev/null || true
        fi
    fi
    if [ -n "$browser_pid" ]; then
        wait "$browser_pid" 2>/dev/null || true
    fi
}
cleanup() {
    trap '' HUP INT TERM
    stop_browser
    rm -rf -- "$profile"
    exit 0
}
trap cleanup HUP INT TERM
exec 3<&0
exec 4>&1
"$browser" "$@" &
browser_pid=$!
exec 0<&-
while kill -0 "$browser_pid" 2>/dev/null; do
    parent=$(ps -o ppid= -p $$ 2>/dev/null) || cleanup
    [ "$parent" -eq "$owner" ] || cleanup
    sleep 0.1
done
wait "$browser_pid"
status=$?
rm -rf -- "$profile"
exit "$status"
"#;

const DEVICE_INSTALL_PROGRAM: &str = r#"set -eu
umask 077
secrets=/mnt/onboard/.adds/cobalt/secrets
state=/mnt/onboard/.adds/cobalt/state
cookie="$secrets/bomtoon-session"
managed="$state/bomtoon-access-token.state"
temporary="$secrets/.bomtoon-session.login"
backup="$secrets/.bomtoon-session.backup"
state_backup="$state/.bomtoon-access-token.state.backup"
marker="$state/.bomtoon-login.transaction"
marker_new="$state/.bomtoon-login.transaction.new"
lock="$state/.bomtoon-access-token.lock"
had_cookie=0
had_state=0
committed=0
stage=
rank=0
durable() {
    sync
}
stage_rank() {
    case "$stage" in
        prepared) rank=0 ;;
        cookie-backed-up) rank=1 ;;
        cookie-installed) rank=2 ;;
        state-backed-up) rank=3 ;;
        committed) rank=4 ;;
        *) return 1 ;;
    esac
}
write_marker() {
    stage=$1
    printf 'cobalt-bomtoon-install-v1\n%s\n%s\n%s\n' \
        "$stage" "$had_cookie" "$had_state" > "$marker_new"
    chmod 600 "$marker_new"
    durable
    mv "$marker_new" "$marker"
    durable
}
read_marker() {
    {
        IFS= read -r version
        IFS= read -r stage
        IFS= read -r had_cookie
        IFS= read -r had_state
    } < "$marker"
    [ "$version" = cobalt-bomtoon-install-v1 ]
    case "$had_cookie:$had_state" in
        0:0|0:1|1:0|1:1) ;;
        *) return 1 ;;
    esac
    stage_rank
}
cleanup_committed() {
    rm -f "$temporary" "$backup" "$state_backup" "$marker_new"
    durable
    rm -f "$marker"
    durable
}
recover() {
    if [ ! -e "$marker" ]; then
        [ ! -e "$backup" ] && [ ! -e "$state_backup" ] || return 1
        rm -f "$temporary" "$marker_new"
        durable
        return 0
    fi
    read_marker
    if [ "$stage" = committed ]; then
        cleanup_committed
        return 0
    fi
    rm -f "$temporary" "$marker_new"
    durable
    if [ -e "$state_backup" ]; then
        rm -f "$managed"
        mv "$state_backup" "$managed"
        durable
    elif [ "$had_state" -eq 1 ] && [ "$rank" -ge 3 ]; then
        return 1
    fi
    if [ -e "$backup" ]; then
        rm -f "$cookie"
        mv "$backup" "$cookie"
        durable
    elif [ "$had_cookie" -eq 1 ] && [ "$rank" -ge 1 ]; then
        return 1
    elif [ "$had_cookie" -eq 0 ] && [ "$rank" -ge 1 ]; then
        rm -f "$cookie"
        durable
    fi
    rm -f "$marker"
    durable
}
rollback() {
    status=$?
    trap - EXIT HUP INT TERM
    set +e
    if [ "$committed" -ne 1 ]; then
        recover
    fi
    exit "$status"
}
mkdir -p "$secrets" "$state"
chmod 700 "$secrets" "$state"
command -v flock >/dev/null 2>&1 || exit 1
exec 9>"$lock"
chmod 600 "$lock"
flock -w 5 9 || exit 1
recover
set -C
: > "$temporary"
set +C
chmod 600 "$temporary"
cat > "$temporary"
durable
[ -e "$cookie" ] && had_cookie=1
[ -e "$managed" ] && had_state=1
write_marker prepared
trap rollback EXIT HUP INT TERM
if [ "$had_cookie" -eq 1 ]; then
    mv "$cookie" "$backup"
    durable
fi
write_marker cookie-backed-up
mv "$temporary" "$cookie"
durable
write_marker cookie-installed
if [ "$had_state" -eq 1 ]; then
    mv "$managed" "$state_backup"
    durable
fi
write_marker state-backed-up
write_marker committed
committed=1
cleanup_committed
trap - EXIT HUP INT TERM
exit 0
"#;

const BROWSER_SESSION_EXPRESSION: &str = r"(async () => {
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
})()";

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
        login(&target)
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
fn login(target: &LoginTarget) -> Result<(), String> {
    let browser = discover_chrome().ok_or_else(|| BROWSER_LAUNCH_FAILED.to_owned())?;
    let profile = create_private_profile_at(&std::env::temp_dir())
        .map_err(|_| BROWSER_LAUNCH_FAILED.to_owned())?;
    let Ok(child) = launch_chrome(&browser, &profile) else {
        let _ = fs::remove_dir_all(&profile);
        return Err(BROWSER_LAUNCH_FAILED.to_owned());
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
    let pipe = DevToolsPipe::new(output, input);
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    let browser_result = run_browser_login(pipe, deadline, || {
        let _ = guard.cleanup();
    });
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
        |selected| install_target(target, selected),
    )
}

fn chrome_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let system_chrome =
        PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    let system_chromium = PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium");
    let user_chrome =
        home.map(|path| path.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
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
                if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o700)) {
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
        .arg(std::process::id().to_string())
        .arg(browser)
        .arg(profile)
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
        let status = Command::new("/bin/kill")
            .args(["-TERM", &self.id().to_string()])
            .status()?;
        if status.success() || self.try_wait()?.is_some() {
            Ok(())
        } else {
            Err(io::Error::other("browser supervisor did not stop"))
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
    process_exited: bool,
    profile_removed: bool,
}

impl<C: BrowserProcess, F: ProfileCleaner> ChromeGuard<C, F> {
    fn new(child: C, profile: PathBuf, cleaner: F) -> Self {
        Self {
            child,
            profile,
            cleaner,
            process_exited: false,
            profile_removed: false,
        }
    }

    fn child_mut(&mut self) -> &mut C {
        &mut self.child
    }

    fn cleanup(&mut self) -> Result<(), ()> {
        if !self.process_exited {
            self.child.stop().map_err(|_| ())?;
            self.child.wait_for_exit().map_err(|_| ())?;
            self.process_exited = true;
        }
        if !self.profile_removed {
            self.cleaner.remove_profile(&self.profile).map_err(|_| ())?;
            self.profile_removed = true;
        }
        Ok(())
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
    Timeout,
}

struct DevToolsPipe<R: Read, W: Write> {
    reader: R,
    writer: W,
    next_id: u32,
    buffered: Vec<u8>,
    failed: Option<CdpError>,
}

impl<R: Read, W: Write> DevToolsPipe<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
            buffered: Vec::new(),
            failed: None,
        }
    }

    fn call(
        &mut self,
        deadline: Instant,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, CdpError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.call_inner(deadline, method, params, session_id);
        if let Err(error) = &result {
            self.buffered.clear();
            self.failed = Some(*error);
        }
        result
    }

    fn call_inner(
        &mut self,
        deadline: Instant,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, CdpError> {
        ensure_cdp_deadline(deadline)?;
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
        ensure_cdp_deadline(deadline)?;

        loop {
            let frame = self.read_frame(deadline)?;
            ensure_cdp_deadline(deadline)?;
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
            return response.get("result").cloned().ok_or(CdpError::Protocol);
        }
    }

    fn read_frame(&mut self, deadline: Instant) -> Result<Vec<u8>, CdpError> {
        loop {
            ensure_cdp_deadline(deadline)?;
            if let Some(end) = self.buffered.iter().position(|byte| *byte == 0) {
                if end > MAX_CDP_FRAME_BYTES {
                    return Err(CdpError::Framing);
                }
                let mut frame = self.buffered.drain(..=end).collect::<Vec<_>>();
                frame.pop();
                if frame.is_empty() {
                    return Err(CdpError::Framing);
                }
                return Ok(frame);
            }
            if self.buffered.len() > MAX_CDP_FRAME_BYTES {
                return Err(CdpError::Framing);
            }
            let mut chunk = [0_u8; 8192];
            let remaining = MAX_CDP_FRAME_BYTES.saturating_sub(self.buffered.len());
            let limit = (remaining + 1).min(chunk.len());
            let read = self
                .reader
                .read(&mut chunk[..limit])
                .map_err(|_| CdpError::Transport)?;
            ensure_cdp_deadline(deadline)?;
            if read == 0 {
                return Err(CdpError::Transport);
            }
            self.buffered.extend_from_slice(&chunk[..read]);
        }
    }
}

fn ensure_cdp_deadline(deadline: Instant) -> Result<(), CdpError> {
    (Instant::now() < deadline)
        .then_some(())
        .ok_or(CdpError::Timeout)
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

fn run_browser_login<R, W>(
    pipe: DevToolsPipe<R, W>,
    deadline: Instant,
    mut cancel: impl FnMut(),
) -> Result<(String, String), BrowserFlowError>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    let _worker = thread::spawn(move || {
        let mut pipe = pipe;
        let result = browser_login(&mut pipe, deadline);
        let _ = sender.send(result);
    });
    let remaining = deadline.saturating_duration_since(Instant::now());
    match receiver.recv_timeout(remaining) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            cancel();
            Err(BrowserFlowError::Timeout)
        }
        Err(RecvTimeoutError::Disconnected) => Err(BrowserFlowError::Launch),
    }
}

fn browser_login<R: Read, W: Write>(
    pipe: &mut DevToolsPipe<R, W>,
    deadline: Instant,
) -> Result<(String, String), BrowserFlowError> {
    browser_login_with(pipe, deadline, thread::sleep)
}

fn browser_login_with<R: Read, W: Write>(
    pipe: &mut DevToolsPipe<R, W>,
    deadline: Instant,
    mut pause: impl FnMut(Duration),
) -> Result<(String, String), BrowserFlowError> {
    let target_id = bomtoon_target(pipe, deadline, &mut pause)?;
    let attached = pipe
        .call(
            deadline,
            "Target.attachToTarget",
            ObjectBuilder::new()
                .set("targetId", target_id)
                .set("flatten", true)
                .build(),
            None,
        )
        .map_err(|error| browser_cdp_error(error, BrowserFlowError::Launch))?;
    let session_id = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or(BrowserFlowError::Launch)?
        .to_owned();
    let browser_fingerprint = loop {
        match page_authentication(pipe, &session_id, deadline) {
            Ok(Some(fingerprint)) => break fingerprint,
            Ok(None) => retry_pause(deadline, &mut pause)?,
            Err(error) => {
                return Err(browser_cdp_error(error, BrowserFlowError::Launch));
            }
        }
    };
    let cookies = pipe
        .call(
            deadline,
            "Network.getCookies",
            ObjectBuilder::new().set("urls", vec![COOKIE_URL]).build(),
            Some(&session_id),
        )
        .map_err(|error| browser_cdp_error(error, BrowserFlowError::CookieSelection))?;
    let cookies =
        parse_network_cookies(&cookies).map_err(|()| BrowserFlowError::CookieSelection)?;
    let selected =
        select_session_cookie(&cookies).map_err(|()| BrowserFlowError::CookieSelection)?;
    Ok((selected, browser_fingerprint))
}

fn browser_cdp_error(error: CdpError, stage: BrowserFlowError) -> BrowserFlowError {
    if error == CdpError::Timeout {
        BrowserFlowError::Timeout
    } else {
        stage
    }
}

fn retry_pause(
    deadline: Instant,
    pause: &mut impl FnMut(Duration),
) -> Result<(), BrowserFlowError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or(BrowserFlowError::Timeout)?;
    pause(LOGIN_POLL_INTERVAL.min(remaining));
    (Instant::now() < deadline)
        .then_some(())
        .ok_or(BrowserFlowError::Timeout)
}

fn bomtoon_target<R: Read, W: Write>(
    pipe: &mut DevToolsPipe<R, W>,
    deadline: Instant,
    pause: &mut impl FnMut(Duration),
) -> Result<String, BrowserFlowError> {
    loop {
        let targets = pipe
            .call(deadline, "Target.getTargets", empty_object(), None)
            .map_err(|error| browser_cdp_error(error, BrowserFlowError::Launch))?;
        let infos = targets
            .get("targetInfos")
            .and_then(Value::as_array)
            .ok_or(BrowserFlowError::Launch)?;
        for target in infos {
            let kind = target
                .get("type")
                .and_then(Value::as_str)
                .ok_or(BrowserFlowError::Launch)?;
            let url = target
                .get("url")
                .and_then(Value::as_str)
                .ok_or(BrowserFlowError::Launch)?;
            let target_id = target
                .get("targetId")
                .and_then(Value::as_str)
                .ok_or(BrowserFlowError::Launch)?;
            if kind == "page" && bomtoon_page_url(url) {
                return Ok(target_id.to_owned());
            }
        }
        retry_pause(deadline, pause)?;
    }
}

fn bomtoon_page_url(url: &str) -> bool {
    url == "https://www.bomtoon.tw" || url.starts_with("https://www.bomtoon.tw/")
}

fn page_authentication<R: Read, W: Write>(
    pipe: &mut DevToolsPipe<R, W>,
    session_id: &str,
    deadline: Instant,
) -> Result<Option<String>, CdpError> {
    let evaluated = pipe.call(
        deadline,
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
                name: cookie
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(())?
                    .to_owned(),
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
                path: cookie
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or(())?
                    .to_owned(),
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
    let Some(suffix) = name
        .strip_prefix(family)
        .and_then(|name| name.strip_prefix('.'))
    else {
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
    .map_err(|()| TARGET_INSTALLATION_FAILED.to_owned())
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

trait DeviceInstallOperation {
    fn poll_writer(&mut self) -> Option<Result<(), ()>>;
    fn poll_child(&mut self) -> Result<Option<bool>, ()>;
    fn terminate(&mut self);
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn pause(&mut self, duration: Duration);
}

struct RunningDeviceInstall<'a> {
    child: &'a mut Child,
    writer: Receiver<Result<(), ()>>,
}

impl DeviceInstallOperation for RunningDeviceInstall<'_> {
    fn poll_writer(&mut self) -> Option<Result<(), ()>> {
        match self.writer.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(())),
        }
    }

    fn poll_child(&mut self) -> Result<Option<bool>, ()> {
        self.child
            .try_wait()
            .map(|status| status.map(|status| status.success()))
            .map_err(|_| ())
    }

    fn terminate(&mut self) {
        super::terminate_remote_child(self.child);
    }

    fn pause(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

fn wait_for_device_install(
    operation: &mut impl DeviceInstallOperation,
    deadline: Instant,
) -> Result<(), ()> {
    let mut writer_finished = false;
    let mut child_succeeded = None;
    loop {
        if operation.now() >= deadline {
            operation.terminate();
            return Err(());
        }
        if !writer_finished {
            match operation.poll_writer() {
                Some(Ok(())) => writer_finished = true,
                Some(Err(())) => {
                    operation.terminate();
                    return Err(());
                }
                None => {}
            }
        }
        if child_succeeded.is_none() {
            match operation.poll_child() {
                Ok(Some(true)) => child_succeeded = Some(true),
                Ok(Some(false)) | Err(()) => {
                    operation.terminate();
                    return Err(());
                }
                Ok(None) => {}
            }
        }
        if writer_finished && child_succeeded == Some(true) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(operation.now());
        operation.pause(Duration::from_millis(25).min(remaining));
    }
}

fn run_device_install(mut command: Command, cookie: &[u8]) -> Result<(), ()> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let deadline = Instant::now() + DEVICE_INSTALL_TIMEOUT;
    let mut child = command.spawn().map_err(|_| ())?;
    let Some(mut input) = child.stdin.take() else {
        super::terminate_remote_child(&mut child);
        return Err(());
    };
    let mut secret = cookie.to_vec();
    let (sender, receiver) = mpsc::sync_channel(1);
    let _writer = thread::spawn(move || {
        let result = input.write_all(&secret).map_err(|_| ());
        drop(input);
        secret.fill(0);
        let _ = sender.send(result);
    });
    wait_for_device_install(
        &mut RunningDeviceInstall {
            child: &mut child,
            writer: receiver,
        },
        deadline,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallPoint {
    CookieInstalled,
    StateDetached,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum InstallStage {
    Prepared,
    CookieBackedUp,
    CookieInstalled,
    StateBackedUp,
    Committed,
}

impl InstallStage {
    fn encoded(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::CookieBackedUp => "cookie-backed-up",
            Self::CookieInstalled => "cookie-installed",
            Self::StateBackedUp => "state-backed-up",
            Self::Committed => "committed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "cookie-backed-up" => Some(Self::CookieBackedUp),
            "cookie-installed" => Some(Self::CookieInstalled),
            "state-backed-up" => Some(Self::StateBackedUp),
            "committed" => Some(Self::Committed),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InstallRecord {
    stage: InstallStage,
    had_cookie: bool,
    had_state: bool,
}

impl InstallRecord {
    fn encode(self) -> String {
        format!(
            "cobalt-bomtoon-install-v1\n{}\n{}\n{}",
            self.stage.encoded(),
            u8::from(self.had_cookie),
            u8::from(self.had_state)
        )
    }

    fn parse(value: &str) -> Option<Self> {
        let mut lines = value.split('\n');
        if lines.next()? != "cobalt-bomtoon-install-v1" {
            return None;
        }
        let stage = InstallStage::parse(lines.next()?)?;
        let had_cookie = match lines.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let had_state = match lines.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let trailing = lines.next();
        if !matches!(trailing, None | Some("")) || lines.next().is_some() {
            return None;
        }
        Some(Self {
            stage,
            had_cookie,
            had_state,
        })
    }
}

struct SimulatorTransaction {
    cookie: PathBuf,
    cookie_backup: PathBuf,
    temporary: PathBuf,
    state: PathBuf,
    state_backup: PathBuf,
    marker: PathBuf,
    marker_temporary: PathBuf,
}

impl SimulatorTransaction {
    fn new(paths: &SimulatorAuthPaths) -> Self {
        Self {
            cookie: paths.secrets.join(SESSION_SECRET),
            cookie_backup: paths.secrets.join(".bomtoon-session.backup"),
            temporary: paths.secrets.join(".bomtoon-session.login"),
            state: paths.state.join(MANAGED_STATE),
            state_backup: paths.state.join(".bomtoon-access-token.state.backup"),
            marker: paths.state.join(".bomtoon-login.transaction"),
            marker_temporary: paths.state.join(".bomtoon-login.transaction.new"),
        }
    }

    fn write_record(&self, record: InstallRecord) -> io::Result<()> {
        remove_file_synced(&self.marker_temporary)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&self.marker_temporary)?;
        file.write_all(record.encode().as_bytes())?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.sync_all()?;
        drop(file);
        rename_synced(&self.marker_temporary, &self.marker)
    }

    fn read_record(&self) -> io::Result<Option<InstallRecord>> {
        let file = match File::open(&self.marker) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut encoded = String::new();
        file.take(257).read_to_string(&mut encoded)?;
        if encoded.len() > 256 {
            return Err(io::Error::other("invalid simulator transaction"));
        }
        InstallRecord::parse(&encoded)
            .ok_or_else(|| io::Error::other("invalid simulator transaction"))
            .map(Some)
    }

    fn recover(&self) -> io::Result<()> {
        let Some(record) = self.read_record()? else {
            if path_exists(&self.cookie_backup)? || path_exists(&self.state_backup)? {
                return Err(io::Error::other("orphaned simulator transaction"));
            }
            remove_file_synced(&self.temporary)?;
            remove_file_synced(&self.marker_temporary)?;
            return Ok(());
        };
        if record.stage == InstallStage::Committed {
            return self.cleanup_committed();
        }

        remove_file_synced(&self.marker_temporary)?;
        remove_file_synced(&self.temporary)?;
        if path_exists(&self.state_backup)? {
            remove_file_synced(&self.state)?;
            rename_synced(&self.state_backup, &self.state)?;
        } else if record.had_state && record.stage >= InstallStage::StateBackedUp {
            return Err(io::Error::other("missing simulator state backup"));
        }

        if path_exists(&self.cookie_backup)? {
            remove_file_synced(&self.cookie)?;
            rename_synced(&self.cookie_backup, &self.cookie)?;
        } else if record.had_cookie && record.stage >= InstallStage::CookieBackedUp {
            return Err(io::Error::other("missing simulator cookie backup"));
        } else if !record.had_cookie && record.stage >= InstallStage::CookieBackedUp {
            remove_file_synced(&self.cookie)?;
        }
        remove_file_synced(&self.marker)
    }

    fn cleanup_committed(&self) -> io::Result<()> {
        remove_file_synced(&self.temporary)?;
        remove_file_synced(&self.cookie_backup)?;
        remove_file_synced(&self.state_backup)?;
        remove_file_synced(&self.marker_temporary)?;
        remove_file_synced(&self.marker)
    }
}

fn path_exists(path: &Path) -> io::Result<bool> {
    path.try_exists()
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn sync_parent(path: &Path) -> io::Result<()> {
    sync_directory(
        path.parent()
            .ok_or_else(|| io::Error::other("path has no parent"))?,
    )
}

fn remove_file_synced(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn rename_synced(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)?;
    sync_parent(source)?;
    if source.parent() != destination.parent() {
        sync_parent(destination)?;
    }
    Ok(())
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
    let _lease = kobo_sim::acquire_simulator_auth_lease(paths)
        .map_err(|_| io::Error::other("simulator credential lease unavailable"))?;
    let transaction = SimulatorTransaction::new(paths);
    transaction.recover()?;
    let mut record = InstallRecord {
        stage: InstallStage::Prepared,
        had_cookie: path_exists(&transaction.cookie)?,
        had_state: path_exists(&transaction.state)?,
    };
    let operation = (|| -> io::Result<()> {
        transaction.write_record(record)?;
        let mut temporary = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&transaction.temporary)?;
        temporary.write_all(cookie.as_bytes())?;
        temporary.set_permissions(fs::Permissions::from_mode(0o600))?;
        temporary.sync_all()?;
        drop(temporary);

        if record.had_cookie {
            rename_synced(&transaction.cookie, &transaction.cookie_backup)?;
        }
        record.stage = InstallStage::CookieBackedUp;
        transaction.write_record(record)?;

        rename_synced(&transaction.temporary, &transaction.cookie)?;
        record.stage = InstallStage::CookieInstalled;
        transaction.write_record(record)?;
        checkpoint(InstallPoint::CookieInstalled)?;

        if record.had_state {
            rename_synced(&transaction.state, &transaction.state_backup)?;
        }
        record.stage = InstallStage::StateBackedUp;
        transaction.write_record(record)?;
        checkpoint(InstallPoint::StateDetached)?;

        record.stage = InstallStage::Committed;
        transaction.write_record(record)
    })();
    if operation.is_err() {
        return match transaction.recover() {
            Ok(()) => Err(io::Error::other("simulator installation failed")),
            Err(_) => Err(io::Error::other("simulator rollback failed")),
        };
    }
    transaction
        .cleanup_committed()
        .map_err(|_| io::Error::other("simulator commit cleanup failed"))
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
    use std::cell::{Cell, RefCell};
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
            let state = root.join("state");
            let paths = SimulatorAuthPaths {
                secrets: root.join("secrets"),
                lock: state.join(".bomtoon-access-token.lock"),
                state,
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
                PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
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
        let boundary = "x".repeat(kobo_net::bomtoon::SESSION_COOKIE_MAX_BYTES - prefix_bytes);
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
        assert!(
            select_session_cookie(&[cookie(&format!("{SECURE_COOKIE}.01"), "ambiguous")]).is_err()
        );
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

    fn push_cdp_frame(buffer: &mut Vec<u8>, frame: &str) {
        buffer.extend_from_slice(frame.as_bytes());
        buffer.push(0);
    }

    struct BlockingReader {
        release: Receiver<()>,
    }

    impl Read for BlockingReader {
        fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
            let _ = self.release.recv();
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected blocked pipe release",
            ))
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
            .call(
                Instant::now() + Duration::from_secs(1),
                "Runtime.evaluate",
                empty_object(),
                Some("flat-session"),
            )
            .expect("matching result");
        assert_eq!(result.get("accepted").and_then(Value::as_bool), Some(true));
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
            pipe.call(
                Instant::now() + Duration::from_secs(1),
                "Network.getCookies",
                empty_object(),
                None,
            ),
            Err(CdpError::Protocol)
        );
        assert!(!format!("{:?}", CdpError::Protocol).contains("credential-material"));
    }

    #[test]
    fn browser_deadline_cancels_blocked_cdp_io() {
        let (release, blocked) = mpsc::sync_channel(1);
        let cancelled = Rc::new(Cell::new(false));
        let observed = Rc::clone(&cancelled);
        let result = run_browser_login(
            DevToolsPipe::new(BlockingReader { release: blocked }, Vec::new()),
            Instant::now() + Duration::from_millis(10),
            || {
                observed.set(true);
                let _ = release.send(());
            },
        );
        assert_eq!(result, Err(BrowserFlowError::Timeout));
        assert!(cancelled.get());
    }

    #[test]
    fn target_and_authentication_poll_only_explicit_incomplete_results() {
        let expected_fingerprint = fingerprint('a');
        let frames = vec![
            r#"{"id":1,"result":{"targetInfos":[{"type":"page","url":"about:blank","targetId":"transient"}]}}"#.to_owned(),
            r#"{"id":2,"result":{"targetInfos":[{"type":"page","url":"https://www.bomtoon.tw/user/login","targetId":"bomtoon"}]}}"#.to_owned(),
            r#"{"id":3,"result":{"sessionId":"flat-session"}}"#.to_owned(),
            r#"{"id":4,"result":{"result":{"value":{"authenticated":false}}}}"#.to_owned(),
            format!(
                r#"{{"id":5,"result":{{"result":{{"value":{{"authenticated":true,"tokenFingerprint":"{expected_fingerprint}"}}}}}}}}"#
            ),
            format!(
                r#"{{"id":6,"result":{{"cookies":[{{"name":"{SECURE_COOKIE}","value":"selected","domain":".bomtoon.tw","path":"/"}}]}}}}"#
            ),
        ];
        let mut incoming = Vec::new();
        for frame in frames {
            push_cdp_frame(&mut incoming, &frame);
        }
        let pauses = Cell::new(0);
        let mut pipe = DevToolsPipe::new(Cursor::new(incoming), Vec::new());
        let result = browser_login_with(&mut pipe, Instant::now() + Duration::from_secs(1), |_| {
            pauses.set(pauses.get() + 1);
        });
        assert_eq!(
            result,
            Ok((format!("{SECURE_COOKIE}=selected"), expected_fingerprint))
        );
        assert_eq!(pauses.get(), 2);
    }

    #[test]
    fn fatal_cdp_errors_abort_and_oversized_frames_poison_the_pipe() {
        let mut incoming = Vec::new();
        for frame in [
            r#"{"id":1,"result":{"targetInfos":[{"type":"page","url":"https://www.bomtoon.tw/user/login","targetId":"bomtoon"}]}}"#,
            r#"{"id":2,"result":{"sessionId":"flat-session"}}"#,
            r#"{"id":3,"error":{"message":"sensitive response text"}}"#,
        ] {
            push_cdp_frame(&mut incoming, frame);
        }
        let mut pipe = DevToolsPipe::new(Cursor::new(incoming), Vec::new());
        assert_eq!(
            browser_login_with(
                &mut pipe,
                Instant::now() + Duration::from_secs(1),
                |_| panic!("fatal protocol errors must not poll"),
            ),
            Err(BrowserFlowError::Launch)
        );
        assert_eq!(pipe.writer.split(|byte| *byte == 0).count() - 1, 3);

        let oversized = vec![b'x'; MAX_CDP_FRAME_BYTES + 1];
        let mut pipe = DevToolsPipe::new(Cursor::new(oversized), Vec::new());
        assert_eq!(
            pipe.call(
                Instant::now() + Duration::from_secs(1),
                "Target.getTargets",
                empty_object(),
                None,
            ),
            Err(CdpError::Framing)
        );
        assert!(pipe.buffered.is_empty());
        let written = pipe.writer.len();
        assert_eq!(
            pipe.call(
                Instant::now() + Duration::from_secs(1),
                "Target.getTargets",
                empty_object(),
                None,
            ),
            Err(CdpError::Framing)
        );
        assert_eq!(pipe.writer.len(), written);
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
            assert_eq!(
                arguments.last().map(String::as_str),
                Some(DEVICE_INSTALL_PROGRAM)
            );
            assert!(arguments.iter().all(|argument| !argument.contains(&cookie)));
            assert!(DEVICE_INSTALL_PROGRAM.contains("flock -w 5 9 || exit 1"));
            assert!(DEVICE_INSTALL_PROGRAM.contains("read_marker"));
            assert!(DEVICE_INSTALL_PROGRAM.contains("recover"));
            assert!(DEVICE_INSTALL_PROGRAM.contains("marker=\"$state/.bomtoon-login.transaction\""));
            assert!(DEVICE_INSTALL_PROGRAM.contains("backup=\"$secrets/.bomtoon-session.backup\""));
            assert!(DEVICE_INSTALL_PROGRAM
                .contains("state_backup=\"$state/.bomtoon-access-token.state.backup\""));
            assert!(!DEVICE_INSTALL_PROGRAM.contains("backup.$$"));
            let recovered = DEVICE_INSTALL_PROGRAM
                .find("\nrecover\nset -C")
                .expect("startup recovery");
            let mutation = DEVICE_INSTALL_PROGRAM
                .find("mv \"$cookie\" \"$backup\"")
                .expect("cookie backup");
            let committed = DEVICE_INSTALL_PROGRAM
                .rfind("write_marker committed")
                .expect("durable commit marker");
            let cleanup = DEVICE_INSTALL_PROGRAM
                .rfind("cleanup_committed")
                .expect("commit cleanup");
            assert!(recovered < mutation && mutation < committed && committed < cleanup);
            Err(())
        });
        let error = result.map_err(|()| TARGET_INSTALLATION_FAILED.to_owned());
        assert_eq!(error, Err(TARGET_INSTALLATION_FAILED.to_owned()));
        assert!(!error.expect_err("fixed error").contains(&cookie));
    }

    struct StalledDeviceOperation {
        writer_polls: usize,
        child_polls: usize,
        terminated: bool,
        now: Instant,
    }

    impl DeviceInstallOperation for StalledDeviceOperation {
        fn poll_writer(&mut self) -> Option<Result<(), ()>> {
            self.writer_polls += 1;
            None
        }

        fn poll_child(&mut self) -> Result<Option<bool>, ()> {
            self.child_polls += 1;
            Ok(None)
        }

        fn terminate(&mut self) {
            self.terminated = true;
        }
        fn now(&mut self) -> Instant {
            self.now
        }

        fn pause(&mut self, duration: Duration) {
            self.now += duration;
        }
    }

    #[test]
    fn device_timeout_covers_stalled_stdin_and_child_wait() {
        let start = Instant::now();
        let mut operation = StalledDeviceOperation {
            writer_polls: 0,
            child_polls: 0,
            terminated: false,
            now: start,
        };
        assert_eq!(
            wait_for_device_install(&mut operation, start + Duration::from_millis(2)),
            Err(())
        );
        assert!(operation.writer_polls > 0);
        assert!(operation.child_polls > 0);
        assert!(operation.terminated);
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
        assert_eq!(
            fs::read_to_string(&cookie_path).expect("new cookie"),
            "new-cookie"
        );
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
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>();
            assert!(names.iter().all(|name| {
                !matches!(
                    name.as_str(),
                    ".bomtoon-session.login"
                        | ".bomtoon-session.backup"
                        | ".bomtoon-access-token.state.backup"
                        | ".bomtoon-login.transaction"
                        | ".bomtoon-login.transaction.new"
                )
            }));
        }

        fs::write(&cookie_path, "rollback-cookie").expect("rollback cookie");
        fs::write(&state_path, "rollback-state").expect("rollback state");
        let failure = install_simulator_at_with("replacement", &directories.paths, |point| {
            if point == InstallPoint::StateDetached {
                Err(io::Error::other("injected failure"))
            } else {
                Ok(())
            }
        });
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

    #[test]
    fn simulator_startup_recovers_fixed_backups_after_an_interrupted_install() {
        let directories = TestDirectories::new();
        ensure_private_directory(&directories.paths.secrets).expect("secrets");
        ensure_private_directory(&directories.paths.state).expect("state");
        let transaction = SimulatorTransaction::new(&directories.paths);
        fs::write(&transaction.cookie, "prior-cookie").expect("old cookie");
        fs::write(&transaction.state, "prior-state").expect("old state");
        fs::write(&transaction.temporary, "replacement-cookie").expect("replacement");
        transaction
            .write_record(InstallRecord {
                stage: InstallStage::Prepared,
                had_cookie: true,
                had_state: true,
            })
            .expect("prepared marker");
        rename_synced(&transaction.cookie, &transaction.cookie_backup)
            .expect("durable cookie backup");
        transaction
            .write_record(InstallRecord {
                stage: InstallStage::CookieBackedUp,
                had_cookie: true,
                had_state: true,
            })
            .expect("cookie backup marker");
        rename_synced(&transaction.temporary, &transaction.cookie).expect("durable replacement");
        rename_synced(&transaction.state, &transaction.state_backup)
            .expect("crash-window state backup");

        SimulatorTransaction::new(&directories.paths)
            .recover()
            .expect("startup recovery");
        assert_eq!(
            fs::read_to_string(&transaction.cookie).expect("restored cookie"),
            "prior-cookie"
        );
        assert_eq!(
            fs::read_to_string(&transaction.state).expect("restored state"),
            "prior-state"
        );
        assert!(!transaction.marker.exists());
        assert!(!transaction.cookie_backup.exists());
        assert!(!transaction.state_backup.exists());
    }

    #[test]
    fn simulator_rollback_failure_preserves_recovery_backup_and_is_explicit() {
        let directories = TestDirectories::new();
        ensure_private_directory(&directories.paths.secrets).expect("secrets");
        ensure_private_directory(&directories.paths.state).expect("state");
        let cookie_path = directories.paths.secrets.join(SESSION_SECRET);
        let state_path = directories.paths.state.join(MANAGED_STATE);
        fs::write(&cookie_path, "recoverable-old-cookie").expect("old cookie");
        fs::write(&state_path, "recoverable-old-state").expect("old state");

        let error = install_simulator_at_with("replacement-cookie", &directories.paths, |point| {
            if point == InstallPoint::StateDetached {
                fs::remove_file(&cookie_path)?;
                fs::create_dir(&cookie_path)?;
                Err(io::Error::other("injected rollback obstruction"))
            } else {
                Ok(())
            }
        })
        .expect_err("rollback must report obstruction");
        assert_eq!(error.to_string(), "simulator rollback failed");
        assert!(!error.to_string().contains("replacement-cookie"));
        assert_eq!(
            fs::read_to_string(&state_path).expect("state restored"),
            "recoverable-old-state"
        );
        let cookie_backup = directories.paths.secrets.join(".bomtoon-session.backup");
        assert_eq!(
            fs::read_to_string(cookie_backup).expect("backup contents"),
            "recoverable-old-cookie"
        );
        assert!(directories
            .paths
            .state
            .join(".bomtoon-login.transaction")
            .exists());
    }

    #[derive(Clone)]
    struct FakeProcess {
        events: Rc<RefCell<Vec<&'static str>>>,
        wait_failures: usize,
        stop_failures: usize,
    }

    impl BrowserProcess for FakeProcess {
        fn stop(&mut self) -> io::Result<()> {
            self.events.borrow_mut().push("stop");
            if self.stop_failures > 0 {
                self.stop_failures -= 1;
                Err(io::Error::other("injected stop failure"))
            } else {
                Ok(())
            }
        }

        fn wait_for_exit(&mut self) -> io::Result<()> {
            self.events.borrow_mut().push("wait");
            if self.wait_failures > 0 {
                self.wait_failures -= 1;
                Err(io::Error::other("injected wait failure"))
            } else {
                Ok(())
            }
        }
    }

    struct FakeCleaner {
        events: Rc<RefCell<Vec<&'static str>>>,
        failures: usize,
    }

    impl ProfileCleaner for FakeCleaner {
        fn remove_profile(&mut self, _profile: &Path) -> io::Result<()> {
            self.events.borrow_mut().push("remove");
            if self.failures > 0 {
                self.failures -= 1;
                Err(io::Error::other("injected removal failure"))
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn chrome_supervisor_stops_waits_and_removes_profile_after_owner_exit() {
        let profile = create_private_profile_at(&std::env::temp_dir()).expect("supervised profile");
        let mut supervisor = Command::new("/bin/sh")
            .args(["-c", CHROME_LAUNCH_PROGRAM, "test-supervisor"])
            .arg("0")
            .arg("/bin/sh")
            .arg(&profile)
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn supervisor");
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Some(status) = supervisor.try_wait().expect("poll supervisor") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = supervisor.kill();
                let _ = supervisor.wait();
                panic!("supervisor did not react to owner exit");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success());
        assert!(!profile.exists());
    }

    #[test]
    fn every_exit_path_stops_chrome_and_removes_the_profile() {
        let events = Rc::new(RefCell::new(Vec::new()));
        {
            let _guard = ChromeGuard::new(
                FakeProcess {
                    events: Rc::clone(&events),
                    wait_failures: 0,
                    stop_failures: 0,
                },
                PathBuf::from("private-profile"),
                FakeCleaner {
                    events: Rc::clone(&events),
                    failures: 0,
                },
            );
        }
        assert_eq!(*events.borrow(), ["stop", "wait", "remove"]);

        events.borrow_mut().clear();
        let guard = ChromeGuard::new(
            FakeProcess {
                events: Rc::clone(&events),
                wait_failures: 1,
                stop_failures: 0,
            },
            PathBuf::from("private-profile"),
            FakeCleaner {
                events: Rc::clone(&events),
                failures: 0,
            },
        );
        assert_eq!(guard.close(), Err(()));
        assert_eq!(*events.borrow(), ["stop", "wait", "stop", "wait", "remove"]);

        events.borrow_mut().clear();
        let guard = ChromeGuard::new(
            FakeProcess {
                events: Rc::clone(&events),
                wait_failures: 0,
                stop_failures: 1,
            },
            PathBuf::from("private-profile"),
            FakeCleaner {
                events: Rc::clone(&events),
                failures: 0,
            },
        );
        assert_eq!(guard.close(), Err(()));
        assert_eq!(*events.borrow(), ["stop", "stop", "wait", "remove"]);

        events.borrow_mut().clear();
        let guard = ChromeGuard::new(
            FakeProcess {
                events: Rc::clone(&events),
                wait_failures: 0,
                stop_failures: 0,
            },
            PathBuf::from("private-profile"),
            FakeCleaner {
                events: Rc::clone(&events),
                failures: 1,
            },
        );
        assert_eq!(guard.close(), Err(()));
        assert_eq!(*events.borrow(), ["stop", "wait", "remove", "remove"]);

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
