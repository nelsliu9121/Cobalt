# BOMTOON Wallet Balances Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collect BOMTOON coin and ticket information, show aggregate coins in library top bars, show complete Account detail on demand, and expose applicable tickets on Comic/Episodes pages.

**Architecture:** Add bounded wallet models, exact bearer-authenticated API tasks, and strict parsers inside the existing BOMTOON app. `Bomtoon` owns a generation-checked background wallet task registry independent of its existing foreground request and reader registries. One re-entrant summary refresh path serves startup, Account, retry, and future purchase/rent completion.

**Tech Stack:** Rust 2021, Rust 1.85.1, existing `kobo-json`, `kobo-sdk::Task`, `ScreenBuilder`, `AppRunner`, and `CLARA_BW_METRICS`; no new dependency.

## Global Constraints

- API behavior is derived from BOMTOON production Next.js build `OwHQUV9KN_XCsgOkqJPqz`; no authenticated response was captured.
- Summary endpoint: `GET https://www.bomtoon.tw/api/balcony-api-v2/asset/user`, maximum 64 KiB.
- Detail endpoint: `GET https://www.bomtoon.tw/api/balcony-api-v2/payment/charge`, maximum 512 KiB per request.
- Detail query: 90-day `createdAt` window, `sort=EXPIRE`, and exactly one of `coinKind=COIN` or `coinKind=TICKET`.
- Each detail result accepts at most 256 remote entries, 256 flattened display rows, and 256 UTF-8 bytes per retained description.
- Use only `Credential::bearer("bomtoon-access-token")` and the existing Balcony headers; credentials never enter URLs, ordinary headers, state, errors, or logs.
- Top bars display aggregate coins only. Tickets appear on Account and ticket-applicable Comic/Episodes pages.
- Wallet failures never block reading or clear library/content state unless the error is `NoCredential` or `Unauthorized`.
- No purchasing, renting, charging, mutation endpoint, polling, persistence, dependency, SDK, protocol, runtime, CLI, catalog, or policy change.
- All new behavior is covered beside the Rust implementation and every changed screen is checked with `CLARA_BW_METRICS`.

---

### Task 1: Model bounded wallet and ticket data

**Files:**
- Modify: `apps/bomtoon/src/model.rs:1-98`
- Test: `apps/bomtoon/src/model.rs:98-210`

**Interfaces:**
- Consumes: existing `Episode`, `PurchaseState`, and `EpisodeAvailability`.
- Produces:
  - `AssetKind::{Coin, Ticket}`
  - `AssetSubtype::{Standard, Bonus, Free}`
  - `AssetAmounts { standard, bonus, free }`
  - `AssetAmounts::total(self) -> Option<usize>`
  - `WalletSummary { coins, tickets }`
  - `ExpirationRow { kind, subtype, quantity, expires_at, description }`
  - `Episode::ticket_quantity: Option<usize>`
  - `Episode::uses_ticket(&self) -> bool`

- [ ] **Step 1: Write failing aggregate and ticket-state tests**

Add imports and tests that establish checked totals and make ticket applicability unrepresentable without a quantity:

```rust
use super::{
    display_text, AssetAmounts, Episode, EpisodeAvailability, PurchaseState,
};

#[test]
fn asset_amounts_total_is_checked() {
    assert_eq!(
        AssetAmounts {
            standard: 7,
            bonus: 2,
            free: 1,
        }
        .total(),
        Some(10)
    );
    assert_eq!(
        AssetAmounts {
            standard: usize::MAX,
            bonus: 1,
            free: 0,
        }
        .total(),
        None
    );
}

#[test]
fn only_episodes_with_a_ticket_quantity_use_tickets() {
    let ticket = Episode {
        alias: "ticket".to_owned(),
        title: "Ticket episode".to_owned(),
        purchase: PurchaseState::NotOwned,
        ticket_quantity: Some(1),
    };
    let coin = Episode {
        alias: "coin".to_owned(),
        title: "Coin episode".to_owned(),
        purchase: PurchaseState::NotOwned,
        ticket_quantity: None,
    };
    assert!(ticket.uses_ticket());
    assert!(!coin.uses_ticket());
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p kobo-bomtoon asset_amounts_total_is_checked
cargo test -p kobo-bomtoon only_episodes_with_a_ticket_quantity_use_tickets
```

Expected: compilation fails because `AssetAmounts` and `Episode::ticket_quantity` do not exist.

- [ ] **Step 3: Add the minimal domain types**

