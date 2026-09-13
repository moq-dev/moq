[![Documentation](https://docs.rs/moq-e2ee/badge.svg)](https://docs.rs/moq-e2ee/)
[![Crates.io](https://img.shields.io/crates/v/moq-e2ee.svg)](https://crates.io/crates/moq-e2ee)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-e2ee

End-to-end encryption for [Media over QUIC](https://moq.dev) groups, datagrams,
catalogs, and opaque track names. Profile `moq-e2ee-01` from
[draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/).

Relays forward ciphertext. Content keys never enter `moq-net`. The TypeScript twin is `@moq/e2ee`.

```bash
cargo add moq-e2ee
```

## Processing

AES-128-GCM is inline and synchronous. `cargo bench -p moq-e2ee` on this tree measured
about 1.3 µs per 1 KiB grouped frame (~770 MiB/s) and about 440 ns per 160-byte
Opus-sized datagram. A 20 ms Opus cadence is 50 datagrams/s, so encryption is a
tiny fraction of one core. There is no async pump.

Memory is bounded without changing the wire profile:

- grouped duplicate window: the current group's frame indices plus the previous group
- datagram duplicate window: 1024 sequences
- producer ciphertext retention: the same windows, for retransmission without re-encryption
