# BOMTOON Login Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add macOS Chrome login provisioning, runtime-owned BOMTOON token bootstrap and refresh, bearer-only content loading, and in-app logout with simulator-only acceptance checks.

**Architecture:** The CLI installs only the NextAuth session cookie. A generic managed-credential provider in `kobo-policy` owns token state, locking, expiry, retry, and local revocation; the BOMTOON recipe in `kobo-net` owns exact HTTP requests and strict JSON parsing. The Bomtoon app names only `bomtoon-access-token`, receives no authentication response, and revokes through a closed protocol task.

**Tech Stack:** Rust 2021, Rust 1.85.1, existing `kobo-json`, existing zero-dependency SHA-256, Chrome DevTools Protocol over `--remote-debugging-pipe`, Cobalt task protocol, `AppRunner`, `CLARA_BW_METRICS`.

## Global Constraints

- Hosts: macOS only; browsers: Google Chrome and Chromium only.
- Production provisioning uses `kobo bomtoon login --device <IP>`; development uses `kobo bomtoon login --sim`.
- Every test, trial, screenshot, and attended check uses browser or runtime simulators. No command may connect to a physical Kobo during verification.
- The application may never call `/api/auth/session`, send `bomtoon-session`, receive authenticated detail HTML, read a refresh token, or receive token-bearing response bytes.
- The runtime rechecks the SHA-256 session-cookie digest on every managed resolve and refreshes within five minutes of access-token expiry.
- A 401 or 403 permits one forced renewal and one request retry. Network errors, malformed responses, 404, and 5xx responses do not trigger authentication retry.
- Logout invalidates local credentials before remote network I/O. Remote failure leaves the account locally signed out.
- Session tokens are `user.accessToken` and `user.refreshToken`; refresh tokens are `result.accessToken` and `result.refreshToken`. Their expiry fields are integer epoch milliseconds.
- Content detail is bearer-only `GET /api/balcony-api-v2/contents/<alias>?isNotLoginAdult=false&isPorch=false`; its episodes are `data.episodes`, and `purchaseStatus` may be a string or `null`.
- Credentialed GET, POST, and PUT requests never follow redirects or forward credentials to a `Location`.
- Secret values never enter app memory, protocol outcomes, logs, terminal output, process arguments, shell history, errors, screenshots, simulator traces, or snapshots.
- Temporary Chrome profiles use mode `0700`; named credentials and managed state use mode `0600` with atomic replacement.
- Unsafe Rust remains forbidden. Match rustfmt and the workspace's 100-column style.
- Fixtures contain stable redactions only. Never copy values from `request-hars/` into source, tests, snapshots, or commits.

## File Map

- Create `crates/kobo-policy/src/managed.rs`: generic managed token state, recipe interface, locking, expiry, atomic storage, cookie binding, and revocation ordering.
- Modify `crates/kobo-policy/src/lib.rs`: export managed-credential types.
- Modify `crates/kobo-policy/src/tasks.rs`: method-aware credential policy, separate fetch credentials, managed resolution, one authentication retry, and revoke task execution.
- Modify `crates/kobo-protocol/src/lib.rs`: revoke task and typed logout errors on the wire.
- Modify `crates/kobo-sdk/src/lib.rs`: user-facing failure descriptions for the new task errors.
- Modify `crates/kobod/src/app_link.rs`, `crates/kobod/src/app_store.rs`, `crates/kobod/src/device.rs`, and `crates/kobod/src/update.rs`: exhaustive mappings for the new task errors and device runtime provider wiring.
- Create `crates/kobo-net/src/bomtoon.rs`: strict BOMTOON session, IP, refresh, and logout recipes.
- Modify `crates/kobo-net/src/lib.rs`: credential-separated GET transport, redirect refusal, PUT transport, method-aware allowlist, exact content URL validation, and broker export.
- Modify `crates/kobo-net/Cargo.toml`: add path dependencies on `kobo-json` and `kobo-policy`.
- Modify `crates/kobo-sim/src/lib.rs`: shared simulator auth paths and provider wiring.
- Create `crates/kobo-cli/src/bomtoon.rs`: CLI parsing, Chrome discovery, CDP pipe, cookie selection, validation, target installation, and cleanup.
- Modify `crates/kobo-cli/src/main.rs`: command dispatch and help.
- Modify `apps/bomtoon/src/api.rs`: bearer content request and revoke task; remove session/detail-cookie requests.
- Modify `apps/bomtoon/src/parse.rs`: remove session/HTML parsing and parse `data.episodes` JSON.
- Modify `apps/bomtoon/src/model.rs`: interpret nullable purchase status.
- Modify `apps/bomtoon/src/main.rs`: direct library startup, signed-out states, Sign out, warning handling, and data clearing.

---

### Task 1: Extend the task protocol for managed revocation

**Files:**
- Modify: `crates/kobo-protocol/src/lib.rs:341-540,1690-1760,2000-2090,3680-3760,8168-8205`
- Modify: `crates/kobo-sdk/src/lib.rs:270-335`
- Modify: `crates/kobod/src/app_link.rs:230-241`
- Modify: `crates/kobo-policy/src/tasks.rs:318-365,495-615`
- Modify: `crates/kobod/src/app_store.rs:877-890`
- Modify: `crates/kobod/src/device.rs:1159-1183`
- Modify: `crates/kobod/src/update.rs:49-61`

**Interfaces:**
- Consumes: existing `Task`, `TaskOutcome`, `TaskError`, and append-only protocol tags.
- Produces: `Task::RevokeCredential { credential: String }`, `TaskError::LocalStorage`, and `TaskError::RevocationUnconfirmed`.

- [ ] **Step 1: Write failing protocol round-trip and validation tests**

Add tests beside the current task codec tests:

```rust
#[test]
fn managed_credential_revocation_round_trips() {
    let message = Message::Spawn {
        task: TaskId(41),
        work: Task::RevokeCredential {
            credential: "bomtoon-access-token".to_owned(),
        },
    };
    let encoded = encode(&Frame {
        request_id: 9,
        message: message.clone(),
    })
    .expect("encode revoke");
    assert_eq!(decode(&encoded).expect("decode revoke").message, message);
}

#[test]
fn revoke_refuses_an_invalid_credential_name() {
    let work = Task::RevokeCredential {
        credential: "bad credential".to_owned(),
    };
    assert!(!work.is_sendable());
}


#[test]
fn logout_errors_keep_append_only_wire_tags() {
    assert_eq!(encode_task_error(TaskError::LocalStorage), 8);
    assert_eq!(encode_task_error(TaskError::RevocationUnconfirmed), 9);
    assert_eq!(decode_task_error(8), Ok(TaskError::LocalStorage));
    assert_eq!(decode_task_error(9), Ok(TaskError::RevocationUnconfirmed));
}
```

