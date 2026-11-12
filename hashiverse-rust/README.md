# Hashiverse rust server and libs

This is Hashiverse.

## Crates

| Crate | Role |
|---|---|
| [`hashiverse-lib`](hashiverse-lib) | Core protocol: transport traits, RPC and PoW, posts and bundles, the client API, crypto |
| [`hashiverse-server-lib`](hashiverse-server-lib) | Server logic: HTTPS/TCP transports, Kademlia, storage, DDoS protection |
| [`hashiverse-server`](hashiverse-server) | The server binary (Docker image and apt/dnf package) |
| [`hashiverse-client-rust`](hashiverse-client-rust) | Native Rust client with production defaults |
| [`hashiverse-cli`](hashiverse-cli) | Command-line client (apt/dnf package) |
| [`hashiverse-client-wasm`](hashiverse-client-wasm) | Browser client (npm) |
| [`hashiverse-client-python`](hashiverse-client-python) | Python client (PyPI) |
| [`hashiverse-client-nodejs`](hashiverse-client-nodejs) | Node.js client (npm) |
| [`hashiverse-integration-tests`](hashiverse-integration-tests) | End-to-end tests and the test harness |

## Builds

- [![build-server](https://github.com/hashiverse/hashiverse/actions/workflows/build-server.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/build-server.yml)
- [![build-www](https://github.com/hashiverse/hashiverse/actions/workflows/build-www.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/build-www.yml) — includes the `hashiverse-server` and `hashiverse-cli` apt/dnf packages (see [`packaging/`](../packaging))
- [![publish-hashiverse-client-python](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-client-python.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-client-python.yml)

## Tests

- [![test-hashiverse-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml)
- [![test-hashiverse-server-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-server-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-server-lib-default.yml)
- [![test-hashiverse-lib-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml)
- [![test-hashiverse-client-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-wasm.yml)
- [![test-hashiverse-client-python](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-python.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-python.yml)

## Fuzzing

- [![fuzz-hashiverse-lib](https://github.com/hashiverse/hashiverse/actions/workflows/fuzz-hashiverse-lib.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/fuzz-hashiverse-lib.yml)
- NB: This is very lightweight fuzzing - mainly to catch regressions.  To do extensive fuzzing, run `./run_extensive_fuzzing.sh` on a fairly powerful box.

## Get started

- Install [Rust via rustup](https://rustup.rs/) (the nightly toolchain specified in `rust-toolchain.toml` will be selected automatically)
- Build the workspace with `cargo build`
- Run the test harness with `cargo run -p hashiverse-integration-tests --bin test-harness`
- Run the server with `cargo run -p hashiverse-server`
- Run the command-line client with `cargo run -p hashiverse-cli -- --help`
- Run tests for `cargo nextest run -p hashiverse-lib`
- Run tests for `cargo nextest run -p hashiverse-server-lib`
- Run integration tests with `cargo nextest run --cargo-profile profiling -p hashiverse-integration-tests` (the `profiling` profile gives release-level optimisations so the accelerated-clock tests don't bottleneck)
- Run WASM compatibilty tests with `cargo nextest run --target wasm32-wasip1 -p hashiverse-lib`
- Lint with `cargo clippy`