Place the wallet types after `RecentEntry` and extend `Episode`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetKind {
    Coin,
    Ticket,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetSubtype {
    Standard,
    Bonus,
    Free,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AssetAmounts {
    pub standard: usize,
    pub bonus: usize,
    pub free: usize,
}

impl AssetAmounts {
    pub fn total(self) -> Option<usize> {
        self.standard.checked_add(self.bonus)?.checked_add(self.free)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalletSummary {
    pub coins: AssetAmounts,
    pub tickets: AssetAmounts,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpirationRow {
    pub kind: AssetKind,
    pub subtype: AssetSubtype,
    pub quantity: usize,
    pub expires_at: Option<i64>,
    pub description: Option<String>,
}
```

Extend `Episode` with `pub ticket_quantity: Option<usize>` and add:

```rust
impl Episode {
    #[must_use]
    pub const fn uses_ticket(&self) -> bool {
        self.ticket_quantity.is_some()
    }
}
```

Update every existing `Episode` literal in tests with `ticket_quantity: None`.

- [ ] **Step 4: Run model tests and verify GREEN**

Run: `cargo test -p kobo-bomtoon model::tests`

Expected: all model tests pass, including overflow returning `None`.

- [ ] **Step 5: Commit the domain model**

```bash
git add apps/bomtoon/src/model.rs
git commit -m "feat(bomtoon): model wallet balances"
```

---

### Task 2: Build exact summary and history requests

**Files:**
- Modify: `apps/bomtoon/src/api.rs:3-104`
- Test: `apps/bomtoon/src/api.rs:106-288`

**Interfaces:**
- Consumes: `model::AssetKind`, existing `fetch`, `balcony_headers`, and managed bearer credential.
- Produces:
  - `api::asset_summary() -> Task`
  - `api::expiration_history(kind: AssetKind, created_at: i64) -> Task`
  - constants `ASSET_SUMMARY_BYTES = 64 * 1024` and `ASSET_HISTORY_BYTES = 512 * 1024`

- [ ] **Step 1: Write failing exact-request tests**

Extend the test imports and add:

```rust
use crate::model::AssetKind;
use super::{asset_summary, expiration_history};

#[test]
fn asset_summary_uses_exact_bearer_endpoint_and_ceiling() {
    let Task::Fetch {
        url,
        offset,
        max_bytes,
        credential,
        headers,
    } = asset_summary()
    else {
        panic!("expected fetch task");
    };
    assert_eq!(url, "https://www.bomtoon.tw/api/balcony-api-v2/asset/user");
    assert_eq!(offset, 0);
    assert_eq!(max_bytes, 64 * 1024);
    assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
    assert!(headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("accept") && header.value == "application/json"
    }));
}

#[test]
fn expiration_history_fixes_kind_sort_window_and_ceiling() {
    for (kind, value) in [(AssetKind::Coin, "COIN"), (AssetKind::Ticket, "TICKET")] {
        let Task::Fetch {
            url,
            max_bytes,
            credential,
            ..
        } = expiration_history(kind, 1_725_000_000_000)
        else {
            panic!("expected fetch task");
        };
        assert_eq!(
            url,
            format!(
                "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge?createdAt=1725000000000&sort=EXPIRE&coinKind={value}"
            )
        );
        assert_eq!(max_bytes, 512 * 1024);
        assert_eq!(credential, Some(Credential::bearer("bomtoon-access-token")));
    }
}
```

Add `asset_summary()` and both history tasks to `credentials_never_enter_urls_or_regular_headers`.

- [ ] **Step 2: Run API tests and verify RED**

Run: `cargo test -p kobo-bomtoon api::tests`

Expected: compilation fails because the request constructors do not exist.

- [ ] **Step 3: Implement fixed request constructors**

Add:

```rust
use crate::model::AssetKind;

const ASSET_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/asset/user";
const CHARGE_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/payment/charge";
const ASSET_SUMMARY_BYTES: u32 = 64 * 1024;
const ASSET_HISTORY_BYTES: u32 = 512 * 1024;

pub fn asset_summary() -> Task {
    fetch(
        ASSET_URL.to_owned(),
        ASSET_SUMMARY_BYTES,
        Credential::bearer("bomtoon-access-token"),
        balcony_headers(),
    )
}

pub fn expiration_history(kind: AssetKind, created_at: i64) -> Task {
    let coin_kind = match kind {
        AssetKind::Coin => "COIN",
        AssetKind::Ticket => "TICKET",
    };
    fetch(
        format!(
            "{CHARGE_URL}?createdAt={created_at}&sort=EXPIRE&coinKind={coin_kind}"
        ),
        ASSET_HISTORY_BYTES,
        Credential::bearer("bomtoon-access-token"),
        balcony_headers(),
    )
}
```

Do not add `coinStatusGroup`: the approved request needs every valid row in the fixed window.

- [ ] **Step 4: Run API tests and verify GREEN**

Run: `cargo test -p kobo-bomtoon api::tests`

Expected: all exact endpoint, header, credential-separation, and ceiling tests pass.

- [ ] **Step 5: Commit the API tasks**

```bash
git add apps/bomtoon/src/api.rs
git commit -m "feat(bomtoon): add wallet API requests"
```

---

### Task 3: Parse wallet responses and ticket-priced episodes

**Files:**
- Modify: `apps/bomtoon/src/parse.rs:1-199`
- Modify: `apps/bomtoon/src/parse.rs:200-340`
- Test: `apps/bomtoon/src/parse.rs:341-700`

**Interfaces:**
- Consumes: Task 1 wallet types and existing JSON helpers.
- Produces:
  - `parse::asset_summary(bytes: &[u8]) -> Result<WalletSummary, ParseError>`
  - `parse::expiration_history(bytes: &[u8], kind: AssetKind) -> Result<Vec<ExpirationRow>, ParseError>`
  - `parse::episodes` sets `Episode::ticket_quantity` only for exact `coinKind == "TICKET"`

- [ ] **Step 1: Write failing summary parser tests**

Use one production-bundle-shaped fixture:

```rust
#[test]
fn asset_summary_parses_remote_buckets_and_checked_totals() {
    let summary = asset_summary(br#"{
      "result":"SUCCESS",
      "data":{
        "coinBalance":{"coin":7,"bonusCoin":2,"freeCoin":1},
        "ticketBalance":{"ticket":3,"bonusTicket":1,"freeTicket":0}
      }
    }"#).expect("summary");
    assert_eq!(summary.coins.total(), Some(10));
    assert_eq!(summary.tickets.total(), Some(4));
}

#[test]
fn asset_summary_rejects_missing_wrong_negative_and_overflowing_amounts() {
    for body in [
        br#"{"result":"SUCCESS","data":{"coinBalance":{},"ticketBalance":{"ticket":0,"bonusTicket":0,"freeTicket":0}}}"#.as_slice(),
        br#"{"result":"SUCCESS","data":{"coinBalance":{"coin":"7","bonusCoin":0,"freeCoin":0},"ticketBalance":{"ticket":0,"bonusTicket":0,"freeTicket":0}}}"#.as_slice(),
        br#"{"result":"SUCCESS","data":{"coinBalance":{"coin":-1,"bonusCoin":0,"freeCoin":0},"ticketBalance":{"ticket":0,"bonusTicket":0,"freeTicket":0}}}"#.as_slice(),
    ] {
        assert!(asset_summary(body).is_err());
    }
}
```

Add a target-width overflow fixture conditionally with `usize::MAX` serialized and another positive component so checked aggregation returns `ParseError::InvalidValue("coin total")`.

- [ ] **Step 2: Write failing history and episode ticket tests**

```rust
#[test]
fn expiration_history_flattens_nonzero_components_in_server_order() {
    let rows = expiration_history(
        br#"{
          "result":"SUCCESS",
          "data":[{
            "title":"Signup gift",
            "coin":2,
            "coinExpiredAt":1756684800000,
            "bonusCoin":1,
            "bonusCoinExpiredAt":0,
            "freeCoin":0
          }]
        }"#,
        AssetKind::Coin,
    ).expect("history");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].subtype, AssetSubtype::Standard);
    assert_eq!(rows[0].quantity, 2);
    assert_eq!(rows[0].expires_at, Some(1_756_684_800_000));
    assert_eq!(rows[1].subtype, AssetSubtype::Bonus);
    assert_eq!(rows[1].expires_at, None);
}

