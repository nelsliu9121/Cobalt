# BOMTOON Normal-Chrome Extension Login Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Google-blocked Chrome DevTools login flow with an explicit handoff from a checked-in Chrome extension running in the user's normal Chrome profile.

**Architecture:** The CLI materializes a Manifest V3 extension, starts a nonce-protected HTTP rendezvous on IPv4 loopback, and opens BOMTOON in normal Chrome. After the user signs in and clicks the extension action, the extension fingerprints the BOMTOON session, reads only the NextAuth session-cookie family from the active Chrome cookie store, and submits a bounded payload. Rust keeps final authority over cookie reconstruction, server validation, and transactional target installation.

**Tech Stack:** Rust 2021 standard library, existing `kobo-json`, existing `kobo-net` BOMTOON validator, Manifest V3 JavaScript, Chrome `cookies` and `storage.session` APIs, dependency-free `node --test` tests.

## Global Constraints

- Host support is macOS with normal Google Chrome only.
- Keep `kobo bomtoon login --sim` and `kobo bomtoon login --device <IP>` unchanged.
- Add `kobo bomtoon extension install` for one-time unpacked-extension setup.
- Materialize the extension under `~/Library/Application Support/Cobalt/bomtoon-login-extension` with directory mode `0700`.
- Bind the rendezvous only to `127.0.0.1` on an operating-system-selected port.
- Bind the shared host lock exclusively to `127.0.0.1:53941`; never accept or read data from that socket. Use it to serialize both extension replacement and login, acquiring it before observing the extension destination and retaining it through materialization and cleanup.
- Generate a 32-byte nonce from `/dev/urandom`; encode it as 43 unpadded base64url characters.
- Put only protocol version, loopback port, and nonce in `#cobalt-login=v1.<port>.<nonce>`.
- Refuse at most 32 invalid requests and cap each connection read at two seconds within the original login deadline.
- Accept only `POST /bomtoon-login/<nonce>`, exact `Host: 127.0.0.1:<port>`, `Content-Type: application/json`, and a body no larger than 16 KiB.
- Accept version `1`, one 64-character lowercase hexadecimal fingerprint, and at most 16 cookie objects with only `name`, `value`, `domain`, `path`, and `secure`.
- Enforce UTF-8 byte limits: `name` 128, `value` 4096, `domain` 255, `path` 1024.
- Reject duplicate and unknown JSON fields.
- Reject an expired challenge and remove it from `storage.session` on the next extension activation.
- Never put cookies, token values, fingerprints, email, user IDs, or account data in arguments, URLs, logs, terminal output, errors, listener responses, extension logs, screenshots, or persistent extension storage.
- Keep extension permissions to `cookies`, `storage`, `activeTab`, exact BOMTOON hosts, and `http://127.0.0.1/*`. Request no Google permission.
- Store the challenge only in `chrome.storage.session` through the extension service worker.
- The user must click `Send BOMTOON sign-in to Kobo`; no automatic credential transfer.
- Preserve existing native cookie selection, `validate_session_cookie`, fingerprint comparison, credential lease, simulator transaction, SSH-stdin device installation, refresh, revocation, and app behavior.
- Remove the DevTools pipe, temporary profile, browser supervisor, automation flags, and all obsolete tests in the same cutover.
- Every attended verification uses `--sim`. Do not connect to or discover a physical Kobo.
- This plan changes more than three files. The execution-choice approval after this plan is also approval for the exact file map below.

## File Map

**Create:**

- `crates/kobo-cli/src/bomtoon_handoff.rs`: nonce generation, host lock, strict HTTP/JSON parsing, bounded loopback rendezvous, and fixed responses.
- `crates/kobo-cli/bomtoon-extension/manifest.json`: exact Manifest V3 permissions and entry points.
- `crates/kobo-cli/bomtoon-extension/protocol.js`: pure challenge, session, cookie, payload, and response rules shared by extension contexts.
- `crates/kobo-cli/bomtoon-extension/background.js`: trusted challenge capture and `storage.session` ownership.
- `crates/kobo-cli/bomtoon-extension/content.js`: URL-fragment capture and same-origin session fingerprinting.
- `crates/kobo-cli/bomtoon-extension/popup.html`: explicit attended handoff UI.
- `crates/kobo-cli/bomtoon-extension/popup.js`: active-tab/store cookie capture and loopback POST.
- `crates/kobo-cli/bomtoon-extension/tests/extension.test.js`: dependency-free Node tests with fake Chrome APIs.

**Modify:**

- `crates/kobo-cli/src/main.rs`: declare `bomtoon_handoff` beside `bomtoon`.
- `crates/kobo-cli/src/bomtoon.rs`: command parsing, embedded asset materialization, normal-Chrome launch, rendezvous orchestration, payload-to-cookie conversion, existing validation/install reuse, and CDP deletion.
- `docs/superpowers/specs/2026-08-28-bomtoon-extension-login-design.md`: already approved; retain as the contract.

