# Repository Guidelines

## Project Structure and Module Organization

Cobalt is a Rust 2021 workspace for Kobo applications and platform services.

- `crates/` contains reusable SDK, runtime, UI, networking, simulator, and CLI crates.
- `apps/<app-id>/` contains independently published Store apps. Register each app in the root
  `Cargo.toml` and `apps/catalog.json`.
- `examples/` contains built-in applications and SDK examples.
- `docs/` contains installation, device, SDK, and publishing guides. Screenshots and other app
  assets belong beside their app or example.
- Unit tests normally live beside implementation code in `#[cfg(test)]` modules. Integration
  tests belong in a package's `tests/` directory.

## Build, Test, and Development Commands

Run commands from the repository root unless noted:

```sh
cargo fmt --all --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p kobo-cli -- run --sim --app sudoku
```

For focused work, use `cargo test -p kobo-<app-id>`. From an app directory, run the browser
simulator with `cargo run --manifest-path ../../crates/kobo-cli/Cargo.toml -- dev`.

## Coding Style and Naming Conventions

Use `rustfmt` with 100-column lines and field-init shorthand. Treat Clippy warnings as errors.
Unsafe Rust is forbidden by workspace lint policy. Use `snake_case` for modules and functions,
`PascalCase` for types, and stable lowercase IDs for apps. App packages follow
`kobo-<app-id>`. Keep capability declarations minimal and explicit.

## Testing Guidelines

Add tests for every behavior or safety-boundary change. Name tests after observable behavior,
for example `an_empty_library_uses_recent_reading`. App UI changes should include layout checks
using `CLARA_BW_METRICS`, then be exercised in both browser and runtime simulators. Run the full
format, test, and Clippy gates before opening a pull request.

## Commit and Pull Request Guidelines

Keep commits focused. Use a short imperative subject; conventional prefixes such as
`feat(bomtoon): ...`, `fix: ...`, and `docs: ...` are accepted. Open an issue before large work.
Pull requests must explain behavior and safety changes, link relevant issues, include tests, and
record simulator or device evidence. Add screenshots for visible UI changes.

## Security and Device Safety

Never commit credentials, tokens, serial numbers, or personal network details. Report security
issues through `SECURITY.md`. Device profile changes must fail closed and remain non-write-ready
until the attended display, touch, exit, and recovery evidence is reviewed.
