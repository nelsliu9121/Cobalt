# BOMTOON Private Device Installation Design

## Problem

`kobo-bomtoon` is registered as a Store application, but it is not part of the Cobalt platform payload. `crates/kobo-cli/src/main.rs` lists it in `STORE_PACKAGES` and omits it from `INSTALLED_PACKAGES`. A normal USB setup therefore installs Cobalt without the BOMTOON executable, and installing BOMTOON through the fixed signed catalog would use the public App Store release path that is outside this work.

The target is one owner-attended Kobo Libra Colour with the exact supported identity N428, device code 390, firmware 4.45.23697, and kernel 4.9.77. Cobalt is already installed. The finished device must run the current local BOMTOON source, sign in through the existing managed-credential flow, open an owned or free episode, and return cleanly to the stock Kobo reader. No public release or App Store publication is allowed.

## Decision

Build BOMTOON into a private Cobalt USB payload as a built-in application.

Add `kobo-bomtoon` to `INSTALLED_PACKAGES` in `crates/kobo-cli/src/main.rs` and add BOMTOON to `MANAGED_BUILTINS` in `crates/kobod/src/app_store.rs`. The package entry puts the executable on the device; the runtime entry exposes its verified built-in metadata and `network` capability to the launcher. Retain the existing `STORE_PACKAGES` and `apps/catalog.json` entries because they remain the source of public metadata and simulator/package validation. Do not change the catalog version, minimum Cobalt version, release registry, generated website, signing configuration, release workflow, or public catalog assets.

This is a private source-build decision, not a new distribution channel. The existing package builder remains responsible for compiling the static ARM hard-float executable, verifying its ELF shape, placing it at `.adds/cobalt/bin/kobo-bomtoon`, and refusing every archive path outside `.adds/cobalt`.

## Alternatives Rejected

### One-off binary copy

Building and manually copying only `kobo-bomtoon` would avoid a source change, but it would create device state that the repository cannot reproduce. It would also bypass package-membership tests and the normal archive inspection/read-back path. This is not suitable for a load-bearing real-device installation.

### Private signed catalog

A private catalog would preserve Store-style app isolation, but it would require a private signing key, a separately trusted catalog configuration, HTTPS package hosting, and runtime trust-root or catalog-URL changes. That is a new distribution system and is unnecessary for one private device.

## Source Changes

The implementation changes only the local platform package membership, runtime built-in registration, and their focused regression coverage:

- Add `("kobo-bomtoon", None)` to `INSTALLED_PACKAGES` in the normal application order.
- Add a BOMTOON entry to `MANAGED_BUILTINS` with ID `bomtoon`, display name and short label `BOMTOON`, catalog version `0.6.0`, book glyph, and exactly the `network` capability.
- Extend the existing CLI package-membership tests to prove the generated Cobalt payload contains `.adds/cobalt/bin/kobo-bomtoon` as an executable.
- Extend the existing runtime app-store tests to prove the present binary is listed as an included built-in, carries the expected metadata and capability, and launches by ID without a Store-installed manifest.
- Preserve the existing invariant that the payload contains no path outside `.adds/cobalt`.
- Leave `STORE_PACKAGES`, `apps/catalog.json`, BOMTOON application behavior, protocol, policy, networking, login implementation, and public release files unchanged.

No app version bump is required because no public package or catalog entry is being released.

## Host-Side Readiness Gates

All non-device checks complete before an attended write:

1. Run the focused `kobo-cli` package-membership tests and `kobod` managed-built-in tests.
2. Run the complete `kobo-bomtoon` test target.
3. Validate `apps/catalog.json` with the existing `app-check` command to prove the unchanged public metadata remains valid.
4. Confirm the ARM target and cross-compiler are available.
5. Build the USB payload with the existing `kobo package` path.
6. Inspect `target/KoboRoot.tgz` with the existing package inspector.
7. Prove the inspected archive contains `.adds/cobalt/bin/kobo-bomtoon`, marks it executable, and contains no path outside `.adds/cobalt`.
8. Run `kobo setup --dry-run --enable-ssh` against the mounted reader and inspect the complete plan before allowing writes.

A test failure, build failure, missing toolchain component, malformed archive, unexpected archive member, failed dry run, or device identity mismatch stops the procedure before installation.

## Attended USB Installation

The reader must be charged, awake, connected with a data-capable USB cable, and explicitly placed in USB connected mode. The mounted volume must identify as the supported Libra Colour profile before any write.

