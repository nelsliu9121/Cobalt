# BOMTOON Private Device Installation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Install the current local BOMTOON build as an included Cobalt application on one supported Kobo Libra Colour, provision its managed session, and leave the finished reader usable with firmware SSH disabled.

**Architecture:** Reuse Cobalt's existing built-in application path. `kobo-cli` adds the BOMTOON ARM executable to the checked USB platform payload; `kobod` adds matching built-in metadata and the existing `network` capability so Launcher can enumerate and resolve it without a Store-installed manifest. Existing USB setup, managed BOMTOON login, package inspection, profile refusal, and stock-reader recovery remain unchanged.

**Tech Stack:** Rust 2021, Rust 1.85.1, Cargo, `armv7-unknown-linux-musleabihf`, existing `kobo-cli` package/setup/inspect commands, existing `kobod` app-store registry, existing managed BOMTOON credential provider, Kobo Libra Colour firmware 4.45.23697.

## Global Constraints

- Target exactly one owner-attended Kobo Libra Colour: N428, device code 390, firmware 4.45.23697, kernel 4.9.77.
- The exact framebuffer, touch, device code, model prefix, firmware, and kernel profile gates remain enabled; never bypass a refusal.
- Keep `STORE_PACKAGES`, `apps/catalog.json`, public signing configuration, generated website, release workflows, and public catalog assets unchanged.
- Do not bump the BOMTOON app version or Cobalt version for this private build.
- Add no dependency, new distribution channel, private catalog, signing key, login protocol, or credential-copy path.
- BOMTOON receives exactly the existing `network` capability.
- Never print, log, package, screenshot, or commit credential values.
- Do not exercise purchase, rental, Gift, coin, ticket, or other commerce mutations during device verification.
- Temporary SSH uses only the firmware-provided server and the dedicated `~/.ssh/kobo_cobalt` key.
- Never run `kobo setup --undo` during teardown; it removes Cobalt.
- Preserve unrelated authorized keys when removing the dedicated `kobo-cobalt` line.
- The existing untracked research files under `docs/research/` are user work and remain untouched.
- The approved contract is `docs/superpowers/specs/2026-09-01-bomtoon-private-device-install-design.md`.

---

### Task 1: Include BOMTOON in the Checked USB Payload

**Files:**
- Modify: `crates/kobo-cli/src/main.rs:44-61`
- Test: `crates/kobo-cli/src/main.rs:5891-5905`

**Interfaces:**
- Consumes: `INSTALLED_PACKAGES: &[(&str, Option<&str>)]`, `device_build_command`, and existing package assembly in `build_package_bytes`.
- Produces: package member `mnt/onboard/.adds/cobalt/bin/kobo-bomtoon`, built without additional Cargo features.

- [ ] **Step 1: Add the failing package-membership regression**

Add this test beside `every_installed_package_is_a_member_of_this_workspace`:

```rust
#[test]
fn private_device_package_includes_bomtoon() {
    let (_, features) = super::INSTALLED_PACKAGES
        .iter()
        .find(|(name, _)| *name == "kobo-bomtoon")
        .expect("private device payload must include BOMTOON");
    assert_eq!(*features, None, "BOMTOON needs no package-only Cargo feature");
}
```

- [ ] **Step 2: Run the test and observe RED**

Run:

```sh
cargo test -p kobo-cli private_device_package_includes_bomtoon -- --nocapture
```

Expected: FAIL at `private device payload must include BOMTOON` because the package is still Store-only.

- [ ] **Step 3: Add the minimal package entry**

Insert BOMTOON after Audiobook in `INSTALLED_PACKAGES`:

```rust
    ("kobo-audiobook", None),
    ("kobo-bomtoon", None),
    ("kobo-terminal", None),
```

Do not remove its `STORE_PACKAGES` entry.

- [ ] **Step 4: Run focused CLI package tests**

Run:

```sh
cargo test -p kobo-cli private_device_package_includes_bomtoon -- --nocapture
cargo test -p kobo-cli every_installed_package_is_a_member_of_this_workspace -- --nocapture
cargo test -p kobo-cli a_package_writes_nothing_outside_the_install_root -- --nocapture
```

