use crate::tasks::MAX_SECRET_BYTES;
use kobo_protocol::{Credential, SecretHeader, TaskError};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Access tokens at or inside this window are renewed before use.
pub const REFRESH_WINDOW_MS: u64 = 5 * 60 * 1000;
/// A thread-safe clock returning milliseconds since the Unix epoch.
pub type Clock = dyn Fn() -> u64 + Send + Sync;

const STATE_VERSION: &str = "cobalt-managed-v1";
const MAX_STATE_BYTES: usize = 2 * MAX_SECRET_BYTES + 1024;
static NEXT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum FaultPoint {
    ReadState,
    RenameCookie,
    RemoveState,
    SyncStateParentAfterRename,
    SyncSecretsAfterRename,
}

#[cfg(test)]
thread_local! {
    static INJECTED_FAULT: std::cell::Cell<Option<FaultPoint>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
fn inject_fault(point: FaultPoint) {
    INJECTED_FAULT.with(|fault| {
        assert!(fault.replace(Some(point)).is_none(), "fault already injected");
    });
}

#[cfg(test)]
fn take_fault(point: FaultPoint) -> bool {
    INJECTED_FAULT.with(|fault| {
        if fault.get() == Some(point) {
            fault.set(None);
            true
        } else {
            false
        }
    })
}

/// A provider-issued access and refresh token pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedTokenPair {
    /// Secret sent to the provider as a bearer token.
    pub access_token: String,
    /// Epoch-millisecond expiry of the access token.
    pub access_expires_at_ms: u64,
    /// Secret used only to rotate the access token.
    pub refresh_token: String,
    /// Epoch-millisecond expiry of the refresh token.
    pub refresh_expires_at_ms: u64,
}

/// A request header resolved without exposing its source credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCredential {
    /// HTTP header name.
    pub header_name: String,
    /// Complete HTTP header value.
    pub header_value: String,
}

/// Provider-specific operations for one cookie-bound managed credential.
pub trait ManagedCredentialRecipe: Send + Sync {
    /// Application-facing credential name.
    fn credential_name(&self) -> &'static str;
    /// Runtime secret file holding the provider session cookie.
    fn binding_secret_name(&self) -> &'static str;
    /// Stable, non-secret identifier for the current session cookie.
    fn binding_digest(&self, secret: &str) -> String;
    /// Creates a token pair from the current provider session.
    fn bootstrap(&self, binding_secret: &str) -> Result<ManagedTokenPair, TaskError>;
    /// Rotates a token pair using its refresh token.
    fn refresh(
        &self,
        binding_secret: &str,
        pair: &ManagedTokenPair,
    ) -> Result<ManagedTokenPair, TaskError>;
    /// Invalidates the provider session and token pair remotely.
    fn revoke(&self, binding_secret: &str, pair: &ManagedTokenPair)
        -> Result<(), TaskError>;
}

/// Runtime-owned state for one provider-managed credential.
pub struct ManagedCredentials {
    secrets: PathBuf,
    state: PathBuf,
    clock: Arc<Clock>,
    recipe: Arc<dyn ManagedCredentialRecipe>,
    inner: Mutex<ProviderState>,
}

#[derive(Default)]
struct ProviderState {
    cached: Option<BoundPair>,
}

#[derive(Clone)]
struct BoundPair {
    cookie_digest: String,
    pair: ManagedTokenPair,
}

/// Returns the durable state path for a managed credential.
#[must_use]
pub fn managed_state_path(root: &Path, credential: &str) -> PathBuf {
    root.join(format!("{credential}.state"))
}

impl ManagedCredentials {
    /// Constructs a provider and completes any interrupted local revocation.
    pub fn new(
        secrets: impl Into<PathBuf>,
        state: impl Into<PathBuf>,
        clock: Arc<Clock>,
        recipe: Arc<dyn ManagedCredentialRecipe>,
    ) -> Result<Self, TaskError> {
        let provider = Self {
            secrets: secrets.into(),
            state: state.into(),
            clock,
            recipe,
            inner: Mutex::new(ProviderState::default()),
        };
        provider.remove_stale_detached()?;
        Ok(provider)
    }

    /// Resolves the managed bearer credential, refreshing it when needed.
    pub fn resolve(
        &self,
        wanted: &Credential,
    ) -> Result<Option<ResolvedCredential>, TaskError> {
        if wanted.secret != self.recipe.credential_name() {
            return Ok(None);
        }
        if !matches!(&wanted.header, SecretHeader::Bearer) {
            return Err(TaskError::Denied);
        }

        let mut inner = self.lock_inner()?;
        let cookie = self
            .read_binding_secret()?
            .ok_or(TaskError::NoCredential)?;
        let digest = self.checked_digest(&cookie)?;
        let pair = self.obtain_pair(&mut inner, &cookie, &digest, false)?;
        if pair.access_expires_at_ms <= (self.clock)() {
            return Err(TaskError::Unreachable);
        }
        Ok(Some(ResolvedCredential {
            header_name: "Authorization".to_owned(),
            header_value: format!("Bearer {}", pair.access_token),
        }))
    }

    /// Renews a matching managed bearer credential regardless of its expiry.
    pub fn force_renew(&self, wanted: &Credential) -> Result<bool, TaskError> {
        if wanted.secret != self.recipe.credential_name() {
            return Ok(false);
        }
        if !matches!(&wanted.header, SecretHeader::Bearer) {
            return Err(TaskError::Denied);
        }

        let mut inner = self.lock_inner()?;
        let cookie = self
            .read_binding_secret()?
            .ok_or(TaskError::NoCredential)?;
        let digest = self.checked_digest(&cookie)?;
        self.obtain_pair(&mut inner, &cookie, &digest, true)?;
        Ok(true)
    }

