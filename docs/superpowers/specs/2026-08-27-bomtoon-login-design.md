# BOMTOON Login and Runtime Token Refresh Design

Status: Approved design
Date: 2026-08-27

## Goal

Let a Kobo Cobalt user sign in to bomtoon.tw through BOMTOON's real Chrome login page, transfer the resulting session to Cobalt, keep access tokens fresh inside the runtime, and sign out from the Bomtoon app.

The production command targets a Kobo over the existing SSH connection. Development and every acceptance check target the local simulators:

```sh
kobo bomtoon login --device <Kobo IP>
kobo bomtoon login --sim
```

The command opens a temporary BOMTOON-only Chrome or Chromium profile where the user may choose email or any social login offered by BOMTOON. After transfer, the CLI closes Chrome and deletes the profile.

## Scope

The first version supports:

- macOS hosts
- Google Chrome and Chromium
- one BOMTOON account per Kobo
- the existing SSH `--device <IP>` connection
- a local `--sim` target using the credential and state roots shared with `kobo-sim`
- runtime-owned token bootstrap and refresh
- in-app logout
- the existing library, recent-reading, and episode-status workflows

The first version does not support Safari, Firefox, Windows, Linux, multiple BOMTOON accounts, LAN discovery, QR pairing, or direct password entry on the Kobo.

## Current State and Problem

The Bomtoon app currently expects two installed secrets:

- `bomtoon-session`, sent as the `Cookie` header to the session and detail endpoints
- `bomtoon-access-token`, sent as a bearer token to library endpoints

Startup fetches `https://www.bomtoon.tw/api/auth/session` through an ordinary app task. The app only checks whether the response has a `user` object, but the response also contains access and refresh token objects. The raw response therefore enters app memory through `TaskOutcome::Completed`.

The credentialed detail-page request has the same class of problem. BOMTOON's authenticated Next.js page data can include session-derived token fields. The app must not receive authenticated HTML. Episode purchase status will come from the bearer-authenticated JSON content endpoint instead.

The design closes both app-level paths. It removes the session request and credentialed detail HTML from the app, removes `/api/auth/session` and `/detail/*` from the Bomtoon credential allowlist, and adds denial tests for both. Only the runtime broker may call the session endpoint.

A manually installed access token also expires. The current app cannot refresh or replace it because applications cannot read or write runtime secrets.

## Architecture

### CLI login driver

`kobo-cli` gains a `bomtoon login` command.

The command:

1. Finds Google Chrome or Chromium in the standard macOS application locations.
2. Creates a mode `0700` temporary profile directory.
3. Starts the browser with that profile and Chrome DevTools pipe transport. It does not expose a debugging TCP port.
4. Opens `https://www.bomtoon.tw/user/login`.
5. Waits for the user to complete BOMTOON's login flow.
6. Calls `/api/auth/session` inside the browser and accepts the session only when it contains an authenticated user plus valid access and refresh token objects.
7. Reads only the BOMTOON authentication session cookie needed by `/api/auth/session`.
8. Installs `bomtoon-session` in the selected device or simulator credential root.
9. Removes any previous runtime-managed BOMTOON token state for that target in the same operation.
10. Closes the browser process and deletes the temporary profile.

The simulator target uses the exact credential and managed-state paths supplied to `kobo-sim`. One shared path function owns these locations so the CLI and both simulators cannot drift.

The CLI never reads form fields. Passwords and social-provider credentials remain inside BOMTOON's browser flow. Device transfer sends the cookie to the remote shell over stdin; simulator transfer writes directly to the simulator credential root. Neither path creates a plaintext staging file, passes the cookie through a process argument, or prints secret values.

Cookie selection accepts only `__Secure-next-auth.session-token` or `next-auth.session-token`, including their numeric chunk suffixes. A chunked family must start at `.0` and remain contiguous. The secure family wins if both base names exist. The assembled `Cookie` value must fit the runtime's 4096-byte secret ceiling and contain no control characters. Analytics, callback, CSRF, and advertising cookies are excluded.

Before credential transfer, the CLI repeats the bounded session check with only the assembled authentication cookie. Installation proceeds only if that request identifies the same authenticated session and valid token shape seen inside Chrome.