Expected: all three PASS. The workspace-membership test resolves `kobo-bomtoon` to the existing `apps/bomtoon` workspace member; the archive-root invariant remains unchanged.

- [ ] **Step 5: Commit the payload membership**

```sh
git add crates/kobo-cli/src/main.rs
git commit -m "build(cli): include bomtoon in private device package"
```

---

### Task 2: Expose BOMTOON as a Managed Built-In Application

**Files:**
- Modify: `crates/kobod/src/app_store.rs:32-139`
- Test: `crates/kobod/src/app_store.rs:1009-1078,1503-1537`

**Interfaces:**
- Consumes: `BuiltinApp`, `MANAGED_BUILTINS`, `installed`, `resolve`, `builtin_declared`, `builtin_binary`, `Glyph::Book`, and `kobo_policy::Capability::Network`.
- Produces: managed built-in ID `bomtoon`, title `BOMTOON`, label `BOMTOON`, summary `Read owned and free BOMTOON episodes on your Kobo.`, version `0.6.0`, glyph `Book`, and exactly one declared capability: `Network`.

- [ ] **Step 1: Add failing built-in listing and capability tests**

Add this test after `bundled_apps_update_uninstall_and_reinstall_in_place`:

```rust
#[test]
fn private_bomtoon_binary_is_listed_and_resolved_as_a_builtin() {
    let root = root();
    fs::create_dir_all(root.join("bin")).expect("built-in directory");
    fs::write(builtin_binary(&root, "bomtoon"), b"private BOMTOON binary")
        .expect("built-in BOMTOON");

    let entries = installed(&root).expect("installed built-ins");
    let bomtoon = entries
        .iter()
        .find(|entry| entry.id == "bomtoon")
        .expect("BOMTOON launcher entry");
    assert_eq!(bomtoon.title, "BOMTOON");
    assert_eq!(bomtoon.label, "BOMTOON");
    assert_eq!(
        bomtoon.summary,
        "Read owned and free BOMTOON episodes on your Kobo."
    );
    assert_eq!(bomtoon.version, "0.6.0");
    assert_eq!(bomtoon.installed_version.as_deref(), Some("0.6.0"));
    assert_eq!(bomtoon.glyph, Glyph::Book);
    assert_eq!(bomtoon.capabilities, vec!["network".to_owned()]);
    assert_eq!(
        resolve(&root, "bomtoon").expect("resolve built-in BOMTOON"),
        builtin_binary(&root, "bomtoon")
    );

    let _ignored = fs::remove_dir_all(root);
}
```

Extend `bundled_apps_use_the_same_capabilities_as_their_store_manifests` with:

```rust
    let bomtoon = builtin_declared("bomtoon").expect("BOMTOON declaration");
    assert_eq!(
        bomtoon.iter().collect::<Vec<_>>(),
        vec![kobo_policy::Capability::Network]
    );
```

Update the already-stale assertions in `bomtoon_catalog_requires_compatible_runtime_and_only_network` to the current unchanged registry values, then require the built-in entry to match them:

```rust
    assert_eq!(version, "0.6.0");
    assert_eq!(minimum, "0.5.0");
    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].as_str(), Some("network"));
    assert_eq!(env!("CARGO_PKG_VERSION"), "0.5.0");

    let builtin = managed_builtin("bomtoon").expect("BOMTOON built-in entry");
    assert_eq!(builtin.version, version);
    assert_eq!(builtin.capabilities, &["network"]);
```

Retain the existing compatibility assertions below this block.

- [ ] **Step 2: Run the new tests and observe RED**

Run:

```sh
cargo test -p kobod private_bomtoon_binary_is_listed_and_resolved_as_a_builtin -- --nocapture
cargo test -p kobod bundled_apps_use_the_same_capabilities_as_their_store_manifests -- --nocapture
cargo test -p kobod bomtoon_catalog_requires_compatible_runtime_and_only_network -- --nocapture
```