**Unchanged by design:**

- `crates/kobo-cli/Cargo.toml`: add no Rust or JavaScript dependency.
- `crates/kobo-net/src/bomtoon.rs`: keep `validate_session_cookie` and `SESSION_COOKIE_MAX_BYTES` unchanged.
- `crates/kobo-sim/src/lib.rs`, runtime policy, and `apps/bomtoon`: keep installation, token brokerage, and UI behavior unchanged.

---

### Task 1: Build and Test the Chrome Extension

**Files:**

- Create: `crates/kobo-cli/bomtoon-extension/manifest.json`
- Create: `crates/kobo-cli/bomtoon-extension/protocol.js`
- Create: `crates/kobo-cli/bomtoon-extension/background.js`
- Create: `crates/kobo-cli/bomtoon-extension/content.js`
- Create: `crates/kobo-cli/bomtoon-extension/popup.html`
- Create: `crates/kobo-cli/bomtoon-extension/popup.js`
- Create: `crates/kobo-cli/bomtoon-extension/tests/extension.test.js`

**Interfaces:**

- Consumes: the fragment grammar and payload schema in Global Constraints.
- Produces: `globalThis.CobaltBomtoonProtocol` with `parseChallenge`, `authenticatedFingerprint`, `filterSessionCookies`, `payload`, `endpoint`, and `terminalStatus`; storage key `bomtoonChallenge`; messages `capture-challenge` and `session-fingerprint`.

- [ ] **Step 1: Write the failing protocol tests**

Create `tests/extension.test.js` with Node's built-in test runner and `vm` loader. The complete test matrix must assert:

```js
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const root = path.resolve(__dirname, "..");
const context = vm.createContext({
  TextEncoder,
  URL,
  crypto: require("node:crypto").webcrypto,
});
vm.runInContext(fs.readFileSync(path.join(root, "protocol.js"), "utf8"), context);
const protocol = context.CobaltBomtoonProtocol;
const plain = (value) => JSON.parse(JSON.stringify(value));

const nonce = "A".repeat(43);

test("challenge grammar is exact", () => {
  assert.deepEqual(
    plain(protocol.parseChallenge(`#cobalt-login=v1.43125.${nonce}`)),
    { version: 1, port: 43125, nonce },
  );
  for (const value of [
    "", "#cobalt-login=v2.43125." + nonce,
    "#cobalt-login=v1.0." + nonce,
    "#cobalt-login=v1.65536." + nonce,
    "#cobalt-login=v1.43125.short",
    "#cobalt-login=v1.43125." + "A".repeat(42) + "/",
  ]) assert.equal(protocol.parseChallenge(value), null);
});

test("authenticated session produces the access-NUL-refresh digest", async () => {
  const session = {
    user: {
      accessToken: { token: "access", createdAt: 1, expiredAt: 2 },
      refreshToken: { token: "refresh", createdAt: 1, expiredAt: 3 },
    },
  };
  assert.equal(
    await protocol.authenticatedFingerprint(session),
    "f9a78e1bd56546fabfa615264de77c95eca9fea9bf87d5b9bc72a4fc1887b237",
  );
  for (const rejected of [null, {}, { user: {} }, {
    user: {
      accessToken: { token: "", createdAt: 1, expiredAt: 2 },
      refreshToken: { token: "refresh", createdAt: 1, expiredAt: 3 },
    },
  }]) assert.equal(await protocol.authenticatedFingerprint(rejected), null);
});

test("cookie filtering preserves NextAuth candidates for native validation", () => {
  const cookies = protocol.filterSessionCookies([
    { name: "__Secure-next-auth.session-token.0", value: "a", domain: ".bomtoon.tw", path: "/", secure: true },
    { name: "__Secure-next-auth.session-token.01", value: "bad", domain: ".bomtoon.tw", path: "/", secure: true },
    { name: "next-auth.csrf-token", value: "secret", domain: ".bomtoon.tw", path: "/", secure: true },
  ]);
  assert.deepEqual(plain(cookies), [
    { name: "__Secure-next-auth.session-token.0", value: "a", domain: ".bomtoon.tw", path: "/", secure: true },
    { name: "__Secure-next-auth.session-token.01", value: "bad", domain: ".bomtoon.tw", path: "/", secure: true },
  ]);
  assert.throws(() => protocol.filterSessionCookies(
    Array(17).fill(null).map((_, index) => ({
      name: `next-auth.session-token.${index}`,
      value: "x", domain: ".bomtoon.tw", path: "/", secure: true,
    })),
  ));
});

