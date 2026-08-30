# BOMTOON Episode Commerce Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add restart-safe single-episode Gift rental, Coin rental, and Coin purchase to the BOMTOON episode list.

**Architecture:** A new app-local `commerce.rs` owns the transaction state machine, durable unresolved marker, title Gift state, and quote policy. The existing foreground task lane performs quote, POST, and content reconciliation serially; wallet refresh remains independent. Before app work can issue POST, the managed credential stack adds a narrow `CredentialScope` task that returns a stable opaque account scope without exposing credentials or the provider account identifier.

**Tech Stack:** Rust 2021, `kobo-protocol`, `kobo-policy`, `kobo-net`, `kobo-sdk`, `kobo-json`, retained `ScreenBuilder` UI, colocated Rust tests.

## Global Constraints

- Work only in `/Users/cowfishorange/Codes/cobalt-bomtoon-tw/.worktrees/bomtoon-coin-ticket-redemption` on `feature/bomtoon-coin-ticket-redemption`.
- Follow `CONTEXT.md`: Gift rents an episode; Ticket never directly pays for an episode.
- Supported mutations are exactly `RENT_GIFT`, `RENT`, and `POSSESSION`; `isMobile` is exactly `false`.
- Persist and acknowledge an account-scoped unresolved marker before POST; acknowledge its deletion before enabling another mutation.
- Never reconcile, clear, or bypass a marker under a different account scope.
- No optimistic access, Coin, or Gift updates.
- Offline, signed-out, expired, unknown-auth, and unknown-scope states cannot quote or mutate.
- No Coin top-up, Ticket redemption, Mileage exchange, bulk selection, active-rental purchase, polling, or countdown timer.
- Gift, quote, and receipt responses are capped at 64 KiB; Gift arrays are capped at 64 entries each.
- Aliases are capped at 128 UTF-8 bytes; titles at 512 UTF-8 bytes; safe result codes at 128 UTF-8 bytes.
- Expand the BOMTOON bearer allowlist only for exact wallet/Gift/quote GET shapes and exact `/api/balcony-api/purchase` POST with no query or fragment. Keep all other methods, paths, and origins denied.
- Bump `kobo_protocol::VERSION` from 12 to 13 and the coordinated workspace/runtime version from `0.3.0` to `0.4.0`. Publish BOMTOON app `0.5.0` with `minimum_cobalt_version` `0.4.0`; keep its capability list exactly `["network"]`.
- Raw HARs remain outside the worktree. Before every commit run `git diff --cached --name-only -- evidences` and require no output.
- Do not add dependencies, workspace members, capabilities, origins, device-policy changes, general credential-reading APIs, shims, or deprecated Ticket-commerce paths.
- Real Coin/Gift spending is operator-attended only; automated tests and agents never submit live mutations.

## File Structure

### Shared managed-account scope and network policy

- `crates/kobo-policy/src/managed.rs`: retain bounded provider subject; persist provider-local scope key; derive and resolve opaque scopes.
- `crates/kobo-net/src/bomtoon.rs`: parse `user.id` during bootstrap, preserve it during refresh, and derive account scope.
- `crates/kobo-net/src/sha256.rs`: dependency-free HMAC-SHA-256.
- `crates/kobo-protocol/src/lib.rs`: protocol v13 plus wire-stable `Task::CredentialScope { credential }` using task-kind tag `5`.
- `crates/kobo-policy/src/tasks.rs`: execute `CredentialScope` through the managed provider.
- `crates/kobo-net/src/lib.rs`: exact method/URL allowlist for wallet and commerce routes.
- Root `Cargo.toml`, `Cargo.lock`, and explicit internal path-version constraints in `kobo-policy`, `kobo-protocol`, `kobo-sdk`, and `kobo-text`: coordinated Cobalt `0.4.0` release identity.

### BOMTOON app

- `apps/bomtoon/src/model.rs`: content IDs, episode access, rental expiry, Gift/quote/receipt models, purchase types.
- `apps/bomtoon/src/parse.rs`: bounded content, Gift, quote, and receipt parsing.
- `apps/bomtoon/src/api.rs`: exact scope, Gift, quote, and purchase tasks.
- `apps/bomtoon/src/commerce.rs`: pure state machine, marker codec, account/connectivity gates, Gift generation, quote policy.
- `apps/bomtoon/src/main.rs`: lifecycle/store/task wiring, quote screen, action dispatch, reconciliation, reader transition, library invalidation.
- `apps/catalog.json`: BOMTOON `0.5.0`, minimum Cobalt `0.4.0`, unchanged `network` capability.

---

### Task 1: Retain Managed Account Subject

**Files:**
- Modify: `crates/kobo-policy/src/managed.rs`
- Modify: `crates/kobo-net/src/bomtoon.rs`
- Modify: `crates/kobo-policy/src/tasks.rs`

