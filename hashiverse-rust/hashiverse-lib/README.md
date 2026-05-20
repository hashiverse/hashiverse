## About

This library is common to the associated hashiverse-server and hashiverse-client-lib repos.

## Usage

[![test-hashiverse-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml) with `cargo nextest run -p hashiverse-lib`

[![test-hashiverse-lib-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml) with `cargo nextest run --target wasm32-wasip1 -p hashiverse-lib`

## Publish

[![publish-hashiverse-rust](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-rust.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-rust.yml) — published to [crates.io](https://crates.io/crates/hashiverse-lib) on release events, alongside `hashiverse-server-lib` and `hashiverse-client-rust`.

## Fuzzing

[![fuzz-hashiverse-lib](https://github.com/hashiverse/hashiverse/actions/workflows/fuzz-hashiverse-lib.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/fuzz-hashiverse-lib.yml) with `cargo bolero test`
