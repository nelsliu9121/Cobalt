# BOMTOON Normal-Chrome Extension Login Design

Status: Draft for review
Date: 2026-08-28
Supersedes: The automated Chrome/CDP login driver in `2026-08-27-bomtoon-login-design.md`

## Goal

Let a user who authenticates to BOMTOON through Google complete `kobo bomtoon login` in their normal Chrome profile. Google refuses the current DevTools-controlled temporary profile. The replacement must keep the session cookie out of the clipboard, command arguments, logs, and screenshots.

The runtime token broker, target installation, refresh, revocation, app behavior, and device safety rules from the previous design remain unchanged.

## Scope

The first release supports:

- macOS and normal Google Chrome
- a checked-in Manifest V3 extension loaded once as an unpacked extension
- Google and any other login method that works in the user's normal Chrome profile
- the existing `--sim` and `--device <IP>` targets
- one attended handoff at a time

It does not publish to the Chrome Web Store. It does not support Safari, Firefox, Windows, Linux, or automation-evasion flags.

## Decision

Replace the temporary-profile DevTools driver with an explicit extension-to-loopback handoff.

Do not hide Chrome automation or weaken Google's browser checks. Manual cookie copy remains rejected because it puts a live credential in the clipboard and asks the user to handle an HttpOnly value.

## Components

### Extension materializer

The extension source lives beside `kobo-cli` and is embedded in the CLI binary. `kobo bomtoon extension install` materializes the exact files under:

```text
~/Library/Application Support/Cobalt/bomtoon-login-extension
```

The directory uses mode `0700`. Setup prints the path and these one-time Chrome steps:

1. Open `chrome://extensions`.
2. Enable Developer mode.
3. Choose Load unpacked.
4. Select the printed directory.
5. Pin the Cobalt BOMTOON Login extension if desired.

A later CLI release may replace the materialized files atomically. Chrome may require the user to select Reload on the extensions page after an update.

### CLI rendezvous

`kobo bomtoon login --sim` and `kobo bomtoon login --device <IP>`:

1. Acquire a host-local lock so only one BOMTOON login handoff can run.
2. Bind an HTTP listener to `127.0.0.1` on an operating-system-selected port.
3. Generate a 32-byte nonce with the operating system CSPRNG.
4. Open the BOMTOON login page in normal Chrome using macOS `open`.
5. Put only the protocol version, loopback port, and base64url nonce in the URL fragment.
6. Wait for one valid extension POST or the existing bounded login deadline.
7. Reconstruct and validate the cookie using the existing native checks.
8. Install the cookie with the existing transactional simulator or device path.
9. Close the listener and release the host-local lock.

The fragment format is:

```text
#cobalt-login=v1.<port>.<nonce>
```

URL fragments are not sent to BOMTOON. The nonce is a one-time capability, not a credential. The content script removes it from the address bar with `history.replaceState` immediately after capture.

The listener accepts only:

- a TCP connection to `127.0.0.1`
- `POST /bomtoon-login/<nonce>`
- `Host: 127.0.0.1:<selected-port>`
- `Content-Type: application/json`
- a body no larger than 16 KiB
- this exact schema: version `1`, a 64-character lowercase hexadecimal fingerprint, and at most 16 cookie objects containing only string `name`, `value`, `domain`, `path`, and boolean `secure`
- per-field UTF-8 byte limits: `name` 128, `value` 4096, `domain` 255, and `path` 1024

It returns a fixed success or failure body with no cookie, fingerprint, account data, or target details. Invalid HTTP or schema requests do not extend the deadline and leave the listener available. The first schema-valid payload is terminal: native validation either installs it or returns a fixed error, then the listener closes.

### Chrome extension

The Manifest V3 extension permissions are:

```text
cookies
storage
activeTab
```

Its host permissions are:

```text
https://www.bomtoon.tw/*
https://*.bomtoon.tw/*
http://127.0.0.1/*
```

Chrome requires both `cookies` and host permissions for cookie access. The loopback host permission covers the random listener port. No Google host permission is requested.

A BOMTOON content script recognizes the exact fragment grammar, stores the challenge in `chrome.storage.session`, and removes the fragment. Chrome documents `storage.session` as in-memory state that is cleared when the extension reloads, updates, is disabled, or the browser restarts. The Google redirect may leave BOMTOON, but the challenge survives the redirect because it belongs to the extension session rather than the page.

After login, the user clicks the extension action and chooses `Send BOMTOON sign-in to Kobo`. The extension:

1. Requires an active `https://www.bomtoon.tw/` tab.
2. Fetches `/api/auth/session` in the page context with same-origin credentials.
3. Verifies an authenticated user and valid access and refresh token objects.
4. Computes SHA-256 over access token, a NUL byte, and refresh token.
5. Sends only the hexadecimal fingerprint out of the page context.
6. Reads cookies from the active tab's cookie store.
7. Keeps only the secure or insecure NextAuth session base names and numeric chunks.
8. POSTs bounded cookie metadata and the fingerprint to the stored loopback challenge.
9. Clears the challenge after success, explicit cancel, or expiry.