A cleanup guard owns the Chrome process and temporary directory, and it runs after a normal exit, browser cancellation, or interruption. Timeout, SSH, and parse errors take the same cleanup path.

### Runtime credential provider

`kobo-policy` gains a managed-credential provider alongside the existing file-backed secret lookup.

For each credentialed request, the task runner:

1. Applies the existing app, credential, and URL authorization policy.
2. Asks the managed provider to resolve the credential.
3. Falls back to the existing file-backed resolver when the provider does not manage that credential.
4. Runs the request.
5. On `Unauthorized`, asks the managed provider to force one renewal and retries the original request once.

The provider returns a resolved header value only to the runtime task runner. It never returns a token through a protocol frame or `TaskOutcome`.

A managed provider also supports revocation. The protocol gains a revoke operation that identifies a managed credential and returns only completion or a typed error. Revocation serializes with resolution, detaches local credentials before network I/O, and holds only a one-shot in-memory token copy for remote revocation. Provider policy rejects credentials and apps without a registered revocation recipe.

### BOMTOON credential broker

`kobo-net::bomtoon` owns the BOMTOON HTTP recipes and strict response parsing. It does not own files. The policy/runtime layer supplies secret values and stores validated state.

The broker manages `bomtoon-access-token` for the `bomtoon` app. Its durable state contains:

- access token value and expiry
- refresh token value and expiry
- SHA-256 digest of the session cookie used to derive the pair

The state is a runtime-owned mode `0600` file under Cobalt's state directory. It is not a generic named secret, so an application cannot request the refresh token or read token metadata.

The session cookie remains the named `bomtoon-session` secret installed by the CLI.

The provider computes the session-cookie digest again on every resolve. A mismatch invalidates both the in-memory pair and the durable state before any bearer token is sent. This check covers account replacement while the runtime is already running; deleting the state file through the CLI target is not enough because a live provider may still hold the old pair in memory.

### Bomtoon app

The app removes `Pending::Session`, `api::session`, and `parse::session_is_authenticated`. Startup requests the first library page directly.

The runtime resolves or bootstraps `bomtoon-access-token` before sending the request. The app receives only library data or a typed failure, never a session or refresh response.

Episode loading uses bearer-authenticated `GET /api/balcony-api-v2/contents/<alias>?isNotLoginAdult=false&isPorch=false` in place of authenticated `GET /detail/<alias>` HTML. The parser consumes the bounded JSON `episodes` array and its purchase fields. This endpoint returns content data without session or refresh tokens.

The library screen gains a `Sign out` action. Signed-out screens show:

```text
Run this on your Mac:
kobo bomtoon login --device <Kobo IP>
```

The screen also provides `Try again` so the user can return after the CLI finishes without restarting the app.

## Network Protocol

The runtime broker may call only these BOMTOON authentication endpoints:

- `GET https://www.bomtoon.tw/api/auth/session`
- `GET https://www.bomtoon.tw/api/balcony/ip`
- `POST https://www.bomtoon.tw/api/balcony/auth/refresh`
- `PUT https://www.bomtoon.tw/api/balcony-api/auth/logout`

The app may use `bomtoon-access-token` for the existing library endpoints and this content endpoint:

- `GET https://www.bomtoon.tw/api/balcony-api-v2/contents/<alias>?isNotLoginAdult=false&isPorch=false`

The credential policy validates the alias path segment and the exact query string. App-level use of `bomtoon-session` is denied for every URL.

Session bootstrap sends `bomtoon-session` as `Cookie`. It accepts an authenticated session only when the response contains:

- a `user` object
- `accessToken.token`
- `accessToken.expiredAt`
- `refreshToken.token`
- `refreshToken.expiredAt`

Token strings must be non-empty bounded text, and expiry fields must parse as valid timestamps. Malformed or oversized responses fail without replacing stored state.

Refresh follows BOMTOON's web client:

1. Fetch the current public client address from `/api/balcony/ip`.
2. Send the current access token as a bearer credential to `/api/balcony/auth/refresh`.
3. Send the refresh token and `clientIp` in the bounded JSON body.
4. Validate both rotated tokens and their expiry fields.
5. Atomically replace the durable token state.

Logout follows BOMTOON's web client. It sends a `PUT` to `/api/balcony-api/auth/logout` with the access token as bearer authorization and the refresh token in the bounded JSON body.