**Interfaces:**
- Consumes: BOMTOON session `user.id`, observed as a string.
- Produces:

```rust
pub struct ManagedTokenPair {
    pub access_token: String,
    pub access_expires_at_ms: u64,
    pub refresh_token: String,
    pub refresh_expires_at_ms: u64,
    pub account_subject: Option<String>,
}
```

- Invariant: a present `account_subject` is non-empty, at most 128 UTF-8 bytes, and contains no control character. `None` is reserved for valid legacy v1 state and cannot produce a commerce scope.

- [ ] **Step 1: Add failing BOMTOON parser tests**

Add tests proving bootstrap retains only `user.id`, refresh preserves the previous subject, and missing/wrong/empty/oversized/control-character IDs fail:

```rust
#[test]
fn session_bootstrap_retains_bounded_account_subject() {
    let pair = parse_session_tokens(SESSION_WITH_ACCOUNT_A).expect("session pair");
    assert_eq!(pair.account_subject.as_deref(), Some("account-a"));
}

#[test]
fn refresh_preserves_bootstrap_account_subject() {
    let pair = parse_refresh_tokens(REFRESH_RESPONSE, Some("account-a")).expect("refresh pair");
    assert_eq!(pair.account_subject.as_deref(), Some("account-a"));
}
```

Fixtures contain fake tokens and fake account names only.

- [ ] **Step 2: Run the parser test and verify red**

Run: `cargo test -p kobo-net session_bootstrap_retains_bounded_account_subject`

Expected: compilation fails because the subject field and subject-aware refresh parser do not exist.

- [ ] **Step 3: Extend the pair and parsers**

Parse exact `user.id` in the session envelope. Change refresh parsing to:

```rust
fn parse_refresh_tokens(
    response: &[u8],
    account_subject: Option<&str>,
) -> Result<ManagedTokenPair, TaskError>;
```

In `Recipe::refresh`, pass `pair.account_subject.as_deref()`. Do not retain email, name, IP address, or provider. A fresh session bootstrap requires valid `user.id`; a refresh of valid legacy state preserves `None`.

- [ ] **Step 4: Preserve legacy v1 state while adding v2**

Read `cobalt-managed-v1` into `account_subject: None` and keep it usable for bearer requests. Write v1 again when a refreshed legacy pair still has no subject. Write `cobalt-managed-v2` with a final bounded subject line only when the pair has `Some(subject)`. Do not silently bootstrap, migrate, or assign a scope to v1 state.

Add tests:

```rust
#[test]
fn v2_state_round_trips_account_subject() {
    let directories = TestDirectories::new("v2-subject");
    let bound = BoundPair {
        cookie_digest: "digest:cookie-a".to_owned(),
        pair: token_pair("a", 1_000_000, Some("account-a")),
    };
    write_bound_pair(&directories.managed_state(), &bound).expect("write v2 state");
    assert_eq!(
        read_bound_pair(&directories.managed_state())
            .expect("read v2 state")
            .expect("stored pair")
            .pair
            .account_subject
            .as_deref(),
        Some("account-a")
    );
}

#[test]
fn v1_state_remains_readable_without_account_scope() {
    let directories = TestDirectories::new("v1-readable");
    fs::write(directories.cookie(), "cookie-a").expect("cookie");
    fs::write(
        directories.managed_state(),
        "cobalt-managed-v1\ndigest:cookie-a\n1000000\n4600000\naccess-a\nrefresh-a",
    )
    .expect("write v1 state");
    let recipe = Arc::new(FakeRecipe::default());
    let (_, clock) = adjustable_clock(1);
    let managed = provider(&directories, clock, Arc::clone(&recipe));

    assert!(
        managed
            .resolve(&Credential::bearer("bomtoon-access-token"))
            .expect("resolve")
            .is_some()
    );
    assert!(recipe.calls.lock().expect("calls").is_empty());
    assert_eq!(
        read_bound_pair(&directories.managed_state())
            .expect("read v1 state")
            .expect("legacy pair")
            .pair
            .account_subject,
        None
    );
}
```

- [ ] **Step 5: Update all fake recipes and constructors**

Give fresh/bootstrap test pairs `Some("account-a")` and legacy fixtures `None`. Verify no error or assertion prints real credential material.

- [ ] **Step 6: Format and test**

Run:

