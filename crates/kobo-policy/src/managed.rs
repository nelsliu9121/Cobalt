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
        if !valid_pair(&pair) {
            return Err(TaskError::Unreachable);
        }
        let bound = BoundPair {
            cookie_digest: digest.to_owned(),
            pair,
        };
        write_bound_pair(&self.state_path(), &bound)?;
        inner.cached = Some(bound);
        Ok(())
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
        let renamed = match fs::rename(&cookie, &detached) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(_) => return Err(TaskError::LocalStorage),
        };

        let mut failed = renamed && sync_directory(&self.secrets).is_err();
        if remove_file_synced(&self.state_path()).is_err() {
            failed = true;
        }
        inner.cached = None;
        if renamed && remove_file_synced(&detached).is_err() {
            failed = true;
        }
        if failed {
            Err(TaskError::LocalStorage)
        } else {
            Ok(())
        }
    }

    fn remove_stale_detached(&self) -> Result<(), TaskError> {
        let detached = self.detached_path();
        if remove_file_synced(&detached)? {
            remove_file_synced(&self.state_path())?;
        }
        Ok(())
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

fn write_bound_pair(path: &Path, bound: &BoundPair) -> Result<(), TaskError> {
    if !valid_secret_field(&bound.cookie_digest) || !valid_pair(&bound.pair) {
        return Err(TaskError::LocalStorage);
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
        return Err(TaskError::LocalStorage);
    }

    let (temporary, mut file) = create_temporary(path)?;
    let write_result = (|| {
        file.write_all(encoded.as_bytes())
            .map_err(|_| TaskError::LocalStorage)?;
        file.sync_all().map_err(|_| TaskError::LocalStorage)?;
        drop(file);
        fs::rename(&temporary, path).map_err(|_| TaskError::LocalStorage)?;
        sync_parent(path)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
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
    use std::collections::VecDeque;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Barrier;
    use std::thread;

    type RevokeObserver = dyn Fn() + Send + Sync;

    #[derive(Default)]
    struct FakeRecipe {
        calls: Mutex<Vec<&'static str>>,
        bootstrap: Mutex<VecDeque<Result<ManagedTokenPair, TaskError>>>,
        refresh: Mutex<VecDeque<Result<ManagedTokenPair, TaskError>>>,
        revoke: Mutex<VecDeque<Result<(), TaskError>>>,
        revoke_observer: Mutex<Option<Box<RevokeObserver>>>,
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
            self.refresh.lock().expect("refresh queue").pop_front().expect("refresh result")
        }

        fn revoke(&self, _binding_secret: &str, _pair: &ManagedTokenPair) -> Result<(), TaskError> {
            self.calls.lock().expect("calls").push("revoke");
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
        recipe.bootstrap.lock().expect("bootstrap queue").push_back(Ok(token_pair("a", 1_000_000)));
        recipe.revoke.lock().expect("revoke queue").push_back(Ok(()));
        let cookie = directories.cookie(); let detached = directories.detached(); let state = directories.managed_state();
        *recipe.revoke_observer.lock().expect("revoke observer") = Some(Box::new(move || { assert!(!cookie.exists()); assert!(!detached.exists()); assert!(!state.exists()); }));
        let managed = provider(&directories, Arc::new(|| 1), Arc::clone(&recipe));
        managed.resolve(&Credential::bearer("bomtoon-access-token")).expect("initial resolution");
        assert_eq!(managed.revoke("bomtoon-access-token"), Ok(true));
        assert!(!directories.cookie().exists()); assert!(!directories.detached().exists()); assert!(!directories.managed_state().exists());
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
}