Expected: the new listing test cannot find a BOMTOON launcher entry, the capability test cannot find a BOMTOON declaration, and the catalog test cannot find matching built-in metadata.

- [ ] **Step 3: Add the minimal managed built-in entry**

Insert this entry after Audiobook in `MANAGED_BUILTINS`:

```rust
    BuiltinApp {
        id: "bomtoon",
        title: "BOMTOON",
        label: "BOMTOON",
        summary: "Read owned and free BOMTOON episodes on your Kobo.",
        version: "0.6.0",
        glyph: Glyph::Book,
        capabilities: &["network"],
    },
```

Do not add BOMTOON to `SYSTEM_APPS`; it is a managed application so a future public package can still update, remove, or reinstall it through the existing Store semantics.

- [ ] **Step 4: Format and run the focused runtime tests**

Run:

```sh
cargo fmt --all
cargo test -p kobod private_bomtoon_binary_is_listed_and_resolved_as_a_builtin -- --nocapture
cargo test -p kobod bundled_apps_use_the_same_capabilities_as_their_store_manifests -- --nocapture
cargo test -p kobod bomtoon_catalog_requires_compatible_runtime_and_only_network -- --nocapture
cargo test -p kobod remote_install_plans_distinguish_new_updated_current_and_included_apps -- --nocapture
```

Expected: all four PASS. A present flat binary is listed and resolved as included, declares only Network, matches catalog version/capability metadata, and retains existing included-app Store behavior.

- [ ] **Step 5: Commit the runtime registration**

```sh
git add crates/kobod/src/app_store.rs
git commit -m "feat(runtime): list bomtoon as a bundled app"
```

---

### Task 3: Prove the Host Build and USB Artifact

**Files:**
- Verify only: `crates/kobo-cli/src/main.rs`
- Verify only: `crates/kobod/src/app_store.rs`
- Verify only: `apps/bomtoon/src/{api,commerce,feature,main,model,parse}.rs`
- Artifact: `target/KoboRoot.tgz` (generated, never committed)

**Interfaces:**
- Consumes: the Task 1 package entry, Task 2 built-in metadata, root `apps/catalog.json`, and existing `kobo package`, `kobo inspect`, and `app-check` commands.
- Produces: a locally verified ARM USB archive whose inspected member list contains executable `mnt/onboard/.adds/cobalt/bin/kobo-bomtoon` and no path outside `mnt/onboard/.adds/cobalt`.

- [ ] **Step 1: Run formatting and focused source gates**

Run:

```sh
cargo fmt --all -- --check
cargo test -p kobo-cli private_device_package_includes_bomtoon -- --nocapture
cargo test -p kobod private_bomtoon_binary_is_listed_and_resolved_as_a_builtin -- --nocapture
cargo test -p kobod bundled_apps_use_the_same_capabilities_as_their_store_manifests -- --nocapture
cargo test -p kobod bomtoon_catalog_requires_compatible_runtime_and_only_network -- --nocapture
cargo test -p kobo-bomtoon
```

Expected: every command exits 0. The complete BOMTOON target passes without changing application, parser, networking, policy, or commerce behavior.

- [ ] **Step 2: Validate the unchanged public registry**

Run the repository's locked catalog gate:

```sh
cargo run --locked -p kobo-cli -- app-check --registry apps/catalog.json
```

Expected: every registered application verifies; BOMTOON reports version `0.6.0`, minimum Cobalt `0.5.0`, and only `network`. This command validates/builds public entries locally but performs no signing, upload, release, tag, or publication.

- [ ] **Step 3: Ensure the ARM toolchain is available**

Run:

```sh
rustup target add armv7-unknown-linux-musleabihf
which armv7-unknown-linux-musleabihf-gcc
```

Expected: rustup reports the target installed and `which` prints the Homebrew cross-compiler path. If the compiler is missing on macOS, install the documented toolchain before continuing:

```sh
brew install messense/macos-cross-toolchains/armv7-unknown-linux-musleabihf
```