```bash
cargo fmt --all
cargo test -p kobo-net bomtoon
cargo test -p kobo-policy managed
cargo test -p kobo-policy managed_provider
```

Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/kobo-policy/src/managed.rs crates/kobo-policy/src/tasks.rs crates/kobo-net/src/bomtoon.rs
git diff --cached --name-only -- evidences
git diff --cached --check
git commit -m "feat(auth): retain managed account subject"
```

### Task 2: Expose Stable Opaque Credential Scope

**Files:**
- Modify: `crates/kobo-net/src/sha256.rs`
- Modify: `crates/kobo-net/src/bomtoon.rs`
- Modify: `crates/kobo-policy/src/managed.rs`
- Modify: `crates/kobo-policy/src/tasks.rs`
- Modify: `crates/kobo-protocol/src/lib.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/kobo-policy/Cargo.toml`
- Modify: `crates/kobo-protocol/Cargo.toml`
- Modify: `crates/kobo-sdk/Cargo.toml`
- Modify: `crates/kobo-text/Cargo.toml`

**Interfaces:**
- Consumes: `ManagedTokenPair::account_subject: Option<String>`.
- Produces:

```rust
Task::CredentialScope { credential: String }

pub trait ManagedCredentialRecipe {
    fn derive_scope_key(&self, binding_secret: &str) -> String;
    fn derive_account_scope(&self, scope_key: &str, account_subject: &str) -> String;
}

