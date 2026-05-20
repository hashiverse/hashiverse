## About

This library provides the rust/wasm backend for the associated hashiverse-client-web repo.

## Install

```
npm install @hashiverse/hashiverse-client-wasm
```

## Usage (bundler — Vite, rsbuild, webpack)

```js
import { wasm_init, HashiverseClientWasm } from "@hashiverse/hashiverse-client-wasm";

wasm_init(true);
// see hashiverse-client-web for a full integration example
```

A future `@hashiverse/hashiverse-client-nodejs` package will provide a native-Node binding via NAPI-RS (parallel to the PyPI wheel). Use this WASM build only in browser/bundler contexts.

## Tests

[![test-hashiverse-client-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-wasm.yml) — run with `wasm-pack test --headless --chrome --lib`.

## Get started (contributors)

- Build for development with `wasm-pack build --dev`
- Build for release with `wasm-pack build --release`
- Run tests with `wasm-pack test --chrome --headless --lib`

