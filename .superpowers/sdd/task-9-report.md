DONE

Fix SHA: bf4355ea238dc57a58fa62f3c5d022d091e5365d
Review base SHA: 24897cf

Static review:
- Finding 1 — simulator task-file sandbox: `simulator_task_root_at` places ordinary files under an app-private `cobalt-sim-app-files/<app>` root, disjoint from both managed secret and state roots (`crates/kobo-sim/src/lib.rs:2104-2120,2183-2194`). `open_task_file` also rejects symlink components (`crates/kobo-policy/src/tasks.rs:847-874`). Deterministic tests cover direct/traversal auth-root addressing and symlink escape (`crates/kobo-sim/src/lib.rs:2841-2882`; `crates/kobo-policy/src/tasks.rs:1094-1123`).
- Finding 2 — termination-safe browser cleanup: Chrome now runs behind an independent shell supervisor that owns Chrome, traps `HUP`/`INT`/`TERM`, observes CLI-parent death, stops and waits for Chrome, and removes the private profile (`crates/kobo-cli/src/bomtoon.rs:33-77,404-445`). Normal `ChromeGuard` cleanup remains. A local deterministic process test covers owner exit, child termination, wait, and profile removal (`crates/kobo-cli/src/bomtoon.rs:2098-2126`).
- Finding 3 — unmatched-cookie revoke: revocation records whether a cookie was present, invalidates cookie and managed state locally, and returns `RevocationUnconfirmed` when no matching refresh pair can be sent to the provider (`crates/kobo-policy/src/managed.rs:285-313`). The mismatch test proves both local artifacts are removed and the call fails closed (`crates/kobo-policy/src/managed.rs:1411-1424`).
- Finding 4 — simulator install recovery: installation uses fixed marker, temporary, cookie-backup, and state-backup paths; syncs files, renames, marker stages, and directories; runs recovery before mutation; rolls back active stages; and finishes committed cleanup (`crates/kobo-cli/src/bomtoon.rs:1217-1408`). Deterministic crash-window recovery and obstruction tests cover restoration and non-orphaning (`crates/kobo-cli/src/bomtoon.rs:1965-2048`). Managed-provider startup independently performs the same recovery before reading credentials (`crates/kobo-policy/src/managed.rs:230-244,508-583`) with rollback/committed tests (`crates/kobo-policy/src/managed.rs:1327-1408`).
- Finding 5 — device install recovery/exclusion: the remote shell program uses fixed discoverable transaction paths, durable marker transitions, startup recovery before mutation, a bounded fail-closed `flock -w 5`, rollback, and post-commit cleanup (`crates/kobo-cli/src/bomtoon.rs:78-216`). The fake-transport test proves the cookie remains stdin-only and statically checks fixed paths, recovery order, commit order, and absence of PID-suffixed backups (`crates/kobo-cli/src/bomtoon.rs:1786-1842`).
- Finding 6 — cross-instance/process refresh serialization: managed credential operations use a shared `.bomtoon-access-token.lock` with bounded `fs4` acquisition, whose OS lock is released on file/process close (`crates/kobo-policy/src/managed.rs:206-220,639-673`). Providers re-read durable state while holding the lease. Tests prove separate provider instances consume one refresh generation and prove bounded contention plus release-on-drop (`crates/kobo-policy/src/managed.rs:1241-1325`). The simulator also reuses one weak-cached provider per auth root (`crates/kobo-sim/src/lib.rs:2139-2174,2833-2838`).
- Finding 7 — resolve/send generation synchronization: managed Fetch/Post authorization, resolution or forced renewal, and backend dispatch now occur inside one generation-lease closure; a first `Unauthorized` causes exactly one forced renewal and one bounded retry (`crates/kobo-policy/src/tasks.rs:164-188,554-736`; `crates/kobo-policy/src/managed.rs:316-377`). A deterministic backend test proves a competing lease cannot be acquired during dispatch and is available afterward (`crates/kobo-policy/src/tasks.rs:1654-1705`). Existing request-count and renewal tests remain unchanged.

Security/static checks:
- No unsafe Rust, credential logging, credential command arguments, or credential-bearing error text was added.
- Simulator and device install artifacts are fixed, private, and recoverable; the shared lease is bounded and crash-released.
- Secret-bearing request values remain scoped to the managed lease closure or revoke operation and are dropped before result mapping.

Validation: No formatter, build, lint, test, simulator, Chrome, physical-device, SSH, or network validation command was run, as explicitly required. Dependency resolution was performed offline solely to update `Cargo.lock`; full metadata loading could not complete because an unrelated cached `bumpalo v3.20.3` archive was unavailable. Parent validation remains required.

Concerns: The fix adds `fs4` because this workspace's Rust 1.85.1 MSRV predates stable standard-library file locking. The lockfile now records `fs4` and its platform dependencies. The pre-existing unstaged `kobo-net` lockfile dependency additions were deliberately preserved outside the fix commit.

Parent validation:
- `rtk cargo test -p kobo-policy`: 112 passed.
- `rtk cargo test -p kobo-sim`: 25 passed.
- `rtk cargo test -p kobo-cli -- --skip every_uploaded_artifact_is_built_from_this_workspace --skip every_packaged_binary_is_built_with_what_it_needs --skip smoke_build_is_pinned_to_this_workspace_and_feature_targeted`: 219 passed, 2 filtered.
- `rtk cargo fmt --all --check`: passed.
- `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed with no issues.
- `rtk cargo test --workspace --all-targets --all-features -- --skip every_uploaded_artifact_is_built_from_this_workspace --skip every_packaged_binary_is_built_with_what_it_needs --skip smoke_build_is_pinned_to_this_workspace_and_feature_targeted`: 2191 passed, 2 ignored, 3 filtered.
- Exact `rtk cargo test --workspace --all-targets --all-features`: 227 CLI tests passed and only the three pre-existing ARM C cross-compiler-dependent tests failed.
- `rtk cargo run -p kobo-cli -- run --sim --app bomtoon`: exited 0, connected `bomtoon`, and rendered the direct `Loading your library` screen. No session-check screen appeared.
- The simulator terminal output contained no credential, token response, email, user ID, IP address, or CDP payload. No physical Kobo, SSH, or device-discovery command ran.

Attended evidence still requiring the operator's BOMTOON credentials:
- Complete `kobo bomtoon login --sim` in the temporary Chrome profile.
- Exercise authenticated library, recent, episode, Sign out, and re-login behavior.

Formal final re-review: approved; all seven security findings are resolved.