#[test]
fn ticket_coin_kind_requires_and_retains_effective_quantity() {
    let parsed = episodes(br#"{
      "result":"SUCCESS",
      "data":{"episodes":[
        {"alias":"ticket","title":"Ticket","purchaseStatus":"NONE","isSample":false,"coinKind":"TICKET","rentCoin":1},
        {"alias":"coin","title":"Coin","purchaseStatus":"NONE","isSample":false,"coinKind":"COIN","rentCoin":3}
      ]}
    }"#).expect("episodes");
    assert_eq!(parsed[0].ticket_quantity, Some(1));
    assert_eq!(parsed[1].ticket_quantity, None);
}
```

Also test 257 history entries, a valid input whose nonzero components flatten to 257 display rows, a 257-byte description, an invalid timestamp type, a zero quantity, and `coinKind: "TICKET"` with neither `rentCoin` nor `possessionCoin`.

- [ ] **Step 3: Run parser tests and verify RED**

Run: `cargo test -p kobo-bomtoon parse::tests`

Expected: compilation fails for missing parser functions and `Episode::ticket_quantity` remains unset.

- [ ] **Step 4: Implement strict bounded parsing**

Add `MAX_HISTORY_ENTRIES: usize = 256`, `MAX_HISTORY_ROWS: usize = 256`, and `MAX_HISTORY_DESCRIPTION_BYTES: usize = 256`. Parse the summary through one helper:

```rust
fn asset_amounts(
    value: &Value,
    standard: (&str, &'static str),
    bonus: (&str, &'static str),
    free: (&str, &'static str),
    total_name: &'static str,
) -> Result<AssetAmounts, ParseError> {
    let amounts = AssetAmounts {
        standard: unsigned(value, standard.0, standard.1)?,
        bonus: unsigned(value, bonus.0, bonus.1)?,
        free: unsigned(value, free.0, free.1)?,
    };
    amounts
        .total()
        .ok_or(ParseError::InvalidValue(total_name))?;
    Ok(amounts)
}
```

Implement `asset_summary` with exact coin and ticket field names. Implement history by checking the remote-entry cap before allocation, validating an optional bounded `title`, and flattening nonzero `standard`, `bonus`, and `free` components in that order. Before each push, reject output at `MAX_HISTORY_ROWS`; do not truncate. Convert zero/missing expiry timestamps to `None`; reject negative, non-integer, or values larger than `i64::MAX`.

In `episodes`, parse `coinKind` with `optional_string`. For exact `TICKET`, set the quantity to `rentCoin` when present, otherwise `possessionCoin`; return `ParseError::Missing("episode ticket quantity")` when neither exists. Preserve the current `PurchaseState::from_remote` precedence.

- [ ] **Step 5: Run parser and model tests and verify GREEN**

Run:

```bash
cargo test -p kobo-bomtoon parse::tests
cargo test -p kobo-bomtoon model::tests
```

Expected: valid fixtures parse, every limit fails closed, and legacy episode fixtures remain non-ticket episodes.

- [ ] **Step 6: Commit strict parsing**

```bash
git add apps/bomtoon/src/model.rs apps/bomtoon/src/parse.rs
git commit -m "feat(bomtoon): parse wallet balances"
```

---

### Task 4: Add generation-checked summary refresh state

**Files:**
- Modify: `apps/bomtoon/src/main.rs:14-267`
- Modify: `apps/bomtoon/src/main.rs:530-585`
- Modify: `apps/bomtoon/src/main.rs:1247-1310`
- Modify: `apps/bomtoon/src/main.rs:1791-1932`
- Test: `apps/bomtoon/src/main.rs:2649-3300`
- Test: `apps/bomtoon/src/main.rs:4880-5126`

**Interfaces:**
- Consumes: `api::asset_summary`, `parse::asset_summary`, and `WalletSummary`.
- Produces:
  - `WalletTaskPurpose::Summary { generation: u64 }`
  - `WalletState::summary: Option<WalletSummary>`
  - `Bomtoon::refresh_asset_summary(&mut self, context: &mut Context)`
  - queued refresh coalescing and stale-outcome rejection

- [ ] **Step 1: Update test helpers for parallel startup**

Keep `only_spawn` for flows that still spawn exactly one task. Add:

```rust
fn spawns(commands: &[Command]) -> Vec<(TaskId, Task)> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::Spawn { task, work } => Some((*task, work.clone())),
            _ => None,
        })
        .collect()
}

