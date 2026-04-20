# hashiverse-integration-tests

End-to-end integration tests for the Hashiverse protocol. Also includes the test harness binary that spins up a local cluster of servers and clients for interactive development.

## Get started

- Build with `cargo build -p hashiverse-integration-tests`
- Run the test harness with `cargo run -p hashiverse-integration-tests --bin test-harness`
- Run integration tests with `cargo nextest run --cargo-profile profiling -p hashiverse-integration-tests` (the `profiling` profile gives release-level optimisations so the accelerated-clock tests don't bottleneck)
- Run a profiler with e.g. `samply record cargo nextest run --cargo-profile profiling -p hashiverse-integration-tests test_server_meets_server_thousands`