- [ ] **Step 4: Build the private USB payload**

Run:

```sh
cargo run -p kobo-cli -- package
```

Expected: exit 0 and a generated `target/KoboRoot.tgz`. Every device-side executable, including `kobo-bomtoon`, passes the existing static ARM ELF checks; `kobod` retains `device-write`.

- [ ] **Step 5: Inspect the exact generated archive**

Run:

```sh
cargo run -p kobo-cli -- inspect target/KoboRoot.tgz
shasum -a 256 target/KoboRoot.tgz
```

Expected inspector evidence includes a line equivalent to:

```text
file 755 [nonzero byte count] mnt/onboard/.adds/cobalt/bin/kobo-bomtoon
```

The inspector must end with:

```text
nothing outside mnt/onboard/.adds/cobalt; this package writes no root filesystem file
```

Record the printed SHA-256 in the execution report. Do not commit `target/KoboRoot.tgz`.

---

### Task 4: Install, Provision, Verify, and Disable SSH

**Files:**
- Device payload: `/mnt/onboard/.adds/cobalt/`
- Firmware SSH marker: `/mnt/onboard/.kobo/ssh-enabled` → `/mnt/onboard/.kobo/ssh-disabled`
- Root authorized keys: the existing `authorized_keys` under root's actual home directory
- Repository files: none

**Interfaces:**
- Consumes: the verified source and artifact from Task 3, a charged connected Kobo Libra Colour, existing `kobo setup`, `kobo devices`, `kobo bomtoon login`, and `kobo shell` commands.
- Produces: installed private BOMTOON binary, launcher built-in metadata, managed BOMTOON session, two attended smoke-check results, removed dedicated developer key, and firmware SSH disabled after reboot.

- [ ] **Step 1: Establish the attended device gate**

Before any device command, the owner must confirm all of the following:

- the reader reports Kobo Libra Colour firmware `4.45.23697`;
- the battery is charged sufficiently for a restart/update;
- the USB cable carries data;
- the reader has been tapped into **Connect** mode;
- `/Volumes/KOBOeReader` is mounted;
- the owner is present for every display, touch, reboot, and recovery check.

Stop if any condition is false.

- [ ] **Step 2: Run the read-only setup plan**

Run:

```sh
cargo run -p kobo-cli -- setup --dry-run --enable-ssh
```

Expected: the plan identifies the mounted Kobo, installs the local Cobalt payload, stages only this machine's public key, renames the firmware's SSH marker, preserves the exact device-profile refusal boundary, and performs no write because `--dry-run` is active. Stop on an unrecognized volume, firmware/profile mismatch, unexpected root path, or any plan different from the approved design.

- [ ] **Step 3: Perform the USB setup**

Run:

```sh
cargo run -p kobo-cli -- setup --enable-ssh
```

Expected before physical interaction: build succeeds, every copied file reads back byte-for-byte, the volume ejects, and the command asks for the reader restart. Hold the power button to shut down, restart once, leave NickelMenu untouched for at least one minute, reconnect Wi-Fi, and open Cobalt once. This installs the staged dedicated public key and deletes its staged copy. Let setup/device discovery report the reader's exact IPv4 address and assign that value to the host shell variable `KOBO_IP` for the remaining commands.

- [ ] **Step 4: Confirm read-only identity over the temporary SSH path**

Run:

```sh
cargo run -p kobo-cli -- devices
```

Expected: the row for `$KOBO_IP` identifies N428, firmware 4.45.23697, and the newly installed local Cobalt version. Stop if the model, firmware, or reported device identity differs.

- [ ] **Step 5: Provision the existing managed BOMTOON session**

Run:

```sh
cargo run -p kobo-cli -- bomtoon login --device "$KOBO_IP"
```

Complete the attended browser login. Expected: the CLI reports successful session installation without printing the session value. Do not use `kobo secret set`, copy browser cookies manually, or capture login output in a screenshot.

If login fails, leave BOMTOON signed out and continue directly to Steps 7-9 to remove the temporary key and disable SSH before diagnosing further.