fn fetch_task_with(commands: &[Command], needle: &str) -> (TaskId, Task) {
    spawns(commands)
        .into_iter()
        .find(|(_, work)| matches!(work, Task::Fetch { url, .. } if url.contains(needle)))
        .expect("matching fetch task")
}
```

Change `loaded_library_with_metrics` to settle the task whose URL contains `/library?`; leave the summary task active unless a test explicitly settles it. Update `failed_start` and startup tests to select the intended URL rather than call `only_spawn`.

- [ ] **Step 2: Write failing startup, coalescing, and stale-result tests**

Add an `ASSET_RESPONSE` fixture with 10 coins and 4 tickets. Add tests:

```rust
#[test]
fn startup_loads_library_and_asset_summary_independently() {
    let (_, commands) = started();
    let spawned = spawns(&commands);
    assert_eq!(spawned.len(), 2);
    assert!(spawned.iter().any(|(_, work)| matches!(work, Task::Fetch { url, .. } if url.contains("/library?"))));
    assert!(spawned.iter().any(|(_, work)| matches!(work, Task::Fetch { url, .. } if url.ends_with("/asset/user"))));
}

#[test]
fn summary_refresh_requests_coalesce_into_one_follow_up() {
    let mut wallet = WalletState::default();
    wallet.summary_task = Some(TaskId(7));
    assert_eq!(wallet.request_summary_generation(), None);
    assert_eq!(wallet.request_summary_generation(), None);
    assert!(wallet.summary_refresh_queued);

    wallet.summary_task = None;
    assert!(wallet.take_queued_summary_refresh());
    assert!(!wallet.take_queued_summary_refresh());
}
```

Add `stale_summary_generation_is_rejected`: obtain generation 1 from `request_summary_generation`, advance `summary_generation` to generation 2, pass a parsed 10-coin summary to `accept_summary(1, summary)`, and assert it returns `false` without replacing the generation-2 cache.

```rust
#[test]
fn stale_summary_generation_is_rejected() {
    let mut wallet = WalletState::default();
    let stale = wallet.request_summary_generation().expect("generation 1");
    let current = wallet.request_summary_generation().expect("generation 2");
    assert_ne!(stale, current);
    wallet.summary = Some(WalletSummary {
        coins: AssetAmounts { standard: 20, bonus: 0, free: 0 },
        tickets: AssetAmounts::default(),
    });
    assert!(!wallet.accept_summary(
        stale,
        WalletSummary {
            coins: AssetAmounts { standard: 10, bonus: 0, free: 0 },
            tickets: AssetAmounts::default(),
        },
    ));
    assert_eq!(
        wallet.summary.and_then(|summary| summary.coins.total()),
        Some(20)
    );
}
```

Add lifecycle tests through `AppRunner`:

```rust
#[test]
fn wallet_credential_failure_cancels_library_and_ignores_its_late_outcome() {
    let (mut runner, commands) = started();
    let (library_task, _) = fetch_task_with(&commands, "/library?");
    let (summary_task, _) = fetch_task_with(&commands, "/asset/user");
    runner.task_outcome(summary_task, TaskOutcome::Failed(TaskError::NoCredential));
    assert_eq!(runner.app().account, AccountState::SignedOut);
    assert_eq!(runner.app().task, None);
    assert_eq!(runner.app().pending, None);

    runner.task_outcome(
        library_task,
        TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()),
    );
    assert_eq!(runner.app().account, AccountState::SignedOut);
    assert!(runner.app().comics.is_empty());
}

