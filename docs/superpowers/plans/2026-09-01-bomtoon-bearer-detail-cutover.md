# BOMTOON Bearer Detail Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore every authenticated episode list by replacing the obsolete `ssrPersonalized` HTML boundary with the observed bearer-authenticated content JSON contract.

**Architecture:** `api::detail` becomes the single bearer JSON request for initial title loading and post-purchase reconciliation. `parse::content_detail` consumes only the bounded `result/data` envelope, validates `result == "SUCCESS"` and exact alias identity, then reuses the existing episode, thumbnail, date, access, and string validation. No HTML compatibility parser or relaxed personalization flag remains.

**Tech Stack:** Rust 2021, `kobo_sdk::Task`, managed `Credential::bearer`, `kobo_json`, colocated Rust tests, Kobo browser simulator.

## Global Constraints

- The detail URL is exactly `https://www.bomtoon.tw/api/balcony-api-v2/contents/{alias}?isNotLoginAdult=false&isPorch=false`.
- The request uses managed bearer credential `bomtoon-access-token`, `Accept: application/json`, existing balcony headers, and a 512 KiB ceiling.
- The parser requires root `result == "SUCCESS"` and exact `data.alias == expected_alias` before trusting access data.
- Parse only allowlisted `data` business fields. Never parse, store, expose, or log account data, headers, cookies, access tokens, or credentials.
- HTML and `__NEXT_DATA__` input are rejected. No compatibility parser, public fallback, cookie request, or `ssrPersonalized` exception remains.
- Existing title metadata, episode date/thumbnail/access limits, unknown-state fail-closed behavior, commerce flow, reader flow, Gift flow, pagination, and stale-result identity checks remain unchanged.
- No new dependency, capability, origin, credential, protocol type, Store metadata, source module, or SDK change.
- No automated or unattended verification may spend Coin or consume a Gift.
- Every shell command is prefixed with `rtk`.

---

### Task 1: Cut Detail Loading Over to Bearer JSON

**Files:**
- Modify: `apps/bomtoon/src/api.rs:5-27,116-123,412-442`
- Modify: `apps/bomtoon/src/parse.rs:538-592,1622-1644,2504-2835`
- Modify: `apps/bomtoon/src/main.rs:8079-8126,11663-11688,15104-15130,15165-15187`

**Interfaces:**
- Consumes: managed credential `bomtoon-access-token`, existing `balcony_headers()`, `parse_json`, `field`, `bounded_string`, `bounded_array`, and `parse_episode`.
- Produces: unchanged `api::detail(alias: &str) -> Task` and `parse::content_detail(bytes: &[u8], expected_alias: &str) -> Result<ContentDetail, ParseError>` signatures backed by the bearer JSON contract.

- [ ] **Step 1: Replace the API regression with the bearer request contract**

Replace `detail_uses_managed_session_cookie_html_endpoint` with:

```rust
#[test]
fn detail_uses_bearer_content_json_endpoint() {
    let Task::Fetch {
        url,
        offset,
        max_bytes,
        credential,
        headers,
    } = detail("365")
    else {
        panic!("detail must be a fetch");
    };
    assert_eq!(
        url,
        "https://www.bomtoon.tw/api/balcony-api-v2/contents/365?isNotLoginAdult=false&isPorch=false"
    );
    assert_eq!(offset, 0);
    assert_eq!(max_bytes, 512 * 1024);
    assert!(matches!(
        credential,
        Some(value)
            if value.secret == "bomtoon-access-token"
                && value.header == SecretHeader::Bearer
    ));
    assert_eq!(headers, balcony_headers());
    assert!(headers.iter().all(|header| {
        !header.name.eq_ignore_ascii_case("cookie")
            && !header.name.eq_ignore_ascii_case("authorization")
            && !header.value.to_ascii_lowercase().contains("text/html")
    }));
}
```

Add `balcony_headers` to the test module's `use super::{...}` list if it is not already imported.

- [ ] **Step 2: Replace the parser fixture with the observed JSON envelope**

Replace `personalized_detail_html` with:

```rust
fn content_detail_response(result: &str, alias: &str, episodes: &str) -> Vec<u8> {
    format!(
        concat!(
            "{{\"result\":\"{result}\",\"data\":{{",
            "\"id\":41,\"alias\":\"{alias}\",\"title\":\"Hunter Q\",",
            "\"creators\":[{{\"creatorId\":1,\"name\":\"Writer\",\"type\":\"WRITER\"}},",
            "{{\"creatorId\":2,\"name\":\"Artist\",\"type\":\"ARTIST\"}}],",
            "\"synopsis\":\"A complete synopsis.\",",
            "\"episodes\":[{episodes}]",
            "}}}}"
        ),
        result = result,
        alias = alias,
        episodes = episodes,
    )
    .into_bytes()
}
```

Replace every `personalized_detail_html(true, "hunter_q", episodes)` call with `content_detail_response("SUCCESS", "hunter_q", episodes)`. Replace every fixture error name from `ssrDetail.*` to `data.*`.

- [ ] **Step 3: Add fail-closed JSON parser regressions**

Replace the old personalization tests with:

```rust
#[test]
fn bearer_content_detail_retains_metadata_episode_date_thumbnail_and_access() {
    let parsed = content_detail(
        &content_detail_response("SUCCESS", "hunter_q", OWNED_EPISODE),
        "hunter_q",
    )
    .expect("bearer content detail");
    assert_eq!(parsed.id, 41);
    assert_eq!(parsed.title, "Hunter Q");
    assert_eq!(
        parsed.creators,
        vec!["Writer".to_owned(), "Artist".to_owned()]
    );
    assert_eq!(parsed.synopsis, "A complete synopsis.");
    assert_eq!(parsed.episodes[0].opened_at, 1_709_136_000_000);
    assert_eq!(
        parsed.episodes[0].thumbnail_url.as_deref(),
        Some("https://image.balcony.studio/tw/ep_thumbnail/101/cover.webp")
    );
    assert_eq!(parsed.episodes[0].purchase, PurchaseState::Owned);
}

#[test]
fn detail_rejects_non_success_wrong_alias_and_html() {
    assert!(matches!(
        content_detail(
            &content_detail_response("FAIL", "hunter_q", OWNED_EPISODE),
            "hunter_q"
        ),
        Err(ParseError::InvalidValue("result"))
    ));
    assert!(matches!(
        content_detail(
            &content_detail_response("SUCCESS", "another", OWNED_EPISODE),
            "hunter_q"
        ),
        Err(ParseError::InvalidValue("data.alias"))
    ));
    assert!(matches!(
        content_detail(
            br#"<script id="__NEXT_DATA__">{"props":{}}</script>"#,
            "hunter_q"
        ),
        Err(ParseError::Json(_))
    ));
}

#[test]
fn detail_requires_string_result() {
    let body = content_detail_response("SUCCESS", "hunter_q", OWNED_EPISODE);
    let wrong_type = String::from_utf8(body)
        .expect("synthetic JSON")
        .replace("\"result\":\"SUCCESS\"", "\"result\":true");
    assert!(matches!(
        content_detail(wrong_type.as_bytes(), "hunter_q"),
        Err(ParseError::WrongType("result"))
    ));
}
```

- [ ] **Step 4: Migrate the main state-machine fixture and request assertions**

Replace `detail_response` with a JSON fixture:

```rust
fn detail_response(id: usize, alias: &str, title: &str, episodes: &str) -> Vec<u8> {
    format!(
        concat!(
            "{{\"result\":\"SUCCESS\",\"data\":{{",
            "\"id\":{id},\"alias\":\"{alias}\",\"title\":\"{title}\",",
            "\"creators\":[{{\"name\":\"Writer\"}}],",
            "\"synopsis\":\"Synopsis\",",
            "\"episodes\":[{episodes}]",
            "}}}}"
        ),
        id = id,
        alias = alias,
        title = title,
        episodes = episodes,
    )
    .into_bytes()
}
```

Update these integration contracts:

```rust
assert!(url.contains(
    "/api/balcony-api-v2/contents/hunter_q?isNotLoginAdult=false&isPorch=false"
));
assert!(matches!(
    credential,
    Some(value)
        if value.secret == "bomtoon-access-token"
            && value.header == SecretHeader::Bearer
));
```