    /// Removes local credentials before attempting provider revocation.
    pub fn revoke(&self, credential: &str) -> Result<bool, TaskError> {
        if credential != self.recipe.credential_name() {
            return Ok(false);
        }

        let mut inner = self.lock_inner()?;
        let cookie = self.read_binding_secret()?;
        let remote = if let Some(cookie) = cookie {
            let digest = self.checked_digest(&cookie)?;
            self.pair_for_revoke(&inner, &digest)?
                .map(|pair| (cookie, pair))
        } else {
            None
        };
        self.detach_and_clear(&mut inner)?;
        drop(inner);

        let remote_result = remote
            .as_ref()
            .map_or(Ok(()), |(cookie, pair)| self.recipe.revoke(cookie, pair));
        drop(remote);
        remote_result.map_err(|_| TaskError::RevocationUnconfirmed)?;
        Ok(true)
    }

    fn obtain_pair(
        &self,
        inner: &mut ProviderState,
        cookie: &str,
        digest: &str,
        force_refresh: bool,
    ) -> Result<ManagedTokenPair, TaskError> {
        self.bind_cached_or_durable(inner, digest)?;
        let had_pair = inner.cached.is_some();
        if inner.cached.is_none() {
            let pair = self.recipe.bootstrap(cookie)?;
            self.replace_with_provider_pair(inner, digest, pair)?;
        }

        let now = (self.clock)();
        let needs_refresh = (force_refresh && had_pair)
            || inner.cached.as_ref().is_some_and(|bound| {
                bound.pair.access_expires_at_ms <= now.saturating_add(REFRESH_WINDOW_MS)
            });
        if needs_refresh {
            self.refresh_pair(inner, cookie, digest)?;
        }
        inner
            .cached
            .as_ref()
            .map(|bound| bound.pair.clone())
            .ok_or(TaskError::LocalStorage)
    }

    fn bind_cached_or_durable(
        &self,
        inner: &mut ProviderState,
        digest: &str,
    ) -> Result<(), TaskError> {
        if inner
            .cached
            .as_ref()
            .is_some_and(|bound| bound.cookie_digest != digest)
        {
            inner.cached = None;
            remove_file_synced(&self.state_path())?;
        }
        if inner.cached.is_some() {
            return Ok(());
        }

        match read_bound_pair(&self.state_path())? {
            Some(bound) if bound.cookie_digest == digest => inner.cached = Some(bound),
            Some(_) => {
                remove_file_synced(&self.state_path())?;
            }
            None => {}
        }
        Ok(())
    }

    fn refresh_pair(
        &self,
        inner: &mut ProviderState,
        cookie: &str,
        digest: &str,
    ) -> Result<(), TaskError> {
        let old_pair = inner
            .cached
            .as_ref()
            .map(|bound| bound.pair.clone())
            .ok_or(TaskError::LocalStorage)?;
        match self.recipe.refresh(cookie, &old_pair) {
            Ok(pair) => self.replace_with_provider_pair(inner, digest, pair),
            Err(TaskError::Unauthorized) => match self.recipe.bootstrap(cookie) {
                Ok(pair) => self.replace_with_provider_pair(inner, digest, pair),
                Err(TaskError::Unauthorized) => {
                    self.detach_and_clear(inner)?;
                    Err(TaskError::Unauthorized)
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        }
    }

    fn replace_with_provider_pair(
        &self,
        inner: &mut ProviderState,
        digest: &str,
        pair: ManagedTokenPair,
    ) -> Result<(), TaskError> {
        let now = (self.clock)();
        if !valid_provider_pair(&pair, now) {
            return Err(TaskError::Unreachable);
        }
        let bound = BoundPair {
            cookie_digest: digest.to_owned(),
            pair,
        };
        match write_bound_pair(&self.state_path(), &bound) {
            Ok(()) => {
                inner.cached = Some(bound);
                Ok(())
            }
            Err(StateWriteFailure::BeforeRename) => Err(TaskError::LocalStorage),
            Err(StateWriteFailure::AfterRename) => {
                inner.cached = None;
                Err(TaskError::LocalStorage)
            }
        }
    }

    fn pair_for_revoke(
        &self,
        inner: &ProviderState,
        digest: &str,
    ) -> Result<Option<ManagedTokenPair>, TaskError> {
        if let Some(bound) = &inner.cached {
            return Ok((bound.cookie_digest == digest).then(|| bound.pair.clone()));
        }
        Ok(read_bound_pair(&self.state_path())?
            .filter(|bound| bound.cookie_digest == digest)
            .map(|bound| bound.pair))
    }

    fn detach_and_clear(&self, inner: &mut ProviderState) -> Result<(), TaskError> {
        let cookie = self.cookie_path();
        let detached = self.detached_path();
        #[cfg(test)]
        if take_fault(FaultPoint::RenameCookie) {
            return Err(TaskError::LocalStorage);
        }
        let renamed = match fs::rename(&cookie, &detached) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(_) => return Err(TaskError::LocalStorage),
        };

        let rename_sync_failed = if renamed {
            #[cfg(test)]
            if take_fault(FaultPoint::SyncSecretsAfterRename) {
                true
            } else {
                sync_directory(&self.secrets).is_err()
            }
            #[cfg(not(test))]
            {
                sync_directory(&self.secrets).is_err()
            }
        } else {
            false
        };
        let state_result = self.clear_state_synced();
        inner.cached = None;
        if state_result.is_err() || rename_sync_failed {
            return Err(TaskError::LocalStorage);
        }
        if renamed {
            remove_file_synced(&detached)?;
        }
        Ok(())
    }

    fn remove_stale_detached(&self) -> Result<(), TaskError> {
        let detached = self.detached_path();
        let detached_exists = match fs::metadata(&detached) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(_) => return Err(TaskError::LocalStorage),
        };
        if detached_exists {
            self.clear_state_synced()?;
            remove_file_synced(&detached)?;
        }
        Ok(())
    }