The extension never sends token strings, email, user ID, profile fields, analytics cookies, CSRF cookies, or callback cookies. Token strings exist only in the BOMTOON page response long enough to compute the fingerprint.

### Native validation and installation

The CLI remains the authority for cookie selection. It applies the existing rules:

- secure family wins without downgrade
- base and chunks cannot coexist in one family
- chunks start at zero and are contiguous
- domain and path must match BOMTOON
- controls and oversized values are rejected
- only the selected cookie is sent to `validate_session_cookie`

`validate_session_cookie` returns the native token-pair fingerprint. Installation proceeds only when it equals the page fingerprint supplied by the extension. The existing credential lease, transaction recovery, simulator paths, SSH stdin transfer, and managed-state invalidation remain unchanged.

## Error Behavior

| Condition | Result |
| --- | --- |
| Extension not installed | CLI times out with setup guidance; target state is unchanged |
| Google login incomplete | Extension reports that BOMTOON is not signed in; challenge remains retryable |
| Wrong active tab | Extension asks the user to select the BOMTOON tab |
| Missing or malformed challenge | Extension refuses the handoff |
| Invalid method, host, nonce, schema, or size | Listener returns a fixed refusal and keeps the original deadline |
| Cookie family malformed | CLI returns `cookie selection failed`; target state is unchanged |
| Page and native fingerprints differ | CLI returns `session validation failed`; target state is unchanged |
| Loopback POST fails | Extension keeps the challenge so the user can retry before timeout |
| Target install fails | Existing transaction restores the previous target state |
| Login succeeds | Listener closes, extension clears the challenge, and CLI reports success without account data |

The CLI does not close normal Chrome or remove its profile. It owns only the listener, nonce, host lock, and target transaction.

## Security Requirements

- Bind only IPv4 loopback. Do not listen on LAN, wildcard, IPv6 wildcard, or a Unix socket exposed outside the user account.
- Generate the nonce from OS entropy and compare it without logging it.
- Limit the listener body to 16 KiB and the cookie array to 16 exact-shape entries.
- Reject unknown fields, duplicate fields, and values outside their field-specific bounds.
- Never place the cookie, token values, or fingerprint in process arguments, URLs, terminal output, error strings, extension logs, screenshots, or persistent extension storage.
- Keep extension host permissions limited to BOMTOON and IPv4 loopback.
- Use `chrome.storage.session`; do not use `storage.local` or `storage.sync` for the challenge.
- Clear the challenge after success, expiry, extension reload, or explicit cancel. A transport or validation failure remains retryable until the original deadline.
- Preserve strict native cookie selection and server validation. JavaScript does not decide which cookie is installed.
- Preserve the existing target credential lease and crash-recoverable transaction.
- Remove the current CDP driver, temporary Chrome profile, DevTools framing, and automation supervisor after every caller and test migrates.

## Testing

Rust tests cover:

- setup command parsing and atomic extension materialization
- exact manifest permissions and absence of Google or broad web permissions
- loopback-only binding and random-port reporting
- nonce format, wrong nonce, replay, duplicate request, and timeout
- Host, method, content type, content length, JSON shape, duplicate field, and body ceiling rejection
- payload cookie filtering boundaries and active cookie-store selection
- strict native cookie reconstruction and fingerprint comparison
- no credential in browser-open arguments, listener responses, errors, or fake target commands
- unchanged transactional simulator and device installation
- deletion of every CDP/temp-profile path and obsolete test

Extension tests are dependency-free `node --test` modules with a fake `chrome` API. They cover:

- fragment capture and immediate removal
- `storage.session` lifecycle
- wrong-tab and signed-out messages
- authenticated response validation and fingerprint generation
- exact cookie-name filtering
- active cookie-store selection
- bounded POST payload
- challenge clearing and retry behavior

The project verification gate runs these JavaScript tests explicitly alongside the focused Rust package tests.

The attended acceptance test uses normal Chrome:

1. Load the unpacked extension once.
2. Run `kobo bomtoon login --sim`.
3. Complete Google login in normal Chrome.
4. Click `Send BOMTOON sign-in to Kobo`.
5. Confirm CLI success without secret or account output.
6. Run the BOMTOON simulator and verify library, recent reading, episode status, Sign out, and Try again.
7. Confirm no physical Kobo, SSH, device discovery, or automation-controlled browser was used.

## Documentation Sources

- Chrome Cookies API permissions: https://developer.chrome.com/docs/extensions/reference/api/cookies
- Chrome extension storage areas: https://developer.chrome.com/docs/extensions/reference/api/storage
- Chrome extension security and minimum host permissions: https://developer.chrome.com/docs/extensions/develop/security-privacy/stay-secure
