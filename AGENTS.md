# Repository Guidelines

## Project Overview

Cobalt is a Rust 2021 workspace for Kobo e-readers. It contains the SDK, runtime daemon,
hardware and networking services, CLI/simulators, built-in examples, and independently
published Store apps. This checkout includes the Bomtoon reader app in `apps/bomtoon/`.

## Architecture & Data Flow

- Apps and examples implement `kobo_sdk::KoboApp`, keep mutable UI/domain state in one app
  struct, render with `ScreenBuilder`, and start with `kobo_sdk::run("<app-id>", app)`.
- `crates/kobo-sdk` exposes lifecycle callbacks and `Context`; `crates/kobo-protocol` defines
  the closed request/result types exchanged with the runtime.
- `crates/kobod` owns the event loop. It enforces `kobo-policy` capabilities, dispatches network
  work through `kobo-net`, device work through `kobo-hal`, and returns exactly one callback result.
- App work is event-driven: an action calls `Context::spawn(Task)`, `on_task` receives a
  `TaskOutcome`, state changes, and the app renders a new screen. Apps should not create their own
  network clients, threads, or competing async runtimes.
- Bomtoon follows `on_action` -> `apps/bomtoon/src/api.rs` task -> `on_task` ->
  `parse.rs`/`model.rs` -> state mutation -> `show`. Reader, cover, shelf, wallet, and navigation
  state live in `apps/bomtoon/src/main.rs`.
- Inputs cross explicit boundaries: parsers validate and bound remote data, protocol enums keep
  wire states closed, and policy grants clamp device/network capabilities before execution.

## Key Directories

- `crates/`: reusable SDK, protocol, runtime, UI, policy, networking, document/image, simulator,
  CLI, and hardware crates.
- `apps/<app-id>/`: Store apps. Register each app in root `Cargo.toml` and `apps/catalog.json`.
- `examples/`: built-in apps and SDK examples; screenshots and app-specific instructions live
  beside each example.
- `docs/`: installation, development, device support/porting, Store, and publishing procedures.
- `scripts/`: repeatable icon generation, device recording, screenshots, redaction, and tour tools.
- `tools/`: repository utilities, including the optional credential-scanning pre-commit hook.

## Development Commands

Run commands from the repository root unless noted. Use the CI form with `--all-targets`; some
older prose documentation omits it.

```sh
cargo build --workspace
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings

cargo test -p kobo-bomtoon
cargo run -p kobo-cli -- run --sim --app bomtoon
cargo run -p kobo-cli -- dev --builtin
cargo run --locked -p kobo-cli -- app-check --registry apps/catalog.json
```

From an app directory, launch the browser simulator with:

```sh
cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev
```

For a device build, first install the ARM target and cross compiler, then run:

```sh
rustup target add armv7-unknown-linux-musleabihf
cargo run -p kobo-cli -- build --device
```

## Code Conventions & Common Patterns

- `rustfmt.toml` sets 100-column lines and field-init shorthand. Workspace lints forbid unsafe
  Rust and enable Clippy `all` and `pedantic`; CI promotes warnings to errors.
- Use `snake_case` for modules/functions, `PascalCase` for types, stable lowercase app IDs, and
  package names such as `kobo-bomtoon`.
- Organize app modules by domain (`api`, `model`, `parse`, `reader`, `services`, `tasks`) rather
  than adding generic utility layers.
- Prefer closed enums, typed IDs, checked arithmetic, bounded collections/strings, and exhaustive
  matching. Preserve fail-closed behavior at protocol, parser, capability, and device boundaries.
- Return typed errors such as `ParseError` or `TaskError`; convert them to an explicit user-facing
  problem/retry state at the UI boundary. Do not suppress malformed input or collapse cancellation,
  offline, unreachable, and protocol failures into one path.
- Dependency injection is explicit through callback arguments and `&mut Context`. State belongs in
  the app struct; avoid mutable globals and service locators.