Add this policy test beside the existing runner tests:

```rust
#[test]
fn revoke_is_fail_closed_before_the_managed_provider_exists() {
    let mut runner = TaskRunner::simulated(temp_root("revoke-closed"))
        .with_capabilities([Capability::Network]);
    runner
        .submit(
            TaskId(1),
            Task::RevokeCredential {
                credential: "bomtoon-access-token".to_owned(),
            },
        )
        .expect("admit revoke");
    assert_eq!(
        runner.wait_for(Duration::from_secs(1)).unwrap().outcome,
        TaskOutcome::Failed(TaskError::Denied)
    );
}
```

- [ ] **Step 2: Run the focused protocol tests and confirm failure**

Run:

```bash
rtk cargo test -p kobo-protocol managed_credential_revocation_round_trips
rtk cargo test -p kobo-protocol logout_errors_keep_append_only_wire_tags
```

Expected: compilation fails because the revoke task and error variants do not exist.

- [ ] **Step 3: Add the closed revoke task and typed errors**

Add the variant and validation:

```rust
pub enum Task {
    Fetch {
        url: String,
        offset: u32,
        max_bytes: u32,
        credential: Option<Credential>,
        headers: Vec<Header>,
    },
    Post {
        url: String,
        body: String,
        content_type: String,
        credential: Option<Credential>,
        headers: Vec<Header>,
        max_bytes: u32,
    },
    RevokeCredential {
        credential: String,
    },
    ReadFile {
        path: String,
    },
    Sleep {
        seconds: u32,
    },
}
```

Use the existing secret-name grammar for the new branch:

```rust
Self::RevokeCredential { credential } => {
    !credential.is_empty()
        && credential.len() <= MAX_HEADER_NAME
        && credential
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
```

Append these errors without renumbering tags 0 through 7:

```rust
pub enum TaskError {
    Denied,
    Unreachable,
    TooLarge,
    TimedOut,
    NotFound,
    Offline,
    NoCredential,
    Unauthorized,
    LocalStorage,
    RevocationUnconfirmed,
}
```

Encode `RevokeCredential` with task tag `4`, containing one bounded string. Decode tag `4` into the same variant. Add its encoded length as `encoded_string_len(credential)`. Encode the new errors as tags `8` and `9`; update the decoder and `Display`:

```rust
Self::LocalStorage => "the runtime could not update local credential storage",
Self::RevocationUnconfirmed => "the account is signed out locally but remote revocation was not confirmed",
```

Neither new error is retryable in `TaskError::worth_retrying`.

Keep `kobo-policy` exhaustive and fail closed until Task 3 installs the managed provider:

```rust
let required = match &work {
    Task::Fetch { .. } | Task::Post { .. } | Task::RevokeCredential { .. } => {
        Some(Capability::Network)
    }
    Task::ReadFile { .. } | Task::Sleep { .. } => None,
};
```

Add this temporary execution arm:

```rust
Task::RevokeCredential { .. } => TaskOutcome::Failed(TaskError::Denied),
```

Task 3 replaces that arm with provider-backed revocation.

- [ ] **Step 4: Update exhaustive application-facing mappings**

Add SDK failures:

```rust
TaskError::LocalStorage => Self {
    state: StandardState::Error,
    advice: "This reader could not remove the local sign-in data.",
    retryable: false,
},
TaskError::RevocationUnconfirmed => Self {
    state: StandardState::Error,
    advice: "Signed out here, but BOMTOON did not confirm remote sign-out.",
    retryable: false,
},
```

In non-BOMTOON `DeviceError` conversions, map `LocalStorage` to the existing backend or invalid-input category and `RevocationUnconfirmed` to the existing unreachable category. Use explicit arms rather than wildcards so future `TaskError` additions remain compiler-visible.

- [ ] **Step 5: Run protocol, SDK, policy, and workspace checks**

Run:

```bash
rtk cargo test -p kobo-protocol
rtk cargo test -p kobo-sdk
rtk cargo test -p kobo-policy revoke_is_fail_closed_before_the_managed_provider_exists
rtk cargo check --workspace --all-targets
```

Expected: protocol, SDK, and fail-closed policy tests pass. The workspace check has no errors; pre-existing warnings remain.

- [ ] **Step 6: Commit**

```bash
rtk git add crates/kobo-protocol/src/lib.rs crates/kobo-sdk/src/lib.rs crates/kobo-policy/src/tasks.rs crates/kobod/src/app_link.rs crates/kobod/src/app_store.rs crates/kobod/src/device.rs crates/kobod/src/update.rs
rtk git commit -m "feat(protocol): add credential revocation task"
```

### Task 2: Add generic managed credential state

**Files:**
- Create: `crates/kobo-policy/src/managed.rs`
- Modify: `crates/kobo-policy/src/lib.rs`

**Interfaces:**
- Consumes: `Credential`, `TaskError`, file-backed session secrets, and a millisecond clock.
- Produces: `ManagedTokenPair`, `ManagedCredentialRecipe`, `ManagedCredentials`, `ResolvedCredential`, and `managed_state_path`.

- [ ] **Step 1: Write failing provider tests**

The test recipe must record calls without retaining secret-bearing response bodies:

```rust
#[derive(Default)]
struct FakeRecipe {
    calls: Mutex<Vec<&'static str>>,
    bootstrap: Mutex<VecDeque<Result<ManagedTokenPair, TaskError>>>,
    refresh: Mutex<VecDeque<Result<ManagedTokenPair, TaskError>>>,
    revoke: Mutex<VecDeque<Result<(), TaskError>>>,
}

impl ManagedCredentialRecipe for FakeRecipe {
    fn credential_name(&self) -> &'static str {
        "bomtoon-access-token"
    }

    fn binding_secret_name(&self) -> &'static str {
        "bomtoon-session"
    }

    fn binding_digest(&self, secret: &str) -> String {
        format!("digest:{secret}")
    }

    fn bootstrap(&self, _binding_secret: &str) -> Result<ManagedTokenPair, TaskError> {
        self.calls.lock().expect("calls").push("bootstrap");
        self.bootstrap
            .lock()
            .expect("bootstrap queue")
            .pop_front()
            .expect("bootstrap result")
    }

    fn refresh(&self, _binding_secret: &str, _pair: &ManagedTokenPair) -> Result<ManagedTokenPair, TaskError> {
        self.calls.lock().expect("calls").push("refresh");
        self.refresh
            .lock()
            .expect("refresh queue")
            .pop_front()
            .expect("refresh result")
    }

    fn revoke(&self, _binding_secret: &str, _pair: &ManagedTokenPair) -> Result<(), TaskError> {
        self.calls.lock().expect("calls").push("revoke");
        self.revoke
            .lock()
            .expect("revoke queue")
            .pop_front()
            .expect("revoke result")
    }
}
```