impl ManagedCredentials {
    pub fn scope(&self, credential: &str) -> Result<Option<String>, TaskError>;
}
```

- Wire contract: task-kind tag `5`; success bytes are exactly 32 lowercase hexadecimal characters.
- Durable contract: `<credential>.scope-key` is private, atomic, retained across revoke/re-login, and never returned to the app.

- [ ] **Step 1: Add failing HMAC and derivation tests**

Add standard HMAC-SHA-256 vectors and provider behavior:

```rust
#[test]
fn account_scope_is_stable_and_subject_specific() {
    let key = recipe.derive_scope_key("high-entropy-cookie-a");
    let first = recipe.derive_account_scope(&key, "account-a");
    assert_eq!(first, recipe.derive_account_scope(&key, "account-a"));
    assert_ne!(first, recipe.derive_account_scope(&key, "account-b"));
    assert_eq!(first.len(), 32);
    assert!(first.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
}
```

- [ ] **Step 2: Implement dependency-free HMAC and domains**

Expose a raw SHA-256 digest helper and:

```rust
pub fn hmac_hex(key: &[u8], message: &[u8]) -> String;
```

Use standard 64-byte HMAC pads and distinct inputs:

```text
scope-key material = "cobalt-managed-scope-key-v1\0" || binding secret
scope = HMAC(scope key, "bomtoon-account-scope-v1\0" || account subject)
```

Expose only the first 32 lowercase hex characters.

- [ ] **Step 3: Add failing persistence tests**

Cover same subject with a replacement cookie → same scope; different subject → different scope; revoke retains scope key; missing credential; valid legacy subject-less credential; malformed scope; and scope-key write failure. The legacy test must prove ordinary bearer resolution succeeds while `scope()` returns `TaskError::Denied`.

- [ ] **Step 4: Implement atomic scope-key storage**

Reuse managed-state private-directory, synchronized temp-write, rename, and permission patterns. `ManagedCredentials::scope` verifies the name, acquires the managed lease, obtains the current pair, rejects `account_subject: None` with `TaskError::Denied`, reads or atomically creates the scope key, derives/validates the scope, and returns only the scope. Revocation must not delete the scope-key file.

- [ ] **Step 5: Add protocol v13 and coordinate Cobalt 0.4.0**

Extend the protocol version-history comment and change:

```rust
pub const VERSION: u8 = 13;
```

Document that v13 adds `CredentialScope`, which v12 runtimes cannot decode. Extend `Task`, `is_sendable`, frame length, encode, and decode:

```rust
Task::CredentialScope {
    credential: "bomtoon-access-token".to_owned(),
}
```

Encode kind byte `5` plus one bounded credential-name string. Add round-trip, oversize, truncation, unknown-tag, v12-rejection, and v13-handshake tests. Do not make v12 decode or ignore the new task.

Set `[workspace.package] version = "0.4.0"` in the root manifest. Change only the explicit internal path constraints found in:

```text
crates/kobo-policy/Cargo.toml
crates/kobo-protocol/Cargo.toml
crates/kobo-sdk/Cargo.toml
crates/kobo-text/Cargo.toml
```

from `0.3.0` to `0.4.0`. Regenerate `Cargo.lock` through Cargo; do not globally replace the unrelated registry package `cpufeatures 0.3.0`.

- [ ] **Step 6: Execute through managed policy**

Classify `CredentialScope` as requiring `Capability::Network` because resolving a pair may bootstrap/refresh. Add:

```rust
Task::CredentialScope { credential } => match backends.managed {
    Some(provider) => match provider.scope(credential) {
        Ok(Some(scope)) => TaskOutcome::Completed(scope.into_bytes()),
        Ok(None) => TaskOutcome::Failed(TaskError::Denied),
        Err(error) => TaskOutcome::Failed(error),
    },
    None => TaskOutcome::Failed(TaskError::Denied),
}
```

Test success, wrong name, no provider, no credential, valid legacy bearer plus denied scope, offline bootstrap, and cancellation.

- [ ] **Step 7: Run focused tests**

Run:

```bash
cargo fmt --all
cargo test -p kobo-net sha256
cargo test -p kobo-net bomtoon
cargo test -p kobo-protocol credential_scope
cargo test -p kobo-policy managed
cargo test -p kobo-policy credential_scope
cargo test -p kobo-sdk task
cargo test -p kobo-protocol version
cargo metadata --no-deps --format-version 1
cargo test -p kobod app_store
```

Expected: all pass; same-account scope survives cookie replacement; valid v1 credentials still read but cannot obtain commerce scope; workspace packages and `kobod` report `0.4.0`; unrelated registry `cpufeatures` remains `0.3.0`.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock crates/kobo-policy/Cargo.toml crates/kobo-protocol/Cargo.toml crates/kobo-sdk/Cargo.toml crates/kobo-text/Cargo.toml crates/kobo-net/src/sha256.rs crates/kobo-net/src/bomtoon.rs crates/kobo-policy/src/managed.rs crates/kobo-policy/src/tasks.rs crates/kobo-protocol/src/lib.rs
git diff --cached --name-only -- evidences
git diff --cached --check
git commit -m "feat(auth): expose opaque account scope"
```

### Task 3: Model and Parse Episode Commerce

**Files:**
- Modify: `apps/bomtoon/src/model.rs`
- Modify: `apps/bomtoon/src/parse.rs`
- Modify: `apps/bomtoon/src/main.rs` (caller migration and Ticket UI removal only)

**Interfaces:**
- Produces:

```rust
pub struct ContentDetail { pub id: usize, pub episodes: Vec<Episode> }

pub struct Episode {
    pub id: usize,
    pub alias: String,
    pub title: String,
    pub purchase: PurchaseState,
    pub rent_expires_at: Option<i64>,
    pub rent_coin: Option<usize>,
    pub purchase_coin: Option<usize>,
    pub gift_eligible: bool,
}

pub enum PurchaseState { Owned, Rented, Sample, Free, NotOwned, Other(String) }
pub enum PurchaseType { RentGift, Rent, Possession }
pub struct GiftBalance { pub available: usize }
pub struct Quote {
    pub content_id: usize,
    pub episode_id: usize,
    pub content_alias: String,
    pub episode_alias: String,
    pub is_available: bool,
    pub coin_kind: String,
    pub rent_coin: usize,
    pub possession_coin: usize,
    pub permanent_coin: Option<usize>,
    pub is_rent_gift: bool,
    pub is_possession_gift: bool,
}
pub struct CoinUse {
    pub aggregate: usize,
    pub standard: usize,
    pub bonus: usize,
    pub free: usize,
}
pub struct PurchaseReceipt {
    pub purchase_type: PurchaseType,
    pub content_alias: String,
    pub episode_alias: String,
    pub coin_use: CoinUse,
}

pub fn content_detail(bytes: &[u8]) -> Result<ContentDetail, ParseError>;
pub fn gift_balance(bytes: &[u8]) -> Result<GiftBalance, ParseError>;
pub fn quote(bytes: &[u8]) -> Result<Quote, ParseError>;
pub fn purchase_receipt(bytes: &[u8]) -> Result<PurchaseReceipt, ParseError>;
```

- [ ] **Step 1: Write failing model tests**

Replace direct-Ticket tests with `RENT` readability, exact/fractional/48-hour/elapsed/missing-expiry cases, `POSSESSION` precedence, and unknown-status fail-closed behavior:

```rust
assert_eq!(rented(Some(HOUR_MS + 1)).remaining_rental_hours(0), Some(2));
assert!(PurchaseState::Rented.is_readable());
assert!(!PurchaseState::Other("FUTURE".into()).is_readable());
```

- [ ] **Step 2: Verify red**

Run: `cargo test -p kobo-bomtoon model::tests`

Expected: compilation fails because the new model does not exist.

- [ ] **Step 3: Implement domain types**

Use checked whole-hour ceiling:

```rust
let remaining = expiry.saturating_sub(now_ms).max(0);
let hours = remaining / HOUR_MS + i64::from(remaining % HOUR_MS != 0);
usize::try_from(hours).ok()
```

`PurchaseType::as_remote()` returns exactly `RENT_GIFT`, `RENT`, or `POSSESSION`. Paid prices are available only for exact `coinKind == "COIN"`; conflicting optional `permanentCoin` disables only purchase.

- [ ] **Step 4: Add failing sanitized parser tests**

Cover content IDs/status/expiry, Gift checked sum and 64-entry limits, quote identity/prices/Gift flags/unknown coin kind, receipt identity/type/aggregate use/all-or-none bucket breakdown, and alias/title boundaries. Fixtures contain no headers, tokens, cookies, or real user data.

- [ ] **Step 5: Implement bounded parsers**

Replace `episodes` with `content_detail`. Remove `coinKind == "TICKET"` quantity extraction. Treat receipt `useCoin` as aggregate; if any of `useGoldCoin`, `useBonusCoin`, `useFreeCoin` appears, require all and require their checked sum to equal `useCoin`. Do not retain payment info, thumbnails, timestamps, `isRepeatPurchase`, or server prose.

- [ ] **Step 6: Migrate current callers and remove Ticket episode UI**

Retain `selected_content_id`, remove `uses_ticket`, `ticket_quantity`, title Ticket summary, and `Ticket · quantity`. Render `Rented` as `Read · N hrs` or `Read · Rented`. Do not expose mutations yet.

- [ ] **Step 7: Test and commit**

Run:

```bash
cargo fmt --all
cargo test -p kobo-bomtoon model::tests
cargo test -p kobo-bomtoon parse::tests
cargo test -p kobo-bomtoon
```

Then:

```bash
git add apps/bomtoon/src/model.rs apps/bomtoon/src/parse.rs apps/bomtoon/src/main.rs
git diff --cached --name-only -- evidences
git diff --cached --check
git commit -m "feat(bomtoon): model episode commerce"
```

### Task 4: Allow and Construct Exact Commerce Requests

**Files:**
- Modify: `crates/kobo-net/src/lib.rs`
- Modify: `apps/bomtoon/src/api.rs`
- Modify: `apps/bomtoon/src/model.rs` only if a request constructor belongs there

**Interfaces:**
- Consumes: `PurchaseType`, `Task::CredentialScope`.
- Produces:

```rust
pub fn account_scope() -> Task;
pub fn title_gifts(content_id: usize) -> Task;
pub fn quote(content_alias: &str, episode_alias: &str, purchase: PurchaseType) -> Task;
pub fn purchase(content_alias: &str, episode_id: usize, purchase: PurchaseType) -> Task;
```

- [ ] **Step 1: Add failing credential-policy tests**

Permit BOMTOON bearer GET only for:

```text
/api/balcony-api-v2/asset/user
/api/balcony-api-v2/payment/charge?<fixed keys and enum values>
/api/balcony-api-v2/gift/contents/detail?contentsId=<digits>
/api/balcony-api-v2/contents/price/<bounded alias>/<bounded alias>?purchaseType=RENT|POSSESSION
```

Permit POST only for exact `/api/balcony-api/purchase` with no query or fragment. Add table-driven denials for wrong method, origin, port, path, prefix/suffix, empty/invalid alias, nonnumeric ID, duplicate/extra/missing query key, unapproved query value, fragment, named/basic credential, and purchase GET/PUT.

- [ ] **Step 2: Verify policy tests red**

Run: `cargo test -p kobo-net bomtoon_credential_allows_commerce_routes`

Expected: the new routes are denied by current GET-only policy.

- [ ] **Step 3: Implement exact URL classifiers**

Split BOMTOON policy by method:

```rust
match method {
    RequestMethod::Get => bomtoon_existing_get(url)
        || bomtoon_asset_url(url)
        || bomtoon_history_url(url)
        || bomtoon_gift_url(url)
        || bomtoon_quote_url(url),
    RequestMethod::Post => bomtoon_purchase_url(url),
    _ => false,
}
```

Use parsed origin plus exact path/query structure; never substring-match the purchase path.

- [ ] **Step 4: Add failing app task-construction tests**

Assert `account_scope()` returns exact `CredentialScope`. Destructure Gift/quote tasks and assert paths, fixed query keys, bearer credential, Balcony headers, offset zero, and 64 KiB. Destructure POST and assert exact URL, JSON content type, body fields, `x-referer`, and absence of cookies, `Origin`, and authorization values in ordinary headers.

- [ ] **Step 5: Implement task constructors**

Use:

```rust
let body = ObjectBuilder::new()
    .set("id", episode_id)
    .set("purchaseType", purchase.as_remote())
    .set("isMobile", false)
    .build()
    .to_json();
```

Gift/Rent re-quotes use `RENT`; purchase uses `POSSESSION`. Build referer only from the selected bounded title alias.

- [ ] **Step 6: Run focused tests**

Run:

```bash
cargo fmt --all
cargo test -p kobo-net bomtoon
cargo test -p kobo-bomtoon api::tests
```

Expected: exact approved routes pass; all adversarial variants remain denied.

- [ ] **Step 7: Commit**

```bash
git add crates/kobo-net/src/lib.rs apps/bomtoon/src/api.rs apps/bomtoon/src/model.rs
git diff --cached --name-only -- evidences
git diff --cached --check
git commit -m "feat(bomtoon): allow commerce requests"
```

### Task 5: Build Commerce State Machine and Marker Codec

**Files:**
- Create: `apps/bomtoon/src/commerce.rs`
- Modify: `apps/bomtoon/src/main.rs` (`mod commerce;` only)

**Interfaces:**

```rust
pub const MARKER_KEY: &str = "commerce.unresolved.v1";

pub struct AccountScope([u8; 16]);
pub struct UnresolvedMutationV1 {
    pub account_scope: AccountScope,
    pub title_id: usize,
    pub title_alias: String,
    pub episode_id: usize,
    pub episode_alias: String,
    pub purchase_type: PurchaseType,
    pub quoted_price: usize,
    pub pre_mutation_spendable_coin: Option<usize>,
    pub pre_mutation_title_gifts: Option<usize>,
}

pub enum CommerceCommand {
    SaveMarker(Vec<u8>),
    FetchQuote { selection: Selection, purchase: PurchaseType },
    Post(UnresolvedMutationV1),
    RefreshContent(Selection),
    ForgetMarker,
}

pub struct CommerceEffects {
    pub command: Option<CommerceCommand>,
    pub refresh_wallet: bool,
    pub refresh_gifts: bool,
    pub redraw: bool,
}
```

States are `LoadingSafetyState`, `Idle`, `Quoting`, `Choosing`, `Requoting`, `PersistingIntent`, `Mutating`, `Reconciling`, `ClearingIntent`, and `AcceptedButStale`.

- [ ] **Step 1: Write failing scope/marker codec tests**

Cover exact 32-character lowercase hex, marker round trip, version/field/type failures, alias bounds, number overflow, trailing data, and exactly one pre-mutation balance. Assert encoded bytes contain no title, episode title, token, cookie, or raw account ID.

- [ ] **Step 2: Implement bounded JSON codec**

Use one explicit version. `RENT_GIFT` requires only Gift snapshot; paid types require only Coin snapshot. Reject both-present and both-absent markers.

- [ ] **Step 3: Write failing write-ahead transition tests**

```rust
let save = commerce.choose(Action::Rent).command;
assert!(matches!(save, Some(CommerceCommand::SaveMarker(_))));
assert!(!commerce.can_emit_post());
let post = commerce.marker_saved(MARKER_KEY).command;
assert!(matches!(post, Some(CommerceCommand::Post(_))));
```

Also cover mismatched/denied save, explicit rejection, ambiguous outcome, acknowledged/denied forget, restart load, same/different scope, and no second POST from every marker-bearing state.

- [ ] **Step 4: Implement gates and transitions**

Expose events instead of public state mutation. Every transition returns `CommerceEffects`; at most one foreground/store command is emitted. Wallet/Gift refresh booleans may accompany content reconciliation without allocating a command list.

- [ ] **Step 5: Write failing quote-policy tests**

Cover unknown/insufficient Coin, unknown/zero Gift, Gift without Coin balance, unknown coin kind, `isAvailable`, changed re-quote, and active rental.

- [ ] **Step 6: Implement quote presentation data**

Return labels/control states without building SDK screens. Exact actions: `Use Gift`, `Rent · N coins`, `Buy · N coins`, `Cancel`, with separate concrete disabled reasons.

- [ ] **Step 7: Test and commit**

Run:

```bash
cargo fmt --all
cargo test -p kobo-bomtoon commerce::tests
```

Then:

```bash
git add apps/bomtoon/src/commerce.rs apps/bomtoon/src/main.rs
git diff --cached --name-only -- evidences
git diff --cached --check
git commit -m "feat(bomtoon): add safe commerce state machine"
```

### Task 6: Restore Pending Commerce Safely at Startup

**Files:**
- Modify: `apps/bomtoon/src/main.rs`
- Modify: `apps/bomtoon/src/api.rs` only if scope construction needs adjustment

**Interfaces:**
- Consumes: `api::account_scope`, `Commerce`, `MARKER_KEY`, `AppStore::{load,save,forget}`, `StoreResult`.
- Produces: `AccountState::Checking`, `ConnectionState::{Unknown,Online,Offline}`, scope-task tracking, and `KoboApp::on_store`.

- [ ] **Step 1: Write failing startup-order tests**

Assert startup emits marker `Load`, `CredentialScope`, library, and wallet requests; no commerce action appears until marker and scope settle. Deliver callbacks in both orders.

- [ ] **Step 2: Write failing auth × connectivity tests**

Cover same scope/no marker, same scope/marker, different scope, no credential, unauthorized, legacy credential without scope, warm offline, and cold-start offline. Inspect commands to prove forbidden rows emit neither POST nor marker Forget.

- [ ] **Step 3: Implement startup safety state**

Default account state becomes `Checking`. Add `commerce`, `connection`, and dedicated scope task ID. `restart` loads marker and requests scope while starting library/wallet within the four-task limit.

- [ ] **Step 4: Dispatch scope and store outcomes**

Route exact scope task before other task registries. Parse with `AccountScope::from_bytes`; map `NoCredential`, `Unauthorized`, and `Offline` to the approved matrix.

Implement exact-key/expected-operation store dispatch:

```rust
match result {
    StoreResult::Loaded { key, value } if key == MARKER_KEY => {
        let effects = self.commerce.marker_loaded(value.as_deref());
        self.apply_commerce_effects(context, effects);
    }
    StoreResult::Saved { key } if key == MARKER_KEY => {
        let effects = self.commerce.marker_saved(&key);
        self.apply_commerce_effects(context, effects);
    }
    StoreResult::Forgotten { key } if key == MARKER_KEY => {
        let effects = self.commerce.marker_forgotten(&key);
        self.apply_commerce_effects(context, effects);
    }
    StoreResult::Denied(error) => {
        let effects = self.commerce.store_denied(error);
        self.apply_commerce_effects(context, effects);
    }
    _ => {}
}
```

- [ ] **Step 5: Preserve marker across account clearing**

Credential failure, logout, suspend, background, and exit clear volatile commerce and cancel tasks but never forget the marker. Same-account login resumes reconciliation; another account gets read access plus commerce lock.

- [ ] **Step 6: Add restart/store-failure tests**

Prove denied save emits no POST; exit between Saved and POST reloads; exit during POST reloads; denied forget stays locked; matching Forgotten alone restores Idle.

- [ ] **Step 7: Test and commit**

Run:

```bash
cargo fmt --all
cargo test -p kobo-bomtoon startup
cargo test -p kobo-bomtoon account_scope
cargo test -p kobo-bomtoon unresolved_marker
cargo test -p kobo-bomtoon credential_failure
cargo test -p kobo-bomtoon offline
```

Then:

```bash
git add apps/bomtoon/src/main.rs apps/bomtoon/src/api.rs
git diff --cached --name-only -- evidences
git diff --cached --check
git commit -m "feat(bomtoon): restore pending commerce"
```

### Task 7: Integrate Gift, Quote, Mutation, and Reconciliation UI

**Files:**
- Modify: `apps/bomtoon/src/main.rs`
- Modify: `apps/bomtoon/src/commerce.rs`
- Modify: `apps/bomtoon/src/api.rs`
- Modify: `apps/bomtoon/src/parse.rs`
- Modify: `apps/catalog.json`
- Modify: `crates/kobod/src/app_store.rs`

**Interfaces:**
- Consumes: Tasks 3–6.
- Produces: complete episode-page commerce behavior.

- [ ] **Step 1: Write failing episode layout tests**

With `CLARA_BW_METRICS`, assert `Coins N · Gifts N`, independent failure states, actionable `View options`, readable access labels, no Ticket UI, maximum counts, `48 hrs`, `1 hr`, `0 hrs`, and `Rented` all fit.

- [ ] **Step 2: Write failing quote-screen tests**

Kobo confirmations are retained full-screen surfaces. Build the logical modal with heading, text, divider, and four `button_with_state` calls because `confirmation` supports two actions. Assert labels, disabled reasons, Cancel/Back before marker save, and pinned navigation after marker save.

- [ ] **Step 3: Load title Gift balance**

After content detail, request title Gifts. Track one Gift task plus title generation. Ignore stale outcomes. Failure disables only Gift. Cancel Account history tasks when leaving Account so foreground + Gift + wallet summary is at most three tasks.

- [ ] **Step 4: Wire initial quote and re-quote**

Not-owned tap starts `POSSESSION` quote. Gift/Rent actions re-quote `RENT`; Buy re-quotes `POSSESSION`. Changed identity, price, eligibility, or availability emits no save and replaces/reconciles the quote.

- [ ] **Step 5: Persist then POST**

Map save commands to `context.store().save`. Only matching `Saved` can spawn `api::purchase`. Render non-actionable progress while persisting/mutating. At `AppRunner` command level, prove every POST follows save acknowledgement, not merely save request.

- [ ] **Step 6: Reconcile every outcome**

Explicit rejection clears only through acknowledged Forget. Gift success refreshes content+Gift; paid success refreshes content+wallet; unknown POST refreshes all three. Accepted entitlement cannot duplicate; same-scope no-entitlement plus exact unchanged affected balance can clear; partial/cross-account/offline/contradictory result stays `Accepted, refresh needed` with only `Refresh status`.

- [ ] **Step 7: Preserve episode page after success**

After Forgotten, retain Episodes view/title/page and refreshed balances. Permanent purchase invalidates library cache so Back refreshes owned count. Active Rent has no purchase action; `0 hrs` refreshes before reader open.

- [ ] **Step 8: Publish the compatible app version**

In the BOMTOON catalog entry set:

```json
"version": "0.5.0",
"minimum_cobalt_version": "0.4.0",
"capabilities": ["network"]
```

Add or update catalog and `kobod::app_store` tests proving a runtime reporting `0.3.x` refuses this app, the built `0.4.0` runtime accepts it, and the capability list remains exactly `network`. Do not change any other app entry.

- [ ] **Step 9: Add financial integration tests**

Cover Gift `-1`+RENT, receipt-backed Coin delta+RENT/POSSESSION, no double-counting, lost response with entitlement, lost response with unchanged state, refresh failure, cross-account/offline lock, stale generations, and task-capacity failure.

- [ ] **Step 10: Test and commit**

Run:

```bash
cargo fmt --all
cargo test -p kobo-bomtoon
cargo clippy -p kobo-bomtoon --all-targets --all-features -- -D warnings
```

Then:

```bash
git add apps/bomtoon/src/main.rs apps/bomtoon/src/commerce.rs apps/bomtoon/src/api.rs apps/bomtoon/src/parse.rs apps/catalog.json crates/kobod/src/app_store.rs
git diff --cached --name-only -- evidences
git diff --cached --check
git commit -m "feat(bomtoon): enable episode transactions"
```

### Task 8: Verify Simulators, Workspace Gates, and Attended Safety

**Files:**
- Modify: `docs/superpowers/specs/2026-08-30-bomtoon-episode-commerce-design.md` only to record verified status/evidence
- Modify source/tests only when a gate exposes a source defect

**Interfaces:**
- Consumes: Tasks 1–7.
- Produces: release evidence without committed secrets.

- [ ] **Step 1: Run focused gates**

```bash
cargo fmt --all --check
cargo test -p kobo-net bomtoon
cargo test -p kobo-policy managed
cargo test -p kobo-protocol credential_scope
cargo test -p kobo-protocol version
cargo test -p kobo-app-store catalog
cargo test -p kobod app_store
cargo test -p kobo-bomtoon
cargo clippy -p kobo-bomtoon --all-targets --all-features -- -D warnings
```

Expected: all exit zero.

- [ ] **Step 2: Run workspace gates**

```bash
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all exit zero; fix responsible source rather than suppressing or narrowing.

- [ ] **Step 3: Exercise browser simulator**

From `apps/bomtoon`:

```bash
cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

Use browser automation on the actual surface. Exercise signed-out, offline, Gift unavailable, insufficient Coin, quote actions, Cancel/Back, rental labels, different-account lock, and accepted-but-stale refresh. Never expose live account data.

- [ ] **Step 4: Exercise runtime simulator**

From repository root:

```bash
cargo run -p kobo-cli -- run --sim --app bomtoon
```

Verify action dispatch, Back ownership, progress surfaces, store callbacks, restart marker load, `Join Wi-Fi`, reader entry, and return to the same episode page. Do not submit a real mutation.

- [ ] **Step 5: Prove restart/account isolation without spending**

Interrupt simulator-controlled flows before Saved, between Saved and POST, during POST, after receipt before refresh, and during Forget. Restart and prove no second POST. Switch to another fake scope and prove reading remains available while commerce stays locked.

- [ ] **Step 6: Run attended backend checks**

Stop for the operator. The operator—not the agent—performs one lowest-cost Gift rental, Coin rental, and Coin purchase. Record only purchase type/price, success classification, aggregate `useCoin`, affected pre/post balance, refreshed `RENT`/`POSSESSION`, and expiry. Induce one post-accept refresh failure and verify no repeat POST. Revoke the capture session afterward; never add HARs.

- [ ] **Step 7: Record status and staged safety**

Update design status with exact commands/scenarios that passed; no raw responses or identifiers. Run:

```bash
git diff --check
git diff --cached --name-only -- evidences
```

Expected: no output.

- [ ] **Step 8: Commit verification evidence**

```bash
git add docs/superpowers/specs/2026-08-30-bomtoon-episode-commerce-design.md
git diff --cached --name-only -- evidences
git diff --cached --check
git commit -m "test(bomtoon): verify episode commerce"
```

## Completion Criteria

- Every Task 1–8 checkbox is complete.
- Marker save is acknowledged before POST; deletion is acknowledged before another mutation.
- Same-account restart reconciles; different-account, signed-out, expired, and offline states cannot bypass the marker.
- Exact credential policy permits approved GET/POST shapes and denies every tested variant.
- Protocol v13 rejects v12 peers before task dispatch; the built runtime reports Cobalt `0.4.0`; BOMTOON `0.5.0` requires Cobalt `0.4.0` and retains only `network`.
- Gift rental, Coin rental, and Coin purchase refresh authoritative entitlement and balances.
- Ticket remains Account-only; no episode Ticket field, label, action, alias, or compatibility path remains.
- Browser and runtime simulators exercise the actual UI.
- Focused and workspace test/Clippy/format gates pass.
- Attended checks record sanitized deltas/status only, and capture credentials are revoked.
- `evidences/` is absent from every commit.