#[test]
fn sign_out_clears_wallet_and_ignores_late_summary() {
    let (mut runner, commands) = started();
    let (library_task, _) = fetch_task_with(&commands, "/library?");
    let (summary_task, _) = fetch_task_with(&commands, "/asset/user");
    runner.task_outcome(
        library_task,
        TaskOutcome::Completed(LIBRARY_RESPONSE.to_vec()),
    );
    let logout_task = begin_logout(&mut runner);
    runner.task_outcome(logout_task, TaskOutcome::Completed(Vec::new()));
    assert!(runner.app().wallet.summary.is_none());
    assert!(runner.app().wallet.tasks.is_empty());

    runner.task_outcome(
        summary_task,
        TaskOutcome::Completed(ASSET_RESPONSE.to_vec()),
    );
    assert!(runner.app().wallet.summary.is_none());
}
```


- [ ] **Step 3: Run focused app tests and verify RED**

Run:

```bash
cargo test -p kobo-bomtoon startup_loads_library_and_asset_summary_independently
cargo test -p kobo-bomtoon summary_refresh_requests_coalesce_into_one_follow_up
cargo test -p kobo-bomtoon stale_summary_generation_is_rejected
cargo test -p kobo-bomtoon wallet_credential_failure_cancels_library
cargo test -p kobo-bomtoon sign_out_clears_wallet
```
Expected: startup still has one spawn and wallet state does not exist.

- [ ] **Step 4: Implement wallet summary state and outcome routing**

Add:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WalletTaskPurpose {
    Summary { generation: u64 },
    CoinHistory { generation: u64 },
    TicketHistory { generation: u64 },
}

#[derive(Default)]
struct WalletState {
    summary: Option<WalletSummary>,
    summary_error: bool,
    summary_stale: bool,
    summary_generation: u64,
    summary_task: Option<TaskId>,
    summary_refresh_queued: bool,
    detail_generation: u64,
    tasks: BTreeMap<TaskId, WalletTaskPurpose>,
    coin_history: Vec<ExpirationRow>,
    ticket_history: Vec<ExpirationRow>,
    coin_history_error: bool,
    ticket_history_error: bool,
}
```

Implement the pure coalescing operations first:

```rust
impl WalletState {
    fn request_summary_generation(&mut self) -> Option<u64> {
        if self.summary_task.is_some() {
            self.summary_refresh_queued = true;
            return None;
        }
        self.summary_generation = self.summary_generation.wrapping_add(1);
        Some(self.summary_generation)
    }

    fn take_queued_summary_refresh(&mut self) -> bool {
        std::mem::take(&mut self.summary_refresh_queued)
    }

    fn accept_summary(&mut self, generation: u64, summary: WalletSummary) -> bool {
        if generation != self.summary_generation {
            return false;
        }
        self.summary = Some(summary);
        self.summary_error = false;
        self.summary_stale = false;
        true
    }
}
```

Embed `wallet: WalletState` in `Bomtoon`. Implement `refresh_asset_summary` by calling `request_summary_generation`; when it returns a generation, spawn `api::asset_summary`, store its `TaskId` in both `summary_task` and `tasks`, and mark an unavailable error if spawning fails. Add `handle_wallet_outcome` before reader and foreground task matching in `on_task`.

