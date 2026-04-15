<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/moq-boy/badge.svg)](https://docs.rs/moq-boy/)
[![Crates.io](https://img.shields.io/crates/v/moq-boy.svg)](https://crates.io/crates/moq-boy)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-boy

A crowd-controlled Game Boy Color emulator that streams video and audio over [Media over QUIC](https://moq.dev).

Viewers connect via the [@moq/boy](https://github.com/moq-dev/moq/tree/main/js/moq-boy) web client, watch the stream with sub-second latency, and collectively send button inputs back to the emulator. The emulator auto-pauses when nobody is watching.

See the [demo](https://github.com/moq-dev/moq/tree/main/demo/boy) for the orchestration `justfile` and ROM hosting setup.

## Install

```bash
cargo install moq-boy
```