Cover these observable cases with separate tests:

Write separate tests named:

- `missing_state_bootstraps_and_persists_a_cookie_bound_pair`
- `replacing_the_cookie_invalidates_cached_and_durable_tokens`
- `resolve_refreshes_inside_the_five_minute_window`
- `malformed_refresh_preserves_the_last_valid_pair`
- `concurrent_resolves_rotate_one_refresh_token_once`
- `revoke_detaches_local_credentials_before_remote_io`
- `remote_revoke_failure_keeps_resolution_disabled`
- `startup_removes_a_cookie_left_in_the_detached_path`

Use redacted values such as `access-a`, `refresh-a`, and `cookie-a`; no fixture may resemble a JWT.

- [ ] **Step 2: Run the provider tests and confirm failure**

Run:

```bash
rtk cargo test -p kobo-policy managed::tests
```

Expected: compilation fails because `managed` and its interfaces do not exist.

- [ ] **Step 3: Add the provider interfaces**

Create `managed.rs` with these public types:

```rust
use kobo_protocol::{Credential, SecretHeader, TaskError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const REFRESH_WINDOW_MS: u64 = 5 * 60 * 1000;
pub type Clock = dyn Fn() -> u64 + Send + Sync;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedTokenPair {
    pub access_token: String,
    pub access_expires_at_ms: u64,
    pub refresh_token: String,
    pub refresh_expires_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCredential {
    pub header_name: String,
    pub header_value: String,
}

pub trait ManagedCredentialRecipe: Send + Sync {
    fn credential_name(&self) -> &'static str;
    fn binding_secret_name(&self) -> &'static str;
    fn binding_digest(&self, secret: &str) -> String;
    fn bootstrap(&self, binding_secret: &str) -> Result<ManagedTokenPair, TaskError>;
    fn refresh(&self, binding_secret: &str, pair: &ManagedTokenPair) -> Result<ManagedTokenPair, TaskError>;
    fn revoke(&self, binding_secret: &str, pair: &ManagedTokenPair) -> Result<(), TaskError>;
}

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

#[must_use]
pub fn managed_state_path(root: &Path, credential: &str) -> PathBuf {
    root.join(format!("{credential}.state"))
}
```

Expose these methods:

```rust
impl ManagedCredentials {
    pub fn new(
        secrets: impl Into<PathBuf>,
        state: impl Into<PathBuf>,
        clock: Arc<Clock>,
        recipe: Arc<dyn ManagedCredentialRecipe>,
    ) -> Result<Self, TaskError>;

    pub fn resolve(
        &self,
        wanted: &Credential,
    ) -> Result<Option<ResolvedCredential>, TaskError>;

    pub fn force_renew(&self, wanted: &Credential) -> Result<bool, TaskError>;

    pub fn revoke(&self, credential: &str) -> Result<bool, TaskError>;
}
```

`resolve` returns `Ok(None)` only when the recipe does not manage the credential name. A matching name with a non-Bearer header returns `Denied`. A missing binding secret returns `NoCredential`.

- [ ] **Step 4: Implement bounded state and atomic replacement**

Use this versioned six-line format, rejecting control characters in every secret field:

```text
cobalt-managed-v1
<cookie-digest>
<access-expiry-ms>
<refresh-expiry-ms>
<access-token>
<refresh-token>
```

Write with `OpenOptionsExt::mode(0o600)`, `write_all`, `sync_all`, and `rename`. Sync the parent directory after rename. Read through a byte ceiling of `2 * MAX_SECRET_BYTES + 1024`. Never include path contents in errors.

`resolve` must execute under `inner`'s mutex in this order:

1. Read the current binding secret.
2. Compute its digest.
3. Drop cached and durable state if the digest differs.
4. Load matching durable state when no cache exists.
5. Bootstrap when no bound pair exists.
6. Refresh when `access_expires_at_ms <= now + REFRESH_WINDOW_MS`.
7. Replace durable state only after a complete pair validates.
8. Return `Authorization: Bearer <access token>`.

On refresh `Unauthorized`, run one cookie bootstrap. If bootstrap is also `Unauthorized`, detach and remove the cookie, clear cache/state, and return `Unauthorized`. Other refresh/bootstrap failures preserve the last valid pair.

- [ ] **Step 5: Implement local-first revocation**

Use fixed paths under the provider lock:

```rust
let cookie = self.secrets.join(self.recipe.binding_secret_name());
let detached = self
    .secrets
    .join(format!(".{}.revoking", self.recipe.binding_secret_name()));
let state = managed_state_path(&self.state, self.recipe.credential_name());
```

Required ordering:

1. Copy the current session cookie and pair into one local `Option<(String, ManagedTokenPair)>`.
2. Atomically rename the cookie to `detached`.
3. Remove durable state and clear the cache.
4. Delete `detached` before releasing the provider lock.
5. Release the lock.
6. Call `recipe.revoke` with the one-shot cookie and pair.
7. Return `RevocationUnconfirmed` for remote failure after local invalidation.

If the initial rename fails for a reason other than `NotFound`, return `LocalStorage` and do not call the recipe. On construction, remove a stale detached cookie before serving any resolution.

- [ ] **Step 6: Export and run provider tests**

Add to `crates/kobo-policy/src/lib.rs`:

```rust
mod managed;

pub use managed::{
    managed_state_path, Clock, ManagedCredentialRecipe, ManagedCredentials, ManagedTokenPair,
    ResolvedCredential, REFRESH_WINDOW_MS,
};
```

Run:

```bash
rtk cargo test -p kobo-policy managed::tests
```

Expected: all binding, expiry, serialization, atomic replacement, concurrency, and logout-order tests pass.

- [ ] **Step 7: Commit**

```bash
rtk git add crates/kobo-policy/src/lib.rs crates/kobo-policy/src/managed.rs
rtk git commit -m "feat(policy): manage runtime credentials"
```

### Task 3: Integrate managed credentials into the task runner

**Files:**
- Modify: `crates/kobo-policy/src/tasks.rs:60-205,224-380,495-630,887-1160`

