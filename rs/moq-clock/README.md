<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/moq-clock/badge.svg)](https://docs.rs/moq-clock/)
[![Crates.io](https://img.shields.io/crates/v/moq-clock.svg)](https://crates.io/crates/moq-clock)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-clock

A tiny example app that publishes or subscribes to a clock track over [Media over QUIC](https://moq.dev).

Useful as a reference for [moq-lite](https://github.com/moq-dev/moq/tree/main/rs/moq-lite) usage and for sanity-checking relay connectivity and latency. The JS port lives in [@moq/clock](https://github.com/moq-dev/moq/tree/main/js/clock).

## Install

```bash
cargo install moq-clock
```

## Usage

```bash
# Publish a clock to a relay
moq-clock --url https://relay.example.com/anon --broadcast clock publish

# Subscribe to it
moq-clock --url https://relay.example.com/anon --broadcast clock subscribe
```
