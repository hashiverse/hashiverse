## Overview

- This is a decentralized P2P open source social network protocol, replacing the likes of Twitter, Bluesky and Mastodon.  
- Think a cryptographically secure, distributed posting system with Kademlia DHT for peer discovery.

### Key Design Patterns

- Completely decentralised
  - no central servers for things like account management, global counts, sending emails, etc.
  - no reliance on the goodwill of a single government or cloud provider or dns provider or repository hoster.
- Pluggable traits throughout
  - `TransportFactory`, `EnvironmentFactory`, `TimeProvider`, `ClientStorage`, `KeyLockerManager`
  - Swap implementations for testing vs production without changing protocol logic.
- Proof-of-Work (PoW), embedded in:
  - the RPC layer (`protocol/rpc.rs`), 
  - server identity (`tools/server_id.rs`), 
  - reporting feedback
- Cryptography stack
  - Signatures: Ed25519 (ed25519-dalek), with post-quantum ML-DSA and FN-DSA support
  - Hashing: Blake3 for general hashing, but then for PoW-hardness use chained Blake2, Blake3, SHA2, SHA3, Whirlpool, Groestl, Skein
  - Encryption: ChaCha20Poly1305
  - all hash libraries are built with `opt-level=3` even in debug mode (see workspace `Cargo.toml` profile overrides)

## Top level directives

- Be direct and not obseqious.  I don't need flattery.
- Prefer long variables names like are already in the codebase - generally prefer `encoded_post_bundle_feedback: EncodedPostBundleFeedbackV1` and `let bytes_gatherer: BytesGatherer = xxx` over `let g: BytesGatherer = xxx` 
- In git, omit "Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>" from commit messages
- Every suggested addition or refactor should be able to be tested using tests - write them if they are missing.  For a refactor, lets write any missing tests and test them before doing the refactor.
- All strings in both rust and typescript should be " delimited, not '

## Repository Layout

```
hashiverse/
├── hashiverse-rust/       # Rust workspace (server, protocol, WASM client)
├── hashiverse-client-web/ # TypeScript/React web client
├── hashiverse-rust/hashiverse-client-python/ # Python API wrapping hashiverse-client
├── www/                   # Static landing page assets
└── doc/                   # Documentation
```

## Build & Test Commands

### Rust (run from `hashiverse-rust/`)

The workspace `default-members` are the host-buildable crates (`hashiverse-lib`, `hashiverse-server-lib`, `hashiverse-server`, `hashiverse-client-rust`). The extension-only crates (`hashiverse-client-wasm`, `hashiverse-client-python`, `hashiverse-client-nodejs`) and the slow `hashiverse-integration-tests` are workspace members but excluded from the default selection. So bare `cargo` invocations (no `-p`, no `--workspace`) are safe and fast — they won't try to compile wasm-bindgen / pyo3-extension-module / napi against the host target.

```bash
# Fast: does everything compile? (default-members only, no tests run)
cargo check

# Default test run — host-buildable crates only
cargo nextest run --cargo-profile profiling

# Run tests for a specific crate
cargo nextest run --cargo-profile profiling -p hashiverse-lib
cargo nextest run --cargo-profile profiling -p hashiverse-server

# Slow integration tests — opt in explicitly; only worth running for major refactors
# Slow integration tests — opt in explicitly; only worth running for major refactors
cargo nextest run --cargo-profile profiling -p hashiverse-integration-tests

# Run a single test by name
cargo nextest run --cargo-profile profiling -p hashiverse-lib <test_name>

# WASM lib tests (wasmtime runner is configured in .cargo/config.toml)
cargo nextest run --target wasm32-wasip1 -p hashiverse-lib

# WASM client tests in headless Chrome (requires wasm-pack)
wasm-pack test --chrome --headless hashiverse-client-wasm --lib

# Python / Node extension crates compile only via their own build pipelines —
# not through bare cargo. See hashiverse-client-python/ and hashiverse-client-nodejs/
# READMEs for maturin / napi-cli usage.

# Build the default-members
cargo build

# Build absolutely everything in the workspace, including the extension crates
# (will fail on host unless you also pass --target wasm32-... for the wasm crate;
# only useful for sanity-checking workspace metadata, not for normal dev).
cargo build --workspace

# Run a simulation binary (starts 1 primary + 5 secondary servers + 10 clients)
cargo run -p hashiverse-integration-tests
```

**Toolchain**: Rust nightly (required — specified in `rust-toolchain.toml`).

**Lint**: No explicit lint commands configured; `cargo clippy` is standard.

**Formatting**: `rustfmt.toml` sets max line width to 250, tab spaces to 4.

### TypeScript Web Client (run from `hashiverse-client-web/`)

```bash
npm install          # Install dependencies
npm run dev          # Start dev server
npm run build        # Type-check + production build (rsbuild)
npm run preview      # Preview production build locally
npm run check        # Biome lint+format (writes fixes)
npm run format       # Biome format only (writes fixes)
```
**Stack**: React 19, TypeScript 5, Mantine 8 UI, Tiptap rich-text editor, React Router 7, rsbuild bundler, Biome linter/formatter.

### hashiverse-rust — Workspace Crates

| Crate | Role |
|---|---|
| `hashiverse-lib` | Core protocol logic shared by all |
| `hashiverse-server` | Server binary and server-specific logic |
| `hashiverse-client-wasm` | Browser WASM client wrapper |
| `hashiverse-client-python` | Python client wrapper |
| `hashiverse-integration-tests` | End-to-end tests combining all crates |

### hashiverse-lib (Core)

Four top-level modules in `src/lib.rs`:

- **`transport/`** — Abstract network traits (`TransportFactory`, `TransportServer`, `TransportServerHandler`) with two implementations: `mem_transport` (in-memory, for tests) and concrete HTTP/TCP implementations in `hashiverse-server`.
- **`protocol/`** — RPC packet encoding/decoding (with PoW and compression), peer structs, request/response payload types, and post/bundle data structures.
- **`client/`** — `HashiverseClient` API, peer tracker, post bundle management, timeline with recursive bucket traversal, and pluggable `ClientStorage`/`KeyLockerManager` traits.
- **`tools/`** — Core types (`Hash`, `Id`, `Signature`), cryptographic primitives (hashing, signing, encryption, compression), proof-of-work, `TimeProvider` abstraction, and protocol config.

### hashiverse-server

The standalone production hashiverse server.

### test-harness

A "test harness" that fires up a number of servers and clients so that the developer can interact with a "sizable" hashiverse instance.

### hashiverse-client-wasm

Thin WASM wrapper exposing `HashiverseClientWasm` to JavaScript:
- `wasm_transport.rs` — HTTP transport via `gloo-net`
- `wasm_client_storage.rs` — IndexedDB persistence
- `wasm_key_locker.rs` — In-browser key management

### hashiverse-client-web (TypeScript)

React SPA that consumes the WASM client. Key source layout under `src/`:

- **`Hashiverse.ts`** — Top-level client wrapper / singleton (exposing proxy to WASM in Web Worker)
- **`HashiverseWorker.ts`** — Web Worker wrapper of hashiverse-client for off-thread WASM calls
- **`tabs/`** — Page-level components: `home`, `timeline`, `compose`, `login`, `people`, `hashtags`, `config`
- **`tools/`** — Shared utilities (`Tools.ts`, `PostPurifier.ts`, `NeedsLoggedIn.tsx`, `DeferredPromise.ts`)

### hashiverse-client-python

A simple Python wrapper around hashiverse_client.
