# hashiverse-cli

Command-line client for [Hashiverse](https://www.hashiverse.com) — your open-source decentralized X/Twitter replacement.

Read posts by user or hashtag, see trending hashtags, and post — either as one-shot commands for scripting, or from an interactive prompt.

It is a thin layer over [`hashiverse-client-rust`](../hashiverse-client-rust), so it uses the same defaults: sqlite client storage, an on-disk encrypted key locker, DNSSEC-validated bootstrap, HTTPS transport and native parallel proof-of-work.

## Getting help

```bash
hashiverse-cli --help            # global options and the list of commands
hashiverse-cli <command> --help  # options for a single command
```

At the interactive prompt, type `help` to list the available commands.

## Building

From `hashiverse-rust/` (nightly Rust, as pinned by `rust-toolchain.toml`):

```bash
cargo build --release -p hashiverse-cli
./target/release/hashiverse-cli --help
```

## Identity: guest or logged in

Hashiverse works without logging in. Reading posts and trending hashtags needs no identity; only posting does.

The CLI takes identity from environment variables only. It never prompts for secrets and has no `--keyphrase` / `--password` flags, because argv is visible in `ps` and shell history.

| Environment                                       | Result                                                                                                               |
|---------------------------------------------------|----------------------------------------------------------------------------------------------------------------------|
| neither `HASHIVERSE_KEYPHRASE` nor `HASHIVERSE_PROFILE` | **Guest**: read-only. `post` is refused.                                                                        |
| `HASHIVERSE_KEYPHRASE` + `HASHIVERSE_PASSWORD`    | Derives your key from the keyphrase and saves it in the data dir, encrypted with the password. Prints your client id. |
| `HASHIVERSE_PROFILE` + `HASHIVERSE_PASSWORD`      | Unlocks a key previously saved in the data dir. The profile is the client id (hex).                                  |
| both `HASHIVERSE_KEYPHRASE` and `HASHIVERSE_PROFILE` | Error: set only one.                                                                                             |
| keyphrase or profile without `HASHIVERSE_PASSWORD` | Error.                                                                                                              |

`HASHIVERSE_PASSWORD` may be empty (`HASHIVERSE_PASSWORD=""`), but the variable must exist. An empty password leaves the saved key file effectively unprotected. Note that on Windows `cmd`, `set HASHIVERSE_PASSWORD=` deletes the variable rather than setting it to empty.

The typical flow is to use the keyphrase once, then the profile from then on:

```bash
HASHIVERSE_KEYPHRASE="correct horse battery staple" HASHIVERSE_PASSWORD="s3cret" hashiverse-cli trending
# unlocked as 04bfbfac...e6a9

HASHIVERSE_PROFILE=04bfbfac...e6a9 HASHIVERSE_PASSWORD="s3cret" hashiverse-cli post "hello #hashiverse"
```

## Commands

| Command                                  | What it does                                                                 |
|------------------------------------------|------------------------------------------------------------------------------|
| `post <text...>`                         | Submits a plain-text post. Needs an identity. Quoting is optional; words are joined with spaces. `#hashtag` and `@<64-hex client id>` mentions are recognised. |
| `user <client_id_hex> [--pages N]`       | Lists a user's posts, fetching N batches (default 1).                        |
| `hashtag <tag> [--pages N]`              | Lists posts with a hashtag; the leading `#` is optional.                     |
| `trending [--limit N]`                   | Shows trending hashtags (default 20).                                        |

Global options, accepted before or after the command:

| Option                      | Environment            | Meaning                                                   |
|-----------------------------|------------------------|-----------------------------------------------------------|
| `--data-dir <dir>`          | `HASHIVERSE_DATA_DIR`  | Where keys and client storage live. Defaults to the platform data dir + `hashiverse`. |
| `--profile <client_id_hex>` | `HASHIVERSE_PROFILE`   | Stored key to unlock (see above).                         |
| `--bootstrap <host:port,...>` |                      | Override the bootstrap peers.                             |
| `--json`                    |                        | Machine-readable JSON output instead of text.             |
| `--log-level <level>`       |                        | `error`, `warn` (default), `info`, `debug`, `trace`.      |

### Output

Text output prints each post as a header line starting with `POST>>` (then UTC time, author client id, post id) followed by the full post body rendered from Hashiverse's HTML to plain text: hashtags as `#Tag`, mentions as `@<client id>`, line breaks preserved, links as `text <url>`, images as `[image: url]`.

`--json` prints the raw data instead, including the original `post_html` and `bucket_location` of each post. In JSON mode stdout carries only the JSON: the identity line is suppressed, and logs and errors always go to stderr.

```bash
hashiverse-cli hashtag rust --pages 2
hashiverse-cli trending --limit 5 --json
```

## Interactive prompt

Run `hashiverse-cli` with no command to get a prompt. The identity is resolved once, as above, and the connection to the network is reused between commands.

- Any of the commands above can be typed at the prompt, e.g. `hashtag rust`.
- Repeating `user X` or `hashtag Y` fetches progressively older posts — timelines can always go further back.
- `login <client_id_hex>` switches to another stored profile, using the `HASHIVERSE_PASSWORD` from the environment. If it fails, the current identity is kept.
- `help` lists the commands; `quit`, `exit` or end-of-input leaves.

## Layout

| File               | Contents                                                              |
|--------------------|-----------------------------------------------------------------------|
| `src/main.rs`      | Entry point: parse args, resolve identity, run the command.           |
| `src/cli_args.rs`  | `clap` definitions for global options and commands.                  |
| `src/unlock.rs`    | Identity resolution from the environment, and building the client.   |
| `src/commands.rs`  | The commands, each taking a plain `&HashiverseClient`.                |
| `src/render.rs`    | Post-HTML → text rendering, and text/JSON formatting.                 |
| `src/repl.rs`      | Interactive prompt: line splitting, `help`/`login`/`quit`, the loop. |

The commands take a `&HashiverseClient` rather than building their own, so the same code runs against the real network and against in-memory test clients.

## Tests

```bash
# Unit tests: identity resolution, argument parsing, rendering, the prompt loop
cargo nextest run --cargo-profile profiling -p hashiverse-cli

# End-to-end against an in-memory network of servers (guest reads, guest post refused, author posts)
cargo nextest run --cargo-profile profiling -p hashiverse-integration-tests test_cli_commands
```

## License

MIT OR Apache-2.0
