<img align="right" src="www/logo.png" width="150">

# Hashiverse monorepo

Welcome to the Hashiverse!

Remember when social media was still cool? No spam? No ads? No algorithms perverting your reality?

Hashiverse is owned by you, the Hashiverse community. It is completely open-source, and its servers are sponsored by thousands of active volunteers. Join us in taking back our Internet!

Live it right now at https://www.hashiverse.com


## Builds

- [![build-app](https://github.com/hashiverse/hashiverse/actions/workflows/build-app.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/build-app.yml)
- [![build-www](https://github.com/hashiverse/hashiverse/actions/workflows/build-www.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/build-www.yml)
- [![build-server](https://github.com/hashiverse/hashiverse/actions/workflows/build-server.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/build-server.yml)
- [![publish-hashiverse-client-python](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-client-python.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-client-python.yml)

## Translations

- [![check-translations-www](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-www.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-www.yml) — red means a developer should run the translation prompt: `node www/translations/check-translations.mjs` and feed the JSON output into a Claude Code session.
- [![check-translations-app](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-app.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-app.yml) — red means a developer should run the translation prompt: `node hashiverse-client-web/translations/check-translations.mjs` and feed the JSON output into a Claude Code session.

## Tests

- [![test-hashiverse-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml)
- [![test-hashiverse-server-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-server-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-server-lib-default.yml)
- [![test-hashiverse-lib-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml)
- [![test-hashiverse-client-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-wasm.yml)
- [![test-hashiverse-client-python](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-python.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-python.yml)

## Licence

Hashiverse is dual-licensed under either of

- [MIT license](LICENSE-MIT) ([`https://opensource.org/licenses/MIT`](https://opensource.org/licenses/MIT))
- [Apache License, Version 2.0](LICENSE-APACHE) ([`https://www.apache.org/licenses/LICENSE-2.0`](https://www.apache.org/licenses/LICENSE-2.0))

at your option. This is the same dual-licence arrangement used by the Rust project itself, and by the majority of the Rust ecosystem.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Hashiverse by you, as defined in the Apache-2.0 licence, shall be dual-licensed as above, without any additional terms or conditions.
