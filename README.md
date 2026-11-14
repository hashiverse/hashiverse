<img align="right" src="www/logo.png" width="150">

# Hashiverse — your open-source decentralized X/Twitter replacement

Welcome to the Hashiverse!

Remember when social media was still cool? No spam? No ads? No algorithms perverting your reality?

Hashiverse is owned by you, the Hashiverse community. It is completely open-source, and its servers are sponsored by thousands of active volunteers. Join us in taking back our Internet!

Live it right now at https://www.hashiverse.com

## Run a server, or use the command line

`hashiverse-server` and `hashiverse-cli` are published as signed apt/dnf packages (amd64 and arm64) for Debian 12+, Ubuntu 22.04+, RHEL/Rocky/Alma 9+ and Fedora. The install commands below write to system directories, so run them in a root shell (e.g. `sudo -i`) or prefix each one with `sudo`.

Debian / Ubuntu:

```bash
curl -fsSL https://www.hashiverse.com/packages/hashiverse-packages.asc -o /usr/share/keyrings/hashiverse-packages.asc
curl -fsSL https://www.hashiverse.com/packages/apt/hashiverse.sources -o /etc/apt/sources.list.d/hashiverse.sources
curl -fsSL https://www.hashiverse.com/packages/apt/hashiverse-mirrors.txt -o /etc/apt/hashiverse-mirrors.txt
apt-get update
apt-get install -y hashiverse-server hashiverse-cli
```

Fedora / RHEL / Rocky / Alma:

```bash
curl -fsSL https://www.hashiverse.com/packages/hashiverse-packages.asc -o /etc/pki/rpm-gpg/RPM-GPG-KEY-hashiverse
curl -fsSL https://www.hashiverse.com/packages/rpm/hashiverse.repo -o /etc/yum.repos.d/hashiverse.repo
dnf install -y hashiverse-server hashiverse-cli
```

Install either package or both.

