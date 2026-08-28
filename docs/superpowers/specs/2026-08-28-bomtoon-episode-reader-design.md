# BOMTOON plain episode reader

## Status

Approved design. Implementation has not started.

## Goal

A signed-in reader can open an owned or free BOMTOON episode and read its plain WebP images on a Kobo. The app fetches a bearer-authenticated image manifest, validates its short-lived CDN references, fetches one image at a time without an account credential, decodes it, and presents screen-height slices through the standard Kobo page-turn surface.

This milestone supports only manifests whose `line` and `point` fields are null. Scrambled episodes fail explicitly until their POST and decryption contract has a live fixture.

## Current state

The BOMTOON app can load the account library, recent reading, content metadata, and episode purchase status. Its episode screen is text-only. Episode rows do not open a reader.

The managed `bomtoon-access-token` credential is available to the app only through runtime-owned resolution. `kobo-net::credential_allowed` currently permits bearer GET requests for the exact library, recent, and content metadata routes. It denies the Next.js viewer, session endpoint, other methods, other credentials, and other routes.

`kobo-image` decodes JPEG, PNG, and static WebP. It rejects compressed sources above 4 MiB and decoded sources above 6,209,024 pixels. Its `Picture::fit` operation contains a picture inside both a width and a height, which is unsuitable for a tall comic strip that must stay readable at panel width.

Live API inspection established these contracts:

- Authenticated-free and owned plain episodes use `GET /api/balcony-api-v2/contents/images/<content-alias>/<episode-alias>?imageWidth=1080` with the managed bearer token.
- A 35-image manifest was 29,980 bytes. An 81-image manifest was 69,336 bytes.
- Image entries were one-based, unique, and contiguous.
- Image URLs used HTTPS on `image.balcony.studio`, paths below `/tw/ep/`, a `.webp` suffix, and exactly the `Policy`, `Signature`, and `Key-Pair-Id` query fields.
- Signed URLs expired after 60 seconds and fetched successfully without an account credential or redirect.
- The observed images were 1280 pixels wide and 4578 through 5120 pixels high. The API returned the same files for `imageWidth=720`, `imageWidth=1080`, no width parameter, `WEB`, and `MOBILE_IOS`.
- 34 of 35 images in the authenticated-free episode exceeded the current `kobo-image` pixel ceiling. The observed maximum was 6,553,600 pixels.
- The Next.js viewer JSON contained the images but also embedded access tokens, refresh tokens, and account data. It is not an application API and remains denied.
- One owned episode and one owned episode per title across the first 30 library titles had null `line` and `point` fields. No live scrambled manifest was found.

## Scope

The first reader includes:

- owned and free episode actions
- the exact bearer-authenticated plain image-manifest GET
- strict manifest and signed-URL validation
- uncredentialed signed WebP fetches
- a bounded shared image ceiling with small source-size headroom
- width scaling and fixed-height grayscale slicing
- standard page turns, page position, and Back behavior
- one manifest refresh when a signed image URL is rejected
- reader-specific loading, unsupported-content, decode, and network errors
- parser, policy, image, state, and Clara BW layout tests
- browser and runtime simulator evidence using an authenticated-free episode

## Non-goals

- scrambled episode support
- the scramble-key POST, fallback key, AES-CBC decryption, or 4 by 4 tile reordering
- purchasing, renting, gifting, or unlocking episodes
- sample episode reading until its `isSample=true` manifest path is verified live
- continuous vertical scrolling
- chapter-to-chapter navigation
- reading-position persistence or resume
- disk caching or offline reading
- image prefetch
- cover art in the library or episode list
- Next.js viewer HTML or `/_next/data` access
- exposing account or signed-CDN credentials to logs, persistent state, or documentation

## Readable episode eligibility

`PurchaseState::Owned` and `PurchaseState::Free` episode rows are actions. `PurchaseState::Sample`, `PurchaseState::NotOwned`, and `PurchaseState::Other` remain non-actionable status rows.