- [ ] **Step 6: Perform the first owner-attended smoke check**

On the reader:

1. Open Launcher and confirm **BOMTOON** appears.
2. Open BOMTOON and confirm the account is signed in.
3. Load Library or Recent.
4. Open one owned or free episode; do not trigger any commerce action.
5. Confirm the first page is legible at the Libra Colour's 1264×1680 panel size.
6. Navigate forward and backward by touch; exercise physical page buttons where the reader flow supports them.
7. Exit the episode and BOMTOON, then select **Return to Kobo reader**.

Expected: no malformed layout, stuck input, unintended purchase/rental/Gift action, runtime exit, or profile refusal. If the panel or input is wrong, stop and recover with a long power-button reboot; never weaken the profile check.

- [ ] **Step 7: Identify and remove only the dedicated authorized key**

On the host, print the expected public line:

```sh
cat "$HOME/.ssh/kobo_cobalt.pub"
cargo run -p kobo-cli -- shell --device "$KOBO_IP"
```

At the device shell, locate root's actual home and require exactly one dedicated key line:

```sh
set -eu
home=$(awk -F: '$1 == "root" { print $6 }' /etc/passwd)
keys="$home/.ssh/authorized_keys"
test -f "$keys"
matches=$(grep -c ' kobo-cobalt$' "$keys")
test "$matches" -eq 1
grep ' kobo-cobalt$' "$keys"
```

Compare the one printed device line byte-for-byte with the host's `~/.ssh/kobo_cobalt.pub` output. Do not continue if they differ. Once they match, remove exactly that line while preserving all others:

```sh
before=$(wc -l < "$keys")
cp "$keys" "$keys.before-cobalt-removal"
awk '!/ kobo-cobalt$/' "$keys" > "$keys.new"
chmod 600 "$keys.new"
mv "$keys.new" "$keys"
after=$(wc -l < "$keys")
test "$after" -eq "$((before - 1))"
if grep -q ' kobo-cobalt$' "$keys"; then exit 1; fi
rm "$keys.before-cobalt-removal"
exit
```

Expected: exactly one line disappears, every unrelated key remains, and the active session exits normally. If any assertion fails, keep the current session available for inspection; never replace or clear the whole file.

- [ ] **Step 8: Disable the firmware SSH server over USB**

Reconnect USB, tap **Connect**, and require the enabled marker to be the only marker present:

```sh
test -f /Volumes/KOBOeReader/.kobo/ssh-enabled
test ! -e /Volumes/KOBOeReader/.kobo/ssh-disabled
mv /Volumes/KOBOeReader/.kobo/ssh-enabled /Volumes/KOBOeReader/.kobo/ssh-disabled
sync
diskutil eject /Volumes/KOBOeReader
```

Restart the reader. Do not run `kobo setup --undo`; the marker rename disables the firmware server without removing `.adds/cobalt` or the managed BOMTOON credential.

- [ ] **Step 9: Prove SSH is closed after reboot**

After the reader rejoins Wi-Fi, run:

```sh
if nc -z "$KOBO_IP" 22; then
    echo "refusing completion: Kobo SSH still accepts connections" >&2
    exit 1
else
    echo "Kobo SSH is closed"
fi
```

Expected: `Kobo SSH is closed`. If DHCP changed the address, determine the reader's current address from the router or Kobo network details and repeat the same port check against that exact address; do not infer closure from the old address alone.

- [ ] **Step 10: Perform final no-SSH acceptance**

On the reader, with SSH confirmed closed:

1. Open Cobalt and BOMTOON.
2. Confirm the managed session still signs in.
3. Reopen the same owned or free episode.
4. Navigate at least one page.
5. Exit BOMTOON and return cleanly to the stock Kobo reader.

Expected: the private built-in, credential, reader, and exit path all work without SSH. Completion evidence consists of the Task 3 test/build/inspection outputs, exact supported identity, successful login result without secret material, both attended smoke checks, dedicated-key removal, and the post-reboot closed-port check.
