## About

This library is common to the associated hashiverse-server and hashiverse-client-lib repos.

## Usage

[![test-hashiverse-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml) with `cargo nextest run -p hashiverse-lib`

[![test-hashiverse-lib-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml) with `cargo nextest run --target wasm32-wasip1 -p hashiverse-lib`

## Fuzzing

[![fuzz-hashiverse-lib](https://github.com/hashiverse/hashiverse/actions/workflows/fuzz-hashiverse-lib.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/fuzz-hashiverse-lib.yml) with `cargo bolero test`