Credentialed redirects are forbidden, and each response has a service-specific byte ceiling. Authentication bodies and headers are never logged.

## Login Data Flow

1. The user runs `kobo bomtoon login` with either `--device <IP>` or `--sim`.
2. The CLI opens the temporary Chrome profile at BOMTOON's login page.
3. The user completes the real BOMTOON login.
4. The CLI validates the authenticated session and selects only the authentication session cookie.
5. One target operation writes `bomtoon-session` with mode `0600` and removes the old managed token-state file.
6. The CLI reports success without printing cookie or token values.
7. The CLI closes Chrome and deletes the temporary profile.
8. The user selects `Try again` in the app.
9. The first library request asks for `bomtoon-access-token`.
10. With no token state bound to the installed cookie, the runtime bootstraps from `/api/auth/session`, stores the token pair and cookie digest, and sends the library request.

The runtime binds every derived pair to a digest of the session cookie. On each resolve it compares the current cookie with the stored and cached digest before sending a bearer token. Installing a new cookie therefore invalidates the previous account's tokens even when the broker is already live.

## Refresh Data Flow

Every resolve recomputes the session-cookie digest before checking token expiry. A mismatch clears the in-memory pair and removes the stale durable state, then starts a session bootstrap.

Before a request, the managed provider compares the access-token expiry with the current time. It refreshes when the token is expired or within five minutes of expiry.

If a request still returns `401` or `403`, the task runner forces one refresh and retries the original request once. It never retries a network error, malformed response, `404`, or `5xx` response as an authentication refresh.

If refresh is rejected, the broker performs one session bootstrap using the cookie. This covers a rotated server session or stale derived token state. If the session endpoint is also unauthenticated, the runtime deletes the cookie and managed token state and returns `Unauthorized`.

Network failures and malformed responses preserve the last valid credentials. They remain ordinary retryable errors in the app.

Refresh operations are serialized per managed account. Two requests cannot rotate the same refresh token concurrently. Token-state replacement uses write, permission, sync, and atomic rename semantics so interruption cannot leave a mixed pair.

## Logout Data Flow

1. The user selects `Sign out` in the Bomtoon app.
2. The app submits the managed-credential revoke operation.
3. Under the provider lock, the runtime copies the current token pair into a one-shot value.
4. The runtime atomically renames `bomtoon-session` out of its resolvable path, clears the in-memory pair, and removes the managed token-state file.
5. The detached cookie file is deleted before the provider lock is released. Startup cleanup removes a detached file left by a crash between rename and deletion.
6. Only after local invalidation succeeds does the runtime attempt the BOMTOON logout request with the one-shot token copy.
7. The one-shot copy is released when the request finishes, fails, or times out.
8. The app clears loaded library, recent, and episode data and returns to the signed-out screen.
9. If the remote request failed, the screen warns that remote revocation could not be confirmed. The Kobo remains locally signed out.

Local invalidation is complete before network I/O. A hang, cancellation, or process crash during the remote request cannot reactivate the account on the Kobo. Failure to detach the local cookie is a local logout failure; the runtime does not report a signed-out state until the cookie is outside the resolvable path.

## Error Behavior

| Condition | Credential state | User-visible result |
| --- | --- | --- |
| Session cookie missing | No change | Signed-out instructions |
| Session cookie expired or revoked | Delete cookie and managed state | Session-expired instructions |
| Access token near expiry | Rotate pair atomically | Continue request |
| Refresh rejected, session still valid | Replace pair from session | Continue request |
| Refresh and session rejected | Delete all BOMTOON credentials | Session-expired instructions |
| Network unavailable | Preserve credentials | Existing retry screen |
| Authentication response malformed | Preserve last valid state | Service-response error |
| Local credential detachment fails | Do not start remote logout; keep current state | Local storage error |
| Remote logout succeeds | Already invalidated locally | Signed-out screen |
| Remote logout fails | Already invalidated locally | Signed-out screen with revocation warning |
| CLI browser closes before login | No device changes | Command reports cancellation |
| SSH install fails | Existing device state remains | Command reports transfer failure |
| Simulator install fails | Existing simulator state remains | Command reports local transfer failure |