Opening an eligible row records the selected content alias, episode alias, and display title, then starts the manifest request. It does not infer access from a title count or recent-reading record. The server remains authoritative: a manifest denial is handled as an account or content failure even when local metadata said the episode was readable.

## Manifest request

The app issues:

```text
GET https://www.bomtoon.tw/api/balcony-api-v2/contents/images/<content-alias>/<episode-alias>?imageWidth=1080
```

The request uses `Credential::bearer("bomtoon-access-token")`, a 512 KiB response ceiling, and these non-secret headers:

```text
Accept: application/json
Accept-Language: zh-TW,zh;q=0.9,en-US;q=0.8,en;q=0.7
x-balcony-id: BOMTOON_TW
x-balcony-timezone: Asia/Taipei
x-platform: MOBILE_IOS
x-referer: https://www.bomtoon.tw/viewer/<content-alias>/<episode-alias>
```

Content and episode aliases use the existing BOMTOON alias alphabet: non-empty ASCII alphanumeric characters, underscore, or hyphen. The query key, order, and value are fixed. No other manifest route or query form is admitted.

The 512 KiB ceiling applies before parsing because `Task::Fetch` buffers the body. The accepted manifest has at most 256 entries and each signed URL has at most 1024 bytes. URLs alone can therefore consume 256 KiB; the remaining capacity covers dimensions, ordering, expiry, JSON syntax, and bounded ignored fields. This is about 7.5 times the largest observed response while keeping a malformed server response from consuming unbounded device memory.

`kobo-net::credential_allowed` gains only this exact bearer GET. It continues to deny POST, altered or extra query fields, empty or malformed aliases, wrong origins, session credentials, content HTML, viewer HTML, and Next.js data routes. Credentialed redirects remain denied by the existing network layer.

## Manifest contract

The root must report `result: "SUCCESS"` and a `data` array containing 1 through 256 entries. Each accepted entry provides:

- `orderNo`: an integer in the contiguous one-based sequence `1..=count`
- `width`: a positive integer
- `height`: a positive integer
- `imagePath`: a signed URL no longer than 1024 bytes
- `line`: null
- `point`: null

The parser rejects missing fields, wrong types, zero dimensions, duplicate or non-contiguous ordering, more than 256 entries, or any image whose `width * height` exceeds 7,000,000 pixels.

A signed URL is accepted only when all of these are true:

- scheme is `https`
- no username, password, fragment, or non-default port exists
- host is exactly `image.balcony.studio`
- path begins `/tw/ep/` and ends `.webp`
- query fields are exactly one non-empty `Policy`, one non-empty `Signature`, and one non-empty `Key-Pair-Id`
- no duplicate or unknown query field exists

A non-null `line` or `point` rejects the complete manifest as unsupported scrambled content. The app neither fetches a subset nor attempts to render the signed source without reordering.

The parsed model retains order, dimensions, and the signed URL. It does not retain unrelated response fields, account data, or the Next.js page shape.

## Signed image requests

The app fetches only the current manifest entry. The request:

- uses the validated `imagePath` verbatim
- carries no `Credential`
- accepts WebP image data
- starts at offset zero
- has the existing `kobo-image::MAX_SOURCE_BYTES` 4 MiB ceiling

The URL is a short-lived content capability. It may exist transiently in app and task memory because the app must name the uncredentialed request. It is never logged, persisted, placed in screen text, copied into an error, or included in a test snapshot. The implementation and tests describe URL structure using redacted values.

The generic uncredentialed HTTPS redirect policy does not gain a BOMTOON exception. The observed CDN response did not redirect. No account credential can be replayed because the image task has none.

## Signed URL refresh

The app does not use `expiredAt` or the device clock to predict expiry. Device time may be wrong, and a prediction adds state without removing the failure path.

When an image task returns `TaskError::Unauthorized`, the reader refreshes the bearer manifest once and retries the same `orderNo`. A refreshed manifest is accepted for the open episode only when it preserves:

- entry count
- contiguous order
- width and height for every order
- HTTPS origin and path for every order

Only the three signed query values may change. The reader replaces all signed URLs atomically after the complete refreshed manifest passes validation.

A second CDN rejection becomes an image-unavailable error. It does not expire or sign out the BOMTOON account because the rejected request carried no account credential. If the manifest refresh itself returns `NoCredential` or `Unauthorized`, the existing signed-out or expired-account state applies.

Other image failures use the standard task advice and do not refresh the manifest. Retrying from the error screen restarts the current image request. It does not reload the library or change the selected episode.

## Image policy and preparation

`kobo-image::MAX_PIXELS` becomes 7,000,000.

The limit is 6.8 percent above the observed 6,553,600-pixel BOMTOON maximum and 12.7 percent above the current ceiling. At the boundary, the principal buffers remain bounded at approximately 28 MB for an RGBA decode, 14 MB for luminance plus alpha, and 7 MB for final grayscale. An 8-megapixel source remains rejected before allocation.

The limit remains shared by `size()`, `decode()`, `Picture::from_grey`, and resize operations. The public contract accepts exactly 7,000,000 pixels and rejects 7,000,001. BOMTOON does not receive a caller-specific bypass.

`kobo-image` adds a width-scaling operation that preserves aspect ratio and returns a grayscale `Picture`. It validates the target dimensions against the same pixel ceiling before allocation. Existing contain, contain-enlarging, and cover behavior does not change.

The reader scales one decoded source image to the reader picture width. It then copies consecutive row ranges into fixed-height grayscale slices sized for the current display metrics and reader chrome. The implementation reuses the UI layout's physical measurements rather than introducing a Clara-specific pixel constant.

Every non-final slice has the full reader picture height. The final slice is white-padded below the remaining source rows so content stays top-aligned and the screen geometry does not jump. Slices neither overlap nor omit source rows.

The reader retains one scaled source `Picture` while moving among its slices. Crossing to another source drops that decoded picture and fetches the new source. Returning across a source boundary fetches and decodes the previous source again. No compressed or decoded episode cache is retained.

## Reader state and navigation

The BOMTOON state gains a reader view and explicit pending states for manifest, manifest refresh, and image fetch. Reader state contains:

- selected content and episode aliases
- selected display title
- validated manifest metadata
- current source index
- current slice index
- total page count derived from manifest dimensions and slice geometry
- one scaled source picture
- one uploaded slice handle
- whether the current image has already used its manifest-refresh retry

Only the recorded task ID may mutate this state. Pending actions do not turn pages or start a second reader request. Stale outcomes after Back, retry, or a newer task are ignored.

The reader screen uses:

- the standard top bar with the selected episode title
- one unframed picture
- standard Previous and Next page-turn zones
- one-based page position
- application-owned Back to the episode list

Previous and Next first move among slices of the current source. Crossing a source boundary starts that source's image task. Previous on the first page and Next on the final page are harmless no-ops. Back releases the current runtime picture handle, drops decoded source data and signed URLs, restores the episode list page, and ignores any stale task outcome.

The app uploads a new slice under a fresh handle before changing the screen. After the upload succeeds, it queues the new reader screen and then releases the prior handle, so the displayed screen never references a dropped picture. A failed upload is an episode-specific rendering error. Application exit still benefits from the runtime's existing release-all behavior.

## Loading and errors

Loading screens distinguish:

- loading the episode page list
- loading a comic image
- refreshing expired comic links

Manifest `NoCredential` returns to the signed-out screen. Manifest `Unauthorized` returns to the expired-sign-in screen. Manifest parse, unsupported-scramble, image fetch, decode, dimension mismatch, and upload failures stay within the selected episode and offer retry or Back.

