# hashiverse-client

Python client for Hashiverse — your open-source decentralized X/Twitter replacement. Wraps the Rust `hashiverse-lib` via PyO3 for native performance.

[![test-hashiverse-client-python](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-python.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-python.yml)

## Install

```bash
pip install hashiverse-client
```

Requires Python 3.9+. Pre-built wheels are published for Linux, macOS, and Windows.

## Quick start

```python
from hashiverse_client import HashiverseClient

client = HashiverseClient.create_from_keyphrase(
    key_phrase="my secret keyphrase",
    data_dir="~/.hashiverse",
    passphrase="my secret passphrase",  # encrypts key files at rest (default: "" = no encryption)
)

print(client.client_id)
client.post("Hello from Python! #hashiverse-python")

timeline = client.get_hashtag_timeline("hashiverse-python")
for post in timeline.posts:
    print(f"{post.client_id[:8]}: {post.post}")
```

## What you can do

- Create or reload a cryptographic identity from a key phrase, with optional at-rest encryption.
- Post messages to the network.
- Read hashtag timelines.
- Fetch trending hashtags.
- Persist keys to a local data directory and reconnect to the same identity later.

See `examples/hello_hashiverse.py` for an end-to-end walkthrough covering identity creation, key reload, timeline reads, and trending hashtags.

## Reloading a stored identity

After `create_from_keyphrase` has run once, the key is persisted in `data_dir`. Subsequent runs can skip the keyphrase and reconnect via the stored key:

```python
client = HashiverseClient.create_from_stored_key(
    client_id_hex=client_id,
    data_dir="~/.hashiverse",
    passphrase="my secret passphrase",  # must match the passphrase used to create the key
)
```

## Connecting to a local server

By default the client uses the public bootstrap addresses. To point at a local cluster (handy for testing), pass `bootstrap_addresses`:

```python
client = HashiverseClient.create_from_keyphrase(
    key_phrase="my example keyphrase",
    data_dir=data_dir,
    passphrase="my secret passphrase",
    bootstrap_addresses=["127.0.0.1:443"],
)
```

A local cluster can be started with the test harness from this repository — see the "Running the example" section below.

---

## Developing this package

The rest of this document describes how to build, test, and release `hashiverse-client` itself. End-user usage is covered above.

### Prerequisites

- Python 3.9+
- Rust nightly toolchain (see `rust-toolchain.toml` in the workspace root)

### Development build

```bash
cd hashiverse-rust/hashiverse-client-python

# Create and activate a virtual environment
python -m venv .venv

# Windows
.venv\Scripts\activate

# Linux/macOS
source .venv/bin/activate

# Install maturin and build the extension into the venv
pip install maturin
maturin develop
```

`maturin develop` compiles the Rust code in debug mode and installs the resulting Python package into the active venv. Re-run it after any Rust code changes.

For a faster extension (recommended if exercising PoW or network operations):

```bash
maturin develop --release
```

### Production build

To build a distributable wheel:

```bash
maturin build --release
```

The wheel is written to `target/wheels/` and can be installed with:

```bash
pip install target/wheels/hashiverse_client-*.whl
```

### Running tests

The test suite exercises offline client operations (no server required): identity creation, key management, and storage operations.

```bash
cd hashiverse-rust/hashiverse-client-python
source .venv/bin/activate  # or .venv\Scripts\activate on Windows
pip install pytest
maturin develop --release
python -m pytest tests/ -v
```

### Running the example

The example connects to a local hashiverse server, creates an identity, fetches trending hashtags, and reads the `#hashiverse` timeline.

1. Start a local server cluster using the test harness (in a separate terminal from the workspace root):

```bash
cd hashiverse-rust
cargo run -p hashiverse-integration-tests --bin test_harness
```

2. Run the example:

```bash
cd hashiverse-rust/hashiverse-client-python
python examples/hello_hashiverse.py
```