Run the existing USB setup with `--enable-ssh`. Setup builds before writing, installs the complete locally built Cobalt payload, reads every copied file back, stages the dedicated `kobo_cobalt` public key, and enables the firmware-provided SSH server for the next boot. It must not enable telnet, FTP, a password login, or any third-party SSH server.

After setup ejects the volume:

1. Restart the reader once.
2. Leave NickelMenu undisturbed for at least one minute.
3. Join the intended local Wi-Fi network.
4. Open Cobalt once from NickelMenu so the root-owned startup path installs the staged public key and deletes the staged copy.
5. Use the address reported by the existing read-only device discovery flow.

The exact model, device code, firmware, kernel, framebuffer, and touch profile remain authoritative. A product-name match alone is insufficient, and no profile gate may be bypassed.

## Credential Provisioning

Run the existing attended login flow:

```sh
cargo run -p kobo-cli -- bomtoon login --device <address>
```

The browser flow installs the BOMTOON session through the existing managed-credential provider. Credential values must never enter source, the USB package, command output, logs, screenshots, or the design evidence. A login failure leaves the application signed out and does not justify a direct secret copy or policy bypass.

The credential is independent of SSH after provisioning. Disabling the server and removing the developer key must not remove the managed BOMTOON credential.

## First Real-Device Smoke Check

With temporary SSH still available, perform an owner-attended smoke check:

1. Open the Cobalt launcher and confirm BOMTOON appears as an included local application.
2. Launch BOMTOON and confirm it is signed in.
3. Load the account library or recent list.
4. Open one owned or free episode.
5. Confirm the first page renders legibly on the 1264×1680 colour panel.
6. Navigate forward and backward using touch; exercise the physical page buttons where the current reader flow supports them.
7. Exit the episode, exit BOMTOON, and use Cobalt's return action to restore the stock Kobo reader.

No purchase, rental, Gift redemption, coin spend, ticket spend, or other commerce mutation is part of this smoke check.

If display, touch, navigation, or exit behavior is wrong, stop. A long power-button reboot remains the recovery path to the stock reader; the profile gate must not be weakened to continue.

## Temporary SSH Teardown

`kobo setup --undo` must not be used because it removes Cobalt as well as disabling SSH.

While the authenticated SSH session is still active:

1. Determine root's actual home directory from the device account database.
2. Remove only the authorized-key line installed from `~/.ssh/kobo_cobalt.pub`, identified by its exact public key and `kobo-cobalt` comment.
3. Preserve every unrelated authorized key and verify the dedicated line is absent.

Then reconnect the reader over USB and rename the firmware marker `.kobo/ssh-enabled` back to `.kobo/ssh-disabled`. Eject and restart the reader. The marker change disables the firmware SSH server at boot; it does not remove Cobalt or the managed BOMTOON credential.

If the dedicated key cannot be removed exactly, do not rewrite or replace the complete `authorized_keys` file. Stop and inspect the file while the current session remains available.

## Final Real-Device Acceptance

After the SSH-disabled reboot:

1. Confirm the firmware SSH server no longer accepts a connection from the local network.
2. Open Cobalt and launch BOMTOON without SSH.
3. Confirm the managed session still signs the user in.
4. Reopen the same owned or free episode and navigate at least one page.
5. Exit BOMTOON and return cleanly to the stock Kobo reader.

The installation is complete only when both attended smoke checks pass: once before SSH teardown and once after the server is disabled and its dedicated key is removed.

## Failure and Recovery Behavior

- Host-side failures stop before device writes.
- Low battery, an unmounted volume, an unrecognized volume, or any exact-profile mismatch stops installation.
- USB read-back mismatch is a failed installation; do not proceed to credential provisioning.
- Login failure leaves BOMTOON signed out. Remove the temporary key and disable SSH before further diagnosis.
- A Cobalt runtime or rendering failure is recovered by rebooting to the stock reader. Do not retry by bypassing the device profile.
- SSH teardown failure leaves the current authenticated session available for inspection; never delete all authorized keys as a shortcut.
- Public Store state remains unchanged throughout. No release, tag, catalog upload, signing action, or website generation is performed.

## Scope

In scope: private built-in package membership, focused package regression coverage, host-side ARM/package proof, one attended Libra Colour USB update, existing managed login, temporary SSH teardown, and pre/post-teardown device smoke checks.

Out of scope: App Store publishing, private catalog infrastructure, app version changes, new signing keys, generated app pages, new login protocols, direct credential copying, device profile changes, firmware changes, commerce mutations, unrelated refactoring, and support claims for any other model or firmware.