test("payload and endpoint are bounded", () => {
  const challenge = { version: 1, port: 43125, nonce };
  assert.equal(protocol.endpoint(challenge), `http://127.0.0.1:43125/bomtoon-login/${nonce}`);
  assert.deepEqual(plain(protocol.payload("a".repeat(64), [])), {
    version: 1,
    fingerprint: "a".repeat(64),
    cookies: [],
  });
  assert.throws(() => protocol.payload("A".repeat(64), []));
  assert.throws(() => protocol.payload("a".repeat(64), Array(17).fill({})));
});

test("only native terminal responses consume the challenge", () => {
  assert.equal(protocol.terminalStatus(204), true);
  assert.equal(protocol.terminalStatus(422), true);
  assert.equal(protocol.terminalStatus(400), false);
  assert.equal(protocol.terminalStatus(500), false);
});
```


- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
rtk node --test crates/kobo-cli/bomtoon-extension/tests/extension.test.js
```

Expected: FAIL because `protocol.js` does not exist.

- [ ] **Step 3: Implement the pure protocol module**

Create `protocol.js` as a strict IIFE. Use these exact constants and exports:

```js
(() => {
  "use strict";
  const SECURE = "__Secure-next-auth.session-token";
  const INSECURE = "next-auth.session-token";
  const HEX = /^[0-9a-f]{64}$/;
  const NONCE = /^[A-Za-z0-9_-]{43}$/;

  function parseChallenge(fragment) {
    const match = /^#cobalt-login=v1\.([1-9][0-9]{0,4})\.([A-Za-z0-9_-]{43})$/.exec(fragment);
    if (!match || !NONCE.test(match[2])) return null;
    const port = Number(match[1]);
    return Number.isInteger(port) && port <= 65535
      ? { version: 1, port, nonce: match[2] }
      : null;
  }

  function validToken(value) {
    return value && typeof value === "object" && !Array.isArray(value)
      && typeof value.token === "string" && value.token.length > 0
      && !/[\u0000-\u001f\u007f]/.test(value.token)
      && Number.isSafeInteger(value.createdAt) && value.createdAt >= 0
      && Number.isSafeInteger(value.expiredAt) && value.expiredAt > value.createdAt;
  }

  async function authenticatedFingerprint(session) {
    const user = session && session.user;
    if (!user || typeof user !== "object" || Array.isArray(user)
      || !validToken(user.accessToken) || !validToken(user.refreshToken)) return null;
    const encoder = new TextEncoder();
    const access = encoder.encode(user.accessToken.token);
    const refresh = encoder.encode(user.refreshToken.token);
    const material = new Uint8Array(access.length + 1 + refresh.length);
    material.set(access);
    material[access.length] = 0;
    material.set(refresh, access.length + 1);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", material));
    return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
  }
  function member(name, family) {
    return typeof name === "string"
      && (name === family || name.startsWith(family + "."));
  }

  const bytes = (value) => new TextEncoder().encode(value).length;

  function filterSessionCookies(cookies) {
    if (!Array.isArray(cookies)) throw new Error("invalid cookie list");
    const selected = cookies.filter((cookie) =>
      cookie && typeof cookie === "object" && !Array.isArray(cookie)
      && (member(cookie.name, SECURE) || member(cookie.name, INSECURE))
    );
    if (selected.length > 16) throw new Error("too many session cookies");
    return selected.map((cookie) => {
      if (typeof cookie.value !== "string"
        || typeof cookie.domain !== "string"
        || typeof cookie.path !== "string"
        || typeof cookie.secure !== "boolean"
        || bytes(cookie.name) > 128
        || bytes(cookie.value) > 4096
        || bytes(cookie.domain) > 255
        || bytes(cookie.path) > 1024) {
        throw new Error("invalid session cookie");
      }
      const { name, value, domain, path, secure } = cookie;
      return { name, value, domain, path, secure };
    });
  }

  function payload(fingerprint, cookies) {
    if (!HEX.test(fingerprint) || cookies.length > 16) throw new Error("invalid handoff payload");
    const value = { version: 1, fingerprint, cookies };
    if (new TextEncoder().encode(JSON.stringify(value)).length > 16 * 1024) {
      throw new Error("handoff payload too large");
    }
    return value;
  }

  function endpoint(challenge) {
    return `http://127.0.0.1:${challenge.port}/bomtoon-login/${challenge.nonce}`;
  }

  function terminalStatus(status) {
    return status === 204 || status === 422;
  }

  globalThis.CobaltBomtoonProtocol = Object.freeze({
    parseChallenge, authenticatedFingerprint, filterSessionCookies,
    payload, endpoint, terminalStatus,
  });
})();
```


- [ ] **Step 4: Add the exact Manifest V3 surface**

Create `manifest.json`:

```json
{
  "manifest_version": 3,
  "name": "Cobalt BOMTOON Login",
  "version": "1.0.0",
  "description": "Transfers one attended BOMTOON sign-in to the local Cobalt CLI.",
  "permissions": ["activeTab", "cookies", "storage"],
  "host_permissions": [
    "https://www.bomtoon.tw/*",
    "https://*.bomtoon.tw/*",
    "http://127.0.0.1/*"
  ],
  "background": { "service_worker": "background.js" },
  "action": { "default_popup": "popup.html" },
  "content_scripts": [{
    "matches": ["https://www.bomtoon.tw/*"],
    "js": ["protocol.js", "content.js"],
    "run_at": "document_start"
  }]
}
```

Create `background.js` using `importScripts("protocol.js")`. Its message listener must accept `capture-challenge` only from a sender tab whose URL parses to HTTPS host `www.bomtoon.tw`, parse the supplied fragment, store `{...challenge, expiresAt: Date.now() + 600000}` under `bomtoonChallenge`, and reply `{accepted: true}` only after `chrome.storage.session.set` resolves. Every rejection replies `{accepted: false}`. Do not call `chrome.storage.session.setAccessLevel`.

- [ ] **Step 5: Add fragment capture and page-session fingerprinting**

Create `content.js`. At document start, parse `location.hash`; if valid, send `capture-challenge`, and call `history.replaceState(history.state, "", location.pathname + location.search)` only after `{accepted: true}`. Register `session-fingerprint`; for that message only, fetch `https://www.bomtoon.tw/api/auth/session` with `credentials: "same-origin"`, `cache: "no-store"`, and `Accept: application/json`, call `authenticatedFingerprint`, and reply with `{authenticated: true, fingerprint}` or `{authenticated: false}`. Return `true` from the listener while the Promise is pending. Do not return the session JSON or either token through extension messaging.

