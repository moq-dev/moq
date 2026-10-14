[![Documentation](https://docs.rs/moq-clock/badge.svg)](https://docs.rs/moq-clock/)
[![Crates.io](https://img.shields.io/crates/v/moq-clock.svg)](https://crates.io/crates/moq-clock)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-clock

One clock for [Media over QUIC](https://moq.dev): logical instants, wall time,
deadlines, and a deterministic simulator for tests.

```bash
cargo add moq-clock
cargo add --dev moq-clock --features sim
```

```rust
#[moq_clock::test]
async fn retries_after_a_second() {
    let start = moq_clock::now(); // Instant::ZERO
    moq_clock::sleep(std::time::Duration::from_secs(1)).await; // returns at once
    assert_eq!(moq_clock::now() - start, std::time::Duration::from_secs(1));
}
```

Versions up to 0.10 were the "CLOCK over QUIC" demo binary, now the
[clock example](https://github.com/moq-dev/moq/blob/main/rs/moq-tokio/examples/clock.rs).

See [docs.rs/moq-clock](https://docs.rs/moq-clock) for the API and
[doc.moq.dev](https://doc.moq.dev/lib/rs/moq-clock) for an overview.