- `hashiverse-server`:
  - It runs as the systemd service `hashiverse-server` and needs port 443 open.
  - It updates itself once a day; turn that off with `systemctl disable --now hashiverse-server-updater.timer`.
  - Settings are in `/etc/default/hashiverse-server`, and logs are at `journalctl -u hashiverse-server`.
  - The full walkthrough, including firewall setup, is at [www.hashiverse.com/en/lore/contribute](https://www.hashiverse.com/en/lore/contribute/).
- `hashiverse-cli`: see its [README](hashiverse-rust/hashiverse-cli/README.md).

Release candidates are published the same way from `https://www-prerelease.hashiverse.com/packages` (use it in place of `https://www.hashiverse.com/packages` above). A machine set up from there follows release candidates only.

## Repository layout

| Path | What it is |
|---|---|
| [`hashiverse-rust/`](hashiverse-rust) | Rust workspace: the protocol (`hashiverse-lib`), the server (`hashiverse-server-lib`, `hashiverse-server`), the command-line client (`hashiverse-cli`), and the Rust, WASM, Python and Node.js clients |
| [`hashiverse-client-web/`](hashiverse-client-web) | The web app at [app.hashiverse.com](https://app.hashiverse.com), in TypeScript/React over the WASM client |
| [`www/`](www) | The website at [www.hashiverse.com](https://www.hashiverse.com), in Astro; it also hosts the docs and the apt/dnf repository |
| [`packaging/`](packaging) | Linux packaging: nfpm configs, systemd units, the auto-updater, the signed-repository builder and a Docker end-to-end test |
| [`docker/`](docker) | Dockerfile for the `hashiverse-server` image |
| [`news-feeds/`](news-feeds) | Curated RSS feed list for seeding the network with news |

## Licence

Hashiverse is dual-licensed under either of

- [MIT license](LICENSE-MIT) ([`https://opensource.org/licenses/MIT`](https://opensource.org/licenses/MIT))
- [Apache License, Version 2.0](LICENSE-APACHE) ([`https://www.apache.org/licenses/LICENSE-2.0`](https://www.apache.org/licenses/LICENSE-2.0))

at your option. This is the same dual-licence arrangement used by the Rust project itself, and by the majority of the Rust ecosystem.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Hashiverse by you, as defined in the Apache-2.0 licence, shall be dual-licensed as above, without any additional terms or conditions.

## Builds

- [![build-app](https://github.com/hashiverse/hashiverse/actions/workflows/build-app.yml/badge.svg?event=release)](https://github.com/hashiverse/hashiverse/actions/workflows/build-app.yml)
- [![build-server](https://github.com/hashiverse/hashiverse/actions/workflows/build-server.yml/badge.svg?event=release)](https://github.com/hashiverse/hashiverse/actions/workflows/build-server.yml)
- [![build-www](https://github.com/hashiverse/hashiverse/actions/workflows/build-www.yml/badge.svg?event=release)](https://github.com/hashiverse/hashiverse/actions/workflows/build-www.yml) — the website, plus `hashiverse-server` and `hashiverse-cli` (amd64/arm64) as signed apt/dnf packages under `www.hashiverse.com/packages` (release candidates under `www-prerelease.hashiverse.com/packages`).

## Publishes

- [![publish-hashiverse-rust](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-rust.yml/badge.svg?event=release)](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-rust.yml) — `hashiverse-lib`, `hashiverse-server-lib`, `hashiverse-client-rust` to [crates.io](https://crates.io/users/hashiverse).
- [![publish-hashiverse-client-python](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-client-python.yml/badge.svg?event=release)](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-client-python.yml) — [`hashiverse-client`](https://pypi.org/project/hashiverse-client/) to PyPI.
- [![publish-hashiverse-client-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-client-wasm.yml/badge.svg?event=release)](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-client-wasm.yml) — [`@hashiverse/hashiverse-client-wasm`](https://www.npmjs.com/package/@hashiverse/hashiverse-client-wasm) to npm (browser/bundler).
- [![publish-hashiverse-client-nodejs](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-client-nodejs.yml/badge.svg?event=release)](https://github.com/hashiverse/hashiverse/actions/workflows/publish-hashiverse-client-nodejs.yml) — [`@hashiverse/hashiverse-client-nodejs`](https://www.npmjs.com/package/@hashiverse/hashiverse-client-nodejs) to npm (native Node).

## Translations

- [![check-translations-www](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-www.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-www.yml) — red means a developer should run the translation prompt: `node www/translations/check-translations.mjs` and feed the JSON output into a Claude Code session.
- [![check-translations-app](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-app.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-app.yml) — red means a developer should run the translation prompt: `node hashiverse-client-web/translations/check-translations.mjs` and feed the JSON output into a Claude Code session.

## Checks

- [![check-hashiverse-client-web](https://github.com/hashiverse/hashiverse/actions/workflows/check-hashiverse-client-web.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/check-hashiverse-client-web.yml) — biome lint, TypeScript type-check, and rsbuild production build for `hashiverse-client-web/`.
- [![check-www](https://github.com/hashiverse/hashiverse/actions/workflows/check-www.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/check-www.yml) — Astro production build for `www/`.

## Tests

- [![test-hashiverse-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-default.yml)
- [![test-hashiverse-server-lib-default](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-server-lib-default.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-server-lib-default.yml)
- [![test-hashiverse-lib-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-lib-wasm.yml)
- [![test-hashiverse-client-wasm](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-wasm.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-wasm.yml)
- [![test-hashiverse-client-python](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-python.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-python.yml)

## Mirrors

- [![mirror-hashiverse](https://github.com/hashiverse/hashiverse/actions/workflows/mirror-hashiverse.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/mirror-hashiverse.yml) — mirrors this repo to [`codeberg.org/hashiverse/hashiverse`](https://codeberg.org/hashiverse/hashiverse).
- [![mirror-app](https://github.com/hashiverse/hashiverse/actions/workflows/mirror-app.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/mirror-app.yml) — after `build-app` succeeds, mirrors [`github.com/hashiverse/app`](https://github.com/hashiverse/app) to `codeberg.org/hashiverse/app-eu` and `app-ch` for Codeberg Pages hosting at [app.hashiverse.eu](https://app.hashiverse.eu) and [app.hashiverse.ch](https://app.hashiverse.ch).
- [![mirror-www](https://github.com/hashiverse/hashiverse/actions/workflows/mirror-www.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/mirror-www.yml) — after `build-www` succeeds, mirrors [`github.com/hashiverse/www`](https://github.com/hashiverse/www) to `codeberg.org/hashiverse/www-eu` and `www-ch` for Codeberg Pages hosting at [www.hashiverse.eu](https://www.hashiverse.eu) and [www.hashiverse.ch](https://www.hashiverse.ch).
- [![mirror-app-v2](https://github.com/hashiverse/hashiverse/actions/workflows/mirror-app-v2.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/mirror-app-v2.yml) — after `build-app` succeeds, syncs [`github.com/hashiverse/app`](https://github.com/hashiverse/app) into the Bunny Edge Storage zone `hashiverse-app`, served from the EU and CH pull zones at [app.hashiverse.eu](https://app.hashiverse.eu) and [app.hashiverse.ch](https://app.hashiverse.ch).
- [![mirror-www-v2](https://github.com/hashiverse/hashiverse/actions/workflows/mirror-www-v2.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/mirror-www-v2.yml) — after `build-www` succeeds, syncs [`github.com/hashiverse/www`](https://github.com/hashiverse/www) into the Bunny Edge Storage zone `hashiverse-www`, served from the EU and CH pull zones at [www.hashiverse.eu](https://www.hashiverse.eu) and [www.hashiverse.ch](https://www.hashiverse.ch).