- [ ] **Step 6: Add the explicit popup handoff**

Create `popup.html` with no inline script, one heading, one status paragraph with `aria-live="polite"`, one `Send BOMTOON sign-in to Kobo` button, one `Cancel` button, and script tags for `protocol.js` then `popup.js`. Keep CSS local and minimal.

Create `popup.js` with this exact flow:

1. Read `bomtoonChallenge` from `chrome.storage.session`.
2. If absent or expired, clear it and show `Run kobo bomtoon login first.`
3. Query the active tab and require `https://www.bomtoon.tw/`.
4. Send `session-fingerprint` to that tab; reject signed-out responses without clearing the challenge.
5. Call `chrome.cookies.getAllCookieStores()` and select the store whose `tabIds` contains the active tab ID.
6. Call `chrome.cookies.getAll({ url: "https://www.bomtoon.tw/", storeId: store.id })`.
7. Run `filterSessionCookies` and `payload`.
8. POST JSON to `endpoint(challenge)` with exact `Content-Type: application/json`, `credentials: "omit"`, `cache: "no-store"`, and `redirect: "error"`.
9. Clear `bomtoonChallenge` on status 204 or 422. Keep it on status 400 or a fetch exception.
10. Show fixed, credential-free status text. Never call `console.log`.
11. Cancel clears `bomtoonChallenge` and closes the popup.

Add fake-API tests to `extension.test.js` that execute each script in `vm`, trigger registered listeners, and prove fragment removal happens only after storage acknowledgement, wrong-tab and signed-out states do not read cookies, the active tab's cookie store ID is passed to `cookies.getAll`, non-session cookies never enter the POST body, a 204 clears the challenge, and a rejected fetch keeps it.

- [ ] **Step 7: Run extension tests**

Run:

```bash
rtk node --test crates/kobo-cli/bomtoon-extension/tests/extension.test.js
```

Expected: all extension tests PASS.

- [ ] **Step 8: Commit the extension**

```bash
rtk git add crates/kobo-cli/bomtoon-extension
rtk git commit -m "feat(cli): add BOMTOON login extension"
```

---
### Task 2: Add Atomic Extension Materialization

**Files:**

- Modify: `crates/kobo-cli/src/bomtoon.rs:15-32,258-293,1417-1523`

**Interfaces:**

- Consumes: the seven checked-in extension files from Task 1.
- Produces: `Action::{Login(LoginTarget), InstallExtension}`, `parse_action(&[String]) -> Result<Action, String>`, `extension_directory(&Path) -> PathBuf`, and `materialize_extension_at(&Path) -> io::Result<PathBuf>`.

- [ ] **Step 1: Write failing parser and materializer tests**

Add tests beside the current command parser:

```rust
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
fn extension_materialization_is_exact_private_and_replaceable() {
    let root = create_private_directory_at(&std::env::temp_dir()).expect("test root");
    let installed = materialize_extension_at(&root).expect("first install");
    assert_eq!(installed, root.join("bomtoon-login-extension"));
    assert_eq!(fs::metadata(&installed).expect("metadata").permissions().mode() & 0o777, 0o700);
    let names = fs::read_dir(&installed).expect("extension files")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<BTreeSet<_>>();
    assert_eq!(names, EXTENSION_FILES.iter().map(|(name, _)| (*name).into()).collect());
    fs::write(installed.join("stale.js"), "stale").expect("stale file");
    materialize_extension_at(&root).expect("replacement");
    assert!(!installed.join("stale.js").exists());
    fs::remove_dir_all(root).expect("cleanup");
}
```

Import `BTreeSet` in the test module for the exact file and permission sets.

Add a manifest test that parses the embedded manifest through `kobo_json` and compares exact permission and host-permission sets. It must assert that no string contains `google`, `<all_urls>`, `http://*/*`, or `https://*/*`.

- [ ] **Step 2: Run the focused tests to verify failure**

```bash
rtk cargo test -p kobo-cli extension_
```

Expected: FAIL because `Action`, `parse_action`, and the materializer do not exist.

- [ ] **Step 3: Implement the command parser and embedded asset table**

Replace `parse_target` at the command boundary with:

```rust
const USAGE: &str = "usage: kobo bomtoon (login (--device IP | --sim) | extension install)";

#[derive(Clone, Debug, Eq, PartialEq)]
enum Action {
    Login(LoginTarget),
    InstallExtension,
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
```

Define `EXTENSION_FILES: &[(&str, &[u8])]` with `include_bytes!` for `manifest.json`, `protocol.js`, `background.js`, `content.js`, `popup.html`, and `popup.js`. Do not materialize `tests/`.

- [ ] **Step 4: Implement atomic materialization**

`extension_directory(home)` returns `home/Library/Application Support/Cobalt/bomtoon-login-extension`. The production installation path must:

1. Acquire the existing `HostLock` before checking whether the destination exists and retain it through the full materialization result, including backup cleanup or rollback and replacement bookkeeping.
2. Ensure `cobalt_root` exists with mode `0700`.
3. Create a same-parent private staging directory using `private_name`.
4. Write every embedded file with `OpenOptionsExt::mode(0o600)`, `write_all`, and `sync_all`.
5. Sync the staging directory.
6. Rename an existing destination to a same-parent backup.
7. Rename staging to the destination and sync the parent.
8. Restore the backup if the destination rename fails.
9. Remove the backup only after the destination exists and is synced.
10. Clean staging and backup paths on every error without printing their contents.

Wire `command` so `InstallExtension` requires macOS, resolves `HOME`, calls the materializer, then prints only the installed path and the five approved `chrome://extensions` setup steps. It must not open `chrome://extensions` automatically.
If the destination already existed, also print `Reload Cobalt BOMTOON Login on chrome://extensions.` without opening Chrome.

- [ ] **Step 5: Run focused materializer tests**

```bash
rtk cargo test -p kobo-cli extension_
```

Expected: parser, manifest, first-install, replacement, exact-file-set, and permission tests PASS.

- [ ] **Step 6: Commit the materializer**

```bash
rtk git add crates/kobo-cli/src/bomtoon.rs
rtk git commit -m "feat(cli): install BOMTOON extension"
```

---

### Task 3: Implement the Strict Loopback Rendezvous

**Files:**

- Create: `crates/kobo-cli/src/bomtoon_handoff.rs`
- Modify: `crates/kobo-cli/src/main.rs:13-16`

**Interfaces:**

- Consumes: `kobo_json::parse` and the exact HTTP/JSON limits from Global Constraints.
- Produces: `Challenge::new() -> io::Result<(Challenge, TcpListener)>`, `Challenge::fragment() -> String`, `HostLock::acquire() -> io::Result<HostLock>`, `wait_for_payload(&TcpListener, &Challenge, Instant) -> Result<PendingHandoff, HandoffError>`, `PendingHandoff::payload() -> &HandoffPayload`, and `PendingHandoff::{succeed, fail}`.

Use these types:

```rust
pub const MAX_BODY_BYTES: usize = 16 * 1024;
pub const MAX_COOKIES: usize = 16;
pub const MAX_REJECTED_REQUESTS: usize = 32;
pub const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(2);

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
```

- [ ] **Step 1: Write failing pure protocol tests**

Add unit tests for:

```rust
fn request_bytes(method: &str, host: &str, path: &str, content_type: &str, body: &str) -> Vec<u8> {
    format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn request_with_body(body: &str) -> Vec<u8> {
    request_bytes(
        "POST", "127.0.0.1:43125", "/bomtoon-login/nonce", "application/json", body,
    )
}

fn replace_once(input: &[u8], from: &str, to: &str) -> Vec<u8> {
    String::from_utf8(input.to_vec())
        .expect("ASCII request")
        .replacen(from, to, 1)
        .into_bytes()
}

#[test]
fn nonce_is_unpadded_base64url_with_full_entropy_length() {
    let source: Vec<u8> = (0_u8..32).collect();
    assert_eq!(nonce_from(&mut Cursor::new(source)).expect("nonce").len(), 43);
    assert!(nonce_from(&mut Cursor::new(vec![0xff; 32]))
        .expect("nonce")
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
}

#[test]
fn request_requires_exact_http_metadata_and_json_shape() {
    let valid = request_bytes(
        "POST", "127.0.0.1:43125", "/bomtoon-login/nonce", "application/json",
        r#"{"version":1,"fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","cookies":[]}"#,
    );
    assert!(parse_request(&valid, 43125, "nonce").is_ok());
    for rejected in [
        replace_once(&valid, "POST", "GET"),
        replace_once(&valid, "127.0.0.1:43125", "localhost:43125"),
        replace_once(&valid, "/bomtoon-login/nonce", "/bomtoon-login/wrong"),
        replace_once(&valid, "application/json", "text/plain"),
        request_with_body(r#"{"version":1,"version":1,"fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","cookies":[]}"#),
        request_with_body(r#"{"version":1,"fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","cookies":[],"extra":true}"#),
    ] {
        assert!(parse_request(&rejected, 43125, "nonce").is_err());
    }
}
```


Add boundary tests for the 16 KiB body ceiling, 17 cookies, every string limit, lowercase fingerprint, unknown cookie fields, duplicate cookie fields, invalid UTF-8, missing or duplicate `Content-Length`, transfer encoding, extra bytes after the declared body, the 8 KiB header ceiling, the two-second per-connection deadline, and closure after 32 refused requests.

- [ ] **Step 2: Write failing host-lock and live-listener tests**

Use a temporary root and real loopback sockets. Prove:

- the second `HostLock::acquire` fails while the first guard lives;
- 256 repeated acquisition attempts cannot bypass the first guard;
- dropping the first guard permits reacquisition;
- an unrelated listener already bound to `127.0.0.1:53941` makes acquisition fail closed;
- invalid HTTP receives status 400 and a later valid request still succeeds before the original deadline;
- `PendingHandoff::succeed` writes status 204 with no body;
- `PendingHandoff::fail` writes status 422 with a fixed body containing no payload values;
- a second terminal request cannot be accepted after the listener is dropped;

- a short deadline returns `HandoffError::Timeout`.

- [ ] **Step 3: Run the handoff tests to verify failure**

```bash
rtk cargo test -p kobo-cli bomtoon_handoff
```

Expected: FAIL because the module and interfaces do not exist.

- [ ] **Step 4: Implement nonce and strict JSON parsing**

Implement `nonce_from(&mut impl Read)` by `read_exact` of 32 bytes and a local unpadded RFC 4648 base64url encoder. `Challenge::new` opens `/dev/urandom`, generates the nonce, binds `TcpListener::bind((Ipv4Addr::LOCALHOST, 0))`, records `local_addr().port()`, and returns both values.

Add `mod bomtoon_handoff;` beside `mod bomtoon;` in `crates/kobo-cli/src/main.rs`.

Implement strict object extraction against `Value::Object(Vec<(String, Value)>)`: compare every field against the allowed set, reject a repeated name before reading any value, require every field exactly once, and perform the limits before cloning strings. Require `secure` to be JSON boolean and `version` to be integer `1`.

- [ ] **Step 5: Implement strict HTTP parsing and the accept loop**

Use only `TcpListener`, `TcpStream`, `Read`, and `Write` from the standard library. Set the listener nonblocking. Until the original deadline:

1. Accept only IPv4-loopback peers.
2. Apply a per-connection read timeout no longer than the remaining deadline.
3. Read at most 8 KiB through `\r\n\r\n`.
4. Require request line `POST /bomtoon-login/<nonce> HTTP/1.1`.
5. Parse headers case-insensitively by name, but require one exact Host value and one media type value `application/json` with no parameter.
6. Require one canonical decimal `Content-Length` no larger than 16 KiB.
7. Reject `Transfer-Encoding`, duplicate headers named above, premature EOF, trailing request bytes, and malformed UTF-8.
8. Write fixed 400 on rejection and continue without changing the deadline.
9. Stop with `HandoffError::Listener` after 32 refused requests. For each accepted stream, use the smaller of two seconds and the remaining overall deadline as its read timeout.
10. Return the first schema-valid `PendingHandoff`.

