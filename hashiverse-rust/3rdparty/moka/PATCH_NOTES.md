# Vendored moka — patch notes

This directory is a **temporary vendored snapshot** of a forked `moka`, consumed
by the workspace via `[patch.crates-io]` (see `hashiverse-rust/Cargo.toml`). It
exists only until the upstream PR is released; then it should be deleted.

## Why

moka's cache TTL/TTI/expiry are measured against an internal wall clock
(`std::time::Instant`) that isn't injectable. Hashiverse's integration tests run
on a scaled `TimeProvider` (e.g. 900×), so wall-clock eviction fires on a
different schedule than the simulated clock — breaking time-dependent cache
tests. The fork adds an `ExternalClock` seam so moka's native TTL/TTI/expiry can
be driven by our `TimeProvider` instead.

## Source / provenance

- Fork: <https://github.com/jimmejardine/moka> (PR open against `moka-rs/moka`)
- Vendored at fork commit: `608b089` ("Both sync and futures builders can use ExternalClock")
- Base: upstream moka `0.12.15` (`7006d8c`)

## Divergence from upstream (the only functional change)

Two fork commits add the `ExternalClock` seam (`9ede6e7`, `608b089`):

- `src/common/time/clock.rs`: `pub trait ExternalClock { fn elapsed_since_origin(&self) -> Duration; }`,
  a `ClockType::External { std_origin, source }` variant, a `pub fn Clock::external(source)`
  constructor, and one arm each in `now()` / `fast_now()` / `to_std_instant()`.
- `src/common/time.rs` + `src/lib.rs`: re-export the trait as `moka::ExternalClock`.
- `src/sync/builder.rs` + `src/future/builder.rs`: `pub fn external_clock(self, source: Arc<dyn ExternalClock>) -> Self`.

No changes to `base_cache` / `housekeeper` / `timer_wheel`.

## Local vendoring trims (NOT upstream changes)

Only `src/`, `README.md`, `Cargo.toml`, and `Cargo.lock` were copied. To keep the
manifest valid against the missing dirs, the vendored `Cargo.toml` drops, vs. the
fork: `[dev-dependencies]`, the two `cfg`-target dev-dependency blocks, and all
`[[example]]` entries. These are irrelevant when the crate is consumed only as a
library. (The fork's own `examples/external_time.rs` demonstrating the feature
lives in the fork repo, not here.)

## How it's wired

`hashiverse-rust/Cargo.toml`:

```toml
[workspace]
exclude = ["3rdparty/moka"]   # don't treat the vendored crate as a member

[patch.crates-io]
moka = { path = "3rdparty/moka" }
```

## Removal recipe (when upstream ships `ExternalClock`)

1. Delete `hashiverse-rust/3rdparty/moka/`.
2. Remove the `[patch.crates-io]` entry and the `exclude = ["3rdparty/moka"]` line.
3. Bump the `moka` version constraint to the release containing `ExternalClock`.
4. `cargo update -p moka && cargo check`.