**Interfaces:**
- Consumes: `Arc<ManagedCredentials>` and `Task::RevokeCredential` from Tasks 1 and 2.
- Produces: method-aware `CredentialAuthorizer`, credential-separated `Fetcher`, and `TaskRunner::with_managed_credentials`.

- [ ] **Step 1: Write failing runner tests**

Add these tests with a fake managed provider and fake fetch/post backends:

Write separate tests named:

- `credential_policy_receives_get_or_post_method`
- `fetch_passes_the_runtime_credential_separately_from_app_headers`
- `an_app_cannot_resolve_a_managed_credential_before_policy_allows_it`
- `an_unauthorized_managed_fetch_renews_and_retries_once`
- `a_second_unauthorized_result_is_returned_without_a_third_request`
- `network_and_not_found_errors_do_not_renew_managed_credentials`
- `revoke_rejects_an_unregistered_managed_credential`
- `revoke_returns_only_an_empty_completion_body`

- [ ] **Step 2: Run the focused runner tests and confirm failure**

Run:

```bash
rtk cargo test -p kobo-policy tasks::tests::an_unauthorized_managed_fetch_renews_and_retries_once
```

Expected: compilation fails because the runner has no managed provider or method-aware policy.

- [ ] **Step 3: Make credential authorization method-aware**

Add:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestMethod {
    Get,
    Post,
}

pub type CredentialAuthorizer =
    dyn Fn(&Credential, RequestMethod, &str) -> bool + Send + Sync;
```

Pass `RequestMethod::Get` for `Task::Fetch` and `RequestMethod::Post` for `Task::Post`. Policy authorization remains the first credential step, before managed or file-backed resolution.

Keep application headers and runtime credentials distinct at the network boundary:

```rust
pub type Fetcher = dyn Fn(
        &str,
        u32,
        u32,
        Option<(&str, &str)>,
        &[(&str, &str)],
    ) -> Result<Vec<u8>, TaskError>
    + Send
    + Sync;
```

Never append a resolved credential to the application header vector.

- [ ] **Step 4: Add provider wiring and managed-first resolution**

Extend `TaskRunner` and `Backends` with `Option<Arc<ManagedCredentials>>`. Add:

```rust
#[must_use]
pub fn with_managed_credentials(mut self, managed: Arc<ManagedCredentials>) -> Self {
    self.managed = Some(managed);
    self
}
```

Change credential resolution to return whether the result was managed:

```rust
struct ResolvedHeader {
    name: String,
    value: String,
    managed: bool,
}
```

After policy approval, call `managed.resolve(wanted)`. Use its result when it returns `Some`; otherwise use the current file-backed secret resolver unchanged. Pass the resolved header through `Fetcher` or `Poster`'s separate credential argument. Application headers remain unchanged and non-secret.

- [ ] **Step 5: Add one authentication retry**

For fetch and post, call the backend once. Only when all of these are true may the runner call `force_renew` and repeat the original request:

```rust
matches!(first_result, Err(TaskError::Unauthorized))
    && resolved.as_ref().is_some_and(|value| value.managed)
```

Resolve the header again after renewal so the retry cannot reuse the rejected token. Return the second backend result directly. Do not route this internal retry through the SDK's generic delayed retry mechanism.

- [ ] **Step 6: Execute revoke tasks**

Treat `Task::RevokeCredential` as requiring `Capability::Network`. Execute:

```rust
Task::RevokeCredential { credential } => match managed {
    Some(provider) => match provider.revoke(credential) {
        Ok(true) => TaskOutcome::Completed(Vec::new()),
        Ok(false) => TaskOutcome::Failed(TaskError::Denied),
        Err(error) => TaskOutcome::Failed(error),
    },
    None => TaskOutcome::Failed(TaskError::Denied),
}
```

No provider value, token, body, or metadata enters the completion bytes.

- [ ] **Step 7: Run task-runner tests**

Run:

```bash
rtk cargo test -p kobo-policy tasks::tests
```

Expected: existing file-backed credential tests remain green; new method, retry, and revoke tests pass.

- [ ] **Step 8: Commit**

```bash
rtk git add crates/kobo-policy/src/tasks.rs crates/kobo-policy/src/lib.rs
rtk git commit -m "feat(policy): resolve managed task credentials"
```

### Task 4: Implement the strict BOMTOON broker and allowlist

**Files:**
- Create: `crates/kobo-net/src/bomtoon.rs`
- Modify: `crates/kobo-net/src/lib.rs:29-32,206-301,625-725,1001-1034,1200-1340`
- Modify: `crates/kobo-net/Cargo.toml`

**Interfaces:**
- Consumes: `ManagedCredentialRecipe`, `ManagedTokenPair`, `RequestMethod`, `kobo_json::Value`, `fetch_from`, `post`, and the new `put` transport.
- Produces: `bomtoon::Recipe`, `validate_session_cookie`, BOMTOON credential/path constants, strict parsers, and method-aware `credential_allowed`.

- [ ] **Step 1: Write failing broker and allowlist tests**

Use redacted JSON fixtures with the observed shapes:

```rust
const SESSION: &str = r#"{
  "user": {
    "id": "USER_ID_A",
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
```

Add tests:

Write separate tests named:

- `session_tokens_are_read_only_beneath_user`
- `refresh_tokens_are_read_only_beneath_result`
- `partial_or_oversized_token_pairs_are_rejected`
- `refresh_sends_cookie_ip_and_refresh_token_with_access_bearer`
- `logout_sends_cookie_and_bearer_with_put_without_redirects`
- `credentialed_get_rejects_relative_and_absolute_redirects_without_a_second_request`
- `bomtoon_session_is_denied_to_every_application_task`
- `authenticated_next_data_and_detail_html_are_denied`
- `content_json_requires_get_bearer_exact_alias_and_query`

- [ ] **Step 2: Run focused network tests and confirm failure**

Run:

```bash
rtk cargo test -p kobo-net bomtoon
```

Expected: compilation fails because the broker and method-aware allowlist do not exist.

- [ ] **Step 3: Add JSON body PUT support without redirects**

Extend the private HTTP method:

```rust
enum Method<'a> {
    Get {
        offset: Option<u32>,
        credential: Option<(&'a str, &'a str)>,
        headers: &'a [(&'a str, &'a str)],
    },
    Post {
        body: &'a [u8],
        content_type: &'a str,
        credential: Option<(&'a str, &'a str)>,
        headers: &'a [(&'a str, &'a str)],
    },
    Put {
        body: &'a [u8],
        content_type: &'a str,
        credential: Option<(&'a str, &'a str)>,
        headers: &'a [(&'a str, &'a str)],
    },
}
```

`Method::verb` returns `PUT` for the new branch. Change `fetch_from` to accept the runtime credential separately from application headers. Uncredentialed GET retains bounded redirect behavior. A credentialed GET returns `TaskError::Denied` on the first relative or absolute redirect without issuing a request to the target. Reuse one private body-sending function for POST and PUT; both issue one request and reject every redirect with `TaskError::Denied`. Export `put` with the same parameters as `post`.

- [ ] **Step 4: Add the broker contract**

Create `bomtoon.rs` with:

```rust
pub const ACCESS_CREDENTIAL: &str = "bomtoon-access-token";
pub const SESSION_SECRET: &str = "bomtoon-session";
pub const SESSION_COOKIE_MAX_BYTES: usize = kobo_policy::tasks::MAX_SECRET_BYTES;
pub const SESSION_URL: &str = "https://www.bomtoon.tw/api/auth/session";
pub const IP_URL: &str = "https://www.bomtoon.tw/api/balcony/ip";
pub const REFRESH_URL: &str = "https://www.bomtoon.tw/api/balcony/auth/refresh";
pub const LOGOUT_URL: &str = "https://www.bomtoon.tw/api/balcony-api/auth/logout";