`PendingHandoff::succeed` writes `HTTP/1.1 204 No Content`, `Connection: close`, and `Content-Length: 0`. `fail` writes status 422 and one fixed ASCII body. Both consume `self`, call `shutdown(Shutdown::Both)`, and expose no payload in the response.

- [ ] **Step 6: Implement the kernel-backed host lock**

Define `HOST_LOCK_PORT: u16 = 53941`. `HostLock::acquire` binds `TcpListener::bind((Ipv4Addr::LOCALHOST, HOST_LOCK_PORT))` and retains the listener without accepting or reading connections. Kernel-exclusive binding supplies atomic acquisition and crash cleanup; dropping `HostLock` closes the socket. Any `AddrInUse`, including an unrelated local process, fails closed as an active login. Add an internal `acquire_at(port)` helper so tests can select an unused loopback port without racing unrelated services; the production wrapper always passes `HOST_LOCK_PORT`.

- [ ] **Step 7: Run handoff tests**

```bash
rtk cargo test -p kobo-cli bomtoon_handoff
```

Expected: all protocol, boundary, lock, response, replay, and timeout tests PASS.

- [ ] **Step 8: Commit the rendezvous**

```bash
rtk git add crates/kobo-cli/src/main.rs crates/kobo-cli/src/bomtoon_handoff.rs
rtk git commit -m "feat(cli): add BOMTOON login handoff"
```

---
### Task 4: Replace CDP Login With the Extension Handoff

**Files:**

- Modify: `crates/kobo-cli/src/bomtoon.rs:1-1004,1417-2193`

**Interfaces:**

- Consumes: `Challenge`, `HostLock`, `PendingHandoff`, and `HandoffPayload` from Task 3; existing `select_session_cookie`, `validate_and_install`, and `install_target`.
- Produces: `open_normal_chrome_with(&Challenge, impl FnOnce(&mut Command) -> io::Result<ExitStatus>) -> Result<(), String>`, its production `open_normal_chrome(&Challenge) -> Result<(), String>` wrapper, `browser_cookies(&HandoffPayload) -> Vec<BrowserCookie>`, and the final `login(&LoginTarget) -> Result<(), String>` flow.

- [ ] **Step 1: Update cookie and orchestration tests first**

Add `secure: bool` to `BrowserCookie` and the test `cookie` helper. Add these focused tests:

```rust
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
    assert_eq!(browser_cookies(&payload), vec![cookie(SECURE_COOKIE, "secret")]);
}
```

Extract `login_with` so tests can inject challenge creation, browser opening, payload receipt, validation, and installation. Prove in separate tests that:

- the browser command is exactly `open -a "Google Chrome" "https://www.bomtoon.tw/user/login#cobalt-login=v1.<port>.<nonce>"`;
- browser arguments contain no cookie, fingerprint, token, email, user ID, CDP flag, remote-debugging flag, or user-data directory;
- cookie selection runs before server validation;
- server validation runs before target installation;
- a matching fingerprint installs once and attempts 204 exactly once;
- a failed 204 write after validation and installation still returns success because the target is already committed;
- malformed cookies return `cookie selection failed`, install nothing, and send 422;
- validator failure or fingerprint mismatch returns `session validation failed`, install nothing, and sends 422;
- target failure returns `target installation failed` and sends 422;
- timeout returns setup guidance naming `kobo bomtoon extension install`;
- every result drops the host lock and listener.

- [ ] **Step 2: Run replacement tests to verify failure**

```bash
rtk cargo test -p kobo-cli
```

Expected: FAIL because the handoff orchestration does not exist.

- [ ] **Step 3: Implement normal-Chrome launch**

Build the URL only after the listener has selected its port:

```rust
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
```

The production wrapper calls `Command::status`. It must not discover the Chrome binary, spawn Chrome directly, control Chrome, close Chrome, or touch its profile.

- [ ] **Step 4: Implement the final login order**

`login` must:

1. Acquire `HostLock`.
2. Create `Challenge` and loopback listener.
3. Open the normal Chrome URL.
4. Wait until `Instant::now() + LOGIN_TIMEOUT` for one schema-valid payload.
5. Convert `HandoffCookie` values to `BrowserCookie` without copying unrelated fields.
6. Run `select_session_cookie`; on failure, consume the pending handoff with 422 and return `COOKIE_SELECTION_FAILED`.
7. Call `kobo_net::bomtoon::validate_session_cookie` through `validate_and_install`.
8. Install through the unchanged `install_target` only on equal valid fingerprints.
9. After native validation and target installation commit, attempt 204 exactly once. Treat an I/O failure delivering that success response as best-effort: do not roll back the target or return `browser launch failed`.
10. Send 422 on native validation/install failure and preserve the existing fixed CLI errors. The timeout string adds `; run kobo bomtoon extension install if the extension is not loaded`.