- Async work uses SDK `Task`/`TaskOutcome` callbacks. Keep request ownership and pending state
  explicit, cancel deferred/reader work on suspension, and handle stale results deterministically.
- Declare only required app capabilities in `apps/catalog.json`; never bypass policy with direct
  sockets, shell access, or hardware writes.

## Important Files

- `Cargo.toml`, `Cargo.lock`: workspace membership, shared versions, release profile, and lints.
- `.cargo/config.toml`, `.cargo/audit.toml`, `rustfmt.toml`, `clippy.toml`: target and
  quality rules.
- `.github/workflows/ci.yml`: authoritative host tests, lint gates, ARM check, catalog check, audit.
- `apps/catalog.json`: Store registration, versions, minimum Cobalt versions, and capabilities.
- `apps/bomtoon/src/main.rs`: Bomtoon entry point, lifecycle callbacks, state machine, rendering.
- `apps/bomtoon/src/{api,parse,model}.rs`: remote tasks, validation, and domain model.
- `crates/kobo-sdk/src/lib.rs`: app contract, `Context`, runner, lifecycle and result dispatch.
- `crates/kobo-protocol/src/lib.rs`: task, device, Store, shell, and wire result types.
- `crates/kobod/src/main.rs`: runtime entry point and service dispatch.
- `crates/kobo-ui/src/lib.rs`, `crates/kobo-policy/src/lib.rs`, `crates/kobo-net/src/lib.rs`:
  rendering, capability enforcement, and bounded TLS networking.
- `docs/DEVELOPING.md`, `docs/DEVICES.md`, `docs/PORTING.md`: simulator workflows and mandatory
  device safety/evidence procedures.

## Runtime/Tooling Preferences

- Required baseline: Rust `1.85.1`, edition 2021, Cargo. There is no `rust-toolchain` file; honor
  `workspace.package.rust-version` and the CI pin.
- Cargo and the `kobo` CLI are the task surface; there is no Makefile, Justfile, or JS package
  manager. Node is used only for isolated tooling such as the browser-extension contract test and
  generated app pages.
- Device artifacts target `armv7-unknown-linux-musleabihf`. Install
  `armv7-unknown-linux-musleabihf` tooling on macOS or `gcc-arm-linux-gnueabihf` on Debian/Ubuntu.
- Hardware writes are opt-in through `device-write`, exact device/profile checks, and attended
  confirmation. Read `docs/DEVICES.md` and `docs/PORTING.md` before changing profiles or
  write paths.
- Keep credentials out of source and logs. Runtime secrets use `kobo secret set`; never commit
  tokens, signing seeds, serial numbers, SSH keys, SSIDs, or personal network details.

## Testing & QA

- Unit tests live beside Rust code in `#[cfg(test)]` modules; integration tests and fixtures live in
  `<crate>/tests/`. Use standard `#[test]` and behavior-oriented names.
- Tests should defend observable contracts: state transitions, parser limits, exact wire/layout
  output, malformed input, security refusal, cancellation, and deterministic error behavior.
- Focused examples:

  ```sh
  cargo test -p kobo-opds --test parity
  cargo test -p kobo-ui contact_sheet -- --ignored --nocapture
  node --test crates/kobo-cli/bomtoon-extension/tests/extension.test.js
  ```

- App UI changes require layout checks with `CLARA_BW_METRICS`, then exercise in both browser and
  runtime simulators. Use `kobo drive` for scripted touch/layout flows and failure screenshots.
- Visible or hardware-dependent changes need simulator/device evidence. `scripts/record-apps.sh`
  and `scripts/shoot-apps.sh` cover repeatable device capture; redact SSIDs before committing
  assets.
- Device profile changes remain `write_ready: false` until owner-attended display, touch, exit, and
  recovery evidence is reviewed. Simulator evidence does not replace this gate.
- No repository coverage tool or numeric threshold is configured. Behavioral and safety-boundary
  coverage, full workspace tests, formatting, Clippy, and the ARM/catalog checks are the gates.
