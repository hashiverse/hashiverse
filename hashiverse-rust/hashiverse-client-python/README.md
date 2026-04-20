# hashiverse-client-python

Python client for the Hashiverse decentralized social network. Wraps the Rust `hashiverse-lib` via PyO3 for native performance.

## Usage

[![test-hashiverse-client-python](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-python.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/test-hashiverse-client-python.yml) with `maturin develop --release` + `pytest`

## Prerequisites

- Python 3.9+
- Rust nightly toolchain (see `rust-toolchain.toml` in workspace root)

## Development build

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

For a faster extension (recommended if testing PoW or network operations):

```bash
maturin develop --release
```

## Production build

To build a distributable wheel:

```bash
maturin build --release
```

The wheel is written to `target/wheels/` and can be installed with:

```bash
pip install target/wheels/hashiverse_client-*.whl
```

## Running the example

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

## Running tests

The test suite exercises offline client operations (no server required): identity creation, key management, and storage operations.

```bash
cd hashiverse-rust/hashiverse-client-python
source .venv/bin/activate  # or .venv\Scripts\activate on Windows
pip install pytest
maturin develop --release
python -m pytest tests/ -v
```

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
