## About

Shared server-side library used by the `hashiverse-server` binary. Holds the real server implementation (Kademlia, transport, environment, DDoS protection, handlers) so the binary stays a thin wrapper.

## Usage

[![test-hashiverse-server-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-server-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-server-lib-default.yml) with `cargo nextest run -p hashiverse-server-lib`

## Publish

[![publish-hashiverse-rust](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-rust.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-rust.yml) — published to [crates.io](https://crates.io/crates/hashiverse-server-lib) on release events, alongside `hashiverse-lib` and `hashiverse-client-rust`.