Decoded dimensions must equal manifest dimensions. A mismatch invalidates that source and prevents page-count drift. Oversized compressed bodies, dimensions above 7,000,000 pixels, unknown formats, and malformed WebP data retain their existing `kobo-image` distinctions but are presented as an episode image failure.

Logout and account clearing release any current reader handle and remove manifest, source, slice, and selection state along with the existing library, recent, and episode data.

## Tests

### Network policy

Tests permit only the exact bearer manifest GET for valid content and episode aliases. They deny:

- POST and other methods
- session credentials
- wrong secret or header convention
- HTTP, alternate hosts, ports, usernames, or fragments
- missing, reordered, altered, duplicated, or extra query fields
- empty aliases, slashes, traversal, percent-encoded separators, and unsupported characters
- viewer HTML and `/_next/data` routes

Existing library, recent, content, and credential-denial tests remain green.

### API construction and parsing

API tests assert the exact manifest URL, 512 KiB ceiling, bearer credential, headers, and viewer referer.

Parser tests use bounded fixtures shaped after the observed 35- and 81-image responses. They cover:

- valid ordered plain manifests
- one and 256 entries
- 257 entries
- 1024-byte and 1025-byte signed URLs
- missing and wrong-typed fields
- zero and overflowing dimensions
- exactly 7,000,000 and 7,000,001 pixels
- duplicate, skipped, zero, and out-of-order `orderNo`
- wrong scheme, host, port, path, suffix, fragment, credentials, query keys, duplicates, and empty signature values
- non-null `line` or `point`

Fixtures contain redacted synthetic signatures, never captured live values.

### Image operations

`kobo-image` tests cover:

- the new exact pixel boundary and one pixel above it
- width scaling with preserved aspect ratio
- no enlargement unless the new operation explicitly requires it
- target allocation checks before resize
- unchanged source-byte, orientation, alpha-compositing, grayscale, dithering, and existing fit behavior

Reader slicing tests prove that consecutive slices reproduce every scaled source row exactly once, the final slice is white-padded, and arithmetic cannot overflow.

### Reader state and layout

BOMTOON state tests cover:

- actions only for owned and free episodes
- inert sample, not-owned, and unknown statuses
- manifest loading and plain-only rejection
- image loading and decode success
- page turns within one source and across source boundaries
- Previous and Next boundaries
- stable total page count
- one manifest refresh and same-order retry after CDN `Unauthorized`
- refreshed-manifest shape and path mismatch rejection
- no account-state change for CDN failure
- account-state changes for manifest credential failures
- retrying the current episode operation rather than the library
- stale task rejection
- picture handle replacement, Back cleanup, logout cleanup, and exit behavior

Layout checks use `CLARA_BW_METRICS` to prove the top bar, picture, page position, and page-turn zones remain within the panel. The picture is unframed and its final padded slice remains top-aligned.

## Verification

Run focused checks first:

```sh
cargo test -p kobo-image
cargo test -p kobo-net
cargo test -p kobo-bomtoon
cargo check -p kobo-bomtoon --target armv7-unknown-linux-musleabihf
```

Exercise the changed surface in the browser simulator with the authenticated-free episode:

- open the title and episode list
- open the free episode
- turn within one source image
- cross a source-image boundary
- go backward across that boundary
- reach a later page after the original signed URLs have expired, proving refresh and retry
- return to the episode list

Repeat the reader path in the runtime simulator. Record that no purchase, physical Kobo, or captured credential value was used.

Then run repository gates:

```sh
cargo fmt --all --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Files

Expected implementation files:

- `apps/bomtoon/Cargo.toml`
- `apps/bomtoon/src/api.rs`
- `apps/bomtoon/src/main.rs`
- `apps/bomtoon/src/model.rs`
- `apps/bomtoon/src/parse.rs`
- `apps/catalog.json`
- `crates/kobo-image/src/lib.rs`
- `crates/kobo-net/src/lib.rs`

Tests remain beside the affected Rust implementation. No new fixture asset file is required. This design document and its implementation plan live under `docs/superpowers/`.