On completed summary, parse and update only the matching generation. On transient failure or parse failure, retain cached summary, set `summary_error`, and set `summary_stale` only when cached data exists. On settlement, clear `summary_task` and start exactly one queued follow-up.

Call `refresh_asset_summary` after the library request is spawned in `restart`; it must not set `pending` or replace the visible screen.

- [ ] **Step 5: Implement credential and cleanup behavior**

Add `cancel_wallet(context)` that increments both generations, cancels every wallet task, and clears active IDs and the queued refresh. When a wallet task reports `NoCredential` or `Unauthorized`, also cancel `self.task.take()` through `Context::cancel`, clear `pending`, clear all account-owned data, and set the existing SignedOut or Expired state. This prevents an overlapping late library outcome from restoring content after a wallet credential failure. Existing library/content credential failures call the same wallet cleanup through `clear_account_data`.

- [ ] **Step 6: Run focused and existing startup tests**

Run:

```bash
cargo test -p kobo-bomtoon startup_
cargo test -p kobo-bomtoon summary_refresh_
cargo test -p kobo-bomtoon library_recent_and_content_credential_errors_show_account_states
cargo test -p kobo-bomtoon reader_cleanup_
```

Expected: parallel startup is explicit, only one queued refresh runs, stale outcomes are ignored, and existing reader cleanup remains unchanged.

- [ ] **Step 7: Commit summary orchestration**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): refresh wallet summary"
```

---

### Task 5: Display coins and implement Account detail

**Files:**
- Modify: `apps/bomtoon/src/main.rs:14-66`
- Modify: `apps/bomtoon/src/main.rs:238-499`
- Modify: `apps/bomtoon/src/main.rs:1247-1310`
- Modify: `apps/bomtoon/src/main.rs:1817-1900`
- Test: `apps/bomtoon/src/main.rs:3200-3280`
- Test: `apps/bomtoon/src/main.rs:4909-5250`

**Interfaces:**
- Consumes: Task 4 wallet summary state; `api::expiration_history`; `parse::expiration_history`.
- Produces:
  - `View::Account`
  - `Bomtoon::account_screen() -> Screen`
  - `Bomtoon::open_account`, `refresh_account_details`, and history outcome handling
  - Library/Recent top-bar aggregate coin copy

- [ ] **Step 1: Write failing top-bar tests**

Set a summary directly in the loaded-library helper and assert exact copy:

```rust
#[test]
fn library_and_recent_top_bars_show_only_aggregate_coins() {
    let (mut runner, _) = loaded_library();
    runner.app_mut().wallet.summary = Some(WalletSummary {
        coins: AssetAmounts { standard: 7, bonus: 2, free: 1 },
        tickets: AssetAmounts { standard: 3, bonus: 1, free: 0 },
    });
    let screen = runner.app().screen();
    let drawn = format!("{screen:?}");
    assert!(drawn.contains("Library · Coins 10"));
    assert!(!drawn.contains("Tickets"));
    assert_fits(&screen);

    runner.app_mut().shelf = Shelf::Recent;
    let screen = runner.app().screen();
    let drawn = format!("{screen:?}");
    assert!(drawn.contains("Recent · Coins 10"));
    assert!(!drawn.contains("Tickets"));
    assert_fits(&screen);
}
```

Add initial-loading and unavailable tests using `None` with an active summary task and `summary_error = true`.

- [ ] **Step 2: Write failing Account load, Back, partial-failure, and layout tests**

Add `ACCOUNT`, `RETRY_BALANCES`, `ACCOUNT_PREVIOUS`, and `ACCOUNT_NEXT` actions. Test that Account:

- preserves the exact library shelf/page;
- re-queries `/asset/user` and starts COIN/TICKET history requests;
- renders cached summary immediately;
- keeps coin rows when ticket history fails;
- formats timestamps as `YYYY-MM-DD`;
- paginates bounded combined rows;
- returns to the prior Library/Recent page on Back;
- passes `assert_fits` with maximum `usize` counts, unavailable copy, empty history, and a full page.

Use exact assertions such as:

```rust
assert!(drawn.contains("Coins"));
assert!(drawn.contains("10"));
assert!(drawn.contains("Tickets"));
assert!(drawn.contains("4"));
assert!(drawn.contains("Expires 2027-09-01"));
assert!(drawn.contains("Retry balances"));
```

- [ ] **Step 3: Run UI tests and verify RED**

Run:

```bash
cargo test -p kobo-bomtoon library_and_recent_top_bars_show_only_aggregate_coins
cargo test -p kobo-bomtoon account_
```

Expected: top bars still show only shelf names and `View::Account` does not exist.

- [ ] **Step 4: Implement top-bar states and Account navigation**

Add `View::Account`. Build the shelf title through one helper:

```rust
fn library_title(&self) -> String {
    let shelf = if self.shelf == Shelf::Recent { "Recent" } else { "Library" };
    match self.wallet.summary.and_then(|summary| summary.coins.total()) {
        Some(total) => format!("{shelf} · Coins {total}"),
        None if self.wallet.summary_task.is_some() => format!("{shelf} · Coins…"),
        None => format!("{shelf} · Coins unavailable"),
    }
}
```

Use it in `library_screen`, add the Account action, and make Account own Back without mutating the remembered library page.

- [ ] **Step 5: Implement history requests and Account rendering**

Compute the request start with this checked helper:

```rust
const HISTORY_WINDOW_MS: i64 = 90 * 24 * 60 * 60 * 1_000;