    fn clear_state_synced(&self) -> Result<(), TaskError> {
        remove_file_synced(&self.state_path())?;
        sync_directory(&self.state)
    }

    fn read_binding_secret(&self) -> Result<Option<String>, TaskError> {
        read_secret(&self.cookie_path())
    }

    fn checked_digest(&self, cookie: &str) -> Result<String, TaskError> {
        let digest = self.recipe.binding_digest(cookie);
        if valid_secret_field(&digest) {
            Ok(digest)
        } else {
            Err(TaskError::LocalStorage)
        }
    }

    fn cookie_path(&self) -> PathBuf {
        self.secrets.join(self.recipe.binding_secret_name())
    }

    fn detached_path(&self) -> PathBuf {
        self.secrets
            .join(format!(".{}.revoking", self.recipe.binding_secret_name()))
    }

    fn state_path(&self) -> PathBuf {
        managed_state_path(&self.state, self.recipe.credential_name())
    }

    fn lock_inner(&self) -> Result<MutexGuard<'_, ProviderState>, TaskError> {
        self.inner.lock().map_err(|_| TaskError::LocalStorage)
    }
}

fn valid_secret_field(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SECRET_BYTES
        && !value.chars().any(char::is_control)
}

fn valid_pair(pair: &ManagedTokenPair) -> bool {
    valid_secret_field(&pair.access_token) && valid_secret_field(&pair.refresh_token)
}

fn valid_provider_pair(pair: &ManagedTokenPair, now: u64) -> bool {
    valid_pair(pair)
        && pair.access_expires_at_ms > now
        && pair.refresh_expires_at_ms > now
}

fn read_secret(path: &Path) -> Result<Option<String>, TaskError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(TaskError::LocalStorage),
    };
    let mut bytes = Vec::new();
    file.take(MAX_SECRET_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| TaskError::LocalStorage)?;
    if bytes.len() > MAX_SECRET_BYTES {
        return Err(TaskError::LocalStorage);
    }
    let secret = String::from_utf8(bytes).map_err(|_| TaskError::LocalStorage)?;
    let secret = secret.trim().to_owned();
    if secret.is_empty() {
        return Ok(None);
    }
    if secret.chars().any(char::is_control) {
        return Err(TaskError::LocalStorage);
    }
    Ok(Some(secret))
}

fn read_bound_pair(path: &Path) -> Result<Option<BoundPair>, TaskError> {
    #[cfg(test)]
    if take_fault(FaultPoint::ReadState) {
        return Err(TaskError::LocalStorage);
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(TaskError::LocalStorage),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| TaskError::LocalStorage)?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(TaskError::LocalStorage);
    }
    let encoded = std::str::from_utf8(&bytes).map_err(|_| TaskError::LocalStorage)?;
    parse_bound_pair(encoded).ok_or(TaskError::LocalStorage).map(Some)
}