Rename `expired_session_returns_to_login_instructions` to `expired_access_token_returns_to_login_instructions` and `comic_selection_uses_personalized_detail_and_back_returns_to_the_library` to `comic_selection_uses_bearer_detail_and_back_returns_to_the_library`. Update every `/detail/hunter_q` fetch assertion to the exact content API path.

- [ ] **Step 5: Run the new tests and confirm red**

Run:

```bash
rtk cargo test -p kobo-bomtoon detail_uses_bearer_content_json_endpoint
rtk cargo test -p kobo-bomtoon bearer_content_detail_retains_metadata_episode_date_thumbnail_and_access
rtk cargo test -p kobo-bomtoon detail_rejects_non_success_wrong_alias_and_html
rtk cargo test -p kobo-bomtoon comic_selection_uses_bearer_detail_and_back_returns_to_the_library
```

Expected: the API test observes the old cookie HTML request, parser tests reject the JSON fixture, and the main integration test observes the old detail URL.

- [ ] **Step 6: Implement the bearer request**

Add:

```rust
const CONTENT_URL: &str = "https://www.bomtoon.tw/api/balcony-api-v2/contents/";
const CONTENT_BYTES: u32 = 512 * 1024;
```

Replace `api::detail` with:

```rust
pub fn detail(alias: &str) -> Task {
    fetch(
        format!("{CONTENT_URL}{alias}?isNotLoginAdult=false&isPorch=false"),
        CONTENT_BYTES,
        Credential::bearer("bomtoon-access-token"),
        balcony_headers(),
    )
}
```

Keep `DETAIL_URL` and `PUBLIC_HTML_BYTES` because `public_detail`, public HTML feature parsing, and purchase referers still use them.

- [ ] **Step 7: Implement the bounded JSON parser**

Replace only `content_detail`; keep `parse_episode` unchanged:

```rust
pub fn content_detail(bytes: &[u8], expected_alias: &str) -> Result<ContentDetail, ParseError> {
    if !valid_alias(expected_alias) {
        return Err(ParseError::InvalidValue("detail alias"));
    }
    let root = parse_json(bytes)?;
    if bounded_string(&root, "result", "result", MAX_REMOTE_CODE_BYTES)? != "SUCCESS" {
        return Err(ParseError::InvalidValue("result"));
    }
    let detail = field(&root, "data", "data")?;
    let alias = bounded_string(detail, "alias", "data.alias", MAX_ALIAS_BYTES)?;
    if alias != expected_alias {
        return Err(ParseError::InvalidValue("data.alias"));
    }
    let title = bounded_string(detail, "title", "data.title", MAX_TITLE_BYTES)?;
    if title.trim().is_empty() {
        return Err(ParseError::InvalidValue("data.title"));
    }
    let creator_values = bounded_array(
        detail,
        "creators",
        "data.creators",
        MAX_DETAIL_CREATORS,
    )?;
    let mut creators = Vec::with_capacity(creator_values.len());
    for creator in creator_values {
        let name = bounded_string(creator, "name", "creator.name", MAX_CREATOR_NAME_BYTES)?;
        if name.trim().is_empty() {
            return Err(ParseError::InvalidValue("creator.name"));
        }
        creators.push(name.to_owned());
    }
    let synopsis = bounded_string(detail, "synopsis", "data.synopsis", MAX_SYNOPSIS_BYTES)?;
    if synopsis.trim().is_empty() {
        return Err(ParseError::InvalidValue("data.synopsis"));
    }
    let episodes = bounded_array(detail, "episodes", "data.episodes", MAX_EPISODES)?
        .iter()
        .map(parse_episode)
        .collect::<Result<Vec<_>, ParseError>>()?;
    Ok(ContentDetail {
        id: unsigned(detail, "id", "data.id")?,
        title: title.to_owned(),
        creators,
        synopsis: synopsis.to_owned(),
        episodes,
    })
}
```

Delete no shared HTML scanner: `public_detail` and homepage parsing still consume `__NEXT_DATA__` through their own parser entry points.

- [ ] **Step 8: Run focused regressions**

Run:

```bash
rtk cargo test -p kobo-bomtoon detail_uses_bearer_content_json_endpoint
rtk cargo test -p kobo-bomtoon bearer_content_detail_retains_metadata_episode_date_thumbnail_and_access
rtk cargo test -p kobo-bomtoon detail_rejects_non_success_wrong_alias_and_html
rtk cargo test -p kobo-bomtoon detail_requires_string_result
rtk cargo test -p kobo-bomtoon detail_bounds_creator_names_and_synopsis
rtk cargo test -p kobo-bomtoon detail_bounds_episode_count_and_requires_positive_opened_at
rtk cargo test -p kobo-bomtoon detail_thumbnail_selection_is_bounded_unambiguous_and_public
rtk cargo test -p kobo-bomtoon detail_rejects_invalid_selected_thumbnail_shapes
rtk cargo test -p kobo-bomtoon expired_access_token_returns_to_login_instructions
rtk cargo test -p kobo-bomtoon comic_selection_uses_bearer_detail_and_back_returns_to_the_library
rtk cargo test -p kobo-bomtoon quote_requote_and_marker_acknowledgement_order_the_purchase_post
```

Expected: every selected test passes; HTML is rejected; exact bearer request and all previous metadata/access safety boundaries remain covered.

- [ ] **Step 9: Run package quality gates**

Run:

```bash
rtk cargo fmt --all -- --check
rtk cargo test -p kobo-bomtoon
rtk cargo clippy -p kobo-bomtoon --all-targets --all-features -- -D warnings
rtk git diff --check
```

Expected: formatting passes, all Bomtoon tests pass, Clippy emits no warnings, and the diff has no whitespace errors.

- [ ] **Step 10: Commit the code cutover**

```bash
rtk git add apps/bomtoon/src/api.rs apps/bomtoon/src/parse.rs apps/bomtoon/src/main.rs
rtk git commit -m "fix(bomtoon): restore bearer title detail"
```

---

### Task 2: Prove the Authenticated Episode Flow

**Files:**
- Modify: `docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md:3-5`

**Interfaces:**
- Consumes: Task 1's bearer detail request and `result/data` parser.
- Produces: non-spending browser evidence that the exact reported error is gone and a completed status sentence in the approved design.

- [ ] **Step 1: Launch the browser simulator**

From `apps/bomtoon`, start the long-running process through the harness process manager:

```bash
rtk cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

Expected: `Kobo app simulator: http://127.0.0.1:8787`.

- [ ] **Step 2: Exercise the original reproduction without commerce**

Open `http://127.0.0.1:8787/` with the existing authenticated simulator state. Select at least two titles from different visible shelf positions. For each title:

- wait for the foreground detail request to settle;
- verify the Episodes screen replaces the loading overlay;
- verify no `props.pageProps.ssrPersonalized` error appears;
- verify title, creators, synopsis preview, balances, and episode rows render;
- do not tap an episode, quote action, purchase action, Coin control, or Gift control;
- return to the shelf before selecting the next title.

For one title with multiple episode pages, verify Next and Previous preserve the same ranges and the edge pager remains at the panel bottom.

- [ ] **Step 3: Check diagnostics**

Fetch `/diagnostics` without mutation.

Expected:

```json
{"issues":[]}
```

Record selected title names, row counts, screenshots, exact diagnostics, and the non-spending boundary in `.superpowers/sdd/bearer-detail-proof.md`.

- [ ] **Step 4: Mark the regression correction complete**

Replace the pending status sentence with:

```markdown
The bearer detail correction is complete. Authenticated title loading and post-purchase reconciliation use the bounded content JSON endpoint, require `result == "SUCCESS"` and exact alias identity, and retain the existing metadata and episode access limits. Focused parser/API tests, the full Bomtoon suite, formatting, Clippy, authenticated browser simulation across multiple titles, stable episode navigation, and clean layout diagnostics pass. The browser proof was non-spending; no episode, quote, purchase, Coin, or Gift action was used. The runtime simulator remains the host build, SDK IPC, daemon, device-result, and frame-render gate because it has no post-start action channel or browser credential reuse.
```

- [ ] **Step 5: Verify and commit the status update**

Run:

```bash
rtk git diff --check
rtk git add docs/superpowers/specs/2026-08-31-bomtoon-episode-list-design.md
rtk git commit -m "docs(bomtoon): record bearer detail proof"
```

Expected: no whitespace errors and one documentation commit.