Keep `validate_and_install`, `install_device`, `install_simulator`, and all transaction code unchanged except signatures forced by the new payload source.

- [ ] **Step 5: Delete the obsolete automation implementation and tests**

Delete:

- `COOKIE_URL`, `LOGIN_POLL_INTERVAL`, `MAX_CDP_FRAME_BYTES`, and `CHROME_LAUNCH_PROGRAM`;
- `BROWSER_SESSION_EXPRESSION`;
- Chrome candidate discovery and direct browser spawning;
- rename `create_private_profile_at` to `create_private_directory_at` with LSP and migrate every test and materializer caller; keep its generic private-directory behavior;
- `BrowserProcess`, `ProfileCleaner`, `ChromeGuard`;
- `CdpError`, `DevToolsPipe`, `BrowserFlowError`;
- `run_browser_login`, `browser_login`, `browser_login_with`, `browser_cdp_error`, `retry_pause`, `bomtoon_target`, `bomtoon_page_url`, `page_authentication`, and `parse_network_cookies`;
- every CDP framing, partial-reader, target-event, timeout supervisor, profile cleanup, Chrome discovery, and automation argument test.

Remove imports made unused by this deletion. Retain cookie-family, validation, target-install, and transaction tests.

- [ ] **Step 6: Run focused CLI tests**

```bash
rtk cargo test -p kobo-cli bomtoon
```

Expected: all BOMTOON CLI tests PASS, with no CDP or temporary-profile test remaining.

- [ ] **Step 7: Run Clippy for the changed package**

```bash
rtk cargo clippy -p kobo-cli --all-targets --all-features -- -D warnings
```

Expected: PASS with no warnings.

- [ ] **Step 8: Commit the clean cutover**

```bash
rtk git add crates/kobo-cli/src/bomtoon.rs
rtk git commit -m "fix(cli): use normal Chrome for BOMTOON login"
```

---

### Task 5: Verify Setup, Handoff, and Simulator Behavior

**Files:**

- Verify only; no production file changes unless a failing acceptance check exposes a defect in Tasks 1-4.

**Interfaces:**

- Consumes: final `kobo bomtoon extension install` and `kobo bomtoon login --sim` commands.
- Produces: command, test, and attended simulator evidence for the approved design.

- [ ] **Step 1: Run formatting and diff hygiene**

```bash
rtk cargo fmt --all --check
rtk git diff --check
```

Expected: both commands exit successfully with no output from `git diff --check`.

- [ ] **Step 2: Run extension and focused Rust tests**

```bash
rtk node --test crates/kobo-cli/bomtoon-extension/tests/extension.test.js
rtk cargo test -p kobo-cli
```

Expected: all tests PASS.

- [ ] **Step 3: Run workspace gates**

```bash
rtk cargo test --workspace --all-targets --all-features
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: both workspace gates PASS.

- [ ] **Step 4: Exercise extension materialization**

Run:

```bash
rtk cargo run -p kobo-cli -- bomtoon extension install
```

Expected: the command prints the extension directory and the five one-time Chrome setup steps. Inspect only file names and permission modes. Confirm the directory is mode `0700`, contains exactly the six runtime files, and contains no `tests` directory.

- [ ] **Step 5: Perform the attended normal-Chrome login**

1. Open `chrome://extensions` in the user's normal Google Chrome.
2. Enable Developer mode, choose Load unpacked, and select the printed directory.
3. Run `rtk cargo run -p kobo-cli -- bomtoon login --sim`.
4. Confirm Chrome opens without an automation banner or temporary profile.
5. Complete Google login.
6. Click the extension action, then `Send BOMTOON sign-in to Kobo`.
7. Confirm the CLI reports success without displaying cookie, token, fingerprint, email, user ID, profile data, or account data.
8. Do not take a screenshot while the login or extension popup is visible.

Expected: Google login completes in the normal profile and the explicit extension click completes the CLI command.

- [ ] **Step 6: Exercise BOMTOON in both simulators**

Start the browser simulator from `apps/bomtoon` with `rtk cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev`, open `http://127.0.0.1:8787` with the browser verification tool, and exercise the visible app. Stop that server, then start the runtime simulator from the repository root with `rtk cargo run -p kobo-cli -- run --sim --app bomtoon`. Verify:

1. Library titles load.
2. Recent reading loads.
3. Episode purchase status loads.
4. Sign out returns to the signed-out instructions and clears account data.
5. `Try again` remains signed out until `kobo bomtoon login --sim` runs again.
6. No physical Kobo, SSH command, device discovery, or automation-controlled browser is used.

Expected: runtime behavior matches the existing BOMTOON acceptance contract; only the login handoff changed.