## Security Requirements

- `/api/auth/session` and authenticated `/detail/*` HTML are denied by app-level credential policy. Only the runtime broker can call the session endpoint.
- Denial tests must prove an ordinary Bomtoon task cannot receive either token-bearing response.
- The app cannot name, resolve, or revoke the runtime-owned refresh token.
- The app cannot use `bomtoon-session` for any request.
- Credential policy binds cookie and token use to a complete HTTPS request target: host and port plus method and path.
- Credentialed POST and PUT requests do not follow redirects.
- Credential values and token-bearing response bodies stay out of protocol frames and app memory.
- Secrets do not appear in logs, terminal output, process arguments, shell history, error strings, screenshots, simulator traces, or test snapshots.
- Device login transfers the cookie over stdin; simulator login writes directly to its credential root. Both paths use mode `0600`.
- The temporary Chrome profile uses mode `0700`, DevTools pipe transport, and unconditional cleanup.
- Runtime token state uses mode `0600` and atomic replacement.
- Runtime token state is bound to the digest of the session cookie that produced it. The provider rechecks that digest on every resolve.
- Logout serializes with credential resolution and invalidates local credentials before remote network I/O.

## Testing and Verification

All tests, trials, screenshots, and attended checks run against Cobalt's browser or runtime simulators. No verification step connects to a physical Kobo. Device SSH behavior is covered by command construction and fake-transport tests.

### CLI tests

- parse `kobo bomtoon login --device <IP>` and `kobo bomtoon login --sim`
- reject unsupported flags, malformed device hosts, and multiple targets
- find standard macOS Chrome and Chromium applications
- identify an authenticated BOMTOON session
- reject sessions without user, token, refresh token, or expiry fields
- select only the allowed NextAuth session-cookie family
- assemble contiguous cookie chunks and reject gaps, mixed families, controls, or an oversized value
- validate the selected cookie by itself before credential transfer
- keep secret values out of process arguments, output, and errors
- install the cookie and remove old state in one target operation
- install and invalidate state through the shared simulator roots
- remove simulator credentials and managed state during test cleanup
- close Chrome and remove the temporary profile on every exit path

### Network tests

- parse a valid session bootstrap
- reject unauthenticated, malformed, and oversized session responses
- build the exact IP, refresh, logout, and JSON content requests
- parse a valid rotated token pair
- reject partial token rotation
- preserve the old pair when refresh parsing fails
- refuse credentialed redirects
- parse episode purchase state from the bounded content JSON response

### Policy and runtime tests

- deny app-level access to `/api/auth/session`, authenticated `/detail/*`, and every use of `bomtoon-session`
- bootstrap when the managed access token is absent
- replace the session cookie while the broker is live and prove the old bearer token is never sent
- refresh within five minutes of expiry
- refresh and retry exactly once after `Unauthorized`
- do not refresh after network or non-authentication errors
- serialize concurrent refresh requests
- atomically replace token state
- never return secret-bearing response bytes to the app
- detach the session cookie and clear the live provider before starting remote logout
- prove credential resolution fails while the best-effort logout request is in flight
- recover and delete a detached cookie left by a simulated crash
- report remote logout failure after local invalidation

### App tests

Use `CLARA_BW_METRICS` for:

- signed-out instructions and `Try again`
- signed-in library with `Sign out`
- expired-session message
- local logout after successful remote revocation
- local logout with a remote-revocation warning
- clearing library, recent, and episode data on logout

### Gates and smoke test

Run focused package tests during implementation, then:

```sh
cargo fmt --all --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

The attended end-to-end simulator check must:

1. Run `kobo bomtoon login --sim` on macOS.
2. Complete BOMTOON login in the temporary Chrome profile.
3. Open the library in the browser simulator and runtime simulator.
4. Select in-app logout.
5. Confirm the app returns to signed-out state and later library access remains unauthorized.
6. Confirm the temporary Chrome profile is gone and no credential values were printed or logged.

Automated provider tests use an injected simulator clock and fake BOMTOON transport to move the access token inside the five-minute refresh window. They must prove token rotation and the single retry without changing a real account or waiting for expiry.

No test or trial may connect to a physical Kobo. No fixture, screenshot, or captured trace may contain a real credential.