fn history_start_ms() -> Option<i64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let now = i64::try_from(elapsed.as_millis()).ok()?;
    now.checked_sub(HISTORY_WINDOW_MS)
}
```

A clock conversion failure marks both detail sections unavailable without affecting summary or reading. `refresh_account_details` increments `detail_generation`, cancels older detail tasks only, and starts one COIN and one TICKET history request. Account open also calls `refresh_asset_summary`.

Render summary through `facts`, then a bounded combined list through `paged_list`; prefix each row with `Coin` or `Ticket`, subtype, quantity, and one expiration label. Format Asia/Taipei dates without a dependency:

```rust
fn taipei_date(timestamp_ms: i64) -> Option<String> {
    const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
    const TAIPEI_OFFSET_MS: i64 = 8 * 60 * 60 * 1_000;
    let days = timestamp_ms.checked_add(TAIPEI_OFFSET_MS)?.div_euclid(DAY_MS);
    let shifted = days.checked_add(719_468)?;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096)
            / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    Some(format!("{year:04}-{month:02}-{day:02}"))
}
```

Test `taipei_date(0) == Some("1970-01-01")`, `taipei_date(1_709_136_000_000) == Some("2024-02-29")`, and `taipei_date(1_735_660_800_000) == Some("2025-01-01")`. Retry always refreshes summary and restarts only detail sections whose error flag is set. Partial success stays cached and visible.


- [ ] **Step 6: Run Account and library tests and verify GREEN**

Run:

```bash
cargo test -p kobo-bomtoon library_and_recent_top_bars_
cargo test -p kobo-bomtoon account_
cargo test -p kobo-bomtoon wallet_date_
```

Expected: exact coin-only titles, independent Account sections, Back restoration, deterministic dates, and no Clara layout errors.

- [ ] **Step 7: Commit Account UI**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): add wallet account view"
```

---

### Task 6: Show matching tickets on Comic/Episodes pages

**Files:**
- Modify: `apps/bomtoon/src/main.rs:477-499`
- Modify: `apps/bomtoon/src/main.rs:1817-1900`
- Test: `apps/bomtoon/src/main.rs:2657-2694`
- Test: `apps/bomtoon/src/main.rs:3768-3810`

**Interfaces:**
- Consumes: `Episode::uses_ticket`, `Episode::ticket_quantity`, and cached `WalletSummary::tickets.total()`.
- Produces: ticket-applicable header count and per-episode quantity labels without enabling purchase.

- [ ] **Step 1: Extend the content fixture and write failing presentation tests**

Add ticket metadata to locked fixture rows while retaining readable fixtures:

```json
{"alias":"ticket","title":"Ticket Episode","isSample":false,"purchaseStatus":"NONE","paid":true,"coinKind":"TICKET","rentCoin":1}
```

Add tests:

```rust
#[test]
fn ticket_comic_shows_cached_ticket_total_and_episode_quantity() {
    let (mut runner, _) = loaded_library();
    runner.app_mut().wallet.summary = Some(WalletSummary {
        coins: AssetAmounts::default(),
        tickets: AssetAmounts { standard: 3, bonus: 1, free: 0 },
    });
    let commands = runner.action(action_id("comic-0"));
    let (content_task, _) = only_spawn(&commands);
    let commands = runner.task_outcome(
        content_task,
        TaskOutcome::Completed(CONTENT_RESPONSE.to_vec()),
    );
    let screen = last_screen(&commands);
    let drawn = format!("{screen:?}");
    assert!(drawn.contains("Tickets 4"));
    assert!(drawn.contains("Ticket · 1"));
    assert_fits(&screen);
}

#[test]
fn coin_only_comic_has_no_ticket_ui() {
    let (mut runner, _) = loaded_library();
    let commands = runner.action(action_id("comic-0"));
    let (content_task, _) = only_spawn(&commands);
    let commands = runner.task_outcome(
        content_task,
        TaskOutcome::Completed(COIN_ONLY_CONTENT_RESPONSE.to_vec()),
    );
    assert!(!format!("{:?}", last_screen(&commands)).contains("Tickets"));
}
```