pub trait Transport: Send + Sync {
    fn get(
        &self,
        url: &str,
        credential: Option<(&str, &str)>,
        headers: &[(&str, &str)],
        max_bytes: u32,
    ) -> Result<Vec<u8>, TaskError>;

    fn post_json(
        &self,
        url: &str,
        body: &[u8],
        bearer: &str,
        headers: &[(&str, &str)],
        max_bytes: u32,
    ) -> Result<Vec<u8>, TaskError>;

    fn put_json(
        &self,
        url: &str,
        body: &[u8],
        bearer: &str,
        headers: &[(&str, &str)],
        max_bytes: u32,
    ) -> Result<Vec<u8>, TaskError>;
}

pub struct Recipe {
    transport: Arc<dyn Transport>,
}

impl Recipe {
    #[must_use]
    pub fn live() -> Self;

    #[cfg(test)]
    fn with_transport(transport: Arc<dyn Transport>) -> Self;
}

pub fn validate_session_cookie(cookie_header: &str) -> Result<String, TaskError>;
```

`LiveTransport` delegates to `fetch_from`, `post`, and `put`. Fixed BOMTOON headers are `Accept: application/json`, `x-balcony-id: BOMTOON_TW`, `x-balcony-timezone: Asia/Taipei`, and `x-platform: MOBILE_IOS`.

- [ ] **Step 5: Parse and validate exact response shapes**

Session parsing reads `user.accessToken` and `user.refreshToken`. Refresh parsing reads `result.accessToken` and `result.refreshToken`. Both require non-empty token text at or below `MAX_SECRET_BYTES`, no ASCII control characters, integer `createdAt`/`expiredAt`, and `expiredAt > createdAt`.

IP parsing accepts only one non-empty `ipAddress` string under a small response ceiling. JSON body construction must escape string values and produce exactly:

```json
{"refreshToken":"REFRESH_A","clientIp":"203.0.113.1"}
```

Logout produces exactly:

```json
{"refreshToken":"REFRESH_A"}
```

`Recipe::binding_digest` delegates to `crate::sha256::hex_digest`. `bootstrap` uses the binding as its only credential. `refresh` fetches IP with the session cookie, then posts with both the session cookie and access bearer before parsing the rotated pair. `revoke` sends PUT with both credentials and accepts only a success response with a string `result` and object `data`. `validate_session_cookie` fetches the bounded session with only the supplied cookie, parses the same strict pair, returns SHA-256 over access token, one NUL byte, and refresh token, and drops the pair before returning. Every broker GET supplies cookies through `Transport::get`'s credential argument; redirect tests prove the cookie or bearer never reaches a second request.

- [ ] **Step 6: Tighten app credential policy**

Change the public function to:

```rust
#[must_use]
pub fn credential_allowed(
    app: &str,
    credential: &Credential,
    method: RequestMethod,
    url: &str,
) -> bool;
```

For `app == "bomtoon"`, deny `bomtoon-session` unconditionally. Permit `bomtoon-access-token` only with `SecretHeader::Bearer`, `RequestMethod::Get`, HTTPS host `www.bomtoon.tw`, port 443, and one of:

- Existing exact library URL grammar.
- Existing exact recent URL grammar.
- `/api/balcony-api-v2/contents/<alias>?isNotLoginAdult=false&isPorch=false`, where alias is non-empty ASCII alphanumeric, underscore, or hyphen.

Reject reordered, added, missing, or duplicate query fields. Reject `/detail/*`, `/_next/data/*`, `/api/auth/session`, POST, and alternate ports.

- [ ] **Step 7: Add dependencies and run tests**

Add:

```toml
kobo-json = { path = "../kobo-json" }
kobo-policy = { path = "../kobo-policy" }
```

Run:

```bash
rtk cargo test -p kobo-net bomtoon
rtk cargo test -p kobo-net credential
```

Expected: exact request/response tests pass; session and authenticated HTML are denied to app tasks; no redirect is followed.

- [ ] **Step 8: Commit**

```bash
rtk git add crates/kobo-net/Cargo.toml crates/kobo-net/src/lib.rs crates/kobo-net/src/bomtoon.rs
rtk git commit -m "feat(net): add BOMTOON credential broker"
```

### Task 5: Wire managed BOMTOON credentials into both runtimes

**Files:**
- Modify: `crates/kobo-sim/src/lib.rs:2063-2117,2630-2680`
- Modify: `crates/kobod/src/device.rs:60-75,2582-2595`

**Interfaces:**
- Consumes: `ManagedCredentials`, `bomtoon::Recipe`, method-aware `credential_allowed`, and runtime secret/state roots.
- Produces: `SimulatorAuthPaths`, `simulator_auth_paths`, and live providers for the `bomtoon` app only.

- [ ] **Step 1: Write failing simulator path and wiring tests**

Add:

```rust
#[test]
fn simulator_auth_paths_are_shared_and_stable() {
    let root = private_temp_dir();
    let paths = simulator_auth_paths_at(&root);
    assert_eq!(paths.secrets, root.join("cobalt-sim-secrets"));
    assert_eq!(paths.state, root.join("cobalt-sim-state"));
}

#[test]
fn only_bomtoon_receives_the_bomtoon_managed_provider() {
    assert!(managed_credentials("bomtoon", private_temp_dir()).is_some());
    assert!(managed_credentials("chat", private_temp_dir()).is_none());
}
```

- [ ] **Step 2: Run simulator tests and confirm failure**

Run:

```bash
rtk cargo test -p kobo-sim simulator_auth_paths_are_shared_and_stable
```

Expected: compilation fails because shared paths do not exist.

- [ ] **Step 3: Add shared simulator auth paths**

Add:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulatorAuthPaths {
    pub secrets: PathBuf,
    pub state: PathBuf,
}

#[must_use]
pub fn simulator_auth_paths() -> SimulatorAuthPaths {
    simulator_auth_paths_at(&std::env::temp_dir())
}

fn simulator_auth_paths_at(root: &Path) -> SimulatorAuthPaths {
    SimulatorAuthPaths {
        secrets: root.join("cobalt-sim-secrets"),
        state: root.join("cobalt-sim-state"),
    }
}
```

Use this function in `simulated_tasks`; remove the standalone `SIM_SECRETS` path construction.

- [ ] **Step 4: Construct live providers for Bomtoon**

Use a real clock in production wiring:

```rust
fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
```

For `name == "bomtoon"`, construct:

```rust
Arc::new(
    ManagedCredentials::new(
        &paths.secrets,
        &paths.state,
        Arc::new(epoch_millis),
        Arc::new(kobo_net::bomtoon::Recipe::live()),
    )
    .expect("initialize BOMTOON managed credentials"),
)
```

Attach it with `with_managed_credentials`. Other apps receive no managed provider. Update the policy closure to accept `(credential, method, url)`. Update both `with_fetch` closures to forward the separate credential argument to `kobo_net::fetch_from`; never merge it into the application-header slice.

On device, use existing `/mnt/onboard/.adds/cobalt/secrets` and add `/mnt/onboard/.adds/cobalt/state`. Return a startup error rather than panic if provider cleanup or initialization fails.

- [ ] **Step 5: Run simulator and daemon tests**

Run:

```bash
rtk cargo test -p kobo-sim
rtk cargo test -p kobod
```

Expected: simulator path/wiring tests pass; existing runtime task tests remain green.

- [ ] **Step 6: Commit**

```bash
rtk git add crates/kobo-sim/src/lib.rs crates/kobod/src/device.rs
rtk git commit -m "feat(runtime): wire BOMTOON token provider"
```

### Task 6: Add the macOS Chrome login command

**Files:**
- Create: `crates/kobo-cli/src/bomtoon.rs`
- Modify: `crates/kobo-cli/src/main.rs:13-28,351-427,4685-4690`

**Interfaces:**
- Consumes: `kobo_net::bomtoon::validate_session_cookie`, `kobo_sim::simulator_auth_paths`, existing SSH conventions, and CDP JSON messages.
- Produces: `bomtoon::command(arguments: &[String]) -> Result<(), String>`.

- [ ] **Step 1: Write failing command, cookie, CDP, install, and cleanup tests**

Add focused tests in the new module:

Write separate tests named:

- `login_requires_exactly_one_device_or_simulator_target`
- `chrome_discovery_checks_system_and_user_application_directories`
- `secure_nextauth_cookie_wins_without_combining_families`
- `contiguous_cookie_chunks_form_one_cookie_header`
- `a_chunk_gap_control_character_or_oversized_cookie_is_rejected`
- `cdp_messages_are_nul_terminated_and_partial_reads_are_reassembled`
- `selected_cookie_is_validated_alone_before_installation`
- `device_install_keeps_the_cookie_out_of_process_arguments_and_errors`
- `simulator_install_replaces_cookie_and_removes_managed_state`
- `every_exit_path_stops_chrome_and_removes_the_profile`

- [ ] **Step 2: Run the CLI tests and confirm failure**

Run:

```bash
rtk cargo test -p kobo-cli bomtoon::tests
```

Expected: compilation fails because `bomtoon` is not defined.

- [ ] **Step 3: Parse targets and discover Chrome**

Use:

```rust
const USAGE: &str = "usage: kobo bomtoon login (--device IP | --sim)";
const LOGIN_URL: &str = "https://www.bomtoon.tw/user/login";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, Eq, PartialEq)]
enum LoginTarget {
    Device(String),
    Simulator,
}

pub fn command(arguments: &[String]) -> Result<(), String>;
```

Accept only the `login` verb and exactly one target. Reuse `valid_device_host`. Reject all extra flags. Check these executable paths in order:

```text
/Applications/Google Chrome.app/Contents/MacOS/Google Chrome
$HOME/Applications/Google Chrome.app/Contents/MacOS/Google Chrome
/Applications/Chromium.app/Contents/MacOS/Chromium
$HOME/Applications/Chromium.app/Contents/MacOS/Chromium
```

On non-macOS hosts, return a fixed unsupported-host error before creating files.

- [ ] **Step 4: Implement the temporary profile and CDP pipe**

Create a unique directory below `std::env::temp_dir()` with `DirBuilderExt::mode(0o700)`. Launch through `/bin/sh` so standard input/output become Chromium's fixed descriptors without unsafe Rust:

```sh
exec 3<&0
exec 4>&1
browser=$1
shift
exec "$browser" "$@"
```

Pass a fixed dummy `$0`, the Chrome executable as `$1`, and these non-secret arguments after it:

```text
--remote-debugging-pipe
--user-data-dir=<temporary path>
--no-first-run
--no-default-browser-check
https://www.bomtoon.tw/user/login
```

CDP messages are UTF-8 JSON followed by one NUL byte. `DevToolsPipe::call` assigns increasing IDs, ignores unrelated events, matches the response ID, rejects `error`, and never includes a response body in its error text.

Use `Target.getTargets` to locate the BOMTOON page, `Target.attachToTarget` with `flatten: true`, then `Runtime.evaluate` with `awaitPromise: true` and `returnByValue: true`.

The page expression fetches `/api/auth/session`, validates `user`, both nested token objects, and integer expiries, and returns only:

```json
{"authenticated":true,"tokenFingerprint":"SHA256_HEX"}
```

Compute the fingerprint inside the page with Web Crypto over access token, one NUL byte, and refresh token. Never return token values over CDP.

- [ ] **Step 5: Select only the NextAuth session cookie**

Call `Network.getCookies` for `https://www.bomtoon.tw/`. Accept only:

```text
__Secure-next-auth.session-token
__Secure-next-auth.session-token.0
next-auth.session-token
next-auth.session-token.0
```

Numeric chunks must start at zero and remain contiguous. An unchunked base cookie and chunked cookies in the same family are an error. Build a chunked installed `Cookie` header as `name=value` pairs joined by `; `, or use the unchunked base alone. If any secure-family cookie exists, that family must validate; never downgrade to the insecure family. Never combine secure and insecure families. Require BOMTOON domain, path `/`, no control characters, and total bytes at or below `kobo_net::bomtoon::SESSION_COOKIE_MAX_BYTES`.

- [ ] **Step 6: Validate the selected cookie alone**

Call `kobo_net::bomtoon::validate_session_cookie` with the selected family. The helper sends exactly one bounded session request whose only credential is the selected cookie and returns only the token-pair fingerprint. Compare it with the browser fingerprint. Installation proceeds only on an exact match.

Errors name only the failed stage: browser launch, login timeout, session validation, cookie selection, or target installation. They never include CDP payloads, cookies, token fields, or response bodies.

- [ ] **Step 7: Install transactionally to device or simulator**

For device, execute one SSH command whose arguments contain only host and a fixed shell program. Send the cookie through stdin. The shell program must:

1. Create secret/state directories with mode `0700`.
2. Write a mode `0600` temporary cookie.
3. Rename any current cookie to a backup.
4. Rename the temporary cookie into place.
5. Remove `bomtoon-access-token.state`.
6. Restore the backup if any step before completion fails.
7. Remove the backup on success.

For simulator, use `kobo_sim::simulator_auth_paths()` and the same temporary, backup, rename, state-removal, and rollback order with `std::fs`. Test cleanup removes the installed cookie, state, backup, and temporary files.

- [ ] **Step 8: Guarantee cleanup and wire dispatch**

A guard owns `Child` and the profile path. Its `Drop` kills and waits for Chrome, then removes the profile directory. Normal completion explicitly closes the guard and reports success only after cleanup.

Add `mod bomtoon;`, dispatch:

```rust
"bomtoon" => bomtoon::command(&arguments[1..]),
```

Add both command forms to `print_help`. Add `"kobo-bomtoon"` to `STORE_PACKAGES` so the checked-in simulator registry matches the workspace's published apps.

- [ ] **Step 9: Run CLI tests**

Run:

```bash
rtk cargo test -p kobo-cli bomtoon::tests
rtk cargo test -p kobo-cli checked_in_registry_contains_every_store_application
```

Expected: parsing, Chrome discovery, cookie families, CDP framing, secret non-disclosure, transactional device command construction, simulator installation, cleanup, and registry tests pass without opening Chrome or contacting hardware.

- [ ] **Step 10: Commit**

```bash
rtk git add crates/kobo-cli/src/main.rs crates/kobo-cli/src/bomtoon.rs
rtk git commit -m "feat(cli): add BOMTOON Chrome login"
```

### Task 7: Move episode loading to bearer-only JSON

**Files:**
- Modify: `apps/bomtoon/src/api.rs:1-183`
- Modify: `apps/bomtoon/src/parse.rs:1-224`
- Modify: `apps/bomtoon/src/model.rs:24-68`

**Interfaces:**
- Consumes: observed bearer-only content contract and nullable `purchaseStatus`.
- Produces: `api::content(alias)`, JSON `parse::episodes`, and nullable `PurchaseState::from_remote`.

- [ ] **Step 1: Write failing API and parser tests**

Use a redacted content fixture:

```rust
const CONTENT: &[u8] = br#"{
  "result":"SUCCESS",
  "data":{
    "episodes":[
      {"alias":"sample","title":"Sample","isSample":true,"purchaseStatus":null,"paid":null},
      {"alias":"owned","title":"Owned","isSample":false,"purchaseStatus":"POSSESSION","paid":true},
      {"alias":"locked","title":"Locked","isSample":false,"purchaseStatus":null,"paid":null}
    ]
  }
}"#;
```

Add tests:

Write separate tests named:

- `content_uses_exact_bearer_json_endpoint`
- `content_request_contains_no_cookie_or_html_accept_header`
- `episodes_are_read_from_data_episodes`
- `null_purchase_status_is_sample_when_sample_and_not_owned_otherwise`
- `next_data_html_is_rejected_as_json`

- [ ] **Step 2: Run focused app tests and confirm failure**

Run:

```bash
rtk cargo test -p kobo-bomtoon api::tests
rtk cargo test -p kobo-bomtoon parse::tests
```

Expected: current tests still expect session cookies and HTML detail parsing.

- [ ] **Step 3: Replace session/detail API functions**

Remove `SESSION_URL`, `DETAIL_URL`, `SESSION_BYTES`, `session`, and the cookie credential. Add:

```rust
const CONTENT_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/contents/";
const CONTENT_BYTES: u32 = 512 * 1024;

pub fn content(alias: &str) -> Task {
    fetch(
        format!(
            "{CONTENT_URL}{alias}?isNotLoginAdult=false&isPorch=false"
        ),
        CONTENT_BYTES,
        Credential::bearer("bomtoon-access-token"),
        balcony_headers(),
    )
}

pub fn logout() -> Task {
    Task::RevokeCredential {
        credential: "bomtoon-access-token".to_owned(),
    }
}
```

Keep library and recent requests unchanged.

- [ ] **Step 4: Parse bounded content JSON**

Remove `NEXT_DATA_ID` and `session_is_authenticated`. Parse `result == "SUCCESS"`, then `data.episodes`. Change:

```rust
pub fn from_remote(status: Option<&str>, is_sample: bool, paid: Option<bool>) -> Self {
    if status == Some("POSSESSION") {
        Self::Owned
    } else if is_sample {
        Self::Sample
    } else if paid == Some(false) {
        Self::Free
    } else if status.is_none() || status == Some("NONE") {
        Self::NotOwned
    } else {
        Self::Other(status.unwrap_or_default().to_owned())
    }
}
```

Each episode requires string `alias` and `title`, boolean `isSample`, and nullable/string `purchaseStatus`. An optional `paid` field may be boolean or null; absence is `None`. Reject any other purchase-status or paid type.

- [ ] **Step 5: Run app API/parser tests**

Run:

```bash
rtk cargo test -p kobo-bomtoon api::tests
rtk cargo test -p kobo-bomtoon parse::tests
```

Expected: exact bearer URL, no cookie, JSON nesting, nullable purchase status, and HTML rejection pass.

- [ ] **Step 6: Commit**

```bash
rtk git add apps/bomtoon/src/api.rs apps/bomtoon/src/parse.rs apps/bomtoon/src/model.rs
rtk git commit -m "fix(bomtoon): load episodes with bearer JSON"
```

### Task 8: Add direct startup and in-app logout states

**Files:**
- Modify: `apps/bomtoon/src/main.rs:12-481`

**Interfaces:**
- Consumes: `api::library`, `api::content`, `api::logout`, `TaskError::LocalStorage`, and `TaskError::RevocationUnconfirmed`.
- Produces: direct library startup, signed-out instructions, Sign out action, local-error preservation, remote-warning state, and account-data clearing.

- [ ] **Step 1: Write failing `AppRunner` behavior and layout tests**

Add helpers that extract spawned tasks and the last `Command::SetScreen`. Use `AppRunner::with_metrics(Bomtoon::default(), CLARA_BW_METRICS)`.

Add tests:

Write separate tests named:

- `startup_requests_library_without_a_session_task`
- `missing_credentials_show_login_instructions_and_try_again`
- `a_loaded_library_shows_sign_out`
- `successful_logout_clears_every_account_collection`
- `unconfirmed_remote_logout_is_signed_out_with_a_warning`
- `local_storage_logout_failure_keeps_loaded_account_data`
- `expired_session_returns_to_login_instructions`

Assert visible labels fit under `CLARA_BW_METRICS` using the screen commands produced by the real app runner.

- [ ] **Step 2: Run focused UI tests and confirm failure**

Run:

```bash
rtk cargo test -p kobo-bomtoon startup_requests_library_without_a_session_task
rtk cargo test -p kobo-bomtoon successful_logout_clears_every_account_collection
```

Expected: startup still creates `Pending::Session`; no Sign out action or logout state exists.

- [ ] **Step 3: Simplify pending work and startup**

Use:

```rust
const SIGN_OUT: &str = "sign-out";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pending {
    Library(usize),
    Recent(usize),
    Content(usize),
    Logout,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AccountState {
    #[default]
    Active,
    SignedOut,
    Expired,
    RevocationUnconfirmed,
}
```

Add `account: AccountState` to `Bomtoon`. Remove every `Pending::Session` branch. `restart` clears the current problem, sets `AccountState::Active`, resets account data, and spawns `api::library(0)` directly.

- [ ] **Step 4: Render signed-out and signed-in controls**

Before the ordinary view match, render signed-out states with:

```text
Run this on your Mac:
kobo bomtoon login --device <Kobo IP>
```

Use `Try again` for `RETRY`. `Expired` adds a session-expired attention banner. `RevocationUnconfirmed` adds the remote-revocation warning while remaining signed out.

Append `.button(SIGN_OUT, "Sign out")` to the library screen. Keep episode screens unchanged.

- [ ] **Step 5: Execute logout and clear data only after local invalidation**

Add:

```rust
fn clear_account_data(&mut self) {
    self.comics.clear();
    self.recent.clear();
    self.episodes.clear();
    self.selected_title.clear();
    self.page = 0;
    self.library_view_page = 0;
    self.recent_view_page = 0;
    self.next_library_page = None;
    self.next_recent_page = None;
    self.total_library_titles = 0;
    self.total_recent_titles = 0;
    self.library_loaded = false;
    self.recent_loaded = false;
}
```

On `SIGN_OUT`, spawn `Pending::Logout` and `api::logout`. Handle outcomes:

```rust
(Pending::Logout, TaskOutcome::Completed(_)) => {
    self.clear_account_data();
    self.account = AccountState::SignedOut;
}
(Pending::Logout, TaskOutcome::Failed(TaskError::RevocationUnconfirmed)) => {
    self.clear_account_data();
    self.account = AccountState::RevocationUnconfirmed;
}
(Pending::Logout, TaskOutcome::Failed(TaskError::LocalStorage)) => {
    self.problem = Some("Could not remove the local BOMTOON sign-in data.".to_owned());
}
```

For library, recent, or content `NoCredential`, set `SignedOut`; for `Unauthorized`, set `Expired`. Other errors retain the existing `Failure::of` path. A local storage failure must not clear loaded account data.

- [ ] **Step 6: Run all Bomtoon tests**

Run:

```bash
rtk cargo test -p kobo-bomtoon
```

Expected: direct startup, signed-out/expired layouts, Sign out, successful and warning logout, data clearing, and local failure preservation pass under Clara metrics.

- [ ] **Step 7: Commit**

```bash
rtk git add apps/bomtoon/src/main.rs
rtk git commit -m "feat(bomtoon): add managed sign out"
```

### Task 9: Verify the complete simulator-only flow

**Files:**
- Verify only; do not modify files unless a gate exposes a task-scoped defect.

**Interfaces:**
- Consumes: all previous task deliverables.
- Produces: focused test evidence, workspace gate evidence, and attended simulator evidence without hardware access.

- [ ] **Step 1: Run focused package gates**

Run:

```bash
rtk cargo test -p kobo-protocol
rtk cargo test -p kobo-policy
rtk cargo test -p kobo-net
rtk cargo test -p kobo-sim
rtk cargo test -p kobo-cli
rtk cargo test -p kobo-bomtoon
rtk cargo test -p kobod
```

Expected: every command passes.

- [ ] **Step 2: Run workspace formatting, tests, and Clippy**

Run:

```bash
rtk cargo fmt --all --check
rtk cargo test --workspace --all-targets --all-features
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all three commands exit successfully with no warnings.

- [ ] **Step 3: Run the CLI simulator login smoke test**

Run:

```bash
rtk cargo run -p kobo-cli -- bomtoon login --sim
```

Complete the real BOMTOON login in the temporary Chrome profile. Expected CLI result: success without printing cookie, token, email, user ID, or IP values. Confirm the profile directory is removed after Chrome exits.

- [ ] **Step 4: Exercise the app in both simulators**

Run the browser simulator through the existing app development command and the runtime simulator through:

```bash
rtk cargo run -p kobo-cli -- run --sim --app bomtoon
```

Expected behavior:

1. Library loads without a session-check screen.
2. Recent reading loads.
3. Opening a title loads bearer-only JSON episode status.
4. The library shows `Sign out`.
5. Sign out returns to instructions and clears library, recent, and episode data.
6. `Try again` remains signed out until `kobo bomtoon login --sim` runs again.

Do not run `--device`, SSH, device discovery, or any physical-Kobo command.

- [ ] **Step 5: Inspect simulator output for secret disclosure**

Check only generated terminal output and simulator traces from this run. Expected: no credential values, token-bearing response bodies, emails, user IDs, IP addresses, or CDP payloads appear. Delete the temporary sanitized HAR workspace after it is no longer needed; it is already excluded locally and must remain uncommitted.

- [ ] **Step 6: Record final evidence**

Record the exact passing commands, simulator surfaces exercised, logout result, Chrome cleanup result, and confirmation that no physical Kobo was contacted. Do not create a commit for verification-only evidence unless a task-scoped fix was necessary.
