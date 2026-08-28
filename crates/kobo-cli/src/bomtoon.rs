use kobo_sim::SimulatorAuthPaths;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::bomtoon_handoff::{
    wait_for_payload, Challenge, HandoffError, HandoffPayload, HostLock, PendingHandoff,
};

const USAGE: &str = "usage: kobo bomtoon (login (--device IP | --sim) | extension install)";
const LOGIN_URL: &str = "https://www.bomtoon.tw/user/login";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEVICE_INSTALL_TIMEOUT: Duration = Duration::from_secs(30);
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
const EXTENSION_FILES: &[(&str, &[u8])] = &[
    (
        "manifest.json",
        include_bytes!("../bomtoon-extension/manifest.json"),
    ),
    (
        "protocol.js",
        include_bytes!("../bomtoon-extension/protocol.js"),
    ),
    (
        "background.js",
        include_bytes!("../bomtoon-extension/background.js"),
    ),
    (
        "content.js",
        include_bytes!("../bomtoon-extension/content.js"),
    ),
    (
        "popup.html",
        include_bytes!("../bomtoon-extension/popup.html"),
    ),
    ("popup.js", include_bytes!("../bomtoon-extension/popup.js")),
];

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

static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
enum LoginTarget {
    Device(String),
    Simulator,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Action {
    Login(LoginTarget),
    InstallExtension,
}

pub fn command(arguments: &[String]) -> Result<(), String> {
    let action = parse_action(arguments)?;
    if !cfg!(target_os = "macos") {
        return Err(UNSUPPORTED_HOST.to_owned());
    }

    #[cfg(target_os = "macos")]
    {
        match action {
            Action::Login(target) => login(&target),
            Action::InstallExtension => install_extension(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = action;
        Err(UNSUPPORTED_HOST.to_owned())
    }
}

fn parse_action(arguments: &[String]) -> Result<Action, String> {
    match arguments {
        [verb, subcommand] if verb == "extension" && subcommand == "install" => {
            Ok(Action::InstallExtension)
        }
        [verb, flag, host]
            if verb == "login" && flag == "--device" && super::valid_device_host(host) =>
        {
            Ok(Action::Login(LoginTarget::Device(host.clone())))
        }
        [verb, flag] if verb == "login" && flag == "--sim" => {
            Ok(Action::Login(LoginTarget::Simulator))
        }
        _ => Err(USAGE.to_owned()),
    }
}

fn install_extension_with<Lock>(
    home: &Path,
    acquire_lock: impl FnOnce() -> io::Result<Lock>,
    materialize: impl FnOnce(&Path) -> io::Result<PathBuf>,
) -> Result<(PathBuf, bool), String> {
    let _host_lock = acquire_lock().map_err(|_| "extension installation failed".to_owned())?;
    let destination = extension_directory(home);
    let replacing = destination
        .try_exists()
        .map_err(|_| "extension installation failed".to_owned())?;
    let cobalt_root = destination
        .parent()
        .ok_or_else(|| "extension installation failed".to_owned())?;
    let installed =
        materialize(cobalt_root).map_err(|_| "extension installation failed".to_owned())?;
    Ok((installed, replacing))
}

#[cfg(target_os = "macos")]
fn install_extension() -> Result<(), String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_owned())?;
    let (installed, replacing) =
        install_extension_with(&home, HostLock::acquire, materialize_extension_at)?;

    println!("{}", installed.display());
    println!("1. Open chrome://extensions.");
    println!("2. Enable Developer mode.");
    println!("3. Choose Load unpacked.");
    println!("4. Select the printed directory.");
    println!("5. Pin the Cobalt BOMTOON Login extension if desired.");
    if replacing {
        println!("Reload Cobalt BOMTOON Login on chrome://extensions.");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn login(target: &LoginTarget) -> Result<(), String> {
    login_with(
        HostLock::acquire,
        Challenge::new,
        open_normal_chrome,
        wait_for_payload,
        |selected| {
            kobo_net::bomtoon::validate_session_cookie(selected)
                .map_err(|_| SESSION_VALIDATION_FAILED.to_owned())
        },
        |selected| install_target(target, selected),
    )
}

fn login_url(challenge: &Challenge) -> String {
    format!("{LOGIN_URL}{}", challenge.fragment())
}

fn open_normal_chrome_with(
    challenge: &Challenge,
    run: impl FnOnce(&mut Command) -> io::Result<ExitStatus>,
) -> Result<(), String> {
    let mut command = Command::new("open");
    command.args(["-a", "Google Chrome"]);
    command.arg(login_url(challenge));
    match run(&mut command) {
        Ok(status) if status.success() => Ok(()),
        _ => Err(BROWSER_LAUNCH_FAILED.to_owned()),
    }
}

#[cfg(target_os = "macos")]
fn open_normal_chrome(challenge: &Challenge) -> Result<(), String> {
    open_normal_chrome_with(challenge, Command::status)
}

trait LoginHandoff: Sized {
    fn payload(&self) -> &HandoffPayload;
    fn succeed(self) -> io::Result<()>;
    fn fail(self) -> io::Result<()>;
}

impl LoginHandoff for PendingHandoff {
    fn payload(&self) -> &HandoffPayload {
        PendingHandoff::payload(self)
    }

    fn succeed(self) -> io::Result<()> {
        PendingHandoff::succeed(self)
    }

    fn fail(self) -> io::Result<()> {
        PendingHandoff::fail(self)
    }
}

fn login_with<Lock, Listener, Handoff>(
    acquire_lock: impl FnOnce() -> io::Result<Lock>,
    create_challenge: impl FnOnce() -> io::Result<(Challenge, Listener)>,
    open_browser: impl FnOnce(&Challenge) -> Result<(), String>,
    receive_payload: impl FnOnce(&Listener, &Challenge, Instant) -> Result<Handoff, HandoffError>,
    validate: impl FnOnce(&str) -> Result<String, String>,
    install: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String>
where
    Handoff: LoginHandoff,
{
    let _host_lock = acquire_lock().map_err(|_| BROWSER_LAUNCH_FAILED.to_owned())?;
    let (challenge, listener) = create_challenge().map_err(|_| BROWSER_LAUNCH_FAILED.to_owned())?;
    open_browser(&challenge).map_err(|_| BROWSER_LAUNCH_FAILED.to_owned())?;
    let deadline = Instant::now()
        .checked_add(LOGIN_TIMEOUT)
        .ok_or_else(|| BROWSER_LAUNCH_FAILED.to_owned())?;
    let handoff =
        receive_payload(&listener, &challenge, deadline).map_err(|error| match error {
            HandoffError::Timeout => format!(
            "{LOGIN_TIMED_OUT}; run kobo bomtoon extension install if the extension is not loaded"
        ),
            HandoffError::Listener => BROWSER_LAUNCH_FAILED.to_owned(),
        })?;
    drop(listener);

    let cookies = browser_cookies(handoff.payload());
    let Ok(selected) = select_session_cookie(&cookies) else {
        let _ = handoff.fail();
        return Err(COOKIE_SELECTION_FAILED.to_owned());
    };
    let result = validate_and_install(&selected, &handoff.payload().fingerprint, validate, install);
    match result {
        Ok(()) => {
            let _ = handoff.succeed();
            Ok(())
        }
        Err(error) => {
            let _ = handoff.fail();
            Err(error)
        }
    }
}

fn private_name(prefix: &str) -> String {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{epoch}-{counter}", std::process::id())
}

fn create_private_directory_at(root: &Path) -> io::Result<PathBuf> {
    for _ in 0..128 {
        let path = root.join(private_name("kobo-bomtoon-private"));
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
        "private directory collision",
    ))
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
    secure: bool,
}

fn browser_cookies(payload: &HandoffPayload) -> Vec<BrowserCookie> {
    payload
        .cookies
        .iter()
        .map(|cookie| BrowserCookie {
            name: cookie.name.clone(),
            value: cookie.value.clone(),
            domain: cookie.domain.clone(),
            path: cookie.path.clone(),
            secure: cookie.secure,
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

fn valid_cookie_scope(cookie: &BrowserCookie, family: &str) -> bool {
    matches!(
        cookie.domain.strip_prefix('.').unwrap_or(&cookie.domain),
        "bomtoon.tw" | "www.bomtoon.tw"
    ) && cookie.path == "/"
        && (family != SECURE_COOKIE || cookie.secure)
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
        if !valid_cookie_scope(cookie, family)
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

fn extension_directory(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Cobalt/bomtoon-login-extension")
}

fn remove_extension_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn record_extension_cleanup(result: io::Result<()>, first_error: &mut Option<io::Error>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => {
            if first_error.is_none() {
                *first_error = Some(error);
            }
            false
        }
    }
}

fn rollback_extension_materialization(
    cobalt_root: &Path,
    staging: &Path,
    destination: &Path,
    backup: &Path,
    backup_active: bool,
    installed: bool,
) -> io::Result<()> {
    let mut first_error = None;
    record_extension_cleanup(remove_extension_path(staging), &mut first_error);

    if backup_active {
        let destination_exists = match path_exists(destination) {
            Ok(exists) => exists,
            Err(error) => {
                record_extension_cleanup(Err(error), &mut first_error);
                false
            }
        };
        let destination_removed = if installed || destination_exists {
            record_extension_cleanup(remove_extension_path(destination), &mut first_error)
        } else {
            true
        };
        if destination_removed {
            record_extension_cleanup(fs::rename(backup, destination), &mut first_error);
        }
    } else if installed {
        record_extension_cleanup(remove_extension_path(destination), &mut first_error);
    }

    record_extension_cleanup(sync_directory(cobalt_root), &mut first_error);
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn cleanup_committed_extension(
    cobalt_root: &Path,
    staging: &Path,
    backup: &Path,
    backup_active: bool,
) -> io::Result<()> {
    let mut first_error = None;
    record_extension_cleanup(remove_extension_path(staging), &mut first_error);
    if backup_active {
        record_extension_cleanup(remove_extension_path(backup), &mut first_error);
    }
    record_extension_cleanup(sync_directory(cobalt_root), &mut first_error);
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn materialize_extension_at_with(
    cobalt_root: &Path,
    before_destination_rename: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    ensure_private_directory(cobalt_root)?;
    let destination = cobalt_root.join("bomtoon-login-extension");
    let staging = create_private_directory_at(cobalt_root)?;
    let backup = cobalt_root.join(private_name("bomtoon-login-extension.backup"));
    let mut backup_active = false;
    let mut installed = false;
    let mut committed = false;

    let operation = (|| -> io::Result<()> {
        for (name, contents) in EXTENSION_FILES {
            let path = staging.join(name);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(contents)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.sync_all()?;
        }
        sync_directory(&staging)?;

        if path_exists(&destination)? {
            fs::rename(&destination, &backup)?;
            backup_active = true;
            sync_directory(cobalt_root)?;
        }
        before_destination_rename(&staging)?;
        fs::rename(&staging, &destination)?;
        installed = true;
        sync_directory(cobalt_root)?;
        committed = true;

        if backup_active {
            remove_extension_path(&backup)?;
            backup_active = false;
            sync_directory(cobalt_root)?;
        }
        Ok(())
    })();

    if let Err(error) = operation {
        let cleanup = if committed {
            cleanup_committed_extension(cobalt_root, &staging, &backup, backup_active)
        } else {
            rollback_extension_materialization(
                cobalt_root,
                &staging,
                &destination,
                &backup,
                backup_active,
                installed,
            )
        };
        return match cleanup {
            Ok(()) => Err(error),
            Err(_) => Err(io::Error::other("extension materialization cleanup failed")),
        };
    }

    Ok(destination)
}

fn materialize_extension_at(cobalt_root: &Path) -> io::Result<PathBuf> {
    materialize_extension_at_with(cobalt_root, |_| Ok(()))
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
    use crate::bomtoon_handoff::{Challenge, HandoffCookie, HandoffError, HandoffPayload};
    use kobo_json::Value;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeSet;
    use std::os::unix::process::ExitStatusExt;
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
            secure: true,
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
            let root = create_private_directory_at(&std::env::temp_dir()).expect("test root");
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
            parse_action(&argument_list(&["login", "--device", "192.0.2.1"])),
            Ok(Action::Login(LoginTarget::Device("192.0.2.1".to_owned())))
        );
        assert_eq!(
            parse_action(&argument_list(&["login", "--sim"])),
            Ok(Action::Login(LoginTarget::Simulator))
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
            assert_eq!(parse_action(&rejected), Err(USAGE.to_owned()));
        }
    }

    #[test]
    fn extension_install_is_an_exact_command() {
        assert_eq!(
            parse_action(&argument_list(&["extension", "install"])),
            Ok(Action::InstallExtension)
        );
        for rejected in [
            argument_list(&["extension"]),
            argument_list(&["extension", "install", "extra"]),
            argument_list(&["extension", "remove"]),
        ] {
            assert_eq!(parse_action(&rejected), Err(USAGE.to_owned()));
        }
    }

    #[test]
    fn extension_manifest_has_only_the_approved_surface() {
        fn collect_strings<'a>(value: &'a Value, strings: &mut Vec<&'a str>) {
            match value {
                Value::String(value) => strings.push(value),
                Value::Array(values) => {
                    for value in values {
                        collect_strings(value, strings);
                    }
                }
                Value::Object(fields) => {
                    for (name, value) in fields {
                        strings.push(name);
                        collect_strings(value, strings);
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::Integer(_) => {}
            }
        }

        let manifest_bytes = EXTENSION_FILES
            .iter()
            .find_map(|(name, bytes)| (*name == "manifest.json").then_some(*bytes))
            .expect("embedded manifest");
        let manifest =
            kobo_json::parse(std::str::from_utf8(manifest_bytes).expect("manifest is valid UTF-8"))
                .expect("manifest is valid JSON");
        let permissions = manifest
            .get("permissions")
            .and_then(Value::as_array)
            .expect("permissions")
            .iter()
            .map(|value| value.as_str().expect("string permission"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            permissions,
            BTreeSet::from(["activeTab", "cookies", "storage"])
        );
        let host_permissions = manifest
            .get("host_permissions")
            .and_then(Value::as_array)
            .expect("host permissions")
            .iter()
            .map(|value| value.as_str().expect("string host permission"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            host_permissions,
            BTreeSet::from([
                "http://127.0.0.1/*",
                "https://*.bomtoon.tw/*",
                "https://www.bomtoon.tw/*",
            ])
        );
        let mut strings = Vec::new();
        collect_strings(&manifest, &mut strings);
        for forbidden in ["google", "<all_urls>", "http://*/*", "https://*/*"] {
            assert!(
                strings.iter().all(|value| !value.contains(forbidden)),
                "manifest contains forbidden string {forbidden}"
            );
        }
    }

    #[test]
    fn extension_materialization_is_exact_private_and_replaceable() {
        assert_eq!(
            extension_directory(Path::new("/Users/cobalt")),
            PathBuf::from(
                "/Users/cobalt/Library/Application Support/Cobalt/bomtoon-login-extension"
            )
        );
        let root = create_private_directory_at(&std::env::temp_dir()).expect("test root");
        let installed = materialize_extension_at(&root).expect("first install");
        assert_eq!(installed, root.join("bomtoon-login-extension"));
        assert_eq!(
            fs::metadata(&installed)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let names = fs::read_dir(&installed)
            .expect("extension files")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names,
            EXTENSION_FILES
                .iter()
                .map(|(name, _)| (*name).into())
                .collect()
        );
        for (name, contents) in EXTENSION_FILES {
            let path = installed.join(name);
            assert_eq!(fs::read(&path).expect("materialized file"), *contents);
            assert_eq!(
                fs::metadata(path)
                    .expect("file metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::write(installed.join("stale.js"), "stale").expect("stale file");
        materialize_extension_at(&root).expect("replacement");
        assert!(!installed.join("stale.js").exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn extension_replacement_rolls_back_when_installation_fails() {
        let root = create_private_directory_at(&std::env::temp_dir()).expect("test root");
        let installed = materialize_extension_at(&root).expect("first install");
        let sentinel = installed.join("existing-install");
        fs::write(&sentinel, "preserve me").expect("sentinel");

        let result = materialize_extension_at_with(&root, |staging| {
            fs::remove_dir_all(staging)?;
            Ok(())
        });

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(sentinel).expect("restored install"),
            "preserve me"
        );
        let transient_names = fs::read_dir(&root)
            .expect("root entries")
            .map(|entry| entry.expect("entry").file_name())
            .filter(|name| name != "bomtoon-login-extension")
            .collect::<Vec<_>>();
        assert!(transient_names.is_empty(), "{transient_names:?}");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn extension_install_lock_serializes_destination_observation_and_materialization() {
        let root = create_private_directory_at(&std::env::temp_dir()).expect("test root");
        let home = root.join("home");
        let destination = extension_directory(&home);
        let cobalt_root = destination
            .parent()
            .expect("extension parent")
            .to_path_buf();
        ensure_private_directory(&cobalt_root).expect("extension parent");
        let lock_drops = Rc::new(Cell::new(0));
        let acquired_lock_drops = Rc::clone(&lock_drops);
        let materialization_lock_drops = Rc::clone(&lock_drops);

        assert_eq!(
            install_extension_with(
                &home,
                || {
                    fs::create_dir(&destination).expect("destination created while acquiring lock");
                    Ok(DropSpy(acquired_lock_drops))
                },
                |observed_root| {
                    assert_eq!(observed_root, cobalt_root);
                    assert_eq!(materialization_lock_drops.get(), 0);
                    Ok(destination.clone())
                },
            ),
            Ok((destination.clone(), true))
        );
        assert_eq!(lock_drops.get(), 1);

        let sentinel = destination.join("existing-install");
        fs::write(&sentinel, "preserve me").expect("sentinel");
        assert_eq!(
            install_extension_with(
                &home,
                || Err::<DropSpy, _>(io::Error::new(io::ErrorKind::AddrInUse, "locked")),
                |_| panic!("a contending install must not materialize the extension"),
            ),
            Err("extension installation failed".to_owned())
        );
        assert_eq!(
            fs::read_to_string(sentinel).expect("unchanged install"),
            "preserve me"
        );
        fs::remove_dir_all(root).expect("cleanup");
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

    fn test_challenge() -> Challenge {
        let (challenge, listener) = Challenge::new().expect("test challenge");
        drop(listener);
        challenge
    }

    fn handoff_payload(cookies: Vec<BrowserCookie>, fingerprint: String) -> HandoffPayload {
        HandoffPayload {
            version: 1,
            fingerprint,
            cookies: cookies
                .into_iter()
                .map(|cookie| HandoffCookie {
                    name: cookie.name,
                    value: cookie.value,
                    domain: cookie.domain,
                    path: cookie.path,
                    secure: cookie.secure,
                })
                .collect(),
        }
    }

    struct FakeHandoff {
        payload: HandoffPayload,
        events: Rc<RefCell<Vec<&'static str>>>,
        drops: Option<Rc<Cell<usize>>>,
        succeed_fails: bool,
    }

    impl LoginHandoff for FakeHandoff {
        fn payload(&self) -> &HandoffPayload {
            &self.payload
        }

        fn succeed(self) -> io::Result<()> {
            self.events.borrow_mut().push("204");
            if self.succeed_fails {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected response failure",
                ))
            } else {
                Ok(())
            }
        }

        fn fail(self) -> io::Result<()> {
            self.events.borrow_mut().push("422");
            Ok(())
        }
    }

    impl Drop for FakeHandoff {
        fn drop(&mut self) {
            if let Some(drops) = &self.drops {
                drops.set(drops.get() + 1);
            }
        }
    }

    struct DropSpy(Rc<Cell<usize>>);

    impl Drop for DropSpy {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn a_secure_family_member_requires_the_secure_attribute() {
        let mut exposed = cookie(SECURE_COOKIE, "secret");
        exposed.secure = false;
        assert!(select_session_cookie(&[exposed]).is_err());
    }

    #[test]
    fn handoff_payload_maps_only_cookie_fields() {
        let payload = HandoffPayload {
            version: 1,
            fingerprint: fingerprint('a'),
            cookies: vec![HandoffCookie {
                name: SECURE_COOKIE.to_owned(),
                value: "secret".to_owned(),
                domain: ".bomtoon.tw".to_owned(),
                path: "/".to_owned(),
                secure: true,
            }],
        };
        assert_eq!(
            browser_cookies(&payload),
            vec![cookie(SECURE_COOKIE, "secret")]
        );
    }

    #[test]
    fn normal_chrome_receives_only_the_exact_fragment_login_url() {
        let challenge = test_challenge();
        let expected_url = format!("{LOGIN_URL}{}", challenge.fragment());
        let mut observed = Vec::new();
        assert_eq!(
            open_normal_chrome_with(&challenge, |command| {
                observed.push(command.get_program().to_os_string());
                observed.extend(command.get_args().map(ToOwned::to_owned));
                Ok(std::process::ExitStatus::from_raw(0))
            }),
            Ok(())
        );
        assert_eq!(
            observed,
            ["open", "-a", "Google Chrome", expected_url.as_str()]
                .into_iter()
                .map(Into::into)
                .collect::<Vec<std::ffi::OsString>>()
        );
        let rendered = observed
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        let forbidden = [
            SECURE_COOKIE.to_owned(),
            "fingerprint".to_owned(),
            "token".to_owned(),
            "email".to_owned(),
            "userId".to_owned(),
            ["remote", "debugging"].join("-"),
            ["user", "data", "dir"].join("-"),
        ];
        for forbidden in &forbidden {
            assert!(
                !rendered.contains(forbidden),
                "browser argument exposed {forbidden}"
            );
        }
    }

    #[test]
    fn normal_chrome_launch_failures_use_the_fixed_error() {
        let challenge = test_challenge();
        assert_eq!(
            open_normal_chrome_with(&challenge, |_| {
                Ok(std::process::ExitStatus::from_raw(1 << 8))
            }),
            Err(BROWSER_LAUNCH_FAILED.to_owned())
        );
        assert_eq!(
            open_normal_chrome_with(&challenge, |_| {
                Err(io::Error::other("injected launch failure"))
            }),
            Err(BROWSER_LAUNCH_FAILED.to_owned())
        );
    }

    #[test]
    fn matching_handoff_selects_validates_installs_then_sends_204() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let locked = Rc::clone(&events);
        let challenged = Rc::clone(&events);
        let opened = Rc::clone(&events);
        let received = Rc::clone(&events);
        let responded = Rc::clone(&events);
        let validated = Rc::clone(&events);
        let installed = Rc::clone(&events);
        let expected_fingerprint = fingerprint('a');
        let payload = handoff_payload(
            vec![
                cookie(INSECURE_COOKIE, "not-selected"),
                cookie(SECURE_COOKIE, "selected"),
            ],
            expected_fingerprint.clone(),
        );

        assert_eq!(
            login_with(
                || {
                    locked.borrow_mut().push("lock");
                    Ok(DropSpy(Rc::new(Cell::new(0))))
                },
                || {
                    challenged.borrow_mut().push("challenge");
                    Ok((test_challenge(), DropSpy(Rc::new(Cell::new(0)))))
                },
                |_| {
                    opened.borrow_mut().push("open");
                    Ok(())
                },
                |_, _, deadline| {
                    assert!(deadline > Instant::now());
                    received.borrow_mut().push("receive");
                    Ok(FakeHandoff {
                        payload,
                        events: responded,
                        drops: None,
                        succeed_fails: false,
                    })
                },
                |selected| {
                    validated.borrow_mut().push("validate");
                    assert_eq!(selected, format!("{SECURE_COOKIE}=selected"));
                    Ok(expected_fingerprint)
                },
                |selected| {
                    installed.borrow_mut().push("install");
                    assert_eq!(selected, format!("{SECURE_COOKIE}=selected"));
                    Ok(())
                },
            ),
            Ok(())
        );
        assert_eq!(
            *events.borrow(),
            [
                "lock",
                "challenge",
                "open",
                "receive",
                "validate",
                "install",
                "204",
            ]
        );
    }

    #[test]
    fn terminal_success_response_failure_does_not_fail_committed_login() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let responded = Rc::clone(&events);
        let validated = Rc::clone(&events);
        let installed = Rc::clone(&events);
        let expected_fingerprint = fingerprint('a');
        let payload = handoff_payload(
            vec![cookie(SECURE_COOKIE, "selected")],
            expected_fingerprint.clone(),
        );

        assert_eq!(
            login_with(
                || Ok(DropSpy(Rc::new(Cell::new(0)))),
                || Ok((test_challenge(), DropSpy(Rc::new(Cell::new(0))))),
                |_| Ok(()),
                |_, _, _| {
                    Ok(FakeHandoff {
                        payload,
                        events: responded,
                        drops: None,
                        succeed_fails: true,
                    })
                },
                |_| {
                    validated.borrow_mut().push("validate");
                    Ok(expected_fingerprint)
                },
                |_| {
                    installed.borrow_mut().push("install");
                    Ok(())
                },
            ),
            Ok(())
        );
        assert_eq!(*events.borrow(), ["validate", "install", "204"]);
    }

    #[test]
    fn malformed_handoff_cookies_install_nothing_and_send_422() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let responded = Rc::clone(&events);
        let payload = handoff_payload(
            vec![cookie(&format!("{SECURE_COOKIE}.1"), "missing-zero")],
            fingerprint('a'),
        );
        assert_eq!(
            login_with(
                || Ok(DropSpy(Rc::new(Cell::new(0)))),
                || Ok((test_challenge(), DropSpy(Rc::new(Cell::new(0))))),
                |_| Ok(()),
                |_, _, _| {
                    Ok(FakeHandoff {
                        payload,
                        events: responded,
                        drops: None,
                        succeed_fails: false,
                    })
                },
                |_| panic!("malformed cookies must not be validated"),
                |_| panic!("malformed cookies must not be installed"),
            ),
            Err(COOKIE_SELECTION_FAILED.to_owned())
        );
        assert_eq!(*events.borrow(), ["422"]);
    }

    #[test]
    fn validation_failure_or_fingerprint_mismatch_sends_422_without_installing() {
        for validation in [
            Err("injected validator failure".to_owned()),
            Ok(fingerprint('b')),
        ] {
            let events = Rc::new(RefCell::new(Vec::new()));
            let responded = Rc::clone(&events);
            let validated = Rc::clone(&events);
            let payload =
                handoff_payload(vec![cookie(SECURE_COOKIE, "selected")], fingerprint('a'));
            assert_eq!(
                login_with(
                    || Ok(DropSpy(Rc::new(Cell::new(0)))),
                    || Ok((test_challenge(), DropSpy(Rc::new(Cell::new(0))))),
                    |_| Ok(()),
                    |_, _, _| {
                        Ok(FakeHandoff {
                            payload,
                            events: responded,
                            drops: None,
                            succeed_fails: false,
                        })
                    },
                    |_| {
                        validated.borrow_mut().push("validate");
                        validation
                    },
                    |_| panic!("an invalid session must not be installed"),
                ),
                Err(SESSION_VALIDATION_FAILED.to_owned())
            );
            assert_eq!(*events.borrow(), ["validate", "422"]);
        }
    }

    #[test]
    fn target_failure_sends_422_after_the_install_attempt() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let responded = Rc::clone(&events);
        let installed = Rc::clone(&events);
        let expected_fingerprint = fingerprint('a');
        let payload = handoff_payload(
            vec![cookie(SECURE_COOKIE, "selected")],
            expected_fingerprint.clone(),
        );
        assert_eq!(
            login_with(
                || Ok(DropSpy(Rc::new(Cell::new(0)))),
                || Ok((test_challenge(), DropSpy(Rc::new(Cell::new(0))))),
                |_| Ok(()),
                |_, _, _| {
                    Ok(FakeHandoff {
                        payload,
                        events: responded,
                        drops: None,
                        succeed_fails: false,
                    })
                },
                |_| Ok(expected_fingerprint),
                |_| {
                    installed.borrow_mut().push("install");
                    Err("injected target failure".to_owned())
                },
            ),
            Err(TARGET_INSTALLATION_FAILED.to_owned())
        );
        assert_eq!(*events.borrow(), ["install", "422"]);
    }

    #[test]
    fn handoff_timeout_names_the_extension_install_guidance() {
        assert_eq!(
            login_with(
                || Ok(DropSpy(Rc::new(Cell::new(0)))),
                || Ok((test_challenge(), DropSpy(Rc::new(Cell::new(0))))),
                |_| Ok(()),
                |_, _, _| -> Result<FakeHandoff, HandoffError> {
                    Err(HandoffError::Timeout)
                },
                |_| panic!("a timeout has no cookie"),
                |_| panic!("a timeout cannot install"),
            ),
            Err(format!(
                "{LOGIN_TIMED_OUT}; run kobo bomtoon extension install if the extension is not loaded"
            ))
        );
    }

    #[derive(Clone, Copy)]
    enum ResourceResult {
        BrowserFailure,
        Timeout,
        CookieFailure,
        ValidationFailure,
        InstallationFailure,
        Success,
    }

    #[test]
    fn every_login_result_drops_the_host_lock_listener_and_pending_handoff() {
        for scenario in [
            ResourceResult::BrowserFailure,
            ResourceResult::Timeout,
            ResourceResult::CookieFailure,
            ResourceResult::ValidationFailure,
            ResourceResult::InstallationFailure,
            ResourceResult::Success,
        ] {
            let lock_drops = Rc::new(Cell::new(0));
            let listener_drops = Rc::new(Cell::new(0));
            let handoff_drops = Rc::new(Cell::new(0));
            let lock = Rc::clone(&lock_drops);
            let listener = Rc::clone(&listener_drops);
            let pending = Rc::clone(&handoff_drops);
            let payload = handoff_payload(
                if matches!(scenario, ResourceResult::CookieFailure) {
                    Vec::new()
                } else {
                    vec![cookie(SECURE_COOKIE, "selected")]
                },
                fingerprint('a'),
            );
            let _ = login_with(
                || Ok(DropSpy(lock)),
                || Ok((test_challenge(), DropSpy(listener))),
                |_| {
                    if matches!(scenario, ResourceResult::BrowserFailure) {
                        Err(BROWSER_LAUNCH_FAILED.to_owned())
                    } else {
                        Ok(())
                    }
                },
                |_, _, _| {
                    if matches!(scenario, ResourceResult::Timeout) {
                        Err(HandoffError::Timeout)
                    } else {
                        Ok(FakeHandoff {
                            payload,
                            events: Rc::new(RefCell::new(Vec::new())),
                            drops: Some(pending),
                            succeed_fails: false,
                        })
                    }
                },
                |_| {
                    if matches!(scenario, ResourceResult::ValidationFailure) {
                        Err("injected validator failure".to_owned())
                    } else {
                        Ok(fingerprint('a'))
                    }
                },
                |_| {
                    if matches!(scenario, ResourceResult::InstallationFailure) {
                        Err("injected target failure".to_owned())
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(lock_drops.get(), 1);
            assert_eq!(listener_drops.get(), 1);
            assert_eq!(
                handoff_drops.get(),
                usize::from(!matches!(
                    scenario,
                    ResourceResult::BrowserFailure | ResourceResult::Timeout
                ))
            );
        }
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
}
