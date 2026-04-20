# Hashiverse rust server and libs

This is Hashiverse.

## Builds

- [![build-server](https://github.com/hashiverse/hashiverse/actions/workflows/build-server.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/build-server.yml)

## Tests

- [![test-hashiverse-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml)
- [![test-hashiverse-server-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-server-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-server-lib-default.yml)
- [![test-hashiverse-lib-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml)
- [![test-hashiverse-client-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-wasm.yml)

## Fuzzing

- [![fuzz-hashiverse-lib](https://github.com/hashiverse/hashiverse/actions/workflows/fuzz-hashiverse-lib.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/fuzz-hashiverse-lib.yml)
- NB: This is very lightweight fuzzing - mainly to catch regressions.  To do extensive fuzzing, run `./run_extensive_fuzzing.sh` on a fairly powerful box.

## Get started

- Install [Rust via rustup](https://rustup.rs/) (the nightly toolchain specified in `rust-toolchain.toml` will be selected automatically)
- Build the workspace with `cargo build`
- Run the test harness with `cargo run -p hashiverse-integration-tests --bin test-harness`
- Run the server with `cargo run -p hashiverse-server`
- Run tests for `cargo nextest run -p hashiverse-lib`
- Run tests for `cargo nextest run -p hashiverse-server-lib`
- Run integration tests with `cargo nextest run --cargo-profile profiling -p hashiverse-integration-tests` (the `profiling` profile gives release-level optimisations so the accelerated-clock tests don't bottleneck)
- Run WASM compatibilty tests with `cargo nextest run --target wasm32-wasip1 -p hashiverse-lib`
- Lint with `cargo clippy`
