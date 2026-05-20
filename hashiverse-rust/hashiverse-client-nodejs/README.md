# @hashiverse/hashiverse-client-nodejs

Native Node.js binding for the Hashiverse client — your open-source decentralized X/Twitter replacement.

This is the Node.js sibling of the [`hashiverse-client`](https://pypi.org/project/hashiverse-client/) PyPI wheel: it binds `hashiverse-lib` directly via [NAPI-RS](https://napi.rs/), producing prebuilt platform-native `.node` binaries — no in-process WASM, no browser APIs. For browser/bundler usage, see [`@hashiverse/hashiverse-client-wasm`](https://www.npmjs.com/package/@hashiverse/hashiverse-client-wasm) instead.

## Install

```
npm install @hashiverse/hashiverse-client-nodejs
```

Prebuilt binaries are published for Linux x64/arm64, macOS x64/arm64, and Windows x64.

## Usage

```js
import {
  HashiverseClient,
  initLogging,
  convertTextToHashiverseHtml,
} from "@hashiverse/hashiverse-client-nodejs";

initLogging();

const client = await HashiverseClient.createFromKeyphrase({
  keyPhrase: "your secret keyphrase",
  dataDir: "/var/lib/hashiverse-bot",
});

console.log("client id:", client.clientId);

await client.submitPost(convertTextToHashiverseHtml("hello hashiverse"));
```

## API

Every I/O method returns a `Promise<T>`. Properties (`clientId`, `loggedIn`) are synchronous getters.

The API mirrors the Python wheel's `HashiverseClient` (~30 methods covering posting, timelines, follows, bios, feedback, key management, and URL/trending previews) — see the [Python crate](https://github.com/hashiverse/hashiverse/tree/main/hashiverse-rust/hashiverse-client-python) for parity-level documentation of each method.

## Developing this package

- `npm ci` — install dev deps (the `@napi-rs/cli` and vitest)
- `npm run build:debug` — build the native binding for the host platform (debug)
- `npm run build` — release build
- `npm test` — run the offline test suite via vitest
- `cargo nextest run -p hashiverse-client-nodejs` — run any Rust-side unit tests directly

The `napi build` step produces `index.js`, `index.d.ts`, and a `*.node` artifact; only the first two are published with the root package — the platform-native binary is delivered via the optional per-platform sibling packages.