fn parse_bound_pair(encoded: &str) -> Option<BoundPair> {
    let mut lines = encoded.split('\n');
    if lines.next()? != STATE_VERSION {
        return None;
    }
    let cookie_digest = lines.next()?;
    let access_expires_at_ms = lines.next()?.parse().ok()?;
    let refresh_expires_at_ms = lines.next()?.parse().ok()?;
    let access_token = lines.next()?;
    let refresh_token = lines.next()?;
    if lines.next().is_some()
        || !valid_secret_field(cookie_digest)
        || !valid_secret_field(access_token)
        || !valid_secret_field(refresh_token)
    {
        return None;
    }
    let pair = ManagedTokenPair {
        access_token: access_token.to_owned(),
        access_expires_at_ms,
        refresh_token: refresh_token.to_owned(),
        refresh_expires_at_ms,
    };
    valid_pair(&pair).then(|| BoundPair {
        cookie_digest: cookie_digest.to_owned(),
        pair,
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum StateWriteFailure {
    BeforeRename,
    AfterRename,
}

fn write_bound_pair(path: &Path, bound: &BoundPair) -> Result<(), StateWriteFailure> {
    if !valid_secret_field(&bound.cookie_digest) || !valid_pair(&bound.pair) {
        return Err(StateWriteFailure::BeforeRename);
    }
    let encoded = format!(
        "{STATE_VERSION}\n{}\n{}\n{}\n{}\n{}",
        bound.cookie_digest,
        bound.pair.access_expires_at_ms,
        bound.pair.refresh_expires_at_ms,
        bound.pair.access_token,
        bound.pair.refresh_token
    );
    if encoded.len() > MAX_STATE_BYTES {
        return Err(StateWriteFailure::BeforeRename);
    }

    let (temporary, mut file) =
        create_temporary(path).map_err(|_| StateWriteFailure::BeforeRename)?;
    if file.write_all(encoded.as_bytes()).is_err() || file.sync_all().is_err() {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(StateWriteFailure::BeforeRename);
    }
    drop(file);
    if fs::rename(&temporary, path).is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(StateWriteFailure::BeforeRename);
    }
    #[cfg(test)]
    if take_fault(FaultPoint::SyncStateParentAfterRename) {
        return Err(StateWriteFailure::AfterRename);
    }
    sync_parent(path).map_err(|_| StateWriteFailure::AfterRename)
}

fn create_temporary(path: &Path) -> Result<(PathBuf, File), TaskError> {
    let file_name = path.file_name().ok_or(TaskError::LocalStorage)?;
    for _ in 0..16 {
        let sequence = NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(file_name);
        name.push(format!(".tmp.{}.{}", std::process::id(), sequence));
        let temporary = path.with_file_name(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(TaskError::LocalStorage),
        }
    }
    Err(TaskError::LocalStorage)
}

fn remove_file_synced(path: &Path) -> Result<bool, TaskError> {
    #[cfg(test)]
    if path
        .extension()
        .is_some_and(|extension| extension == std::ffi::OsStr::new("state"))
        && take_fault(FaultPoint::RemoveState)
    {
        return Err(TaskError::LocalStorage);
    }
    match fs::remove_file(path) {
        Ok(()) => {
            sync_parent(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(TaskError::LocalStorage),
    }
}

fn sync_parent(path: &Path) -> Result<(), TaskError> {
    let parent = path.parent().ok_or(TaskError::LocalStorage)?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), TaskError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| TaskError::LocalStorage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{hash_map::DefaultHasher, VecDeque};
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{mpsc, Barrier};
    use std::thread;
    use std::time::Duration;

    type OperationObserver = dyn Fn() + Send + Sync;

    #[derive(Debug, Eq, PartialEq)]
    struct RevokeInput {
        cookie_fingerprint: u64,
        pair_fingerprint: u64,
    }

    #[derive(Default)]
    struct FakeRecipe {
        calls: Mutex<Vec<&'static str>>,
        bootstrap: Mutex<VecDeque<Result<ManagedTokenPair, TaskError>>>,
        refresh: Mutex<VecDeque<Result<ManagedTokenPair, TaskError>>>,
        revoke: Mutex<VecDeque<Result<(), TaskError>>>,
        refresh_observer: Mutex<Option<Box<OperationObserver>>>,
        revoke_observer: Mutex<Option<Box<OperationObserver>>>,
        revoke_inputs: Mutex<Vec<RevokeInput>>,
    }

    impl ManagedCredentialRecipe for FakeRecipe {
        fn credential_name(&self) -> &'static str { "bomtoon-access-token" }
        fn binding_secret_name(&self) -> &'static str { "bomtoon-session" }
        fn binding_digest(&self, secret: &str) -> String { format!("digest:{secret}") }

        fn bootstrap(&self, _binding_secret: &str) -> Result<ManagedTokenPair, TaskError> {
            self.calls.lock().expect("calls").push("bootstrap");
            self.bootstrap.lock().expect("bootstrap queue").pop_front().expect("bootstrap result")
        }

        fn refresh(&self, _binding_secret: &str, _pair: &ManagedTokenPair) -> Result<ManagedTokenPair, TaskError> {
            self.calls.lock().expect("calls").push("refresh");
            if let Some(observer) = self.refresh_observer.lock().expect("refresh observer").as_ref() {
                observer();
            }
            self.refresh.lock().expect("refresh queue").pop_front().expect("refresh result")
        }

        fn revoke(&self, binding_secret: &str, pair: &ManagedTokenPair) -> Result<(), TaskError> {
            self.calls.lock().expect("calls").push("revoke");
            self.revoke_inputs
                .lock()
                .expect("revoke inputs")
                .push(RevokeInput {
                    cookie_fingerprint: fingerprint(binding_secret),
                    pair_fingerprint: pair_fingerprint(pair),
                });
            if let Some(observer) = self.revoke_observer.lock().expect("revoke observer").as_ref() {
                observer();
            }
            self.revoke.lock().expect("revoke queue").pop_front().expect("revoke result")
        }
    }

    struct TestDirectories { root: PathBuf, secrets: PathBuf, state: PathBuf }

    impl TestDirectories {
        fn new(label: &str) -> Self {
            static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("cobalt-managed-{label}-{}-{sequence}", std::process::id()));
            let secrets = root.join("secrets");
            let state = root.join("state");
            fs::create_dir_all(&secrets).expect("secret directory");
            fs::create_dir_all(&state).expect("state directory");
            Self { root, secrets, state }
        }
        fn cookie(&self) -> PathBuf { self.secrets.join("bomtoon-session") }
        fn detached(&self) -> PathBuf { self.secrets.join(".bomtoon-session.revoking") }
        fn managed_state(&self) -> PathBuf { managed_state_path(&self.state, "bomtoon-access-token") }
    }

    impl Drop for TestDirectories {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); }
    }

    fn token_pair(label: &str, access_expires_at_ms: u64) -> ManagedTokenPair {
        ManagedTokenPair {
            access_token: format!("access-{label}"),
            access_expires_at_ms,
            refresh_token: format!("refresh-{label}"),
            refresh_expires_at_ms: access_expires_at_ms.saturating_add(3_600_000),
        }
    }

    fn fingerprint(value: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    fn pair_fingerprint(pair: &ManagedTokenPair) -> u64 {
        let mut hasher = DefaultHasher::new();
        pair.access_token.hash(&mut hasher);
        pair.access_expires_at_ms.hash(&mut hasher);
        pair.refresh_token.hash(&mut hasher);
        pair.refresh_expires_at_ms.hash(&mut hasher);
        hasher.finish()
    }

    fn adjustable_clock(initial: u64) -> (Arc<AtomicU64>, Arc<Clock>) {
        let now = Arc::new(AtomicU64::new(initial));
        let clock_now = Arc::clone(&now);
        let clock: Arc<Clock> = Arc::new(move || clock_now.load(Ordering::SeqCst));
        (now, clock)
    }

    fn provider(directories: &TestDirectories, clock: Arc<Clock>, recipe: Arc<FakeRecipe>) -> ManagedCredentials {
        ManagedCredentials::new(directories.secrets.clone(), directories.state.clone(), clock, recipe).expect("managed provider")
    }

    #[test]
    fn missing_state_bootstraps_and_persists_a_cookie_bound_pair() {
        let directories = TestDirectories::new("bootstrap");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe.bootstrap.lock().expect("bootstrap queue").push_back(Ok(token_pair("a", 1_000_000)));
        let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));
        assert_eq!(managed.resolve(&Credential::bearer("bomtoon-access-token")), Ok(Some(ResolvedCredential { header_name: "Authorization".to_owned(), header_value: "Bearer access-a".to_owned() })));
        assert_eq!(fs::read_to_string(directories.managed_state()).expect("managed state"), "cobalt-managed-v1\ndigest:cookie-a\n1000000\n4600000\naccess-a\nrefresh-a");
        let mode = fs::metadata(directories.managed_state()).expect("managed metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn replacing_the_cookie_invalidates_cached_and_durable_tokens() {
        let directories = TestDirectories::new("cookie-replacement");
        fs::write(directories.cookie(), "cookie-a").expect("cookie a");
        let recipe = Arc::new(FakeRecipe::default());
        recipe.bootstrap.lock().expect("bootstrap queue").extend([Ok(token_pair("a", 1_000_000)), Ok(token_pair("b", 1_000_000))]);
        let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));
        managed.resolve(&Credential::bearer("bomtoon-access-token")).expect("first resolution");
        fs::write(directories.cookie(), "cookie-b").expect("cookie b");
        assert_eq!(managed.resolve(&Credential::bearer("bomtoon-access-token")), Ok(Some(ResolvedCredential { header_name: "Authorization".to_owned(), header_value: "Bearer access-b".to_owned() })));
        let encoded = fs::read_to_string(directories.managed_state()).expect("managed state");
        assert!(encoded.contains("digest:cookie-b"));
        assert!(encoded.contains("access-b"));
        assert!(!encoded.contains("access-a"));
    }

    #[test]
    fn resolve_refreshes_inside_the_five_minute_window() {
        let directories = TestDirectories::new("refresh-window");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe.bootstrap.lock().expect("bootstrap queue").push_back(Ok(token_pair("a", REFRESH_WINDOW_MS + 101)));
        recipe.refresh.lock().expect("refresh queue").push_back(Ok(token_pair("b", 2_000_000)));
        let (now, clock) = adjustable_clock(100);
        let managed = provider(&directories, clock, Arc::clone(&recipe));
        managed.resolve(&Credential::bearer("bomtoon-access-token")).expect("initial resolution");
        now.store(101, Ordering::SeqCst);
        assert_eq!(managed.resolve(&Credential::bearer("bomtoon-access-token")), Ok(Some(ResolvedCredential { header_name: "Authorization".to_owned(), header_value: "Bearer access-b".to_owned() })));
    }

    #[test]
    fn malformed_refresh_preserves_the_last_valid_pair() {
        let directories = TestDirectories::new("malformed-refresh");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe.bootstrap.lock().expect("bootstrap queue").push_back(Ok(token_pair("a", 1_000_000)));
        recipe.refresh.lock().expect("refresh queue").push_back(Ok(ManagedTokenPair { access_token: "access-invalid\nvalue".to_owned(), access_expires_at_ms: 2_000_000, refresh_token: "refresh-b".to_owned(), refresh_expires_at_ms: 3_000_000 }));
        let (now, clock) = adjustable_clock(1);
        let managed = provider(&directories, clock, Arc::clone(&recipe));
        managed.resolve(&Credential::bearer("bomtoon-access-token")).expect("initial resolution");
        let before = fs::read(directories.managed_state()).expect("state before refresh");
        now.store(700_000, Ordering::SeqCst);
        assert_eq!(managed.resolve(&Credential::bearer("bomtoon-access-token")), Err(TaskError::Unreachable));
        assert_eq!(fs::read(directories.managed_state()).expect("state after refresh"), before);
    }

    #[test]
    fn concurrent_resolves_rotate_one_refresh_token_once() {
        let directories = TestDirectories::new("serialized-refresh");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe.bootstrap.lock().expect("bootstrap queue").push_back(Ok(token_pair("a", 1_000_000)));
        recipe.refresh.lock().expect("refresh queue").push_back(Ok(token_pair("b", 2_000_000)));
        let (now, clock) = adjustable_clock(1);
        let managed = Arc::new(provider(&directories, clock, Arc::clone(&recipe)));
        managed.resolve(&Credential::bearer("bomtoon-access-token")).expect("initial resolution");
        now.store(800_000, Ordering::SeqCst);
        let barrier = Arc::new(Barrier::new(3));
        let threads: Vec<_> = (0..2).map(|_| { let managed = Arc::clone(&managed); let barrier = Arc::clone(&barrier); thread::spawn(move || { barrier.wait(); managed.resolve(&Credential::bearer("bomtoon-access-token")) }) }).collect();
        barrier.wait();
        for worker in threads {
            assert_eq!(worker.join().expect("resolution thread"), Ok(Some(ResolvedCredential { header_name: "Authorization".to_owned(), header_value: "Bearer access-b".to_owned() })));
        }
        assert_eq!(recipe.calls.lock().expect("calls").iter().filter(|call| **call == "refresh").count(), 1);
    }

    #[test]
    fn revoke_detaches_local_credentials_before_remote_io() {
        let directories = TestDirectories::new("revoke-order");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        let pair = token_pair("a", 1_000_000);
        recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .push_back(Ok(pair.clone()));
        recipe
            .revoke
            .lock()
            .expect("revoke queue")
            .push_back(Ok(()));
        let cookie = directories.cookie();
        let detached = directories.detached();
        let state = directories.managed_state();
        *recipe.revoke_observer.lock().expect("revoke observer") =
            Some(Box::new(move || {
                assert!(!cookie.exists());
                assert!(!detached.exists());
                assert!(!state.exists());
            }));
        let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));
        managed
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("initial resolution");

        assert_eq!(managed.revoke("bomtoon-access-token"), Ok(true));
        assert_eq!(managed.revoke("bomtoon-access-token"), Ok(true));
        assert_eq!(
            *recipe.revoke_inputs.lock().expect("revoke inputs"),
            vec![RevokeInput {
                cookie_fingerprint: fingerprint("cookie-a"),
                pair_fingerprint: pair_fingerprint(&pair),
            }]
        );
        assert!(!directories.cookie().exists());
        assert!(!directories.detached().exists());
        assert!(!directories.managed_state().exists());
    }

    #[test]
    fn remote_revoke_failure_keeps_resolution_disabled() {
        let directories = TestDirectories::new("revoke-uncertain");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe.bootstrap.lock().expect("bootstrap queue").push_back(Ok(token_pair("a", 1_000_000)));
        recipe.revoke.lock().expect("revoke queue").push_back(Err(TaskError::Unreachable));
        let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));
        managed.resolve(&Credential::bearer("bomtoon-access-token")).expect("initial resolution");
        assert_eq!(managed.revoke("bomtoon-access-token"), Err(TaskError::RevocationUnconfirmed));
        assert_eq!(managed.resolve(&Credential::bearer("bomtoon-access-token")), Err(TaskError::NoCredential));
        assert!(!directories.cookie().exists()); assert!(!directories.managed_state().exists());
    }

    #[test]
    fn startup_removes_a_cookie_left_in_the_detached_path() {
        let directories = TestDirectories::new("stale-detached");
        fs::write(directories.detached(), "cookie-a").expect("detached cookie");
        fs::write(directories.managed_state(), "cobalt-managed-v1\ndigest:cookie-a\n1000000\n4600000\naccess-a\nrefresh-a").expect("managed state");
        let recipe = Arc::new(FakeRecipe::default());
        let _managed = provider(&directories, Arc::new(|| 1), recipe);
        assert!(!directories.detached().exists()); assert!(!directories.managed_state().exists());
    }

    #[test]
    fn force_renew_refreshes_before_the_window() {
        let directories = TestDirectories::new("force-renew");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe.bootstrap.lock().expect("bootstrap queue").push_back(Ok(token_pair("a", 2_000_000)));
        recipe.refresh.lock().expect("refresh queue").push_back(Ok(token_pair("b", 3_000_000)));
        let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));
        managed.resolve(&Credential::bearer("bomtoon-access-token")).expect("initial resolution");
        assert_eq!(managed.force_renew(&Credential::bearer("bomtoon-access-token")), Ok(true));
        assert!(fs::read_to_string(directories.managed_state()).expect("managed state").contains("access-b"));
    }

    #[test]
    fn repeated_unauthorized_detaches_cookie_and_state() {
        let directories = TestDirectories::new("unauthorized");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe.bootstrap.lock().expect("bootstrap queue").extend([Ok(token_pair("a", 1_000_000)), Err(TaskError::Unauthorized)]);
        recipe.refresh.lock().expect("refresh queue").push_back(Err(TaskError::Unauthorized));
        let (now, clock) = adjustable_clock(1);
        let managed = provider(&directories, clock, Arc::clone(&recipe));
        managed.resolve(&Credential::bearer("bomtoon-access-token")).expect("initial resolution");
        now.store(800_000, Ordering::SeqCst);
        assert_eq!(managed.resolve(&Credential::bearer("bomtoon-access-token")), Err(TaskError::Unauthorized));
        assert!(!directories.cookie().exists()); assert!(!directories.detached().exists()); assert!(!directories.managed_state().exists());
    }

    #[test]
    fn fresh_provider_loads_matching_durable_state_without_bootstrap() {
        let directories = TestDirectories::new("durable-load");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let initial_recipe = Arc::new(FakeRecipe::default());
        initial_recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .push_back(Ok(token_pair("a", 1_000_000)));
        let managed = provider(
            &directories,
            Arc::new(|| 1),
            Arc::clone(&initial_recipe),
        );
        managed
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("initial resolution");
        drop(managed);

        let fresh_recipe = Arc::new(FakeRecipe::default());
        let fresh = provider(
            &directories,
            Arc::new(|| 1),
            Arc::clone(&fresh_recipe),
        );
        assert_eq!(
            fresh.resolve(&Credential::bearer("bomtoon-access-token")),
            Ok(Some(ResolvedCredential {
                header_name: "Authorization".to_owned(),
                header_value: "Bearer access-a".to_owned(),
            }))
        );
        assert!(fresh_recipe.calls.lock().expect("calls").is_empty());
    }

    #[test]
    fn malformed_durable_state_fails_closed_without_bootstrap() {
        let cases = [
            "cobalt-managed-v0\ndigest:cookie-a\n1000000\n4600000\naccess-a\nrefresh-a"
                .to_owned(),
            "cobalt-managed-v1\ndigest:cookie-a\n1000000\n4600000\naccess-\u{7}\nrefresh-a"
                .to_owned(),
            format!(
                "cobalt-managed-v1\ndigest:cookie-a\n1000000\n4600000\n{}\nrefresh-a",
                "x".repeat(MAX_STATE_BYTES)
            ),
        ];
        for (index, encoded) in cases.into_iter().enumerate() {
            let directories = TestDirectories::new(&format!("malformed-state-{index}"));
            fs::write(directories.cookie(), "cookie-a").expect("cookie");
            fs::write(directories.managed_state(), encoded).expect("managed state");
            let recipe = Arc::new(FakeRecipe::default());
            let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));

            assert_eq!(
                managed.resolve(&Credential::bearer("bomtoon-access-token")),
                Err(TaskError::LocalStorage)
            );
            assert!(recipe.calls.lock().expect("calls").is_empty());
        }
    }

    #[test]
    fn bootstrap_at_the_exact_window_is_persisted_then_refreshed() {
        let directories = TestDirectories::new("boundary-bootstrap");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        let now = 1_000_000;
        recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .push_back(Ok(token_pair("boundary", now + REFRESH_WINDOW_MS)));
        recipe
            .refresh
            .lock()
            .expect("refresh queue")
            .push_back(Ok(token_pair("rotated", now + 2 * REFRESH_WINDOW_MS)));
        let boundary_state = directories.managed_state();
        *recipe.refresh_observer.lock().expect("refresh observer") =
            Some(Box::new(move || {
                assert!(
                    fs::read_to_string(&boundary_state)
                        .expect("boundary state")
                        .contains("access-boundary")
                );
            }));
        let managed = provider(&directories, Arc::new(move || now), Arc::clone(&recipe));

        assert_eq!(
            managed.resolve(&Credential::bearer("bomtoon-access-token")),
            Ok(Some(ResolvedCredential {
                header_name: "Authorization".to_owned(),
                header_value: "Bearer access-rotated".to_owned(),
            }))
        );
        assert_eq!(
            *recipe.calls.lock().expect("calls"),
            vec!["bootstrap", "refresh"]
        );
        assert!(
            fs::read_to_string(directories.managed_state())
                .expect("rotated state")
                .contains("access-rotated")
        );
    }

    #[test]
    fn expired_refresh_and_fallback_outputs_preserve_the_valid_pair() {
        let directories = TestDirectories::new("expired-refresh");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .extend([
                Ok(token_pair("a", 2_000_000)),
                Ok(token_pair("fallback", 1_800_000)),
            ]);
        recipe
            .refresh
            .lock()
            .expect("refresh queue")
            .extend([
                Ok(token_pair("expired", 1_800_000)),
                Err(TaskError::Unauthorized),
            ]);
        let (now, clock) = adjustable_clock(1);
        let managed = provider(&directories, clock, Arc::clone(&recipe));
        managed
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("initial resolution");
        let durable = fs::read(directories.managed_state()).expect("durable state");
        now.store(1_800_000, Ordering::SeqCst);

        assert_eq!(
            managed.resolve(&Credential::bearer("bomtoon-access-token")),
            Err(TaskError::Unreachable)
        );
        assert_eq!(
            fs::read(directories.managed_state()).expect("state after refresh"),
            durable
        );
        assert_eq!(
            managed.resolve(&Credential::bearer("bomtoon-access-token")),
            Err(TaskError::Unreachable)
        );
        assert_eq!(
            fs::read(directories.managed_state()).expect("state after fallback"),
            durable
        );
        assert!(directories.cookie().exists());
    }

    #[test]
    fn durable_read_failure_fails_closed_without_bootstrap() {
        let directories = TestDirectories::new("read-fault");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let initial_recipe = Arc::new(FakeRecipe::default());
        initial_recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .push_back(Ok(token_pair("a", 1_000_000)));
        let initial = provider(
            &directories,
            Arc::new(|| 1),
            Arc::clone(&initial_recipe),
        );
        initial
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("initial resolution");
        drop(initial);

        let recipe = Arc::new(FakeRecipe::default());
        let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));
        inject_fault(FaultPoint::ReadState);
        assert_eq!(
            managed.resolve(&Credential::bearer("bomtoon-access-token")),
            Err(TaskError::LocalStorage)
        );
        assert!(directories.managed_state().exists());
        assert!(recipe.calls.lock().expect("calls").is_empty());
    }

    #[test]
    fn cookie_rename_failure_preserves_local_state_and_skips_remote_io() {
        let directories = TestDirectories::new("rename-fault");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .push_back(Ok(token_pair("a", 1_000_000)));
        let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));
        managed
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("initial resolution");

        inject_fault(FaultPoint::RenameCookie);
        assert_eq!(
            managed.revoke("bomtoon-access-token"),
            Err(TaskError::LocalStorage)
        );
        assert!(directories.cookie().exists());
        assert!(directories.managed_state().exists());
        assert!(!directories.detached().exists());
        assert!(recipe.revoke_inputs.lock().expect("revoke inputs").is_empty());
    }

    #[test]
    fn failed_state_removal_keeps_detached_marker_for_startup_retry() {
        let directories = TestDirectories::new("remove-fault");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .push_back(Ok(token_pair("a", 1_000_000)));
        let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));
        managed
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("initial resolution");

        inject_fault(FaultPoint::RemoveState);
        assert_eq!(
            managed.revoke("bomtoon-access-token"),
            Err(TaskError::LocalStorage)
        );
        assert!(!directories.cookie().exists());
        assert!(directories.detached().exists());
        assert!(directories.managed_state().exists());
        assert_eq!(
            managed.resolve(&Credential::bearer("bomtoon-access-token")),
            Err(TaskError::NoCredential)
        );
        drop(managed);

        inject_fault(FaultPoint::RemoveState);
        let failed_startup = ManagedCredentials::new(
            directories.secrets.clone(),
            directories.state.clone(),
            Arc::new(|| 1),
            Arc::new(FakeRecipe::default()),
        );
        assert!(matches!(failed_startup, Err(TaskError::LocalStorage)));
        assert!(directories.detached().exists());
        assert!(directories.managed_state().exists());

        let recovered = ManagedCredentials::new(
            directories.secrets.clone(),
            directories.state.clone(),
            Arc::new(|| 1),
            Arc::new(FakeRecipe::default()),
        );
        assert!(recovered.is_ok());
        assert!(!directories.detached().exists());
        assert!(!directories.managed_state().exists());
    }

    #[test]
    fn post_rename_sync_failure_invalidates_cache_and_loads_visible_generation() {
        let directories = TestDirectories::new("post-rename-sync");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .push_back(Ok(token_pair("a", 1_000_000)));
        recipe
            .refresh
            .lock()
            .expect("refresh queue")
            .push_back(Ok(token_pair("b", 2_000_000)));
        let (now, clock) = adjustable_clock(1);
        let managed = provider(&directories, clock, Arc::clone(&recipe));
        managed
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("initial resolution");
        now.store(800_000, Ordering::SeqCst);

        inject_fault(FaultPoint::SyncStateParentAfterRename);
        assert_eq!(
            managed.resolve(&Credential::bearer("bomtoon-access-token")),
            Err(TaskError::LocalStorage)
        );
        assert!(
            fs::read_to_string(directories.managed_state())
                .expect("visible generation")
                .contains("access-b")
        );
        assert_eq!(
            managed.resolve(&Credential::bearer("bomtoon-access-token")),
            Ok(Some(ResolvedCredential {
                header_name: "Authorization".to_owned(),
                header_value: "Bearer access-b".to_owned(),
            }))
        );
        assert_eq!(
            recipe
                .calls
                .lock()
                .expect("calls")
                .iter()
                .filter(|call| **call == "refresh")
                .count(),
            1
        );
    }

    #[test]
    fn remote_revoke_runs_after_the_provider_lock_is_released() {
        let directories = TestDirectories::new("revoke-lock-release");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .push_back(Ok(token_pair("a", 1_000_000)));
        recipe
            .revoke
            .lock()
            .expect("revoke queue")
            .push_back(Ok(()));
        let entered_remote = Arc::new(Barrier::new(2));
        let release_remote = Arc::new(Barrier::new(2));
        let observer_entered = Arc::clone(&entered_remote);
        let observer_release = Arc::clone(&release_remote);
        *recipe.revoke_observer.lock().expect("revoke observer") =
            Some(Box::new(move || {
                observer_entered.wait();
                observer_release.wait();
            }));
        let managed = Arc::new(provider(
            &directories,
            Arc::new(|| 1),
            Arc::clone(&recipe),
        ));
        managed
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("initial resolution");

        let revoking = Arc::clone(&managed);
        let revoke_thread =
            thread::spawn(move || revoking.revoke("bomtoon-access-token"));
        entered_remote.wait();
        let resolving = Arc::clone(&managed);
        let (resolved_tx, resolved_rx) = mpsc::channel();
        let resolve_thread = thread::spawn(move || {
            let result = resolving.resolve(&Credential::bearer("bomtoon-access-token"));
            resolved_tx.send(result.clone()).expect("resolution result");
            result
        });
        let while_remote_blocked = resolved_rx.recv_timeout(Duration::from_secs(1));
        release_remote.wait();

        assert_eq!(
            revoke_thread.join().expect("revoke thread"),
            Ok(true)
        );
        assert_eq!(
            resolve_thread.join().expect("resolve thread"),
            Err(TaskError::NoCredential)
        );
        assert_eq!(
            while_remote_blocked,
            Ok(Err(TaskError::NoCredential))
        );
    }

    #[test]
    fn network_refresh_failure_preserves_the_last_valid_pair() {
        let directories = TestDirectories::new("network-refresh");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .push_back(Ok(token_pair("a", 1_000_000)));
        recipe
            .refresh
            .lock()
            .expect("refresh queue")
            .push_back(Err(TaskError::Unreachable));
        let (now, clock) = adjustable_clock(1);
        let managed = provider(&directories, clock, Arc::clone(&recipe));
        managed
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("initial resolution");
        let durable = fs::read(directories.managed_state()).expect("durable state");
        now.store(800_000, Ordering::SeqCst);

        assert_eq!(
            managed.resolve(&Credential::bearer("bomtoon-access-token")),
            Err(TaskError::Unreachable)
        );
        assert_eq!(
            fs::read(directories.managed_state()).expect("state after network failure"),
            durable
        );
        assert!(directories.cookie().exists());
    }

    #[test]
    fn cookie_directory_sync_failure_keeps_marker_and_skips_remote_revoke() {
        let directories = TestDirectories::new("cookie-sync-fault");
        fs::write(directories.cookie(), "cookie-a").expect("cookie");
        let recipe = Arc::new(FakeRecipe::default());
        recipe
            .bootstrap
            .lock()
            .expect("bootstrap queue")
            .push_back(Ok(token_pair("a", 1_000_000)));
        recipe
            .revoke
            .lock()
            .expect("revoke queue")
            .push_back(Ok(()));
        let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));
        managed
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("initial resolution");

        inject_fault(FaultPoint::SyncSecretsAfterRename);
        assert_eq!(
            managed.revoke("bomtoon-access-token"),
            Err(TaskError::LocalStorage)
        );
        assert!(!directories.cookie().exists());
        assert!(directories.detached().exists());
        assert!(!directories.managed_state().exists());
        assert!(recipe.revoke_inputs.lock().expect("revoke inputs").is_empty());
        drop(managed);

        let recovered = provider(
            &directories,
            Arc::new(|| 1),
            Arc::new(FakeRecipe::default()),
        );
        assert_eq!(
            recovered.resolve(&Credential::bearer("bomtoon-access-token")),
            Err(TaskError::NoCredential)
        );
        assert!(!directories.detached().exists());
        assert!(!directories.managed_state().exists());
    }
}
