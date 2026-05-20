# hashiverse-client-rust

Friendly Rust client wrapper for [Hashiverse](https://www.hashiverse.com) — your open-source decentralized X/Twitter replacement.

Hashiverse is a protocol, not a product. Like email or the web itself, no one can own it, sell it, or shut it down. Your identity is a cryptographic key pair, generated on your device, never held by anyone else. Your posts are signed with that key and propagate across a distributed network. No company can ban you, no acquisition can erase your history, no algorithm can decide you're not worth showing.

This crate picks sensible defaults for every pluggable trait in [`hashiverse-lib`](https://crates.io/crates/hashiverse-lib):

| Trait | Default |
| --- | --- |
| `ClientStorage` | sqlite, under `<data_dir>/client_storage/` |
| `KeyLockerManager` | disk-backed, under `<data_dir>/` |
| `BootstrapProvider` | DNSSEC-validated public seed list |
| `TransportFactory` | HTTPS with rustls + ring |
| `PowGenerator` | native parallel (rayon) |
| `TimeProvider` | real-time |

If you need to swap any of these out, build a `HashiverseClient` directly against `hashiverse-lib`. This crate is opinionated by design.

## Quick start

```rust
use hashiverse_client_rust::HashiverseBuilder;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let hashiverse = HashiverseBuilder::new()
        .data_dir("~/.hashiverse")
        .build_with_keyphrase("correct horse battery staple")
        .await?;

    hashiverse.submit_post("<p>hello, hashiverse</p>").await?;
    Ok(())
}
```

On a subsequent run, load the same identity from the on-disk key locker:

```rust
let hashiverse = HashiverseBuilder::new()
    .data_dir("~/.hashiverse")
    .build_from_stored_key(client_id_hex)
    .await?;
```

## Nightly Rust required

`hashiverse-lib` uses `#![feature(try_blocks)]` and so requires a nightly toolchain. Pin one in your project with a `rust-toolchain.toml`:

```toml
[toolchain]
channel = "nightly"
```

## License

MIT OR Apache-2.0