Also test a ticket comic with no cached summary: header says `Tickets unavailable`, each applicable episode still says `Ticket · N`, and no wallet fetch starts on comic open.

- [ ] **Step 2: Run ticket UI tests and verify RED**

Run:

```bash
cargo test -p kobo-bomtoon ticket_comic_
cargo test -p kobo-bomtoon coin_only_comic_has_no_ticket_ui
```

Expected: episode screen contains no ticket header or label.

- [ ] **Step 3: Implement ticket-aware episode copy**

In `episode_screen`, detect `self.episodes.iter().any(Episode::uses_ticket)`. Add one header line only for such comics:

```rust
let ticket_text = self
    .wallet
    .summary
    .and_then(|summary| summary.tickets.total())
    .map_or_else(|| "Tickets unavailable".to_owned(), |total| format!("Tickets {total}"));
```

For each ticket episode, append ` · Ticket · {quantity}` to the existing status label. Preserve actionability strictly through `PurchaseState::is_readable`; a ticket label never turns a locked row into a button. Do not call `refresh_asset_summary` from comic open.

- [ ] **Step 4: Run ticket, episode, and reader-entry tests and verify GREEN**

Run:

```bash
cargo test -p kobo-bomtoon ticket_
cargo test -p kobo-bomtoon owned_sample_and_free_episode_rows_are_actions
cargo test -p kobo-bomtoon owned_episode_opens_full_screen_reader_with_hidden_chrome
```

Expected: tickets appear only where applicable, locked ticket rows remain text, and readable episodes still enter the reader.

- [ ] **Step 5: Commit ticket presentation**

```bash
git add apps/bomtoon/src/main.rs
git commit -m "feat(bomtoon): show applicable tickets"
```

---

### Task 7: Verify the complete feature and clean up

**Files:**
- Modify only if verification exposes a defect: `apps/bomtoon/src/api.rs`, `apps/bomtoon/src/model.rs`, `apps/bomtoon/src/parse.rs`, `apps/bomtoon/src/main.rs`
- Verify: `docs/superpowers/specs/2026-08-30-bomtoon-wallet-balances-design.md`

**Interfaces:**
- Consumes: all prior tasks.
- Produces: formatted, warning-free, simulator-exercised BOMTOON wallet behavior matching the approved specification.

- [ ] **Step 1: Run formatting**

Run: `cargo fmt --all --check`

Expected: exit 0. If it fails, run `cargo fmt --all`, inspect only formatter changes, and rerun the check.

- [ ] **Step 2: Run the complete BOMTOON test target**

Run: `cargo test -p kobo-bomtoon`

Expected: all API, model, parser, AppRunner, reader regression, and layout tests pass.

- [ ] **Step 3: Run focused Clippy**

Run: `cargo clippy -p kobo-bomtoon --all-targets -- -D warnings`

Expected: exit 0 with no warnings.

- [ ] **Step 4: Exercise the browser simulator**

With simulator credentials provisioned through the existing attended login flow, run from `apps/bomtoon`:

```bash
cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

Verify without purchasing or renting:

1. Library appears while wallet summary resolves.
2. Library and Recent top bars show coins only.
3. Account opens with cached totals, refreshes, and displays coin/ticket history or section-local unavailable states.
4. Back returns to the same shelf and page.
5. A ticket-applicable comic shows the ticket total and per-episode quantity.
6. A coin-only comic shows no ticket UI.

Expected: all changed screens fit, remain interactive, and issue no mutation request.

- [ ] **Step 5: Exercise the runtime simulator**

Run from the repository root:

```bash
cargo run -p kobo-cli -- run --sim --app bomtoon
```

Repeat the read-only Library, Account, Back, ticket-comic, and coin-comic path. If no simulator credential is available, verify the signed-out path and record that authenticated visual evidence remains unavailable; do not connect to a physical Kobo or fabricate evidence.

- [ ] **Step 6: Review implementation against the spec**

Check every requirement and non-goal in `docs/superpowers/specs/2026-08-30-bomtoon-wallet-balances-design.md`. Confirm no SDK/protocol/runtime/CLI/catalog/dependency file changed, no mutation endpoint exists, and the future purchase/rent seam is only `refresh_asset_summary` plus its coalescing test.

- [ ] **Step 7: Commit verification-only fixes if any**

If formatter, Clippy, or simulator verification required source changes:

```bash
git add apps/bomtoon/src/api.rs apps/bomtoon/src/model.rs apps/bomtoon/src/parse.rs apps/bomtoon/src/main.rs
git commit -m "fix(bomtoon): satisfy wallet quality gates"
```

If verification required no source change, create no empty commit.
